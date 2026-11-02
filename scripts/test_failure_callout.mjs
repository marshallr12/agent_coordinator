// Browser test of the task page's failed-check callout (book/src/docs/web-ui-refresh.md).
//
//   CHROME_BIN=/path/to/chrome [CHROME_ARGS="--no-sandbox"] node scripts/test_failure_callout.mjs
//
// A fixture server stands in for the coordinator and serves one task per case: a current failure, a
// failure superseded by a later pass, a failure of an older candidate revision, a clean candidate, and
// a candidate whose newest failure belongs to an integration activity. The test requires the callout
// where a failure is current and its absence everywhere else, that the evidence button focuses the job
// entry, that a narrow window does not overflow, and that no more than one bounded history request is
// made. Only Node's standard library and Chrome are used.
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
const CURRENT = 'a'.repeat(40), OLDER = 'b'.repeat(40);
const failedAt = '2026-10-02T10:00:00Z', laterAt = '2026-10-02T11:00:00Z', newestAt = '2026-10-02T12:00:00Z';
const job = (id, label, revision, state, exit_code, created_at) => ({
  id, producer_id: 'producer-1', label, source_revision: revision, source_tree: 'c'.repeat(40), check_identity: label, check_version: '1', check_environment: 'ci',
  state, exit_code, inputs_unchanged: true, last_observed_at: created_at, created_at, observation_freshness: 'fresh', summary: '',
});
const makeTask = (id, jobs, workflow = {}) => {
  const submission = { id: `submission-${id}`, task_id: id, kind: 'code', summary: 'Candidate summary', candidate_revision: CURRENT, candidate_tree: 'c'.repeat(40), candidate_ref: 'refs/fixture', task_revision: 1, project_policy_revision: 1, acceptance_evidence: [] };
  return {
    id, title: `Fixture ${id}`, description: `Description ${id}`, revision: 1, kind: 'code', lifecycle: 'open', work_status: 'in_review', created_at: '2026-10-01T00:00:00Z', acceptance_criteria: [],
    attempts: [], checkpoints: [], lifecycle_events: [], replaces: [],
    job_evidence: { jobs, reservations: [], job_total: jobs.length },
    workflow: { phase: 'review', blockers: [], next_actions: [], submission, activities: [], ...workflow },
  };
};
const blocked = { kind: 'validation', verdict: 'failed', check_name: 'Unit tests', target_revision: 'd'.repeat(40), report_id: 'report-1' };
const details = new Map([
  // Lint failed first, then Unit tests failed twice: the newest failure is Unit tests with exit 3.
  ['task-current', makeTask('task-current', [
    job('job-lint', 'Lint', CURRENT, 'failed', 1, failedAt), job('job-unit-1', 'Unit tests', CURRENT, 'failed', 2, failedAt),
    job('job-unit-2', 'Unit tests', CURRENT, 'failed', 3, laterAt), job('job-old', 'Docs', OLDER, 'failed', 4, newestAt),
  ], { integration_blocked: blocked })],
  // Unit tests failed and then passed on the same revision; Docs failed only on an older revision.
  ['task-superseded', makeTask('task-superseded', [
    job('job-unit-1', 'Unit tests', CURRENT, 'failed', 2, failedAt), job('job-unit-2', 'Unit tests', CURRENT, 'succeeded', 0, laterAt),
    job('job-old', 'Docs', OLDER, 'failed', 4, newestAt),
  ], { integration_blocked: blocked })],
  ['task-older', makeTask('task-older', [job('job-old', 'Docs', OLDER, 'failed', 4, failedAt), job('job-new', 'Docs', CURRENT, 'succeeded', 0, laterAt)])],
  ['task-clean', makeTask('task-clean', [job('job-unit', 'Unit tests', CURRENT, 'succeeded', 0, failedAt)], { integration_blocked: blocked })],
  // The only failure belongs to an integration activity's task, so it is found through the job history.
  ['task-activity', makeTask('task-activity', [job('job-unit', 'Unit tests', CURRENT, 'succeeded', 0, failedAt)], { activities: [{ id: 'activity-1', kind: 'integration', state: 'open', slot: 1 }] })],
]);
const activityHistory = { items: [
  { relation: 'subject', task_id: 'task-activity', record: job('job-unit', 'Unit tests', CURRENT, 'succeeded', 0, failedAt) },
  { relation: 'workflow_activity', task_id: 'activity-task', record: job('job-validate', 'Integration validation', CURRENT, 'failed', 9, newestAt) },
], next_cursor: null };
const tasks = [...details.values()].map(({ id, title, description, revision, kind, lifecycle, work_status, created_at, acceptance_criteria }) => ({ id, title, description, revision, kind, lifecycle, work_status, created_at, acceptance_criteria }));
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
  const history = rest?.match(/^\/tasks\/([^/]+)\/history$/);
  if (history) {
    historyRequests.push({ task: history[1], kind: url.searchParams.get('kind'), limit: Number(url.searchParams.get('limit')) });
    return json(history[1] === 'task-activity' && url.searchParams.get('kind') === 'jobs' ? activityHistory : { items: [], next_cursor: null });
  }
  const detail = rest?.match(/^\/tasks\/([^/]+)$/);
  if (detail && details.has(detail[1])) return json(details.get(detail[1]));
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
  const profile = await mkdtemp(join(tmpdir(), 'agent-coordinator-failure-callout-'));
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
    await send('Page.enable');

    // Loads one fixture task in a fresh page and waits until its candidate and job evidence are drawn.
    const open = async (id) => {
      await send('Page.navigate', { url: 'about:blank' });
      historyRequests.length = 0;
      await send('Page.navigate', { url: `http://127.0.0.1:${serverPort}/#/projects/p1/task/${id}` });
      await waitPage(`document.querySelector('#task-detail-heading')?.textContent === 'Fixture ${id}' && document.querySelector('#workflow-content .workflow-entry') && document.querySelector('#job-evidence-content .evidence-entry')`, `${id} detail`);
      await sleep(300);
    };
    const callouts = () => evaluate(`[...document.querySelectorAll('#workflow-content [data-failure-callout]')].map((node) => ({ kind: node.dataset.failureCallout, text: node.textContent, error: node.classList.contains('error'), buttons: node.querySelectorAll('button').length }))`);
    const observed = (iso) => evaluate(`new Date(${JSON.stringify(iso)}).toLocaleString([], { dateStyle: 'medium', timeStyle: 'short' })`);

    // Current failure: names the newest failed check, its exit code and time, and the integration report.
    await open('task-current');
    let found = await callouts();
    assert(found.length === 1 && found[0].kind === 'current' && found[0].error, `task-current: expected one current callout, saw ${JSON.stringify(found)}`);
    assert(found[0].text.includes('Unit tests (1, ci)') && !found[0].text.includes('Lint') && !found[0].text.includes('Docs'), `task-current: callout must name Unit tests only: ${found[0].text}`);
    assert(found[0].text.includes('Exit code: 3') && found[0].text.includes(await observed(laterAt)), `task-current: callout needs exit code 3 and its time: ${found[0].text}`);
    assert(found[0].text.includes('Integration blocked') && found[0].text.includes('report-1'), `task-current: callout must carry the integration report: ${found[0].text}`);
    assert(await evaluate("document.querySelectorAll('#workflow-content .inline-alert.error strong').length === 1 && [...document.querySelectorAll('#workflow-content strong')].filter((node) => node.textContent === 'Integration blocked').length === 0"), 'task-current: the integration report must not also appear as a separate box');
    assert(await evaluate("document.querySelector('#workflow-content [data-failure-callout]').nextElementSibling?.classList.contains('workflow-entry')"), 'task-current: the callout must sit directly above the candidate');
    assert(!historyRequests.some((item) => item.kind === 'jobs'), `task-current: no job history request is needed without activities, saw ${JSON.stringify(historyRequests)}`);
    // The button is keyboard reachable and moves focus to the matching job evidence entry.
    assert(await evaluate("(() => { const button = document.querySelector('[data-failure-callout] button'); button.focus(); return document.activeElement === button && button.textContent.trim().length > 0; })()"), 'task-current: the evidence button must be focusable and labelled');
    await evaluate("document.querySelector('[data-failure-callout] button').click()");
    await waitPage("document.activeElement?.dataset?.jobId === 'job-unit-2'", 'focus on the failed job entry');

    // Narrow window: the callout stays inside the viewport and its button stays usable.
    await send('Emulation.setDeviceMetricsOverride', { width: 360, height: 700, deviceScaleFactor: 1, mobile: true });
    await sleep(200);
    assert(await evaluate("(() => { const box = document.querySelector('[data-failure-callout]'), rect = box.getBoundingClientRect(), button = box.querySelector('button').getBoundingClientRect(); return rect.right <= innerWidth && box.scrollWidth <= box.clientWidth + 1 && button.right <= innerWidth && button.width > 40 && document.documentElement.scrollWidth <= innerWidth + 1; })()"), 'task-current: the callout overflows a 360px window');
    await send('Emulation.clearDeviceMetricsOverride');

    // Superseded: the failure is not presented as current, and an older revision's failure is ignored.
    await open('task-superseded');
    found = await callouts();
    assert(found.length === 1 && found[0].kind === 'superseded' && !found[0].error, `task-superseded: expected one non-error superseded note, saw ${JSON.stringify(found)}`);
    assert(found[0].text.includes('superseded') && found[0].text.includes('Unit tests') && !found[0].text.includes('Docs'), `task-superseded: ${found[0].text}`);
    assert(await evaluate("[...document.querySelectorAll('#workflow-content strong')].some((node) => node.textContent === 'Integration blocked')"), 'task-superseded: the standalone integration report must remain when no failure is current');

    // Older revision only: nothing is shown.
    await open('task-older');
    assert((await callouts()).length === 0, 'task-older: a failure of an older candidate revision must not be shown');

    // No failed job: nothing is shown, but the integration report keeps its own box.
    await open('task-clean');
    assert((await callouts()).length === 0, 'task-clean: no callout without a failed job');
    assert(await evaluate("[...document.querySelectorAll('#workflow-content strong')].some((node) => node.textContent === 'Integration blocked')"), 'task-clean: the integration report must still be shown');

    // An activity's failing job is found with exactly one bounded history request.
    await open('task-activity');
    await waitPage("document.querySelector('#workflow-content [data-failure-callout]')", 'activity failure callout');
    found = await callouts();
    assert(found.length === 1 && found[0].kind === 'current' && found[0].text.includes('Integration validation') && found[0].text.includes('Exit code: 9') && found[0].text.includes(await observed(newestAt)), `task-activity: ${JSON.stringify(found)}`);
    const jobPages = historyRequests.filter((item) => item.kind === 'jobs');
    assert(jobPages.length === 1 && jobPages[0].limit > 0 && jobPages[0].limit <= 50, `task-activity: expected one job history request with limit <= 50, saw ${JSON.stringify(historyRequests)}`);
    // That job has no entry on this page, so the button opens the job history instead.
    await evaluate("document.querySelector('[data-failure-callout] button').click()");
    await waitPage("document.querySelector('dialog[open] select[name=kind]')?.value === 'jobs'", 'job history dialog');
    console.log('PASS: the failure callout names the newest current failure, hides superseded, older-revision and absent failures, and costs at most one bounded request.');
  } finally {
    socket?.close();
    if (chrome.pid && chrome.exitCode === null) { const exited = once(chrome, 'exit'); chrome.kill('SIGTERM'); await exited; }
    await new Promise((done) => server.close(done));
    await rm(profile, { recursive: true, force: true });
    if (launchError) throw launchError;
  }
}

main().catch((error) => { console.error(`FAIL: ${error.message}`); process.exitCode = 1; });
