// Browser test of the task page's evidence packet export.
//
//   CHROME_BIN=/path/to/chrome [CHROME_ARGS="--no-sandbox"] node scripts/test_evidence_packet.mjs
//
// A fixture server stands in for the coordinator. The test opens a reviewed and integrated task, a task
// with no submission and a task whose history is paged, exports each packet, and reads the downloaded
// Blobs. It requires the packet to carry the criteria with evidence, the candidate identity, the review
// decisions and findings, the integration result with its check job IDs, and the artifact SHA-256 values;
// to contain none of the credentials, session identifiers, reporter IDs or headers planted in the fixture
// records; to flag truncated history; and to make at most three history requests per export.
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
const actor = { id: 'op-1', name: 'Operator', role: 'admin', kind: 'human', session_id: 'SECRET-BROWSER-SESSION' };
const SECRETS = ['SECRET-BROWSER-SESSION', 'SECRET-REPORTER-ID', 'SECRET-AGENT-SESSION', 'SECRET-REVIEWER-SESSION', 'SECRET-PRODUCER', 'SECRET-OWNER', 'SECRET-TOKEN', 'SECRET-PROOF', 'fixture-csrf', 'x-csrf-token', 'bearer ', 'set-cookie', 'idempotency-key'];
const SHA = 'a'.repeat(64), REVISION = '1234567890abcdef1234567890abcdef12345678', TREE = 'fedcba0987654321fedcba0987654321fedcba09', RESULT = 'abcdefabcdefabcdefabcdefabcdefabcdefabcd';
const CRITERIA = ['The export holds every criterion', 'The export holds no credential'];

