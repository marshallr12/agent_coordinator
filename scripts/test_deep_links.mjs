// Browser test of the dashboard's address history (book/src/docs/web-ui-refresh.md).
//
//   CHROME_BIN=/path/to/chrome [CHROME_ARGS="--no-sandbox"] node scripts/test_deep_links.mjs
//
// A fixture server stands in for the coordinator. The test clicks through ten views, walks back
// through them with history.back(), walks forward again with history.forward(), and then opens a
// deep link in a fresh page load. Each step requires both the address hash and the visible view.
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

const actor = { id: 'op-1', name: 'Operator', role: 'admin', kind: 'human', session_id: 'browser-1' };
const projects = [{ id: 'p1', name: 'Alpha', repository_url: 'https://example.invalid/alpha.git', target_branch: 'main' }];
const makeTask = (index) => ({ id: `task-${index}`, title: `Fixture task ${index}`, description: `Description ${index}`, revision: 1, kind: 'code', lifecycle: 'open', work_status: 'ready', created_at: '2026-10-01T00:00:00Z', acceptance_criteria: [] });
const tasks = Array.from({ length: 5 }, (_, index) => makeTask(index));
const detail = { ...makeTask(1), attempts: [], checkpoints: [], workflow: { phase: 'open', blockers: [], next_actions: [], submission: null, activities: [] }, job_evidence: { jobs: [], reservations: [] }, lifecycle_events: [], replaces: [] };

const files = { '/': ['web/index.html', 'text/html; charset=utf-8'], '/app.js': ['web/app.js', 'text/javascript; charset=utf-8'], '/style.css': ['web/style.css', 'text/css; charset=utf-8'] };
async function route(request, response) {
  const url = new URL(request.url, 'http://fixture.invalid');
  const path = url.pathname;
  const json = (data) => { response.writeHead(200, { 'Content-Type': 'application/json' }); response.end(JSON.stringify({ data })); };
  if (files[path]) { response.writeHead(200, { 'Content-Type': files[path][1], 'Cache-Control': 'no-store' }); return response.end(await readFile(join(root, files[path][0]))); }
  if (path === '/api/v1/me') return json({ actor, csrf_token: 'csrf' });
  if (path === '/api/v1/projects') return json({ items: projects });
  const rest = path.match(/^\/api\/v1\/projects\/p1(\/.*)?$/)?.[1];
  if (rest === '/tasks') return json({ items: tasks, next_cursor: null });
  if (rest === '/tasks/archived') return json({ items: [], next_cursor: null });
  if (rest === `/tasks/${detail.id}`) return json(detail);
  if (rest === `/tasks/${detail.id}/history`) return json({ items: [], next_cursor: null });
  if (rest?.endsWith('/orientation')) return json({ project: projects[0], policy_revision: 1 });
  if (path === '/api/v1/admin/clock' || path === '/api/v1/admin/restore') return json({});
  if (rest || path.startsWith('/api/v1/')) return json({ items: [], next_cursor: null });
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

// Each step: a click that opens a view, the hash it must leave, and the view it must show.
const STEPS = [
  { label: 'project tasks', click: '.project-open', hash: '#/projects/p1/tasks', view: 'tasks' },
  { label: 'task detail', click: "[...document.querySelectorAll('#tasks-list .task-row')].find((row) => row.textContent.includes('Fixture task 1'))", hash: '#/projects/p1/task/task-1', view: 'task-detail' },
  { label: 'completed tasks', click: '#completed-tasks-tab', hash: '#/projects/p1/completed', view: 'tasks' },
  { label: 'archived tasks', click: '#archived-tasks-tab', hash: '#/projects/p1/archived', view: 'archived' },
  { label: 'knowledge', click: '.nav-item[data-view="shared"]', hash: '#/shared', view: 'shared' },
  { label: 'resources', click: '.nav-item[data-view="resources"]', hash: '#/resources', view: 'resources' },
  { label: 'access', click: '.nav-item[data-view="admin"]', hash: '#/admin', view: 'admin' },
  { label: 'overview', click: '.nav-item[data-view="overview"]', hash: '', view: 'overview' },
  { label: 'project settings', click: '.project-settings-button', hash: '#/projects/p1/settings', view: 'project' },
  { label: 'integrator reports', click: '#integrator-reports-button', hash: '#/projects/p1/reports', view: 'reports' },
];

async function main() {
  const server = createServer((request, response) => route(request, response).catch((error) => { response.writeHead(500); response.end(String(error)); }));
  server.listen(0, '127.0.0.1');
  await once(server, 'listening');
  const serverPort = server.address().port;
  const debugPort = await unusedPort();
  const profile = await mkdtemp(join(tmpdir(), 'agent-coordinator-deep-links-'));
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

    await send('Page.enable');
    await send('Page.navigate', { url: `http://127.0.0.1:${serverPort}/` });
    await waitPage("document.querySelector('.project-card')", 'project list');
    await expectPage('', 'overview', 'start on the overview');
    const start = await evaluate('history.length');

    for (const step of STEPS) {
      await clickTarget(step.click);
      await expectPage(step.hash, step.view, `click to ${step.label}`);
    }
    assert((await evaluate('history.length')) - start === STEPS.length, 'Each click should add exactly one history entry.');

    for (let index = STEPS.length - 2; index >= -1; index -= 1) {
      await evaluate('history.back()');
      const previous = index < 0 ? { hash: '', view: 'overview', label: 'the start' } : STEPS[index];
      await expectPage(previous.hash, previous.view, `back to ${previous.label}`);
    }
    for (const step of STEPS) {
      await evaluate('history.forward()');
      await expectPage(step.hash, step.view, `forward to ${step.label}`);
    }

    // A deep link opens its view in a fresh page load, and a link to an unknown project falls back.
    await send('Page.navigate', { url: `http://127.0.0.1:${serverPort}/#/projects/p1/task/task-1` });
    await waitPage("location.hash === '#/projects/p1/task/task-1' && !document.querySelector('#task-detail-view').hidden && document.querySelector('#task-detail-heading').textContent.includes('Fixture task 1')", 'task deep link');
    await send('Page.navigate', { url: `http://127.0.0.1:${serverPort}/#/projects/nope/tasks` });
    await waitPage("location.hash === '' && !document.querySelector('#overview-view').hidden", 'unknown project fallback');
    console.log(`PASS: ${STEPS.length} clicked views were walked back and forward, and a task deep link opened directly.`);
  } finally {
    socket?.close();
    if (chrome.pid && chrome.exitCode === null) { const exited = once(chrome, 'exit'); chrome.kill('SIGTERM'); await exited; }
    await new Promise((done) => server.close(done));
    await rm(profile, { recursive: true, force: true });
    if (launchError) throw launchError;
  }
}

main().catch((error) => { console.error(`FAIL: ${error.message}`); process.exitCode = 1; });
