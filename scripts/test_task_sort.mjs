// Browser test of the task queue's Sort control.
//
//   CHROME_BIN=/path/to/chrome [CHROME_ARGS="--no-sandbox"] node scripts/test_task_sort.mjs
//
// A fixture server stands in for the coordinator and serves the task list newest id first, so any
// order the page shows comes from its own sort. The test selects every sort option and requires the
// exact order across two pages and in the completed view, with a deterministic id tie-break and
// tasks without progress last for the progress sorts. It also requires that the chosen sort
// survives a reload, that an unknown stored value and unavailable storage fall back to the default,
// and that sorting never requests a task's detail. Only Node's standard library and Chrome are used.
import { createServer } from 'node:http';
import { mkdtemp, readFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { spawn } from 'node:child_process';
import { once } from 'node:events';
import net from 'node:net';

const root = resolve(import.meta.dirname, '..');
const assert = (condition, message) => { if (!condition) throw new Error(message); };
const sleep = (ms) => new Promise((done) => setTimeout(done, ms));
const same = (actual, expected) => JSON.stringify(actual) === JSON.stringify(expected);

// ---- Fixture data ------------------------------------------------------------------------------
const actor = { id: 'op-1', name: 'Operator', role: 'admin', kind: 'human', session_id: 'browser-1' };
const projects = [{ id: 'p1', name: 'Alpha', repository_url: 'https://example.invalid/alpha.git', target_branch: 'main' }];
const day = (value) => (value ? `2026-${value}T00:00:00Z` : null);
// [id, priority, created_at, ready_since, last_progress_at]; ties (t-c/t-f, t-b/t-d, t-d/t-g) exercise the id tie-break.
const rows = [
  ['t-a', 1, '10-01', '10-05', '10-09'], ['t-b', 0, '10-03', '10-04', null], ['t-c', 1, '10-02', '10-03', '10-07'],
  ['t-d', 0, '10-03', '10-02', '10-08'], ['t-e', 2, '10-05', '10-01', null], ['t-f', 1, '10-02', '10-03', '10-07'],
  ['t-g', 0, '10-04', '10-02', '10-08'], ['t-h', 2, '10-06', '10-06', '10-10'], ['t-i', 1, '10-01', '10-05', null],
  ['t-j', 0, '10-07', '10-07', '10-06'], ['t-k', 2, '10-02', '10-01', '10-05'], ['t-l', 1, '10-08', '10-03', null],
];
const completedRows = [['t-x', 1, '10-01', '10-02', '10-03'], ['t-y', 0, '10-02', '10-01', null], ['t-z', 1, '10-03', '10-03', '10-04']];
const makeTask = ([id, priority, created, ready, progress], workStatus) => ({
  id, title: `Task ${id}`, description: `Description ${id}`, revision: 1, kind: 'code', priority, lifecycle: workStatus === 'done' ? 'done' : 'open',
  work_status: workStatus, created_at: day(created), ready_since: day(ready), last_progress_at: day(progress), acceptance_criteria: [],
});
const tasks = [...rows.map((row) => makeTask(row, 'ready')), ...completedRows.map((row) => makeTask(row, 'done'))].reverse();
const expectedQueue = {
  priority: ['t-d', 't-g', 't-b', 't-j', 't-c', 't-f', 't-l', 't-a', 't-i', 't-e', 't-k', 't-h'],
  oldest: ['t-a', 't-i', 't-c', 't-f', 't-k', 't-b', 't-d', 't-g', 't-e', 't-h', 't-j', 't-l'],
  newest: ['t-l', 't-j', 't-h', 't-e', 't-g', 't-b', 't-d', 't-c', 't-f', 't-k', 't-a', 't-i'],
  'longest-ready': ['t-e', 't-k', 't-d', 't-g', 't-c', 't-f', 't-l', 't-b', 't-a', 't-i', 't-h', 't-j'],
  'recent-progress': ['t-h', 't-a', 't-d', 't-g', 't-c', 't-f', 't-j', 't-k', 't-b', 't-e', 't-i', 't-l'],
  'least-recent-progress': ['t-k', 't-j', 't-c', 't-f', 't-d', 't-g', 't-a', 't-h', 't-b', 't-e', 't-i', 't-l'],
};
const expectedCompleted = {
  priority: ['t-y', 't-x', 't-z'], oldest: ['t-x', 't-y', 't-z'], newest: ['t-z', 't-y', 't-x'],
  'longest-ready': ['t-y', 't-x', 't-z'], 'recent-progress': ['t-z', 't-x', 't-y'], 'least-recent-progress': ['t-x', 't-z', 't-y'],
};
const SORT_KEY = 'agent-coordinator.task-sort';

// ---- Fixture server ------------------------------------------------------------------------------
const requests = [];
function json(response, data) {
  response.writeHead(200, { 'Content-Type': 'application/json', 'Cache-Control': 'no-store' });
  response.end(JSON.stringify({ data }));
}
async function route(request, response) {
  const url = new URL(request.url, 'http://fixture.invalid');
  const path = url.pathname;
  requests.push(`${request.method} ${path}`);
  const files = { '/': ['web/index.html', 'text/html; charset=utf-8'], '/app.js': ['web/app.js', 'text/javascript; charset=utf-8'], '/style.css': ['web/style.css', 'text/css; charset=utf-8'] };
  if (files[path]) {
    response.writeHead(200, { 'Content-Type': files[path][1], 'Cache-Control': 'no-store' });
    return response.end(await readFile(join(root, files[path][0])));
  }
  if (path === '/api/v1/me') return json(response, { actor, csrf_token: 'csrf' });
  if (path === '/api/v1/projects') return json(response, { items: projects });
  if (path === '/api/v1/projects/p1/tasks') {
    const queueOnly = url.searchParams.get('exclude_done') === 'true';
    return json(response, { items: tasks.filter((task) => !queueOnly || task.work_status !== 'done'), next_cursor: null });
  }
  if (path === '/api/v1/projects/p1/orientation') return json(response, { project: projects[0], policy_revision: 1 });
  response.writeHead(404); response.end();
}

// ---- Chrome driver -------------------------------------------------------------------------------
async function unusedPort() {
  const probe = net.createServer();
  probe.listen(0, '127.0.0.1');
  await once(probe, 'listening');
  const { port } = probe.address();
  await new Promise((done) => probe.close(done));
  return port;
}
async function waitFor(fetchUrl, predicate, description) {
  const deadline = Date.now() + 15000;
  while (Date.now() < deadline) {
    try {
      const response = await fetch(fetchUrl);
      if (response.ok) { const value = await response.json(); if (predicate(value)) return value; }
    } catch (_) { /* Chrome is still starting. */ }
    await sleep(50);
  }
  throw new Error(`Timed out waiting for ${description}`);
}
// Makes every localStorage read and write throw, like a browser with storage blocked.
const BROKEN_STORAGE = `
  for (const method of ['getItem', 'setItem', 'removeItem']) Storage.prototype[method] = () => { throw new DOMException('Storage is unavailable', 'SecurityError'); };
`;

async function main() {
  const server = createServer((request, response) => route(request, response).catch((error) => { response.writeHead(500); response.end(String(error)); }));
  server.listen(0, '127.0.0.1');
  await once(server, 'listening');
  const serverPort = server.address().port;
  const debugPort = await unusedPort();
  const profile = await mkdtemp(join(tmpdir(), 'agent-coordinator-sort-test-'));
  const executable = process.env.CHROME_BIN || 'chromium';
  const extra = (process.env.CHROME_ARGS || '').split(/\s+/).filter(Boolean);
  const chrome = spawn(executable, ['--headless=new', '--disable-gpu', '--no-first-run', '--no-default-browser-check', '--disable-background-networking', '--window-size=1100,700', `--remote-debugging-port=${debugPort}`, `--user-data-dir=${profile}`, ...extra], { stdio: 'ignore' });
  let socket, launchError;
  chrome.on('error', (error) => { launchError = error; });
  try {
    const version = await waitFor(`http://127.0.0.1:${debugPort}/json/version`, (value) => Boolean(value.webSocketDebuggerUrl), 'headless Chrome');
    const connect = async (url) => {
      const connection = new WebSocket(url);
      await once(connection, 'open');
      let nextId = 1; const pending = new Map();
      connection.addEventListener('message', ({ data }) => { const message = JSON.parse(data); const callback = pending.get(message.id); if (callback) { pending.delete(message.id); callback(message); } });
      const send = (method, params = {}) => new Promise((resolveMessage, rejectMessage) => {
        const id = nextId++;
        const timer = setTimeout(() => { pending.delete(id); rejectMessage(new Error(`Timed out calling ${method}`)); }, 15000);
        pending.set(id, (message) => { clearTimeout(timer); message.error ? rejectMessage(new Error(message.error.message)) : resolveMessage(message.result); });
        connection.send(JSON.stringify({ id, method, params }));
      });
      return { connection, send };
    };
    let connection = await connect(version.webSocketDebuggerUrl);
    const target = await connection.send('Target.createTarget', { url: 'about:blank' });
    connection.connection.close();
    const list = await waitFor(`http://127.0.0.1:${debugPort}/json/list`, (items) => items.find((item) => item.id === target.targetId)?.webSocketDebuggerUrl, 'page target');
    connection = await connect(list.find((item) => item.id === target.targetId).webSocketDebuggerUrl);
    socket = connection.connection;
    const { send } = connection;
    const evaluate = async (expression) => {
      const result = await send('Runtime.evaluate', { expression, awaitPromise: true, returnByValue: true });
      if (result.exceptionDetails) throw new Error(`${result.exceptionDetails.exception?.description || result.exceptionDetails.text} IN: ${expression.slice(0, 300)}`);
      return result.result.value;
    };
    const waitPage = async (expression, label) => {
      const deadline = Date.now() + 10000;
      while (Date.now() < deadline) { if (await evaluate(expression)) return; await sleep(30); }
      throw new Error(`Timed out waiting for ${label}`);
    };
    await send('Page.enable');

    // ---- Helpers over the page ---------------------------------------------------------------------
    const click = (selector) => evaluate(`document.querySelector(${JSON.stringify(selector)}).click()`);
    const openQueue = async () => {
      await send('Page.navigate', { url: `http://127.0.0.1:${serverPort}/` });
      await waitPage("document.querySelector('.project-open')", 'project list');
      await click('.project-open');
      await waitPage("document.querySelectorAll('#tasks-list .task-row').length > 0", 'task queue');
    };
    const reload = async () => { await send('Page.reload'); await waitPage("document.querySelector('.project-open')", 'project list'); await click('.project-open'); await waitPage("document.querySelectorAll('#tasks-list .task-row').length > 0", 'task queue'); };
    const titles = () => evaluate("[...document.querySelectorAll('#tasks-list .task-row .task-title')].map((node) => node.textContent.replace('Task ', ''))");
    const choose = async (value) => {
      await evaluate(`(() => { const select = document.querySelector('#tasks-sort'); select.value = ${JSON.stringify(value)}; select.dispatchEvent(new Event('change', { bubbles: true })); })()`);
    };
    const selected = () => evaluate("document.querySelector('#tasks-sort').value");
    const stored = () => evaluate(`localStorage.getItem(${JSON.stringify(SORT_KEY)})`);
    // Reads the whole queue by paging through it, 10 tasks to a page.
    const queueOrder = async () => {
      await waitPage("document.querySelector('#tasks-first-page') && !document.querySelector('#tasks-view').hidden", 'queue page');
      await click('#tasks-first-page');
      const all = [];
      for (let page = 0; page < 5; page++) {
        all.push(...await titles());
        if (await evaluate("document.querySelector('#tasks-next-page').disabled")) break;
        await click('#tasks-next-page');
      }
      await click('#tasks-first-page');
      return all;
    };
    const expectQueue = async (sort) => {
      await waitPage(`document.querySelector('#tasks-list .task-row .task-title').textContent === 'Task ${expectedQueue[sort][0]}'`, `${sort} order`);
      const actual = await queueOrder();
      assert(same(actual, expectedQueue[sort]), `${sort}: expected ${expectedQueue[sort].join(' ')} but saw ${actual.join(' ')}`);
    };

    // ---- 1. The control ---------------------------------------------------------------------------
    await openQueue();
    await evaluate("(() => { const select = document.querySelector('#tasks-page-size'); select.value = '10'; select.dispatchEvent(new Event('change', { bubbles: true })); })()");
    await waitPage("document.querySelectorAll('#tasks-list .task-row').length === 10", 'page size');
    const control = await evaluate(`(() => { const select = document.querySelector('#tasks-sort'); const label = document.querySelector('label[for=tasks-sort]'); return { options: [...select.options].map((option) => [option.value, option.textContent]), label: label && label.textContent.startsWith('Sort'), visible: select.offsetParent !== null, disabled: select.disabled }; })()`);
    assert(same(control.options, [['priority', 'Priority then oldest'], ['oldest', 'Oldest first'], ['newest', 'Newest first'], ['longest-ready', 'Longest ready'], ['recent-progress', 'Most recent progress'], ['least-recent-progress', 'Least recent progress']]), `Unexpected sort options: ${JSON.stringify(control.options)}`);
    assert(control.label && control.visible && !control.disabled, 'The Sort select needs a visible label and must be enabled.');
    assert(await selected() === 'priority' && await stored() === null, 'The default sort should be "Priority then oldest" with nothing stored yet.');
    assert(await evaluate("(() => { const select = document.querySelector('#tasks-sort'); select.focus(); return document.activeElement === select; })()"), 'The Sort select is not focusable.');

    // ---- 2. Every option orders the queue across pages ---------------------------------------------
    await expectQueue('priority');
    for (const sort of Object.keys(expectedQueue)) {
      await choose(sort);
      await expectQueue(sort);
      assert(await stored() === sort, `${sort}: the choice was not stored under ${SORT_KEY}.`);
    }
    // Rows show the last progress time, or say that there is none.
    await choose('recent-progress');
    await waitPage("document.querySelector('#tasks-list .task-row .task-title').textContent === 'Task t-h'", 'progress order');
    const progress = await evaluate("[...document.querySelectorAll('#tasks-list .task-row')].map((row) => [row.querySelector('.task-title').textContent, row.querySelector('.task-progress')?.textContent || '', row.querySelector('.task-progress')?.dateTime || ''])");
    const lead = progress.find(([title]) => title === 'Task t-h'), none = await (async () => { await click('#tasks-last-page'); return evaluate("[...document.querySelectorAll('#tasks-list .task-row')].map((row) => [row.querySelector('.task-title').textContent, row.querySelector('.task-progress')?.textContent || ''])"); })();
    assert(lead[1].startsWith('Last progress ') && lead[2] === '2026-10-10T00:00:00.000Z', `A task with progress should show its time: ${JSON.stringify(lead)}`);
    assert(none.find(([title]) => title === 'Task t-l')[1] === 'No progress yet', 'A task without progress should say so.');
    await click('#tasks-first-page');

    // ---- 3. Completed view -----------------------------------------------------------------------
    await click('#completed-tasks-tab');
    await waitPage("document.querySelectorAll('#tasks-list .completed-task-row').length === 3", 'completed tasks');
    const completed = () => evaluate("[...document.querySelectorAll('#tasks-list .completed-task-row .task-title')].map((node) => node.textContent.replace('Task ', ''))");
    assert(await selected() === 'recent-progress', 'The completed view should keep the chosen sort.');
    for (const sort of Object.keys(expectedCompleted)) {
      await choose(sort);
      await waitPage(`document.querySelector('#tasks-list .completed-task-row .task-title').textContent === 'Task ${expectedCompleted[sort][0]}'`, `completed ${sort}`);
      assert(same(await completed(), expectedCompleted[sort]), `completed ${sort}: expected ${expectedCompleted[sort].join(' ')} but saw ${(await completed()).join(' ')}`);
    }
    await click('#task-queue-tab');
    await waitPage("document.querySelectorAll('#tasks-list .task-row:not(.completed-task-row)').length > 0", 'queue');

    // ---- 4. Persistence ---------------------------------------------------------------------------
    await choose('least-recent-progress');
    await reload();
    assert(await selected() === 'least-recent-progress', 'The chosen sort did not survive a reload.');
    await evaluate("(() => { const select = document.querySelector('#tasks-page-size'); select.value = '10'; select.dispatchEvent(new Event('change', { bubbles: true })); })()");
    await expectQueue('least-recent-progress');
    await evaluate(`localStorage.setItem(${JSON.stringify(SORT_KEY)}, 'bogus')`);
    await reload();
    assert(await selected() === 'priority', 'An unknown stored value should fall back to the default sort.');
    await evaluate("(() => { const select = document.querySelector('#tasks-page-size'); select.value = '10'; select.dispatchEvent(new Event('change', { bubbles: true })); })()");
    await expectQueue('priority');

    // ---- 5. Storage unavailable ----------------------------------------------------------------------
    await evaluate(`localStorage.setItem(${JSON.stringify(SORT_KEY)}, 'newest')`);
    await send('Page.addScriptToEvaluateOnNewDocument', { source: BROKEN_STORAGE });
    await reload();
    assert(await evaluate("(() => { try { localStorage.getItem('x'); return false; } catch { return true; } })()"), 'The test could not make storage unavailable.');
    assert(await selected() === 'priority', 'Unavailable storage should fall back to the default sort.');
    await evaluate("(() => { const select = document.querySelector('#tasks-page-size'); select.value = '10'; select.dispatchEvent(new Event('change', { bubbles: true })); })()");
    await expectQueue('priority');
    await choose('oldest');
    await expectQueue('oldest');
    assert(await selected() === 'oldest', 'The sort should still apply for this page when storage is unavailable.');

    // ---- 6. No per-task detail requests ------------------------------------------------------------
    const detail = requests.filter((entry) => /^GET \/api\/v1\/projects\/p1\/tasks\/[^/]+/.test(entry));
    assert(detail.length === 0, `Sorting requested task detail: ${detail.slice(0, 3).join(', ')}`);

    console.log('PASS: each sort option orders the queue and completed view with an id tie-break across pages and puts tasks without progress last; the choice survives a reload, an unknown stored value and unavailable storage fall back to the default, rows show the last progress time, and no task detail was requested.');
  } finally {
    socket?.close();
    if (chrome.pid && chrome.exitCode === null) { const exited = once(chrome, 'exit'); chrome.kill('SIGTERM'); await exited; }
    await new Promise((done) => server.close(done));
    await rm(profile, { recursive: true, force: true }).catch(() => {});
    if (launchError) throw launchError;
  }
}

main().catch((error) => { console.error(`FAIL: ${error.message}`); process.exitCode = 1; });
