// Browser test of the task queue's Blockers summary: every blocker category shows a heading with its count and
// links each item to the task, report or decision that clears it, "Show all" applies the matching status filter,
// a project with nothing blocked says so, the summary costs one digest and one integrator-report request per
// queue load however many tasks there are, a failed source is named instead of reading as "nothing is waiting",
// and the groups stack in one column at mobile width.
//
//   CHROME_BIN=/path/to/chrome [CHROME_ARGS="--no-sandbox"] node scripts/test_blockers_summary.mjs
//
// A fixture server stands in for the coordinator. Only Node's standard library and Chrome are used.
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
const policy = { id: 'p1', name: 'Alpha', policy_revision: 3, review_mode: 'either', recovery_mode: 'agent', lease_seconds: 600, rules: 'Be careful.', agent_rule_editing: false, automatic_integration: true };
const makeTask = (id, title, workStatus, preconditions = [], extra = {}) => ({ id, title, description: `Description of ${title}`, revision: 1, kind: 'code', lifecycle: 'open', work_status: workStatus, created_at: '2026-10-12T00:00:00Z', acceptance_criteria: ['One'], current_attempt_id: null, preconditions, ...extra });
const blockedBy = (code, message) => [{ code, message }];
// One fixture task per category, plus enough prerequisite waits to need "Show all".
const DECISION_MESSAGE = 'Resolve current scoped decisions for this task or workflow activity.';
const BLOCKER_MESSAGE = 'Waiting for the vendor to publish the credentials rotation window.';
const PREREQUISITE_MESSAGE = 'Complete every prerequisite task before claiming this task.';
const CHILDREN_MESSAGE = 'Complete the required child tasks before claiming this task.';
const RECOVERY_MESSAGE = 'Inspect the expired or unauthorized attempt and its jobs through the recovery workflow before resuming.';
const baseTasks = () => [
  makeTask('t-ready', 'Ready task', 'ready'),
  makeTask('t-decision-1', 'Decision task one', 'blocked', blockedBy('scoped_decisions_pending', DECISION_MESSAGE)),
  makeTask('t-decision-2', 'Decision task two', 'blocked', blockedBy('scoped_decisions_pending', DECISION_MESSAGE)),
  makeTask('t-saved', 'Saved blocker task', 'blocked', blockedBy('task_blocked', BLOCKER_MESSAGE)),
  ...Array.from({ length: 10 }, (_, index) => makeTask(`t-wait-${index}`, `Prerequisite task ${index + 1}`, 'blocked', blockedBy('dependencies_incomplete', PREREQUISITE_MESSAGE))),
  makeTask('t-children', 'Objective awaiting children', 'blocked', blockedBy('required_objective_children_incomplete', CHILDREN_MESSAGE)),
  makeTask('t-recovery', 'Recovery task', 'recovery_required', blockedBy('recovery_inspection_required', RECOVERY_MESSAGE), { current_attempt_id: 'a-1' }),
  makeTask('t-review', 'Review task', 'waiting_review'),
  makeTask('t-integration', 'Integration task', 'waiting_integration'),
];
let tasks = baseTasks();
const fixtureReport = (id, kind, requiresHuman) => ({ id, project_id: 'p1', kind, task_id: 't-review', details: { reason: kind }, requires_human: requiresHuman, created_at: '2026-10-06T00:00:00Z', resolved_at: null });
let reports = [fixtureReport('report-flaky', 'flaky', false), fixtureReport('report-gate', 'privilege_gate', true)];
let digest = { window_hours: 1, last_read_at: null, hri: { items: [] }, proceeded_decisions: [], pending_reversible_decisions: [{ decision_id: 'decision-1', question: 'Adopt the new retry policy?', recommendation: 'yes', proceeds_at: '2026-10-14T00:00:00Z' }] };
let failDigest = false;
const requests = [];
const detailOf = (task) => ({ ...task, attempts: [], checkpoints: [], workflow: { phase: 'review', blockers: [], submission: null, activities: [], next_actions: [] }, job_evidence: { jobs: [], reservations: [] }, lifecycle_events: [], replaces: [] });
const countOf = (needle) => requests.filter((entry) => entry.includes(needle)).length;

