// Browser test of the task queue's Kind, Origin and Delivery filters.
//
//   CHROME_BIN=/path/to/chrome [CHROME_ARGS="--no-sandbox"] node scripts/test_task_filters.mjs
//
// A fixture server stands in for the coordinator and serves one project's task list from mutable
// objects (plus a second project). The test drives each filter alone and in combination with Status
// and search in both the queue and completed views, and checks the row indicators, the page/total
// counts, the empty-state text, Clear filters, the reset on project change, that changing a filter
// makes no request, and that a refresh with filters active records no DOM mutation. Only Node's
// standard library and Chrome are used.
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

// ---- Fixture data ------------------------------------------------------------------------------
const actor = { id: 'op-1', name: 'Operator', role: 'admin', kind: 'human', session_id: 'browser-1' };
const projects = [
  { id: 'p1', name: 'Alpha', repository_url: 'https://example.invalid/alpha.git', target_branch: 'main' },
  { id: 'p2', name: 'Beta', repository_url: 'https://example.invalid/beta.git', target_branch: 'trunk' },
];
const policy = { id: 'p1', name: 'Alpha', policy_revision: 3, review_mode: 'either', recovery_mode: 'agent', lease_seconds: 600, rules: 'Be careful.', agent_rule_editing: false, automatic_integration: false, allow_subagent_reviews: false, integration_owner: 'integrator' };
const makeTask = (id, title, kind, origin, humanDelivered, workStatus = 'ready') => ({
  id, title, description: `Description of ${title}`, revision: 1, kind, origin, human_delivered: humanDelivered,
  lifecycle: workStatus === 'done' ? 'done' : 'open', work_status: workStatus, created_at: '2026-10-01T00:00:00Z', acceptance_criteria: [],
});
const p1Tasks = [
  makeTask('q1', 'Queue one', 'code', 'human', false),
  makeTask('q2', 'Queue two', 'code', 'agent', false),
  makeTask('q3', 'Queue three', 'general', 'agent', false, 'in_progress'),
  makeTask('q4', 'Queue four', 'code', 'human', true),
  makeTask('q5', 'Queue five', 'general', 'human', true),
  makeTask('q6', 'Queue six', 'code', 'agent', true, 'in_progress'),
  makeTask('q7', 'Queue seven', 'general', 'service', false),
  makeTask('d1', 'Done one', 'code', 'human', false, 'done'),
  makeTask('d2', 'Done two', 'code', 'agent', false, 'done'),
  makeTask('d3', 'Done three', 'general', 'agent', true, 'done'),
  makeTask('d4', 'Done four', 'code', 'agent', true, 'done'),
];
const p2Tasks = [makeTask('b1', 'Beta one', 'general', 'service', false)];
const hits = new Map();
const hitCount = (needle) => [...hits].filter(([key]) => key.includes(needle)).reduce((sum, [, count]) => sum + count, 0);

function json(response, data) {
  response.writeHead(200, { 'Content-Type': 'application/json', 'Cache-Control': 'no-store' });
  response.end(JSON.stringify({ data }));
}

async function route(request, response) {
  const url = new URL(request.url, 'http://fixture.invalid');
  const path = url.pathname;
  hits.set(`${request.method} ${path}`, (hits.get(`${request.method} ${path}`) || 0) + 1);
  const files = { '/': ['web/index.html', 'text/html; charset=utf-8'], '/app.js': ['web/app.js', 'text/javascript; charset=utf-8'], '/style.css': ['web/style.css', 'text/css; charset=utf-8'] };
  if (files[path]) {
    response.writeHead(200, { 'Content-Type': files[path][1], 'Cache-Control': 'no-store' });
    return response.end(await readFile(join(root, files[path][0])));
  }
  if (path === '/api/v1/me') return json(response, { actor, csrf_token: 'csrf' });
  if (path === '/api/v1/projects') return json(response, { items: projects });
  const project = path.match(/^\/api\/v1\/projects\/(p[12])(\/.*)?$/);
  if (project) {
    const rest = project[2] || '';
    const list = project[1] === 'p1' ? p1Tasks : p2Tasks;
    if (rest === '/orientation') return json(response, { project: policy, policy_revision: policy.policy_revision });
    if (rest === '/tasks') {
      const queueOnly = url.searchParams.get('exclude_done') === 'true';
      return json(response, { items: list.filter((task) => !queueOnly || task.work_status !== 'done'), next_cursor: null });
    }
    if (rest === '/tasks/archived') return json(response, { items: [], next_cursor: null });
    return json(response, { items: [], next_cursor: null });
  }
  response.writeHead(404); response.end();
}

