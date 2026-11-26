// Browser test of the prefilled project link that `agent-coordinator adopt` prints
// (book/src/docs/project-scaffolding-design.md, "Dashboard change").
//
//   CHROME_BIN=/path/to/chrome [CHROME_ARGS="--no-sandbox"] node scripts/test_new_project_link.mjs
//
// A fixture server stands in for the coordinator and records every project creation request.
// The test opens #/new-project?name=…&repository_url=…&target_branch=… and requires that the
// project dialog opens with the three values as text, that an oversize value or one holding a
// control character is dropped, that nothing is submitted until Create is pressed, that the
// fragment is cleared, and that a visit which starts signed out opens the dialog after sign-in.
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
// The fixture's sign-in state and every body posted to the project creation endpoint.
const fixture = { signedIn: true, created: [] };

const files = { '/': ['web/index.html', 'text/html; charset=utf-8'], '/app.js': ['web/app.js', 'text/javascript; charset=utf-8'], '/style.css': ['web/style.css', 'text/css; charset=utf-8'] };
async function body(request) { const chunks = []; for await (const chunk of request) chunks.push(chunk); return Buffer.concat(chunks).toString(); }
async function route(request, response) {
  const path = new URL(request.url, 'http://fixture.invalid').pathname;
  const json = (data, status = 200) => { response.writeHead(status, { 'Content-Type': 'application/json' }); response.end(JSON.stringify(status < 400 ? { data } : { error: data })); };
  if (files[path]) { response.writeHead(200, { 'Content-Type': files[path][1], 'Cache-Control': 'no-store' }); return response.end(await readFile(join(root, files[path][0]))); }
  if (path === '/api/v1/auth/login' && request.method === 'POST') { await body(request); fixture.signedIn = true; return json({ actor, csrf_token: 'csrf' }); }
  if (path === '/api/v1/me') return fixture.signedIn ? json({ actor, csrf_token: 'csrf' }) : json({ code: 'authentication_required', message: 'Sign in.' }, 401);
  if (path === '/api/v1/projects' && request.method === 'POST') { const created = JSON.parse(await body(request)); fixture.created.push(created); return json({ id: 'p-new', ...created }); }
  if (path === '/api/v1/projects') return json({ items: projects });
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

// The link adopt builds: every value percent-encoded as encodeURIComponent does.
const link = (values) => `#/new-project?${Object.entries(values).map(([key, value]) => `${key}=${encodeURIComponent(value)}`).join('&')}`;
// The three field values of the open project dialog, or null when no dialog is open.
const FIELDS = "(() => { const dialog = document.querySelector('dialog.form-dialog[open]'); return dialog ? ['project-name', 'repository-url', 'target-branch'].map((id) => dialog.querySelector('#' + id).value) : null; })()";

async function main() {
  const server = createServer((request, response) => route(request, response).catch((error) => { response.writeHead(500); response.end(String(error)); }));
  server.listen(0, '127.0.0.1');
  await once(server, 'listening');
  const origin = `http://127.0.0.1:${server.address().port}`;
  const debugPort = await unusedPort();
  const profile = await mkdtemp(join(tmpdir(), 'agent-coordinator-new-project-'));
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
    // Loads the page from scratch at a hash, so each case starts as a person following the link does.
    const open = async (hash) => {
      await send('Page.navigate', { url: 'about:blank' });
      await send('Page.navigate', { url: `${origin}/${hash}` });
    };
    const dialogOpen = "Boolean(document.querySelector('dialog.form-dialog[open]'))";
    await send('Page.enable');

    // 1. Signed in: the dialog opens with the three values as text, a notice, and a cleared fragment.
    const wanted = { name: 'R&D billing #1', repository_url: 'https://github.com/acme/billing', target_branch: 'main' };
    await open(link(wanted));
    await waitPage(dialogOpen, 'the prefilled project dialog');
    assert(JSON.stringify(await evaluate(FIELDS)) === JSON.stringify(Object.values(wanted)), `Prefilled values differ: ${JSON.stringify(await evaluate(FIELDS))}`);
    assert((await evaluate("document.querySelector('dialog[open] #project-prefill-notice')?.textContent")) === 'Prefilled by a link. Check the repository URL before creating.', 'The dialog should say it was prefilled by a link.');
    assert((await evaluate("document.querySelector('dialog[open]').getAttribute('aria-describedby')")) === 'project-prefill-notice', 'The notice should describe the dialog.');
    await waitPage("location.hash === ''", 'the fragment to be cleared');
    assert((await evaluate("[...document.querySelectorAll('.page-view')].filter((view) => !view.hidden).map((view) => view.id).join()")) === 'overview-view', 'The overview should be behind the dialog.');

    // 2. Nothing is submitted by the link. Pressing Create sends exactly the shown values once.
    await sleep(300);
    assert(fixture.created.length === 0, 'The link must never create a project by itself.');
    await evaluate("document.querySelector('dialog[open] button[type=submit]').click()");
    await waitFor(`${origin}/api/v1/me`, () => fixture.created.length === 1, 'the project creation request');
    assert(JSON.stringify(fixture.created[0]) === JSON.stringify(wanted), `Create sent ${JSON.stringify(fixture.created[0])}`);

    // 3. A reload does not reopen the dialog, because the fragment is gone.
    await send('Page.reload');
    await waitPage("document.querySelector('.project-card')", 'the project list after a reload');
    assert(!(await evaluate(dialogOpen)), 'A reload must not reopen the dialog.');

    // 4. Markup in a value stays text.
    const markup = '<img src=x onerror="window.__prefillRan = 1">';
    await open(link({ name: markup, repository_url: 'https://example.invalid/a.git', target_branch: 'main' }));
    await waitPage(dialogOpen, 'the dialog with a markup value');
    assert((await evaluate(FIELDS))[0] === markup, 'A value with markup should appear literally in its field.');
    assert((await evaluate("window.__prefillRan === undefined && document.querySelector('dialog[open] img') === null")), 'A value must never be parsed as HTML.');

    // 5. An oversize value and one with a control character are dropped; the others still fill.
    await open(link({ name: 'x'.repeat(121), repository_url: 'https://example.invalid/a.git', target_branch: 'main\nevil' }));
    await waitPage(dialogOpen, 'the dialog with rejected values');
    assert(JSON.stringify(await evaluate(FIELDS)) === JSON.stringify(['', 'https://example.invalid/a.git', '']), `Rejected values were kept: ${JSON.stringify(await evaluate(FIELDS))}`);
    await open(link({ name: 'é'.repeat(61), repository_url: `https://example.invalid/${'r'.repeat(2048)}`, target_branch: 'b'.repeat(255) }));
    await waitPage(dialogOpen, 'the dialog with boundary values');
    assert(JSON.stringify(await evaluate(FIELDS)) === JSON.stringify(['', '', 'b'.repeat(255)]), 'Limits are counted in UTF-8 bytes, as the service counts them.');

    // 6. A link followed inside an open, signed-in page (a hash change) opens the dialog too.
    await open('');
    await waitPage("document.querySelector('.project-card')", 'the project list');
    await evaluate(`location.hash = ${JSON.stringify(link(wanted))}`);
    await waitPage(dialogOpen, 'the dialog after a hash change');
    await waitPage("location.hash === ''", 'the fragment to be cleared after a hash change');

    // 7. A visit that starts signed out shows sign-in first and opens the dialog afterwards.
    fixture.signedIn = false; fixture.created.length = 0;
    await open(link(wanted));
    await waitPage("!document.querySelector('#login-view').hidden", 'the sign-in view');
    assert(!(await evaluate(dialogOpen)), 'No dialog may open before sign-in.');
    await evaluate("document.querySelector('#username').value = 'operator'; document.querySelector('#password').value = 'fixture-password'; document.querySelector('#login-form').requestSubmit()");
    await waitPage(dialogOpen, 'the prefilled dialog after sign-in');
    assert(JSON.stringify(await evaluate(FIELDS)) === JSON.stringify(Object.values(wanted)), 'The values should survive sign-in.');
    await waitPage("location.hash === ''", 'the fragment to be cleared after sign-in');
    assert(fixture.created.length === 0, 'Signing in must not create the project.');
    console.log('PASS: the prefilled project link fills the dialog as text, drops out-of-bounds values, never submits, clears the fragment, and works from a signed-out start.');
  } finally {
    socket?.close();
    if (chrome.pid && chrome.exitCode === null) { const exited = once(chrome, 'exit'); chrome.kill('SIGTERM'); await exited; }
    await new Promise((done) => server.close(done));
    await rm(profile, { recursive: true, force: true });
    if (launchError) throw launchError;
  }
}

main().catch((error) => { console.error(`FAIL: ${error.message}`); process.exitCode = 1; });
