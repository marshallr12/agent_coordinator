// Browser test of the integrator's activity in the task list and task details: queued and running
// verification, publication, passed and failed checks, and unknown (stale) reports are told apart; only a
// running verification or publication animates; evidence that does not belong to the task's current status
// never shows activity; safe check links appear only for a recognised forge and a full revision; the badges
// follow live state without a reload; the status filter and counts are unchanged; and a refresh with unchanged
// data changes no node in the list.
//
//   CHROME_BIN=/path/to/chrome [CHROME_ARGS="--no-sandbox"] node scripts/test_integration_status.mjs
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
const projects = [{ id: 'p1', name: 'Alpha', repository_url: 'https://github.com/example/alpha.git', target_branch: 'main' }];
const policy = { id: 'p1', name: 'Alpha', policy_revision: 3, review_mode: 'either', recovery_mode: 'agent', lease_seconds: 600, rules: 'Be careful.', agent_rule_editing: false, automatic_integration: true, integration_owner: 'integrator' };
const revision = (digit) => digit.repeat(40);
const makeTask = (id, title, workStatus, extra = {}) => ({ id, title, description: `Description of ${title}`, revision: 1, kind: 'code', lifecycle: workStatus === 'done' ? 'done' : 'open', work_status: workStatus, created_at: '2026-10-12T00:00:00Z', acceptance_criteria: ['One'], current_attempt_id: null, processing: null, ...extra });
const counts = (extra) => ({ total: 2, passed: 0, failed: 0, running: 0, queued: 0, pending: 0, ...extra });
const integration = (stage, active, digit, checks, extra = {}) => ({ kind: 'integration', stage, active, holders: [], submission_id: `sub-${digit}`, candidate_revision: revision('c'), result_id: `res-${digit}`, result_revision: revision(digit), result_created_at: '2026-10-13T10:00:00Z', observed_at: '2026-10-13T10:05:00Z', authority_expires_at: null, checks, check_runs: [], ...extra });
const tasks = [
  makeTask('t-queued', 'Queued verification', 'waiting_integration', { processing: integration('verification_queued', false, '1', counts({ queued: 2 })) }),
  makeTask('t-running', 'Running verification', 'waiting_integration', { processing: integration('verifying', true, '2', counts({ passed: 1, running: 1 }), { check_runs: [{ check_name: 'Coordination checks', state: 'running', run_id: 77, run_attempt: 1, conclusion: null, started_at: '2026-10-13T10:01:00Z' }, { check_name: 'Documentation checks', state: 'passed', run_id: 78, run_attempt: 1, conclusion: 'success', started_at: null }] }) }),
  makeTask('t-publishing', 'Publishing result', 'waiting_integration', { processing: integration('publishing', true, '3', counts({ passed: 2 }), { authority_expires_at: '2026-10-13T10:15:00Z' }) }),
  makeTask('t-unknown', 'Unknown verification', 'waiting_integration', { processing: integration('verification_unknown', false, '4', counts({ running: 1, passed: 1 })) }),
  makeTask('t-failed', 'Failed checks', 'waiting_integration', { processing: integration('checks_failed', false, '5', counts({ failed: 1, passed: 1 })) }),
  makeTask('t-awaiting', 'Awaiting integrator', 'waiting_integration'),
  makeTask('t-unlinked', 'Unlinkable revision', 'waiting_integration', { processing: integration('verifying', true, '6', counts({ running: 2 }), { result_revision: 'abc123' }) }),
  // Evidence that does not belong to the task's current status never shows activity.
  makeTask('t-revision', 'Sent back for revision', 'ready', { processing: integration('publishing', true, '8', counts({ passed: 2 })) }),
  makeTask('t-blocked', 'Blocked prerequisites', 'blocked', { blocked_reason: 'Waiting', processing: integration('verifying', true, '9', counts({ running: 1 })) }),
];
const detailOf = (task) => ({ ...task, attempts: [], checkpoints: [], workflow: { phase: 'integration', blockers: [], submission: { id: `sub-${task.id}`, task_id: task.id, kind: 'code', summary: 'Submitted', candidate_revision: revision('c'), task_revision: 1, project_policy_revision: 3, acceptance_evidence: [] }, activities: [], next_actions: [] }, job_evidence: { jobs: [], reservations: [] }, lifecycle_events: [], replaces: [] });

