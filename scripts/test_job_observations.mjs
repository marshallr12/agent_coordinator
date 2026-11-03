// Browser test of the task page's job observations dialog (book/src/docs/web-ui-refresh.md).
//
//   CHROME_BIN=/path/to/chrome [CHROME_ARGS="--no-sandbox"] node scripts/test_job_observations.mjs
//
// A fixture server stands in for the coordinator's task history. The test opens one job's observations,
// filters by job, search text and state, loads more pages, reaches the 2,000-observation cap, and
// downloads the filtered JSON, which the page hands to a captured Blob instead of the file system.
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
const task = { id: 'task-1', title: 'Fixture task', description: 'Has two jobs.', revision: 1, kind: 'code', lifecycle: 'open', work_status: 'ready', created_at: '2026-10-01T00:00:00Z', acceptance_criteria: [] };
const job = (id, label) => ({ id, label, producer_id: `producer-${id}`, state: 'running', observation_freshness: 'fresh', last_observed_at: '2026-10-01T01:00:00Z', exit_code: null });
const detail = { ...task, attempts: [], checkpoints: [], workflow: { phase: 'open', blockers: [], next_actions: [], submission: null, activities: [] }, job_evidence: { jobs: [job('job-a', 'Unit tests'), job('job-b', 'Lint')], reservations: [] }, lifecycle_events: [], replaces: [] };

const observation = (jobId, sequence, state, summary, extra = {}) => ({
  kind: 'job_observations', relation: 'subject', task_id: task.id, occurred_at: `2026-10-01T00:${String(sequence % 60).padStart(2, '0')}:00Z`,
  record: { job_id: jobId, sequence, producer_id: `producer-${jobId}`, state, pid: 100 + sequence, process_started_at: 'start-token', exit_code: null, inputs_unchanged: true, summary, observed_at: `2026-10-01T00:${String(sequence % 60).padStart(2, '0')}:00Z`, payload_compacted_at: null, ...extra },
});
// Stored oldest first, like the service. The small set is served three records per page to exercise Load more.
const small = [
  observation('job-a', 1, 'running', 'compiling crates'),
  observation('job-b', 1, 'running', 'linting sources'),
  observation('job-a', 2, 'running', 'running widget tests', { payload_compacted_at: '2026-10-02T00:00:00Z', summary: '' }),
  observation('job-b', 2, 'failed', 'lint failure widget', { exit_code: 2 }),
  // A service that ever added credential fields to a record must not reach the page or the download.
  observation('job-a', 3, 'running', 'running gadget tests', { request_hash: 'request-secret-sentinel', reporter_token: 'reporter-secret-sentinel' }),
  observation('job-a', 4, 'failed', 'gadget test failed', { exit_code: 101 }),
  observation('job-a', 5, 'succeeded', 'all widget tests passed', { exit_code: 0 }),
];
// The large set is longer than the cap, and its last record is never reachable.
const large = Array.from({ length: 2300 }, (_, index) => observation(index % 2 ? 'job-b' : 'job-a', Math.floor(index / 2) + 1, 'running', `bulk progress ${index}`, { observed_at: new Date(Date.UTC(2026, 9, 1, 0, 0, index)).toISOString() }));
let dataset = small, pageSize = 3;
const historyQueries = [];

