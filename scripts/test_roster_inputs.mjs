// Browser test of the Required checks dialog's declared inputs.
//
//   CHROME_BIN=/path/to/chrome [CHROME_ARGS="--no-sandbox"] node scripts/test_roster_inputs.mjs
//
// A fixture server stands in for the coordinator and serves one project's workflow policy, where one
// roster entry declares inputs and one does not. The test opens the dialog and checks that the inputs
// are shown; saves without touching them and checks that the PUT body carries the same paths and no
// `inputs` key for the entry without any; adds, changes and removes inputs; and has the service refuse
// a roster, checking that the dialog stays open with its values and shows the service's reason. Only
// Node's standard library and Chrome are used.
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
const projects = [{ id: 'p1', name: 'Alpha', repository_url: 'https://example.invalid/alpha.git', target_branch: 'main' }];
const project = { id: 'p1', name: 'Alpha', repository_url: 'https://example.invalid/alpha.git', policy_revision: 3, review_mode: 'either', recovery_mode: 'agent', lease_seconds: 600, rules: 'Be careful.', agent_rule_editing: false, automatic_integration: false, allow_subagent_reviews: false, integration_owner: 'integrator' };
let roster = [
  { identity: 'dependency-audit', version: 'v1', environment: 'linux-ci', inputs: ['Cargo.lock', '.cargo/audit.toml'] },
  { identity: 'workspace-tests', version: 'v1', environment: 'linux-ci' },
];
let revision = 4;
const puts = [];
let refusal = null;

function json(response, data, status = 200) {
  response.writeHead(status, { 'Content-Type': 'application/json', 'Cache-Control': 'no-store' });
  response.end(JSON.stringify(status === 200 ? { data } : data));
}

