// Browser test of the task queue's saved views.
//
//   CHROME_BIN=/path/to/chrome [CHROME_ARGS="--no-sandbox"] node scripts/test_task_views.mjs
//
// A fixture server stands in for the coordinator and serves two projects' task lists and a signed-in
// actor that the test can change. The test saves the toolbar as a named view, reloads the page and
// applies it, follows the "differs from the view" marker, renames and deletes views (with the
// confirmation), and checks the name and count limits, that another project or actor sees nothing,
// that corrupt or unknown stored values are ignored while other actors' entries are kept, that the
// feature works for the page when localStorage throws on read or on write, that applying a view makes
// no request, that a refresh with a diverged view records no DOM mutation, and the mobile layout.
// Only Node's standard library and Chrome are used.
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
const actors = {
  'op-1': { id: 'op-1', name: 'Operator', role: 'admin', kind: 'human', session_id: 'browser-1' },
  'op-2': { id: 'op-2', name: 'Second operator', role: 'admin', kind: 'human', session_id: 'browser-2' },
};
let actor = actors['op-1'];
const projects = [
  { id: 'p1', name: 'Alpha', repository_url: 'https://example.invalid/alpha.git', target_branch: 'main' },
  { id: 'p2', name: 'Beta', repository_url: 'https://example.invalid/beta.git', target_branch: 'trunk' },
];
const policy = { id: 'p1', name: 'Alpha', policy_revision: 3, review_mode: 'either', recovery_mode: 'agent', lease_seconds: 600, rules: 'Be careful.', agent_rule_editing: false, automatic_integration: false, allow_subagent_reviews: false, integration_owner: 'integrator' };
const makeTask = (id, title, kind, origin, humanDelivered, day, workStatus = 'ready') => ({
  id, title, description: `Description of ${title}`, revision: 1, kind, origin, human_delivered: humanDelivered,
  lifecycle: workStatus === 'done' ? 'done' : 'open', work_status: workStatus, created_at: `2026-10-0${day}T00:00:00Z`, acceptance_criteria: [],
});
const p1Tasks = [
  makeTask('q1', 'Queue one', 'code', 'human', false, 1),
  makeTask('q2', 'Queue two', 'code', 'agent', false, 2),
  makeTask('q3', 'Queue three', 'general', 'agent', false, 3, 'in_progress'),
  makeTask('q4', 'Queue four', 'code', 'human', true, 4),
  makeTask('q5', 'Queue five', 'general', 'human', true, 5),
  makeTask('q6', 'Queue six', 'code', 'agent', true, 6, 'in_progress'),
  makeTask('d1', 'Done one', 'code', 'human', false, 7, 'done'),
];
const p2Tasks = [makeTask('b1', 'Beta one', 'general', 'service', false, 1)];
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
  const switched = path.match(/^\/__actor\/(op-[12])$/);
  if (switched) { actor = actors[switched[1]]; return json(response, {}); }
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
  // The test sets window.name before a reload to make localStorage fail in one of two ways.
  if (window.name === 'storage-denied') Object.defineProperty(window, 'localStorage', { get() { throw new DOMException('denied', 'SecurityError'); } });
  if (window.name === 'storage-full') Storage.prototype.setItem = function () { throw new DOMException('full', 'QuotaExceededError'); };
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
  const profile = await mkdtemp(join(tmpdir(), 'agent-coordinator-views-test-'));
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

    const pageErrors = [];
    socket.addEventListener('message', ({ data }) => { const message = JSON.parse(data); if (message.method === 'Runtime.exceptionThrown') pageErrors.push(message.params.exceptionDetails.exception?.description || message.params.exceptionDetails.text); });
    await send('Runtime.enable');

    // ---- Helpers over the page -------------------------------------------------------------------
    const base = `http://127.0.0.1:${serverPort}/`;
    const storageKey = 'agent-coordinator.task-views';
    const click = (selector) => evaluate(`document.querySelector(${JSON.stringify(selector)}).click()`);
    const choose = (selector, value) => evaluate(`(() => { const select = document.querySelector(${JSON.stringify(selector)}); select.value = ${JSON.stringify(value)}; select.dispatchEvent(new Event('change', { bubbles: true })); })()`);
    const search = (value) => evaluate(`(() => { const input = document.querySelector('#tasks-search'); input.value = ${JSON.stringify(value)}; input.dispatchEvent(new Event('input', { bubbles: true })); })()`);
    const titles = () => evaluate("[...document.querySelectorAll('#tasks-list .task-row .task-title')].map((node) => node.textContent)");
    const expectTitles = async (label, expected) => {
      await sleep(80);
      const found = await titles();
      assert(JSON.stringify(found) === JSON.stringify(expected), `${label}: expected ${JSON.stringify(expected)}, saw ${JSON.stringify(found)}`);
    };
    const requests = () => hitCount('/api/v1/');
    const toolbar = () => evaluate("['#status-filter', '#kind-filter', '#origin-filter', '#delivery-filter', '#tasks-sort', '#tasks-search'].map((selector) => document.querySelector(selector).value).join('|')");
    const options = () => evaluate("[...document.querySelectorAll('#task-views-select option')].map((option) => option.textContent)");
    const selectedView = () => evaluate("document.querySelector('#task-views-select').value");
    const note = () => evaluate("document.querySelector('#task-views-note').textContent");
    const enabled = (selector) => evaluate(`!document.querySelector(${JSON.stringify(selector)}).disabled`);
    const visible = (selector) => evaluate(`!document.querySelector(${JSON.stringify(selector)}).hidden`);
    const stored = async () => JSON.parse(await evaluate(`localStorage.getItem(${JSON.stringify(storageKey)}) || 'null'`));
    const setStored = (value) => evaluate(`localStorage.setItem(${JSON.stringify(storageKey)}, ${JSON.stringify(typeof value === 'string' ? value : JSON.stringify(value))})`);
    const dialogOpen = () => evaluate("Boolean(document.querySelector('dialog[open]'))");
    const dialogProblem = () => evaluate("(() => { const node = document.querySelector('dialog[open] .inline-alert'); return node && !node.hidden ? node.textContent : ''; })()");
    const typeName = (value) => evaluate(`(() => { const input = document.querySelector('dialog[open] input[name=name]'); input.value = ${JSON.stringify(value)}; input.dispatchEvent(new Event('input', { bubbles: true })); })()`);
    const submitDialog = async () => { await click('dialog[open] button[type=submit]'); await sleep(120); };
    const cancelDialog = async () => { await evaluate("document.querySelector('dialog[open] .dialog-actions .button.subtle').click()"); await sleep(80); };
    const saveView = async (name) => {
      await click('#save-task-view');
      await waitPage("document.querySelector('dialog[open] input[name=name]')", 'save dialog');
      await typeName(name); await submitDialog();
      assert(!await dialogOpen(), `The save dialog stayed open for "${name}": ${await dialogProblem()}`);
    };
    const reload = async (windowName = '') => {
      await evaluate(`window.__old = true; window.name = ${JSON.stringify(windowName)}`);
      await send('Page.navigate', { url: base });
      await waitPage("!window.__old && document.querySelector('.project-card')", 'reloaded project list');
    };
    const openProject = async (rows = 6) => {
      await click('.project-open');
      await waitPage(`document.querySelectorAll('#tasks-list .task-row').length === ${rows}`, 'task queue');
      await choose('#tasks-sort', 'priority'); // The sort alone is remembered across reloads; start each page from the default.
    };
    const setActor = (id) => evaluate(`fetch('/__actor/${id}')`);
    const everything = ['Queue one', 'Queue two', 'Queue three', 'Queue four', 'Queue five', 'Queue six'];

    // ---- Controls ---------------------------------------------------------------------------------
    await openProject();
    await expectTitles('unfiltered queue', everything);
    const labels = await evaluate("[...document.querySelectorAll('#task-views label, #task-views button')].map((node) => node.tagName.toLowerCase() + ':' + node.textContent.replace(/\\s+/g, ' ').trim().replace(/No view applied$/, ''))");
    assert(JSON.stringify(labels) === JSON.stringify(['label:Views', 'button:Save view…', 'button:Reset to view', 'button:Rename', 'button:Delete']), `Unexpected view controls: ${JSON.stringify(labels)}`);
    assert(await evaluate("document.querySelector('label[for=task-views-select]').control.id") === 'task-views-select', 'The Views select has no visible label.');
    assert(JSON.stringify(await options()) === JSON.stringify(['No view applied']), `Initial options were ${JSON.stringify(await options())}.`);
    assert(await enabled('#task-views-select') && await enabled('#save-task-view') && !await enabled('#rename-task-view') && !await enabled('#delete-task-view') && !await visible('#reapply-task-view'), 'Initial control state was wrong.');

    // ---- Save, reload, apply ------------------------------------------------------------------------
    await choose('#kind-filter', 'code'); await choose('#origin-filter', 'agent'); await choose('#tasks-sort', 'newest'); await search('Queue');
    await expectTitles('view A settings', ['Queue six', 'Queue two']);
    const settingsA = await toolbar();
    assert(settingsA === 'all|code|agent|all|newest|Queue', `Toolbar was ${settingsA}.`);
    await saveView('  Agent code  ');
    assert(JSON.stringify(await options()) === JSON.stringify(['No view applied', 'Agent code (active)']), `Options after save were ${JSON.stringify(await options())}.`);
    assert(await note() === 'Applied “Agent code”.' && await enabled('#rename-task-view') && await enabled('#delete-task-view') && !await visible('#reapply-task-view'), 'State after save was wrong.');
    const store = await stored();
    const entry = store['op-1'].p1[0];
    assert(JSON.stringify(Object.keys(store)) === '["op-1"]' && JSON.stringify(Object.keys(store['op-1'])) === '["p1"]' && store['op-1'].p1.length === 1, `Store shape was ${JSON.stringify(store)}.`);
    assert(entry.name === 'Agent code' && JSON.stringify(Object.keys(entry).sort()) === JSON.stringify(['delivery', 'id', 'kind', 'name', 'origin', 'search', 'sort', 'status']), `Stored entry was ${JSON.stringify(entry)}.`);
    assert(`${entry.status}|${entry.kind}|${entry.origin}|${entry.delivery}|${entry.sort}|${entry.search}` === settingsA, 'The stored settings differ from the toolbar.');
    assert(!JSON.stringify(store).includes('Queue six'), 'The store contains task data.');

    // The active view notices when the toolbar diverges, and Reset to view restores it with no request.
    await choose('#tasks-sort', 'oldest');
    await expectTitles('diverged sort', ['Queue two', 'Queue six']);
    assert(await note() === 'The toolbar differs from “Agent code”.' && await visible('#reapply-task-view'), `Divergence note was "${await note()}".`);
    assert((await options())[1] === 'Agent code (active, modified)', `Active option was ${(await options())[1]}.`);
    let before = requests();
    await click('#reapply-task-view');
    await expectTitles('reset to view', ['Queue six', 'Queue two']);
    assert(await toolbar() === settingsA && await note() === 'Applied “Agent code”.' && !await visible('#reapply-task-view'), 'Reset to view did not restore the view.');
    assert(await evaluate("document.activeElement.id") === 'task-views-select', 'Focus was lost after Reset to view.');
    assert(requests() === before, 'Reset to view made a request.');
    await search('Queue seven');
    assert((await note()).startsWith('The toolbar differs'), 'A changed search was not flagged.');
    await search('Queue');

    // A refresh with an active, diverged view records no DOM mutation.
    await choose('#kind-filter', 'all');
    await evaluate("window.__observe('#tasks-view')");
    const seen = hitCount('/p1/tasks');
    await click('#refresh-tasks');
    const deadline = Date.now() + 5000;
    while (hitCount('/p1/tasks') === seen && Date.now() < deadline) await sleep(30);
    await sleep(250);
    const mutations = await evaluate('window.__drain()');
    assert(mutations.length === 0, `A refresh with a diverged view mutated the DOM: ${mutations.slice(0, 6).join('; ')}`);

    // After a reload the view is listed, inactive, and applies to the same list.
    await reload();
    await openProject();
    assert(JSON.stringify(await options()) === JSON.stringify(['No view applied', 'Agent code']) && await selectedView() === '', 'The saved view was not listed after reload.');
    await expectTitles('after reload', everything);
    before = requests();
    await choose('#task-views-select', entry.id);
    await expectTitles('applied after reload', ['Queue six', 'Queue two']);
    assert(await toolbar() === settingsA, `Toolbar after apply was ${await toolbar()}.`);
    assert(requests() === before, 'Applying a view made a request.');
    assert(await evaluate("document.querySelector('#task-filter-count').textContent") === '2 filters active', 'The filter bar did not follow the applied view.');
    await click('#completed-tasks-tab');
    await waitPage("document.querySelectorAll('#tasks-list .completed-task-row').length === 0 || document.querySelector('#tasks-state:not([hidden])')", 'completed view');
    assert(await selectedView() === entry.id, 'The active view was lost in the completed view.');
    await click('#task-queue-tab');
    await waitPage("document.querySelectorAll('#tasks-list .task-row').length === 2", 'queue again');
    // Choosing "No view applied" leaves the toolbar alone.
    await choose('#task-views-select', '');
    assert(await toolbar() === settingsA && await note() === '1 of 20 views saved.' && !await enabled('#rename-task-view'), 'Deselecting a view changed the toolbar.');
    await click('#clear-task-filters');
    await search('');

    // ---- Name rules -------------------------------------------------------------------------------
    await choose('#status-filter', 'in_progress'); await choose('#delivery-filter', 'human'); await choose('#tasks-sort', 'oldest');
    await expectTitles('view B settings', ['Queue six']);
    await click('#save-task-view');
    await waitPage("document.querySelector('dialog[open] input[name=name]')", 'save dialog');
    await typeName('   ');
    assert(await dialogProblem() === 'Enter a name for the view.', `Blank problem was "${await dialogProblem()}".`);
    await submitDialog();
    assert(await dialogOpen(), 'A blank name closed the dialog.');
    await typeName('x'.repeat(61));
    assert(await dialogProblem() === 'View names can have at most 60 characters; this one has 61.', `Long-name problem was "${await dialogProblem()}".`);
    await submitDialog();
    assert(await dialogOpen() && (await options()).length === 2, 'A 61-character name was saved.');
    await typeName('agent CODE');
    assert((await dialogProblem()).startsWith('A view named “agent CODE” already exists'), `Duplicate problem was "${await dialogProblem()}".`);
    await submitDialog();
    assert(await dialogOpen() && (await options()).length === 2, 'A duplicate name was saved.');
    await typeName('y'.repeat(60));
    assert(await dialogProblem() === '', 'A 60-character name was rejected.');
    await submitDialog();
    assert(!await dialogOpen() && (await options())[2] === `${'y'.repeat(60)} (active)`, 'The 60-character name was not saved.');

    // ---- Rename -----------------------------------------------------------------------------------
    await click('#rename-task-view');
    await waitPage("document.querySelector('dialog[open] input[name=name]')", 'rename dialog');
    assert(await evaluate("document.querySelector('dialog[open] input[name=name]').value") === 'y'.repeat(60), 'The rename dialog did not start from the current name.');
    await typeName('Agent code');
    assert((await dialogProblem()).includes('already exists'), 'Renaming onto another view was allowed.');
    await submitDialog();
    assert(await dialogOpen(), 'A duplicate rename closed the dialog.');
    await typeName('In progress');
    await submitDialog();
    assert(!await dialogOpen() && JSON.stringify(await options()) === JSON.stringify(['No view applied', 'Agent code', 'In progress (active)']), `Options after rename were ${JSON.stringify(await options())}.`);
    assert(await toolbar() === 'in_progress|all|all|human|oldest|', 'Rename changed the toolbar.');
    const afterRename = (await stored())['op-1'].p1;
    assert(afterRename.map((view) => view.name).join() === 'Agent code,In progress' && afterRename[1].status === 'in_progress' && afterRename[1].id !== entry.id, `Stored views after rename: ${JSON.stringify(afterRename)}.`);

    // ---- Scoping ----------------------------------------------------------------------------------
    await choose('#project-select', 'p2');
    await waitPage("document.querySelectorAll('#tasks-list .task-row').length === 1", 'second project');
    assert(JSON.stringify(await options()) === JSON.stringify(['No view applied']) && await note() === '', 'Another project saw the views.');
    await choose('#project-select', 'p1');
    await waitPage("document.querySelectorAll('#tasks-list .task-row').length === 6", 'first project again');
    assert(JSON.stringify(await options()) === JSON.stringify(['No view applied', 'Agent code', 'In progress']) && await selectedView() === '', 'Returning to the project lost the views or kept one active.');
    await setActor('op-2');
    await reload();
    await openProject();
    assert(JSON.stringify(await options()) === JSON.stringify(['No view applied']), 'Another actor saw the views.');
    await choose('#kind-filter', 'general');
    await saveView('Mine');
    const shared = await stored();
    assert(shared['op-1'].p1.length === 2 && shared['op-2'].p1.length === 1, `Both actors' views should be kept apart: ${JSON.stringify(Object.keys(shared))}.`);
    await setActor('op-1');
    await reload();
    await openProject();
    assert(JSON.stringify(await options()) === JSON.stringify(['No view applied', 'Agent code', 'In progress']), 'The first actor lost their views.');

    // ---- Delete ------------------------------------------------------------------------------------
    await choose('#task-views-select', afterRename[1].id);
    await click('#delete-task-view');
    await waitPage("document.querySelector('dialog[open]')", 'delete dialog');
    const confirmation = await evaluate("document.querySelector('dialog[open]').textContent");
    assert(confirmation.includes('Delete the saved view “In progress”?') && confirmation.includes('cannot be undone'), `Delete confirmation was "${confirmation}".`);
    await cancelDialog();
    assert(!await dialogOpen() && (await options()).length === 3 && (await stored())['op-1'].p1.length === 2, 'Cancel still deleted the view.');
    await click('#delete-task-view');
    await waitPage("document.querySelector('dialog[open]')", 'delete dialog');
    await submitDialog();
    assert(JSON.stringify(await options()) === JSON.stringify(['No view applied', 'Agent code']) && await selectedView() === '' && !await enabled('#delete-task-view'), `Options after delete were ${JSON.stringify(await options())}.`);
    assert((await stored())['op-1'].p1.map((view) => view.name).join() === 'Agent code' && (await stored())['op-2'].p1.length === 1, 'The store after delete was wrong.');
    assert(await evaluate("document.activeElement.id") === 'task-views-select', 'Focus was lost after delete.');

    // ---- Count limit ------------------------------------------------------------------------------
    const makeEntry = (n) => ({ id: `seed-${n}`, name: `Seed ${n}`, status: 'all', kind: 'all', origin: 'all', delivery: 'all', search: '', sort: 'priority' });
    await setStored({ 'op-1': { p1: Array.from({ length: 20 }, (_, n) => makeEntry(n)) } });
    await reload();
    await openProject();
    assert((await options()).length === 21 && await note() === '20 of 20 views saved.', 'Twenty views were not all listed.');
    await click('#save-task-view');
    await waitPage("document.querySelector('dialog[open] input[name=name]')", 'save dialog');
    await typeName('One too many');
    assert(await dialogProblem() === 'This project already has 20 saved views. Delete one before saving another.', `Limit problem was "${await dialogProblem()}".`);
    await submitDialog();
    assert(await dialogOpen() && (await options()).length === 21 && (await stored())['op-1'].p1.length === 20, 'A 21st view was saved.');
    await cancelDialog();
    await setStored({ 'op-1': { p1: [] } });
    await reload();
    await openProject();
    await search('x'.repeat(201));
    await click('#save-task-view');
    await waitPage("document.querySelector('dialog[open] input[name=name]')", 'save dialog');
    assert((await dialogProblem()).startsWith('The search text has 201 characters'), `Long-search problem was "${await dialogProblem()}".`);
    await cancelDialog();

    // ---- Corrupt or unknown stored values ---------------------------------------------------------
    const good = { ...makeEntry(1), name: 'Kept', kind: 'code', sort: 'newest', search: 'Queue' };
    const expectOnly = async (label, names) => {
      await reload();
      await openProject();
      const found = (await options()).slice(1);
      assert(JSON.stringify(found) === JSON.stringify(names), `${label}: expected ${JSON.stringify(names)}, saw ${JSON.stringify(found)}.`);
      await expectTitles(`${label} task list`, everything);
      assert(pageErrors.length === 0, `${label} raised ${pageErrors.join('; ')}.`);
    };
    for (const [label, value] of [['not JSON', 'not json{'], ['array', '[]'], ['string', '"text"'], ['number', '7'], ['actor not an object', '{"op-1":5}'], ['project not a list', '{"op-1":{"p1":"nope"}}'], ['all entries bad', '{"op-1":{"p1":[null,1,"x",[]]}}']]) {
      await setStored(value);
      await expectOnly(label, []);
    }
    const mixed = [
      good,
      { ...makeEntry(2), kind: 'robot' }, { ...makeEntry(3), status: 'nope' }, { ...makeEntry(4), origin: 5 }, { ...makeEntry(5), delivery: null },
      { ...makeEntry(6), sort: 'sideways' }, { ...makeEntry(7), name: 'n'.repeat(61) }, { ...makeEntry(8), name: '   ' }, { ...makeEntry(9), name: 'Bell\u0007' },
      { ...makeEntry(10), id: undefined }, { ...makeEntry(11), id: 'has space' }, { ...makeEntry(12), search: 42 }, { ...makeEntry(13), search: 'x'.repeat(201) },
      { ...makeEntry(14), kind: undefined }, { ...good, name: 'Same id' }, { ...makeEntry(15), name: 'kept' }, null, 3, 'x', [],
      { ...makeEntry(16), name: '  Padded  ', status: 'blocked', origin: 'service', delivery: 'supervisor', sort: 'least-recent-progress' },
    ];
    await setStored(`{"__proto__":{"p1":[]},"op-2":{"p1":[${JSON.stringify(makeEntry(99))}]},"op-1":{"p2":[${JSON.stringify(makeEntry(98))}],"p1":${JSON.stringify(mixed)}}}`);
    await expectOnly('mixed entries', ['Kept', 'Padded']);
    await choose('#task-views-select', 'seed-1');
    await expectTitles('the surviving view still applies', ['Queue six', 'Queue four', 'Queue two', 'Queue one']);
    await choose('#kind-filter', 'all'); await search('');
    await saveView('After corruption');
    const repaired = await stored();
    assert(repaired['op-1'].p1.map((view) => view.name).join() === 'Kept,Padded,After corruption', `Saving did not drop the ignored entries: ${JSON.stringify(repaired['op-1'].p1.map((view) => view.name))}.`);
    assert(repaired['op-2'].p1[0].id === 'seed-99' && repaired['op-1'].p2[0].id === 'seed-98', 'Saving removed another actor\'s or project\'s views.');
    await setStored({ 'op-1': { p1: Array.from({ length: 25 }, (_, n) => makeEntry(n)) } });
    await reload();
    await openProject();
    assert((await options()).length === 21, 'More than 20 stored views were loaded.');
    await setStored('null');

    // ---- localStorage that throws ---------------------------------------------------------------------
    for (const [mode, expected] of [['storage-denied', 'This browser cannot store views. Views you save last until you reload.'], ['storage-full', 'The browser refused to store views. Views you save last until you reload.']]) {
      await reload(mode);
      await openProject();
      await expectTitles(`${mode} task list`, everything);
      await choose('#kind-filter', 'code'); await choose('#tasks-sort', 'newest');
      await saveView('Session only');
      assert(JSON.stringify(await options()) === JSON.stringify(['No view applied', 'Session only (active)']), `${mode}: options were ${JSON.stringify(await options())}.`);
      assert(await note() === expected, `${mode}: note was "${await note()}".`);
      await click('#clear-task-filters');
      await choose('#task-views-select', '');
      await choose('#task-views-select', await selectedView() || await evaluate("document.querySelector('#task-views-select option:last-child').value"));
      await expectTitles(`${mode} apply`, ['Queue six', 'Queue four', 'Queue two', 'Queue one']);
      assert(pageErrors.length === 0, `${mode} raised ${pageErrors.join('; ')}.`);
    }
    await reload();
    await openProject();
    assert(JSON.stringify(await options()) === JSON.stringify(['No view applied']), 'A view that could not be stored came back after a reload.');

    // ---- Mobile width ------------------------------------------------------------------------------
    await send('Emulation.setDeviceMetricsOverride', { width: 375, height: 800, deviceScaleFactor: 1, mobile: true });
    await sleep(150);
    assert(!await evaluate("document.documentElement.scrollWidth > document.documentElement.clientWidth + 1"), 'The views controls overflow the page at mobile width.');
    assert(await evaluate("['#task-views-select', '#save-task-view'].every((selector) => { const box = document.querySelector(selector).getBoundingClientRect(); return box.width > 0 && box.left >= 0 && box.right <= window.innerWidth; })"), 'A views control is clipped at mobile width.');
    assert(pageErrors.length === 0, `The page raised ${pageErrors.join('; ')}.`);
    console.log('ok: task views');
  } finally {
    socket?.close();
    if (chrome.pid && chrome.exitCode === null) { const exited = once(chrome, 'exit'); chrome.kill('SIGTERM'); await exited; }
    await new Promise((done) => server.close(done));
    await rm(profile, { recursive: true, force: true }).catch(() => {});
    if (launchError) throw launchError;
  }
}

main().catch((error) => { console.error(`FAIL: ${error.message}`); process.exitCode = 1; });
