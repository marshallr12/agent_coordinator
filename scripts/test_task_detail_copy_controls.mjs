import { createServer } from 'node:http';
import { mkdtemp, readFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { spawn } from 'node:child_process';
import { once } from 'node:events';
import { createHash } from 'node:crypto';
import net from 'node:net';

const root = resolve(import.meta.dirname, '..');
const task = {
  id: 'task-copy-id-123',
  title: 'Copy this task name exactly',
  description: 'Synthetic browser-fixture task.',
  revision: 7,
  kind: 'code',
  lifecycle: 'open',
  work_status: 'ready',
  acceptance_criteria: [],
  attempts: [],
  checkpoints: [],
  workflow: { activities: [] },
  job_evidence: { jobs: [], reservations: [] },
};
const tasks = Array.from({ length: 30 }, (_, index) => ({
  ...task,
  id: index ? `task-copy-id-${index}` : task.id,
  title: index ? `Fixture task ${index + 1}` : task.title,
}));
const uploadedAttachments = new Map();
const attachmentReservations = new Map();
const attachmentBytes = new Map();
let failFirstAttachmentPut = true;
const attachmentUploadKeys = [];
const archivedTask = { ...task, id: 'archived-fixture-task', title: 'Archived fixture task', lifecycle: 'canceled', archived_at: '2026-09-23T00:00:00Z' };
const fixtureActor = { id: 'fixture-operator', name: 'Fixture operator', role: 'admin', kind: 'human', session_id: 'fixture-browser' };
const policyProject = {
  id: 'fixture-project', name: 'Fixture project', target_branch: 'main', policy_revision: 7, review_mode: 'either', recovery_mode: 'agent',
  lease_seconds: 3600, rules: 'Run the gate.', agent_rule_editing: true, automatic_integration: true, allow_subagent_reviews: true, integration_owner: 'integrator',
};
const policyPatches = [];
let integrationHeld = true;
const fixtureReport = (id, kind, extra = {}) => ({
  id, project_id: 'fixture-project', kind, task_id: task.id, submission_id: 'fixture-submission', result_id: `result-${id}`, dedupe_key: id,
  details: { reason: `Synthetic ${kind}` }, requires_human: false, created_at: '2026-10-06T00:00:00Z', resolved_at: null, resolved_by: null,
  resolution_note: null, decision: null, allowed: false, ...extra,
});
// Stored oldest first, like rowid order; the list route serves newest first.
const reports = [
  ...Array.from({ length: 50 }, (_, index) => fixtureReport(`old-report-${index}`, 'fix_target', { resolved_at: '2026-10-01T00:00:00Z', resolved_by: 'fixture-operator', resolution_note: 'Fixed earlier.' })),
  fixtureReport('flaky-report', 'flaky'),
  fixtureReport('privilege-report', 'privilege_gate', { requires_human: true, details: { paths: ['.github/workflows/ci.yml'] } }),
];
const reportQueries = [];
let failNextReportPage = false;
const reportResolutions = [];
// Connected agent sessions: a subagent reviewer, a plain CLI worker and an idle session.
const agentSessions = [
  { session_id: 'session-reviewer', principal: { id: 'p-builder', name: 'builder-agent', kind: 'agent' }, harness: 'claude-code', workstation_id: 'ws-laptop', capabilities: [], subagent: { name: 'reviewer-1', parent_session_id: 'session-parent' }, credential_id: 'cred-1', started_at: '2026-10-05T08:00:00Z', last_activity_at: '2026-10-05T10:00:00Z',
    held_attempts: [{ attempt_id: 'attempt-review', task_id: 'review-task-id', task_title: 'Review the fixture change', generation: 1, mode: 'work', activity_kind: 'agent_review', subject_task_id: task.id, expires_at: '2026-10-05T11:00:00Z', last_heartbeat_at: '2026-10-05T10:00:00Z', lease_expired: false }] },
  { session_id: 'session-worker', principal: { id: 'p-cli', name: 'cli-agent', kind: 'agent' }, harness: 'agent-coordinator-cli', workstation_id: 'ws-build-server-with-a-very-long-identifier-that-must-wrap', capabilities: [], subagent: null, credential_id: 'cred-2', started_at: '2026-10-05T07:00:00Z', last_activity_at: '2026-10-05T09:30:00Z',
    held_attempts: [{ attempt_id: 'attempt-work', task_id: task.id, task_title: task.title, generation: 2, mode: 'work', activity_kind: null, subject_task_id: null, expires_at: '2026-10-05T10:30:00Z', last_heartbeat_at: '2026-10-05T09:30:00Z', lease_expired: true }] },
  { session_id: 'session-idle', principal: { id: 'p-idle', name: 'idle-agent', kind: 'agent' }, harness: 'agent-coordinator-cli', workstation_id: 'ws-idle', capabilities: [], subagent: null, credential_id: 'cred-3', started_at: '2026-10-04T07:00:00Z', last_activity_at: '2026-10-04T08:00:00Z', held_attempts: [] },
];
const agentSessionQueries = [];
let agentSessionsMode = 'normal';

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

function refuse(response, status, code, message) {
  response.writeHead(status, { 'Content-Type': 'application/json', 'Cache-Control': 'no-store' });
  response.end(JSON.stringify({ error: { code, message } }));
}

async function readJson(request) {
  let body = '';
  for await (const chunk of request) body += chunk;
  return JSON.parse(body);
}

// Serves the project policy routes: the first PATCH meets a held integration.
async function policyRoute(request, response) {
  assert(request.headers['x-csrf-token'] === 'fixture-csrf', 'Policy change omitted the browser CSRF token.');
  const body = await readJson(request);
  policyPatches.push(body);
  if (integrationHeld) { integrationHeld = false; return refuse(response, 409, 'policy_hold_conflict', 'Finish or reconcile the held integration before changing its policy.'); }
  if (body.expected_revision !== policyProject.policy_revision) return refuse(response, 409, 'revision_conflict', 'Read the current project policy before editing it.');
  Object.assign(policyProject, { integration_owner: body.integration_owner ?? policyProject.integration_owner, policy_revision: policyProject.policy_revision + 1 });
  return json(response, policyProject);
}

// Lists reports newest first after the `before` cursor, like the service.
function reportListRoute(url, response) {
  reportQueries.push(Object.fromEntries(url.searchParams));
  if (failNextReportPage && url.searchParams.has('before')) { failNextReportPage = false; return refuse(response, 503, 'unavailable', 'Reports are temporarily unavailable.'); }
  const open = url.searchParams.get('open') === 'true', limit = Number(url.searchParams.get('limit') || 200);
  const newest = [...reports].reverse(), before = url.searchParams.get('before');
  const start = before ? newest.findIndex((report) => report.id === before) + 1 : 0;
  const items = newest.slice(start).filter((report) => !open || !report.resolved_at).slice(0, limit);
  return json(response, { project_id: 'fixture-project', items, next_before: items.length === limit ? items.at(-1).id : null });
}

// Lists connected sessions: the idle one only in the all-sessions window; or empty, or a failure, on demand.
function agentSessionsRoute(url, response) {
  agentSessionQueries.push(Object.fromEntries(url.searchParams));
  if (agentSessionsMode === 'fail') return refuse(response, 503, 'unavailable', 'Sessions are temporarily unavailable.');
  const hours = url.searchParams.get('active_within_hours');
  const items = agentSessionsMode === 'empty' ? [] : agentSessions.filter((session) => hours === '0' || session.held_attempts.length);
  return json(response, { project_id: 'fixture-project', active_within_hours: Number(hours), generated_at: '2026-10-05T10:05:00Z', items, truncated: false });
}

// Resolves one open report; a resolved one is refused as the service does.
async function reportResolveRoute(id, request, response) {
  assert(request.headers['x-csrf-token'] === 'fixture-csrf', 'Report resolution omitted the browser CSRF token.');
  const body = await readJson(request), report = reports.find((item) => item.id === id);
  reportResolutions.push({ id, body });
  if (!report) return refuse(response, 404, 'not_found', 'Not found.');
  if (report.resolved_at) return refuse(response, 409, 'report_already_resolved', 'This report was already resolved.');
  Object.assign(report, { resolved_at: '2026-10-06T01:00:00Z', resolved_by: fixtureActor.id, resolution_note: body.note, decision: body.decision ?? null, allowed: body.decision === 'allow' });
  return json(response, report);
}

async function fixtureServer() {
  const files = {
    '/': ['web/index.html', 'text/html; charset=utf-8'],
    '/index.html': ['web/index.html', 'text/html; charset=utf-8'],
    '/app.js': ['web/app.js', 'text/javascript; charset=utf-8'],
    '/style.css': ['web/style.css', 'text/css; charset=utf-8'],
  };
  const server = createServer(async (request, response) => {
    const url = new URL(request.url, 'http://fixture.invalid');
    if (url.pathname === '/api/v1/me') return json(response, { actor: fixtureActor, csrf_token: 'fixture-csrf' });
    if (url.pathname === '/api/v1/projects') return json(response, { items: [{ id: 'fixture-project', name: 'Fixture project', target_branch: 'main' }] });
    if (url.pathname === '/api/v1/projects/fixture-project/orientation') return json(response, { project: policyProject, policy_revision: policyProject.policy_revision });
    if (url.pathname === '/api/v1/projects/fixture-project/policy' && request.method === 'PATCH') return policyRoute(request, response);
    if (url.pathname === '/api/v1/projects/fixture-project/integrator/reports') return reportListRoute(url, response);
    if (url.pathname === '/api/v1/projects/fixture-project/sessions') return agentSessionsRoute(url, response);
    const resolveMatch = url.pathname.match(/^\/api\/v1\/projects\/fixture-project\/integrator\/reports\/([^/]+)\/resolve$/);
    if (resolveMatch && request.method === 'POST') return reportResolveRoute(decodeURIComponent(resolveMatch[1]), request, response);
    if (url.pathname === '/api/v1/projects/fixture-project/tasks') {
      const limit = Number(url.searchParams.get('limit') || 50);
      const start = Number(url.searchParams.get('cursor') || 0);
      const items = tasks.slice(start, start + limit);
      const nextCursor = start + items.length < tasks.length ? String(start + items.length) : null;
      return json(response, { items, next_cursor: nextCursor });
    }
    if (url.pathname === `/api/v1/projects/fixture-project/tasks/${task.id}/history` && url.searchParams.get('kind') === 'artifacts') {
      const items = [...uploadedAttachments.values()].map((artifact) => ({ task_id: task.id, relation: 'subject', record: artifact }));
      return json(response, { items, next_cursor: null });
    }
    if (url.pathname === '/api/v1/projects/fixture-project/artifacts/uploads' && request.method === 'POST') {
      let body = '';
      for await (const chunk of request) body += chunk;
      const input = JSON.parse(body);
      const id = `fixture-artifact-${attachmentReservations.size + 1}`;
      const artifact = { id, project_id: 'fixture-project', task_id: input.task_id, kind: 'upload', display_name: input.filename, media_type: input.media_type, size_bytes: input.size_bytes, sha256: input.sha256, state: 'reserved', availability: 'pending' };
      attachmentReservations.set(id, { artifact, input });
      return json(response, { artifact, upload_path: `/api/v1/projects/fixture-project/artifacts/${id}/content` });
    }
    const artifactMatch = url.pathname.match(/^\/api\/v1\/projects\/fixture-project\/artifacts\/([^/]+)\/content$/);
    if (artifactMatch && request.method === 'PUT') {
      const id = artifactMatch[1], reservation = attachmentReservations.get(id);
      const key = request.headers['idempotency-key'];
      attachmentUploadKeys.push(key);
      const chunks = [];
      for await (const chunk of request) chunks.push(Buffer.from(chunk));
      const bytes = Buffer.concat(chunks);
      if (failFirstAttachmentPut) {
        failFirstAttachmentPut = false;
        response.writeHead(503, { 'Content-Type': 'application/json' });
        response.end(JSON.stringify({ error: { code: 'upload_busy', message: 'Synthetic uncertain upload result.' } }));
        return;
      }
      assert(Boolean(key), 'Binary upload omitted its idempotency key.');
      assert(request.headers['x-csrf-token'] === 'fixture-csrf', 'Binary upload omitted the browser CSRF token.');
      assert(reservation, 'Binary upload had no task artifact reservation.');
      assert(bytes.length === reservation.input.size_bytes, 'Uploaded bytes differ from the reservation size.');
      assert(createHash('sha256').update(bytes).digest('hex') === reservation.input.sha256, 'Uploaded bytes differ from the reserved digest.');
      const artifact = { ...reservation.artifact, state: 'finalized', availability: 'available', finalized_at: new Date().toISOString() };
      uploadedAttachments.set(id, artifact); attachmentBytes.set(id, bytes);
      return json(response, { artifact });
    }
    if (artifactMatch && request.method === 'GET') {
      const artifact = uploadedAttachments.get(artifactMatch[1]), bytes = attachmentBytes.get(artifactMatch[1]);
      if (!artifact || !bytes) { response.writeHead(404); response.end(); return; }
      response.writeHead(200, { 'Content-Type': artifact.media_type, 'Content-Disposition': `attachment; filename="${artifact.display_name}"`, 'X-Content-Type-Options': 'nosniff' });
      response.end(bytes);
      return;
    }
    if (url.pathname === '/api/v1/projects/fixture-project/tasks/archived') return json(response, { items: [archivedTask], next_cursor: null });
    if (url.pathname === `/api/v1/projects/fixture-project/tasks/${task.id}`) return json(response, task);
    if (url.pathname === `/api/v1/projects/fixture-project/tasks/${archivedTask.id}`) return json(response, archivedTask);
    if (url.pathname === '/api/v1/admin/credentials') return json(response, { items: [] });
    if (url.pathname === '/api/v1/admin/agents' && request.method === 'POST') return json(response, { token: 'synthetic-token-for-clipboard-test', name: 'Synthetic agent' });
    const file = files[url.pathname];
    if (!file) { response.writeHead(404); response.end(); return; }
    try {
      response.writeHead(200, { 'Content-Type': file[1], 'Cache-Control': 'no-store' });
      response.end(await readFile(join(root, file[0])));
    } catch (error) {
      response.writeHead(500); response.end(String(error));
    }
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

const OPEN_DIALOG_FORM = "document.querySelector('dialog[open] form')";
const ALERT_TEXT = "document.querySelector('#global-alert').textContent";

// Opens the integration owner dialog and waits for its owner select.
async function openOwnerDialog({ evaluate, waitPage }) {
  await evaluate("document.querySelector('#integration-owner-button').click()");
  await waitPage("document.querySelector('dialog[open] #workflow-integration_owner')", 'integration owner dialog');
}

// Chooses the new owner, fills provenance, optionally confirms, and submits.
async function submitOwner({ evaluate }, owner, confirm) {
  await evaluate(`(() => { const select = document.querySelector('#workflow-integration_owner'); select.value = ${JSON.stringify(owner)}; select.dispatchEvent(new Event('change', { bubbles: true }));
    document.querySelector('#workflow-provenance').value = 'S6 rollback drill'; document.querySelector('#workflow-confirm_owner').checked = ${confirm}; ${OPEN_DIALOG_FORM}.requestSubmit(); })()`);
}

// Integration owner: current owner shown, same owner and missing confirmation
// refused, a held integration explained, then a confirmed switch saved.
async function checkIntegrationOwner(page) {
  const { evaluate, waitPage } = page;
  await waitPage("document.querySelector('#integration-owner-summary').textContent.includes('Integrator service (policy revision 7)')", 'current integration owner');
  await openOwnerDialog(page);
  assert(await evaluate("document.querySelector('#workflow-integration_owner').value") === 'integrator', 'Owner dialog did not start on the current owner.');
  await submitOwner(page, 'integrator', true);
  assert(await evaluate("Boolean(document.querySelector('dialog[open]')) && !document.querySelector('#workflow-integration_owner').validity.valid") && policyPatches.length === 0, 'Owner dialog saved the unchanged owner.');
  await submitOwner(page, 'agent', false);
  assert(await evaluate("document.querySelector('dialog[open]').textContent.includes('from Integrator service to Agents')"), 'Owner confirmation does not name the exact switch.');
  assert(await evaluate("document.querySelector('#workflow-confirm_owner').validity.valueMissing") && policyPatches.length === 0, 'Owner changed without an explicit confirmation.');
  await submitOwner(page, 'agent', true);
  await waitPage(`${ALERT_TEXT}.includes('an integration currently holds the target')`, 'held-integration refusal');
  const [held] = policyPatches;
  assert(held.integration_owner === 'agent' && held.provenance === 'S6 rollback drill' && held.expected_revision === 7, 'Owner change sent the wrong owner, note or revision.');
  assert(held.rules === 'Run the gate.' && held.allow_subagent_reviews === true && held.review_mode === 'either' && held.automatic_integration === true, 'Owner change did not preserve the other policy values.');
  assert(await evaluate("document.querySelector('#integration-owner-summary').textContent.includes('Integrator service')"), 'Refused owner change altered the displayed owner.');
  await openOwnerDialog(page);
  await submitOwner(page, 'agent', true);
  await waitPage(`${ALERT_TEXT}.includes('Integration owner is now Agents') && document.querySelector('#integration-owner-summary').textContent.includes('Agents (policy revision 8)')`, 'saved integration owner');
}

// Opens the resolve dialog of the report card with this id.
async function openResolve({ evaluate, waitPage }, id) {
  await evaluate(`[...document.querySelector('[data-report-id=${JSON.stringify(id)}]').querySelectorAll('button')].find((button) => button.textContent === 'Resolve report').click()`);
  await waitPage("document.querySelector('dialog[open] #workflow-resolution_note')", `resolve dialog for ${id}`);
}

// Resolves the privilege gate: no preselected decision, allow needs a confirmation.
async function checkPrivilegeResolve(page) {
  const { evaluate, waitPage } = page;
  await openResolve(page, 'privilege-report');
  assert(await evaluate("document.querySelector('#workflow-report_decision').value") === '', 'Privilege decision is preselected.');
  assert(await evaluate("document.querySelector('#workflow-confirm_allow').closest('label').hidden"), 'Allow confirmation shown before allow was chosen.');
  await evaluate(`document.querySelector('#workflow-resolution_note').value = 'Workflow change reviewed'; ${OPEN_DIALOG_FORM}.requestSubmit()`);
  assert(await evaluate("Boolean(document.querySelector('dialog[open]')) && document.querySelector('#workflow-report_decision').validity.valueMissing"), 'Privilege gate resolved without a decision.');
  await evaluate(`(() => { const select = document.querySelector('#workflow-report_decision'); select.value = 'allow'; select.dispatchEvent(new Event('change', { bubbles: true })); ${OPEN_DIALOG_FORM}.requestSubmit(); })()`);
  assert(await evaluate("!document.querySelector('#workflow-confirm_allow').closest('label').hidden && document.querySelector('#workflow-confirm_allow').validity.valueMissing"), 'Allow did not require its confirmation.');
  assert(reportResolutions.length === 0, 'An incomplete privilege resolution was sent.');
  await evaluate(`document.querySelector('#workflow-confirm_allow').checked = true; ${OPEN_DIALOG_FORM}.requestSubmit()`);
  await waitPage("!document.querySelector('[data-report-id=\"privilege-report\"]') && document.querySelectorAll('#reports-list .report-card').length === 1", 'resolved privilege gate leaves the open list');
  const [resolution] = reportResolutions;
  assert(resolution.id === 'privilege-report' && resolution.body.decision === 'allow' && resolution.body.note === 'Workflow change reviewed', 'Privilege resolution sent the wrong body.');
}

// Resolves an ordinary report with a note only.
async function checkNoteResolve(page) {
  const { evaluate, waitPage } = page;
  await openResolve(page, 'flaky-report');
  assert(await evaluate("!document.querySelector('#workflow-report_decision')"), 'A non-privilege report offered allow or deny.');
  await evaluate(`document.querySelector('#workflow-resolution_note').value = 'Reran after the runner outage'; ${OPEN_DIALOG_FORM}.requestSubmit()`);
  await waitPage("document.querySelector('#reports-state').textContent.includes('No open reports')", 'empty open-report list');
  assert(!('decision' in reportResolutions[1].body) && reportResolutions[1].body.note === 'Reran after the runner outage', 'Ordinary resolution sent a decision or lost its note.');
}

// A report resolved elsewhere meanwhile: the refusal is explained and the list refreshed.
async function checkAlreadyResolved(page) {
  const { evaluate, waitPage } = page;
  reports.push(fixtureReport('raced-report', 'ruleset_missing', { requires_human: true }));
  await evaluate("document.querySelector('#refresh-reports').click()");
  await waitPage("document.querySelector('[data-report-id=\"raced-report\"]')", 'newly recorded report');
  Object.assign(reports.at(-1), { resolved_at: '2026-10-06T02:00:00Z', resolved_by: 'other-operator', resolution_note: 'Handled elsewhere.' });
  await openResolve(page, 'raced-report');
  await evaluate(`document.querySelector('#workflow-resolution_note').value = 'Late resolution'; ${OPEN_DIALOG_FORM}.requestSubmit()`);
  await waitPage(`${ALERT_TEXT}.includes('already resolved') && !document.querySelector('[data-report-id="raced-report"]')`, 'already-resolved refusal');
}

// All reports with paging; resolved cards show their decision and note.
async function checkReportPaging(page) {
  const { evaluate, waitPage } = page;
  await evaluate("(() => { const filter = document.querySelector('#reports-filter'); filter.value = 'all'; filter.dispatchEvent(new Event('change', { bubbles: true })); })()");
  await waitPage("document.querySelectorAll('#reports-list .report-card').length === 50 && !document.querySelector('#load-more-reports').hidden", 'first page of all reports');
  assert(!('open' in reportQueries.at(-1)) && reportQueries.at(-1).limit === '50', 'All-report listing sent the wrong query.');
  assert(await evaluate("document.querySelector('[data-report-id=\"privilege-report\"]').textContent.includes('Resolved: allowed')"), 'Resolved privilege gate did not show its decision.');
  await checkLoadMoreFailure(page);
  await evaluate("document.querySelector('#load-more-reports').click()");
  await waitPage("document.querySelectorAll('#reports-list .report-card').length === 53 && document.querySelector('#load-more-reports').hidden", 'second page of all reports');
  assert(reportQueries.at(-1).before === reports[3].id, `Next page used the wrong cursor (${reportQueries.at(-1).before}).`);
}

// A failed next page keeps its cards and cursor and offers Load more again.
async function checkLoadMoreFailure(page) {
  const { evaluate, waitPage } = page;
  failNextReportPage = true;
  await evaluate("document.querySelector('#load-more-reports').click()");
  await waitPage("document.querySelector('#reports-state').textContent.includes('temporarily unavailable') && !document.querySelector('#load-more-reports').hidden", 'load-more failure keeps the control');
  assert(await evaluate("document.querySelectorAll('#reports-list .report-card').length") === 50, 'A failed next page dropped the loaded reports.');
  assert(reportQueries.at(-1).before === reports[3].id, 'The failed next page used the wrong cursor.');
}

// Integrator reports: open list, resolves, a raced resolve, paging, return.
async function checkIntegratorReports(page) {
  const { evaluate, waitPage } = page;
  await evaluate("document.querySelector('#integrator-reports-button').click()");
  await waitPage("!document.querySelector('#reports-view').hidden && document.querySelectorAll('#reports-list .report-card').length === 2", 'open integrator reports');
  assert(reportQueries.at(-1).open === 'true' && reportQueries.at(-1).limit === '50', 'Open-report listing sent the wrong query.');
  assert(await evaluate("document.querySelector('#reports-list .report-card').textContent.includes('Privilege gate') && document.querySelector('#reports-list .report-card').textContent.includes('Needs a person')"), 'Newest privilege gate is not first or not marked for a person.');
  await checkPrivilegeResolve(page);
  await checkNoteResolve(page);
  await checkAlreadyResolved(page);
  await checkReportPaging(page);
  await page.send('Emulation.setDeviceMetricsOverride', { width: 390, height: 844, deviceScaleFactor: 1, mobile: true });
  assert(await evaluate("document.documentElement.scrollWidth <= window.innerWidth"), 'Integrator reports overflow at phone width.');
  await page.send('Emulation.clearDeviceMetricsOverride');
  await evaluate("document.querySelector('#back-to-project-settings').click()");
  await waitPage("!document.querySelector('#project-view').hidden", 'return to project settings');
}

// Selects a connected-sessions window and waits for its query.
async function chooseSessionWindow({ evaluate, waitPage }, hours) {
  const before = agentSessionQueries.length;
  await evaluate(`(() => { const select = document.querySelector('#agent-sessions-window'); select.value = ${JSON.stringify(hours)}; select.dispatchEvent(new Event('change', { bubbles: true })); })()`);
  await waitPage(`document.querySelectorAll('#agent-sessions-list .agent-session-card').length === ${hours === '0' ? 3 : 2} || document.querySelector('#agent-sessions-state').textContent.includes('No agent sessions')`, `sessions window ${hours}`);
  assert(agentSessionQueries.length > before && agentSessionQueries.at(-1).active_within_hours === hours, `Window ${hours} sent the wrong query.`);
}

// Connected sessions: cards with held work, window switching, refresh, empty and error states, phone width, return.
async function checkAgentSessions(page) {
  const { evaluate, waitPage } = page;
  await evaluate("document.querySelector('#agent-sessions-button').click()");
  await waitPage("!document.querySelector('#agent-sessions-view').hidden && document.querySelectorAll('#agent-sessions-list .agent-session-card').length === 2", 'connected sessions');
  assert(agentSessionQueries.at(-1).active_within_hours === '24', 'Connected sessions did not default to 24 hours.');
  assert(await evaluate("document.querySelector('#agent-sessions-project-label').textContent") === 'Fixture project', 'Connected sessions did not name the project.');
  const reviewer = await evaluate("document.querySelector('[data-session-id=\"session-reviewer\"]').textContent");
  assert(reviewer.includes('builder-agent') && reviewer.includes('claude-code / subagent reviewer-1') && reviewer.includes('Review the fixture change') && reviewer.includes('Agent Review') && reviewer.includes('ws-laptop'), 'Reviewer session card is incomplete.');
  assert((await evaluate("document.querySelector('[data-session-id=\"session-worker\"]').textContent")).includes(task.title), 'Worker session card omits its held task.');
  assert(await evaluate("document.querySelector('[data-session-id=\"session-worker\"] .held-attempts .status-badge')?.textContent === 'Lease expired' && document.querySelector('[data-session-id=\"session-worker\"]').textContent.includes('needs recovery')"), 'Lapsed lease is not marked on the worker card.');
  assert(await evaluate("!document.querySelector('[data-session-id=\"session-reviewer\"] .held-attempts .status-badge') && document.querySelector('[data-session-id=\"session-reviewer\"]').textContent.includes('expires ')"), 'A live lease was marked expired or lost its expiry.');
  await chooseSessionWindow(page, '0');
  assert((await evaluate("document.querySelector('[data-session-id=\"session-idle\"]').textContent")).includes('Holds no attempts'), 'Idle session does not say it holds nothing.');
  await chooseSessionWindow(page, '1');
  const before = agentSessionQueries.length;
  await evaluate("document.querySelector('#refresh-agent-sessions').click()");
  await waitPage(`document.querySelectorAll('#agent-sessions-list .agent-session-card').length === 2`, 'refreshed sessions');
  assert(agentSessionQueries.length > before && agentSessionQueries.at(-1).active_within_hours === '1', 'Refresh did not re-query the chosen window.');
  await checkAgentSessionStates(page);
  await page.send('Emulation.setDeviceMetricsOverride', { width: 390, height: 844, deviceScaleFactor: 1, mobile: true });
  assert(await evaluate("document.documentElement.scrollWidth <= window.innerWidth"), 'Connected sessions overflow at phone width.');
  await page.send('Emulation.clearDeviceMetricsOverride');
  await evaluate("document.querySelector('#agent-sessions-back').click()");
  await waitPage("!document.querySelector('#project-view').hidden", 'return from connected sessions');
}

// The empty-window message and a server error both show in the state line.
async function checkAgentSessionStates({ evaluate, waitPage }) {
  agentSessionsMode = 'empty';
  await evaluate("document.querySelector('#refresh-agent-sessions').click()");
  await waitPage("document.querySelector('#agent-sessions-state').textContent === 'No agent sessions are connected to this project in this window.' && !document.querySelector('#agent-sessions-list').childElementCount", 'empty sessions window');
  agentSessionsMode = 'fail';
  await evaluate("document.querySelector('#refresh-agent-sessions').click()");
  await waitPage("document.querySelector('#agent-sessions-state').textContent.includes('temporarily unavailable')", 'sessions load failure');
  agentSessionsMode = 'normal';
  await evaluate("document.querySelector('#refresh-agent-sessions').click()");
  await waitPage("document.querySelectorAll('#agent-sessions-list .agent-session-card').length === 2 && document.querySelector('#agent-sessions-state').hidden", 'sessions recover after a failure');
}

// A non-human actor sees neither the owner control nor report resolution.
async function checkAgentActorControls(page, serverPort) {
  const { evaluate, waitPage, send } = page;
  fixtureActor.kind = 'agent';
  try {
    await send('Page.navigate', { url: `http://127.0.0.1:${serverPort}/` });
    await waitPage("document.querySelector('.project-settings-button')", 'agent-actor project list');
    await evaluate("document.querySelector('.project-settings-button').click()");
    await waitPage("!document.querySelector('#project-view').hidden && document.querySelector('#integration-owner-summary').textContent.includes('Current owner')", 'agent-actor settings');
    assert(await evaluate("document.querySelector('#integration-owner-button').hidden"), 'A non-human actor was offered the integration owner control.');
    reports.push(fixtureReport('agent-view-report', 'flaky'));
    await evaluate("document.querySelector('#integrator-reports-button').click()");
    await waitPage("document.querySelector('[data-report-id=\"agent-view-report\"]')", 'agent-actor reports');
    assert(await evaluate("![...document.querySelectorAll('#reports-list button')].some((button) => button.textContent === 'Resolve report')"), 'A non-human actor was offered report resolution.');
  } finally { fixtureActor.kind = 'human'; }
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
  const profile = await mkdtemp(join(tmpdir(), 'agent-coordinator-copy-test-'));
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
    const pageSocket = page.find((item) => item.id === target.targetId).webSocketDebuggerUrl;
    connection = await connect(pageSocket);
    socket = connection.connection;
    const { send } = connection;
    const evaluate = async (expression) => {
      const result = await send('Runtime.evaluate', { expression, awaitPromise: true, returnByValue: true });
      if (result.exceptionDetails) throw new Error(`${result.exceptionDetails.exception?.description || result.exceptionDetails.text} IN: ${expression.slice(0, 300)}`);
      return result.result.value;
    };

    await send('Page.enable');
    await send('Page.addScriptToEvaluateOnNewDocument', { source: `
      window.__copiedTaskDetailValue = null;
      window.__rejectTaskDetailClipboard = false;
      window.__credentialDownloadRequested = false;
      Object.defineProperty(Navigator.prototype, 'clipboard', { configurable: true, value: {
        writeText(value) {
          if (window.__rejectTaskDetailClipboard) return Promise.reject(new DOMException('Denied', 'NotAllowedError'));
          window.__copiedTaskDetailValue = value;
          return Promise.resolve();
        }
      }});
      const anchorClick = HTMLAnchorElement.prototype.click;
      HTMLAnchorElement.prototype.click = function () {
        if (this.id === 'download-credential') { window.__credentialDownloadRequested = true; return; }
        return anchorClick.call(this);
      };
    ` });
    await send('Page.navigate', { url: `http://127.0.0.1:${serverPort}/` });
    const waitPage = async (expression, label) => {
      const deadline = Date.now() + 10000;
      while (Date.now() < deadline) {
        if (await evaluate(expression)) return;
        await new Promise((resolveDelay) => setTimeout(resolveDelay, 30));
      }
      throw new Error(`Timed out waiting for ${label}`);
    };
    await waitPage("document.querySelector('.project-open')", 'project list');
    const press = async (key, code, virtualKey) => {
      await send('Input.dispatchKeyEvent', { type: 'keyDown', key, code, windowsVirtualKeyCode: virtualKey, text: key === 'Enter' ? '\r' : '' });
      await send('Input.dispatchKeyEvent', { type: 'keyUp', key, code, windowsVirtualKeyCode: virtualKey });
    };
    const back = async () => {
      await evaluate("document.querySelector('.nav-item[data-view=overview]').click()");
      await waitPage("!document.querySelector('#overview-view').hidden", 'return to projects');
    };
    await evaluate(`(() => {
      const title = document.querySelector('.project-card h3');
      const range = document.createRange(); range.selectNodeContents(title);
      window.getSelection().removeAllRanges(); window.getSelection().addRange(range);
      title.click();
    })()`);
    assert(await evaluate("!document.querySelector('#overview-view').hidden && window.getSelection().toString() === 'Fixture project'"), 'Selecting project text navigated away.');
    await evaluate("window.getSelection().removeAllRanges(); document.querySelector('.project-card h3').click()");
    await waitPage("!document.querySelector('#tasks-view').hidden && document.querySelector('#tasks-list .task-row')", 'card title navigation');
    assert(await evaluate("document.querySelector('#project-select').value") === 'fixture-project', 'Card opened the wrong project.');
    await back();
    await evaluate("document.querySelector('.project-card').click()");
    await waitPage("!document.querySelector('#tasks-view').hidden", 'blank card navigation');
    await back();
    await evaluate("document.querySelector('.project-settings-button').focus()");
    await press('Tab', 'Tab', 9);
    assert(await evaluate("document.activeElement.classList.contains('project-open')"), 'Open tasks is not reachable by Tab.');
    assert(await evaluate("getComputedStyle(document.activeElement).outlineStyle !== 'none' && parseFloat(getComputedStyle(document.activeElement).outlineWidth) > 0"), 'Open tasks has no visible keyboard focus.');
    await press('Enter', 'Enter', 13);
    await waitPage("!document.querySelector('#tasks-view').hidden", 'keyboard task navigation');
    await back();
    await evaluate("document.querySelector('.project-settings-button').focus()");
    await press('Enter', 'Enter', 13);
    await waitPage("!document.querySelector('#project-view').hidden", 'keyboard settings navigation');
    assert(await evaluate("document.activeElement.id") === 'project-heading', 'Settings heading did not receive focus.');
    assert(await evaluate("document.querySelector('#tasks-view').hidden"), 'Settings gear also opened tasks.');
    assert(await evaluate("document.querySelector('#project-binding-content button').textContent") === 'Download', 'Project binding download action is missing.');
    await evaluate("document.querySelector('#project-binding-content button').click()");
    await waitPage("document.querySelector('#project-binding-content button').textContent === 'Downloaded'", 'binding download');
    assert(await evaluate("document.querySelector('#project-binding-content code').textContent.includes('project_id = \"fixture-project\"')"), 'Binding omitted the current project.');
    assert(await evaluate("!document.querySelector('#project-view').hidden"), 'Binding download navigated away.');
    const ui = { evaluate, waitPage, send };
    await checkIntegrationOwner(ui);
    await checkIntegratorReports(ui);
    await checkAgentSessions(ui);
    await send('Emulation.setDeviceMetricsOverride', { width: 390, height: 844, deviceScaleFactor: 1, mobile: true });
    assert(await evaluate("document.documentElement.scrollWidth <= window.innerWidth"), 'Settings page overflows at phone width.');
    await evaluate("document.querySelector('#back-to-projects').click()");
    await waitPage("!document.querySelector('#overview-view').hidden", 'settings return navigation');
    assert(await evaluate("document.documentElement.scrollWidth <= window.innerWidth"), 'Project cards overflow at phone width.');
    await send('Emulation.clearDeviceMetricsOverride');
    await evaluate("window.__copiedTaskDetailValue = null; document.querySelector('.project-open').click()");
    await waitPage("document.querySelector('#tasks-list .task-row')", 'task queue');
    await evaluate("document.querySelector('#archived-tasks-tab').click()");
    await waitPage("document.querySelector('#archived-list .task-title')?.textContent === 'Archived fixture task'", 'dedicated archived tasks view');
    assert(await evaluate("!document.querySelector('#archived-view').hidden && document.querySelector('#tasks-view').hidden"), 'Archived tasks did not open in a separate view.');
    await evaluate("document.querySelector('#archived-list [role=button]').click()");
    await waitPage("!document.querySelector('#task-detail-content').hidden", 'archived task detail');
    assert(await evaluate("document.querySelector('#task-operator-actions').textContent.includes('Restore archived task')"), 'Archived task did not expose an explicit restore action.');
    await evaluate("document.querySelector('#back-to-tasks').click()");
    await waitPage("!document.querySelector('#archived-view').hidden", 'return to archive list');
    await evaluate("document.querySelector('#task-queue-tab').click()");
    await waitPage("!document.querySelector('#tasks-view').hidden", 'return from archive');
    assert(await evaluate("!Array.from(document.querySelectorAll('#tasks-list .task-title')).some(node => node.textContent === 'Archived fixture task')"), 'An archived task appeared in the main task queue.');
    assert(await evaluate("document.querySelectorAll('#tasks-list .task-row').length") === 25, `The first task page did not use the default page size (${await evaluate("document.querySelectorAll('#tasks-list .task-row').length")}).`);
    assert(await evaluate("document.querySelector('.task-id-value').textContent") === task.id, 'Queue task ID differs.');
    for (const selector of ['.task-title', '.task-id-value']) {
      const selected = await evaluate(`(() => {
        const node = document.querySelector('${selector}');
        const range = document.createRange(); range.selectNodeContents(node);
        window.getSelection().removeAllRanges(); window.getSelection().addRange(range);
        node.click();
        return { value: window.getSelection().toString(), queueVisible: !document.querySelector('#tasks-view').hidden, selectable: getComputedStyle(node).userSelect };
      })()`);
      assert(selected.value === (selector === '.task-title' ? task.title : task.id), 'Queue selection differs from task identity.');
      assert(selected.queueVisible && selected.selectable === 'text', 'Selecting queue identity navigated away or is disabled.');
    }
    await evaluate("window.getSelection().removeAllRanges()");
    for (const width of [1280, 390, 320]) {
      await send('Emulation.setDeviceMetricsOverride', { width, height: 844, deviceScaleFactor: 1, mobile: true });
      assert(await evaluate("document.documentElement.scrollWidth <= window.innerWidth"), `Task queue overflows at ${width}px.`);
    }
    await send('Emulation.clearDeviceMetricsOverride');
    await evaluate("document.querySelector('#tasks-next-page').click()");
    await waitPage("document.querySelector('#tasks-page-status').textContent.startsWith('Page 2')", 'next task page');
    assert(await evaluate("document.querySelectorAll('#tasks-list .task-row').length") === 5, 'Next page did not show the remaining tasks.');
    assert(await evaluate("document.querySelectorAll('#tasks-page-numbers button').length") === 2, 'The second page was not represented by a numbered control.');
    await evaluate("document.querySelector('#tasks-previous-page').click()");
    await waitPage("document.querySelector('#tasks-page-status').textContent.startsWith('Page 1')", 'previous task page');
    await evaluate("(() => { const size = document.querySelector('#tasks-page-size'); size.value = '10'; size.dispatchEvent(new Event('change', { bubbles: true })); })()");
    await waitPage("document.querySelectorAll('#tasks-list .task-row').length === 10", 'page size update');
    await evaluate("document.querySelector('#tasks-last-page').click()");
    await waitPage("document.querySelector('#tasks-page-status').textContent === 'Page 3 of 30 tasks'", 'last task page');
    await evaluate("document.querySelector('#tasks-first-page').click()");
    await waitPage("document.querySelector('#tasks-page-status').textContent.startsWith('Page 1')", 'first task page');
    await evaluate("[...document.querySelectorAll('#tasks-page-numbers button')].find((button) => button.textContent === '2').click()");
    await waitPage("document.querySelector('#tasks-page-status').textContent.startsWith('Page 2')", 'numbered task page');
    await evaluate("document.querySelector('#tasks-first-page').click()");
    await waitPage("document.querySelector('#tasks-page-status').textContent.startsWith('Page 1')", 'first task page before detail');
    await evaluate("(() => { const size = document.querySelector('#tasks-page-size'); size.value = '50'; size.dispatchEvent(new Event('change', { bubbles: true })); })()");
    await waitPage("document.querySelectorAll('#tasks-list .task-row').length === 30", 'all fixture queue tasks');
    const doneFixtureTask = { ...task, id: 'task-copy-id-done', title: 'Completed fixture task' };
    tasks.push(doneFixtureTask);
    await evaluate("document.querySelector('#refresh-tasks').click()");
    await waitPage("document.querySelectorAll('#tasks-list .task-row').length === 31 && [...document.querySelectorAll('#tasks-list .task-row .task-title')].some((node) => node.textContent === 'Completed fixture task')", 'new example task in queue');
    assert(await evaluate("![...document.querySelector('#status-filter').options].some((option) => option.value === 'done')"), 'Task queue still offers a Done status filter.');
    doneFixtureTask.lifecycle = 'done'; doneFixtureTask.work_status = 'done';
    await evaluate("document.querySelector('#refresh-tasks').click()");
    await waitPage("document.querySelectorAll('#tasks-list .task-row').length === 30 && ![...document.querySelectorAll('#tasks-list .task-row .task-title')].some((node) => node.textContent === 'Completed fixture task')", 'queue after completing example task');
    assert(await evaluate("![...document.querySelectorAll('#tasks-list .task-row .task-title')].some((node) => node.textContent === 'Completed fixture task')"), 'A completed task appeared in the task queue.');
    await evaluate("document.querySelector('#completed-tasks-tab').click()");
    await waitPage("[...document.querySelectorAll('#tasks-list .task-row .task-title')].some((node) => node.textContent === 'Completed fixture task')", 'completed task view');
    assert(await evaluate("document.querySelector('#tasks-heading').textContent === 'Completed tasks'"), 'Completed task view has the wrong heading.');
    assert(await evaluate("document.querySelectorAll('#tasks-list .task-row').length === 1"), 'Completed view included non-completed tasks.');
    await evaluate("document.querySelector('#task-queue-tab').click()");
    await waitPage("document.querySelector('#tasks-heading').textContent === 'Task queue'", 'return to task queue');
    assert(await evaluate("![...document.querySelectorAll('#tasks-list .task-row .task-title')].some((node) => node.textContent === 'Completed fixture task')"), 'A completed task appeared after returning to the task queue.');
    await evaluate("(() => { const size = document.querySelector('#tasks-page-size'); size.value = '25'; size.dispatchEvent(new Event('change', { bubbles: true })); })()");
    await evaluate("(() => { const size = document.querySelector('#tasks-page-size'); size.value = '50'; size.dispatchEvent(new Event('change', { bubbles: true })); })()");
    await waitPage("document.querySelectorAll('#tasks-list .task-row').length === 30 && document.querySelector('#tasks-page-status').textContent === 'Page 1 of 30 tasks'", 'latest 50-item selection');
    assert(await evaluate("document.querySelectorAll('#tasks-list .task-row').length === 30 && document.querySelector('#tasks-page-size').value === '50' && document.querySelector('#tasks-page-status').textContent === 'Page 1 of 30 tasks'"), 'The 50-item selection did not show every queued task on one page.');
    await evaluate("document.querySelector('#tasks-list .task-row[role=button]').focus()");
    await press('Enter', 'Enter', 13);
    await waitPage("document.querySelector('#task-detail-content') && !document.querySelector('#task-detail-content').hidden", 'task detail');

    assert(await evaluate("document.querySelector('#task-detail-heading').textContent") === task.title, 'Task name was not rendered exactly.');
    assert(await evaluate("document.querySelector('#detail-task-id-value').textContent") === task.id, 'Task ID was not rendered exactly.');
    assert(await evaluate("(() => { const labels = Array.from(document.querySelectorAll('#task-operator-actions button'), button => (button.getAttribute('aria-label') || button.textContent)); return labels.includes('Archive task') && labels.includes('Cancel task'); })()"), 'Open task did not expose labeled archive and cancellation controls.');
    await evaluate("document.querySelector('#copy-task-name').click()");
    await waitPage("window.__copiedTaskDetailValue !== null", 'task-name clipboard write');
    assert(await evaluate('window.__copiedTaskDetailValue') === task.title, 'Copy task name did not write the exact task title.');

    await evaluate("window.__copiedTaskDetailValue = null; window.__rejectTaskDetailClipboard = true; document.querySelector('#copy-task-id').click()");
    await waitPage("document.querySelector('#detail-copy-feedback').textContent.includes('Clipboard access was unavailable')", 'manual-copy fallback');
    assert(await evaluate('window.getSelection().toString()') === task.id, 'Clipboard fallback did not select the exact task ID.');
    assert(await evaluate("document.querySelector('#detail-copy-feedback').classList.contains('fallback')"), 'Clipboard fallback was not identified to assistive technology and styling.');
    const openFixtureTask = async () => {
      await send('Page.navigate', { url: `http://127.0.0.1:${serverPort}/` });
      await waitPage("document.querySelector('.project-open')", 'fixture project');
      await evaluate("document.querySelector('.project-open').click()");
      await waitPage("document.querySelector('#tasks-list .task-row')", 'fixture queue');
      await evaluate("document.querySelector('#tasks-list .task-row').click()");
      await waitPage("!document.querySelector('#task-detail-content').hidden", 'fixture detail');
    };
    await openFixtureTask();
    const palette = '<svg xmlns="http://www.w3.org/2000/svg" width="12" height="12"><rect width="12" height="12" fill="#123456"/></svg>';
    await evaluate(`(() => { const input = document.querySelector('#task-attachment-files'); const transfer = new DataTransfer(); transfer.items.add(new File([${JSON.stringify(palette)}], 'palette.svg', { type: 'image/svg+xml' })); input.files = transfer.files; document.querySelector('#upload-task-attachments').click(); })()`);
    await waitPage("!document.querySelector('#retry-task-attachments').hidden", 'saved attachment retry after uncertain upload');
    assert(await evaluate("document.querySelector('#task-attachments-state').textContent.includes('saved bytes and request keys')"), 'Uncertain attachment upload did not explain safe retry.');
    await evaluate("document.querySelector('#retry-task-attachments').click()");
    await waitPage("document.querySelector('#task-attachments-list').textContent.includes('palette.svg') && document.querySelector('#task-attachments-list a')", 'uploaded task attachment');
    const downloadedAttachment = await evaluate("(async () => { const link = document.querySelector('#task-attachments-list a'); const response = await fetch(link.href); return { ok: response.ok, filename: link.download, body: await response.text() }; })()");
    assert(downloadedAttachment.ok && downloadedAttachment.filename === 'palette.svg' && downloadedAttachment.body === palette, 'The attachment download did not preserve the uploaded image context.');
    assert(attachmentUploadKeys.length === 2 && attachmentUploadKeys[0] === attachmentUploadKeys[1], 'Retry did not reuse the exact binary upload idempotency key.');
    task.lifecycle = 'canceled';
    task.work_status = 'canceled';
    await openFixtureTask();
    assert(await evaluate("document.querySelector('#task-operator-actions').textContent.includes('Delete task')"), 'Canceled task did not expose the labeled delete control.');
    await evaluate("[...document.querySelectorAll('#task-operator-actions button')].find(button => button.textContent === 'Delete task').click()");
    await waitPage("document.querySelector('dialog[open] h2')?.textContent === 'Delete task'", 'delete confirmation');
    assert(await evaluate("document.querySelector('dialog[open]').textContent.includes('soft delete that retains task and audit history')"), 'Delete confirmation did not explain retained task and audit history.');
    await evaluate("document.querySelector('dialog[open] button[type=button]').click()");
    task.lifecycle = 'open';
    task.work_status = 'ready';
    const submission = { id: 'fixture-submission', task_id: task.id, kind: 'code', summary: 'Saved candidate', candidate_revision: 'fixture-source', acceptance_evidence: [] };
    task.blocked_reason = 'Saved blocker\nRepository access must be restored.';
    for (const phase of ['review', 'integration', 'done']) {
      task.lifecycle = phase === 'done' ? 'done' : 'open';
      task.work_status = phase === 'done' ? 'done' : `waiting_${phase}`;
      task.workflow = { phase, submission, activities: phase === 'review' ? [{ id: 'fixture-review', kind: 'human_review', status: 'queued' }] : [] };
      await openFixtureTask();
      assert(await evaluate("!Array.from(document.querySelectorAll('#task-operator-actions button')).some(button => (button.getAttribute('aria-label') || button.textContent) === 'Resolve blocker')"), `Ordinary blocker resolution offered during ${phase}.`);
      if (phase === 'review') {
        assert(await evaluate("document.querySelector('#workflow-content').textContent.includes('Claim human review')"), 'Human review claim action missing.');
        assert(await evaluate("document.querySelector('#workflow-content').textContent.includes('Claiming reserves the review for you; it does not approve the work.')"), 'Review claim/decision distinction missing.');
      }
    }
    task.lifecycle = 'open'; task.work_status = 'waiting_review';
    task.workflow = { phase: 'review', submission, activities: [{ id: 'fixture-review', kind: 'human_review', status: 'active', current_attempt: { id: 'fixture-attempt', generation: 1, owner_id: 'fixture-operator', session_id: 'fixture-browser', state: 'active', valid_by_time: true, owner_authorized: true } }] };
    await openFixtureTask();
    await evaluate("Array.from(document.querySelectorAll('#workflow-content button')).find(button => button.textContent === 'Record human review').click()");
    await waitPage("document.querySelector('dialog[open] #workflow-decision')", 'human review dialog');
    assert(await evaluate("document.querySelector('dialog[open]').textContent.includes('fixture-submission')"), 'Review does not identify the saved submission.');
    assert(await evaluate("document.querySelector('#workflow-decision').value") === '', 'Review decision is preselected.');
    await evaluate("document.querySelector('#workflow-summary').value = 'Summary only'; document.querySelector('dialog[open] form').requestSubmit()");
    assert(await evaluate("Boolean(document.querySelector('dialog[open]')) && document.querySelector('#workflow-decision').validity.valueMissing"), 'Review recorded without a chosen decision.');
    await evaluate("document.querySelector('#workflow-decision').value = 'approved'; document.querySelector('#workflow-summary').value = 'Synthetic review'; document.querySelector('#workflow-findings').value = 'Still needs a fix'; document.querySelector('dialog[open] form').requestSubmit()");
    assert(await evaluate("Boolean(document.querySelector('dialog[open]')) && !document.querySelector('#workflow-findings').validity.valid"), 'Required remedies allowed approval.');
    await evaluate("document.querySelector('dialog[open]').close()");

    task.work_status = 'blocked'; task.workflow = { activities: [] };
    await openFixtureTask();
    await evaluate("Array.from(document.querySelectorAll('#task-operator-actions button')).find(button => (button.getAttribute('aria-label') || button.textContent) === 'Resolve blocker').click()");
    await waitPage("document.querySelector('dialog[open] .saved-blocker')", 'saved blocker dialog');
    assert(await evaluate("document.querySelector('.saved-blocker').textContent") === task.blocked_reason, 'Saved blocker reason was lost.');
    assert(await evaluate("document.querySelector('#workflow-reason').maxLength") === 4096, 'Resolution evidence bound changed.');
    assert(await evaluate("document.querySelector('dialog[open]').textContent.includes('Record what changed and how you checked it.')"), 'Resolution instructions missing.');
    const helpSelector = 'dialog[open] [aria-label="Help: Resolution evidence"]';
    const helpVisible = "!document.querySelector('dialog[open] [role=tooltip]').hidden";
    await evaluate(`document.querySelector('${helpSelector}').focus()`);
    await waitPage(helpVisible, 'keyboard help');
    await press('Escape', 'Escape', 27);
    assert(await evaluate("Boolean(document.querySelector('dialog[open]')) && document.querySelector('dialog[open] [role=tooltip]').hidden"), 'Escape closed dialog or failed to dismiss help.');
    const helpPoint = await evaluate(`(() => { const node = document.querySelector('${helpSelector}'); node.scrollIntoView(); const rect = node.getBoundingClientRect(); return { x: rect.x + rect.width / 2, y: rect.y + rect.height / 2 }; })()`);
    await send('Input.dispatchMouseEvent', { type: 'mouseMoved', ...helpPoint });
    await waitPage(helpVisible, 'pointer hover help');
    await send('Input.dispatchMouseEvent', { type: 'mouseMoved', x: 0, y: 0 });
    await waitPage("document.querySelector('dialog[open] [role=tooltip]').hidden", 'hover dismissal');
    await send('Emulation.setTouchEmulationEnabled', { enabled: true });
    await send('Input.dispatchTouchEvent', { type: 'touchStart', touchPoints: [{ ...helpPoint, radiusX: 1, radiusY: 1 }] });
    await send('Input.dispatchTouchEvent', { type: 'touchEnd', touchPoints: [] });
    await waitPage(helpVisible, 'emulated touch help');
    await send('Emulation.setDeviceMetricsOverride', { width: 375, height: 844, deviceScaleFactor: 1, mobile: true });
    assert(await evaluate("document.documentElement.scrollWidth <= window.innerWidth"), 'Blocker dialog overflows on phone.');
    await evaluate("document.querySelector('dialog[open]').close()");
    await evaluate("document.querySelector('button.nav-item[data-view=\"admin\"]').click()");
    await waitPage("!document.querySelector('#admin-view').hidden", 'admin view');
    await evaluate("document.querySelector('#agent-name').value = 'Synthetic agent'; document.querySelector('#issue-form').requestSubmit()");
    await waitPage("!document.querySelector('#token-reveal').hidden", 'issued token reveal');
    assert(await evaluate("document.querySelector('#issued-token').textContent") === 'synthetic-token-for-clipboard-test', 'Issued token was not revealed exactly.');
    assert(await evaluate('window.__credentialDownloadRequested'), 'Credential download was not prepared.');
    await evaluate("window.__copiedTaskDetailValue = null; window.__rejectTaskDetailClipboard = false; document.querySelector('#copy-token').focus()");
    await press('Enter', 'Enter', 13);
    await waitPage("window.__copiedTaskDetailValue === 'synthetic-token-for-clipboard-test'", 'keyboard token clipboard write');
    assert(await evaluate("document.querySelector('#issue-feedback').textContent") === 'Token copied to clipboard.', 'Token success feedback was not truthful.');
    await evaluate("window.__copiedTaskDetailValue = null; window.__rejectTaskDetailClipboard = true; document.querySelector('#copy-token').click()");
    await waitPage("document.querySelector('#issue-feedback').textContent.includes('Clipboard access was unavailable')", 'token manual-copy fallback');
    assert(await evaluate('window.getSelection().toString()') === 'synthetic-token-for-clipboard-test', 'Token fallback did not select the complete synthetic token.');
    await checkAgentActorControls(ui, serverPort);
    console.log('PASS: headless Chrome verified project navigation, the confirmed integration owner setting (held-integration refusal included), integrator reports listing, paging (with a failed-page retry) and resolution (privilege allow/deny and already-resolved refusal), connected agent sessions (held review and work attempts, the lapsed-lease marker, activity windows, refresh, empty and error states), human-only owner and resolve controls, dedicated archived-task view, queue exclusion, task lifecycle controls and deletion safeguards, keyboard focus, queue/completed-task separation, pagination, task attachments with safe uncertain-upload retry and download, binding download, task-detail and issued-token copy/fallback, completion action gating, human-review dialog, saved blockers, and keyboard/hover/emulated-touch help.');
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