const baseTask = (id, title, extra = {}) => ({
  id, project_id: 'p1', title, description: 'Fixture task.', revision: 6, kind: 'code', lifecycle: 'open', work_status: 'ready', priority: 2,
  acceptance_criteria: CRITERIA, attempts: [], checkpoints: [], job_evidence: { jobs: [], reservations: [] }, workflow: { activities: [], submission: null }, ...extra,
});
const submission = {
  id: 'sub-1', task_id: 'task-done', attempt_id: 'attempt-secret', kind: 'code', task_revision: 6, project_policy_revision: 4, workflow_policy_revision: 2,
  summary: 'Implemented the export.', handoff: 'Nothing left.', canonical_repository_key: 'example.invalid/repo', candidate_remote: 'example.invalid/repo', candidate_ref: 'refs/agentc/candidates/task-done/sub-1',
  repository: 'https://example.invalid/repo.git', target_branch: 'main', base_revision: 'b'.repeat(40), candidate_revision: REVISION, candidate_tree: TREE, created_by: 'SECRET-OWNER', created_at: '2026-11-01T00:00:00Z', superseded_at: null,
  acceptance_evidence: [{ criterion: CRITERIA[0], evidence: 'Ran the fixture export and read both files.' }, { criterion: CRITERIA[1], evidence: 'Grepped the export for credentials.' }], artifact_ids: ['art-1'], lessons: [],
};
const tasks = {
  'task-done': baseTask('task-done', 'Reviewed and integrated task', { lifecycle: 'done', work_status: 'done', workflow: { phase: 'done', submission, activities: [{ id: 'act-r', kind: 'agent_review', attempt: { owner_id: 'SECRET-OWNER', session_id: 'SECRET-AGENT-SESSION' } }], next_actions: [] } }),
  'task-empty': baseTask('task-empty', 'Task without a submission'),
  'task-paged': baseTask('task-paged', 'Task with more history', { workflow: { phase: 'review', submission: { ...submission, id: 'sub-p', task_id: 'task-paged' }, activities: [] } }),
};
const activity = (id, kind, state) => ({ id, subject_task_id: 'task-done', submission_id: 'sub-1', activity_task_id: `${id}-task`, kind, slot: 0, state, created_at: '2026-11-01T01:00:00Z', completed_at: '2026-11-01T02:00:00Z', canceled_at: null });
const reviewRecord = {
  activity: activity('act-r', 'agent_review', 'done'),
  decision: { attempt_id: 'attempt-secret', reviewer_id: 'SECRET-OWNER', reviewer_session_id: 'SECRET-REVIEWER-SESSION', decision: 'approved', summary: 'Looks correct and complete.', created_at: '2026-11-01T02:00:00Z' },
  findings: [{ id: 'finding-1', severity: 'advisory', remedy: 'Consider a shorter filename.', evidence: 'See the packet name.', created_at: '2026-11-01T02:00:00Z' }],
};
const integrationRecord = {
  activity: activity('act-i', 'integration', 'done'), authorization: null, authorization_history: [],
  hold: { id: 'hold-1', canonical_repository_key: 'example.invalid/repo', target_branch: 'main', state: 'released', acquired_by: 'SECRET-OWNER', acquired_at: '2026-11-01T03:00:00Z', released_at: '2026-11-01T04:00:00Z', released_by: 'SECRET-OWNER', release_reason: 'published' },
  publication_intent: { submission_id: 'sub-1', attempt_id: 'attempt-secret', observed_target_revision: 'c'.repeat(40), observed_target_tree: 'd'.repeat(40), result_revision: RESULT, result_tree: TREE, created_by: 'SECRET-OWNER', created_at: '2026-11-01T03:30:00Z' },
  integration_result: { submission_id: 'sub-1', attempt_id: 'attempt-secret', publication_state: 'published', observed_target_revision: 'c'.repeat(40), result_revision: RESULT, result_tree: TREE, check_job_ids: ['job-fmt', 'job-test'], summary: 'Published after checks.', reported_by: 'SECRET-OWNER', created_at: '2026-11-01T04:00:00Z' },
  check_jobs: ['job-fmt', 'job-test'].map((id) => ({ id, producer_id: 'SECRET-PRODUCER', attempt_id: 'attempt-secret', runner_instance_id: 'SECRET-PROOF', reporter: { id: 'SECRET-REPORTER-ID' }, reporter_id: 'SECRET-REPORTER-ID', label: id === 'job-fmt' ? 'format' : 'tests', state: 'succeeded', source_revision: RESULT, source_tree: TREE, exit_code: 0, inputs_unchanged: true, summary: 'passed', check_identity: id, created_at: '2026-11-01T03:40:00Z' })),
  publication_reconciliation: null,
};
const artifactRecord = { id: 'art-1', kind: 'upload', task_id: 'task-done', job_id: null, display_name: 'report.txt', media_type: 'text/plain', size_bytes: 321, sha256: SHA, external_url: 'https://user:SECRET-TOKEN@example.invalid/x?token=SECRET-TOKEN', state: 'finalized', created_by: 'SECRET-OWNER', created_at: '2026-11-01T00:30:00Z', finalized_at: '2026-11-01T00:31:00Z', retention_until: '2099-01-01T00:00:00Z', pinned: false, deleted_at: null };
const pageOf = (taskId, kind) => {
  if (taskId === 'task-done') return { items: [{ task_id: taskId, relation: 'workflow_activity', record: { reviews: reviewRecord, integrations: integrationRecord, artifacts: artifactRecord }[kind] }], next_cursor: null };
  if (taskId === 'task-paged') return { items: kind === 'reviews' ? [{ task_id: taskId, relation: 'workflow_activity', record: reviewRecord }] : [], next_cursor: kind === 'reviews' ? 'cursor-2' : null };
  return { items: [], next_cursor: null };
};

const historyRequests = [];
const json = (response, data) => { response.writeHead(200, { 'Content-Type': 'application/json', 'Cache-Control': 'no-store' }); response.end(JSON.stringify({ data })); };
async function fixtureServer() {
  const files = { '/': ['web/index.html', 'text/html; charset=utf-8'], '/app.js': ['web/app.js', 'text/javascript; charset=utf-8'], '/style.css': ['web/style.css', 'text/css; charset=utf-8'] };
  const server = createServer(async (request, response) => {
    const url = new URL(request.url, 'http://fixture.invalid');
    if (url.pathname === '/api/v1/me') return json(response, { actor, csrf_token: 'fixture-csrf' });
    if (url.pathname === '/api/v1/projects') return json(response, { items: [{ id: 'p1', name: 'Alpha', target_branch: 'main' }] });
    if (url.pathname === '/api/v1/projects/p1/orientation') return json(response, { project: { id: 'p1', name: 'Alpha', policy_revision: 4, review_mode: 'either', recovery_mode: 'agent', lease_seconds: 600, rules: '' }, policy_revision: 4 });
    if (url.pathname === '/api/v1/projects/p1/tasks') return json(response, { items: Object.values(tasks), next_cursor: null });
    const history = url.pathname.match(/^\/api\/v1\/projects\/p1\/tasks\/([^/]+)\/history$/);
    if (history) {
      const kind = url.searchParams.get('kind');
      // Every read is logged; the test slices out the reads one export makes.
      historyRequests.push({ task: history[1], kind, limit: url.searchParams.get('limit') });
      return json(response, pageOf(history[1], kind));
    }
    const task = url.pathname.match(/^\/api\/v1\/projects\/p1\/tasks\/([^/]+)$/);
    if (task && tasks[task[1]]) return json(response, tasks[task[1]]);
    const file = files[url.pathname];
    if (!file) { response.writeHead(404); response.end(); return; }
    response.writeHead(200, { 'Content-Type': file[1], 'Cache-Control': 'no-store' });
    response.end(await readFile(join(root, file[0])));
  });
  server.listen(0, '127.0.0.1');
  await once(server, 'listening');
  return server;
}
async function unusedPort() {
  const probe = net.createServer(); probe.listen(0, '127.0.0.1'); await once(probe, 'listening');
  const { port } = probe.address(); await new Promise((done) => probe.close(done)); return port;
}
async function waitFor(fetchUrl, predicate, description) {
  const deadline = Date.now() + 10000;
  while (Date.now() < deadline) {
    try { const response = await fetch(fetchUrl); if (response.ok) { const value = await response.json(); if (predicate(value)) return value; } } catch (_) { /* Chrome is still starting. */ }
    await sleep(50);
  }
  throw new Error(`Timed out waiting for ${description}`);
}

