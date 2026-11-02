// Browser test of the artifact integrity metadata shown beside downloads: an available upload, a pinned upload,
// an expired upload, a deleted upload (with its deletion reason), an unfinalized upload and an external link, in
// both the task detail and Knowledge & evidence. Checks the availability badge, the copyable SHA-256, the size,
// media type and retention line, that no unavailable artifact has a download link, that external links are
// labelled unverified, that no artifact content is requested, and that the layout fits a mobile width.
//
//   CHROME_BIN=/path/to/chrome [CHROME_ARGS="--no-sandbox"] [UI_SHOTS=DIR] node scripts/test_artifact_integrity.mjs
//
// A fixture server stands in for the coordinator. Only Node's standard library and Chrome are used.
import { createServer } from 'node:http';
import { mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
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
const task = { id: 't1', title: 'Artifact task', description: 'Has artifacts.', revision: 1, kind: 'code', lifecycle: 'open', work_status: 'ready', created_at: '2026-10-12T00:00:00Z', acceptance_criteria: ['One'], current_attempt_id: null, processing: null, attempts: [], checkpoints: [], workflow: { phase: 'review', blockers: [], submission: null, activities: [], next_actions: [] }, job_evidence: { jobs: [], reservations: [] }, lifecycle_events: [], replaces: [] };

const digest = (letter) => letter.repeat(64);
const future = '2099-01-01T00:00:00Z', past = '2020-01-01T00:00:00Z';
const upload = (id, extra) => ({ id, project_id: 'p1', task_id: 't1', kind: 'upload', display_name: `${id}.txt`, media_type: 'text/plain', size_bytes: 1234, sha256: digest(id[0] === 'a' ? 'a' : id[0] === 'p' ? 'b' : id[0] === 'e' ? 'c' : id[0] === 'd' ? 'd' : 'e'), external_url: null, state: 'finalized', pinned: false, retention_until: future, deleted_at: null, deletion_reason: null, ...extra });
// Task history records omit the live `availability`; the artifact list reports it.
const records = [
  upload('available'),
  upload('pinned', { pinned: true, retention_until: past }),
  upload('expired', { retention_until: past }),
  upload('deleted', { state: 'deleted', deleted_at: '2026-09-01T00:00:00Z', deletion_reason: 'Contained a leaked credential' }),
  upload('reserved', { state: 'reserved', sha256: null, retention_until: null }),
  { id: 'link', project_id: 'p1', task_id: 't1', kind: 'external_link', display_name: 'link', media_type: 'text/html', size_bytes: null, sha256: digest('f'), external_url: 'https://example.invalid/evidence', state: 'finalized', pinned: false, retention_until: future, deleted_at: null, deletion_reason: null },
];
const availabilityOf = { available: 'available', pinned: 'available', expired: 'expired', deleted: 'deleted', reserved: 'pending', link: 'available' };
const listed = records.map((record) => ({ ...record, availability: availabilityOf[record.id], content_path: record.kind === 'upload' && availabilityOf[record.id] === 'available' ? `/api/v1/projects/p1/artifacts/${record.id}/content` : null }));
const contentRequests = [];

function json(response, data) {
  response.writeHead(200, { 'Content-Type': 'application/json', 'Cache-Control': 'no-store' });
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
  if (/\/content$/.test(path)) { contentRequests.push(path); response.writeHead(200); return response.end('bytes'); }
  const rest = (path.match(/^\/api\/v1\/projects\/p1(\/.*)?$/) || [])[1];
  if (rest === '/orientation') return json(response, { project: policy, policy_revision: policy.policy_revision });
  if (rest === '/tasks') return json(response, { items: [task], next_cursor: null });
  if (rest === '/tasks/t1') return json(response, task);
  if (rest === '/tasks/t1/history') return json(response, { items: records.map((record) => ({ record })), next_cursor: null });
  if (rest === '/artifacts') return json(response, { items: listed, next_cursor: null });
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
  const profile = await mkdtemp(join(tmpdir(), 'agent-coordinator-artifact-test-'));
  const executable = process.env.CHROME_BIN || 'chromium';
  const extra = (process.env.CHROME_ARGS || '').split(/\s+/).filter(Boolean);
  const chrome = spawn(executable, ['--headless=new', '--disable-gpu', '--no-first-run', '--no-default-browser-check', '--disable-background-networking', '--window-size=1200,900', `--remote-debugging-port=${debugPort}`, `--user-data-dir=${profile}`, ...extra], { stdio: 'ignore' });
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
    await send('Page.addScriptToEvaluateOnNewDocument', { source: `
      window.__copied = null; window.__rejectClipboard = false;
      Object.defineProperty(Navigator.prototype, 'clipboard', { configurable: true, value: {
        writeText(value) { if (window.__rejectClipboard) return Promise.reject(new DOMException('Denied', 'NotAllowedError')); window.__copied = value; return Promise.resolve(); }
      }});
    ` });
    await send('Page.navigate', { url: `http://127.0.0.1:${serverPort}/` });
    await waitPage("document.querySelector('.project-card')", 'project list');
    await evaluate("document.querySelector('.project-open').click()");
    await waitPage("document.querySelector('#tasks-list .task-row')", 'task list');
    await evaluate("document.querySelector('#tasks-list .task-row').click()");
    await waitPage("document.querySelectorAll('#task-attachments-list .task-attachment-entry').length === 6", 'task attachments');

    // What one artifact shows: its text, badge, links, digest and layout.
    const describe = (container, entrySelector, name) => evaluate(`(() => {
      const entry = [...document.querySelectorAll(${JSON.stringify(`${container} ${entrySelector}`)})].find((node) => node.textContent.includes(${JSON.stringify(name)}));
      if (!entry) return null;
      const box = entry.querySelector('.artifact-integrity'), badge = box.querySelector('.artifact-badge'), copy = box.querySelector('.artifact-copy'), sha = box.querySelector('.artifact-sha');
      return { text: box.textContent, badge: badge.textContent, availability: badge.dataset.availability, retention: box.querySelector('.artifact-retention')?.textContent ?? null, reason: box.querySelector('.artifact-reason')?.textContent ?? null,
        unverified: box.querySelector('.artifact-unverified')?.textContent ?? null, shaShown: sha?.textContent ?? null, shaFull: sha?.title ?? null, copyLabel: copy?.textContent ?? null, copyIsButton: copy ? copy.tagName === 'BUTTON' && copy.type === 'button' : null,
        download: Boolean(entry.querySelector('a[download]')), summary: box.querySelector('.artifact-summary').textContent, overflow: entry.scrollWidth > entry.clientWidth + 1 };
    })()`);
    const expectations = {
      'available.txt': { badge: 'Available', retention: 'Retained until', download: true, sha: digest('a') },
      'pinned.txt': { badge: 'Available', retention: 'Pinned', download: true, sha: digest('b') },
      'expired.txt': { badge: 'Expired', reason: 'Retention ended', download: false, sha: digest('c') },
      'deleted.txt': { badge: 'Deleted', reason: 'Contained a leaked credential', download: false, sha: digest('d') },
      'reserved.txt': { badge: 'Not finalized', reason: 'Not yet finalized', download: false, sha: null },
      link: { badge: 'Unverified link', retention: 'Retained until', unverified: 'does not verify this link', download: false, sha: digest('f') },
    };
    const check = async (where, container, entrySelector, downloadSelector) => {
      for (const [name, wanted] of Object.entries(expectations)) {
        const view = await describe(container, entrySelector, name);
        assert(view, `${where}: no entry for ${name}.`);
        assert(view.badge === wanted.badge, `${where}: ${name} badge reads "${view.badge}", expected "${wanted.badge}".`);
        assert(view.retention === null ? !wanted.retention : view.retention.startsWith(wanted.retention || '\u0000'), `${where}: ${name} retention reads ${JSON.stringify(view.retention)}, expected ${wanted.retention}.`);
        if (wanted.reason) assert(view.reason?.includes(wanted.reason), `${where}: ${name} does not state its reason (${view.reason}).`);
        else assert(view.reason === null, `${where}: ${name} states an unexpected reason: ${view.reason}.`);
        assert(view.download === wanted.download, `${where}: ${name} download link presence is ${view.download}, expected ${wanted.download}.`);
        assert(view.unverified ? Boolean(wanted.unverified) && view.unverified.includes(wanted.unverified) : !wanted.unverified, `${where}: ${name} unverified label is ${JSON.stringify(view.unverified)}.`);
        if (wanted.sha) {
          assert(view.shaFull === wanted.sha && view.shaShown.length < wanted.sha.length && wanted.sha.startsWith(view.shaShown.replace('…', '')), `${where}: ${name} digest is not truncated with the full value available.`);
          assert(view.copyIsButton && view.copyLabel === 'Copy SHA-256', `${where}: ${name} has no labelled copy button.`);
        } else assert(view.shaShown === null && view.text.includes('SHA-256: none recorded'), `${where}: ${name} should say that no digest is recorded.`);
        if (name !== 'link') assert(view.summary.startsWith('text/plain · 1234 bytes'), `${where}: ${name} summary reads "${view.summary}".`);
        assert(!view.overflow, `${where}: ${name} overflows its container.`);
      }
      assert(await evaluate(`document.querySelectorAll(${JSON.stringify(`${container} ${downloadSelector}`)}).length`) === 2, `${where}: exactly the two available uploads should have a download link.`);
    };
    await check('task detail', '#task-attachments-list', '.task-attachment-entry', 'a[download]');
    await shot('task-detail');

    // Copying hands over the full digest; a refused clipboard selects the full digest instead.
    const copyFirst = (name) => evaluate(`(() => { const entry = [...document.querySelectorAll('#task-attachments-list .task-attachment-entry')].find((node) => node.textContent.includes(${JSON.stringify(name)})); entry.querySelector('.artifact-copy').click(); })()`);
    await copyFirst('available.txt');
    await waitPage(`window.__copied === ${JSON.stringify(digest('a'))}`, 'the full digest on the clipboard');
    await waitPage("document.querySelector('#task-attachments-list .artifact-copy-status').textContent === 'SHA-256 copied.'", 'copy confirmation');
    await evaluate('window.__rejectClipboard = true');
    await copyFirst('expired.txt');
    await waitPage("[...document.querySelectorAll('#task-attachments-list .artifact-copy-status')].some((node) => node.textContent.includes('selected'))", 'the clipboard fallback message');
    assert(await evaluate(`window.getSelection().toString() === ${JSON.stringify(digest('c'))}`), 'The clipboard fallback did not select the full digest.');
    await evaluate('window.__rejectClipboard = false');
    assert(await evaluate(`(() => { const button = document.querySelector('#task-attachments-list .artifact-copy'); button.focus(); return document.activeElement === button; })()`), 'The copy button is not keyboard focusable.');

    // Knowledge & evidence shows the same facts for the same records.
    await evaluate("document.querySelector('.nav-item[data-view=shared]').click()");
    await waitPage("!document.querySelector('#shared-view').hidden", 'knowledge view');
    await evaluate("(() => { const select = document.querySelector('#shared-kind'); select.value = 'artifacts'; select.dispatchEvent(new Event('change', { bubbles: true })); })()");
    await waitPage("document.querySelectorAll('#shared-list article').length === 6 && document.querySelector('#shared-list .artifact-integrity')", 'artifact records');
    await check('knowledge & evidence', '#shared-list', 'article', 'a[download]');
    await shot('knowledge');
    const sharedLink = await evaluate("(() => { const card = [...document.querySelectorAll('#shared-list article')].find((node) => node.textContent.includes('link')); return { label: card.querySelector('.record-actions a')?.getAttribute('aria-label') }; })()");
    assert(sharedLink.label === 'Open the external evidence in a new tab', 'The external link action disappeared from Knowledge & evidence.');

    // Mobile width: nothing overflows the viewport and the digest and copy button stay reachable.
    await send('Emulation.setDeviceMetricsOverride', { width: 360, height: 740, deviceScaleFactor: 1, mobile: true });
    await sleep(200);
    const narrow = await evaluate("({ page: document.documentElement.scrollWidth <= window.innerWidth + 1, boxes: [...document.querySelectorAll('#shared-list .artifact-integrity')].every((box) => box.scrollWidth <= box.clientWidth + 1), copy: [...document.querySelectorAll('#shared-list .artifact-copy')].every((button) => button.getBoundingClientRect().right <= window.innerWidth) })");
    assert(narrow.page && narrow.boxes && narrow.copy, `Artifact metadata does not fit a 360px viewport: ${JSON.stringify(narrow)}`);
    await shot('knowledge-mobile');
    await send('Emulation.clearDeviceMetricsOverride');

    assert(contentRequests.length === 0, `Rendering artifact metadata requested artifact content: ${contentRequests.join(', ')}`);
    console.log('PASS: available, pinned, expired, deleted (with reason), unfinalized and external-link artifacts show their availability badge, size, media type, retention or Pinned, reason and copyable SHA-256 in the task detail and Knowledge & evidence; only available uploads have download links; external links are labelled unverified; no artifact content is requested; the layout fits a mobile width.');
  } finally {
    socket?.close();
    if (chrome.pid && chrome.exitCode === null) { const exited = once(chrome, 'exit'); chrome.kill('SIGTERM'); await exited; }
    await new Promise((done) => server.close(done));
    await rm(profile, { recursive: true, force: true }).catch(() => {});
    if (launchError) throw launchError;
  }
}

main().catch((error) => { console.error(`FAIL: ${error.message}`); process.exitCode = 1; });