function json(response, data, status = 200) {
  response.writeHead(status, { 'Content-Type': 'application/json', 'Cache-Control': 'no-store' });
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
  const project = path.match(/^\/api\/v1\/projects\/p1(\/.*)?$/);
  if (project) {
    const rest = project[1] || '';
    if (rest === '/orientation') return json(response, { project: policy, policy_revision: policy.policy_revision });
    if (rest === '/tasks') return json(response, { items: tasks, next_cursor: null });
    if (rest === '/tasks/archived') return json(response, { items: [], next_cursor: null });
    const one = rest.match(/^\/tasks\/([^/]+)$/);
    if (one && tasks.find((task) => task.id === one[1])) return json(response, detailOf(tasks.find((task) => task.id === one[1])));
    if (/^\/tasks\/[^/]+\/history$/.test(rest)) return json(response, { items: [], next_cursor: null });
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


async function main() {
  const server = createServer((request, response) => route(request, response).catch((error) => { response.writeHead(500); response.end(String(error)); }));
  server.listen(0, '127.0.0.1');
  await once(server, 'listening');
  const serverPort = server.address().port;
  const debugPort = await unusedPort();
  const profile = await mkdtemp(join(tmpdir(), 'agent-coordinator-integration-status-test-'));
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
    await send('Page.navigate', { url: `http://127.0.0.1:${serverPort}/` });
    await waitPage("document.querySelector('.project-card')", 'project list');
    await evaluate("document.querySelector('.project-open').click()");
    await waitPage(`document.querySelectorAll('#tasks-list .task-row').length === ${tasks.length}`, 'task list');

    const row = (title) => `[...document.querySelectorAll('#tasks-list .task-row')].find((item) => item.querySelector('.task-title').textContent === ${JSON.stringify(title)})`;
    const describe = (title) => evaluate(`(() => { const row = ${row(title)}, badge = row.querySelector('.status-badge'), icon = badge.querySelector('.status-icon'), tip = badge.querySelector('.status-tip'), link = row.querySelector('.task-checks-link'); return { text: badge.querySelector('.status-label').textContent, animated: icon ? getComputedStyle(icon).animationName !== 'none' : false, active: badge.className.includes('is-active'), tip: tip ? tip.textContent : null, href: link ? link.href : null, rowClass: row.className }; })()`);
    const expectations = {
      'Queued verification': { text: 'Verification Queued', animated: false },
      'Running verification': { text: 'Verifying', animated: true },
      'Publishing result': { text: 'Publishing', animated: true },
      'Unknown verification': { text: 'Verification Unknown', animated: false },
      'Failed checks': { text: 'Checks Failed', animated: false },
      'Awaiting integrator': { text: 'Waiting Integration', animated: false },
      'Unlinkable revision': { text: 'Verifying', animated: true },
      'Sent back for revision': { text: 'Ready', animated: false },
      'Blocked prerequisites': { text: 'Blocked', animated: false },
    };
    for (const [title, expected] of Object.entries(expectations)) {
      const view = await describe(title);
      assert(view.text === expected.text && view.animated === expected.animated, `Badge for "${title}" is ${JSON.stringify([view.text, view.animated])}, expected ${JSON.stringify(expected)}.`);
    }
    // Queued is not running, and no badge claims an agent lease.
    const queued = await describe('Queued verification');
    assert(!queued.active && queued.tip.includes('None is running yet') && queued.tip.includes('0 of 2 passed, 2 queued'), `Queued tooltip is wrong: ${queued.tip}`);
    const running = await describe('Running verification');
    assert(running.active && running.tip.includes('1 of 2 passed, 1 running') && running.tip.includes('no agent owns or has claimed it') && !running.tip.includes('lease proves'), `Running tooltip is wrong: ${running.tip}`);
    assert((await describe('Publishing result')).tip.includes('until the target branch is observed'), 'Publishing tooltip does not say when it ends.');
    assert((await describe('Unknown verification')).tip.includes('cannot say whether verification is still going'), 'The stale report is not described as unknown.');
    // Stale, mismatched or finished evidence leaves no stage class, active state or tooltip.
    for (const title of ['Sent back for revision', 'Blocked prerequisites']) {
      const view = await describe(title);
      assert(!/stage-|is-active/.test(view.rowClass) && !view.active && view.tip === null, `"${title}" still shows integrator activity: ${JSON.stringify(view)}`);
    }
    // Links come from the integrator's result revision and the recognised repository only.
    assert((await describe('Running verification')).href === `https://github.com/example/alpha/commit/${revision('2')}/checks`, 'The checks link is not the exact result revision on the recognised forge.');
    for (const title of ['Unlinkable revision', 'Awaiting integrator', 'Sent back for revision']) assert((await describe(title)).href === null, `"${title}" has a checks link.`);
    // The link opens the forge, not the task.
    await evaluate(`${row('Running verification')}.querySelector('.task-checks-link').addEventListener('click', (event) => event.preventDefault())`);
    await evaluate(`${row('Running verification')}.querySelector('.task-checks-link').click()`);
    assert(await evaluate("document.querySelector('#task-detail-view').hidden"), 'Clicking the checks link opened the task.');

    // Filters and counts still follow the work status.
    await evaluate("(() => { const filter = document.querySelector('#status-filter'); filter.value = 'waiting_integration'; filter.dispatchEvent(new Event('change')); })()");
    await waitPage("document.querySelectorAll('#tasks-list .task-row').length === 7", 'the waiting-for-integration filter');
    await evaluate("(() => { const filter = document.querySelector('#status-filter'); filter.value = 'all'; filter.dispatchEvent(new Event('change')); })()");
    await waitPage(`document.querySelectorAll('#tasks-list .task-row').length === ${tasks.length}`, 'the unfiltered list');

    // The detail shows the same stage and the integrator's evidence, with no agent owner.
    await evaluate(`${row('Running verification')}.click()`);
    await waitPage("!document.querySelector('#task-detail-view').hidden && document.querySelector('#detail-status .status-label') && document.querySelector('.integrator-activity')", 'the detail');
    assert(await evaluate("document.querySelector('#detail-status .status-label').textContent === 'Verifying'"), 'The detail badge does not follow the stage.');
    const detail = await evaluate("document.querySelector('.integrator-activity').textContent");
    for (const wanted of ['Integrator: Verifying', 'No agent owns or has claimed it', 'Candidate revision', 'Integration result revision', '1 of 2 passed, 1 running', 'Coordination checks · Running · run 77 attempt 1', 'Documentation checks · Passed · run 78 attempt 1 · conclusion success']) assert(detail.includes(wanted), `The integrator activity lacks "${wanted}": ${detail}`);
    assert(await evaluate(`[...document.querySelectorAll('.integrator-activity a')].some((link) => link.href === 'https://github.com/example/alpha/commit/${revision('2')}/checks')`), 'The detail lacks the result revision checks link.');
    await evaluate("document.querySelector('#back-to-tasks').click()");
    await waitPage(`document.querySelectorAll('#tasks-list .task-row').length === ${tasks.length}`, 'task list');

    // A refresh with unchanged data touches no node.
    await evaluate("window.__records = []; window.__observer = new MutationObserver((list) => window.__records.push(...list)); window.__observer.observe(document.querySelector('#tasks-list'), { subtree: true, childList: true, attributes: true, characterData: true }); document.querySelector('#refresh-tasks').click(); document.querySelector('#tasks-auto-refresh').click()");
    await sleep(6500);
    assert(await evaluate('window.__records.length') === 0, `An unchanged refresh changed ${await evaluate('window.__records.length')} nodes.`);

    // Live transitions without a reload: queued starts running, the running one fails, publication finishes,
    // and the unknown report is refreshed. Only the changed rows are repainted.
    const label = (title) => `(${row(title)})?.querySelector('.status-label')?.textContent`;
    await evaluate('window.__noReload = true; window.__records = []');
    const sibling = await evaluate(`window.__sibling = ${row('Failed checks')}; true`);
    tasks[0] = { ...tasks[0], processing: integration('verifying', true, '1', counts({ running: 1, queued: 1 })) };
    tasks[1] = { ...tasks[1], processing: integration('checks_failed', false, '2', counts({ passed: 1, failed: 1 })) };
    tasks[2] = { ...tasks[2], lifecycle: 'done', work_status: 'done', processing: null };
    tasks[3] = { ...tasks[3], processing: integration('verifying', true, '4', counts({ running: 1, passed: 1 })) };
    await waitPage(`${label('Queued verification')} === 'Verifying' && ${label('Running verification')} === 'Checks Failed' && ${label('Publishing result')} === undefined && ${label('Unknown verification')} === 'Verifying'`, 'the list to follow the integrator', 9000);
    assert((await describe('Queued verification')).animated && !(await describe('Running verification')).animated, 'Animation does not follow the live stage.');
    assert(await evaluate(`${row('Failed checks')} === window.__sibling`), 'An unchanged row was replaced.');
    assert(await evaluate('window.__noReload === true'), 'The page reloaded instead of updating in place.');
    assert(await evaluate(`${row('Publishing result')} === undefined`), 'A completed integration is still listed as active work.');

    console.log('PASS: queued and running verification, publication, failed and unknown reports are told apart and animated only while active; stale, mismatched and finished evidence shows no activity; checks links are exact and safe; filters, counts, details and live transitions are consistent.');
  } finally {
    socket?.close();
    if (chrome.pid && chrome.exitCode === null) { const exited = once(chrome, 'exit'); chrome.kill('SIGTERM'); await exited; }
    await new Promise((done) => server.close(done));
    await rm(profile, { recursive: true, force: true }).catch(() => {});
    if (launchError) throw launchError;
  }
}

main().catch((error) => { console.error(`FAIL: ${error.message}`); process.exitCode = 1; });
