import { createServer } from 'node:http';
import { mkdtemp, readFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { spawn } from 'node:child_process';
import { once } from 'node:events';
import net from 'node:net';

const root = resolve(import.meta.dirname, '..');

function assert(condition, message) {
  if (!condition) throw new Error(message);
}

async function unusedPort() {
  const server = net.createServer();
  server.listen(0, '127.0.0.1');
  await once(server, 'listening');
  const { port } = server.address();
  await new Promise(resolveClose => server.close(resolveClose));
  return port;
}

async function fixtureServer() {
  let signedOut = false;
  const files = {
    '/': ['web/index.html', 'text/html; charset=utf-8'],
    '/index.html': ['web/index.html', 'text/html; charset=utf-8'],
    '/app.js': ['web/app.js', 'text/javascript; charset=utf-8'],
    '/style.css': ['web/style.css', 'text/css; charset=utf-8'],
  };
  const server = createServer(async (request, response) => {
    const url = new URL(request.url, 'http://fixture.invalid');
    const json = data => { response.writeHead(200, { 'Content-Type': 'application/json' }); response.end(JSON.stringify({ data })); };
    if (url.pathname === '/api/v1/me') return json({ actor: { id: 'fixture-operator', name: 'Fixture operator', role: 'operator' }, csrf_token: 'fixture-csrf' });
    if (url.pathname === '/api/v1/projects') return json({ items: [] });
    if (url.pathname === '/api/v1/auth/account') return json({ operator: { id: 'fixture-operator', name: 'Fixture operator', role: 'operator', revision: 1 } });
    if (url.pathname === '/api/v1/auth/logout' && request.method === 'POST') { signedOut = true; return json({}); }
    const file = files[url.pathname];
    if (!file) { response.writeHead(404); response.end(); return; }
    response.writeHead(200, { 'Content-Type': file[1] });
    response.end(await readFile(join(root, file[0])));
  });
  server.listen(0, '127.0.0.1');
  await once(server, 'listening');
  return { server, signedOut: () => signedOut };
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
    await new Promise(resolveDelay => setTimeout(resolveDelay, 50));
  }
  throw new Error(`Timed out waiting for ${description}`);
}

async function main() {
  const fixture = await fixtureServer();
  const serverPort = fixture.server.address().port;
  const debugPort = await unusedPort();
  const profile = await mkdtemp(join(tmpdir(), 'agent-coordinator-account-menu-'));
  const executable = process.env.CHROME_BIN || (process.platform === 'win32' ? 'C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe' : 'chromium');
  const chrome = spawn(executable, ['--headless=new', '--disable-gpu', '--no-first-run', '--no-default-browser-check', '--disable-background-networking', `--remote-debugging-port=${debugPort}`, `--user-data-dir=${profile}`], { stdio: 'ignore', windowsHide: true });
  let socket;
  let launchError;
  chrome.on('error', error => { launchError = error; });
  try {
    const version = await waitFor(`http://127.0.0.1:${debugPort}/json/version`, value => Boolean(value.webSocketDebuggerUrl), 'headless Chrome');
    const connect = async url => {
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
        pending.set(id, message => { clearTimeout(timer); message.error ? rejectMessage(new Error(message.error.message)) : resolveMessage(message.result); });
        connection.send(JSON.stringify({ id, method, params }));
      });
      return { connection, send };
    };
    let connection = await connect(version.webSocketDebuggerUrl);
    const target = await connection.send('Target.createTarget', { url: 'about:blank' });
    connection.connection.close();
    const pages = await waitFor(`http://127.0.0.1:${debugPort}/json/list`, items => items.some(item => item.id === target.targetId), 'headless Chrome page');
    connection = await connect(pages.find(item => item.id === target.targetId).webSocketDebuggerUrl);
    socket = connection.connection;
    const evaluate = async expression => {
      const result = await connection.send('Runtime.evaluate', { expression, awaitPromise: true, returnByValue: true });
      if (result.exceptionDetails) throw new Error(result.exceptionDetails.text);
      return result.result.value;
    };
    await connection.send('Page.enable');
    await connection.send('Page.navigate', { url: `http://127.0.0.1:${serverPort}/` });
    const waitPage = async (expression, label) => {
      const deadline = Date.now() + 10000;
      while (Date.now() < deadline) {
        if (await evaluate(expression)) return;
        await new Promise(resolveDelay => setTimeout(resolveDelay, 30));
      }
      throw new Error(`Timed out waiting for ${label}`);
    };

    await waitPage("!document.querySelector('#dashboard-view').hidden", 'authenticated dashboard');
    assert(await evaluate("document.querySelectorAll('.topbar-actions button').length") === 1, 'Expected one account action in the header.');
    assert(await evaluate("document.querySelector('#account-button').textContent") === 'Fixture operator', 'The account action did not use the signed-in username.');
    assert(await evaluate("!document.querySelector('#actor-chip') && !document.querySelector('#logout-button')"), 'Legacy account controls are still present.');
    await evaluate("document.querySelector('#account-button').click()");
    await waitPage("document.querySelector('dialog[open] h2')?.textContent === 'My account'", 'My account dialog');
    assert(await evaluate("[...document.querySelectorAll('dialog[open] button')].some(button => button.textContent === 'Sign out')"), 'The My account dialog has no Sign out button.');
    await evaluate("[...document.querySelectorAll('dialog[open] button')].find(button => button.textContent === 'Sign out').click()");
    await waitPage("!document.querySelector('#login-view').hidden && document.querySelector('#dashboard-view').hidden", 'local sign-out');
    assert(fixture.signedOut(), 'The Sign out button did not call the logout endpoint.');
    console.log('PASS: headless Chrome verified the username account action, My account dialog, and sign out.');
  } finally {
    socket?.close();
    if (chrome.pid && chrome.exitCode === null && process.platform === 'win32') {
      const taskkill = spawn('taskkill', ['/PID', String(chrome.pid), '/T', '/F'], { stdio: 'ignore', windowsHide: true });
      await once(taskkill, 'exit');
      await once(chrome, 'exit');
    }
    if (chrome.pid && chrome.exitCode === null) { const exited = once(chrome, 'exit'); chrome.kill('SIGTERM'); await exited; }
    await new Promise(resolveClose => fixture.server.close(resolveClose));
    await rm(profile, { recursive: true, force: true });
    if (launchError) throw launchError;
  }
}

main().catch(error => { console.error(`FAIL: ${error.message}`); process.exitCode = 1; });
