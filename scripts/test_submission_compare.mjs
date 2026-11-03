// Browser test of the submission comparison on the task detail page (web/app.js, compareSubmissions).
//
//   CHROME_BIN=/path/to/chrome [CHROME_ARGS="--no-sandbox"] node scripts/test_submission_compare.mjs
//
// A fixture server stands in for the coordinator. One task has two submissions with a changes-requested review
// of the first; one has a single submission; one serves its submissions in two history pages. The test checks
// the default comparison, the findings beside the earlier submission, text markers on changed rows, the
// single-submission explanation, the bounded page requests and the mobile layout.
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
const makeTask = (id, title) => ({ id, title, description: `Description of ${title}`, revision: 1, kind: 'code', lifecycle: 'open', work_status: 'ready', created_at: '2026-10-01T00:00:00Z', acceptance_criteria: [] });
const tasks = [makeTask('task-multi', 'Two submissions'), makeTask('task-single', 'One submission'), makeTask('task-paged', 'Paged submissions')];
const submission = (id, extra) => ({
  id, attempt_id: `attempt-${id}`, kind: 'code', task_revision: 1, project_policy_revision: 1, workflow_policy_revision: 1, summary: 'Summary', handoff: 'Handoff', lessons: [],
  artifact_ids: [], candidate_revision: 'a'.repeat(40), candidate_tree: 'b'.repeat(40), candidate_ref: 'refs/ac/one', acceptance_evidence: [], created_at: '2026-10-02T00:00:00Z', ...extra,
});
const first = submission('sub-first-0001', {
  summary: 'First attempt <img src=x onerror="window.injected=1">', handoff: 'Same handoff', artifact_ids: ['artifact-kept', 'artifact-dropped'], candidate_revision: '1'.repeat(40),
  acceptance_evidence: [{ criterion: 'Criterion A', evidence: 'Ran A once' }, { criterion: 'Criterion B', evidence: 'B is fine' }, { criterion: 'Criterion D', evidence: 'D only before' }],
});
const second = submission('sub-second-0002', {
  summary: 'Second attempt', handoff: 'Same handoff', artifact_ids: ['artifact-kept', 'artifact-new'], candidate_revision: '2'.repeat(40), created_at: '2026-10-03T00:00:00Z',
  acceptance_evidence: [{ criterion: 'Criterion A', evidence: 'Ran A twice' }, { criterion: 'Criterion B', evidence: 'B is fine' }, { criterion: 'Criterion C', evidence: 'C is new' }],
});
const review = (id, submissionId, decision, summary, findings) => ({ activity: { id, submission_id: submissionId, kind: 'agent_review', state: 'completed' }, decision: { decision, summary, reviewer_id: 'reviewer-1' }, findings });
const reviews = [
  review('act-1', first.id, 'changes_requested', 'Evidence for A is thin', [{ id: 'f1', severity: 'required', remedy: 'Run criterion A twice', evidence: 'Only one run was logged' }]),
  review('act-2', second.id, 'approved', 'Looks right', []),
];
const pagedFirst = submission('sub-paged-0001', { summary: 'Paged one' }), pagedSecond = submission('sub-paged-0002', { summary: 'Paged two', created_at: '2026-10-03T00:00:00Z' });
const histories = {
  'task-multi': { submissions: [[first, second]], reviews: [reviews] },
  'task-single': { submissions: [[first]], reviews: [[]] },
  'task-paged': { submissions: [[pagedFirst], [pagedSecond]], reviews: [[]] },
};
const details = Object.fromEntries(tasks.map((task) => [task.id, {
  ...task, attempts: [], checkpoints: [], lifecycle_events: [], replaces: [], job_evidence: { jobs: [], reservations: [] },
  workflow: { phase: 'review', blockers: [], next_actions: [], activities: [], submission: { ...histories[task.id].submissions.flat().at(-1), task_id: task.id } },
}]));
// Every history request the page made, to prove they are bounded.
const historyRequests = [];