function json(response, data, status = 200) {
  response.writeHead(status, { 'Content-Type': 'application/json', 'Cache-Control': 'no-store' });
  response.end(JSON.stringify({ data }));
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
  const project = path.match(/^\/api\/v1\/projects\/p1(\/.*)?$/);
  if (project) {
    const rest = project[1] || '';
    requests.push(`${request.method} ${rest}${url.search}`);
    if (rest === '/orientation') return json(response, { project: policy, policy_revision: policy.policy_revision });
    if (rest === '/tasks') return json(response, { items: tasks, next_cursor: null });
    if (rest === '/tasks/archived') return json(response, { items: [], next_cursor: null });
    if (rest === '/digest') {
      if (failDigest) { response.writeHead(503, { 'Content-Type': 'application/json' }); return response.end(JSON.stringify({ error: { code: 'unavailable', message: 'Digest store is offline.' } })); }
      return json(response, digest);
    }
    if (rest === '/integrator/reports') return json(response, { items: [...reports].reverse().filter((report) => url.searchParams.get('open') !== 'true' || !report.resolved_at), next_before: null });
    if (rest === '/decisions' || rest === '/knowledge' || rest === '/artifacts') return json(response, { items: [], next_cursor: null });
    const one = rest.match(/^\/tasks\/([^/]+)$/);
    if (one && tasks.find((task) => task.id === one[1])) return json(response, detailOf(tasks.find((task) => task.id === one[1])));
    return json(response, { items: [], next_cursor: null });
  }
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

// Runs before the page's own scripts: a 5 s interval that only runs when the test calls __tick(), like a poll that fires on demand.
const PAGE_SETUP = `
  (() => {
    const realSet = window.setInterval.bind(window), realClear = window.clearInterval.bind(window);
    let next = 1000000; window.__polls = new Map();
    window.setInterval = (callback, ms, ...args) => { if (ms === 5000) { const id = ++next; window.__polls.set(id, callback); return id; } return realSet(callback, ms, ...args); };
    window.clearInterval = (id) => { if (window.__polls.delete(id)) return; realClear(id); };
    window.__tick = () => { for (const callback of [...window.__polls.values()]) callback(); };
  })();
`;

async function main() {
  const server = createServer((request, response) => route(request, response).catch((error) => { response.writeHead(500); response.end(String(error)); }));
  server.listen(0, '127.0.0.1');
  await once(server, 'listening');
  const serverPort = server.address().port;
  const debugPort = await unusedPort();
  const profile = await mkdtemp(join(tmpdir(), 'agent-coordinator-blockers-test-'));
  const executable = process.env.CHROME_BIN || 'chromium';
  const extra = (process.env.CHROME_ARGS || '').split(/\s+/).filter(Boolean);
  const chrome = spawn(executable, ['--headless=new', '--disable-gpu', '--no-first-run', '--no-default-browser-check', '--disable-background-networking', '--window-size=1200,800', `--remote-debugging-port=${debugPort}`, `--user-data-dir=${profile}`, ...extra], { stdio: 'ignore' });
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
    await send('Page.addScriptToEvaluateOnNewDocument', { source: PAGE_SETUP });
    await send('Page.navigate', { url: `http://127.0.0.1:${serverPort}/` });
    await waitPage("document.querySelector('.project-card')", 'project list');
    await evaluate("document.querySelector('.project-open').click()");

    // Reads the groups as {heading, items:[{label, note}], buttons:[aria-label]}, in page order.
    const groups = () => evaluate(`[...document.querySelectorAll('#blockers-groups > .blocker-group')].map((group) => ({
      heading: group.querySelector('h3').textContent,
      labelledBy: group.getAttribute('aria-labelledby') === group.querySelector('h3').id,
      items: [...group.querySelectorAll('.blocker-item')].map((item) => ({ label: item.querySelector('button').textContent, note: item.querySelector('.blocker-note').textContent })),
      buttons: [...group.querySelectorAll(':scope > button')].map((button) => ({ text: button.textContent, label: button.getAttribute('aria-label') })),
    }))`);
    const state = () => evaluate("(() => { const node = document.querySelector('#blockers-state'); return node.hidden ? '' : node.textContent; })()");
    const poll = async () => { const seen = countOf('GET /tasks?'); await evaluate('window.__tick()'); const deadline = Date.now() + 5000; while (countOf('GET /tasks?') === seen && Date.now() < deadline) await sleep(30); await sleep(250); };
    const showQueue = async () => { await evaluate("document.querySelector('#task-queue-tab').click()"); await waitPage("!document.querySelector('#tasks-view').hidden && document.querySelector('#blockers-groups')", 'task queue'); };
    await evaluate("(() => { const box = document.querySelector('#tasks-auto-refresh'); box.checked = true; box.dispatchEvent(new Event('change', { bubbles: true })); })()");

    // ---- Every category: counts, items, messages -----------------------------------------------------
    await waitPage("document.querySelectorAll('#blockers-groups > .blocker-group').length === 6", 'six blocker groups');
    const first = await groups();
    assert(first.every((group) => group.labelledBy), 'Every group must be a section labelled by its heading.');
    assert(await evaluate("document.querySelector('#blockers-card').getAttribute('aria-labelledby') === 'blockers-heading' && document.querySelector('#blockers-heading').tagName === 'H2'"), 'The card needs an h2 heading its groups sit under.');
    assert(JSON.stringify(first.map((group) => group.heading)) === JSON.stringify([
      'Needs a human decision (3)', 'Saved blocker (1)', 'Waiting on prerequisites (11)', 'Recovery required (1)', 'Awaiting review or integration (2)', 'Integrator reports needing a person (1)',
    ]), `Unexpected group headings: ${JSON.stringify(first.map((group) => group.heading))}`);
    const [decision, saved, prerequisites, recovery, review, reportGroup] = first;
    assert(JSON.stringify(decision.items.map((item) => item.label)) === JSON.stringify(['Decision task one', 'Decision task two', 'Adopt the new retry policy?']), `Decision items: ${JSON.stringify(decision.items)}`);
    assert(decision.items[0].note === DECISION_MESSAGE && decision.items[2].note.startsWith('Proceeds on its recommendation (yes)'), 'The decision items must show the precondition message or the recommendation.');
    assert(saved.items.length === 1 && saved.items[0].note === BLOCKER_MESSAGE, 'The saved blocker must show its recorded reason.');
    assert(prerequisites.items.length === 10 && prerequisites.items[0].note === PREREQUISITE_MESSAGE, 'A group lists at most ten items.');
    assert(JSON.stringify(prerequisites.buttons.map((button) => button.text)) === JSON.stringify(['Show all blocked tasks']), `Prerequisite group controls: ${JSON.stringify(prerequisites.buttons)}`);
    assert(prerequisites.buttons[0].label.startsWith('Show all blocked tasks') && prerequisites.buttons[0].label.includes('Waiting on prerequisites'), 'Show all needs an accessible name that names its group.');
    assert(recovery.items[0].label === 'Recovery task' && recovery.items[0].note === RECOVERY_MESSAGE, 'The recovery group must link the task with its precondition message.');
    assert(JSON.stringify(review.items.map((item) => [item.label, item.note])) === JSON.stringify([['Review task', 'Waiting Review'], ['Integration task', 'Waiting Integration']]), `Review items: ${JSON.stringify(review.items)}`);
    assert(reportGroup.items.length === 1 && reportGroup.items[0].label === 'Privilege gate', `Only reports that need a person belong in the summary: ${JSON.stringify(reportGroup.items)}`);
    for (const group of [decision, saved, recovery, review, reportGroup]) assert(group.buttons.length === 0, `A group with at most ten items needs no Show all: ${group.heading}`);
    assert(!JSON.stringify(first).match(/token|secret|password|bearer/i), 'The summary must not render credentials.');
    assert(await state() === '', 'The empty-state panel must be hidden while something is waiting.');

    // ---- Request budget: one digest and one reports request per queue load ---------------------------
    const queueLoads = () => countOf('GET /tasks?');
    assert(countOf('GET /digest') >= 1 && countOf('GET /digest') <= queueLoads() && countOf('GET /integrator/reports') >= 1 && countOf('GET /integrator/reports') <= queueLoads(), `Each queue load may issue at most one digest and one reports request, saw ${countOf('GET /digest')} and ${countOf('GET /integrator/reports')} for ${queueLoads()} loads.`);
    assert(requests.filter((entry) => entry.includes('GET /tasks/')).length === 0, 'The summary must not fetch task detail per task.');
    const taskLoads = queueLoads(), digests = countOf('GET /digest'), reportLoads = countOf('GET /integrator/reports');
    tasks = [...tasks, ...Array.from({ length: 60 }, (_, index) => makeTask(`t-extra-${index}`, `Extra waiting task ${index}`, 'blocked', blockedBy('dependencies_incomplete', PREREQUISITE_MESSAGE)))];
    await poll();
    const loaded = queueLoads() - taskLoads;
    assert(loaded >= 1, 'The poll did not reload the queue.');
    assert(countOf('GET /digest') - digests >= 1 && countOf('GET /digest') - digests <= loaded && countOf('GET /integrator/reports') - reportLoads >= 1 && countOf('GET /integrator/reports') - reportLoads <= loaded, `A reload of ${tasks.length} tasks must add at most one digest and one reports request per queue load, saw ${countOf('GET /digest') - digests} and ${countOf('GET /integrator/reports') - reportLoads} for ${loaded} load(s).`);
    assert((await groups())[2].heading === 'Waiting on prerequisites (71)', 'The count must follow the whole queue, not the visible page.');
    assert((await groups())[2].items.length === 10, 'More waiting tasks must still show ten items.');
    tasks = baseTasks();
    await poll();

    // ---- Links: task, report, decision ----------------------------------------------------------------
    await evaluate("[...document.querySelectorAll('#blockers-groups .blocker-item button')].find((button) => button.textContent === 'Saved blocker task').click()");
    await waitPage("!document.querySelector('#task-detail-view').hidden && document.querySelector('#task-detail-heading').textContent === 'Saved blocker task'", 'task detail from a blocker');
    await evaluate("document.querySelector('#back-to-tasks').click()");
    await waitPage("!document.querySelector('#tasks-view').hidden && document.querySelectorAll('#blockers-groups > .blocker-group').length === 6", 'queue after returning');
    await evaluate("[...document.querySelectorAll('#blockers-groups .blocker-item button')].find((button) => button.textContent === 'Privilege gate').click()");
    await waitPage("!document.querySelector('#reports-view').hidden && document.querySelector('#reports-list .report-card')", 'integrator reports from a blocker');
    await evaluate("document.querySelector('.nav-item[data-view=tasks]').click()");
    await waitPage("!document.querySelector('#tasks-view').hidden && document.querySelectorAll('#blockers-groups > .blocker-group').length === 6", 'queue after reports');
    await evaluate("[...document.querySelectorAll('#blockers-groups .blocker-item button')].find((button) => button.textContent === 'Adopt the new retry policy?').click()");
    await waitPage("!document.querySelector('#shared-view').hidden && document.querySelector('#shared-kind').value === 'decisions'", 'decisions list from a blocker');
    await evaluate("document.querySelector('.nav-item[data-view=tasks]').click()");
    await waitPage("!document.querySelector('#tasks-view').hidden && document.querySelectorAll('#blockers-groups > .blocker-group').length === 6", 'queue after decisions');

    // ---- Show all applies the matching status filter --------------------------------------------------
    await evaluate("document.querySelector('#blockers-groups .blocker-group:nth-child(3) > button').click()");
    await waitPage("document.querySelector('#status-filter').value === 'blocked'", 'blocked status filter');
    assert(await evaluate("[...document.querySelectorAll('#tasks-list .task-row')].every((row) => row.classList.contains('blocked')) && document.querySelectorAll('#tasks-list .task-row').length === 14"), 'Show all must list every blocked task.');
    assert(await evaluate("document.querySelectorAll('#blockers-groups > .blocker-group').length === 6"), 'Filtering the list must not change the summary, which always covers the whole queue.');
    assert(await evaluate("document.activeElement.id === 'status-filter'"), 'Show all must move focus to the filter it applied.');
    await evaluate("(() => { const filter = document.querySelector('#status-filter'); filter.value = 'all'; filter.dispatchEvent(new Event('change')); })()");

    // ---- Keyboard: every control is a native button that can take focus --------------------------------
    assert(await evaluate("[...document.querySelectorAll('#blockers-card button')].every((button) => button.tagName === 'BUTTON' && button.type === 'button' && button.tabIndex >= 0 && button.textContent.trim())"), 'Blocker controls must be labelled, focusable buttons.');
    await evaluate("document.querySelector('#blockers-groups .blocker-item button').focus()");
    assert(await evaluate("document.activeElement.closest('.blocker-group') !== null"), 'A blocker link did not take keyboard focus.');
    await evaluate('document.activeElement.blur()');

    // ---- Mobile: one column; desktop: more than one ---------------------------------------------------
    const columns = () => evaluate("getComputedStyle(document.querySelector('#blockers-groups')).gridTemplateColumns.split(' ').length");
    assert(await columns() > 1, 'Wide screens should lay the groups out in several columns.');
    await send('Emulation.setDeviceMetricsOverride', { width: 390, height: 844, deviceScaleFactor: 1, mobile: true });
    await sleep(150);
    assert(await columns() === 1, 'The groups must lay out in one column at mobile width.');
    assert(await evaluate("(() => { const card = document.querySelector('#blockers-card'); return card.scrollWidth <= card.clientWidth; })()"), 'The summary must not overflow its card at mobile width.');
    await send('Emulation.clearDeviceMetricsOverride');

    // ---- A source that fails is named, never read as "nothing is waiting" -------------------------------
    failDigest = true; tasks = baseTasks().filter((task) => task.work_status === 'ready'); reports = [];
    await poll();
    assert((await state()).includes('The attention digest could not be loaded'), `A failed digest must be named, saw "${await state()}".`);
    assert(!(await state()).startsWith('Nothing is waiting'), 'A failed source must not read as nothing waiting.');
    failDigest = false;

    // ---- Empty: nothing is waiting ---------------------------------------------------------------------
    digest = { ...digest, pending_reversible_decisions: [] };
    reports = [fixtureReport('report-flaky', 'flaky', false)];
    await poll();
    await waitPage("document.querySelectorAll('#blockers-groups > .blocker-group').length === 0 && !document.querySelector('#blockers-state').hidden", 'empty summary');
    assert((await state()).startsWith('Nothing is waiting'), `Unexpected empty message: ${await state()}`);
    assert(await evaluate("document.querySelector('#blockers-state').getAttribute('role') === 'status'"), 'The empty message must be announced as a status.');

    // ---- Completed view hides the card; the queue shows it again ---------------------------------------
    await evaluate("document.querySelector('#completed-tasks-tab').click()");
    await waitPage("document.querySelector('#blockers-card').hidden", 'blockers hidden on completed tasks');
    await showQueue();
    await waitPage("!document.querySelector('#blockers-card').hidden", 'blockers back on the queue');

    console.log('PASS: the Blockers summary counts and links every category, applies the status filter from Show all, says nothing is waiting when that is true, names a failed source, makes one digest and one reports request per queue load, and uses one column at mobile width.');
  } finally {
    socket?.close();
    if (chrome.pid && chrome.exitCode === null) { const exited = once(chrome, 'exit'); chrome.kill('SIGTERM'); await exited; }
    await new Promise((done) => server.close(done));
    await rm(profile, { recursive: true, force: true }).catch(() => {});
    if (launchError) throw launchError;
  }
}

main().catch((error) => { console.error(`FAIL: ${error.message}`); process.exitCode = 1; });
