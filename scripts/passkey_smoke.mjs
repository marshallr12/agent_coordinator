// Headless-browser check of passkey authentication against a real service.
//
//   CHROME_BIN=/path/to/chrome [CHROME_ARGS="--no-sandbox"] \
//     node scripts/passkey_smoke.mjs [path/to/agent-coordinator-server]
//
// Starts the service on a private loopback port with a temporary database and
// an administrator, drives the dashboard in headless Chrome with a virtual
// CTAP2 authenticator, and checks that the My account dialog offers passkey
// creation, that creating one succeeds, and that after signing out the passkey
// (not the password) signs back in. Only Node's standard library is used.
import { mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { spawn } from 'node:child_process';
import { once } from 'node:events';
import net from 'node:net';

const root = resolve(import.meta.dirname, '..');
const PASSWORD = 'a lengthy test-only administrator password';

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

async function waitFor(fetchUrl, predicate, description) {
  const deadline = Date.now() + 15000;
  while (Date.now() < deadline) {
    try {
      const response = await fetch(fetchUrl);
      if (response.ok) {
        const value = await response.json();
        if (predicate(value)) return value;
      }
    } catch (_) { /* Still starting. */ }
    await new Promise(resolveDelay => setTimeout(resolveDelay, 50));
  }
  throw new Error(`Timed out waiting for ${description}`);
}

async function startService(binary, scratch, port) {
  const common = ['--database', join(scratch, 'coordinator.sqlite3'), '--public-origin', `http://localhost:${port}`, '--listen', `127.0.0.1:${port}`, '--allow-insecure-loopback'];
  const init = spawn(binary, [...common, 'init-admin', '--username', 'admin', '--password-stdin'], { stdio: ['pipe', 'ignore', 'inherit'] });
  init.stdin.end(`${PASSWORD}\n`);
  const [code] = await once(init, 'exit');
  assert(code === 0, 'init-admin failed');
  const service = spawn(binary, [...common, 'serve'], { stdio: 'ignore' });
  await waitFor(`http://127.0.0.1:${port}/healthz`, value => value.data?.status === 'ok', 'the service');
  return service;
}

async function main() {
  const binary = process.argv[2] || join(process.env.CARGO_TARGET_DIR || join(root, 'target'), 'debug', 'agent-coordinator-server');
  const scratch = await mkdtemp(join(tmpdir(), 'agent-coordinator-passkey-'));
  const servicePort = await unusedPort();
  const debugPort = await unusedPort();
  const executable = process.env.CHROME_BIN || 'chromium';
  const extraArguments = (process.env.CHROME_ARGS || '').split(' ').filter(Boolean);
  let service;
  let chrome;
  let socket;
  try {
    service = await startService(binary, scratch, servicePort);
    chrome = spawn(executable, ['--headless=new', '--disable-gpu', '--no-first-run', '--no-default-browser-check', '--disable-background-networking', ...extraArguments, `--remote-debugging-port=${debugPort}`, `--user-data-dir=${join(scratch, 'profile')}`], { stdio: 'ignore' });
    let launchError;
    chrome.on('error', error => { launchError = error; });
    const version = await waitFor(`http://127.0.0.1:${debugPort}/json/version`, value => Boolean(value.webSocketDebuggerUrl), 'headless Chrome').catch(error => { throw launchError || error; });
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
        const timer = setTimeout(() => { pending.delete(id); rejectMessage(new Error(`Timed out calling ${method}`)); }, 15000);
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
      if (result.exceptionDetails) throw new Error(result.exceptionDetails.exception?.description || result.exceptionDetails.text);
      return result.result.value;
    };
    const waitPage = async (expression, label) => {
      const deadline = Date.now() + 15000;
      while (Date.now() < deadline) {
        if (await evaluate(expression).catch(() => false)) return; // The page may still be loading.
        await new Promise(resolveDelay => setTimeout(resolveDelay, 30));
      }
      throw new Error(`Timed out waiting for ${label}`);
    };
    const click = text => evaluate(`[...document.querySelectorAll('dialog[open] button, #passkey-login button')].find(button => button.textContent.trim() === ${JSON.stringify(text)}).click()`);

    // A virtual authenticator that holds discoverable credentials and approves
    // user verification, standing in for a platform authenticator.
    await connection.send('WebAuthn.enable');
    const { authenticatorId } = await connection.send('WebAuthn.addVirtualAuthenticator', { options: { protocol: 'ctap2', transport: 'internal', hasResidentKey: true, hasUserVerification: true, isUserVerified: true, automaticPresenceSimulation: true } });
    await connection.send('Page.enable');
    await connection.send('Page.navigate', { url: `http://localhost:${servicePort}/` });

    // Before any passkey exists the sign-in page offers to set one up.
    await waitPage("!document.querySelector('#login-view').hidden", 'sign-in page');
    assert(await evaluate("!document.querySelector('#passkey-login').hidden && !document.querySelector('#passkey-setup-row').hidden"), 'The sign-in page does not offer passkey sign-in and setup.');

    // Sign in with the password; the My account dialog offers to create a passkey.
    await evaluate(`(() => { document.querySelector('#username').value = 'admin'; document.querySelector('#password').value = ${JSON.stringify(PASSWORD)}; document.querySelector('#login-form button[type=submit]').click(); })()`);
    await waitPage("!document.querySelector('#dashboard-view').hidden", 'dashboard after password sign-in');
    await evaluate("document.querySelector('#account-button').click()");
    await waitPage("document.querySelector('dialog[open] h2')?.textContent === 'My account'", 'My account dialog');
    assert(await evaluate("[...document.querySelectorAll('dialog[open] button')].some(button => button.textContent === 'Create a passkey')"), 'The My account dialog has no Create a passkey button.');
    assert(await evaluate("document.querySelector('dialog[open] .passkey-settings').textContent.includes('No passkeys are set up')"), 'The dialog did not report that no passkey exists.');

    // Creating the passkey succeeds and is stored on the authenticator and the service.
    await click('Create a passkey');
    await waitPage("document.querySelector('#global-alert')?.textContent.includes('Passkey created')", 'passkey creation to succeed');
    const { credentials } = await connection.send('WebAuthn.getCredentials', { authenticatorId });
    assert(credentials.length === 1 && credentials[0].isResidentCredential, 'The authenticator does not hold one discoverable credential.');
    await evaluate("document.querySelector('#account-button').click()");
    await waitPage("document.querySelector('dialog[open] h2')?.textContent === 'My account'", 'My account dialog again');
    assert(await evaluate("document.querySelector('dialog[open] .passkey-row')?.textContent.includes('Passkey 1')"), 'The new passkey is not listed.');
    await evaluate("[...document.querySelectorAll('dialog[open] button')].find(button => button.textContent === 'Sign out').click()");
    await waitPage("!document.querySelector('#login-view').hidden && document.querySelector('#dashboard-view').hidden", 'sign-out');

    // Signed out: the passkey, not the password, signs back in.
    assert(await evaluate("document.querySelector('#password').value === ''"), 'The password field is not empty after sign-out.');
    assert(await evaluate("document.querySelector('#passkey-setup-row').hidden"), 'The sign-in page still offers setup although a passkey exists.');
    await click('Sign in with a passkey');
    await waitPage("!document.querySelector('#dashboard-view').hidden", 'dashboard after passkey sign-in');
    assert(await evaluate("document.querySelector('#account-button').textContent === 'admin'"), 'Passkey sign-in did not sign in the administrator.');
    const used = await evaluate("fetch('/api/v1/auth/passkeys').then(response => response.json()).then(body => body.data.items[0].last_used_at)");
    assert(typeof used === 'string', 'The service did not record the passkey sign-in.');

    // With a passkey remembered on this device, opening the page signed out starts the passkey sign-in by itself.
    await evaluate("document.querySelector('#account-button').click()");
    await waitPage("document.querySelector('dialog[open] h2')?.textContent === 'My account'", 'My account dialog for the second sign-out');
    await evaluate("[...document.querySelectorAll('dialog[open] button')].find(button => button.textContent === 'Sign out').click()");
    await waitPage("!document.querySelector('#login-view').hidden && document.querySelector('#dashboard-view').hidden", 'second sign-out');
    await connection.send('Page.reload');
    await waitPage("!document.querySelector('#dashboard-view')?.hidden", 'automatic passkey sign-in after reload');
    console.log('PASS: headless Chrome created a passkey from My account, signed out, and signed back in with the passkey, including automatically on reload.');
  } finally {
    socket?.close();
    if (chrome?.pid && chrome.exitCode === null) { const exited = once(chrome, 'exit'); chrome.kill('SIGTERM'); await exited; }
    if (service?.pid && service.exitCode === null) { const exited = once(service, 'exit'); service.kill('SIGTERM'); await exited; }
    await rm(scratch, { recursive: true, force: true });
  }
}

main().catch(error => { console.error(`FAIL: ${error.message}`); process.exitCode = 1; });
