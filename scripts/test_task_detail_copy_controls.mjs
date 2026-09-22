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
const tasks = Array.from({ length: 30 }, (_, index) => ({
  ...task,
  id: index ? `task-copy-id-${index}` : task.id,
  title: index ? `Fixture task ${index + 1}` : task.title,
}));

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
    if (url.pathname === '/api/v1/me') return json(response, { actor: { id: 'fixture-operator', name: 'Fixture operator', role: 'admin' }, csrf_token: 'fixture-csrf' });
    if (url.pathname === '/api/v1/projects') return json(response, { items: [{ id: 'fixture-project', name: 'Fixture project', target_branch: 'main' }] });
    if (url.pathname === '/api/v1/projects/fixture-project/tasks') {
      const limit = Number(url.searchParams.get('limit') || 50);
      const start = Number(url.searchParams.get('cursor') || 0);
      const items = tasks.slice(start, start + limit);
      const nextCursor = start + items.length < tasks.length ? String(start + items.length) : null;
      return json(response, { items, next_cursor: nextCursor });
    }
    if (url.pathname === `/api/v1/projects/fixture-project/tasks/${task.id}`) return json(response, task);
    if (url.pathname === '/api/v1/admin/credentials') return json(response, { items: [] });
    if (url.pathname === '/api/v1/admin/agents' && request.method === 'POST') return json(response, { token: 'synthetic-token-for-clipboard-test', name: 'Synthetic agent' });
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

async function removeProfile(profile) {
  for (let attempt = 0; attempt < 3; attempt += 1) {
    try { await rm(profile, { recursive: true, force: true }); return; }
    catch (error) {
      if (attempt === 2 || error.code !== 'ENOTEMPTY') throw error;
      await new Promise((resolveDelay) => setTimeout(resolveDelay, 100));
    }
  }
}