// ---- Chrome driver -----------------------------------------------------------------------------
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

// Runs before the page's own scripts: a mutation observer the test can aim at a view.
const PAGE_SETUP = `
  window.__observe = (selector) => {
    const target = document.querySelector(selector);
    window.__observer?.disconnect();
    window.__records = [];
    window.__observer = new MutationObserver((list) => { window.__records.push(...list); });
    window.__observer.observe(target, { subtree: true, childList: true, attributes: true, characterData: true });
  };
  window.__drain = () => {
    window.__records.push(...window.__observer.takeRecords());
    const seen = window.__records.map((record) => record.type + ':' + (record.attributeName || '') + ' on ' + record.target.nodeName.toLowerCase() + (record.target.id ? '#' + record.target.id : ''));
    window.__records = []; return seen;
  };
`;

async function main() {
  const server = createServer((request, response) => route(request, response).catch((error) => { response.writeHead(500); response.end(String(error)); }));
  server.listen(0, '127.0.0.1');
  await once(server, 'listening');
  const serverPort = server.address().port;
  const debugPort = await unusedPort();
  const profile = await mkdtemp(join(tmpdir(), 'agent-coordinator-filters-test-'));
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
      throw new Error(`Timed out waiting for ${label} (alert: ${await evaluate("document.querySelector('#global-alert')?.textContent || ''")})`);
    };
    await send('Page.enable');
    await send('Page.addScriptToEvaluateOnNewDocument', { source: PAGE_SETUP });
    await send('Page.navigate', { url: `http://127.0.0.1:${serverPort}/` });
    await waitPage("document.querySelector('.project-card')", 'project list');

    // ---- Helpers over the page -------------------------------------------------------------------
    const click = (selector) => evaluate(`document.querySelector(${JSON.stringify(selector)}).click()`);
    const choose = (selector, value) => evaluate(`(() => { const select = document.querySelector(${JSON.stringify(selector)}); select.value = ${JSON.stringify(value)}; select.dispatchEvent(new Event('change', { bubbles: true })); })()`);
    const search = (value) => evaluate(`(() => { const input = document.querySelector('#tasks-search'); input.value = ${JSON.stringify(value)}; input.dispatchEvent(new Event('input', { bubbles: true })); })()`);
    const titles = () => evaluate("[...document.querySelectorAll('#tasks-list .task-row .task-title')].map((node) => node.textContent)");
    const expectTitles = async (label, expected) => {
      await sleep(80);
      const found = await titles();
      assert(JSON.stringify(found) === JSON.stringify(expected), `${label}: expected ${JSON.stringify(expected)}, saw ${JSON.stringify(found)}`);
    };
    const pageStatus = () => evaluate("document.querySelector('#tasks-page-status').textContent");
    const filterBar = () => evaluate("(() => { const bar = document.querySelector('#task-filter-bar'); return bar.hidden ? '' : document.querySelector('#task-filter-count').textContent; })()");
    const stateText = () => evaluate("(() => { const panel = document.querySelector('#tasks-state'); return panel.hidden ? '' : panel.textContent; })()");
    const filtersAre = (kind, origin, delivery) => evaluate(`['#kind-filter', '#origin-filter', '#delivery-filter'].map((selector) => document.querySelector(selector).value).join() === ${JSON.stringify([kind, origin, delivery].join())}`);

    // ---- Queue view ------------------------------------------------------------------------------
    await click('.project-open');
    await waitPage("document.querySelectorAll('#tasks-list .task-row').length === 7", 'task queue');
    await expectTitles('unfiltered queue', ['Queue one', 'Queue two', 'Queue three', 'Queue four', 'Queue five', 'Queue six', 'Queue seven']);
    assert(await pageStatus() === 'Page 1 of 7 tasks', `Unfiltered total was ${await pageStatus()}.`);
    assert(await filterBar() === '', 'The filter bar showed with no filter active.');

    // The controls have visible labels and the right choices.
    const controls = await evaluate("['kind', 'origin', 'delivery'].map((name) => { const select = document.querySelector('#' + name + '-filter'); const label = document.querySelector('label[for=' + name + '-filter]'); return [label.firstChild.textContent, select.options[0].value, select.options[0].textContent, select.options.length]; })");
    assert(JSON.stringify(controls) === JSON.stringify([['Kind', 'all', 'All kinds', 3], ['Origin', 'all', 'All origins', 4], ['Delivery', 'all', 'All deliveries', 3]]), `Unexpected filter controls: ${JSON.stringify(controls)}`);

    // Rows show a compact kind / origin / human-delivered indicator.
    const indicators = await evaluate("[...document.querySelectorAll('#tasks-list .task-row')].map((row) => [...row.querySelectorAll('.task-attribute')].map((pill) => pill.textContent).join('/'))");
    assert(JSON.stringify(indicators) === JSON.stringify(['code/human', 'code/agent', 'general/agent', 'code/human/human-delivered', 'general/human/human-delivered', 'code/agent/human-delivered', 'general/service']), `Row indicators were ${JSON.stringify(indicators)}.`);

    // Changing a filter makes no request.
    const requests = () => hitCount('/api/v1/');
    const before = requests();

    await choose('#kind-filter', 'code');
    await expectTitles('Kind=code', ['Queue one', 'Queue two', 'Queue four', 'Queue six']);
    assert(await pageStatus() === 'Page 1 of 4 tasks', `Kind=code total was ${await pageStatus()}.`);
    assert(await filterBar() === '1 filter active', `Filter count was ${await filterBar()}.`);
    await choose('#kind-filter', 'all');
    await choose('#origin-filter', 'agent');
    await expectTitles('Origin=agent', ['Queue two', 'Queue three', 'Queue six']);
    await choose('#origin-filter', 'all');
    await choose('#delivery-filter', 'human');
    await expectTitles('Delivery=human-delivered', ['Queue four', 'Queue five', 'Queue six']);
    await choose('#delivery-filter', 'supervisor');
    await expectTitles('Delivery=supervisor-claimable', ['Queue one', 'Queue two', 'Queue three', 'Queue seven']);
    await choose('#delivery-filter', 'human');

    // Combinations: filters AND with each other, Status and search.
    await choose('#origin-filter', 'agent');
    await expectTitles('Origin=agent + Delivery=human', ['Queue six']);
    assert(await pageStatus() === 'Page 1 of 1 tasks' && await filterBar() === '2 filters active', `Combined count was ${await pageStatus()} / ${await filterBar()}.`);
    await choose('#kind-filter', 'general');
    assert(await stateText() === 'Filters hid every task. Use Clear filters to show tasks again.', `Empty state was "${await stateText()}".`);
    assert(await pageStatus() === '' && await filterBar() === '3 filters active', `Empty counts were "${await pageStatus()}" / ${await filterBar()}.`);
    await choose('#kind-filter', 'code');
    await choose('#status-filter', 'ready');
    assert(await filterBar() === '4 filters active', `Four-filter count was ${await filterBar()}.`);
    assert((await stateText()).startsWith('Filters hid every task'), 'Status=ready hid no task from the combination.');
    await choose('#status-filter', 'in_progress');
    await expectTitles('Status + three filters', ['Queue six']);
    await search('six');
    await expectTitles('Search + filters', ['Queue six']);
    await search('four');
    assert(await stateText() === 'Filters hid every task that matches this search. Use Clear filters to show tasks again.', `Search empty state was "${await stateText()}".`);

    assert(requests() === before, `Changing a filter made ${requests() - before} request(s).`);

    // Clear filters restores the list (the search is not a filter and stays).
    await click('#clear-task-filters');
    await expectTitles('Cleared with search four', ['Queue four']);
    assert(await filtersAre('all', 'all', 'all') && await evaluate("document.querySelector('#status-filter').value") === 'all', 'Clear filters left a control set.');
    assert(await filterBar() === '', 'The filter bar stayed after Clear filters.');
    assert(await evaluate("document.activeElement.id") === 'status-filter', 'Focus was not placed on a visible control after Clear filters.');
    await search('');
    await expectTitles('Cleared queue', ['Queue one', 'Queue two', 'Queue three', 'Queue four', 'Queue five', 'Queue six', 'Queue seven']);

    // An empty search state without filters keeps its plain wording.
    await search('nothing matches this');
    assert(await stateText() === 'No tasks match this search.', `Plain search empty state was "${await stateText()}".`);
    await search('');

    // Pagination counts follow the filtered set.
    await choose('#tasks-page-size', '10');
    await choose('#origin-filter', 'human');
    await expectTitles('Origin=human', ['Queue one', 'Queue four', 'Queue five']);
    await click('#clear-task-filters');
    await choose('#tasks-page-size', '25');

    // A refresh with filters active records no DOM mutation and keeps the filters.
    await choose('#kind-filter', 'code');
    await choose('#delivery-filter', 'human');
    await expectTitles('Kind=code + Delivery=human', ['Queue four', 'Queue six']);
    await evaluate("window.__observe('#tasks-view')");
    const seen = hitCount('/p1/tasks');
    await click('#refresh-tasks');
    const deadline = Date.now() + 5000;
    while (hitCount('/p1/tasks') === seen && Date.now() < deadline) await sleep(30);
    await sleep(250);
    const mutations = await evaluate('window.__drain()');
    assert(mutations.length === 0, `A refresh with filters active mutated the DOM: ${mutations.slice(0, 6).join('; ')}`);
    await expectTitles('Filters after refresh', ['Queue four', 'Queue six']);

    // ---- Completed view --------------------------------------------------------------------------
    await click('#completed-tasks-tab');
    await waitPage("document.querySelectorAll('#tasks-list .completed-task-row').length === 1", 'filtered completed list');
    await expectTitles('Completed with Kind=code + Delivery=human', ['Done four']);
    assert(await pageStatus() === 'Page 1 of 1 tasks', `Completed filtered total was ${await pageStatus()}.`);
    assert(await filterBar() === '2 filters active', `Completed filter count was ${await filterBar()}.`);
    await choose('#delivery-filter', 'all');
    await expectTitles('Completed Kind=code', ['Done one', 'Done two', 'Done four']);
    await choose('#kind-filter', 'all');
    await choose('#origin-filter', 'agent');
    await expectTitles('Completed Origin=agent', ['Done two', 'Done three', 'Done four']);
    await choose('#delivery-filter', 'human');
    await expectTitles('Completed Origin=agent + Delivery=human', ['Done three', 'Done four']);
    await search('three');
    await expectTitles('Completed filters + search', ['Done three']);
    await search('d1');
    assert(await stateText() === 'Filters hid every completed task that matches this search. Use Clear filters to show completed tasks again.', `Completed empty state was "${await stateText()}".`);
    await search('');
    await choose('#kind-filter', 'general');
    await choose('#origin-filter', 'human');
    assert(await stateText() === 'Filters hid every completed task. Use Clear filters to show completed tasks again.', `Completed empty state was "${await stateText()}".`);
    const completedIndicators = await evaluate("[...document.querySelectorAll('#tasks-list .task-row')].length");
    assert(completedIndicators === 0, 'Completed rows remained under an empty filter.');
    await click('#clear-task-filters');
    await expectTitles('Completed cleared', ['Done one', 'Done two', 'Done three', 'Done four']);
    const doneIndicators = await evaluate("[...document.querySelectorAll('#tasks-list .task-row')].map((row) => [...row.querySelectorAll('.task-attribute')].map((pill) => pill.textContent).join('/'))");
    assert(JSON.stringify(doneIndicators) === JSON.stringify(['code/human', 'code/agent', 'general/agent/human-delivered', 'code/agent/human-delivered']), `Completed row indicators were ${JSON.stringify(doneIndicators)}.`);

    // The filters hold across a switch between views, and the Status filter only counts in the queue.
    await choose('#kind-filter', 'code');
    await click('#task-queue-tab');
    await waitPage("document.querySelectorAll('#tasks-list .task-row').length === 4", 'queue with Kind=code');
    await choose('#status-filter', 'in_progress');
    assert(await filterBar() === '2 filters active', `Queue count with Status was ${await filterBar()}.`);
    await click('#completed-tasks-tab');
    await waitPage("document.querySelectorAll('#tasks-list .completed-task-row').length === 3", 'completed with Kind=code');
    assert(await filterBar() === '1 filter active', `Completed count ignores Status, saw ${await filterBar()}.`);

    // ---- Project change resets every filter ------------------------------------------------------
    await click('#task-queue-tab');
    await waitPage("document.querySelectorAll('#tasks-list .task-row').length === 1", 'queue with Kind=code + Status');
    await choose('#origin-filter', 'agent');
    await choose('#delivery-filter', 'human');
    await choose('#project-select', 'p2');
    await waitPage("document.querySelectorAll('#tasks-list .task-row').length === 1", 'second project queue');
    assert(await filtersAre('all', 'all', 'all') && await evaluate("document.querySelector('#status-filter').value") === 'all', 'Switching project kept a filter.');
    assert(await filterBar() === '', 'The filter bar stayed after switching project.');
    await expectTitles('Second project', ['Beta one']);
    await choose('#project-select', 'p1');
    await waitPage("document.querySelectorAll('#tasks-list .task-row').length === 7", 'first project again');
    assert(await filtersAre('all', 'all', 'all'), 'Returning to a project restored an old filter.');

    // ---- Mobile width: every control stays reachable -----------------------------------------------
    await send('Emulation.setDeviceMetricsOverride', { width: 375, height: 800, deviceScaleFactor: 1, mobile: true });
    await sleep(150);
    const overflow = await evaluate("document.documentElement.scrollWidth > document.documentElement.clientWidth + 1");
    assert(!overflow, 'The task toolbar overflows the page at mobile width.');
    const reach = await evaluate("['#kind-filter', '#origin-filter', '#delivery-filter'].every((selector) => { const box = document.querySelector(selector).getBoundingClientRect(); return box.width > 0 && box.left >= 0 && box.right <= window.innerWidth; })");
    assert(reach, 'A filter select is clipped at mobile width.');
    console.log('ok: task filters');
  } finally {
    socket?.close();
    if (chrome.pid && chrome.exitCode === null) { const exited = once(chrome, 'exit'); chrome.kill('SIGTERM'); await exited; }
    await new Promise((done) => server.close(done));
    await rm(profile, { recursive: true, force: true }).catch(() => {});
    if (launchError) throw launchError;
  }
}

main().catch((error) => { console.error(`FAIL: ${error.message}`); process.exitCode = 1; });