const files = { '/': ['web/index.html', 'text/html; charset=utf-8'], '/app.js': ['web/app.js', 'text/javascript; charset=utf-8'], '/style.css': ['web/style.css', 'text/css; charset=utf-8'] };
async function route(request, response) {
  const url = new URL(request.url, 'http://fixture.invalid');
  const path = url.pathname;
  const json = (data) => { response.writeHead(200, { 'Content-Type': 'application/json' }); response.end(JSON.stringify({ data })); };
  if (files[path]) { response.writeHead(200, { 'Content-Type': files[path][1], 'Cache-Control': 'no-store' }); return response.end(await readFile(join(root, files[path][0]))); }
  if (path === '/api/v1/me') return json({ actor, csrf_token: 'csrf' });
  if (path === '/api/v1/projects') return json({ items: projects });
  const rest = path.match(/^\/api\/v1\/projects\/p1(\/.*)?$/)?.[1];
  if (rest === '/tasks') return json({ items: [task], next_cursor: null });
  if (rest === '/tasks/archived') return json({ items: [], next_cursor: null });
  if (rest === `/tasks/${task.id}`) return json(detail);
  if (rest === `/tasks/${task.id}/history`) {
    historyQueries.push(url.search);
    if (url.searchParams.get('kind') !== 'job_observations') return json({ items: [], next_cursor: null });
    const limit = Math.min(Number(url.searchParams.get('limit')), pageSize), start = Number(url.searchParams.get('cursor') || 0);
    const items = dataset.slice(start, start + limit);
    return json({ project_id: 'p1', task_id: task.id, kind: 'job_observations', items, next_cursor: start + items.length < dataset.length ? String(start + items.length) : null });
  }
  if (rest?.endsWith('/orientation')) return json({ project: projects[0], policy_revision: 1 });
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

async function main() {
  const server = createServer((request, response) => route(request, response).catch((error) => { response.writeHead(500); response.end(String(error)); }));
  server.listen(0, '127.0.0.1');
  await once(server, 'listening');
  const serverPort = server.address().port;
  const debugPort = await unusedPort();
  const profile = await mkdtemp(join(tmpdir(), 'agent-coordinator-job-observations-'));
  const executable = process.env.CHROME_BIN || 'chromium';
  const extra = (process.env.CHROME_ARGS || '').split(/\s+/).filter(Boolean);
  const chrome = spawn(executable, ['--headless=new', '--disable-gpu', '--no-first-run', '--no-default-browser-check', '--disable-background-networking', '--window-size=1100,800', `--remote-debugging-port=${debugPort}`, `--user-data-dir=${profile}`, ...extra], { stdio: 'ignore' });
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

    await send('Page.enable');
    // Downloads are captured, not written: the page's Blob is read back and the object URL's revocation is recorded.
    await send('Page.addScriptToEvaluateOnNewDocument', { source: `
      window.__downloads = []; window.__revoked = [];
      const create = URL.createObjectURL.bind(URL), revoke = URL.revokeObjectURL.bind(URL);
      const blobs = new Map();
      URL.createObjectURL = (blob) => { const url = create(blob); blobs.set(url, blob); return url; };
      URL.revokeObjectURL = (url) => { window.__revoked.push(url); revoke(url); };
      HTMLAnchorElement.prototype.click = function () { const blob = blobs.get(this.href); window.__downloads.push({ name: this.download, url: this.href, text: blob ? blob.text() : Promise.resolve('') }); };
    ` });
    await send('Page.navigate', { url: `http://127.0.0.1:${serverPort}/#/projects/p1/task/${task.id}` });
    await waitPage("document.querySelectorAll('#job-evidence-content .evidence-entry').length === 2", 'task detail with two jobs');

    const rows = () => evaluate("[...document.querySelectorAll('dialog[open] .observation-list .evidence-entry h3')].map((node) => node.textContent)");
    const type = (id, value) => evaluate(`(() => { const input = document.querySelector('dialog[open] #${id}'); input.value = ${JSON.stringify(value)}; input.dispatchEvent(new Event(input.tagName === 'SELECT' ? 'change' : 'input', { bubbles: true })); })()`);
    const status = () => evaluate("document.querySelector('dialog[open] .observation-status').textContent");
    const openJob = (label) => evaluate(`[...document.querySelectorAll('#job-evidence-content .evidence-entry')].find((entry) => entry.textContent.includes(${JSON.stringify(label)})).querySelector('button').click()`);
    const download = async (previous) => {
      await evaluate("document.querySelector('dialog[open] button.primary').click()");
      await waitPage(`window.__downloads.length === ${previous + 1}`, 'download');
      return evaluate(`(async () => { const entry = window.__downloads[${previous}]; return { name: entry.name, revokedAfterWait: false, body: JSON.parse(await entry.text) }; })()`);
    };
    const sequences = async () => (await rows()).map((heading) => heading.split(' · ')[0]);

    // Job-a only, newest first, with the first page of three records and a Load more control.
    await waitPage("!document.querySelector('#task-attachments-state').textContent.startsWith('Loading')", 'task attachments settled');
    const before = await evaluate("document.querySelector('#task-detail-view').innerHTML");
    await openJob('Unit tests');
    await waitPage("document.querySelector('dialog[open] .observation-list .evidence-entry')", 'first page');
    assert((await sequences()).join() === '#2,#1', `First page should show job-a records newest first, saw ${await sequences()}`);
    assert(await evaluate("document.activeElement.id === 'observation-search'"), 'The search box should take focus.');
    assert(await evaluate("[...document.querySelectorAll('dialog[open] label')].every((label) => label.textContent.trim().length > 0 && label.querySelector('input, select'))"), 'Every filter needs a visible label.');
    assert(await evaluate("document.querySelector('dialog[open] .observation-list').textContent.includes('Summary cleared by retention')"), 'A compacted payload needs a note.');
    await evaluate("[...document.querySelectorAll('dialog[open] button')].find((button) => button.textContent === 'Load more').click()");
    await waitPage("document.querySelectorAll('dialog[open] .observation-list .evidence-entry').length === 4", 'second page');
    await evaluate("[...document.querySelectorAll('dialog[open] button')].find((button) => button.textContent === 'Load more').click()");
    await waitPage("document.querySelectorAll('dialog[open] .observation-list .evidence-entry').length === 5", 'last page');
    assert((await sequences()).join() === '#5,#4,#3,#2,#1', `Job-a records should be newest first, saw ${await sequences()}`);
    assert(await evaluate("![...document.querySelectorAll('dialog[open] button')].find((button) => button.textContent === 'Load more').offsetParent"), 'Load more should hide after the last page.');
    assert(!(await evaluate("document.querySelector('dialog[open] .observation-list').textContent.includes('linting sources')")), 'Another job’s observation leaked into the list.');

    // Search covers summary, state and exit code, case-insensitively, and combines with the state filter.
    await type('observation-search', 'WIDGET');
    assert((await sequences()).join() === '#5', `Summary search returned ${await sequences()}`);
    await type('observation-search', 'failed');
    assert((await sequences()).join() === '#4', `State search returned ${await sequences()}`);
    await type('observation-search', '101');
    assert((await sequences()).join() === '#4', `Exit-code search returned ${await sequences()}`);
    await type('observation-search', '');
    assert((await evaluate("[...document.querySelectorAll('#observation-state option')].map((option) => option.value).join()")) === ',failed,running,succeeded', 'The state filter should list the loaded states.');
    await type('observation-state', 'running');
    assert((await sequences()).join() === '#3,#2,#1', `State filter returned ${await sequences()}`);
    await type('observation-search', 'gadget');
    assert((await sequences()).join() === '#3', `Combined filters returned ${await sequences()}`);
    await type('observation-search', 'nothing matches');
    assert((await rows()).length === 0 && (await evaluate("document.querySelector('dialog[open] .observation-list').textContent.includes('No loaded observations match')")), 'An empty search needs a message.');
    assert(await evaluate("document.querySelector('dialog[open] button.primary').disabled"), 'Download should be disabled with nothing to download.');

    // Download holds exactly the filtered, loaded observations plus identities and retrieval time, with no credentials.
    await type('observation-search', ''); await type('observation-state', 'running');
    const started = Date.now();
    const first = await download(0);
    assert(first.name === 'job-observations-job-a.json', `Unexpected file name ${first.name}`);
    const { body } = first;
    assert(body.project_id === 'p1' && body.task_id === task.id && body.job_id === 'job-a', `Identities missing: ${JSON.stringify(body).slice(0, 200)}`);
    assert(Math.abs(Date.parse(body.retrieved_at) - started) < 60000, `Retrieval time is wrong: ${body.retrieved_at}`);
    assert(body.observations.map((item) => item.sequence).join() === '3,2,1', `Download should hold the filtered records newest first, saw ${body.observations.map((item) => item.sequence)}`);
    assert(body.observations.every((item) => item.job_id === 'job-a'), 'The download holds another job’s observations.');
    assert(body.observations[0].summary === 'running gadget tests' && body.observations[1].payload_compacted_at === '2026-10-02T00:00:00Z', 'The download lost observation content.');
    assert(body.filters.state === 'running' && body.load_limit_reached === false, 'The download omitted its filters.');
    assert(!/secret-sentinel|request_hash|reporter_token/.test(JSON.stringify(body)), 'The download mentions credential material.');
    await waitPage('window.__revoked.length === 1 && window.__revoked[0] === window.__downloads[0].url', 'object URL revocation');

    // Closing leaves the task page exactly as it was; the other job opens its own list.
    await evaluate("[...document.querySelectorAll('dialog[open] button')].find((button) => button.textContent === 'Close').click()");
    await waitPage("!document.querySelector('dialog[open]') && !document.querySelector('.observations-dialog')", 'closed dialog');
    const after = await evaluate("document.querySelector('#task-detail-view').innerHTML");
    assert(after === before, 'Closing the dialog changed the task detail view.');
    pageSize = 200;
    await openJob('Lint');
    await waitPage("document.querySelectorAll('dialog[open] .observation-list .evidence-entry').length === 2", 'job-b first page');
    assert((await sequences()).join() === '#2,#1' && (await status()).includes('2 of 2'), `Job-b list is wrong: ${await sequences()} / ${await status()}`);
    await evaluate("document.querySelector('dialog[open]').dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape' })); document.querySelector('dialog[open]').close()");

    // Mobile width: the dialog fits, and its controls stay reachable.
    await send('Emulation.setDeviceMetricsOverride', { width: 375, height: 700, deviceScaleFactor: 1, mobile: true });
    await openJob('Unit tests');
    await waitPage("document.querySelector('dialog[open] .observation-list .evidence-entry')", 'mobile dialog');
    assert(await evaluate("(() => { const box = document.querySelector('dialog[open]').getBoundingClientRect(); return box.left >= 0 && box.right <= window.innerWidth && document.documentElement.scrollWidth <= window.innerWidth; })()"), 'The dialog overflows a phone screen.');
    await evaluate("document.querySelector('dialog[open]').close()");
    await send('Emulation.clearDeviceMetricsOverride');

    // The load cap: 200 per request, 2,000 observations at most, then a visible message and no Load more.
    dataset = large; pageSize = 200; historyQueries.length = 0;
    await openJob('Unit tests');
    await waitPage("document.querySelector('dialog[open] .observation-list .evidence-entry')", 'large first page');
    for (let page = 2; page <= 10; page += 1) {
      await evaluate("[...document.querySelectorAll('dialog[open] button')].find((button) => button.textContent === 'Load more').click()");
      await waitPage(`/\\((${page * 200}) task observations read\\)/.test(document.querySelector('dialog[open] .observation-status').textContent)`, `page ${page}`);
    }
    await waitPage("document.querySelector('dialog[open] .inline-alert')?.textContent.includes('stopped at the limit of 2,000')", 'cap message');
    assert(historyQueries.length === 10 && historyQueries.every((query) => query.includes('limit=200')), `Expected ten 200-record requests, saw ${historyQueries}`);
    assert(await evaluate("![...document.querySelectorAll('dialog[open] button')].find((button) => button.textContent === 'Load more').offsetParent"), 'Load more should hide at the cap.');
    assert((await status()).includes('Showing 1000 of 1000'), `The cap should hold 1000 job-a records, saw ${await status()}`);
    assert((await sequences())[0] === '#1000', `Newest loaded record should lead, saw ${(await sequences())[0]}`);
    const capped = await download(1);
    assert(capped.body.observations.length === 1000 && capped.body.load_limit_reached === true, `The capped download should hold the 1000 loaded records and say it is limited, saw ${capped.body.observations.length}`);
    await evaluate("document.querySelector('dialog[open]').close()");
    console.log('PASS: the job observations dialog listed one job newest first, searched summary/state/exit code, filtered by state, paged with Load more, stopped at the 2,000-observation cap with a message, downloaded exactly the filtered loaded records with identities and retrieval time, revoked the object URL, left the task page unchanged on close, and fit a phone screen.');
  } finally {
    socket?.close();
    if (chrome.pid && chrome.exitCode === null) { const exited = once(chrome, 'exit'); chrome.kill('SIGTERM'); await exited; }
    await new Promise((done) => server.close(done));
    await rm(profile, { recursive: true, force: true });
    if (launchError) throw launchError;
  }
}

main().catch((error) => { console.error(`FAIL: ${error.message}`); process.exitCode = 1; });