const files = { '/': ['web/index.html', 'text/html; charset=utf-8'], '/app.js': ['web/app.js', 'text/javascript; charset=utf-8'], '/style.css': ['web/style.css', 'text/css; charset=utf-8'] };
async function route(request, response) {
  const url = new URL(request.url, 'http://fixture.invalid');
  const path = url.pathname;
  const json = (data) => { response.writeHead(200, { 'Content-Type': 'application/json' }); response.end(JSON.stringify({ data })); };
  if (files[path]) { response.writeHead(200, { 'Content-Type': files[path][1], 'Cache-Control': 'no-store' }); return response.end(await readFile(join(root, files[path][0]))); }
  if (path === '/api/v1/me') return json({ actor, csrf_token: 'csrf' });
  if (path === '/api/v1/projects') return json({ items: projects });
  const rest = path.match(/^\/api\/v1\/projects\/p1(\/.*)?$/)?.[1];
  if (rest === '/tasks') return json({ items: tasks, next_cursor: null });
  if (rest === '/tasks/archived') return json({ items: [], next_cursor: null });
  const detailMatch = rest?.match(/^\/tasks\/([^/]+)$/);
  if (detailMatch && details[detailMatch[1]]) return json(details[detailMatch[1]]);
  const historyMatch = rest?.match(/^\/tasks\/([^/]+)\/history$/);
  if (historyMatch && histories[historyMatch[1]]) {
    const kind = url.searchParams.get('kind'), limit = Number(url.searchParams.get('limit')), cursor = url.searchParams.get('cursor');
    historyRequests.push({ task: historyMatch[1], kind, limit, cursor });
    const pages = histories[historyMatch[1]][kind] || [[]], index = cursor ? Number(cursor) : 0;
    const items = (pages[index] || []).map((record) => ({ task_id: historyMatch[1], relation: 'subject', occurred_at: record.created_at, record }));
    return json({ items, next_cursor: index + 1 < pages.length ? String(index + 1) : null });
  }
  if (rest?.endsWith('/orientation')) return json({ project: projects[0], policy_revision: 1 });
  if (path === '/api/v1/admin/clock' || path === '/api/v1/admin/restore') return json({});
  if (rest || path.startsWith('/api/v1/')) return json({ items: [], next_cursor: null });
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
  const profile = await mkdtemp(join(tmpdir(), 'agent-coordinator-compare-'));
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
    const waitPage = async (expression, label) => {
      const deadline = Date.now() + 10000;
      while (Date.now() < deadline) { if (await evaluate(expression)) return; await sleep(30); }
      throw new Error(`Timed out waiting for ${label}`);
    };
    const compareButton = "[...document.querySelectorAll('#workflow-content button')].find((button) => button.textContent === 'Compare with previous submission')";
    const dialogRows = "[...document.querySelectorAll('dialog.compare-dialog[open] .compare-row')]";
    const openComparison = async (taskId) => {
      await send('Page.navigate', { url: 'about:blank' });
      await send('Page.navigate', { url: `http://127.0.0.1:${serverPort}/#/projects/p1/task/${taskId}` });
      await waitPage(`Boolean(${compareButton})`, `compare button for ${taskId}`);
      await evaluate(`${compareButton}.click()`);
      await waitPage("Boolean(document.querySelector('dialog.compare-dialog[open]'))", 'comparison dialog');
    };
    const closeComparison = () => evaluate("document.querySelector('dialog.compare-dialog[open]').close()");
    const rowFor = (title) => `${dialogRows}.find((row) => row.querySelector('.compare-head').textContent.includes(${JSON.stringify(title)}))`;

    await send('Page.enable');

    // Default comparison: the latest submission against the one before it, with a text marker on every difference.
    await openComparison('task-multi');
    await waitPage(`${dialogRows}.length > 5`, 'comparison rows');
    assert(await evaluate("document.querySelector('#workflow-compare-earlier').value === 'sub-first-0001' && document.querySelector('#workflow-compare-later').value === 'sub-second-0002'"), 'The default comparison should be the latest submission against the one before it.');
    for (const id of ['workflow-compare-earlier', 'workflow-compare-later']) assert(await evaluate(`document.getElementById('${id}').labels[0].textContent.length > 0`), `${id} has no visible label.`);
    const status = async (title) => evaluate(`(${rowFor(title)})?.dataset.status`);
    const marker = async (title) => evaluate(`(${rowFor(title)})?.querySelector('.compare-marker').textContent`);
    for (const [title, expected, word] of [['Summary', 'changed', 'Changed'], ['Source revision', 'changed', 'Changed'], ['Source tree', 'unchanged', 'Unchanged'], ['Handoff', 'unchanged', 'Unchanged'],
      ['Criterion A', 'changed', 'Changed'], ['Criterion B', 'unchanged', 'Unchanged'], ['Criterion C', 'added', 'Added'], ['Criterion D', 'removed', 'Removed'],
      ['artifact-kept', 'unchanged', 'Unchanged'], ['artifact-new', 'added', 'Added'], ['artifact-dropped', 'removed', 'Removed']]) {
      assert((await status(title)) === expected, `${title}: expected status ${expected}, saw ${await status(title)}.`);
      assert((await marker(title)) === word, `${title}: expected the text marker ${word}, saw ${await marker(title)}.`);
    }
    const cells = (title) => evaluate(`[...(${rowFor(title)}).querySelectorAll('.compare-cell')].map((cell) => cell.textContent)`);
    const [evidenceBefore, evidenceAfter] = await cells('Criterion A');
    assert(evidenceBefore.includes('Earlier') && evidenceBefore.includes('Ran A once') && evidenceAfter.includes('Later') && evidenceAfter.includes('Ran A twice'), 'Evidence for a changed criterion should show both sides.');

    // The findings that requested changes sit beside the earlier submission.
    const [findingsBefore, findingsAfter] = await cells('Review findings');
    assert(findingsBefore.includes('Evidence for A is thin') && findingsBefore.includes('Run criterion A twice') && findingsBefore.includes('Only one run was logged'), `Earlier column lacks the findings: ${findingsBefore}`);
    assert(!findingsAfter.includes('Run criterion A twice'), 'The later column should not repeat the earlier findings.');
    assert((await marker('Review findings')) === 'Changed', 'The findings row should carry a text marker.');

    // Submitted text is inserted as text, never as markup.
    assert(await evaluate("!document.querySelector('dialog.compare-dialog img') && window.injected === undefined"), 'Submission text was interpreted as markup.');
    assert((await cells('Summary'))[0].includes('<img src=x'), 'The summary should appear as literal text.');

    // Keyboard: the selects can be reached and changing one re-renders the comparison.
    await evaluate("(() => { const select = document.querySelector('#workflow-compare-earlier'); select.focus(); select.value = 'sub-second-0002'; select.dispatchEvent(new Event('change', { bubbles: true })); })()");
    await waitPage(`${rowFor('Summary')}?.dataset.status === 'unchanged'`, 'comparison of a submission with itself');
    assert(await evaluate("document.activeElement.id === 'workflow-compare-earlier'"), 'Changing the comparison should keep keyboard focus on the select.');

    // Mobile width: the two sides stack and nothing overflows sideways.
    await send('Emulation.setDeviceMetricsOverride', { width: 390, height: 800, deviceScaleFactor: 1, mobile: true });
    await waitPage("(() => { const dialog = document.querySelector('dialog.compare-dialog[open]'); return dialog.scrollWidth <= dialog.clientWidth; })()", 'no horizontal overflow at mobile width');
    assert(await evaluate("getComputedStyle(document.querySelector('.compare-cells')).gridTemplateColumns.split(' ').length === 1"), 'The sides should stack at mobile width.');
    await send('Emulation.clearDeviceMetricsOverride');
    await closeComparison();

    // One submission: the button explains there is nothing to compare.
    await openComparison('task-single');
    await waitPage("document.querySelector('dialog.compare-dialog[open] .compare-body').textContent.includes('Nothing to compare')", 'single-submission explanation');
    assert(await evaluate(`${dialogRows}.length === 0`), 'A single submission should not render comparison rows.');
    await closeComparison();

    // Paging: the second page of submissions is only requested when the person chooses Load more.
    await openComparison('task-paged');
    await waitPage("document.querySelector('dialog.compare-dialog[open] .compare-body').textContent.includes('Nothing to compare')", 'first page only');
    assert(!historyRequests.some((item) => item.task === 'task-paged' && item.cursor), 'The next page was requested without a click.');
    await evaluate("[...document.querySelectorAll('dialog.compare-dialog[open] button')].find((button) => button.textContent === 'Load more records').click()");
    await waitPage(`${rowFor('Summary')}?.dataset.status === 'changed'`, 'comparison after Load more');
    await closeComparison();

    // The task page itself lists attachments (kind=artifacts); the comparison adds submissions and reviews.
    assert(['submissions', 'reviews'].every((kind) => historyRequests.some((item) => item.kind === kind)), 'The comparison did not request submissions and reviews.');
    for (const item of historyRequests) {
      assert(['submissions', 'reviews', 'artifacts'].includes(item.kind), `Unexpected history kind ${item.kind}.`);
      assert(item.limit > 0 && item.limit <= 50, `History request limit ${item.limit} is not within 1..50.`);
    }
    console.log(`PASS: submission comparison covered the default pair, findings, markers, single submission, paging and mobile layout (${historyRequests.length} bounded history requests).`);
  } finally {
    socket?.close();
    if (chrome.pid && chrome.exitCode === null) { const exited = once(chrome, 'exit'); chrome.kill('SIGTERM'); await exited; }
    await new Promise((done) => server.close(done));
    await rm(profile, { recursive: true, force: true });
    if (launchError) throw launchError;
  }
}

main().catch((error) => { console.error(`FAIL: ${error.message}`); process.exitCode = 1; });
