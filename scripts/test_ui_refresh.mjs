// Browser test of the dashboard's refresh behaviour (book/src/docs/web-ui-refresh.md).
//
//   CHROME_BIN=/path/to/chrome [CHROME_ARGS="--no-sandbox"] node scripts/test_ui_refresh.mjs
//
// A fixture server stands in for the coordinator and serves every view's data from mutable objects.
// For each view the test installs a MutationObserver on the view's root, makes the dashboard refresh
// (refresh button, the 5 s poll, navigating away and back, or a finished mutation) while the data is
// unchanged, and requires that the observer saw nothing. It then changes one item and requires that
// only that item's node was replaced. It also checks that focus, text selection, open <details>,
// scroll position and an open dialog survive, and that polling pauses while the tab is hidden, a
// dialog is open or a mutation is pending. Only Node's standard library and Chrome are used.
import { createServer } from 'node:http';
import { mkdtemp, readFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { spawn } from 'node:child_process';
import { once } from 'node:events';
import net from 'node:net';

const root = resolve(import.meta.dirname, '..');
const clone = (value) => JSON.parse(JSON.stringify(value));
const assert = (condition, message) => { if (!condition) throw new Error(message); };
const sleep = (ms) => new Promise((done) => setTimeout(done, ms));

// ---- Fixture data: one mutable object per view -------------------------------------------------
const actor = { id: 'op-1', name: 'Operator', role: 'admin', kind: 'human', session_id: 'browser-1' };
const projects = [
  { id: 'p1', name: 'Alpha', repository_url: 'https://example.invalid/alpha.git', target_branch: 'main' },
  { id: 'p2', name: 'Beta', repository_url: 'https://example.invalid/beta.git', target_branch: 'trunk' },
];
const policy = { id: 'p1', name: 'Alpha', policy_revision: 3, review_mode: 'either', recovery_mode: 'agent', lease_seconds: 600, rules: 'Be careful.', agent_rule_editing: false, automatic_integration: false, allow_subagent_reviews: false, integration_owner: 'agent' };
const makeTask = (index, workStatus = 'ready') => ({ id: `task-${index}`, title: `Fixture task ${index}`, description: `Description ${index}`, revision: 1, kind: 'code', lifecycle: workStatus === 'done' ? 'done' : 'open', work_status: workStatus, created_at: '2026-10-01T00:00:00Z', acceptance_criteria: [] });
const tasks = [...Array.from({ length: 40 }, (_, index) => makeTask(index)), makeTask(100, 'done'), makeTask(101, 'done'), makeTask(102, 'done')];
const archived = [{ ...makeTask(200), lifecycle: 'canceled', archived_at: '2026-10-02T00:00:00Z' }, { ...makeTask(201), lifecycle: 'canceled', archived_at: '2026-10-02T00:00:00Z' }, { ...makeTask(202), lifecycle: 'canceled', archived_at: '2026-10-02T00:00:00Z' }];
const detail = {
  id: 'task-1', title: 'Detail fixture', description: 'Detail description.', revision: 4, kind: 'code', lifecycle: 'open', work_status: 'in_progress', current_attempt_id: 'attempt-1', human_delivered: false,
  acceptance_criteria: ['First criterion', 'Second criterion'],
  attempts: [{ id: 'attempt-1', owner_id: 'agent-1', owner_name: 'Agent One', session_id: 'agent-session', state: 'active', mode: 'work', generation: 1, lease_remaining_ms: 900000, expires_at: '2026-10-13T12:15:00Z' }],
  checkpoints: [
    { id: 'cp-1', attempt_id: 'attempt-1', summary: 'Started', current_action: 'Reading', next_step: 'Edit', blockers: [], created_at: '2026-10-13T10:00:00Z' },
    { id: 'cp-2', attempt_id: 'attempt-1', summary: 'Edited', current_action: 'Testing', next_step: 'Submit', blockers: [], created_at: '2026-10-13T11:00:00Z' },
  ],
  workflow: {
    phase: 'review', blockers: [], integration_blocked: { report_id: 'rep-1', kind: 'check_failed', verdict: 'failed', check_name: 'Coordination checks', target_revision: 'def456' }, next_actions: ['A reviewer must decide.'],
    submission: { id: 'sub-1', task_id: 'task-1', kind: 'code', summary: 'Submitted work', candidate_revision: 'abc123', candidate_ref: 'refs/x', task_revision: 4, project_policy_revision: 3, acceptance_evidence: [{ criterion: 'First criterion', evidence: 'Seen' }], lessons: [{ id: 'lesson-1', body: 'A lesson' }] },
    activities: [{ id: 'act-1', kind: 'human_review', status: 'pending' }, { id: 'act-2', kind: 'agent_review', status: 'pending' }],
  },
  job_evidence: {
    jobs: [{ id: 'job-1', label: 'Gate', state: 'running', observation_freshness: 'fresh', last_observed_at: '2026-10-13T11:59:00Z', producer_id: 'prod-1' }, { id: 'job-2', label: 'Lint', state: 'succeeded', observation_freshness: 'fresh', last_observed_at: '2026-10-13T11:59:00Z', exit_code: 0, producer_id: 'prod-1' }],
    reservations: [{ id: 'res-held', state: 'held', items: [{ key: 'db', units: 1 }] }],
  },
  lifecycle_events: [{ id: 'ev-1', action: 'restored', actor_name: 'Operator', created_at: '2026-10-12T10:00:00Z', reason: 'Testing' }], replaces: [],
};
const attachments = [{ id: 'art-1', display_name: 'notes.txt', media_type: 'text/plain', size_bytes: 12, availability: 'available', kind: 'upload' }];
const credentials = [
  { id: 'cred-1', principal_id: 'pr-1', principal_name: 'agent-a', credential_name: 'initial', created_at: '2026-10-01T00:00:00Z', class: 'supervised' },
  { id: 'cred-2', principal_id: 'pr-2', principal_name: 'agent-b', credential_name: 'initial', created_at: '2026-10-01T00:00:00Z' },
  { id: 'cred-3', principal_id: 'pr-3', principal_name: 'agent-c', credential_name: 'initial', created_at: '2026-10-01T00:00:00Z', revoked_at: '2026-10-05T00:00:00Z' },
];
const operators = [{ id: 'op-1', name: 'operator-one', role: 'admin', enabled: true, revision: 1 }, { id: 'op-2', name: 'operator-two', role: 'operator', enabled: true, revision: 1 }];
const browserSessions = [{ id: 'bs-1', current: true, created_at: '2026-10-10T00:00:00Z', expires_at: '2026-10-20T00:00:00Z' }, { id: 'bs-2', created_at: '2026-10-11T00:00:00Z', expires_at: '2026-10-21T00:00:00Z' }, { id: 'bs-3', created_at: '2026-10-12T00:00:00Z', expires_at: '2026-10-22T00:00:00Z' }];
const clock = { clock_state: { status: 'ready' } };
const restore = {
  service_state: { coordination_state: 'restore_reconciliation', restore_id: 'restore-1' },
  restore: { inspected: 0, required_inspections: 2, old_installation_fenced: false, post_snapshot_gap_reconciled: false },
  items: [{ kind: 'job', target_id: 'hold-1', inspected: false, project_id: 'p1', detail: { task_id: 'task-1' } }, { kind: 'resource', target_id: 'hold-2', inspected: false, project_id: 'p1', detail: {} }],
  next_cursor: null,
};
const resources = [{ id: 'resource-1', key: 'fixture:db', capacity: 2, held_units: 0, description: 'Database' }, { id: 'resource-2', key: 'fixture:device', capacity: 1, held_units: 1, description: 'Device' }];
const reports = [
  { id: 'report-1', kind: 'flaky', requires_human: false, created_at: '2026-10-06T00:00:00Z', resolved_at: null, details: { reason: 'one' } },
  { id: 'report-2', kind: 'fix_target', requires_human: true, created_at: '2026-10-07T00:00:00Z', resolved_at: null, details: { reason: 'two' } },
  { id: 'report-3', kind: 'flaky', requires_human: false, created_at: '2026-10-08T00:00:00Z', resolved_at: null, details: { reason: 'three' } },
];
const autonomy = { window_days: 14, finished: { count: 2, by_day: [{ day: '2026-10-12', count: 2 }] }, human_required: { count: 1, per_finished_task: 0.5, by_day: [{ day: '2026-10-12', count: 1 }], integrator_reports: 1, parked_subjects: 0 }, parked: { count: 1, items: [{ task_id: 'task-1', title: 'Parked task', agent_revises_24h: 3, counted_revises_24h: 3 }] }, reviews: { submissions_reviewed: 2, decisions: 2, approved_first_pass: 1, revised: 1, disagreements: 0 }, caps: [{ host: 'h1', impl_daily_usd: 150, rev_daily_usd: 50, reported_at: '2026-10-12T00:00:00Z' }] };
const costByDay = { items: [{ day: '2026-10-12', impl: { usd: 10, launches: 1 }, rev: { usd: 2, launches: 1 }, total_usd: 12 }], totals: { impl: {}, rev: {} }, truncated: false };
const costByTask = { items: [{ task_id: 'task-1', title: 'Parked task', impl: { usd: 10 }, rev: { usd: 2 }, total_usd: 12 }], totals: { impl: {}, rev: {} }, truncated: false };
const canaries = { items: [{ id: 'c1', host: 'h1', harness: 'claude', outcome: 'ok', duration_seconds: 400, task_id: null, occurred_at: '2026-10-12T00:00:00Z' }], latest: [{ host: 'h1', harness: 'claude', outcome: 'ok', occurred_at: '2026-10-12T00:00:00Z' }], next_before: null };
const digest = { window_hours: 24, last_read_at: '2026-10-12T00:00:00Z', hri: { items: [{ code: 'hri-1', title: 'Needs a person' }] }, proceeded_decisions: [{ decision_id: 'd-1', question: 'Q1?', proceeded_with: 'yes', proceeded_at: '2026-10-12T10:00:00Z' }], pending_reversible_decisions: [] };
const sessions = [
  { session_id: 'sess-1', principal: { id: 'pr-1', name: 'agent-a' }, harness: 'claude-code', workstation_id: 'ws-1', started_at: '2026-10-13T08:00:00Z', last_activity_at: '2026-10-13T11:00:00Z', held_attempts: [{ attempt_id: 'attempt-1', task_id: 'task-1', task_title: 'Detail fixture', mode: 'work', expires_at: '2026-10-13T12:15:00Z' }] },
  { session_id: 'sess-2', principal: { id: 'pr-2', name: 'agent-b' }, harness: 'codex', workstation_id: 'ws-2', started_at: '2026-10-13T08:00:00Z', last_activity_at: '2026-10-13T10:00:00Z', held_attempts: [] },
];
const lessons = [
  { id: 'lesson-a', kind: 'lesson', title: 'Lesson A', body: 'Body A', status: 'observed', revision: 1, project_id: 'p1' },
  { id: 'lesson-b', kind: 'lesson', title: 'Lesson B', body: 'Body B', status: 'observed', revision: 1, project_id: 'p1' },
  { id: 'lesson-c', kind: 'lesson', title: 'Lesson C', body: 'Body C', status: 'observed', revision: 1, project_id: 'p1' },
];
const contextRules = { text: 'Binding rules text.' };

// ---- Fixture server -------------------------------------------------------------------------------
const hits = new Map();
const hitCount = (needle) => [...hits].filter(([key]) => key.includes(needle)).reduce((sum, [, count]) => sum + count, 0);
let mutationDelay = 0;
const posts = [];

function json(response, data, status = 200) {
  response.writeHead(status, { 'Content-Type': 'application/json', 'Cache-Control': 'no-store' });
  response.end(JSON.stringify({ data }));
}
async function readJson(request) {
  let body = '';
  for await (const chunk of request) body += chunk;
  return body ? JSON.parse(body) : {};
}

async function route(request, response) {
  const url = new URL(request.url, 'http://fixture.invalid');
  const path = url.pathname;
  const key = `${request.method} ${path}`;
  hits.set(key, (hits.get(key) || 0) + 1);
  const files = { '/': ['web/index.html', 'text/html; charset=utf-8'], '/app.js': ['web/app.js', 'text/javascript; charset=utf-8'], '/style.css': ['web/style.css', 'text/css; charset=utf-8'] };
  if (files[path]) {
    response.writeHead(200, { 'Content-Type': files[path][1], 'Cache-Control': 'no-store' });
    return response.end(await readFile(join(root, files[path][0])));
  }
  if (path === '/api/v1/me') return json(response, { actor, csrf_token: 'csrf' });
  if (path === '/api/v1/projects') return json(response, { items: projects });
  if (request.method === 'POST') {
    const body = await readJson(request);
    posts.push({ path, body });
    if (mutationDelay) await sleep(mutationDelay);
    if (path.endsWith('/human-delivery')) { detail.human_delivered = body.human_delivered; detail.revision += 1; return json(response, detail); }
    const revoke = path.match(/browser-sessions\/([^/]+)\/revoke$/);
    if (revoke) { const found = browserSessions.find((item) => item.id === revoke[1]); found.revoked_at = '2026-10-13T12:00:00Z'; return json(response, found); }
    return json(response, {});
  }
  const project = path.match(/^\/api\/v1\/projects\/p1(\/.*)?$/);
  if (project) {
    const rest = project[1] || '';
    if (rest === '/orientation') return json(response, { project: policy, policy_revision: policy.policy_revision });
    if (rest === '/tasks') {
      const queueOnly = url.searchParams.get('exclude_done') === 'true';
      return json(response, { items: tasks.filter((task) => !queueOnly || task.work_status !== 'done'), next_cursor: null });
    }
    if (rest === '/tasks/archived') return json(response, { items: archived, next_cursor: null });
    if (rest === `/tasks/${detail.id}`) return json(response, detail);
    if (rest === `/tasks/${detail.id}/history`) return json(response, { items: attachments.map((record) => ({ record })), next_cursor: null });
    if (rest === '/integrator/reports') { const open = url.searchParams.get('open') === 'true'; return json(response, { items: [...reports].reverse().filter((report) => !open || !report.resolved_at), next_before: null }); }
    if (rest === '/telemetry/autonomy') return json(response, autonomy);
    if (rest === '/telemetry/cost') return json(response, url.searchParams.get('group_by') === 'day' ? costByDay : costByTask);
    if (rest === '/telemetry/canaries') return json(response, canaries);
    if (rest === '/digest') return json(response, digest);
    if (rest === '/sessions') return json(response, { items: sessions, truncated: false });
    if (rest === '/knowledge') return json(response, { items: lessons, next_cursor: null });
    if (rest === '/decisions') return json(response, { items: [], next_cursor: null });
    if (rest === '/artifacts') return json(response, { items: [], next_cursor: null });
    if (rest === '/context') return json(response, { items: [], policy: { rules: contextRules.text } });
    if (rest === '/objectives') return json(response, { items: [], next_cursor: null });
  }
  if (path === '/api/v1/admin/credentials') return json(response, { items: credentials });
  if (path === '/api/v1/admin/operators') return json(response, { items: operators, next_cursor: null });
  if (path === '/api/v1/admin/clock') return json(response, clock);
  if (path === '/api/v1/admin/restore') return json(response, restore);
  if (path === '/api/v1/resources') return json(response, { items: resources, next_cursor: null });
  if (path === '/api/v1/browser-sessions') return json(response, { items: browserSessions, next_cursor: null });
  response.writeHead(404); response.end();
}

// ---- Chrome driver --------------------------------------------------------------------------------
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

// Runs before the page's own scripts: a frozen clock (so minute-resolution labels cannot roll over
// mid-test) and a 5 s interval that only runs when the test calls __tick(), like a poll that fires on demand.
const PAGE_SETUP = `
  (() => {
    const RealDate = Date, fixed = new RealDate('2026-10-13T12:00:00Z').getTime();
    class FrozenDate extends RealDate {
      constructor(...args) { if (args.length) super(...args); else super(fixed); }
      static now() { return fixed; }
    }
    window.Date = FrozenDate;
    const realSet = window.setInterval.bind(window), realClear = window.clearInterval.bind(window);
    let next = 1000000; window.__polls = new Map();
    window.setInterval = (callback, ms, ...args) => { if (ms === 5000) { const id = ++next; window.__polls.set(id, callback); return id; } return realSet(callback, ms, ...args); };
    window.clearInterval = (id) => { if (window.__polls.delete(id)) return; realClear(id); };
    window.__tick = () => { for (const callback of [...window.__polls.values()]) callback(); };
    const describe = (node) => node.nodeType === 1 ? node.nodeName.toLowerCase() + (node.id ? '#' + node.id : '') + (typeof node.className === 'string' && node.className ? '.' + node.className.split(' ')[0] : '') : node.nodeName;
    window.__observe = (selector) => {
      const target = document.querySelector(selector);
      window.__observer?.disconnect();
      window.__records = [];
      window.__observer = new MutationObserver((list) => { window.__records.push(...list.filter((record) => !(record.target === target && record.type === 'attributes' && record.attributeName === 'hidden'))); });
      window.__observer.observe(target, { subtree: true, childList: true, attributes: true, characterData: true });
    };
    window.__drain = () => {
      window.__records.push(...window.__observer.takeRecords());
      const seen = window.__records.map((record) => record.type + ':' + (record.attributeName || '') + ' on ' + describe(record.target) + (record.addedNodes.length ? ' +' + record.addedNodes.length : '') + (record.removedNodes.length ? ' -' + record.removedNodes.length : ''));
      window.__records = []; return seen;
    };
    window.__tag = (selector, name = 'kept') => document.querySelectorAll(selector).forEach((node, index) => { node['__' + name] = index + 1; });
    window.__tags = (selector, name = 'kept') => [...document.querySelectorAll(selector)].map((node) => node['__' + name] || 0);
  })();
`;

async function main() {
  const server = createServer((request, response) => route(request, response).catch((error) => { response.writeHead(500); response.end(String(error)); }));
  server.listen(0, '127.0.0.1');
  await once(server, 'listening');
  const serverPort = server.address().port;
  const debugPort = await unusedPort();
  const profile = await mkdtemp(join(tmpdir(), 'agent-coordinator-refresh-test-'));
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

    // ---- Helpers over the page ---------------------------------------------------------------------
    const click = (selector) => evaluate(`document.querySelector(${JSON.stringify(selector)}).click()`);
    const visible = (view) => evaluate(`!document.querySelector('#${view}-view').hidden`);
    const goTo = async (selector, view) => { await click(selector); await waitPage(`!document.querySelector('#${view}-view').hidden`, `${view} view`); };
    const settle = () => sleep(250);
    const observe = (selector) => evaluate(`window.__observe(${JSON.stringify(selector)})`);
    const drain = () => evaluate('window.__drain()');
    const tick = () => evaluate('window.__tick()');
    // Runs `trigger` while the view is observed and requires that nothing in it changed.
    const quiet = async (label, view, trigger) => {
      await observe(`#${view}-view`);
      await trigger();
      await settle();
      const seen = await drain();
      assert(seen.length === 0, `${label}: an unchanged refresh mutated the DOM: ${seen.slice(0, 6).join('; ')}`);
    };
    // Changes one item, refreshes, and requires that only nodes matching `changed` were replaced.
    const onlyChanged = async (label, selector, change, trigger, expectedNew = 1) => {
      await evaluate(`window.__tag(${JSON.stringify(selector)})`);
      const before = (await evaluate(`window.__tags(${JSON.stringify(selector)})`)).length;
      change();
      await trigger();
      await settle();
      const tags = await evaluate(`window.__tags(${JSON.stringify(selector)})`);
      const fresh = tags.filter((tag) => tag === 0).length;
      assert(fresh === expectedNew, `${label}: expected ${expectedNew} rebuilt node(s) among ${tags.length} (was ${before}), saw ${fresh}: ${JSON.stringify(tags)}`);
    };
    const pollTick = (needle) => async () => { const seen = hitCount(needle); await tick(); const deadline = Date.now() + 5000; while (hitCount(needle) === seen && Date.now() < deadline) await sleep(30); };
    const clickAndWait = (selector, needle) => async () => { const seen = hitCount(needle); await click(selector); const deadline = Date.now() + 5000; while (hitCount(needle) === seen && Date.now() < deadline) await sleep(30); };

    // ---- 1. Overview (projects) -------------------------------------------------------------------
    await waitPage("document.querySelector('.autonomy-summary-text')?.textContent.startsWith('HRI')", 'project card autonomy summary');
    await quiet('overview refresh button', 'overview', clickAndWait('#refresh-projects', 'GET /api/v1/projects'));
    await quiet('overview poll', 'overview', pollTick('GET /api/v1/projects'));
    await onlyChanged('overview project change', '#projects-grid .project-card', () => { projects[1] = { ...projects[1], name: 'Beta renamed' }; }, pollTick('GET /api/v1/projects'));
    assert(await evaluate("document.querySelectorAll('#projects-grid .project-card')[1].textContent.includes('Beta renamed')"), 'The changed project card did not show the new name.');
    projects.push({ id: 'p3', name: 'Gamma', repository_url: 'https://example.invalid/gamma.git', target_branch: 'main' });
    await onlyChanged('overview project insert', '#projects-grid .project-card', () => {}, pollTick('GET /api/v1/projects'), 1);
    projects.pop();
    await onlyChanged('overview project removal', '#projects-grid .project-card', () => {}, pollTick('GET /api/v1/projects'), 0);
    assert(await evaluate("document.querySelectorAll('#projects-grid .project-card').length") === 2, 'Removing a project left its card behind.');
    // Focus and selection on a project card survive an unchanged poll.
    await evaluate("document.querySelector('.project-open').focus()");
    await evaluate(`(() => { const title = document.querySelector('.project-card h3'); const range = document.createRange(); range.selectNodeContents(title); getSelection().removeAllRanges(); getSelection().addRange(range); document.querySelector('.project-open').focus(); })()`);
    await pollTick('GET /api/v1/projects')(); await settle();
    assert(await evaluate("document.activeElement.classList.contains('project-open')"), 'A poll took focus from the project button.');
    assert(await evaluate("getSelection().toString()") === 'Alpha', 'A poll cleared the text selection on the project card.');
    // The focused button's own card changes: the rebuilt node regains focus.
    await evaluate("window.__tag('#projects-grid .project-card')");
    projects[0] = { ...projects[0], target_branch: 'release' };
    await pollTick('GET /api/v1/projects')(); await settle();
    assert(await evaluate("document.activeElement.classList.contains('project-open') && document.activeElement.closest('.project-card').textContent.includes('release')"), 'Focus was lost when the focused card was rebuilt.');
    await evaluate("getSelection().removeAllRanges(); document.activeElement.blur()");

    // ---- 2. Project settings ----------------------------------------------------------------------
    await goTo('.project-settings-button', 'project');
    await waitPage("document.querySelector('#integration-owner-summary').textContent.includes('Current owner')", 'integration owner line');
    await quiet('project settings re-entry', 'project', async () => { await click('#back-to-projects'); await waitPage("!document.querySelector('#overview-view').hidden", 'overview'); await click('.project-settings-button'); await waitPage("!document.querySelector('#project-view').hidden", 'project view'); await sleep(100); });

    // ---- 3. Reports (project settings → integrator reports) ---------------------------------------
    await goTo('#integrator-reports-button', 'reports');
    await waitPage("document.querySelectorAll('#reports-list .report-card').length === 3", 'reports');
    await quiet('reports refresh button', 'reports', clickAndWait('#refresh-reports', 'integrator/reports'));
    await evaluate("document.querySelector('[data-report-id=report-2] details').open = true; document.querySelector('[data-report-id=report-2] button').focus()");
    await quiet('reports with open details and focus', 'reports', clickAndWait('#refresh-reports', 'integrator/reports'));
    assert(await evaluate("document.querySelector('[data-report-id=report-2] details').open && document.activeElement.closest('[data-report-id=report-2]') !== null"), 'Open details or focus was lost on an unchanged reports refresh.');
    await onlyChanged('reports one changed (a sibling)', '#reports-list .report-card', () => { reports[0] = { ...reports[0], requires_human: true }; }, clickAndWait('#refresh-reports', 'integrator/reports'));
    assert(await evaluate("document.querySelector('[data-report-id=report-2] details').open"), 'A change to another report closed this report’s details.');
    await onlyChanged('reports the open one changed', '#reports-list .report-card', () => { reports[1] = { ...reports[1], details: { reason: 'two, updated' } }; }, clickAndWait('#refresh-reports', 'integrator/reports'));
    assert(await evaluate("document.querySelector('[data-report-id=report-2] details').open && document.querySelector('[data-report-id=report-2] pre').textContent.includes('updated')"), 'A changed report lost its open details or kept stale content.');
    assert(await evaluate("document.activeElement.closest('[data-report-id=report-2]') !== null && document.activeElement.tagName === 'BUTTON'"), 'Focus was lost when the focused report was rebuilt.');
    await onlyChanged('reports resolved (open filter)', '#reports-list .report-card', () => { reports[2] = { ...reports[2], resolved_at: '2026-10-13T00:00:00Z' }; }, clickAndWait('#refresh-reports', 'integrator/reports'), 0);
    assert(await evaluate("document.querySelectorAll('#reports-list .report-card').length") === 2, 'A resolved report stayed in the open list.');
    reports[2] = { ...reports[2], resolved_at: null };

    // ---- 4. Attention digest ----------------------------------------------------------------------
    await click('#back-to-project-settings'); await waitPage("!document.querySelector('#project-view').hidden", 'project view');
    await goTo('#digest-button', 'digest');
    await waitPage("document.querySelectorAll('#digest-content .report-card').length === 3", 'digest');
    await quiet('digest refresh button', 'digest', clickAndWait('#refresh-digest', '/digest'));
    await onlyChanged('digest one section changed', '#digest-content .report-card', () => { digest.pending_reversible_decisions = [{ decision_id: 'd-2', question: 'Q2?', recommendation: 'go', proceeds_at: '2026-10-14T00:00:00Z' }]; }, clickAndWait('#refresh-digest', '/digest'));

    // ---- 4b. Autonomy -----------------------------------------------------------------------------
    await click('#digest-back'); await waitPage("!document.querySelector('#project-view').hidden", 'project view');
    await goTo('#autonomy-button', 'autonomy');
    await waitPage("document.querySelectorAll('#autonomy-content .autonomy-card').length === 6", 'autonomy');
    await quiet('autonomy refresh button', 'autonomy', clickAndWait('#refresh-autonomy', 'telemetry/autonomy'));
    await onlyChanged('autonomy one section changed', '#autonomy-content .autonomy-card', () => { autonomy.reviews = { ...autonomy.reviews, disagreements: 1 }; }, clickAndWait('#refresh-autonomy', 'telemetry/autonomy'));
    await click('#autonomy-back'); await waitPage("!document.querySelector('#project-view').hidden", 'project view');

    // ---- 5. Connected sessions --------------------------------------------------------------------
    await goTo('#agent-sessions-button', 'agent-sessions');
    await waitPage("document.querySelectorAll('#agent-sessions-list .agent-session-card').length === 2", 'sessions');
    await quiet('sessions refresh button', 'agent-sessions', clickAndWait('#refresh-agent-sessions', '/sessions'));
    await quiet('sessions window select (same data)', 'agent-sessions', async () => { await evaluate("(() => { const select = document.querySelector('#agent-sessions-window'); select.value = '0'; select.dispatchEvent(new Event('change', { bubbles: true })); })()"); await sleep(100); });
    await onlyChanged('sessions one changed', '#agent-sessions-list .agent-session-card', () => { sessions[1] = { ...sessions[1], last_activity_at: '2026-10-13T11:30:00Z' }; }, clickAndWait('#refresh-agent-sessions', '/sessions'));
    await click('#agent-sessions-back'); await waitPage("!document.querySelector('#project-view').hidden", 'project view');

    // ---- 6. Task queue ----------------------------------------------------------------------------
    await click('#back-to-projects'); await waitPage("!document.querySelector('#overview-view').hidden", 'overview');
    await click('.project-open');
    await waitPage("document.querySelectorAll('#tasks-list .task-row').length === 25", 'task queue');
    await evaluate("(() => { const box = document.querySelector('#tasks-auto-refresh'); box.checked = true; box.dispatchEvent(new Event('change', { bubbles: true })); })()");
    await quiet('task queue refresh button', 'tasks', clickAndWait('#refresh-tasks', '/p1/tasks?'));
    await quiet('task queue poll', 'tasks', pollTick('GET /api/v1/projects/p1/tasks'));
    await quiet('task queue navigate away and back', 'tasks', async () => { await click('.nav-item[data-view=overview]'); await waitPage("!document.querySelector('#overview-view').hidden", 'overview'); await click('.nav-item[data-view=tasks]'); await waitPage("!document.querySelector('#tasks-view').hidden", 'tasks'); await sleep(150); });
    await onlyChanged('task queue one task changed', '#tasks-list .task-row', () => { tasks[3] = { ...tasks[3], description: 'A changed description', revision: 2 }; }, pollTick('GET /api/v1/projects/p1/tasks'));
    await onlyChanged('task queue one task inserted', '#tasks-list .task-row', () => { tasks.splice(5, 0, makeTask(500)); }, pollTick('GET /api/v1/projects/p1/tasks'), 1 + 0);
    tasks.splice(5, 1);
    // A change that shifts the paging rebuilds only the rows that enter the page.
    await evaluate("window.__tag('#tasks-list .task-row')");
    await pollTick('GET /api/v1/projects/p1/tasks')(); await settle();
    assert(await evaluate("document.querySelectorAll('#tasks-list .task-row').length") === 25, 'Page size changed after removing a task.');
    // Focus, selection and scroll survive a refresh that changes another row.
    await evaluate("document.querySelector('#tasks-list .task-row:nth-child(10)').focus()");
    await evaluate(`(() => { const title = document.querySelector('#tasks-list .task-row:nth-child(8) .task-title'); const range = document.createRange(); range.selectNodeContents(title); getSelection().removeAllRanges(); getSelection().addRange(range); document.querySelector('#tasks-list .task-row:nth-child(10)').focus(); })()`);
    await evaluate("window.scrollTo(0, 400)"); await sleep(100);
    const scrolled = await evaluate('window.scrollY');
    assert(scrolled > 0, 'The task list did not scroll in this viewport.');
    tasks[2] = { ...tasks[2], description: 'Changed again' };
    await pollTick('GET /api/v1/projects/p1/tasks')(); await settle();
    assert(await evaluate("document.activeElement === document.querySelector('#tasks-list .task-row:nth-child(10)')"), 'Focus left the focused task row on a refresh.');
    assert(await evaluate('getSelection().toString()') === 'Fixture task 7', 'The text selection in a task row was lost.');
    assert(await evaluate('window.scrollY') === scrolled, 'The scroll position moved on a refresh.');
    // The focused row itself changes: it is rebuilt, focused again, and its selection restored.
    tasks[9] = { ...tasks[9], description: 'The focused row changed' };
    await pollTick('GET /api/v1/projects/p1/tasks')(); await settle();
    assert(await evaluate("document.activeElement === document.querySelector('#tasks-list .task-row:nth-child(10)') && document.activeElement.textContent.includes('The focused row changed')"), 'Focus was not restored to the rebuilt task row.');
    assert(await evaluate('window.scrollY') === scrolled, 'The scroll position moved when a row was rebuilt.');
    await evaluate("getSelection().removeAllRanges(); document.activeElement.blur(); window.scrollTo(0, 0)");
    // The search box sits outside the list, so focus in it neither blocks the poll nor is disturbed by it.
    await evaluate("document.querySelector('#tasks-search').focus()");
    tasks[4] = { ...tasks[4], description: 'Changed while searching' };
    await pollTick('GET /api/v1/projects/p1/tasks')(); await settle();
    assert(await evaluate("document.activeElement.id === 'tasks-search' && document.querySelectorAll('#tasks-list .task-row')[4].textContent.includes('Changed while searching')"), 'A poll was blocked by, or stole focus from, the search box.');
    await evaluate("document.querySelector('#tasks-search').blur()");

    // ---- 7. Completed tasks -----------------------------------------------------------------------
    await click('#completed-tasks-tab'); await waitPage("document.querySelectorAll('#tasks-list .completed-task-row').length === 3", 'completed tasks');
    await quiet('completed tasks refresh button', 'tasks', clickAndWait('#refresh-tasks', '/p1/tasks?'));
    await quiet('completed tasks poll', 'tasks', pollTick('GET /api/v1/projects/p1/tasks'));
    await onlyChanged('completed tasks one changed', '#tasks-list .task-row', () => { tasks[41] = { ...tasks[41], title: 'Completed and renamed' }; }, pollTick('GET /api/v1/projects/p1/tasks'));
    await click('#task-queue-tab'); await waitPage("document.querySelectorAll('#tasks-list .completed-task-row').length === 0 && document.querySelectorAll('#tasks-list .task-row').length === 25", 'queue again');

    // ---- 8. Archived tasks ------------------------------------------------------------------------
    await click('#archived-tasks-tab'); await waitPage("document.querySelectorAll('#archived-list .task-row').length === 3", 'archived tasks');
    await quiet('archived refresh button', 'archived', clickAndWait('#refresh-archive', 'tasks/archived'));
    await quiet('archived re-entry', 'archived', async () => { await click('#task-queue-tab'); await waitPage("!document.querySelector('#tasks-view').hidden", 'queue'); await click('#archived-tasks-tab'); await waitPage("document.querySelectorAll('#archived-list .task-row').length === 3", 'archived again'); await sleep(150); });
    await onlyChanged('archived one changed', '#archived-list .task-row', () => { archived[1] = { ...archived[1], description: 'Archived and edited' }; }, clickAndWait('#refresh-archive', 'tasks/archived'));
    await click('#task-queue-tab'); await waitPage("!document.querySelector('#tasks-view').hidden", 'queue');

    // ---- 9. Task detail ---------------------------------------------------------------------------
    await evaluate("document.querySelectorAll('#tasks-list .task-row')[1].click()");
    await waitPage("document.querySelector('#task-detail-heading').textContent === 'Detail fixture' && document.querySelector('#workflow-content .workflow-entry')", 'task detail');
    // The fixture serves one detail record for whichever task is opened.
    await waitPage("document.querySelector('#task-attachments-list .task-attachment-entry')", 'attachments');
    await quiet('task detail poll', 'task-detail', pollTick('GET /api/v1/projects/p1/tasks/task-1'));
    await quiet('task detail poll again', 'task-detail', pollTick('GET /api/v1/projects/p1/tasks/task-1'));
    // Sections patch independently.
    const sections = ['#checkpoints-content .checkpoint', '#job-evidence-content .evidence-entry', '#workflow-content .workflow-entry', '#acceptance-list li', '#task-operator-actions button'];
    await onlyChanged('task detail new checkpoint', '#checkpoints-content .checkpoint', () => { detail.checkpoints.push({ id: 'cp-3', attempt_id: 'attempt-1', summary: 'Third', current_action: 'x', next_step: 'y', blockers: [], created_at: '2026-10-13T11:30:00Z' }); }, pollTick('GET /api/v1/projects/p1/tasks/task-1'));
    assert(await evaluate("document.querySelectorAll('#checkpoints-content .checkpoint').length") === 3, 'The new checkpoint was not shown.');
    assert(await evaluate("document.querySelector('#checkpoints-content .checkpoint .checkpoint-summary').textContent") === 'Third', 'The newest checkpoint is not first.');
    for (const selector of sections.filter((item) => !item.includes('checkpoint'))) {
      const tags = await evaluate(`window.__tag(${JSON.stringify(selector)}), window.__tags(${JSON.stringify(selector)})`);
      assert(tags.length > 0, `No nodes for ${selector}`);
    }
    assert(await evaluate("document.querySelector('#workflow-content').textContent.includes('Integration blocked') && document.querySelector('#workflow-content').textContent.includes('Integrator report rep-1')"), 'The integration-blocked notice is not shown.');
    await onlyChanged('task detail one job changed', '#job-evidence-content .evidence-entry', () => { detail.job_evidence.jobs[0] = { ...detail.job_evidence.jobs[0], state: 'succeeded', exit_code: 0 }; }, pollTick('GET /api/v1/projects/p1/tasks/task-1'));
    await onlyChanged('task detail one activity changed', '#workflow-content .workflow-entry', () => { detail.workflow.activities[1] = { ...detail.workflow.activities[1], status: 'active' }; }, pollTick('GET /api/v1/projects/p1/tasks/task-1'), 1);
    // Acceptance criteria, action buttons and the lease are untouched when only a checkpoint changed.
    const stable = ['#acceptance-list li', '#task-operator-actions button', '#lease-content dl'];
    for (const selector of stable) await evaluate(`window.__tag(${JSON.stringify(selector)}, 'stable')`);
    detail.checkpoints.push({ id: 'cp-4', attempt_id: 'attempt-1', summary: 'Fourth', current_action: 'x', next_step: 'y', blockers: [], created_at: '2026-10-13T11:40:00Z' });
    await pollTick('GET /api/v1/projects/p1/tasks/task-1')(); await settle();
    for (const selector of stable) assert((await evaluate(`window.__tags(${JSON.stringify(selector)}, 'stable')`)).every((tag) => tag > 0), `${selector} was rebuilt by a checkpoint-only change.`);
    // Lease remaining time ticking within the same displayed minute changes nothing.
    await quiet('task detail lease ticks within a minute', 'task-detail', async () => { detail.attempts[0] = { ...detail.attempts[0], lease_remaining_ms: 899000 }; await pollTick('GET /api/v1/projects/p1/tasks/task-1')(); });
    // Open <details>, focus and selection survive.
    await evaluate("document.querySelector('#workflow-content details').open = true");
    await evaluate("[...document.querySelectorAll('#workflow-content button')].find((button) => button.textContent === 'Start human review').focus()");
    await evaluate(`(() => { const text = document.querySelector('#checkpoints-content .checkpoint-summary'); const range = document.createRange(); range.selectNodeContents(text); getSelection().removeAllRanges(); getSelection().addRange(range); })()`);
    await quiet('task detail with open details, focus and selection', 'task-detail', pollTick('GET /api/v1/projects/p1/tasks/task-1'));
    assert(await evaluate("document.querySelector('#workflow-content details').open && document.activeElement.textContent === 'Start human review' && getSelection().toString() === 'Fourth'"), 'Open details, focus or selection were lost on an unchanged task refresh.');
    // The candidate (with the open details) is rebuilt: details stay open, nothing else moves.
    detail.workflow.submission = { ...detail.workflow.submission, summary: 'Submitted work, revised' };
    await pollTick('GET /api/v1/projects/p1/tasks/task-1')(); await settle();
    assert(await evaluate("document.querySelector('#workflow-content details').open && document.querySelector('#workflow-content').textContent.includes('revised')"), 'A rebuilt candidate lost its open details or kept stale text.');
    assert(await evaluate("document.activeElement.textContent === 'Start human review'"), 'Focus on an activity button was lost when the candidate was rebuilt.');
    await evaluate("getSelection().removeAllRanges()");
    // A focused field inside the page holds its region still until it loses focus.
    await evaluate("document.querySelector('#task-attachment-files').focus()");
    const heldHits = hitCount('GET /api/v1/projects/p1/tasks/task-1'); detail.checkpoints.push({ id: 'cp-4b', attempt_id: 'attempt-1', summary: 'Held back', current_action: 'x', next_step: 'y', blockers: [], created_at: '2026-10-13T11:41:00Z' });
    await tick(); await settle();
    assert(await evaluate("document.querySelectorAll('#checkpoints-content .checkpoint').length") === 4, 'The task page refreshed around a focused field.');
    assert(hitCount('GET /api/v1/projects/p1/tasks/task-1') >= heldHits, 'Hit counter went backwards.');
    await evaluate("document.querySelector('#task-attachment-files').blur()");
    detail.checkpoints.pop();
    await pollTick('GET /api/v1/projects/p1/tasks/task-1')(); await settle();
    // An open dialog blocks refreshes and survives; closing it resumes.
    await evaluate("[...document.querySelectorAll('#workflow-content button')].find((button) => button.textContent === 'Start human review').click()");
    await waitPage("document.querySelector('dialog[open] #workflow-decision')", 'review dialog');
    await evaluate("document.querySelector('#workflow-summary').value = 'typing in the dialog'");
    const withDialog = hitCount('GET /api/v1/projects/p1/tasks/task-1'); detail.checkpoints.push({ id: 'cp-5', attempt_id: 'attempt-1', summary: 'Fifth', current_action: 'x', next_step: 'y', blockers: [], created_at: '2026-10-13T11:45:00Z' });
    await tick(); await settle(); await tick(); await settle();
    assert(hitCount('GET /api/v1/projects/p1/tasks/task-1') === withDialog, 'The task page refreshed while a dialog was open.');
    assert(await evaluate("Boolean(document.querySelector('dialog[open]')) && document.querySelector('#workflow-summary').value === 'typing in the dialog'"), 'The open dialog or its text did not survive a poll.');
    assert(await evaluate("document.querySelectorAll('#checkpoints-content .checkpoint').length") === 4, 'The page behind a dialog changed.');
    await evaluate("document.querySelector('dialog[open]').close()");
    await pollTick('GET /api/v1/projects/p1/tasks/task-1')(); await settle();
    assert(await evaluate("document.querySelectorAll('#checkpoints-content .checkpoint').length") === 5, 'Polling did not resume after the dialog closed.');
    // A pending mutation blocks refreshes; the finished mutation patches the page in place.
    mutationDelay = 700;
    await evaluate("[...document.querySelectorAll('#task-operator-actions button')].find((button) => button.textContent === 'Mark human-delivered').click()");
    await waitPage("document.querySelector('dialog[open] #workflow-reason')", 'human delivery dialog');
    await evaluate("document.querySelector('#workflow-reason').value = 'Because'; document.querySelector('dialog[open] form').requestSubmit()");
    await waitPage('Boolean(document.querySelector("#global-alert")) && document.querySelector("#global-alert").textContent.includes("Saving")', 'pending mutation');
    const pending = hitCount('GET /api/v1/projects/p1/tasks/task-1'); await tick(); await settle();
    assert(hitCount('GET /api/v1/projects/p1/tasks/task-1') === pending, 'The task page refreshed during a pending mutation.');
    await evaluate("window.__tag('#checkpoints-content .checkpoint'); window.__tag('#acceptance-list li'); window.__tag('#job-evidence-content .evidence-entry')");
    await observe('#task-detail-view');
    await waitPage("document.querySelector('#detail-human-delivered') && !document.querySelector('#detail-human-delivered').hidden", 'finished mutation');
    await settle();
    mutationDelay = 0;
    const afterMutation = await drain();
    assert(afterMutation.length > 0 && afterMutation.length < 30, `The finished mutation should patch a few nodes, saw ${afterMutation.length}: ${afterMutation.slice(0, 8).join('; ')}`);
    assert(await evaluate("!document.querySelector('#task-detail-content').hidden && document.querySelector('#task-detail-state').hidden"), 'The task page was blanked after a mutation.');
    for (const selector of ['#checkpoints-content .checkpoint', '#acceptance-list li', '#job-evidence-content .evidence-entry']) assert((await evaluate(`window.__tags(${JSON.stringify(selector)})`)).every((tag) => tag > 0), `${selector} was rebuilt by a mutation that did not touch it.`);
    assert(await evaluate("document.querySelector('#detail-task-revision').textContent") === '5', 'The new revision is not shown.');
    // Attachments: unchanged quiet reload, then a new one is added without disturbing the first.
    await quiet('task attachments quiet reload', 'task-detail', pollTick('/history'));
    attachments.push({ id: 'art-2', display_name: 'more.txt', media_type: 'text/plain', size_bytes: 3, availability: 'available', kind: 'upload' });
    await onlyChanged('task attachments new file', '#task-attachments-list .task-attachment-entry', () => {}, pollTick('/history'), 1);
    attachments.pop();
    // Hidden tabs do not poll; becoming visible refreshes once and resumes.
    assert(await evaluate('window.__polls.size') === 1, 'Polling is not scheduled while the tab is visible.');
    await evaluate("(() => { Object.defineProperty(document, 'hidden', { configurable: true, get: () => true }); document.dispatchEvent(new Event('visibilitychange')); })()");
    assert(await evaluate('window.__polls.size') === 0, 'Polling kept running in a hidden tab.');
    const hiddenHits = hitCount('GET /api/v1/projects/p1/tasks/task-1'); await tick(); await settle();
    assert(hitCount('GET /api/v1/projects/p1/tasks/task-1') === hiddenHits, 'A hidden tab fetched the task.');
    detail.checkpoints.push({ id: 'cp-6', attempt_id: 'attempt-1', summary: 'Sixth', current_action: 'x', next_step: 'y', blockers: [], created_at: '2026-10-13T11:50:00Z' });
    await evaluate("(() => { Object.defineProperty(document, 'hidden', { configurable: true, get: () => false }); document.dispatchEvent(new Event('visibilitychange')); })()");
    await waitPage("document.querySelectorAll('#checkpoints-content .checkpoint').length === 6", 'refresh on becoming visible');
    assert(await evaluate('window.__polls.size') === 1, 'Polling did not resume when the tab became visible.');

    // ---- 10. Knowledge & evidence -----------------------------------------------------------------
    await goTo('.nav-item[data-view=shared]', 'shared');
    await waitPage("document.querySelectorAll('#shared-list article').length === 3", 'shared records');
    await quiet('shared refresh button', 'shared', clickAndWait('#refresh-shared', '/knowledge'));
    await evaluate("document.querySelector('#shared-list article details').open = true");
    await onlyChanged('shared one lesson changed', '#shared-list article', () => { lessons[1] = { ...lessons[1], body: 'Body B, corrected', revision: 2 }; }, clickAndWait('#refresh-shared', '/knowledge'));
    assert(await evaluate("document.querySelector('#shared-list article details').open"), 'A refresh closed an open disclosure on another record.');
    await evaluate("(() => { const select = document.querySelector('#shared-kind'); select.value = 'context'; select.dispatchEvent(new Event('change', { bubbles: true })); document.querySelector('#context-query').value = 'rules'; })()");
    await evaluate("document.querySelector('#context-search').requestSubmit()");
    await waitPage("document.querySelector('#shared-list details')", 'context rules');
    await quiet('shared context refresh button', 'shared', clickAndWait('#refresh-shared', '/context'));

    // ---- 11. Resources ----------------------------------------------------------------------------
    await goTo('.nav-item[data-view=resources]', 'resources');
    await waitPage("document.querySelectorAll('#resources-list article').length === 2", 'resources');
    await quiet('resources refresh button', 'resources', clickAndWait('#refresh-resources', '/api/v1/resources'));
    await quiet('resources poll', 'resources', pollTick('GET /api/v1/resources'));
    await onlyChanged('resources one changed', '#resources-list article', () => { resources[0] = { ...resources[0], held_units: 1 }; }, pollTick('GET /api/v1/resources'));
    await evaluate("document.querySelector('#resource-description').focus()");
    resources[1] = { ...resources[1], held_units: 0 };
    await pollTick('GET /api/v1/resources')(); await settle();
    assert(await evaluate("document.activeElement.id === 'resource-description' && document.querySelectorAll('#resources-list article')[1].textContent.includes('0 held')"), 'The resource form lost focus or the list did not refresh beside it.');
    await evaluate("document.querySelector('#resource-description').blur()");

    // ---- 12. Access & credentials -----------------------------------------------------------------
    await goTo('.nav-item[data-view=admin]', 'admin');
    await waitPage("document.querySelectorAll('#credentials-list .credential-row').length === 3 && document.querySelector('#restore-requirements .shared-card') && document.querySelector('#operators-list .credential-row') && document.querySelector('#clock-status p')", 'admin data');
    await quiet('admin credentials poll', 'admin', pollTick('GET /api/v1/admin/credentials'));
    for (const [button, needle] of [['#refresh-credentials', 'admin/credentials'], ['#refresh-operators', 'admin/operators'], ['#refresh-clock', 'admin/clock'], ['#refresh-restore', 'admin/restore']]) {
      await quiet(`admin ${button}`, 'admin', clickAndWait(button, needle));
    }
    await onlyChanged('credentials one changed', '#credentials-list .credential-row', () => { credentials[1] = { ...credentials[1], revoked_at: '2026-10-13T00:00:00Z' }; }, pollTick('GET /api/v1/admin/credentials'), 1);
    await onlyChanged('operators one changed', '#operators-list .credential-row', () => { operators[1] = { ...operators[1], enabled: false, revision: 2 }; }, clickAndWait('#refresh-operators', 'admin/operators'));
    await onlyChanged('restore requirement changed', '#restore-requirements .shared-card', () => { restore.items[0] = { ...restore.items[0], inspected: true }; }, clickAndWait('#refresh-restore', 'admin/restore'));

    // ---- 13. Dialog lists -------------------------------------------------------------------------
    await evaluate("[...document.querySelectorAll('#operators-list button')].find((button) => button.textContent === 'Browser sessions').click()");
    await waitPage("document.querySelectorAll('dialog[open] .shared-card').length === 3", 'browser sessions dialog');
    await evaluate("window.__tag('dialog[open] .shared-card')");
    await evaluate("[...document.querySelectorAll('dialog[open] .shared-card')][1].querySelector('button').click()");
    await waitPage("document.querySelectorAll('dialog[open] .shared-card')[1].textContent.includes('Revoked')", 'revoked browser session');
    assert(JSON.stringify(await evaluate("window.__tags('dialog[open] .shared-card')")) === '[1,0,3]', 'Revoking one browser session rebuilt more than its own row.');
    await evaluate("document.querySelector('dialog[open]').close()");

    console.log('PASS: every view patches in place. Unchanged refreshes (button, poll, navigation, finished mutation) made no DOM mutations on the overview, project settings, reports, digest, sessions, task queue, completed, archived, task detail, knowledge, resources and access views; one changed item rebuilt only its own node; focus, selection, open details and scroll survived; polling paused while hidden, with a dialog open and during a pending mutation.');
  } finally {
    socket?.close();
    if (chrome.pid && chrome.exitCode === null) { const exited = once(chrome, 'exit'); chrome.kill('SIGTERM'); await exited; }
    await new Promise((done) => server.close(done));
    await rm(profile, { recursive: true, force: true }).catch(() => {});
    if (launchError) throw launchError;
  }
}

main().catch((error) => { console.error(`FAIL: ${error.message}`); process.exitCode = 1; });
