// Browser test of the Resources view's reservation History dialog.
//
//   CHROME_BIN=/path/to/chrome [CHROME_ARGS="--no-sandbox"] node scripts/test_resource_history.mjs
//
// A fixture server stands in for the coordinator and serves a two-page reservation trail. The test
// opens the dialog from the keyboard, checks the current holds and the released/resolved entries,
// requires exactly one request per open and per Load more, follows task links into their own
// projects, and checks the phone-width layout. Only Node's standard library and Chrome are used.
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

const actor = { id: 'op-1', name: 'Operator', role: 'admin', kind: 'human', session_id: 'browser-1' };
const projects = [
  { id: 'p1', name: 'Alpha', repository_url: 'https://example.invalid/alpha.git', target_branch: 'main' },
  { id: 'p2', name: 'Beta', repository_url: 'https://example.invalid/beta.git', target_branch: 'main' },
];
const resource = (id, key, held, capacity) => ({ id, key, capacity, held_units: held, description: `Fixture ${key}`, created_at: '2026-10-01T00:00:00Z' });
const resources = [resource('res-gpu', 'device/gpu-0', 2, 4), resource('res-idle', 'device/idle', 0, 1), resource('res-fail', 'device/broken', 0, 1)];
const reservation = (id, project, projectName, task, title, stored, extra = {}) => ({
  id, project_id: project, project_name: projectName, attempt_id: `attempt-${id}`, task_id: task, task_title: title, generation: 1,
  state: stored, stored_state: stored, items: [{ resource_id: 'res-gpu', key: 'device/gpu-0', units: 1 }, { resource_id: 'res-other', key: 'device/other', units: 3 }],
  created_at: '2026-10-05T10:00:00Z', released_at: null, release_reason: null, resolved_at: null, resolution_reason: null, resolution_evidence: null, ...extra,
});
// Two pages, newest first. The second current hold is older than the whole first page.
const pages = new Map([
  ['', { items: [
    reservation('resv-new', 'p2', 'Beta', 'beta-task', 'Beta benchmark run', 'held'),
    reservation('resv-rel', 'p1', 'Alpha', 'task-1', 'Alpha smoke test', 'released', { released_at: '2026-10-04T12:00:00Z', release_reason: 'Bench finished cleanly' }),
  ], next_cursor: 'cursor-2' }],
  ['cursor-2', { items: [
    reservation('resv-old', 'p1', 'Alpha', 'task-2', 'Alpha long soak', 'held', { created_at: '2026-10-03T08:00:00Z' }),
    reservation('resv-res', 'p1', 'Alpha', 'task-3', 'Alpha crashed runner', 'resolved', { created_at: '2026-10-02T08:00:00Z', resolved_at: '2026-10-02T18:00:00Z', resolution_reason: 'Runner host rebuilt', resolution_evidence: 'Host wiped and process list empty' }),
  ], next_cursor: null }],
]);
const makeTask = (id, title) => ({ id, title, description: title, revision: 1, kind: 'code', lifecycle: 'open', work_status: 'ready', created_at: '2026-10-01T00:00:00Z', acceptance_criteria: [], attempts: [], checkpoints: [], workflow: { phase: 'open', blockers: [], next_actions: [], submission: null, activities: [] }, job_evidence: { jobs: [], reservations: [] }, lifecycle_events: [], replaces: [] });
const requests = [];

