// Browser test of the task status indicators: an unclaimed queued review, an actively claimed review and a
// live In Progress task are told apart and animated in both the task list and the task detail, expired and
// recovery states are not animated, the badges follow live state without a reload, tooltips open on focus as
// well as hover, and reduced-motion preferences stop the animation.
//
//   CHROME_BIN=/path/to/chrome [CHROME_ARGS="--no-sandbox"] [UI_SHOTS=DIR] node scripts/test_status_indicators.mjs
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
const reviewer = { role: 'reviewer', name: 'oracle-1-verdict', harness: 'claude-code', workstation: 'ws-review', lease_expires_at: '2026-10-13T12:30:00Z' };
const worker = { role: 'owner', name: 'agent-one', harness: null, workstation: null, lease_expires_at: null };
const makeTask = (id, title, workStatus, extra = {}) => ({ id, title, description: `Description of ${title}`, revision: 1, kind: 'code', lifecycle: 'open', work_status: workStatus, created_at: '2026-10-12T00:00:00Z', acceptance_criteria: ['One'], current_attempt_id: null, processing: null, ...extra });
const tasks = [
  makeTask('t-queued', 'Queued review task', 'waiting_review', { processing: { kind: 'review', stage: 'awaiting_reviewer', active: false, holders: [], pending_reviews: 1 } }),
  makeTask('t-review', 'Active review task', 'waiting_review', { processing: { kind: 'review', stage: 'in_review', active: true, holders: [reviewer], pending_reviews: 1 } }),
  makeTask('t-work', 'In progress task', 'in_progress', { current_attempt_id: 'a-1', processing: { kind: 'work', stage: 'in_progress', active: true, holders: [worker] } }),
  makeTask('t-lapsed', 'Lapsed review task', 'waiting_review', { processing: { kind: 'review', stage: 'review_lease_expired', active: false, holders: [], pending_reviews: 1 } }),
  makeTask('t-recovery', 'Recovery task', 'recovery_required', { current_attempt_id: 'a-2' }),
  makeTask('t-blocked', 'Blocked task', 'blocked', { blocked_reason: 'Waiting' }),
  // A pending scoped decision blocks the task while the reviewer's lease is still live; processing must not override it.
  makeTask('t-blocked-review', 'Blocked live review task', 'blocked', { processing: { kind: 'review', stage: 'in_review', active: true, holders: [reviewer], pending_reviews: 1 } }),
  makeTask('t-recovery-review', 'Recovery with review payload task', 'recovery_required', { current_attempt_id: 'a-3', processing: { kind: 'review', stage: 'in_review', active: true, holders: [reviewer], pending_reviews: 1 } }),
  makeTask('t-integration-work', 'Integration with work payload task', 'waiting_integration', { processing: { kind: 'work', stage: 'in_progress', active: true, holders: [worker] } }),
];
const detailOf = (task) => ({ ...task, attempts: [], checkpoints: [], workflow: { phase: 'review', blockers: [], submission: null, activities: [], next_actions: [] }, job_evidence: { jobs: [], reservations: [] }, lifecycle_events: [], replaces: [] });

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
  const profile = await mkdtemp(join(tmpdir(), 'agent-coordinator-status-test-'));
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
    await waitPage("document.querySelectorAll('#tasks-list .task-row').length === 9", 'task list');

    // What a badge looks like: its text, its icon, whether the icon animates, and its tooltip.
    const describe = (scope, title) => evaluate(`(() => {
      const root = ${scope === 'list' ? `[...document.querySelectorAll('#tasks-list .task-row')].find((row) => row.querySelector('.task-title').textContent === ${JSON.stringify(title)})` : "document.querySelector('#task-detail-view')"};
      const badge = root.querySelector(${scope === 'list' ? "'.status-badge'" : "'#detail-status'"}), icon = badge.querySelector('.status-icon'), tip = badge.querySelector('.status-tip');
      const name = icon ? getComputedStyle(icon).animationName : 'none';
      return { text: badge.querySelector('.status-label').textContent, icon: icon ? icon.className : null, animated: name !== 'none', ink: getComputedStyle(badge).color, tip: tip ? tip.textContent : null, tabindex: badge.getAttribute('tabindex'), described: badge.getAttribute('aria-describedby') === (tip && tip.id), tipShown: tip ? getComputedStyle(tip).display !== 'none' : false, role: tip && tip.getAttribute('role') };
    })()`);
    const expectations = {
      'Queued review task': { text: 'Awaiting Reviewer', animated: false },
      'Active review task': { text: 'In Review', animated: true },
      'In progress task': { text: 'In Progress', animated: true },
      'Lapsed review task': { text: 'Review Lease Expired', animated: false },
      'Recovery task': { text: 'Recovery Required', animated: false },
      'Blocked task': { text: 'Blocked', animated: false },
      'Blocked live review task': { text: 'Blocked', animated: false },
      'Recovery with review payload task': { text: 'Recovery Required', animated: false },
      'Integration with work payload task': { text: 'Waiting Integration', animated: false },
    };
    const inks = {};
    for (const [title, expected] of Object.entries(expectations)) {
      const view = await describe('list', title);
      assert(view.text === expected.text, `List badge for "${title}" reads "${view.text}", expected "${expected.text}".`);
      assert(view.animated === expected.animated, `List badge for "${title}" animated=${view.animated}, expected ${expected.animated}.`);
      inks[title] = view.ink;
    }
    assert(inks['Active review task'] !== inks['Queued review task'] && inks['Active review task'] !== inks['Lapsed review task'], 'The active review does not have a distinct color.');
    await shot('list');
    // An incompatible processing payload never wins: no review accent, stage class, active state or lease tooltip.
    const accent = (title) => evaluate(`(() => { const row = [...document.querySelectorAll('#tasks-list .task-row')].find((item) => item.querySelector('.task-title').textContent === ${JSON.stringify(title)}); const badge = row.querySelector('.status-badge'); return { rowClass: row.className, badgeClass: badge.className, tip: badge.querySelector('.status-tip') ? badge.querySelector('.status-tip').textContent : null, ink: getComputedStyle(badge).color, accent: getComputedStyle(row).getPropertyValue('--task-accent').trim() }; })()`);
    const plainBlocked = await accent('Blocked task');
    for (const title of ['Blocked live review task', 'Recovery with review payload task', 'Integration with work payload task']) {
      const view = await accent(title);
      assert(!/stage-|is-active/.test(view.rowClass + view.badgeClass) && view.tip === null, `"${title}" still carries processing state: ${JSON.stringify(view)}`);
    }
    const blockedLive = await accent('Blocked live review task');
    assert(blockedLive.ink === plainBlocked.ink && blockedLive.accent === plainBlocked.accent, `The blocked task with a live review lease has the review color or accent: ${JSON.stringify([blockedLive, plainBlocked])}`);
    // The queued review has a still dashed icon, never the active animation or color.
    const queued = await describe('list', 'Queued review task');
    assert(queued.icon.includes('queued') && !queued.tip.includes('Lease expires'), 'The queued review should show a still queued icon and no lease.');
    assert(queued.tip.includes('no reviewer has claimed'), `Queued tooltip does not say it is unclaimed: ${queued.tip}`);

    // Tooltips: hidden until hover or keyboard focus, accurate, and honest about missing data.
    const active = await describe('list', 'Active review task');
    assert(!active.tipShown && active.tabindex === '0' && active.described && active.role === 'tooltip', `Active review tooltip is not wired for accessibility: ${JSON.stringify(active)}`);
    for (const wanted of ['Reviewer: oracle-1-verdict', 'Harness: claude-code', 'Workstation: ws-review', 'Lease expires:', 'not that the process is running']) assert(active.tip.includes(wanted), `Active review tooltip lacks "${wanted}": ${active.tip}`);
    const work = await describe('list', 'In progress task');
    for (const wanted of ['Agent: agent-one', 'Harness: not recorded', 'Workstation: not recorded', 'Lease expires: not recorded']) assert(work.tip.includes(wanted), `In Progress tooltip lacks "${wanted}": ${work.tip}`);
    assert(await evaluate("(() => { const badge = [...document.querySelectorAll('#tasks-list .task-row')].find((row) => row.querySelector('.task-title').textContent === 'Active review task').querySelector('.status-badge'); badge.focus(); return getComputedStyle(badge.querySelector('.status-tip')).display !== 'none'; })()"), 'Keyboard focus does not open the tooltip.');
    await shot('list-tooltip');
    await evaluate('document.activeElement.blur()');
    assert(await evaluate("[...document.querySelectorAll('#tasks-list .task-row')].find((row) => row.querySelector('.task-title').textContent === 'Blocked task').querySelector('.status-badge').getAttribute('tabindex') === null"), 'A badge without context should not be a tab stop.');

    // Detail view shows the same indicators for the same tasks.
    const openDetail = async (title) => {
      await evaluate(`[...document.querySelectorAll('#tasks-list .task-row')].find((row) => row.querySelector('.task-title').textContent === ${JSON.stringify(title)}).click()`);
      await waitPage(`!document.querySelector('#task-detail-view').hidden && document.querySelector('#task-detail-heading').textContent === ${JSON.stringify(title)} && document.querySelector('#detail-status .status-label')`, `detail of ${title}`);
    };
    for (const [title, expected] of Object.entries(expectations)) {
      await openDetail(title);
      const view = await describe('detail', title);
      assert(view.text === expected.text && view.animated === expected.animated, `Detail badge for "${title}" is ${JSON.stringify([view.text, view.animated])}, expected ${JSON.stringify(expected)}.`);
      if (title === 'Active review task') await shot('detail-review');
      if (title === 'In progress task') await shot('detail-work');
      await evaluate("document.querySelector('#back-to-tasks').click()");
      await waitPage("document.querySelectorAll('#tasks-list .task-row').length === 9", 'task list');
    }

    // Live updates without a reload: the queued review gets claimed, the active review finishes, the
    // In Progress lease lapses. The 5 s poll repaints the list and the open detail.
    await openDetail('Queued review task');
    const reloadMark = await evaluate("window.__noReload = true");
    tasks[0] = { ...tasks[0], processing: { kind: 'review', stage: 'in_review', active: true, holders: [reviewer], pending_reviews: 1 } };
    tasks[1] = { ...tasks[1], lifecycle: 'done', work_status: 'done', processing: null };
    tasks[2] = { ...tasks[2], work_status: 'recovery_required', processing: null };
    await waitPage("document.querySelector('#detail-status .status-label').textContent === 'In Review' && getComputedStyle(document.querySelector('#detail-status .status-icon')).animationName !== 'none'", 'detail to follow the review claim', 9000);
    await evaluate("document.querySelector('#back-to-tasks').click()");
    await waitPage("document.querySelectorAll('#tasks-list .task-row').length >= 8", 'task list');
    await evaluate("document.querySelector('#tasks-auto-refresh').click()");
    await waitPage("(() => { const text = (title) => [...document.querySelectorAll('#tasks-list .task-row')].find((row) => row.querySelector('.task-title').textContent === title)?.querySelector('.status-label').textContent; return text('Queued review task') === 'In Review' && text('In progress task') === 'Recovery Required'; })()", 'list to follow live state via auto-refresh', 9000);
    const after = await describe('list', 'In progress task');
    assert(!after.animated && after.tip === null, 'A lapsed lease still animates or shows lease context.');
    assert(await evaluate('window.__noReload === true'), 'The page reloaded instead of updating in place.');
    const finished = await evaluate("(() => { const row = [...document.querySelectorAll('#tasks-list .task-row')].find((item) => item.querySelector('.task-title').textContent === 'Active review task'); return row ? row.querySelector('.status-label').textContent : null; })()");
    assert(finished === null || finished === 'Done', `A finished review still shows an active indicator: ${finished}`);

    // Blocked while the review lease stays live, then the decision is resolved: the same processing payload
    // is shown again as In Review in the open detail and in the list, without a reload; a lapsed lease stays inactive.
    const blockedIndex = tasks.findIndex((task) => task.id === 't-blocked-review');
    const blockedLiveReview = tasks[blockedIndex];
    const listLabel = (title) => `(() => { const row = [...document.querySelectorAll('#tasks-list .task-row')].find((item) => item.querySelector('.task-title').textContent === ${JSON.stringify(title)}); return row ? row.querySelector('.status-label').textContent : null; })()`;
    await openDetail('Blocked live review task');
    const detailBlocked = await describe('detail', 'Blocked live review task');
    assert(detailBlocked.text === 'Blocked' && !detailBlocked.animated && detailBlocked.tip === null && !await evaluate("document.querySelector('#detail-status').className.includes('stage-')"), `Detail of a blocked task with a live review lease shows review state: ${JSON.stringify(detailBlocked)}`);
    tasks[blockedIndex] = { ...blockedLiveReview, work_status: 'waiting_review' };
    await waitPage("document.querySelector('#detail-status .status-label').textContent === 'In Review' && getComputedStyle(document.querySelector('#detail-status .status-icon')).animationName !== 'none' && document.querySelector('#detail-status').className.includes('is-active')", 'detail to restore In Review after the decision is resolved', 9000);
    await evaluate("document.querySelector('#back-to-tasks').click()");
    await waitPage(`${listLabel('Blocked live review task')} === 'In Review'`, 'list to restore In Review after the decision is resolved', 9000);
    const restored = await describe('list', 'Blocked live review task');
    assert(restored.animated && restored.tip.includes('Reviewer: oracle-1-verdict'), `The restored review is not animated with its lease context: ${JSON.stringify(restored)}`);
    assert((await accent('Blocked live review task')).rowClass.includes('stage-in_review'), 'The restored review lacks its row accent.');
    // A new decision blocks it again, and then the lease expires: neither state is active.
    tasks[blockedIndex] = blockedLiveReview;
    await waitPage(`${listLabel('Blocked live review task')} === 'Blocked'`, 'list to block the review again', 9000);
    assert(!(await describe('list', 'Blocked live review task')).animated, 'The re-blocked review still animates.');
    tasks[blockedIndex] = { ...blockedLiveReview, work_status: 'waiting_review', processing: { kind: 'review', stage: 'review_lease_expired', active: false, holders: [], pending_reviews: 1 } };
    await waitPage(`${listLabel('Blocked live review task')} === 'Review Lease Expired'`, 'list to show the expired review lease', 9000);
    assert(!(await describe('list', 'Blocked live review task')).animated, 'An expired review lease animates.');
    assert(await evaluate('window.__noReload === true'), 'The page reloaded during the blocked-review transitions.');

    // Reduced motion: the stage text stays, the animation stops.
    await send('Emulation.setEmulatedMedia', { features: [{ name: 'prefers-reduced-motion', value: 'reduce' }] });
    const still = await describe('list', 'Queued review task');
    assert(still.text === 'In Review' && !still.animated, `Reduced motion still animates the active review: ${JSON.stringify(still)}`);
    await send('Emulation.setEmulatedMedia', { features: [{ name: 'prefers-reduced-motion', value: 'no-preference' }] });
    assert((await describe('list', 'Queued review task')).animated, 'The animation did not return when motion was allowed again.');

    console.log('PASS: queued, active and lapsed reviews and live work are told apart and animated only when a live claim exists, in both the list and the detail; badges follow live state without a reload; tooltips open on focus; reduced motion stops the animation.');
  } finally {
    socket?.close();
    if (chrome.pid && chrome.exitCode === null) { const exited = once(chrome, 'exit'); chrome.kill('SIGTERM'); await exited; }
    await new Promise((done) => server.close(done));
    await rm(profile, { recursive: true, force: true }).catch(() => {});
    if (launchError) throw launchError;
  }
}

main().catch((error) => { console.error(`FAIL: ${error.message}`); process.exitCode = 1; });
