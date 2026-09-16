import { createServer } from 'node:http';
import { mkdtemp, readFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { spawn } from 'node:child_process';
import { once } from 'node:events';
import net from 'node:net';

const root = resolve(import.meta.dirname, '..');
const task = {
  id: 'task-copy-id-123',
  title: 'Copy this task name exactly',
  description: 'Synthetic browser-fixture task.',
  revision: 7,
  kind: 'code',
  lifecycle: 'open',
  work_status: 'ready',
  acceptance_criteria: [],
  attempts: [],
  checkpoints: [],
  workflow: { activities: [] },
  job_evidence: { jobs: [], reservations: [] },
};

function assert(condition, message) {
  if (!condition) throw new Error(message);
}

async function unusedPort() {
  const probe = net.createServer();
  probe.listen(0, '127.0.0.1');
  await once(probe, 'listening');
  const { port } = probe.address();
  await new Promise((resolveClose) => probe.close(resolveClose));
  return port;
}

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
    if (url.pathname === '/api/v1/me') return json(response, { actor: { id: 'fixture-operator', name: 'Fixture operator', role: 'operator' }, csrf_token: 'fixture-csrf' });
    if (url.pathname === '/api/v1/projects') return json(response, { items: [{ id: 'fixture-project', name: 'Fixture project', target_branch: 'main' }] });
    if (url.pathname === '/api/v1/projects/fixture-project/tasks') return json(response, { items: [task] });
    if (url.pathname === `/api/v1/projects/fixture-project/tasks/${task.id}`) return json(response, task);
    const file = files[url.pathname];
    if (!file) { response.writeHead(404); response.end(); return; }
    try {
      response.writeHead(200, { 'Content-Type': file[1], 'Cache-Control': 'no-store' });
      response.end(await readFile(join(root, file[0])));
    } catch (error) {
      response.writeHead(500); response.end(String(error));
    }
  });
  server.listen(0, '127.0.0.1');
  await once(server, 'listening');
  return server;
}

async function waitFor(fetchUrl, predicate, description) {
  const deadline = Date.now() + 10000;
  while (Date.now() < deadline) {
    try {
      const response = await fetch(fetchUrl);
      if (response.ok) {
        const value = await response.json();
        if (predicate(value)) return value;
      }
    } catch (_) { /* Chrome is still starting. */ }
    await new Promise((resolveDelay) => setTimeout(resolveDelay, 50));
  }
  throw new Error(`Timed out waiting for ${description}`);
}

