// Headless-Chrome check of the bounded evidence preview.
// Run with: CHROME_BIN=/path/to/chrome [CHROME_ARGS="--no-sandbox"] node scripts/test_artifact_preview.mjs
import { createServer } from 'node:http';
import { mkdtemp, readFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { spawn } from 'node:child_process';
import { once } from 'node:events';
import { createHash } from 'node:crypto';
import net from 'node:net';

const root = resolve(import.meta.dirname, '..');
const LIMIT = 256 * 1024;
const task = {
  id: 'task-preview-1', title: 'Preview fixture task', description: 'Synthetic browser-fixture task.', revision: 1, kind: 'code', priority: 2,
  lifecycle: 'open', work_status: 'ready', acceptance_criteria: [], attempts: [], checkpoints: [], workflow: { activities: [] }, job_evidence: { jobs: [], reservations: [] },
};
const actor = { id: 'fixture-operator', name: 'Fixture operator', role: 'admin', kind: 'human', session_id: 'fixture-browser' };
const sha = (bytes) => createHash('sha256').update(bytes).digest('hex');

// Each fixture: the served bytes and the history record. `record` fields override the defaults.
const malicious = '<img src=x onerror="window.__pwned = true"><script>window.__pwned = true</script><a href="https://example.invalid/">link</a>\n<svg onload="window.__pwned = true"></svg>';
const bigJson = JSON.stringify({ rows: Array.from({ length: 2000 }, (_, index) => ({ index, text: `line ${index} `.repeat(4) })) });
const fixtures = [
  { id: 'json-ok', name: 'result.json', type: 'application/json', body: '{"b":[1,2],"a":{"ok":true}}' },
  { id: 'json-vendor', name: 'report.json', type: 'application/vnd.report+json; charset=utf-8', body: '{"vendor":1}' },
  { id: 'json-bad', name: 'broken.json', type: 'application/json', body: '{"unterminated": ' },
  { id: 'json-big', name: 'big.json', type: 'application/json', body: bigJson },
  { id: 'markdown', name: 'notes.md', type: 'text/markdown', body: '# Heading\n\n* item with **bold** and `code`\n' },
  { id: 'text-utf8', name: 'plain.txt', type: 'text/plain', body: Buffer.concat([Buffer.from('caf\u00e9 '), Buffer.from([0xff]), Buffer.from(' end')]) },
  { id: 'malicious-html', name: 'page.html', type: 'text/html', body: malicious },
  { id: 'malicious-md', name: 'evil.md', type: 'text/markdown', body: `# Title\n${malicious}` },
  { id: 'mismatch', name: 'tampered.txt', type: 'text/plain', body: 'actual bytes', recordedSha: sha('other bytes') },
  { id: 'oversized-record', name: 'huge.txt', type: 'text/plain', body: 'x'.repeat(LIMIT + 1) },
  { id: 'lying-size', name: 'lying.txt', type: 'text/plain', body: 'y'.repeat(LIMIT + 4096), recordSize: 100, chunked: true },
  { id: 'image', name: 'picture.png', type: 'image/png', body: Buffer.from([0x89, 0x50, 0x4e, 0x47]) },
  { id: 'binary', name: 'data.bin', type: 'application/octet-stream', body: Buffer.from([1, 2, 3]) },
  { id: 'deleted', name: 'gone.txt', type: 'text/plain', body: 'gone', record: { deleted_at: '2026-10-01T00:00:00Z' } },
  { id: 'external', name: 'link.txt', type: 'text/plain', body: 'ext', record: { kind: 'external_link', external_url: 'https://example.invalid/e' } },
];
const contentFetches = [];
for (const fixture of fixtures) {
  fixture.bytes = Buffer.isBuffer(fixture.body) ? fixture.body : Buffer.from(fixture.body);
  fixture.record = {
    id: fixture.id, project_id: 'fixture-project', task_id: task.id, kind: 'upload', display_name: fixture.name, media_type: fixture.type,
    size_bytes: fixture.recordSize ?? fixture.bytes.length, sha256: fixture.recordedSha ?? sha(fixture.bytes), state: 'finalized', pinned: true,
    retention_until: null, deleted_at: null, ...(fixture.record || {}),
  };
}

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
    if (url.pathname === '/api/v1/me') return json(response, { actor, csrf_token: 'fixture-csrf' });
    if (url.pathname === '/api/v1/projects') return json(response, { items: [{ id: 'fixture-project', name: 'Fixture project', target_branch: 'main' }] });
    if (url.pathname === '/api/v1/projects/fixture-project/orientation') return json(response, { project: { id: 'fixture-project', name: 'Fixture project', policy_revision: 1 }, policy_revision: 1 });
    if (url.pathname === '/api/v1/projects/fixture-project/workflow-policy') return json(response, { revision: 1, required_checks: [] });
    if (url.pathname === '/api/v1/projects/fixture-project/tasks') return json(response, { items: [task], next_cursor: null });
    if (url.pathname === `/api/v1/projects/fixture-project/tasks/${task.id}/history` && url.searchParams.get('kind') === 'artifacts') {
      return json(response, { items: fixtures.map((fixture) => ({ task_id: task.id, relation: 'subject', record: fixture.record })), next_cursor: null });
    }
    if (url.pathname === `/api/v1/projects/fixture-project/tasks/${task.id}`) return json(response, task);
    const match = url.pathname.match(/^\/api\/v1\/projects\/fixture-project\/artifacts\/([^/]+)\/content$/);
    if (match) {
      const fixture = fixtures.find((candidate) => candidate.id === decodeURIComponent(match[1]));
      if (!fixture) { response.writeHead(404); response.end(); return; }
      contentFetches.push(fixture.id);
      const headers = { 'Content-Type': fixture.type, 'Content-Disposition': `attachment; filename="${fixture.name}"`, 'X-Content-Type-Options': 'nosniff', 'Cache-Control': 'no-store' };
      if (fixture.chunked) {
        // No Content-Length: the body is sent in pieces so the page must stop reading it mid-stream.
        response.writeHead(200, headers);
        for (let offset = 0; offset < fixture.bytes.length && !response.destroyed; offset += 16384) {
          response.write(fixture.bytes.subarray(offset, offset + 16384));
          await new Promise((resolveDelay) => setTimeout(resolveDelay, 5));
        }
        response.end();
        return;
      }
      response.writeHead(200, headers);
      response.end(fixture.bytes);
      return;
    }
    const file = files[url.pathname];
    if (!file) { response.writeHead(404); response.end(); return; }
    response.writeHead(200, { 'Content-Type': file[1], 'Cache-Control': 'no-store' });
    response.end(await readFile(join(root, file[0])));
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
  const profile = await mkdtemp(join(tmpdir(), 'agent-coordinator-preview-test-'));
  const executable = process.env.CHROME_BIN || (process.platform === 'win32' ? 'C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe' : 'chromium');
  const extra = (process.env.CHROME_ARGS || '').split(/\s+/).filter(Boolean);
  const chrome = spawn(executable, [
    ...extra, '--headless=new', '--disable-gpu', '--no-first-run', '--no-default-browser-check',
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
    connection = await connect(page.find((item) => item.id === target.targetId).webSocketDebuggerUrl);
    socket = connection.connection;
    const { send } = connection;
    const evaluate = async (expression) => {
      const result = await send('Runtime.evaluate', { expression, awaitPromise: true, returnByValue: true });
      if (result.exceptionDetails) throw new Error(`${result.exceptionDetails.exception?.description || result.exceptionDetails.text} IN: ${expression.slice(0, 300)}`);
      return result.result.value;
    };
    const waitPage = async (expression, label) => {
      const deadline = Date.now() + 10000;
      while (Date.now() < deadline) {
        if (await evaluate(expression)) return;
        await new Promise((resolveDelay) => setTimeout(resolveDelay, 30));
      }
      throw new Error(`Timed out waiting for ${label} (dialog: ${await evaluate("document.querySelector('dialog')?.textContent.slice(0, 300) || ''")})`);
    };
    const press = async (key, code, virtualKey) => {
      await send('Input.dispatchKeyEvent', { type: 'keyDown', key, code, windowsVirtualKeyCode: virtualKey });
      await send('Input.dispatchKeyEvent', { type: 'keyUp', key, code, windowsVirtualKeyCode: virtualKey });
    };

    await send('Page.enable');
    await send('Page.navigate', { url: `http://127.0.0.1:${serverPort}/` });
    await waitPage("document.querySelector('.project-open')", 'project list');
    await evaluate("document.querySelector('.project-open').click()");
    await waitPage("document.querySelector('#tasks-list .task-row')", 'task queue');
    await evaluate("document.querySelector('#tasks-list .task-row').click()");
    await waitPage(`document.querySelectorAll('#task-attachments-list .task-attachment-entry').length === ${fixtures.length}`, 'attachment list');

    const entry = (name) => `[...document.querySelectorAll('#task-attachments-list .task-attachment-entry')].find((node) => node.querySelector('strong').textContent === ${JSON.stringify(name)})`;
    const previewable = new Set(['result.json', 'report.json', 'broken.json', 'big.json', 'notes.md', 'plain.txt', 'page.html', 'evil.md', 'tampered.txt', 'lying.txt']);
    for (const fixture of fixtures) {
      const hasPreview = await evaluate(`Boolean((${entry(fixture.name)}).querySelector('button'))`);
      assert(hasPreview === previewable.has(fixture.name), `${fixture.name} ${hasPreview ? 'offers' : 'lacks'} a preview unexpectedly.`);
    }
    for (const name of ['huge.txt', 'picture.png', 'data.bin']) {
      assert(await evaluate(`Boolean((${entry(name)}).querySelector('a[download]'))`), `${name} lost its download link.`);
    }
    assert(contentFetches.length === 0, 'Listing attachments fetched file content.');

    const open = async (name) => {
      await evaluate(`(() => { const button = (${entry(name)}).querySelector('button'); button.focus(); button.click(); })()`);
      await waitPage("document.querySelector('dialog.preview-dialog[open] .preview-text, dialog.preview-dialog[open] .inline-alert.error')", `${name} preview`);
    };
    const dialogText = () => evaluate("document.querySelector('dialog.preview-dialog .preview-text')?.textContent ?? null");
    const dialogNotes = () => evaluate("document.querySelector('dialog.preview-dialog .preview-body').textContent");
    const verdict = async (fragment) => waitPage(`document.querySelector('dialog.preview-dialog .preview-body').textContent.includes(${JSON.stringify(fragment)})`, `verdict "${fragment}"`);
    const closeByEscape = async () => {
      await press('Escape', 'Escape', 27);
      await waitPage("!document.querySelector('dialog.preview-dialog')", 'Escape to close the preview');
    };
    const noUnsafeNodes = async (label) => assert(await evaluate("(() => { const body = document.querySelector('dialog.preview-dialog'); return !body.querySelector('script, img, a, svg, iframe, object, embed') && !window.__pwned; })()"), `${label} produced live elements or ran script.`);

    // JSON is pretty-printed, and a matching digest is reported.
    await open('result.json');
    assert(await dialogText() === JSON.stringify({ b: [1, 2], a: { ok: true } }, null, 2), 'JSON was not pretty-printed with two spaces.');
    await verdict('SHA-256 verified in this browser');
    assert(await evaluate("document.activeElement.textContent === 'Close'"), 'Opening the preview did not focus its Close button.');
    assert(await evaluate("getComputedStyle(document.querySelector('.preview-text')).whiteSpace") === 'pre-wrap', 'Preview text does not wrap.');
    await closeByEscape();
    assert(await evaluate("document.activeElement === (" + entry('result.json') + ").querySelector('button')"), 'Closing the preview did not return focus to its button.');

    await open('report.json');
    assert(await dialogText() === '{\n  "vendor": 1\n}', 'An application/*+json type was not pretty-printed.');
    await closeByEscape();

    await open('broken.json');
    assert(await dialogText() === '{"unterminated": ', 'Invalid JSON was not shown as received.');
    assert((await dialogNotes()).includes('Not valid JSON'), 'Invalid JSON was not labeled.');
    await closeByEscape();

    // Markdown and text are plain preformatted text; invalid UTF-8 is replaced, not rejected.
    await open('notes.md');
    assert(await dialogText() === '# Heading\n\n* item with **bold** and `code`\n', 'Markdown was not shown as literal text.');
    assert(await evaluate("!document.querySelector('dialog.preview-dialog .preview-text').children.length && document.querySelector('dialog.preview-dialog .preview-text').tagName === 'PRE'"), 'Markdown was rendered as elements.');
    await closeByEscape();
    await open('plain.txt');
    assert(await dialogText() === 'caf\u00e9 \ufffd end', 'Invalid UTF-8 was not decoded leniently.');
    await closeByEscape();

    // Malicious content is displayed literally and never runs.
    for (const [name, expected] of [['page.html', malicious], ['evil.md', `# Title\n${malicious}`]]) {
      await open(name);
      assert(await dialogText() === expected, `${name} was not shown literally.`);
      await verdict('SHA-256 verified');
      await noUnsafeNodes(name);
      await closeByEscape();
    }
    await new Promise((resolveDelay) => setTimeout(resolveDelay, 150));
    assert(!(await evaluate('window.__pwned')), 'Content from a malicious file executed.');

    // A digest mismatch is stated, not hidden.
    await open('tampered.txt');
    await verdict('SHA-256 MISMATCH');
    assert(await evaluate("document.querySelector('dialog.preview-dialog .inline-alert.error')?.textContent.includes('MISMATCH')"), 'A mismatch was not shown as an error.');
    await closeByEscape();

    // Without crypto.subtle the preview says it is not a verification.
    await evaluate("Object.defineProperty(globalThis.crypto, 'subtle', { configurable: true, value: undefined })");
    await open('result.json');
    await verdict('does not verify the file');
    assert(!(await dialogNotes()).includes('verified in this browser'), 'A preview claimed verification without crypto.subtle.');
    await closeByEscape();
    await evaluate("delete globalThis.crypto.subtle");

    // A body longer than the cap is aborted mid-stream with a message and no content.
    contentFetches.length = 0;
    await open('big.json');
    assert((await dialogText()).length > 100000 && await evaluate("document.querySelector('.preview-text').scrollHeight > document.querySelector('.preview-text').clientHeight"), 'A large in-cap JSON file did not scroll inside its preview.');
    await closeByEscape();
    await open('lying.txt');
    assert(await dialogText() === null, 'An over-cap body still produced a preview.');
    assert((await dialogNotes()).includes('preview limit'), 'The aborted preview gave no message.');
    await closeByEscape();

    // Mobile width: the dialog stays inside the viewport, scrolls inside itself, and keeps keyboard reach.
    await send('Emulation.setDeviceMetricsOverride', { width: 375, height: 667, deviceScaleFactor: 1, mobile: true });
    await open('big.json');
    const geometry = await evaluate(`(() => { const rect = document.querySelector('dialog.preview-dialog').getBoundingClientRect(); const pre = document.querySelector('.preview-text'); const body = document.querySelector('.preview-body');
      return { top: rect.top, left: rect.left, right: rect.right, bottom: rect.bottom, width: window.innerWidth, height: window.innerHeight, pageWide: document.documentElement.scrollWidth > window.innerWidth, preScrolls: pre.scrollHeight > pre.clientHeight, bodyOverflow: getComputedStyle(body).overflowY, dialogOverflowX: document.querySelector('dialog.preview-dialog').scrollWidth > document.querySelector('dialog.preview-dialog').clientWidth }; })()`);
    assert(geometry.top >= 0 && geometry.left >= 0 && geometry.right <= geometry.width && geometry.bottom <= geometry.height, `The preview overflows the mobile viewport: ${JSON.stringify(geometry)}`);
    assert(!geometry.pageWide && !geometry.dialogOverflowX && geometry.preScrolls && geometry.bodyOverflow === 'auto', `The preview does not scroll within the mobile viewport: ${JSON.stringify(geometry)}`);
    await press('Tab', 'Tab', 9);
    assert(await evaluate("document.activeElement.classList.contains('preview-text')"), 'Tab did not reach the scrollable preview text.');
    await closeByEscape();
    await send('Emulation.clearDeviceMetricsOverride');

    // Closing a slow preview aborts its request, and no second dialog stacks on the first.
    await evaluate(`(${entry('lying.txt')}).querySelector('button').click()`);
    await evaluate("document.querySelector('dialog.preview-dialog button').click()");
    await waitPage("!document.querySelector('dialog.preview-dialog')", 'Close button');
    await evaluate(`(${entry('result.json')}).querySelector('button').click(); (${entry('result.json')}).querySelector('button').click()`);
    assert(await evaluate("document.querySelectorAll('dialog.preview-dialog').length") === 1, 'Preview dialogs stacked.');
    await closeByEscape();
    assert(await evaluate('!window.__pwned'), 'Content executed during the run.');

    console.log('PASS: headless Chrome verified evidence previews: pretty JSON (including +json and invalid JSON), literal Markdown/text, lenient UTF-8, literal malicious HTML/SVG/script, SHA-256 match, mismatch and no-crypto wording, no preview for oversized, non-text, deleted and external records, a mid-stream abort past the cap, Escape and Close dismissal with focus return, keyboard reach and in-viewport scrolling at mobile width.');
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
