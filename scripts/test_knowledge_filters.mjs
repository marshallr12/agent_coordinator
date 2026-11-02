import { createServer } from 'node:http';
import { mkdtemp, readFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { spawn } from 'node:child_process';
import { once } from 'node:events';
import net from 'node:net';

// Drives the Knowledge & evidence filters in headless Chrome against a fixture service that applies the
// same tag, kind, status, include_shared and pagination rules as GET .../knowledge. Run with CHROME_BIN.
const root = resolve(import.meta.dirname, '..');
const fixtureActor = { id: 'fixture-operator', name: 'Fixture operator', role: 'admin', kind: 'human', session_id: 'fixture-browser' };
const noScope = { task_ids: [], components: [], environments: [], versions: [] };
const record = (id, extra) => ({ id, source_project_id: 'fixture-project', collection: 'project', revision: 1, kind: 'lesson', status: 'observed', title: `Record ${id}`, body: `Body of ${id}`, scope: noScope, tags: [], applicability: '', ...extra });
const records = [
  record('k-cache', { title: 'Cache lesson', tags: ['cache', 'ci'], scope: { task_ids: ['task-9'], components: ['cache'], environments: ['linux'], versions: ['v2'] } }),
  record('k-db-fact', { title: 'Database fact', kind: 'fact', status: 'validated', tags: ['db'] }),
  record('k-rejected', { title: 'Rejected cache approach', kind: 'rejected_approach', status: 'deprecated', tags: ['Cache'] }),
  record('k-shared', { title: 'Shared cache lesson', source_project_id: 'other-project', collection: 'shared', status: 'validated', tags: ['cache', 'shared-tag'] }),
  ...Array.from({ length: 60 }, (_, index) => record(`k-bulk-${String(index).padStart(2, '0')}`, { title: `Bulk lesson ${index}`, tags: ['bulk'] })),
];
const requests = [];

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

// Mirrors the service's list filter: newest first, case-insensitive tag, shared collection only on request.
function knowledgeRoute(url, response) {
  const params = url.searchParams;
  requests.push({ route: 'knowledge', query: Object.fromEntries(params) });
  const tag = params.get('tag')?.toLowerCase();
  const matching = records.filter((item) => (item.source_project_id === 'fixture-project' || (params.get('include_shared') === 'true' && item.collection === 'shared'))
    && (!params.has('kind') || item.kind === params.get('kind')) && (!params.has('status') || item.status === params.get('status'))
    && (!tag || item.tags.some((value) => value.toLowerCase() === tag)));
  const limit = Number(params.get('limit') || 50), start = Number(params.get('cursor') || 0);
  const items = matching.slice(start, start + limit);
  return json(response, { items, next_cursor: start + items.length < matching.length ? String(start + items.length) : null });
}

function contextRoute(url, response) {
  requests.push({ route: 'context', query: Object.fromEntries(url.searchParams) });
  return json(response, { items: [{ type: 'knowledge', record: records[0] }], policy: { revision: 1, rules: 'Run the gate.' }, instructions_complete: true });
}

async function fixtureServer() {
  const files = {
    '/': ['web/index.html', 'text/html; charset=utf-8'],
    '/app.js': ['web/app.js', 'text/javascript; charset=utf-8'],
    '/style.css': ['web/style.css', 'text/css; charset=utf-8'],
  };
  const server = createServer(async (request, response) => {
    const url = new URL(request.url, 'http://fixture.invalid');
    if (url.pathname === '/api/v1/me') return json(response, { actor: fixtureActor, csrf_token: 'fixture-csrf' });
    if (url.pathname === '/api/v1/projects') return json(response, { items: [{ id: 'fixture-project', name: 'Fixture project', target_branch: 'main' }, { id: 'other-project', name: 'Other project', target_branch: 'main' }] });
    if (url.pathname === '/api/v1/projects/fixture-project/knowledge') return knowledgeRoute(url, response);
    if (url.pathname === '/api/v1/projects/fixture-project/context') return contextRoute(url, response);
    if (url.pathname.startsWith('/api/v1/')) return json(response, { items: [], next_cursor: null });
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
  const profile = await mkdtemp(join(tmpdir(), 'agent-coordinator-knowledge-test-'));
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
    await send('Page.enable');
    await send('Page.navigate', { url: `http://127.0.0.1:${serverPort}/` });
    await waitPage("document.querySelector('.project-open')", 'project list');

    const titles = () => evaluate("[...document.querySelectorAll('#shared-list article h2')].map((node) => node.textContent)");
    const waitTitles = async (expected, label) => {
      await waitPage(`JSON.stringify([...document.querySelectorAll('#shared-list article h2')].map((node) => node.textContent)) === ${JSON.stringify(JSON.stringify(expected))}`, label)
        .catch(async (error) => { throw new Error(`${error.message}; showing ${JSON.stringify(await titles())}`); });
    };
    const lastRequest = (route) => requests.filter((item) => item.route === route).at(-1).query;
    // Sets a control as a person would and fires the event the browser fires when the value is committed.
    const choose = (id, value) => evaluate(`(() => { const node = document.querySelector('#${id}'); if (node.type === 'checkbox') node.checked = ${JSON.stringify(value)}; else node.value = ${JSON.stringify(value)}; node.dispatchEvent(new Event('change', { bubbles: true })); })()`);
    const settle = () => new Promise((resolveDelay) => setTimeout(resolveDelay, 150));

    // Open the first project's records (two projects exist, so the project picker is used explicitly).
    await evaluate("document.querySelector('.project-open').click()");
    await evaluate("document.querySelector('.nav-item[data-view=shared]').click()");
    await waitPage("!document.querySelector('#shared-view').hidden", 'knowledge view');
    await evaluate("(() => { const select = document.querySelector('#shared-project'); select.value = 'fixture-project'; select.dispatchEvent(new Event('change', { bubbles: true })); })()");
    await waitPage("document.querySelectorAll('#shared-list article').length === 50", 'first unfiltered page');
    assert(await evaluate("!document.querySelector('#knowledge-filters').hidden && document.querySelector('#context-search').hidden"), 'Only the lessons filters should show for Lessons & facts.');
    for (const label of ['Tag', 'Kind', 'Status']) assert(await evaluate(`[...document.querySelectorAll('#knowledge-filters label')].some((node) => node.textContent.startsWith(${JSON.stringify(label)}) && node.control)`), `${label} has no visible label bound to its control.`);
    assert(await evaluate("document.querySelector('#knowledge-tag').maxLength === 64"), 'The tag input is not bounded to the server limit of 64.');
    let query = lastRequest('knowledge');
    assert(query.limit === '50' && query.include_shared === 'false' && !('tag' in query) && !('kind' in query) && !('status' in query), `Unfiltered request carried filters: ${JSON.stringify(query)}`);

    // Kind and status.
    await choose('knowledge-kind', 'fact');
    await waitTitles(['Database fact'], 'fact filter');
    query = lastRequest('knowledge');
    assert(query.kind === 'fact' && !('status' in query) && !('tag' in query), `Kind filter query was ${JSON.stringify(query)}`);
    assert(await evaluate("document.querySelector('#load-more-shared').hidden"), 'Load more stayed visible for a single page.');
    await choose('knowledge-status', 'deprecated');
    await waitPage("document.querySelector('#shared-state') && !document.querySelector('#shared-state').hidden && document.querySelector('#shared-state').textContent.includes('No matching records')", 'empty fact+deprecated list');
    assert(JSON.stringify(await titles()) === '[]', 'A filter change left stale records on screen.');
    await choose('knowledge-kind', 'rejected_approach');
    await waitTitles(['Rejected cache approach'], 'rejected approach and deprecated status');
    query = lastRequest('knowledge');
    assert(query.kind === 'rejected_approach' && query.status === 'deprecated', `Combined kind/status query was ${JSON.stringify(query)}`);
    await choose('knowledge-kind', '');
    await choose('knowledge-status', '');
    await waitPage("document.querySelectorAll('#shared-list article').length === 50", 'filters cleared');
    query = lastRequest('knowledge');
    assert(!('kind' in query) && !('status' in query), `Cleared filters were still sent: ${JSON.stringify(query)}`);

    // Tag filter, case-insensitive by server rule; include-shared adds the shared record with its source project.
    await choose('knowledge-status', 'validated');
    await waitTitles(['Database fact'], 'validated status');
    await choose('knowledge-status', '');
    await choose('knowledge-tag', ' cache ');
    await waitTitles(['Cache lesson', 'Rejected cache approach'], 'tag filter');
    assert(lastRequest('knowledge').tag === 'cache' && lastRequest('knowledge').include_shared === 'false', 'The tag was not trimmed and sent, or include_shared was set.');
    await choose('knowledge-shared', true);
    await waitTitles(['Cache lesson', 'Rejected cache approach', 'Shared cache lesson'], 'include shared');
    assert(lastRequest('knowledge').include_shared === 'true', 'include_shared=true was not sent.');
    const sharedCard = await evaluate("[...document.querySelectorAll('#shared-list article')].find((node) => node.querySelector('h2').textContent === 'Shared cache lesson').textContent");
    assert(sharedCard.includes('Shared from project other-project'), 'The shared record is not labelled with its source project.');
    assert(await evaluate("[...document.querySelectorAll('#shared-list article')].filter((node) => node.textContent.includes('Shared from project')).length") === 1, 'Only the shared record should carry a source-project label.');
    await choose('knowledge-shared', false);
    await waitTitles(['Cache lesson', 'Rejected cache approach'], 'shared records removed');

    // Chips: tags and scope as text; a tag chip applies that tag filter.
    const chipTexts = await evaluate("[...document.querySelector('#shared-list article').querySelectorAll('.record-chip')].map((node) => node.textContent)");
    assert(JSON.stringify(chipTexts) === JSON.stringify(['cache', 'ci', 'Task: task-9', 'Component: cache', 'Environment: linux', 'Version: v2']), `The record shows chips ${JSON.stringify(chipTexts)} instead of its tags and scope dimensions.`);
    await choose('knowledge-tag', '');
    await waitPage("document.querySelectorAll('#shared-list article').length === 50", 'tag cleared');
    await evaluate("[...document.querySelectorAll('#shared-list article')].find((node) => node.querySelector('h2').textContent === 'Database fact').querySelector('button.tag-chip').focus()");
    assert(await evaluate("document.activeElement.tagName === 'BUTTON' && document.activeElement.textContent === 'db'"), 'A tag chip is not keyboard-focusable.');
    await send('Input.dispatchKeyEvent', { type: 'keyDown', key: 'Enter', code: 'Enter', windowsVirtualKeyCode: 13, text: '\r' });
    await send('Input.dispatchKeyEvent', { type: 'keyUp', key: 'Enter', code: 'Enter', windowsVirtualKeyCode: 13 });
    await waitTitles(['Database fact'], 'tag chip filter');
    assert(await evaluate("document.querySelector('#knowledge-tag').value") === 'db' && lastRequest('knowledge').tag === 'db', 'Clicking a tag chip did not apply that tag filter.');

    // Pagination: a filter change starts a fresh list; Load more continues within the same filter.
    await choose('knowledge-tag', 'bulk');
    await waitPage("document.querySelectorAll('#shared-list article').length === 50 && !document.querySelector('#load-more-shared').hidden", 'bulk first page');
    assert(!('cursor' in lastRequest('knowledge')), 'A changed filter reused the previous cursor.');
    await evaluate("document.querySelector('#knowledge-tag').value = 'cache'");  // typed but not applied
    await evaluate("document.querySelector('#load-more-shared').click()");
    await waitPage("document.querySelectorAll('#shared-list article').length === 60", 'bulk second page');
    query = lastRequest('knowledge');
    assert(query.cursor === '50' && query.tag === 'bulk', `Load more left the displayed filter: ${JSON.stringify(query)}`);
    assert(await evaluate("document.querySelector('#load-more-shared').hidden"), 'Load more stayed visible after the last page.');
    assert(await evaluate("new Set([...document.querySelectorAll('#shared-list article h2')].map((node) => node.textContent)).size === 60"), 'Load more duplicated or dropped records.');
    await choose('knowledge-tag', '');
    await waitPage("document.querySelectorAll('#shared-list article').length === 50", 'reset after clearing tag');
    assert(!('cursor' in lastRequest('knowledge')) && !('tag' in lastRequest('knowledge')), 'Clearing the filter did not restart pagination.');

    // Over-long values are refused before any request.
    const before = requests.length;
    await choose('knowledge-tag', 'é'.repeat(33));
    await waitPage("document.querySelector('#shared-state').textContent.includes('at most 64 bytes')", 'byte limit message');
    assert(requests.length === before, 'A tag over the 64-byte limit was sent to the service.');
    await choose('knowledge-tag', '');
    await waitPage("document.querySelectorAll('#shared-list article').length === 50", 'recovered from over-long tag');

    // Context search: scope parameters are sent when filled and omitted when empty.
    await evaluate("(() => { const select = document.querySelector('#shared-kind'); select.value = 'context'; select.dispatchEvent(new Event('change', { bubbles: true })); })()");
    await waitPage("!document.querySelector('#context-search').hidden && document.querySelector('#knowledge-filters').hidden", 'context form');
    for (const id of ['context-task', 'context-component', 'context-environment', 'context-version']) assert(await evaluate(`document.querySelector('label[for=${id}]').textContent.length > 0 && document.querySelector('#${id}').labels.length === 1 && document.querySelector('#${id}').maxLength === 255`), `${id} lacks a visible label or the 255-byte bound.`);
    await evaluate("document.querySelector('#context-query').value = 'cache'; document.querySelector('#context-search').requestSubmit()");
    await waitPage("document.querySelectorAll('#shared-list article').length === 1", 'context result');
    query = lastRequest('context');
    assert(query.q === 'cache' && query.include_shared === 'false' && query.budget === '65536', `Context request was ${JSON.stringify(query)}`);
    for (const name of ['task_id', 'component', 'environment', 'version']) assert(!(name in query), `Empty ${name} was sent.`);
    await evaluate(`(() => {
      const set = (id, value) => { document.querySelector('#' + id).value = value; };
      set('context-task', ' task-9 '); set('context-component', 'cache'); set('context-environment', 'linux'); set('context-version', 'v2');
      document.querySelector('#context-shared').checked = true;
      document.querySelector('#context-search').requestSubmit();
    })()`);
    await settle();
    query = lastRequest('context');
    assert(query.task_id === 'task-9' && query.component === 'cache' && query.environment === 'linux' && query.version === 'v2' && query.include_shared === 'true', `Context scope query was ${JSON.stringify(query)}`);
    assert(await evaluate("[...document.querySelectorAll('#shared-list article')][0].querySelectorAll('button.tag-chip').length") === 0, 'Context results should not offer lessons-list tag filtering.');
    await evaluate("document.querySelector('#context-component').value = '  '; document.querySelector('#context-search').requestSubmit()");
    await settle();
    query = lastRequest('context');
    assert(!('component' in query) && query.task_id === 'task-9', `A blank component was sent: ${JSON.stringify(query)}`);

    // Mobile width: the filter controls stay inside the page.
    await send('Emulation.setDeviceMetricsOverride', { width: 375, height: 800, deviceScaleFactor: 1, mobile: true });
    assert(await evaluate("document.documentElement.scrollWidth <= window.innerWidth"), 'The context filters overflow a phone-width page.');
    await evaluate("(() => { const select = document.querySelector('#shared-kind'); select.value = 'knowledge'; select.dispatchEvent(new Event('change', { bubbles: true })); })()");
    await waitPage("!document.querySelector('#knowledge-filters').hidden", 'lessons form on phone');
    assert(await evaluate("document.documentElement.scrollWidth <= window.innerWidth"), 'The lessons filters overflow a phone-width page.');
    console.log('PASS: headless Chrome verified the lessons tag, kind, status and include-shared filters (server query, matching records only, source-project labels), tag and scope chips with keyboard-operable tag chips, pagination reset on every filter change and Load more within the displayed filter, the 64-byte tag bound, context task/component/environment/version parameters (sent when filled, omitted when empty), and phone-width layout.');
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