async function route(request, response) {
  const url = new URL(request.url, 'http://fixture.invalid');
  const path = url.pathname;
  const files = { '/': ['web/index.html', 'text/html; charset=utf-8'], '/app.js': ['web/app.js', 'text/javascript; charset=utf-8'], '/style.css': ['web/style.css', 'text/css; charset=utf-8'] };
  if (files[path]) {
    response.writeHead(200, { 'Content-Type': files[path][1], 'Cache-Control': 'no-store' });
    return response.end(await readFile(join(root, files[path][0])));
  }
  if (path === '/api/v1/me') return json(response, { actor, csrf_token: 'csrf' });
  if (path === '/api/v1/projects') return json(response, { items: projects });
  if (path === '/api/v1/projects/p1/orientation') return json(response, { project, policy_revision: project.policy_revision });
  if (path === '/api/v1/projects/p1/workflow-policy') {
    if (request.method === 'PUT') {
      let raw = ''; for await (const chunk of request) raw += chunk;
      const body = JSON.parse(raw); puts.push(body);
      if (refusal) return json(response, { error: { code: 'invalid_required_check', message: refusal } }, 422);
      roster = body.required_checks; revision += 1;
      return json(response, { revision, canonical_repository_key: 'example.invalid/alpha', required_checks: roster });
    }
    return json(response, { revision, canonical_repository_key: 'example.invalid/alpha', required_checks: roster });
  }
  if (path === '/api/v1/projects/p1/tasks' || path === '/api/v1/projects/p1/tasks/archived') return json(response, { items: [], next_cursor: null });
  if (path.startsWith('/api/v1/')) return json(response, { items: [], next_cursor: null });
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
  const profile = await mkdtemp(join(tmpdir(), 'agent-coordinator-roster-test-'));
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
    const setValue = (selector, value) => evaluate(`(() => { const node = document.querySelector(${JSON.stringify(selector)}); node.value = ${JSON.stringify(value)}; node.dispatchEvent(new Event('input', { bubbles: true })); })()`);
    const rowValues = () => evaluate("[...document.querySelectorAll('dialog[open] .check-row')].map((row) => [...row.querySelectorAll('input, textarea')].map((node) => node.value))");
    const dialogOpen = () => evaluate("Boolean(document.querySelector('dialog[open] .check-row'))");
    const refusalText = () => evaluate("(() => { const node = document.querySelector('dialog[open] [role=alert]'); return node && !node.hidden ? node.textContent : ''; })()");
    const submit = () => evaluate("document.querySelector('dialog[open] form').requestSubmit()");
    const openDialog = async () => { await click('#workflow-settings'); await waitPage("document.querySelector('dialog[open] .check-row')", 'required checks dialog'); };

    await click('.project-open');
    await waitPage("document.querySelector('#project-select')?.value === 'p1'", 'project selected');

    // ---- Declared inputs are shown, and an unrelated edit sends them back --------------------------
    await openDialog();
    let values = await rowValues();
    assert(JSON.stringify(values) === JSON.stringify([['dependency-audit', 'v1', 'linux-ci', 'Cargo.lock\n.cargo/audit.toml'], ['workspace-tests', 'v1', 'linux-ci', '']]), `Dialog showed ${JSON.stringify(values)}.`);
    const help = await evaluate("document.querySelector('dialog[open]').textContent");
    assert(help.includes('every repository path the check reads'), 'The dialog does not say the list must name every path the check reads.');
    await setValue('dialog[open] .check-row:nth-child(2) input[name=version]', 'v2');
    await submit(); await sleep(300);
    assert(puts.length === 1, `Expected one PUT, saw ${puts.length}.`);
    assert(JSON.stringify(puts[0].required_checks) === JSON.stringify([
      { identity: 'dependency-audit', version: 'v1', environment: 'linux-ci', inputs: ['Cargo.lock', '.cargo/audit.toml'] },
      { identity: 'workspace-tests', version: 'v2', environment: 'linux-ci' },
    ]), `Unrelated edit sent ${JSON.stringify(puts[0].required_checks)}.`);
    assert(!(await dialogOpen()), 'The dialog stayed open after a successful save.');

    // ---- Add, change and remove inputs -------------------------------------------------------------
    await openDialog();
    await setValue('dialog[open] .check-row:nth-child(1) textarea[name=inputs]', '  Cargo.lock \n\nrust-toolchain.toml\n');
    await setValue('dialog[open] .check-row:nth-child(2) textarea[name=inputs]', 'Makefile');
    await submit(); await sleep(300);
    assert(JSON.stringify(puts[1].required_checks) === JSON.stringify([
      { identity: 'dependency-audit', version: 'v1', environment: 'linux-ci', inputs: ['Cargo.lock', 'rust-toolchain.toml'] },
      { identity: 'workspace-tests', version: 'v2', environment: 'linux-ci', inputs: ['Makefile'] },
    ]), `Add/change sent ${JSON.stringify(puts[1].required_checks)}.`);
    await openDialog();
    await setValue('dialog[open] .check-row:nth-child(1) textarea[name=inputs]', '');
    await submit(); await sleep(300);
    assert(!('inputs' in puts[2].required_checks[0]) && JSON.stringify(puts[2].required_checks[1].inputs) === '["Makefile"]', `Removal sent ${JSON.stringify(puts[2].required_checks)}.`);

    // ---- A refusal leaves the values intact and shows the service's reason -------------------------
    await openDialog();
    await setValue('dialog[open] .check-row:nth-child(1) textarea[name=inputs]', 'src/*.rs\n../secret');
    const typed = await rowValues();
    refusal = 'Declared input "src/*.rs" is not a plain repository file path.';
    await submit(); await sleep(300);
    await waitPage("(() => { const node = document.querySelector('dialog[open] [role=alert]'); return node && !node.hidden && node.textContent; })()", 'the refusal');
    assert(await refusalText() === refusal, `Refusal text was "${await refusalText()}".`);
    assert(await dialogOpen(), 'The dialog closed on a refusal.');
    assert(JSON.stringify(await rowValues()) === JSON.stringify(typed), 'The refusal changed the dialog values.');
    // After the refusal the same dialog can be corrected and saved.
    refusal = null;
    await setValue('dialog[open] .check-row:nth-child(1) textarea[name=inputs]', 'Cargo.lock');
    await submit(); await sleep(300);
    assert(JSON.stringify(puts.at(-1).required_checks[0].inputs) === '["Cargo.lock"]' && !(await dialogOpen()), 'The corrected roster was not saved.');
    console.log('ok: roster inputs');
  } finally {
    socket?.close();
    if (chrome.pid && chrome.exitCode === null) { const exited = once(chrome, 'exit'); chrome.kill('SIGTERM'); await exited; }
    await new Promise((done) => server.close(done));
    await rm(profile, { recursive: true, force: true }).catch(() => {});
    if (launchError) throw launchError;
  }
}

main().catch((error) => { console.error(`FAIL: ${error.message}`); process.exitCode = 1; });
