// Browser test of the Resources view's search, sort and free-capacity controls (book/src/docs/web-ui-refresh.md).
//
//   CHROME_BIN=/path/to/chrome [CHROME_ARGS="--no-sandbox"] node scripts/test_resource_search.mjs
//
// A fixture server stands in for the coordinator and pages /api/v1/resources by id. The test searches
// by key and by description, runs each sort, applies the free-capacity filter, checks the Available
// column on every row, checks the mobile layout, and then grows the fixture past the 2,000-resource
// cap to require the visible note. Only Node's standard library and Chrome are used.
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
const projects = [{ id: 'p1', name: 'Alpha project', repository_url: 'https://example.invalid/alpha.git', target_branch: 'main' }];

// Six named resources first (by id), then fillers that are fully held. `total` grows for the cap test.
const named = [
  { key: 'alpha:db', capacity: 3, held_units: 1, description: 'Primary database' },
  { key: 'beta:device', capacity: 1, held_units: 1, description: 'Test phone' },
  { key: 'gamma:runner', capacity: 10, held_units: 4, description: 'Build runners for ALPHA' },
  { key: 'delta:bench', capacity: 5, held_units: 5, description: 'Bench' },
  { key: 'Epsilon:lab', capacity: 2, held_units: 0, description: 'Lab' },
  { key: 'zeta:gpu', capacity: 4, held_units: 2, description: '' },
];
let total = 400;
const resourceAt = (index) => {
  const id = `r-${String(index + 1).padStart(5, '0')}`;
  return index < named.length ? { id, created_at: '2026-10-01T00:00:00Z', ...named[index] }
    : { id, key: `zz-filler:${String(index).padStart(5, '0')}`, capacity: 1, held_units: 1, description: 'Bulk filler', created_at: '2026-10-01T00:00:00Z' };
};
const resourceRequests = [];

