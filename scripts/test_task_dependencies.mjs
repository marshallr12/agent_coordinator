// Browser test of the task detail Dependencies card: prerequisites ("Waiting on") and dependents ("Holding up")
// with status badges, each entry opening that task, the empty message, the truncation note, polling that
// leaves an unchanged card untouched, and phone width.
//
//   CHROME_BIN=/path/to/chrome [CHROME_ARGS="--no-sandbox"] node scripts/test_task_dependencies.mjs
//
// A fixture server stands in for the coordinator. Only Node's standard library and Chrome are used.
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
const projects = [{ id: 'p1', name: 'Alpha', repository_url: 'https://example.invalid/alpha.git', target_branch: 'main' }];
const policy = { id: 'p1', name: 'Alpha', policy_revision: 3, review_mode: 'either', recovery_mode: 'agent', lease_seconds: 600, rules: 'Be careful.', agent_rule_editing: false, automatic_integration: true };
const relation = (id, title, workStatus, lifecycle = 'open') => ({ id, title, work_status: workStatus, lifecycle });
const makeTask = (id, title, workStatus, extra = {}) => ({ id, title, description: `Description of ${title}`, revision: 1, kind: 'code', lifecycle: workStatus === 'done' ? 'done' : 'open', work_status: workStatus, created_at: '2026-10-12T00:00:00Z', acceptance_criteria: ['One'], current_attempt_id: null, depends_on: [], prerequisites: [], dependents: [], dependents_truncated: false, ...extra });
// A chain A <- B <- C (B waits on A, C waits on B), a task with no relations, and a task with 50 dependents cut short.
const manyDependents = Array.from({ length: 50 }, (_, index) => relation(`t-many-${index}`, `Dependent ${index + 1}`, 'blocked'));
const tasks = [
  makeTask('t-a', 'Task A', 'done', { dependents: [relation('t-b', 'Task B', 'ready')] }),
  makeTask('t-b', 'Task B', 'ready', { depends_on: ['t-a'], prerequisites: [relation('t-a', 'Task A', 'done', 'done')], dependents: [relation('t-c', 'Task C', 'blocked')] }),
  makeTask('t-c', 'Task C', 'blocked', { depends_on: ['t-b'], prerequisites: [relation('t-b', 'Task B', 'ready')] }),
  makeTask('t-alone', 'Lonely task', 'ready'),
  makeTask('t-fan', 'Fan-out task', 'ready', { dependents: manyDependents, dependents_truncated: true }),
  ...manyDependents.map((item) => makeTask(item.id, item.title, 'blocked', { depends_on: ['t-fan'], prerequisites: [relation('t-fan', 'Fan-out task', 'ready')] })),
];
// The queue lists only these; every task above is still served by id.
const listed = ['t-b', 't-c', 't-alone', 't-fan'];
const detailOf = (task) => ({ ...task, attempts: [], checkpoints: [], workflow: { phase: null, blockers: [], submission: null, activities: [], next_actions: [] }, job_evidence: { jobs: [], reservations: [] }, lifecycle_events: [], replaces: [] });

