// Browser test of the task detail "Compare revisions" dialog.
//
//   CHROME_BIN=/path/to/chrome [CHROME_ARGS="--no-sandbox"] node scripts/test_task_revision_compare.mjs
//
// A fixture server stands in for the coordinator and serves a task whose task_revisions history comes
// in pages of two (oldest first, like the service). The test requires that the dialog defaults to the
// previous and current revision and marks exactly the fields that differ, that acceptance criteria are
// labelled added, removed or unchanged in words, that HTML in task text is shown literally, that other
// revisions are reachable through Load more and re-render the comparison, and that a very long
// description is truncated with a visible note. Only Node's standard library and Chrome are used.
import { createServer } from 'node:http';
import { mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { spawn } from 'node:child_process';
import { once } from 'node:events';
import net from 'node:net';

const root = resolve(import.meta.dirname, '..');
const assert = (condition, message) => { if (!condition) throw new Error(message); };
const sleep = (ms) => new Promise((done) => setTimeout(done, ms));

const HOSTILE = '<img src=x onerror="window.__injected = true">';
const definition = (revision, fields) => ({ title: 'Original title', description: 'line one\nline two\nline three', acceptance_criteria: ['Alpha', 'Beta', 'Gamma'], kind: 'code', priority: 2, lifecycle: 'open', depends_on: [], revision, ...fields });
const LONG_LINES = 2600;
const definitions = [
  definition(1, { lifecycle: 'planned' }),
  definition(2, { title: `Renamed <b>title</b>`, description: `line one\nline 2 ${HOSTILE}\nline three`, acceptance_criteria: ['Alpha', 'Gamma', 'Delta <i>new</i>'] }),
  definition(3, { title: `Renamed <b>title</b>`, description: `line one\nline 2 ${HOSTILE}\nline three`, acceptance_criteria: ['Alpha', 'Gamma', 'Delta <i>new</i>'], priority: 1 }),
  definition(4, { title: 'Renamed again', priority: 1, depends_on: ['task-other'] }),
  definition(5, { title: 'Renamed again', priority: 1, description: Array.from({ length: LONG_LINES }, (_, index) => `long line ${index}`).join('\n') }),
];
const records = definitions.map((fields) => ({ task_id: 'task-cmp', relation: 'subject', occurred_at: `2026-10-0${fields.revision}T00:00:00Z`, record: { revision: fields.revision, definition: fields, actor_id: 'op-1', created_at: `2026-10-0${fields.revision}T00:00:00Z` } }));
const PAGE = 2;
const historyQueries = [];
const actor = { id: 'op-1', name: 'Operator', role: 'admin', kind: 'human', session_id: 'browser-1' };
const detail = { ...definitions[1], id: 'task-cmp', revision: 2, work_status: 'ready', attempts: [], checkpoints: [], workflow: { activities: [] }, job_evidence: { jobs: [], reservations: [] } };
const single = { ...detail, id: 'task-once', title: 'Never edited', revision: 1 };

function json(response, data) {
  response.writeHead(200, { 'Content-Type': 'application/json', 'Cache-Control': 'no-store' });
  response.end(JSON.stringify({ data }));
}

async function fixtureServer() {
  const files = {
    '/': ['web/index.html', 'text/html; charset=utf-8'],
    '/index.html': ['web/index.html', 'text/html; charset=utf-8'],
    '/app.js': ['web/app.js', 'text/javascript; charset=utf-8'],
    '/style.css': ['web/style.css', 'text/css; charset=utf-8'],
  };
  const server = createServer(async (request, response) => {
    const url = new URL(request.url, 'http://fixture.invalid');
    if (url.pathname === '/api/v1/me') return json(response, { actor, csrf_token: 'fixture-csrf' });
    if (url.pathname === '/api/v1/projects') return json(response, { items: [{ id: 'p1', name: 'Fixture project', target_branch: 'main' }] });
    if (url.pathname === '/api/v1/projects/p1/tasks') return json(response, { items: [detail, single], next_cursor: null });
    if (url.pathname === '/api/v1/projects/p1/tasks/task-cmp') return json(response, detail);
    if (url.pathname === '/api/v1/projects/p1/tasks/task-once') return json(response, single);
    if (url.pathname === '/api/v1/projects/p1/tasks/task-cmp/history') {
      if (url.searchParams.get('kind') !== 'task_revisions') return json(response, { items: [], next_cursor: null });
      historyQueries.push(Object.fromEntries(url.searchParams));
      const start = Number(url.searchParams.get('cursor') || 0), items = records.slice(start, start + PAGE);
      return json(response, { items, next_cursor: start + items.length < records.length ? String(start + items.length) : null });
    }
    const file = files[url.pathname];
    if (!file) { response.writeHead(404); response.end(); return; }
    response.writeHead(200, { 'Content-Type': file[1], 'Cache-Control': 'no-store' });
    response.end(await readFile(join(root, file[0])));
  });
  server.listen(0, '127.0.0.1');
  await once(server, 'listening');
  return server;
}

async function unusedPort() {
  const probe = net.createServer();
  probe.listen(0, '127.0.0.1');
  await once(probe, 'listening');
  const { port } = probe.address();
  await new Promise((done) => probe.close(done));
  return port;
}

async function waitFor(fetchUrl, predicate, description) {
  const deadline = Date.now() + 10000;
  while (Date.now() < deadline) {
    try {
      const response = await fetch(fetchUrl);
      if (response.ok) { const value = await response.json(); if (predicate(value)) return value; }
    } catch (_) { /* Chrome is still starting. */ }
    await sleep(50);
  }
  throw new Error(`Timed out waiting for ${description}`);
}

async function removeProfile(profile) {
  for (let attempt = 0; attempt < 3; attempt += 1) {
    try { await rm(profile, { recursive: true, force: true }); return; }
    catch (error) { if (attempt === 2 || error.code !== 'ENOTEMPTY') throw error; await sleep(100); }
  }
}

async function main() {
  const server = await fixtureServer();
  const serverPort = server.address().port, debugPort = await unusedPort();
  const profile = await mkdtemp(join(tmpdir(), 'agent-coordinator-compare-test-'));
  const executable = process.env.CHROME_BIN || (process.platform === 'win32' ? 'C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe' : 'chromium');
  const extra = (process.env.CHROME_ARGS || '').split(/\s+/).filter(Boolean);
  const chrome = spawn(executable, ['--headless=new', '--disable-gpu', '--no-first-run', '--no-default-browser-check', '--disable-background-networking', `--remote-debugging-port=${debugPort}`, `--user-data-dir=${profile}`, ...extra], { stdio: 'ignore', windowsHide: true });
  let socket, launchError;
  chrome.on('error', (error) => { launchError = error; });
  try {
    const version = await waitFor(`http://127.0.0.1:${debugPort}/json/version`, (value) => Boolean(value.webSocketDebuggerUrl), 'headless Chrome');
    const connect = async (url) => {
      const connection = new WebSocket(url);
      await once(connection, 'open');
      let nextId = 1;
      const pending = new Map();
      connection.addEventListener('message', ({ data }) => { const message = JSON.parse(data), callback = pending.get(message.id); if (callback) { pending.delete(message.id); callback(message); } });
      const send = (method, params = {}) => new Promise((resolveMessage, rejectMessage) => {
        const id = nextId++, timer = setTimeout(() => { pending.delete(id); rejectMessage(new Error(`Timed out calling ${method}`)); }, 10000);
        pending.set(id, (message) => { clearTimeout(timer); message.error ? rejectMessage(new Error(message.error.message)) : resolveMessage(message.result); });
        connection.send(JSON.stringify({ id, method, params }));
      });
      return { connection, send };
    };
    let connection = await connect(version.webSocketDebuggerUrl);
    const target = await connection.send('Target.createTarget', { url: 'about:blank' });
    connection.connection.close();
    const page = await waitFor(`http://127.0.0.1:${debugPort}/json/list`, (items) => items.find((item) => item.id === target.targetId)?.webSocketDebuggerUrl, 'headless Chrome page target');
    connection = await connect(page.find((item) => item.id === target.targetId).webSocketDebuggerUrl);
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
      throw new Error(`Timed out waiting for ${label} (url: ${await evaluate('location.href')}, alert: ${await evaluate("document.querySelector('#global-alert')?.textContent || ''")})`);
    };
    const choose = (selector, value) => evaluate(`(() => { const select = document.querySelector(${JSON.stringify(selector)}); select.value = ${JSON.stringify(value)}; select.dispatchEvent(new Event('change', { bubbles: true })); })()`);
    const changedFields = () => evaluate("[...document.querySelectorAll('dialog[open] [data-changed=\"true\"]')].map((node) => node.dataset.field).sort()");
    const opsIn = (section) => evaluate(`[...document.querySelectorAll('dialog[open] [data-field="${section}"] .diff-line')].map((line) => line.dataset.op + ':' + line.querySelector('.diff-text').textContent + ':' + line.querySelector('.diff-mark').textContent)`);

    await send('Page.enable');
    await send('Page.navigate', { url: `http://127.0.0.1:${serverPort}/` });
    await waitPage("document.querySelector('.project-open')", 'project list');
    await evaluate("document.querySelector('.project-open').click()");
    await waitPage("document.querySelectorAll('#tasks-list .task-row').length === 2", 'task queue');

    // A task that was never edited has nothing to compare.
    await evaluate("[...document.querySelectorAll('#tasks-list .task-row')].find((row) => row.textContent.includes('Never edited')).click()");
    await waitPage("document.querySelector('#task-detail-heading').textContent === 'Never edited'", 'unedited task');
    assert(await evaluate("document.querySelector('#compare-revisions-button').disabled"), 'Compare revisions is enabled for a task with a single revision.');
    await evaluate("document.querySelector('#back-to-tasks').click()");
    await waitPage("!document.querySelector('#tasks-view').hidden", 'task queue again');

    await evaluate("[...document.querySelectorAll('#tasks-list .task-row')].find((row) => row.textContent.includes('Renamed')).click()");
    await waitPage("document.querySelector('#detail-task-revision')?.textContent === '2'", 'edited task');
    assert(await evaluate("!document.querySelector('#compare-revisions-button').disabled"), 'Compare revisions is disabled for an edited task.');
    await evaluate("document.querySelector('#compare-revisions-button').click()");
    await waitPage("document.querySelector('dialog[open] .compare-row')", 'comparison dialog');

    // Default: previous (1) against current (2), with exactly the differing fields marked.
    assert(historyQueries[0].kind === 'task_revisions' && historyQueries[0].limit === '50' && !historyQueries[0].cursor, `First page request was wrong: ${JSON.stringify(historyQueries[0])}`);
    assert(historyQueries.length === 1, 'The dialog loaded more than the first page although the current revision was on it.');
    assert(await evaluate("document.querySelector('#workflow-compare_base').value") === '1' && await evaluate("document.querySelector('#workflow-compare_target').value") === '2', 'Dialog did not default to previous versus current.');
    for (const id of ['workflow-compare_base', 'workflow-compare_target']) assert(await evaluate(`document.querySelector('label[for="${id}"]')?.textContent.length > 0`), `${id} has no visible label.`);
    assert(await evaluate("document.querySelector('#workflow-compare_target option:checked').textContent.includes('(current)')"), 'The current revision is not named in the selector.');
    assert(JSON.stringify(await changedFields()) === JSON.stringify(['Acceptance criteria', 'Description', 'lifecycle', 'title']), `Wrong fields marked as changed: ${JSON.stringify(await changedFields())}`);
    assert(await evaluate("[...document.querySelectorAll('dialog[open] .compare-row')].every((row) => row.querySelector('.compare-flag').textContent === (row.dataset.changed === 'true' ? 'Changed' : 'Unchanged'))"), 'A field is marked changed by colour only, without the word.');

    // Criteria: removed, added and unchanged are told apart in words and symbols, not only by colour.
    const criteria = await opsIn('Acceptance criteria');
    assert(JSON.stringify(criteria) === JSON.stringify(['same:Alpha:Unchanged', 'del:Beta:Removed', 'same:Gamma:Unchanged', 'add:Delta <i>new</i>:Added']), `Criteria diff was wrong: ${JSON.stringify(criteria)}`);
    assert(await evaluate("document.querySelector('dialog[open] [data-field=\"Acceptance criteria\"] ul').children.length === 4 && getComputedStyle(document.querySelector('dialog[open] .diff-del .diff-text')).textDecorationLine.includes('line-through')"), 'Removed criteria are not distinguished beyond their label.');

    // HTML in task text is shown literally and never becomes markup.
    const description = await opsIn('Description');
    assert(description.includes(`add:line 2 ${HOSTILE}:+`) && description.includes('del:line two:−'), `Description diff was wrong: ${JSON.stringify(description)}`);
    assert(await evaluate("document.querySelector('dialog[open] .compare-cell-value').textContent === 'Original title' && [...document.querySelectorAll('dialog[open] .compare-cell-value')].some((node) => node.textContent === 'Renamed <b>title</b>')"), 'The title containing HTML was not shown literally.');
    assert(await evaluate("document.querySelectorAll('dialog[open] b, dialog[open] i, dialog[open] img').length === 0 && window.__injected === undefined"), 'HTML in task text became markup.');

    // Older and newer revisions come through Load more and re-render the comparison.
    assert(await evaluate("document.querySelectorAll('#workflow-compare_base option').length") === 2, 'Only the first page should be loaded at first.');
    await evaluate("[...document.querySelectorAll('dialog[open] button')].find((button) => button.textContent === 'Load more revisions').click()");
    await waitPage("document.querySelectorAll('#workflow-compare_base option').length === 4", 'second page');
    assert(historyQueries[1].cursor === '2', 'Load more did not continue from the saved cursor.');
    assert(await evaluate("document.querySelector('#workflow-compare_base').value") === '1' && await evaluate("document.querySelector('#workflow-compare_target').value") === '2', 'Loading more replaced the default selection.');
    await choose('#workflow-compare_base', '3'); await choose('#workflow-compare_target', '4');
    assert(JSON.stringify(await changedFields()) === JSON.stringify(['Acceptance criteria', 'Description', 'depends_on', 'title']), `Revisions 3 and 4 marked the wrong fields: ${JSON.stringify(await changedFields())}`);
    assert(await evaluate("document.querySelector('dialog[open] [data-field=\"depends_on\"] .compare-cell:last-child .compare-cell-value').textContent") === 'task-other', 'Target prerequisites were not shown.');
    await choose('#workflow-compare_base', '1'); await choose('#workflow-compare_target', '1');
    assert(JSON.stringify(await changedFields()) === '[]' && await evaluate("document.querySelector('dialog[open]').textContent.includes('same revision')"), 'Identical selections were reported as different.');

    // A very long description is truncated with a visible note and renders promptly.
    await evaluate("[...document.querySelectorAll('dialog[open] button')].find((button) => button.textContent === 'Load more revisions').click()");
    await waitPage("document.querySelectorAll('#workflow-compare_base option').length === 5", 'third page');
    assert(await evaluate("[...document.querySelectorAll('dialog[open] button')].find((button) => button.textContent === 'Load more revisions').hidden"), 'Load more stayed visible after the last page.');
    const started = Date.now();
    await choose('#workflow-compare_base', '4'); await choose('#workflow-compare_target', '5');
    assert(await evaluate("document.querySelector('dialog[open] .compare-truncation')?.textContent.includes('Truncated') && document.querySelector('dialog[open] .compare-truncation').textContent.includes('2,600')"), 'A long description was not truncated with a visible note.');
    assert(await evaluate("document.querySelectorAll('dialog[open] [data-field=\"Description\"] .diff-line').length") <= 2 * 2000, 'The description diff exceeded its line cap.');
    assert(Date.now() - started < 5000, 'Comparing a very long description was slow.');

    if (process.env.COMPARE_SCREENSHOT) { const { data } = await send('Page.captureScreenshot', { format: 'png' }); await writeFile(process.env.COMPARE_SCREENSHOT, Buffer.from(data, 'base64')); }
    // Phone width: no horizontal overflow, and every field still names its revision.
    await send('Emulation.setDeviceMetricsOverride', { width: 375, height: 844, deviceScaleFactor: 1, mobile: true });
    assert(await evaluate("document.documentElement.scrollWidth <= window.innerWidth && document.querySelector('dialog[open]').scrollWidth <= document.querySelector('dialog[open]').clientWidth + 1"), 'The comparison overflows at phone width.');
    assert(await evaluate("getComputedStyle(document.querySelector('dialog[open] .compare-cell-label')).display !== 'none'"), 'Cells do not name their revision at phone width.');
    await send('Emulation.clearDeviceMetricsOverride');

    // Closing removes the dialog.
    await evaluate("document.querySelector('dialog[open]').close()");
    await waitPage("!document.querySelector('dialog')", 'dialog removed');
    console.log('PASS: the task revision comparison defaults to previous versus current, marks exactly the changed fields, labels added, removed and unchanged criteria in words, shows HTML literally, reaches other revisions through Load more, truncates a very long description with a note, and fits a phone.');
  } finally {
    socket?.close();
    if (chrome.pid && chrome.exitCode === null && process.platform === 'win32') {
      const taskkill = spawn('taskkill', ['/PID', String(chrome.pid), '/T', '/F'], { stdio: 'ignore', windowsHide: true });
      await once(taskkill, 'exit'); await once(chrome, 'exit');
    }
    if (chrome.pid && chrome.exitCode === null) { const exited = once(chrome, 'exit'); chrome.kill('SIGTERM'); await exited; }
    await new Promise((done) => server.close(done));
    await removeProfile(profile);
    if (launchError) throw launchError;
  }
}

main().catch((error) => { console.error(`FAIL: ${error.message}`); process.exitCode = 1; });