const files = { '/': ['web/index.html', 'text/html; charset=utf-8'], '/app.js': ['web/app.js', 'text/javascript; charset=utf-8'], '/style.css': ['web/style.css', 'text/css; charset=utf-8'] };
async function route(request, response) {
  const url = new URL(request.url, 'http://fixture.invalid');
  const path = url.pathname;
  const json = (data) => { response.writeHead(200, { 'Content-Type': 'application/json' }); response.end(JSON.stringify({ data })); };
  if (files[path]) { response.writeHead(200, { 'Content-Type': files[path][1], 'Cache-Control': 'no-store' }); return response.end(await readFile(join(root, files[path][0]))); }
  if (path === '/api/v1/me') return json({ actor, csrf_token: 'csrf' });
  if (path === '/api/v1/projects') return json({ items: projects });
  if (path === '/api/v1/resources') {
    const limit = Number(url.searchParams.get('limit') || 50), cursor = url.searchParams.get('cursor');
    resourceRequests.push({ limit, cursor, hasLimit: url.searchParams.has('limit') });
    assert(limit >= 1 && limit <= 200, `The page limit ${limit} is outside the API's 1-200 range.`);
    const start = cursor ? Number(cursor.slice(2)) : 0;
    const items = Array.from({ length: Math.max(0, Math.min(limit, total - start)) }, (_, offset) => resourceAt(start + offset));
    return json({ items, next_cursor: start + items.length < total ? items.at(-1).id : null });
  }
  if (path === '/api/v1/admin/clock' || path === '/api/v1/admin/restore') return json({});
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
  const profile = await mkdtemp(join(tmpdir(), 'agent-coordinator-resource-search-'));
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
      const deadline = Date.now() + 15000;
      while (Date.now() < deadline) { if (await evaluate(expression)) return; await sleep(30); }
      throw new Error(`Timed out waiting for ${label}`);
    };
    const until = async (predicate, label) => {
      const deadline = Date.now() + 15000;
      while (Date.now() < deadline) { if (predicate()) return; await sleep(30); }
      throw new Error(`Timed out waiting for ${label}`);
    };
    // The first `count` rows as [key, capacity, held, available] cell text.
    const rows = (count) => evaluate(`[...document.querySelectorAll('#resources-list tr')].slice(0, ${count}).map((row) => [...row.children].slice(0, 5).map((cell) => cell.textContent).join('|'))`);
    const setControls = ({ search = '', sort = 'key', free = false }) => evaluate(`(() => {
      const search = document.querySelector('#resources-search'), sort = document.querySelector('#resources-sort'), free = document.querySelector('#resources-free');
      sort.value = ${JSON.stringify(sort)}; sort.dispatchEvent(new Event('change', { bubbles: true }));
      free.checked = ${free}; free.dispatchEvent(new Event('change', { bubbles: true }));
      search.value = ${JSON.stringify(search)}; search.dispatchEvent(new Event('input', { bubbles: true }));
    })()`);
    const expectRows = async (count, expected, label) => {
      await waitPage(`document.querySelectorAll('#resources-list tr').length === ${count}`, `${label}: ${count} rows`);
      const actual = await rows(expected.length);
      assert(JSON.stringify(actual) === JSON.stringify(expected), `${label}: expected ${JSON.stringify(expected)}, saw ${JSON.stringify(actual)}`);
    };

    await send('Page.enable');
    await send('Page.navigate', { url: `http://127.0.0.1:${serverPort}/` });
    await waitPage("document.querySelector('.project-card')", 'project list');
    await evaluate("document.querySelector('.nav-item[data-view=resources]').click()");
    await waitPage("document.querySelectorAll('#resources-list tr').length === 50", 'the first resource page');
    assert(resourceRequests.length === 1 && !resourceRequests[0].hasLimit, 'Opening the view should fetch one default page and nothing more.');
    assert(await evaluate("document.querySelector('#resources-note').hidden"), 'The cap note should be hidden below the cap.');
    assert(await evaluate("!document.querySelector('#load-more-resources').hidden"), 'Load more should be offered while pages remain.');

    // Controls have visible labels and are reachable by keyboard.
    const labels = await evaluate("['resources-search', 'resources-sort', 'resources-free'].map((id) => { const input = document.getElementById(id); return [input.labels[0]?.textContent.trim(), input.tabIndex, getComputedStyle(input).display]; })");
    assert(labels[0][0] === 'Search' && labels[1][0].startsWith('Sort by') && labels[2][0] === 'Only resources with free capacity', `Unexpected control labels ${JSON.stringify(labels)}`);
    assert(labels.every(([, tab, display]) => tab >= 0 && display !== 'none'), 'Every control must be focusable and shown.');

    // Search: key or description, case-insensitive; it fetches the remaining pages (200 at a time) first.
    await setControls({ search: 'alpha' });
    await expectRows(2, ['alpha:db|Primary database|3|1|2', 'gamma:runner|Build runners for ALPHA|10|4|6'], 'search alpha');
    await until(() => resourceRequests.length >= 3, 'the remaining pages');
    assert(resourceRequests.slice(1).every((request) => request.limit === 200) && resourceRequests.length === 3, `Searching should fetch the remaining pages at limit 200, saw ${JSON.stringify(resourceRequests)}`);
    await setControls({ search: 'EPSILON:LAB' });
    await expectRows(1, ['Epsilon:lab|Lab|2|0|2'], 'search by upper-case key');
    await setControls({ search: 'no such resource' });
    await waitPage("document.querySelectorAll('#resources-list tr').length === 0 && document.querySelector('#resources-state').textContent.includes('No resources match')", 'the empty search message');
    await setControls({});
    await waitPage("document.querySelectorAll('#resources-list tr').length === 400 && document.querySelector('#resources-state').hidden", 'all loaded resources after clearing search');
    assert(resourceRequests.length === 3, 'Clearing the search should not fetch again.');

    // Available = capacity - held on every loaded row.
    const badRows = await evaluate("[...document.querySelectorAll('#resources-list tr')].filter((row) => Number(row.children[2].textContent) - Number(row.children[3].textContent) !== Number(row.children[4].textContent)).length");
    assert(badRows === 0, `${badRows} rows have an Available cell that is not capacity minus held.`);
    assert(await evaluate("[...document.querySelectorAll('#resources-table th')].map((th) => th.textContent).join() === 'Key,Description,Capacity,Held,Available,ID,Actions'"), 'The table should list Available after Held.');
    assert(await evaluate("[...document.querySelectorAll('#resources-list tr')].every((row) => row.querySelectorAll('.row-actions button').length === 3)"), 'Every row should keep its history, edit and delete actions.');

    // Each sort option, ties broken by key.
    await setControls({ sort: 'key' });
    await expectRows(400, ['alpha:db|Primary database|3|1|2', 'beta:device|Test phone|1|1|0', 'delta:bench|Bench|5|5|0', 'Epsilon:lab|Lab|2|0|2', 'gamma:runner|Build runners for ALPHA|10|4|6', 'zeta:gpu|No description|4|2|2', 'zz-filler:00006|Bulk filler|1|1|0'], 'sort by key');
    await setControls({ sort: 'capacity' });
    await expectRows(400, ['gamma:runner|Build runners for ALPHA|10|4|6', 'delta:bench|Bench|5|5|0', 'zeta:gpu|No description|4|2|2', 'alpha:db|Primary database|3|1|2', 'Epsilon:lab|Lab|2|0|2', 'beta:device|Test phone|1|1|0'], 'sort by capacity');
    await setControls({ sort: 'held' });
    await expectRows(400, ['delta:bench|Bench|5|5|0', 'gamma:runner|Build runners for ALPHA|10|4|6', 'zeta:gpu|No description|4|2|2', 'alpha:db|Primary database|3|1|2', 'beta:device|Test phone|1|1|0'], 'sort by held');
    assert((await rows(400)).at(-1) === 'Epsilon:lab|Lab|2|0|2', 'Sorting by held should put the resource with nothing held last.');
    await setControls({ sort: 'available' });
    await expectRows(400, ['gamma:runner|Build runners for ALPHA|10|4|6', 'alpha:db|Primary database|3|1|2', 'Epsilon:lab|Lab|2|0|2', 'zeta:gpu|No description|4|2|2', 'beta:device|Test phone|1|1|0'], 'sort by available');

    // The free-capacity filter hides fully held resources, alone and with search and sort.
    await setControls({ free: true });
    await expectRows(4, ['alpha:db|Primary database|3|1|2', 'Epsilon:lab|Lab|2|0|2', 'gamma:runner|Build runners for ALPHA|10|4|6', 'zeta:gpu|No description|4|2|2'], 'free capacity only');
    await setControls({ free: true, sort: 'available' });
    await expectRows(4, ['gamma:runner|Build runners for ALPHA|10|4|6', 'alpha:db|Primary database|3|1|2', 'Epsilon:lab|Lab|2|0|2', 'zeta:gpu|No description|4|2|2'], 'free capacity sorted by available');
    await setControls({ free: true, search: 'alpha' });
    await expectRows(2, ['alpha:db|Primary database|3|1|2', 'gamma:runner|Build runners for ALPHA|10|4|6'], 'free capacity with search');
    await setControls({ free: true, search: 'beta' });
    await waitPage("document.querySelectorAll('#resources-list tr').length === 0", 'a fully held resource hidden by the filter');
    await setControls({});
    await waitPage("document.querySelectorAll('#resources-list tr').length === 400", 'every resource after clearing the controls');

    // Mobile width: the page does not overflow; the table scrolls inside its wrapper.
    await send('Emulation.setDeviceMetricsOverride', { width: 390, height: 800, deviceScaleFactor: 1, mobile: true });
    const layout = await evaluate(`(() => {
      const wrap = document.querySelector('#resources-table').parentElement, toolbar = document.querySelector('.resource-toolbar');
      return { page: document.documentElement.scrollWidth, view: window.innerWidth, scrolls: wrap.scrollWidth > wrap.clientWidth, overflow: getComputedStyle(wrap).overflowX, toolbar: toolbar.getBoundingClientRect().right <= window.innerWidth };
    })()`);
    assert(layout.page <= layout.view && layout.scrolls && layout.overflow === 'auto' && layout.toolbar, `Mobile layout is wrong: ${JSON.stringify(layout)}`);
    await send('Emulation.clearDeviceMetricsOverride');

    // The cap: more than 2,000 resources stops at 2,000 and says so.
    total = 2100;
    resourceRequests.length = 0;
    await evaluate("document.querySelector('#refresh-resources').click()");
    await waitPage("document.querySelectorAll('#resources-list tr').length === 50", 'a fresh first page');
    assert(await evaluate("document.querySelector('#resources-note').hidden"), 'No note before a search needs every resource.');
    await setControls({ sort: 'capacity' });
    await waitPage("!document.querySelector('#resources-note').hidden && document.querySelectorAll('#resources-list tr').length === 2000", 'the capped table').catch(async (error) => { throw new Error(`${error.message}: ${JSON.stringify(await evaluate("[document.querySelectorAll('#resources-list tr').length, document.querySelector('#resources-note').hidden, document.querySelector('#resources-state').textContent]"))} after ${JSON.stringify(resourceRequests)}`); });
    const note = await evaluate("document.querySelector('#resources-note').textContent");
    assert(note.includes('2,000'), `The cap note should name the cap, saw "${note}".`);
    assert(resourceRequests.slice(1).every((request) => request.limit <= 200) && resourceRequests.slice(1).reduce((sum, request) => sum + request.limit, 50) === 2000, `Fetching should stop at 2,000, saw ${JSON.stringify(resourceRequests)}`);
    assert(await evaluate("document.querySelector('#load-more-resources').hidden"), 'Load more should be hidden once the cap is reached.');
    const fetched = resourceRequests.length;
    await setControls({ sort: 'held', search: 'o' });
    await sleep(300);
    assert(resourceRequests.length === fetched, 'A capped list must not fetch again.');
    console.log('PASS: search (key, description, case-insensitive), all four sorts, the free-capacity filter, the Available column, the mobile layout and the 2,000-resource cap note.');
  } finally {
    socket?.close();
    if (chrome.pid && chrome.exitCode === null) { const exited = once(chrome, 'exit'); chrome.kill('SIGTERM'); await exited; }
    await new Promise((done) => server.close(done));
    await rm(profile, { recursive: true, force: true });
    if (launchError) throw launchError;
  }
}

main().catch((error) => { console.error(`FAIL: ${error.message}`); process.exitCode = 1; });