async function main() {
  const server = await fixtureServer(), serverPort = server.address().port, debugPort = await unusedPort();
  const profile = await mkdtemp(join(tmpdir(), 'agent-coordinator-packet-test-'));
  const executable = process.env.CHROME_BIN || (process.platform === 'win32' ? 'C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe' : 'chromium');
  const chrome = spawn(executable, ['--headless=new', '--disable-gpu', '--no-first-run', '--no-default-browser-check', '--disable-background-networking', ...(process.env.CHROME_ARGS || '').split(/\s+/).filter(Boolean), `--remote-debugging-port=${debugPort}`, `--user-data-dir=${profile}`], { stdio: 'ignore', windowsHide: true });
  let socket, launchError;
  chrome.on('error', (error) => { launchError = error; });
  try {
    const version = await waitFor(`http://127.0.0.1:${debugPort}/json/version`, (value) => Boolean(value.webSocketDebuggerUrl), 'headless Chrome');
    const connect = async (url) => {
      const connection = new WebSocket(url); await once(connection, 'open'); let nextId = 1; const pending = new Map();
      connection.addEventListener('message', ({ data }) => { const message = JSON.parse(data); const callback = pending.get(message.id); if (callback) { pending.delete(message.id); callback(message); } });
      const send = (method, params = {}) => new Promise((resolveMessage, rejectMessage) => {
        const id = nextId++; const timer = setTimeout(() => { pending.delete(id); rejectMessage(new Error(`Timed out calling ${method}`)); }, 10000);
        pending.set(id, (message) => { clearTimeout(timer); message.error ? rejectMessage(new Error(message.error.message)) : resolveMessage(message.result); });
        connection.send(JSON.stringify({ id, method, params }));
      });
      return { connection, send };
    };
    let connection = await connect(version.webSocketDebuggerUrl);
    const target = await connection.send('Target.createTarget', { url: 'about:blank' });
    connection.connection.close();
    const list = await waitFor(`http://127.0.0.1:${debugPort}/json/list`, (items) => items.find((item) => item.id === target.targetId)?.webSocketDebuggerUrl, 'headless Chrome page target');
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
    // Downloads are captured as the app clicks its download link: the Blob is read before the app revokes it.
    await send('Page.addScriptToEvaluateOnNewDocument', { source: `
      window.__downloads = []; window.__revoked = [];
      const revoke = URL.revokeObjectURL.bind(URL);
      URL.revokeObjectURL = (url) => { window.__revoked.push(url); revoke(url); };
      HTMLAnchorElement.prototype.click = function () {
        if (!this.download) return;
        const entry = { name: this.download, url: this.href, text: null };
        window.__downloads.push(entry);
        fetch(this.href).then((response) => response.text()).then((value) => { entry.text = value; });
      };
    ` });
    await send('Page.navigate', { url: `http://127.0.0.1:${serverPort}/` });
    await waitPage("document.querySelector('.project-open')", 'project list');
    await evaluate("document.querySelector('.project-open').click()");
    await waitPage("!document.querySelector('#tasks-view').hidden", 'task list');

    // Opens a task through its deep link, as a bookmarked or shared URL would.
    const openTask = async (id, title) => {
      await evaluate(`location.hash = '#/projects/p1/task/${id}'`);
      await waitPage(`!document.querySelector('#task-detail-content').hidden && document.querySelector('#task-detail-heading').textContent === ${JSON.stringify(title)}`, `${title} detail`);
    };
    const exportPacket = async (format) => {
      await evaluate(`window.__downloads = []; window.__revoked = []; document.querySelector('#evidence-packet-format').value = ${JSON.stringify(format)}`);
      const before = historyRequests.length;
      await evaluate("document.querySelector('#export-evidence-packet').click()");
      const wanted = format === 'both' ? 2 : 1;
      await waitPage(`window.__downloads.length === ${wanted} && window.__downloads.every((d) => d.text !== null) && !document.querySelector('#export-evidence-packet').disabled`, 'packet download');
      const downloads = await evaluate('window.__downloads.map((d) => ({ name: d.name, text: d.text }))');
      const requests = historyRequests.slice(before);
      assert(requests.length <= 3, `The export made ${requests.length} history requests, not at most three.`);
      return { downloads, requests };
    };

    // ---- Reviewed and integrated task ------------------------------------------------------------
    await openTask('task-done', 'Reviewed and integrated task');
    assert(await evaluate("document.querySelector('label[for=evidence-packet-format]').textContent.trim().startsWith('Format') && document.querySelector('#export-evidence-packet').textContent.trim() === 'Export evidence packet'"), 'The export controls lack visible labels.');
    await evaluate("document.querySelector('#export-evidence-packet').focus()");
    assert(await evaluate("document.activeElement.id") === 'export-evidence-packet' && await evaluate("getComputedStyle(document.activeElement).outlineStyle !== 'none'"), 'The export button is not keyboard-focusable with a visible outline.');
    historyRequests.length = 0;
    const done = await exportPacket('both');
    assert(done.requests.length === 3 && ['reviews', 'integrations', 'artifacts'].every((kind) => done.requests.some((r) => r.kind === kind && r.limit === '50')), `Unexpected history requests: ${JSON.stringify(done.requests)}`);
    const jsonFile = done.downloads.find((d) => d.name.endsWith('.json')), mdFile = done.downloads.find((d) => d.name.endsWith('.md'));
    assert(jsonFile && mdFile && jsonFile.name === 'evidence-task-done-r6.json', `Unexpected downloads: ${JSON.stringify(done.downloads.map((d) => d.name))}`);
    const packet = JSON.parse(jsonFile.text);
    assert(packet.schema_version === 1 && /^\d{4}-/.test(packet.generated_at) && packet.service_origin === `http://127.0.0.1:${serverPort}`, 'Packet header is wrong.');
    assert(packet.project.id === 'p1' && packet.task.id === 'task-done' && packet.task.revision === 6 && packet.submission_exists === true && packet.statement === null, 'Packet task identity is wrong.');
    assert(packet.criteria.length === 2 && packet.criteria.every((row, index) => row.criterion === CRITERIA[index] && row.evidence && row.listed_on_task), 'Packet lacks criteria with evidence.');
    assert(packet.criteria[0].evidence === 'Ran the fixture export and read both files.', 'Packet evidence differs from the submission.');
    assert(packet.submission.id === 'sub-1' && packet.submission.task_revision === 6 && packet.submission.source.candidate_revision === REVISION && packet.submission.source.candidate_tree === TREE && packet.submission.source.candidate_ref === submission.candidate_ref, 'Packet lacks the candidate revision, tree and ref.');
    assert(packet.reviews.length === 1 && packet.reviews[0].decision.decision === 'approved' && packet.reviews[0].findings[0].remedy === 'Consider a shorter filename.', 'Packet lacks the review decision and findings.');
    assert(packet.integrations.length === 1 && packet.integrations[0].result.publication_state === 'published' && packet.integrations[0].result.result_revision === RESULT && JSON.stringify(packet.integrations[0].result.check_job_ids) === '["job-fmt","job-test"]' && packet.integrations[0].check_jobs.length === 2, 'Packet lacks the integration result and check job IDs.');
    assert(packet.artifacts.length === 1 && packet.artifacts[0].sha256 === SHA && packet.artifacts[0].size_bytes === 321 && packet.artifacts[0].availability === 'available' && packet.artifacts[0].retention_until === '2099-01-01T00:00:00Z', 'Packet lacks the artifact SHA-256, size, availability and retention.');
    assert(packet.truncation.any === false && packet.truncation.reviews === false, 'A complete history was flagged as truncated.');
    for (const needle of [REVISION, TREE, RESULT, SHA, 'job-fmt', 'job-test', CRITERIA[0], 'Ran the fixture export and read both files.', 'Looks correct and complete.', 'Consider a shorter filename.', 'report.txt', 'task-done']) assert(mdFile.text.includes(needle), `The Markdown packet lacks ${needle}.`);
    for (const secret of SECRETS) for (const file of done.downloads) assert(!file.text.toLowerCase().includes(secret.toLowerCase()), `${file.name} contains ${secret}.`);
    assert(!jsonFile.text.includes('external_url') && !jsonFile.text.includes('example.invalid/x'), 'Packet carries an artifact link that can hold a credential.');
    await waitPage('window.__revoked.length === 2', 'download URL revocation');
    assert(await evaluate("window.__revoked.every((url) => window.__downloads.some((d) => d.url === url))"), 'Download URLs were not revoked.');
    assert((await evaluate("document.querySelector('#evidence-packet-summary').textContent")).includes('1 review activity'), 'The export summary was not shown.');
    assert((await exportPacket('json')).downloads.length === 1 && (await exportPacket('markdown')).downloads[0].name.endsWith('.md'), 'The single-format choices did not download one file.');

    // ---- Task with no submission -----------------------------------------------------------------
    await openTask('task-empty', 'Task without a submission');
    assert(await evaluate("document.querySelector('#evidence-packet-summary').textContent") === '', 'The previous task\'s export summary stayed on screen.');
    const empty = await exportPacket('both');
    const emptyPacket = JSON.parse(empty.downloads.find((d) => d.name.endsWith('.json')).text), emptyMarkdown = empty.downloads.find((d) => d.name.endsWith('.md')).text;
    assert(emptyPacket.submission_exists === false && emptyPacket.submission === null && emptyPacket.statement === 'No submission exists for this task.', 'The no-submission packet does not say so.');
    assert(emptyPacket.criteria.length === 2 && emptyPacket.criteria.every((row) => row.evidence === null) && emptyPacket.reviews.length === 0 && emptyPacket.integrations.length === 0 && emptyPacket.artifacts.length === 0, 'The no-submission packet is not a valid empty packet.');
    assert(emptyMarkdown.includes('No submission exists for this task.') && emptyMarkdown.includes('none submitted'), 'The no-submission Markdown does not say so.');

    // ---- Truncated history -----------------------------------------------------------------------
    await openTask('task-paged', 'Task with more history');
    const paged = await exportPacket('both');
    const pagedPacket = JSON.parse(paged.downloads.find((d) => d.name.endsWith('.json')).text);
    assert(pagedPacket.truncation.reviews === true && pagedPacket.truncation.any === true && pagedPacket.truncation.integrations === false, 'Truncated review history was not flagged.');
    assert(paged.downloads.find((d) => d.name.endsWith('.md')).text.includes('## Truncated history'), 'The Markdown packet does not flag truncated history.');
    assert((await evaluate("document.querySelector('#evidence-packet-summary .inline-alert').textContent")).includes('Truncated history: reviews'), 'The page does not warn about truncated history.');

    // ---- Mobile width ----------------------------------------------------------------------------
    await send('Emulation.setDeviceMetricsOverride', { width: 360, height: 700, deviceScaleFactor: 1, mobile: true });
    assert(await evaluate("(() => { const card = document.querySelector('#evidence-packet-card').getBoundingClientRect(); const button = document.querySelector('#export-evidence-packet').getBoundingClientRect(); return card.right <= innerWidth && button.right <= innerWidth && button.width >= 100; })()"), 'The export controls overflow at mobile width.');
    console.log('PASS: headless Chrome verified the evidence packet for a reviewed and integrated task, a task with no submission and truncated history: contents, no credentials, at most three history requests, Blob URL revocation, keyboard focus and mobile width.');
  } finally {
    socket?.close();
    if (chrome.pid && chrome.exitCode === null) { const exited = once(chrome, 'exit'); chrome.kill('SIGTERM'); await exited; }
    await new Promise((done) => server.close(done));
    for (let attempt = 0; attempt < 3; attempt += 1) {
      try { await rm(profile, { recursive: true, force: true }); break; } catch (error) { if (attempt === 2 || error.code !== 'ENOTEMPTY') throw error; await sleep(100); }
    }
    if (launchError) throw launchError;
  }
}

main().catch((error) => { console.error(`FAIL: ${error.message}`); process.exitCode = 1; });
