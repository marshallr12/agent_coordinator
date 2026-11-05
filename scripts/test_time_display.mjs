// Headless-Chrome check of the Local/UTC time display control, against a fixture service.
// Run with CHROME_BIN set to a Chrome or Chromium binary: node scripts/test_time_display.mjs
import { createServer } from 'node:http';
import { mkdtemp, readFile } from 'node:fs/promises';
import { rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { spawn } from 'node:child_process';
import { once } from 'node:events';
import net from 'node:net';

const root = resolve(import.meta.dirname, '..');
// 2026-10-05 is inside US daylight time, so New York local time is UTC-4 and the abbreviation is EDT.
const ZONE = 'America/New_York';
const CREATED = '2026-10-05T14:30:00Z', EXPIRES = '2026-10-05T16:45:00Z', CHECKPOINT = '2026-10-05T15:15:00Z';
const PROGRESS = '2026-10-05T15:20:00Z', FAILED_AT = '2026-10-05T13:45:00Z';
const OBSERVED = '2026-10-05T13:05:00Z', REVIEW_EXPIRES = '2026-10-05T17:20:00Z', STARTED = '2026-10-05T12:00:00Z', ACTIVE = '2026-10-05T15:40:00Z';
const task = {
  id: 'time-task', title: 'Time display task', description: 'Synthetic browser-fixture task.', revision: 3, kind: 'code', priority: 2,
  lifecycle: 'open', work_status: 'in_progress', current_attempt_id: 'time-attempt', created_at: CREATED, last_progress_at: PROGRESS, acceptance_criteria: [],
  processing: { stage: 'in_progress', active: true, holders: [{ role: 'owner', name: 'worker', harness: 'cli', workstation: 'ws', lease_expires_at: EXPIRES }] },
};
const detail = {
  ...task,
  attempts: [{ id: 'time-attempt', owner_id: 'worker', owner_name: 'worker', generation: 1, state: 'active', expires_at: EXPIRES }],
  checkpoints: [{ id: 'cp-1', actor_name: 'worker', summary: 'Halfway there', created_at: CHECKPOINT }],
  job_evidence: { jobs: [{ id: 'job-1', label: 'Gate run', state: 'succeeded', observation_freshness: 'fresh', exit_code: 0, last_observed_at: OBSERVED }, { id: 'job-2', label: 'Lint run', state: 'failed', exit_code: 1, source_revision: 'candidate-rev', created_at: FAILED_AT, last_observed_at: FAILED_AT }], reservations: [] },
  workflow: {
    phase: 'review', blockers: [], submission: { id: 'sub-1', result_id: 'res-1', candidate_revision: 'candidate-rev' },
    activities: [{ id: 'act-review', kind: 'agent_review', status: 'claimed', current_attempt: { owner_id: 'reviewer', expires_at: REVIEW_EXPIRES } }],
  },
};
const fixtureActor = { id: 'fixture-operator', name: 'Fixture operator', role: 'admin', kind: 'human', session_id: 'fixture-browser' };
const sessions = [{
  session_id: 'session-one', principal: { id: 'p-1', name: 'builder-agent', kind: 'agent' }, harness: 'claude-code', workstation_id: 'ws-1', capabilities: [], subagent: null,
  credential_id: 'cred-1', started_at: STARTED, last_activity_at: ACTIVE,
  held_attempts: [{ attempt_id: 'a-1', task_id: task.id, task_title: task.title, generation: 1, mode: 'work', activity_kind: null, subject_task_id: null, expires_at: EXPIRES, last_heartbeat_at: ACTIVE, lease_expired: false }],
}];

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
  const project = { id: 'fixture-project', name: 'Fixture project', target_branch: 'main', policy_revision: 1 };
  const server = createServer(async (request, response) => {
    const url = new URL(request.url, 'http://fixture.invalid');
    const base = '/api/v1/projects/fixture-project';
    if (url.pathname === '/api/v1/me') return json(response, { actor: fixtureActor, csrf_token: 'fixture-csrf' });
    if (url.pathname === '/api/v1/auth/account') return json(response, { operator: { id: fixtureActor.id, name: fixtureActor.name, role: 'admin', revision: 1, email: null, email_verified: false }, email_verification: {} });
    if (url.pathname === '/api/v1/auth/passkeys') return json(response, { items: [{ id: 'pk-1', name: 'Laptop key', created_at: CREATED, last_used_at: null }] });
    if (url.pathname === '/api/v1/projects') return json(response, { items: [project] });
    if (url.pathname === `${base}/orientation`) return json(response, { project, policy_revision: 1 });
    if (url.pathname === `${base}/workflow-policy`) return json(response, { revision: 1, canonical_repository_key: 'fixture/repo', required_checks: [] });
    if (url.pathname === `${base}/sessions`) return json(response, { project_id: 'fixture-project', active_within_hours: 24, generated_at: ACTIVE, items: sessions, truncated: false });
    if (url.pathname === `${base}/tasks`) return json(response, { items: [task], next_cursor: null });
    if (url.pathname === `${base}/tasks/${task.id}`) return json(response, detail);
    if (url.pathname === `${base}/tasks/${task.id}/history`) return json(response, { items: [], next_cursor: null });
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
  const profile = await mkdtemp(join(tmpdir(), 'agent-coordinator-time-test-'));
  const executable = process.env.CHROME_BIN || (process.platform === 'win32' ? 'C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe' : 'chromium');
  const chrome = spawn(executable, [
    '--headless=new', '--disable-gpu', '--no-first-run', '--no-default-browser-check',
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
      throw new Error(`Timed out waiting for ${label} (alert: ${await evaluate("document.querySelector('#global-alert')?.textContent || ''")})`);
    };
    const text = (selector) => evaluate(`document.querySelector(${JSON.stringify(selector)})?.textContent ?? null`);

    await send('Page.enable');
    await send('Emulation.setTimezoneOverride', { timezoneId: ZONE });
    await send('Emulation.setLocaleOverride', { locale: 'en-US' });
    // `?nostorage` makes every localStorage access throw, as a browser that blocks site data does.
    await send('Page.addScriptToEvaluateOnNewDocument', { source: `
      if (location.search === '?nostorage') Object.defineProperty(window, 'localStorage', { configurable: true, get() { throw new DOMException('Denied', 'SecurityError'); } });
    ` });
    const open = async (query = '') => {
      await send('Page.navigate', { url: `http://127.0.0.1:${serverPort}/${query}` });
      await waitPage("document.querySelector('.project-open')", 'project list');
    };

    // Dates are checked through the page's own timestamps: each <time> carries its UTC instant in datetime and title.
    const timeFor = (selector, iso) => `Array.from(document.querySelectorAll(${JSON.stringify(`${selector} time`)})).find(node => node.dateTime === ${JSON.stringify(new Date(iso).toISOString())})`;
    const timeText = (selector, iso) => evaluate(`${timeFor(selector, iso)}?.textContent ?? null`);
    const expectZone = async (selector, iso, expected, label) => {
      try { await waitPage(`${timeFor(selector, iso)}?.textContent.includes(${JSON.stringify(expected)})`, `${label} to show ${expected}`); }
      catch (error) { throw new Error(`${error.message}; it shows ${await timeText(selector, iso)}`); }
      const node = await evaluate(`(() => { const node = ${timeFor(selector, iso)}; return { title: node.title, tabIndex: node.tabIndex, text: node.textContent }; })()`);
      assert(node.title === new Date(iso).toISOString(), `${label} title is ${node.title}, not its exact UTC value.`);
      assert(node.tabIndex === 0, `${label} cannot be focused.`);
      return node.text;
    };
    const checkAll = async (zone) => {
      const utc = zone === 'utc', mark = utc ? 'UTC' : 'EDT';
      // Local 10:30 AM EDT is 2:30 PM UTC.
      const created = await expectZone('#tasks-list .task-row', CREATED, mark, 'Task list created time');
      assert(created.includes(utc ? '2:30 PM' : '10:30 AM'), `Task list time is wrong in ${zone}: ${created}`);
      // 11:20 AM EDT is 3:20 PM UTC.
      const progress = await expectZone('#tasks-list .task-row', PROGRESS, mark, 'Task list last progress');
      assert(progress.startsWith('Last progress ') && progress.includes(utc ? '3:20 PM' : '11:20 AM'), `Task list last progress is wrong in ${zone}: ${progress}`);
      await expectZone('#tasks-list .task-row .status-tip', EXPIRES, mark, 'Task list lease tooltip expiry');
      assert(!(await evaluate("document.querySelector('#tasks-list').textContent")).includes(utc ? 'EDT' : 'UTC'), `Task list mixes zones in ${zone}.`);
    };
    const openAccount = async () => {
      await evaluate("document.querySelector('#account-button').click()");
      await waitPage("document.querySelector('dialog[open] input[name=site-time-zone]')", 'account dialog');
    };
    const choose = async (value) => {
      await openAccount();
      assert(await evaluate("Array.from(document.querySelectorAll('dialog[open] input[name=site-time-zone]')).map(node => node.value).join()") === 'local,utc', 'Time display options are missing or misordered.');
      assert(await evaluate("document.querySelector('dialog[open] .time-zone-selector legend').textContent") === 'Time display', 'Time display group has no visible label.');
      assert(await evaluate("Array.from(document.querySelectorAll('dialog[open] .time-zone-selector label')).every(label => label.querySelector('input') && label.textContent.trim())"), 'A time display option has no visible label.');
      await evaluate(`(() => { const radio = document.querySelector('dialog[open] input[name=site-time-zone][value=${value}]'); radio.click(); })()`);
      // The open dialog's own passkey dates follow the choice immediately.
      await waitPage(`document.querySelector('dialog[open] .passkey-row time')?.textContent.includes(${JSON.stringify(value === 'utc' ? 'UTC' : 'EDT')})`, `passkey date in ${value}`);
      return value;
    };
    const closeDialog = async () => {
      await evaluate("document.querySelector('dialog[open]').close()");
      await waitPage("!document.querySelector('dialog[open]')", 'dialog to close');
    };

    await open();
    assert(await evaluate("localStorage.getItem('agent-coordinator.time-zone')") === null, 'The fixture profile already has a saved time display.');
    await evaluate("document.querySelector('.project-open').click()");
    await waitPage("document.querySelector('#tasks-list .task-row')", 'task list');

    // Default: local time with the local zone abbreviation.
    await checkAll('local');
    await openAccount();
    assert(await evaluate("document.querySelector('dialog[open] input[name=site-time-zone][value=local]').checked"), 'Local time is not the default choice.');
    await closeDialog();

    // Task detail, local.
    await evaluate("document.querySelector('#tasks-list .task-row').click()");
    await waitPage("!document.querySelector('#task-detail-content').hidden && document.querySelector('#checkpoints-content time')", 'task detail');
    const detailChecks = [
      ['#lease-content', EXPIRES, 'Lease expiry'], ['#checkpoints-content', CHECKPOINT, 'Checkpoint time'],
      ['#job-evidence-content', OBSERVED, 'Job observation time'], ['.failure-callout', FAILED_AT, 'Failure callout observation time'], ['#workflow-content', REVIEW_EXPIRES, 'Review lease expiry'],
    ];
    for (const [selector, iso, label] of detailChecks) {
      const shown = await expectZone(selector, iso, 'EDT', label);
      assert(!shown.includes('UTC'), `${label} is not local: ${shown}`);
    }
    assert((await timeText('#lease-content', EXPIRES)).includes('12:45 PM'), 'Lease expiry is not 12:45 PM local.');

    // Switching to UTC re-renders the open task page without a reload.
    await choose('utc');
    assert(await evaluate("localStorage.getItem('agent-coordinator.time-zone')") === 'utc', 'Choosing UTC did not save the choice.');
    for (const [selector, iso, label] of detailChecks) await expectZone(selector, iso, 'UTC', label);
    assert((await timeText('#lease-content', EXPIRES)).includes('4:45 PM'), 'Lease expiry is not 4:45 PM UTC.');
    assert(!(await text('#lease-content')).includes('EDT') && !(await text('#checkpoints-content')).includes('EDT'), 'Task detail still shows local zone text.');
    await closeDialog();
    // The lease expiry in the status tooltip follows the choice too.
    assert(await evaluate("document.querySelector('#detail-status').outerHTML.includes('EDT')") === false, 'The status tooltip still shows local time.');

    // Task list and sessions, UTC.
    await evaluate("document.querySelector('#back-to-tasks').click()");
    await waitPage("!document.querySelector('#tasks-view').hidden", 'task list return');
    await checkAll('utc');
    await evaluate("document.querySelector('.nav-item[data-view=overview]').click()");
    await waitPage("!document.querySelector('#overview-view').hidden", 'projects');
    await evaluate("document.querySelector('.project-settings-button').click()");
    await waitPage("!document.querySelector('#project-view').hidden", 'project settings');
    await evaluate("document.querySelector('#agent-sessions-button').click()");
    await waitPage("document.querySelector('#agent-sessions-list .agent-session-card')", 'sessions');
    for (const [iso, label] of [[STARTED, 'Session start'], [ACTIVE, 'Session activity'], [EXPIRES, 'Held attempt expiry']]) await expectZone('#agent-sessions-list', iso, 'UTC', label);
    assert((await timeText('#agent-sessions-list', STARTED)).includes('12:00 PM'), 'Session start is not 12:00 PM UTC.');

    // Switching back while sessions are open shows local time again.
    await choose('local');
    for (const [iso, label] of [[STARTED, 'Session start'], [ACTIVE, 'Session activity'], [EXPIRES, 'Held attempt expiry']]) await expectZone('#agent-sessions-list', iso, 'EDT', label);
    assert((await timeText('#agent-sessions-list', STARTED)).includes('8:00 AM'), 'Session start is not 8:00 AM local.');
    assert(await evaluate("localStorage.getItem('agent-coordinator.time-zone')") === 'local', 'Choosing Local did not save the choice.');
    await closeDialog();
    await choose('utc');
    await closeDialog();

    // The shown value is exact on keyboard focus, too.
    await evaluate("document.querySelector('#agent-sessions-list time').focus()");
    assert(await evaluate("document.activeElement.matches('time[datetime]')"), 'A time cannot take keyboard focus.');
    assert(await evaluate("getComputedStyle(document.activeElement, '::after').content") === `"${new Date(STARTED).toISOString()}"`, 'A focused time does not show its UTC value.');

    // The choice survives a reload.
    await open();
    await evaluate("document.querySelector('.project-open').click()");
    await waitPage("document.querySelector('#tasks-list .task-row')", 'task list after reload');
    await checkAll('utc');
    await openAccount();
    assert(await evaluate("document.querySelector('dialog[open] input[name=site-time-zone][value=utc]').checked"), 'The saved choice is not selected after a reload.');
    await closeDialog();

    // Phone width: the control and the dated rows fit.
    await send('Emulation.setDeviceMetricsOverride', { width: 390, height: 844, deviceScaleFactor: 1, mobile: true });
    await openAccount();
    assert(await evaluate("document.documentElement.scrollWidth <= window.innerWidth"), 'The account dialog overflows at phone width.');
    await closeDialog();
    assert(await evaluate("document.documentElement.scrollWidth <= window.innerWidth"), 'The task list overflows at phone width.');
    await send('Emulation.clearDeviceMetricsOverride');

    // Blocked storage: local time by default, and choosing still applies for the page.
    await evaluate("localStorage.removeItem('agent-coordinator.time-zone')");
    await open('?nostorage');
    await evaluate("document.querySelector('.project-open').click()");
    await waitPage("document.querySelector('#tasks-list .task-row')", 'task list without storage');
    await checkAll('local');
    await choose('utc');
    await checkAll('utc');
    await closeDialog();
    console.log('Time display control works in local and UTC for the task list, task detail and sessions.');
  } finally {
    socket?.close();
    chrome.kill();
    server.close();
    await removeProfile(profile);
    if (launchError) console.error(launchError);
  }
}

main().catch((error) => { console.error(error); process.exit(1); });
