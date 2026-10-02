// UI verification against a supervised reviewer's verification environment
// (autonomy plan §2.3, M2). Signs in to the dashboard with the run's staging
// session (or a test login), opens a path, optionally asserts visible text,
// and saves a screenshot and the rendered DOM as review evidence.
//
//   node scripts/verify_ui.mjs [--path /] [--expect "text"]... [--out DIR]
//
// Reads $AGENTC_VERIFICATION (written by agentc-supervisor into $RUN) and runs
// $CHROME_BIN headless. Supervised launches name a session_file: a browser
// session the supervisor signed in for this run (decision U22), so no password
// is visible here. A hand-written description may instead name a
// credential_file with a username and password. Neither is ever printed.
// Only Node's standard library is used.
import { mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { spawn } from 'node:child_process';
import { once } from 'node:events';
import { parseArgs } from 'node:util';

/** Parses the command line; every option has a default. */
function options() {
  const { values } = parseArgs({ options: {
    path: { type: 'string', default: '/' },
    expect: { type: 'string', multiple: true, default: [] },
    out: { type: 'string', default: join(process.env.TMPDIR || tmpdir(), 'ui-evidence') },
  } });
  return values;
}

/** Loads the verification description and how to sign in: the run's
 * session ({ session }) or a test login ({ username, password }). */
async function environment() {
  const described = process.env.AGENTC_VERIFICATION;
  if (!described) throw new Error('AGENTC_VERIFICATION is not set; this launch has no verification environment');
  const verification = JSON.parse(await readFile(described, 'utf8'));
  const login = verification.session_file
    ? { session: JSON.parse(await readFile(verification.session_file, 'utf8')) }
    : JSON.parse(await readFile(verification.credential_file, 'utf8'));
  const browser = process.env.CHROME_BIN || verification.browser;
  if (!browser) throw new Error('no browser is offered for this verification environment');
  return { url: verification.url.replace(/\/$/, ''), login, browser };
}

/** A minimal Chrome DevTools Protocol client over Chrome's debugging pipe
 * (fd 3 carries commands to Chrome, fd 4 its replies, each message
 * NUL-terminated). A pipe, unlike --remote-debugging-port, cannot be reached
 * by other local accounts, so no one else can drive the reviewer's browser. */
function pipeClient(chrome) {
  const [toChrome, fromChrome] = [chrome.stdio[3], chrome.stdio[4]];
  let nextId = 1;
  let buffer = '';
  const pending = new Map();
  // A dying Chrome resets the pipe; earlyExit reports why, so these are quiet.
  for (const stream of [toChrome, fromChrome]) stream.on('error', () => {});
  fromChrome.on('data', chunk => {
    buffer += chunk.toString('utf8');
    let end;
    while ((end = buffer.indexOf('\0')) >= 0) {
      settle(pending, JSON.parse(buffer.slice(0, end)));
      buffer = buffer.slice(end + 1);
    }
  });
  const send = (method, params = {}, sessionId) => new Promise((resolve, reject) => {
    const id = nextId++;
    pending.set(id, m => (m.error ? reject(new Error(`${method}: ${m.error.message}`)) : resolve(m.result)));
    toChrome.write(JSON.stringify({ id, method, params, ...(sessionId && { sessionId }) }) + '\0');
  });
  return { send };
}

/** Resolves the pending request a DevTools reply answers; events are ignored. */
function settle(pending, message) {
  pending.get(message.id)?.(message);
  pending.delete(message.id);
}

/** Rejects with the tail of Chrome's stderr if it exits before answering
 * (for example "Socket path too long" when $TMPDIR is very deep). */
function earlyExit(chrome) {
  let stderr = '';
  chrome.stderr.on('data', chunk => { stderr = (stderr + chunk).slice(-2000); });
  return new Promise((_, reject) => chrome.on('exit', code =>
    reject(new Error(`browser exited (${code}) before DevTools answered: ${stderr.trim()}`))));
}

/** Starts headless Chrome and returns a DevTools client bound to a fresh page. */
async function openPage(browser, profile) {
  const chrome = spawn(browser, ['--headless=new', '--disable-gpu', '--no-first-run',
    '--no-default-browser-check', '--disable-background-networking',
    '--remote-debugging-pipe', `--user-data-dir=${profile}`],
  { stdio: ['ignore', 'ignore', 'pipe', 'pipe', 'pipe'] });
  const root = pipeClient(chrome);
  // Headless Chrome starts without a page; create one and attach a flat session.
  const { targetId } = await Promise.race([
    root.send('Target.createTarget', { url: 'about:blank' }), earlyExit(chrome)]);
  const { sessionId } = await root.send('Target.attachToTarget', { targetId, flatten: true });
  const page = { send: (method, params) => root.send(method, params, sessionId) };
  await page.send('Page.enable');
  return { chrome, page };
}

/** Evaluates `expression` in the page and returns its value. */
async function evaluate(page, expression) {
  const result = await page.send('Runtime.evaluate', { expression, awaitPromise: true, returnByValue: true });
  if (result.exceptionDetails) throw new Error(result.exceptionDetails.text);
  return result.result.value;
}

/** Waits up to 15 s for `expression` to become truthy in the page. */
async function waitFor(page, expression, label) {
  for (let i = 0; i < 300; i += 1) {
    if (await evaluate(page, expression)) return;
    await new Promise(done => setTimeout(done, 50));
  }
  throw new Error(`timed out waiting for ${label}`);
}

/** Installs a handed-over session cookie, as the coordinator set it. */
async function useSession(page, session) {
  const { name, value } = session.cookie;
  await page.send('Network.setCookie', { name, value, url: `${session.url}/`, path: '/',
    httpOnly: true, secure: session.url.startsWith('https:'), sameSite: 'Strict' });
}

/** Signs in with the run's session, or through the dashboard's login form. */
async function signIn(page, url, login) {
  if (login.session) await useSession(page, login.session);
  await page.send('Page.navigate', { url: `${url}/` });
  await waitFor(page, "!document.querySelector('#login-view')?.hidden || !document.querySelector('#dashboard-view')?.hidden", 'the dashboard or login view');
  const fill = (selector, value) => `(() => { const input = document.querySelector('${selector}'); input.value = ${JSON.stringify(value)}; input.dispatchEvent(new Event('input', { bubbles: true })); })()`;
  if (await evaluate(page, "!document.querySelector('#login-view').hidden")) {
    if (login.session) throw new Error('the staging session was not accepted; it may have ended');
    await evaluate(page, fill('#username', login.username));
    await evaluate(page, fill('#password', login.password));
    await evaluate(page, "document.querySelector('#login-form').requestSubmit()");
  }
  await waitFor(page, "!document.querySelector('#dashboard-view').hidden", 'the signed-in dashboard');
}

/** Opens `path`, checks each expected text, and writes the evidence files. */
async function capture(page, url, opts) {
  if (opts.path !== '/') await page.send('Page.navigate', { url: url + opts.path });
  await waitFor(page, "document.readyState === 'complete'", 'page load');
  for (const text of opts.expect) {
    await waitFor(page, `document.body.innerText.includes(${JSON.stringify(text)})`, `text ${JSON.stringify(text)}`);
  }
  await mkdir(opts.out, { recursive: true });
  const shot = await page.send('Page.captureScreenshot', { format: 'png' });
  await writeFile(join(opts.out, 'screenshot.png'), Buffer.from(shot.data, 'base64'));
  await writeFile(join(opts.out, 'dom.html'), await evaluate(page, 'document.documentElement.outerHTML'));
}

/** Runs one verification and prints a JSON summary for the review record. */
async function main() {
  const opts = options();
  const env = await environment();
  const profile = await mkdtemp(join(process.env.TMPDIR || tmpdir(), 'verify-ui-'));
  const { chrome, page } = await openPage(env.browser, profile);
  try {
    await signIn(page, env.url, env.login);
    await capture(page, env.url, opts);
    console.log(JSON.stringify({ ok: true, url: env.url + opts.path, expected: opts.expect, evidence: opts.out }));
  } finally {
    if (chrome.exitCode === null) { const exited = once(chrome, 'exit'); chrome.kill('SIGTERM'); await exited; }
    await rm(profile, { recursive: true, force: true });
  }
}

main().catch(error => { console.error(JSON.stringify({ ok: false, error: error.message })); process.exitCode = 1; });