function json(response, data, status = 200) {
  response.writeHead(status, { 'Content-Type': 'application/json', 'Cache-Control': 'no-store' });
  response.end(JSON.stringify({ data }));
}
async function route(request, response) {
  const path = new URL(request.url, 'http://fixture.invalid').pathname;
  const files = { '/': ['web/index.html', 'text/html; charset=utf-8'], '/app.js': ['web/app.js', 'text/javascript; charset=utf-8'], '/style.css': ['web/style.css', 'text/css; charset=utf-8'] };
  if (files[path]) {
    response.writeHead(200, { 'Content-Type': files[path][1], 'Cache-Control': 'no-store' });
    return response.end(await readFile(join(root, files[path][0])));
  }
  if (path === '/api/v1/me') return json(response, { actor, csrf_token: 'csrf' });
  if (path === '/api/v1/projects') return json(response, { items: projects });
  const project = path.match(/^\/api\/v1\/projects\/p1(\/.*)?$/);
  if (project) {
    const rest = project[1] || '';
    if (rest === '/orientation') return json(response, { project: policy, policy_revision: policy.policy_revision });
    if (rest === '/tasks') return json(response, { items: tasks.filter((task) => listed.includes(task.id)), next_cursor: null });
    if (rest === '/tasks/archived') return json(response, { items: [], next_cursor: null });
    const one = rest.match(/^\/tasks\/([^/]+)$/);
    if (one && tasks.find((task) => task.id === one[1])) return json(response, detailOf(tasks.find((task) => task.id === one[1])));
    return json(response, { items: [], next_cursor: null });
  }
  json(response, { items: [], next_cursor: null });
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
  const profile = await mkdtemp(join(tmpdir(), 'agent-coordinator-status-test-'));
  const executable = process.env.CHROME_BIN || 'chromium';
  const extra = (process.env.CHROME_ARGS || '').split(/\s+/).filter(Boolean);
  const chrome = spawn(executable, ['--headless=new', '--disable-gpu', '--no-first-run', '--no-default-browser-check', '--disable-background-networking', '--window-size=1200,800', `--remote-debugging-port=${debugPort}`, `--user-data-dir=${profile}`, ...extra], { stdio: 'ignore' });
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
    const waitPage = async (expression, label, ms = 12000) => {
      const deadline = Date.now() + ms;
      while (Date.now() < deadline) { if (await evaluate(expression)) return; await sleep(30); }
      throw new Error(`Timed out waiting for ${label}`);
    };
    const shot = async (name) => {
      if (!process.env.UI_SHOTS) return;
      const { data } = await send('Page.captureScreenshot', { format: 'png' });
      await writeFile(join(process.env.UI_SHOTS, `${name}.png`), Buffer.from(data, 'base64'));
    };
    await send('Page.enable');
    await send('Page.navigate', { url: `http://127.0.0.1:${serverPort}/` });
    await waitPage("document.querySelector('.project-card')", 'project list');
    await evaluate("document.querySelector('.project-open').click()");
    await waitPage(`document.querySelectorAll('#tasks-list .task-row').length === ${listed.length}`, 'task list');

    const openFromList = async (title) => {
      await evaluate(`[...document.querySelectorAll('#tasks-list .task-row')].find((row) => row.querySelector('.task-title').textContent === ${JSON.stringify(title)}).click()`);
      await waitPage(`!document.querySelector('#task-detail-view').hidden && document.querySelector('#task-detail-heading').textContent === ${JSON.stringify(title)} && document.querySelector('#dependencies-content').children.length`, `detail of ${title}`);
    };
    // The card's columns as {heading: [{title, badge}]}, plus any note and empty message.
    const card = () => evaluate(`(() => {
      const content = document.querySelector('#dependencies-content');
      const column = (selector) => { const section = content.querySelector(selector); return section ? { items: [...section.querySelectorAll('.dependency-item')].map((item) => ({ title: item.querySelector('.dependency-open').textContent, badge: item.querySelector('.status-badge').textContent, label: item.querySelector('.dependency-open').getAttribute('aria-label') })), heading: section.querySelector('h3').textContent, empty: section.querySelector('p.muted')?.textContent || null } : null; };
      return { waiting: column('.dependency-waiting'), holding: column('.dependency-holding'), note: content.querySelector('.dependency-truncated')?.textContent || null, message: content.querySelector(':scope > p.muted')?.textContent || null };
    })()`);
    const backToList = async () => {
      await evaluate("document.querySelector('#back-to-tasks').click()");
      await waitPage(`document.querySelectorAll('#tasks-list .task-row').length === ${listed.length}`, 'task list');
    };

    // The middle of the chain shows one prerequisite (done) and one dependent (not yet ready).
    await openFromList('Task B');
    let view = await card();
    assert(view.waiting.heading === 'Waiting on' && view.holding.heading === 'Holding up', `Unexpected column headings: ${JSON.stringify(view)}`);
    assert(JSON.stringify(view.waiting.items.map((item) => [item.title, item.badge])) === JSON.stringify([['Task A', 'Done']]), `Task B prerequisites are wrong: ${JSON.stringify(view.waiting)}`);
    assert(JSON.stringify(view.holding.items.map((item) => [item.title, item.badge])) === JSON.stringify([['Task C', 'Blocked']]), `Task B dependents are wrong: ${JSON.stringify(view.holding)}`);
    assert(view.waiting.items[0].label === 'Open task Task A' && view.note === null, 'Entries lack an accessible name, or a note shows without truncation.');
    assert(await evaluate("document.querySelector('.dependency-waiting .status-badge').className.includes('done')"), 'The done prerequisite lacks the done badge style.');

    // Each entry is a keyboard-reachable button that opens that task, here by keyboard focus plus click on the dependent, then on the prerequisite.
    assert(await evaluate("(() => { const button = document.querySelector('.dependency-holding .dependency-open'); button.focus(); return document.activeElement === button && button.tagName === 'BUTTON'; })()"), 'A dependency entry is not a focusable button.');
    await evaluate("document.querySelector('.dependency-holding .dependency-open').click()");
    await waitPage("document.querySelector('#task-detail-heading').textContent === 'Task C' && document.querySelector('#detail-task-id-value').textContent === 't-c'", 'navigation to the dependent');
    view = await card();
    assert(view.waiting.items.map((item) => item.title).join() === 'Task B' && view.waiting.items[0].badge === 'Not done · Ready', `Task C prerequisite badge is wrong: ${JSON.stringify(view.waiting)}`);
    assert(view.holding.items.length === 0 && view.holding.empty === 'Nothing waits on this task.', `Task C should say nothing waits on it: ${JSON.stringify(view.holding)}`);
    await evaluate("document.querySelector('.dependency-waiting .dependency-open').click()");
    await waitPage("document.querySelector('#task-detail-heading').textContent === 'Task B'", 'navigation to the prerequisite');

    // A task with no relations shows one short message instead of empty columns.
    await backToList();
    await openFromList('Lonely task');
    await waitPage("document.querySelector('#dependencies-content').textContent.length", 'empty dependencies message');
    view = await card();
    assert(view.message === 'This task has no prerequisites and nothing waits on it.' && !view.waiting && !view.holding, `The empty card is wrong: ${JSON.stringify(view)}`);

    // A truncated dependents list says so.
    await backToList();
    await openFromList('Fan-out task');
    view = await card();
    assert(view.holding.items.length === 50 && view.waiting.empty === 'No prerequisites.', `Fan-out dependents are wrong: ${JSON.stringify([view.holding.items.length, view.waiting])}`);
    assert(view.note && view.note.includes('first 50 dependents'), `The truncation note is missing: ${view.note}`);

    // Phone width: the columns stack and nothing overflows.
    for (const width of [390, 320]) {
      await send('Emulation.setDeviceMetricsOverride', { width, height: 844, deviceScaleFactor: 1, mobile: true });
      assert(await evaluate("document.documentElement.scrollWidth <= window.innerWidth"), `The dependencies card overflows at ${width}px.`);
      assert(await evaluate("(() => { const [a, b] = [...document.querySelectorAll('.dependency-column')].map((item) => item.getBoundingClientRect()); return b.top >= a.bottom; })()"), `The columns do not stack at ${width}px.`);
    }
    await send('Emulation.clearDeviceMetricsOverride');

    // A poll with unchanged data mutates nothing in the card; changed data repaints it in place.
    await backToList();
    await openFromList('Task B');
    await evaluate(`(() => { window.__mutations = 0; window.__observer = new MutationObserver((records) => { window.__mutations += records.length; }); window.__observer.observe(document.querySelector('#dependencies-card'), { subtree: true, childList: true, attributes: true, characterData: true }); window.__noReload = true; })()`);
    await sleep(6500);
    assert(await evaluate('window.__mutations === 0'), `An unchanged poll mutated the dependencies card ${await evaluate('window.__mutations')} times.`);
    tasks[1] = { ...tasks[1], dependents: [relation('t-c', 'Task C', 'ready')] };
    await waitPage("document.querySelector('.dependency-holding .status-badge').textContent === 'Ready'", 'the card to follow a dependent status change', 9000);
    assert(await evaluate('window.__noReload === true'), 'The page reloaded instead of patching in place.');

    console.log('PASS: the task detail Dependencies card lists prerequisites (done and not done) and dependents with status badges along a chain, opens each entry as a task, shows one short message for a task without relations, notes a truncated dependents list, stacks at phone width, and leaves an unchanged poll mutation-free.');
  } finally {
    socket?.close();
    if (chrome.pid && chrome.exitCode === null) { const exited = once(chrome, 'exit'); chrome.kill('SIGTERM'); await exited; }
    await new Promise((done) => server.close(done));
    await rm(profile, { recursive: true, force: true }).catch(() => {});
    if (launchError) throw launchError;
  }
}

main().catch((error) => { console.error(`FAIL: ${error.message}`); process.exitCode = 1; });