async function main() {
  const server = await fixtureServer();
  const serverPort = server.address().port;
  const debugPort = await unusedPort();
  const profile = await mkdtemp(join(tmpdir(), 'agent-coordinator-copy-test-'));
  const executable = process.env.CHROME_BIN || (process.platform === 'win32' ? 'C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe' : 'chromium');
  const chrome = spawn(executable, [
    '--headless=new', '--disable-gpu', '--no-first-run', '--no-default-browser-check',
    '--disable-background-networking', `--remote-debugging-port=${debugPort}`, `--user-data-dir=${profile}`,
  ], { stdio: 'ignore', windowsHide: true });
  let socket;
  let launchError;
  chrome.on('error', (error) => { launchError = error; });
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
    const evaluate = async (expression) => {
      const result = await send('Runtime.evaluate', { expression, awaitPromise: true, returnByValue: true });
      if (result.exceptionDetails) throw new Error(result.exceptionDetails.text);
      return result.result.value;
    };

    await send('Page.enable');
    await send('Page.addScriptToEvaluateOnNewDocument', { source: `
      window.__copiedTaskDetailValue = null;
      window.__rejectTaskDetailClipboard = false;
      window.__credentialDownloadRequested = false;
      Object.defineProperty(Navigator.prototype, 'clipboard', { configurable: true, value: {
        writeText(value) {
          if (window.__rejectTaskDetailClipboard) return Promise.reject(new DOMException('Denied', 'NotAllowedError'));
          window.__copiedTaskDetailValue = value;
          return Promise.resolve();
        }
      }});
      const anchorClick = HTMLAnchorElement.prototype.click;
      HTMLAnchorElement.prototype.click = function () {
        if (this.id === 'download-credential') { window.__credentialDownloadRequested = true; return; }
        return anchorClick.call(this);
      };
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
    const press = async (key, code, virtualKey) => {
      await send('Input.dispatchKeyEvent', { type: 'keyDown', key, code, windowsVirtualKeyCode: virtualKey, text: key === 'Enter' ? '\r' : '' });
      await send('Input.dispatchKeyEvent', { type: 'keyUp', key, code, windowsVirtualKeyCode: virtualKey });
    };
    const back = async () => {
      await evaluate("document.querySelector('#back-to-project').click()");
      await waitPage("!document.querySelector('#overview-view').hidden", 'return to projects');
    };
    await evaluate(`(() => {
      const title = document.querySelector('.project-card h3');
      const range = document.createRange(); range.selectNodeContents(title);
      window.getSelection().removeAllRanges(); window.getSelection().addRange(range);
      title.click();
    })()`);
    assert(await evaluate("!document.querySelector('#overview-view').hidden && window.getSelection().toString() === 'Fixture project'"), 'Selecting project text navigated away.');
    await evaluate("window.getSelection().removeAllRanges(); document.querySelector('.project-card h3').click()");
    await waitPage("!document.querySelector('#tasks-view').hidden && document.querySelector('.task-row')", 'card title navigation');
    assert(await evaluate("document.querySelector('#project-select').value") === 'fixture-project', 'Card opened the wrong project.');
    await back();
    await evaluate("document.querySelector('.project-card').click()");
    await waitPage("!document.querySelector('#tasks-view').hidden", 'blank card navigation');
    await back();
    await evaluate("document.querySelector('.project-settings-button').focus()");
    await press('Tab', 'Tab', 9);
    assert(await evaluate("document.activeElement.classList.contains('project-open')"), 'Open tasks is not reachable by Tab.');
    assert(await evaluate("getComputedStyle(document.activeElement).outlineStyle !== 'none' && parseFloat(getComputedStyle(document.activeElement).outlineWidth) > 0"), 'Open tasks has no visible keyboard focus.');
    await press('Enter', 'Enter', 13);
    await waitPage("!document.querySelector('#tasks-view').hidden", 'keyboard task navigation');
    await back();
    await evaluate("document.querySelector('.project-settings-button').focus()");
    await press('Enter', 'Enter', 13);
    await waitPage("!document.querySelector('#project-view').hidden", 'keyboard settings navigation');
    assert(await evaluate("document.activeElement.id") === 'project-heading', 'Settings heading did not receive focus.');
    assert(await evaluate("document.querySelector('#tasks-view').hidden"), 'Settings gear also opened tasks.');
    await evaluate("document.querySelector('#project-binding-content button').click()");
    await waitPage("window.__copiedTaskDetailValue !== null", 'binding clipboard write');
    assert(await evaluate("window.__copiedTaskDetailValue.includes('project_id = \"fixture-project\"')"), 'Binding copied the wrong project.');
    assert(await evaluate("!document.querySelector('#project-view').hidden"), 'Copy binding navigated away.');
    await send('Emulation.setDeviceMetricsOverride', { width: 390, height: 844, deviceScaleFactor: 1, mobile: true });
    assert(await evaluate("document.documentElement.scrollWidth <= window.innerWidth"), 'Settings page overflows at phone width.');
    await evaluate("document.querySelector('#back-to-projects').click()");
    await waitPage("!document.querySelector('#overview-view').hidden", 'settings return navigation');
    assert(await evaluate("document.documentElement.scrollWidth <= window.innerWidth"), 'Project cards overflow at phone width.');
    await send('Emulation.clearDeviceMetricsOverride');
    await evaluate("window.__copiedTaskDetailValue = null; document.querySelector('.project-open').click()");
    await waitPage("document.querySelector('.task-row')", 'task queue');
    assert(await evaluate("document.querySelectorAll('.task-row').length") === 25, 'The first task page did not use the default page size.');
    await evaluate("document.querySelector('#tasks-next-page').click()");
    await waitPage("document.querySelector('#tasks-page-status').textContent.startsWith('Page 2')", 'next task page');
    assert(await evaluate("document.querySelectorAll('.task-row').length") === 5, 'Next page did not show the remaining tasks.');
    assert(await evaluate("document.querySelectorAll('#tasks-page-numbers button').length") === 2, 'The second page was not represented by a numbered control.');
    await evaluate("document.querySelector('#tasks-previous-page').click()");
    await waitPage("document.querySelector('#tasks-page-status').textContent.startsWith('Page 1')", 'previous task page');
    await evaluate("(() => { const size = document.querySelector('#tasks-page-size'); size.value = '10'; size.dispatchEvent(new Event('change', { bubbles: true })); })()");
    await waitPage("document.querySelectorAll('.task-row').length === 10", 'page size update');
    await evaluate("document.querySelector('#tasks-last-page').click()");
    await waitPage("document.querySelector('#tasks-page-status').textContent === 'Page 3 of 3'", 'last task page');
    await evaluate("document.querySelector('#tasks-first-page').click()");
    await waitPage("document.querySelector('#tasks-page-status').textContent.startsWith('Page 1')", 'first task page');
    await evaluate("[...document.querySelectorAll('#tasks-page-numbers button')].find((button) => button.textContent === '2').click()");
    await waitPage("document.querySelector('#tasks-page-status').textContent.startsWith('Page 2')", 'numbered task page');
    await evaluate("document.querySelector('#tasks-first-page').click()");
    await waitPage("document.querySelector('#tasks-page-status').textContent.startsWith('Page 1')", 'first task page before detail');
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

    await evaluate("document.querySelector('button.nav-item[data-view=\"admin\"]').click()");
    await waitPage("!document.querySelector('#admin-view').hidden", 'admin view');
    await evaluate("document.querySelector('#agent-name').value = 'Synthetic agent'; document.querySelector('#issue-form').requestSubmit()");
    await waitPage("!document.querySelector('#token-reveal').hidden", 'issued token reveal');
    assert(await evaluate("document.querySelector('#issued-token').textContent") === 'synthetic-token-for-clipboard-test', 'Issued token was not revealed exactly.');
    assert(await evaluate('window.__credentialDownloadRequested'), 'Credential download was not prepared.');
    await evaluate("window.__copiedTaskDetailValue = null; window.__rejectTaskDetailClipboard = false; document.querySelector('#copy-token').focus()");
    await press('Enter', 'Enter', 13);
    await waitPage("window.__copiedTaskDetailValue === 'synthetic-token-for-clipboard-test'", 'keyboard token clipboard write');
    assert(await evaluate("document.querySelector('#issue-feedback').textContent") === 'Token copied to clipboard.', 'Token success feedback was not truthful.');
    await evaluate("window.__copiedTaskDetailValue = null; window.__rejectTaskDetailClipboard = true; document.querySelector('#copy-token').click()");
    await waitPage("document.querySelector('#issue-feedback').textContent.includes('Clipboard access was unavailable')", 'token manual-copy fallback');
    assert(await evaluate('window.getSelection().toString()') === 'synthetic-token-for-clipboard-test', 'Token fallback did not select the complete synthetic token.');
    console.log('PASS: headless Chrome verified project navigation, keyboard focus, pagination controls, selection, binding copy, phone layout, task-detail copy/fallback, and synthetic issued-token copy/fallback.');
  } finally {
    socket?.close();
    if (chrome.pid && chrome.exitCode === null && process.platform === 'win32') {
      const taskkill = spawn('taskkill', ['/PID', String(chrome.pid), '/T', '/F'], { stdio: 'ignore', windowsHide: true });
      await once(taskkill, 'exit');
      await once(chrome, 'exit');
    }
    if (chrome.pid && chrome.exitCode === null) {
      const exited = once(chrome, 'exit'); chrome.kill('SIGTERM'); await exited;
    }
    await new Promise((resolveClose) => server.close(resolveClose));
    await removeProfile(profile);
    if (launchError) throw launchError;
  }
}

main().catch((error) => { console.error(`FAIL: ${error.message}`); process.exitCode = 1; });