async function main() {
  const server = await fixtureServer();
  const serverPort = server.address().port;
  const debugPort = await unusedPort();
  const profile = await mkdtemp(join(tmpdir(), 'agent-coordinator-copy-test-'));
  const chrome = spawn('C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe', [
    '--headless=new', '--disable-gpu', '--no-first-run', '--no-default-browser-check',
    '--disable-background-networking', `--remote-debugging-port=${debugPort}`, `--user-data-dir=${profile}`,
  ], { stdio: 'ignore', windowsHide: true });
  let socket;
  try {
    const version = await waitFor(`http://127.0.0.1:${debugPort}/json/version`, (value) => Boolean(value.webSocketDebuggerUrl), 'headless Chrome');
    const connect = async (url) => {
      const connection = new WebSocket(url);
      await once(connection, 'open');
      let nextId = 1;
      const pending = new Map();
      connection.addEventListener('message', ({ data }) => {
        const message = JSON.parse(data);
        const callback = pending.get(message.id);
        if (callback) { pending.delete(message.id); callback(message); }
      });
      const send = (method, params = {}) => new Promise((resolveMessage, rejectMessage) => {
        const id = nextId++;
        const timer = setTimeout(() => { pending.delete(id); rejectMessage(new Error(`Timed out calling ${method}`)); }, 10000);
        pending.set(id, (message) => { clearTimeout(timer); message.error ? rejectMessage(new Error(message.error.message)) : resolveMessage(message.result); });
        connection.send(JSON.stringify({ id, method, params }));
      });
      return { connection, send };
    };
    let connection = await connect(version.webSocketDebuggerUrl);
    const target = await connection.send('Target.createTarget', { url: 'about:blank' });
    connection.connection.close();
    const page = await waitFor(`http://127.0.0.1:${debugPort}/json/list`, (items) => items.find((item) => item.id === target.targetId)?.webSocketDebuggerUrl, 'headless Chrome page target');
    const pageSocket = page.find((item) => item.id === target.targetId).webSocketDebuggerUrl;
    connection = await connect(pageSocket);
    socket = connection.connection;
    const { send } = connection;
    const evaluate = async (expression) => (await send('Runtime.evaluate', { expression, awaitPromise: true, returnByValue: true })).result.value;

    await send('Page.enable');
    await send('Page.addScriptToEvaluateOnNewDocument', { source: `
      window.__copiedTaskDetailValue = null;
      window.__rejectTaskDetailClipboard = false;
      Object.defineProperty(Navigator.prototype, 'clipboard', { configurable: true, value: {
        writeText(value) {
          if (window.__rejectTaskDetailClipboard) return Promise.reject(new DOMException('Denied', 'NotAllowedError'));
          window.__copiedTaskDetailValue = value;
          return Promise.resolve();
        }
      }});
    ` });
    await send('Page.navigate', { url: `http://127.0.0.1:${serverPort}/` });
    const waitPage = async (expression, label) => {
      const deadline = Date.now() + 10000;
      while (Date.now() < deadline) {
        if (await evaluate(expression)) return;
        await new Promise((resolveDelay) => setTimeout(resolveDelay, 30));
      }
      throw new Error(`Timed out waiting for ${label}`);
    };
    await waitPage("document.querySelector('.project-open')", 'project list');
    await evaluate("document.querySelector('.project-open').click()");
    await waitPage("document.querySelector('.task-row')", 'task queue');
    await evaluate("document.querySelector('.task-row').click()");
    await waitPage("document.querySelector('#task-detail-content') && !document.querySelector('#task-detail-content').hidden", 'task detail');

    assert(await evaluate("document.querySelector('#task-detail-heading').textContent") === task.title, 'Task name was not rendered exactly.');
    assert(await evaluate("document.querySelector('#detail-task-id-value').textContent") === task.id, 'Task ID was not rendered exactly.');
    await evaluate("document.querySelector('#copy-task-name').click()");
    await waitPage("window.__copiedTaskDetailValue !== null", 'task-name clipboard write');
    assert(await evaluate('window.__copiedTaskDetailValue') === task.title, 'Copy task name did not write the exact task title.');

    await evaluate("window.__copiedTaskDetailValue = null; window.__rejectTaskDetailClipboard = true; document.querySelector('#copy-task-id').click()");
    await waitPage("document.querySelector('#detail-copy-feedback').textContent.includes('Clipboard access was unavailable')", 'manual-copy fallback');
    assert(await evaluate('window.getSelection().toString()') === task.id, 'Clipboard fallback did not select the exact task ID.');
    assert(await evaluate("document.querySelector('#detail-copy-feedback').classList.contains('fallback')"), 'Clipboard fallback was not identified to assistive technology and styling.');
    console.log('PASS: headless Chrome verified task-detail copy controls and the manual-copy fallback.');
  } finally {
    socket?.close();
    if (chrome.exitCode === null) {
      const taskkill = spawn('taskkill', ['/PID', String(chrome.pid), '/T', '/F'], { stdio: 'ignore', windowsHide: true });
      await once(taskkill, 'exit');
      await once(chrome, 'exit');
    }
    await new Promise((resolveClose) => server.close(resolveClose));
    await rm(profile, { recursive: true, force: true });
  }
}

main().catch((error) => { console.error(`FAIL: ${error.message}`); process.exitCode = 1; });