const files = { '/': ['web/index.html', 'text/html; charset=utf-8'], '/app.js': ['web/app.js', 'text/javascript; charset=utf-8'], '/style.css': ['web/style.css', 'text/css; charset=utf-8'] };
async function route(request, response) {
  const url = new URL(request.url, 'http://fixture.invalid');
  const path = url.pathname;
  const json = (data) => { response.writeHead(200, { 'Content-Type': 'application/json' }); response.end(JSON.stringify({ data })); };
  if (files[path]) { response.writeHead(200, { 'Content-Type': files[path][1], 'Cache-Control': 'no-store' }); return response.end(await readFile(join(root, files[path][0]))); }
  requests.push(`${request.method} ${path}${url.search}`);
  if (path === '/api/v1/me') return json({ actor, csrf_token: 'csrf' });
  if (path === '/api/v1/projects') return json({ items: projects });
  if (path === '/api/v1/resources') return json({ items: resources, next_cursor: null });
  const trail = path.match(/^\/api\/v1\/resources\/([^/]+)\/reservations$/);
  if (trail) {
    if (trail[1] === 'res-idle') return json({ items: [], next_cursor: null });
    if (trail[1] === 'res-fail') { response.writeHead(500, { 'Content-Type': 'application/json' }); return response.end(JSON.stringify({ error: { code: 'internal', message: 'Trail unavailable' } })); }
    return json(pages.get(url.searchParams.get('cursor') || ''));
  }
  const task = path.match(/^\/api\/v1\/projects\/(p1|p2)\/tasks\/([^/]+)$/);
  if (task) return json(makeTask(task[2], task[2] === 'beta-task' ? 'Beta benchmark run' : task[2]));
  if (path.endsWith('/orientation')) return json({ project: projects[0], policy_revision: 1 });
  if (path.startsWith('/api/v1/')) return json({ items: [], next_cursor: null });
  response.writeHead(404); response.end();
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

async function main() {
  const server = createServer((request, response) => route(request, response).catch((error) => { response.writeHead(500); response.end(String(error)); }));
  server.listen(0, '127.0.0.1');
  await once(server, 'listening');
  const serverPort = server.address().port;
  const debugPort = await unusedPort();
  const profile = await mkdtemp(join(tmpdir(), 'agent-coordinator-resource-history-'));
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
    const shown = () => evaluate("[...document.querySelectorAll('.page-view')].filter((view) => !view.hidden).map((view) => view.id.replace(/-view$/, '')).join(',')");
    const expectPage = async (hash, view, label) => {
      await waitPage(`location.hash === ${JSON.stringify(hash)} && [...document.querySelectorAll('.page-view')].filter((view) => !view.hidden).map((page) => page.id).join() === '${view}-view'`, label);
      assert((await shown()) === view, `${label}: expected only the ${view} view, saw ${await shown()}`);
    };
    const clickTarget = (spec) => evaluate(spec.startsWith('[') ? `(${spec}).click()` : `document.querySelector(${JSON.stringify(spec)}).click()`);

    const trailRequests = () => requests.filter((line) => /\/api\/v1\/resources\/[^/]+\/reservations/.test(line));
    const press = async (key, code, keyCode) => {
      await send('Input.dispatchKeyEvent', { type: 'keyDown', key, code, windowsVirtualKeyCode: keyCode, text: key === 'Enter' ? '\r' : '' });
      await send('Input.dispatchKeyEvent', { type: 'keyUp', key, code, windowsVirtualKeyCode: keyCode });
    };
    const names = (selector) => evaluate(`JSON.stringify([...document.querySelectorAll(${JSON.stringify(selector)} + ' .history-entry .history-title .text-button')].map((node) => node.textContent))`);
    const LOAD_MORE = "document.querySelector('dialog[open] .history-body > .button.subtle')";
    const openResources = async () => {
      await evaluate("document.querySelector('.nav-item[data-view=\"resources\"]').click()");
      await waitPage("document.querySelectorAll('#resources-list tr').length === 3", 'resource rows');
    };
    const openHistory = async (key) => {
      await evaluate(`[...document.querySelectorAll('#resources-list tr')].find((row) => row.textContent.includes(${JSON.stringify(key)})).querySelector('button[aria-label^="History for resource"]').click()`);
      await waitPage("document.querySelector('dialog[open].history-dialog')", `history dialog for ${key}`);
    };

    await send('Page.enable');
    await send('Page.navigate', { url: `http://127.0.0.1:${serverPort}/` });
    await waitPage("document.querySelector('.project-card')", 'project list');
    await openResources();

    // The row action is a visible, labelled, keyboard-reachable button.
    assert(await evaluate("[...document.querySelectorAll('#resources-list tr')].every((row) => row.querySelector('button[aria-label^=\"History for resource\"]')?.textContent === 'History')"), 'Every resource row needs a visible History button.');
    await evaluate("document.querySelector('#resources-list tr button[aria-label^=\"History for resource\"]').focus()");
    await press('Enter', 'Enter', 13);
    await waitPage("document.querySelector('dialog[open].history-dialog #resource-history-holds .history-entry')", 'dialog opened from the keyboard');
    assert(trailRequests().length === 1 && trailRequests()[0].includes('/api/v1/resources/res-gpu/reservations') && trailRequests()[0].includes('limit='), `Opening requested ${trailRequests().length} pages.`);

    // Current holds first, with project, task, units and since; then the released/resolved trail.
    assert(await evaluate("document.querySelector('dialog[open]').getAttribute('aria-labelledby') === 'resource-history-heading'"), 'The dialog has no accessible name.');
    assert(await names('#resource-history-holds') === '["Beta benchmark run"]', 'Current holds on the first page are wrong.');
    assert(await names('#resource-history-past') === '["Alpha smoke test"]', 'Historical entries on the first page are wrong.');
    const hold = await evaluate("document.querySelector('#resource-history-holds .history-entry').textContent");
    assert(hold.includes('Beta') && hold.includes('Beta benchmark run') && hold.includes('1 of 4') && hold.includes('Since') && hold.includes('Held'), `Current hold lacks project, task, units or since: ${hold}`);
    assert(!hold.includes('3 of 4'), 'Units of another resource in the same reservation were shown.');
    const released = await evaluate("document.querySelector('#resource-history-past .history-entry').textContent");
    assert(released.includes('Released') && released.includes('Bench finished cleanly'), `Released entry lacks its reason: ${released}`);
    assert(await evaluate("(() => { const note = document.querySelector('dialog[open] .inline-alert'); return !note.hidden && note.textContent.includes('2 units are held in total, but only 1'); })()"), 'The older-holds notice is missing.');
    assert(await evaluate(`!${LOAD_MORE}.hidden`), 'Load more is hidden while a cursor remains.');

    // Load more fetches exactly one further page and places the older hold with the current holds.
    await evaluate(`${LOAD_MORE}.click()`);
    await waitPage("document.querySelectorAll('dialog[open] .history-entry').length === 4", 'second page');
    assert(trailRequests().length === 2 && trailRequests()[1].includes('cursor=cursor-2'), `Load more requested ${trailRequests().length - 1} pages.`);
    assert(await names('#resource-history-holds') === '["Beta benchmark run","Alpha long soak"]', 'The older hold did not join the current holds.');
    assert(await names('#resource-history-past') === '["Alpha smoke test","Alpha crashed runner"]', 'The resolved entry did not follow the released one.');
    const resolved = await evaluate("document.querySelectorAll('#resource-history-past .history-entry')[1].textContent");
    assert(resolved.includes('Resolved') && resolved.includes('Runner host rebuilt') && resolved.includes('Host wiped and process list empty'), `Resolved entry lacks reason or evidence: ${resolved}`);
    assert(await evaluate(`${LOAD_MORE}.hidden && document.querySelector('dialog[open] .inline-alert').hidden`), 'Load more or the notice stayed after the last page.');
    await sleep(1200);
    assert(trailRequests().length === 2, 'The dialog requested pages without a user action.');
    assert(!/csrf|token|secret|bearer|acr_/i.test(await evaluate("document.querySelector('dialog[open]').textContent")), 'The dialog shows credential-like text.');

    // Phone width: no horizontal overflow and tap-sized controls.
    await send('Emulation.setDeviceMetricsOverride', { width: 375, height: 700, deviceScaleFactor: 1, mobile: true });
    assert(await evaluate("(() => { const body = document.querySelector('dialog[open] .history-body'); return body.scrollWidth <= body.clientWidth && document.documentElement.scrollWidth <= window.innerWidth; })()"), 'The dialog overflows at phone width.');
    assert(await evaluate("[...document.querySelectorAll('dialog[open] .history-entry .text-button, dialog[open] .dialog-actions .button')].every((node) => node.getBoundingClientRect().height >= 36)"), 'History controls are too small at phone width.');
    await send('Emulation.clearDeviceMetricsOverride');

    // Escape closes the dialog and leaves nothing behind.
    await press('Escape', 'Escape', 27);
    await waitPage("!document.querySelector('dialog')", 'Escape to close');

    // A task link opens that task in its own project, not the selected one.
    await openHistory('device/gpu-0');
    await waitPage("document.querySelector('#resource-history-holds .history-entry')", 'reopened trail');
    assert(trailRequests().length === 3, 'Reopening did not request exactly one page.');
    await evaluate("document.querySelector('#resource-history-holds .history-entry .text-button').click()");
    await waitPage("location.hash === '#/projects/p2/task/beta-task' && !document.querySelector('#task-detail-view').hidden && !document.querySelector('dialog')", 'task detail in project Beta');
    await waitPage("document.querySelector('#task-detail-heading').textContent === 'Beta benchmark run'", 'task heading');
    assert(requests.includes('GET /api/v1/projects/p2/tasks/beta-task'), 'The task was not loaded from its own project.');

    // An older hold from the second page opens in project Alpha.
    await openResources();
    await openHistory('device/gpu-0');
    await waitPage("document.querySelector('#resource-history-holds .history-entry')", 'trail after navigation');
    await evaluate(`${LOAD_MORE}.click()`);
    await waitPage("document.querySelectorAll('dialog[open] .history-entry').length === 4", 'second page again');
    await evaluate("[...document.querySelectorAll('#resource-history-holds .text-button')].find((node) => node.textContent === 'Alpha long soak').click()");
    await waitPage("location.hash === '#/projects/p1/task/task-2' && !document.querySelector('dialog')", 'task detail in project Alpha');

    // A resource nobody reserved says so, and a failing request is explained inside the dialog.
    await openResources();
    await openHistory('device/idle');
    await waitPage("document.querySelector('dialog[open]').textContent.includes('never been reserved')", 'empty trail message');
    assert(await evaluate(`${LOAD_MORE}.hidden`), 'Load more shown for an empty trail.');
    await press('Escape', 'Escape', 27);
    await waitPage("!document.querySelector('dialog')", 'Escape from the empty trail');
    await openHistory('device/broken');
    await waitPage("document.querySelector('dialog[open] .state-panel.error')?.textContent.includes('Trail unavailable')", 'trail failure message');
    await press('Escape', 'Escape', 27);
    await waitPage("!document.querySelector('dialog')", 'Escape from the failed trail');
    console.log('PASS: the History dialog showed current holds before released and resolved entries with their reasons and evidence, fetched one page per open and per Load more, opened tasks in their own projects, reported empty and failed trails, and fit a phone width.');
  } finally {
    socket?.close();
    if (chrome.pid && chrome.exitCode === null) { const exited = once(chrome, 'exit'); chrome.kill('SIGTERM'); await exited; }
    await new Promise((done) => server.close(done));
    await rm(profile, { recursive: true, force: true });
    if (launchError) throw launchError;
  }
}

main().catch((error) => { console.error(`FAIL: ${error.message}`); process.exitCode = 1; });
