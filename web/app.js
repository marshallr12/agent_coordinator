/* Agent Coordinator dashboard. Same-origin API client; no framework required. */
(() => {
  'use strict';

  const $ = (id) => document.getElementById(id);
  const state = {
    actor: null, csrfToken: null, projects: [], tasks: [], credentials: [], resources: [], resourceCursor: null, resourcePagesExtended: false,
    projectId: '', selectedTaskId: '', currentView: 'overview', detail: null,
    taskCursor: null, hasExtraTaskPages: false, mutation: null, fetching: new Set(), inflight: { projects: false, tasks: null, detail: null, credentials: null },
    requestSeq: { projects: 0, tasks: 0, detail: 0, credentials: 0 }, pollTimer: null, lastSync: null
  };

  const PENDING_MUTATION_KEY = 'agent-coordinator.pending-mutation';

  class ApiError extends Error {
    constructor(message, status, code, details, uncertain = false) {
      super(message); this.name = 'ApiError'; this.status = status; this.code = code;
      this.details = details; this.uncertain = uncertain;
    }
  }

  const text = (value) => value === null || value === undefined ? '' : String(value);
  const setText = (node, value) => { if (node) node.textContent = text(value); };
  const show = (node, visible) => { if (node) node.hidden = !visible; };
  const clear = (node) => { while (node && node.firstChild) node.removeChild(node.firstChild); };
  const el = (tag, className, content) => {
    const node = document.createElement(tag);
    if (className) node.className = className;
    if (content !== undefined) node.textContent = text(content);
    return node;
  };
  const add = (parent, child) => { if (parent && child) parent.appendChild(child); return child; };
  const formatDate = (value) => {
    if (!value) return '—';
    const date = new Date(value);
    return Number.isNaN(date.getTime()) ? text(value) : date.toLocaleString([], { dateStyle: 'medium', timeStyle: 'short' });
  };
  const shortDate = (value) => {
    if (!value) return '—';
    const date = new Date(value);
    return Number.isNaN(date.getTime()) ? text(value) : date.toLocaleDateString([], { month: 'short', day: 'numeric' });
  };
  const displayStatus = (value) => text(value || 'ready').replace(/_/g, ' ').replace(/\b\w/g, (char) => char.toUpperCase());
  const taskStatus = (task) => {
    if (task?.work_status) return task.work_status;
    if (task?.current_attempt_id && task?.lifecycle === 'open') return 'in_progress';
    return task?.lifecycle || task?.status || 'ready';
  };
  const listData = (data) => Array.isArray(data) ? data : Array.isArray(data?.items) ? data.items : [];
  const newKey = () => {
    if (globalThis.crypto?.randomUUID) return crypto.randomUUID();
    return `${Date.now()}-${Math.random().toString(16).slice(2)}`;
  };

  const actorId = () => text(state.actor?.id || state.actor?.principal_id);
  const mutationOperation = (path, method) => {
    if (['POST', 'PUT', 'PATCH'].includes(method) && /\/projects\/[^/]+\/(workflow-policy|policy|workflow-activities\/[^/]+\/[^/]+|tasks\/[^/]+\/workflow\/reopen)$/.test(path)) return 'workflow_change';
    if (method === 'POST' && path === '/api/v1/resources') return 'create_resource';
    if (method === 'POST' && /\/projects\/[^/]+\/reservations\/[^/]+\/resolve$/.test(path)) return 'resolve_resource';
    if (method === 'POST' && path === '/api/v1/projects') return 'create_project';
    if (method === 'POST' && /\/projects\/[^/]+\/tasks$/.test(path)) return 'create_task';
    if (method === 'POST' && path === '/api/v1/admin/agents') return 'issue_credential';
    if (method === 'POST' && /\/admin\/credentials\/[^/]+\/revoke$/.test(path)) return 'revoke_credential';
    if (method === 'POST' && path === '/api/v1/auth/logout') return 'logout';
    return null;
  };
  const sanitizeForSession = (value) => {
    if (Array.isArray(value)) return value.map(sanitizeForSession);
    if (!value || typeof value !== 'object') return value;
    return Object.fromEntries(Object.entries(value).filter(([key]) => !/(password|token|secret|proof|csrf)/i.test(key)).map(([key, item]) => [key, sanitizeForSession(item)]));
  };
  function persistMutation(mutation) {
    if (!mutation.operation || !actorId()) return;
    try { sessionStorage.setItem(PENDING_MUTATION_KEY, JSON.stringify({ actor_id: actorId(), operation: mutation.operation, path: mutation.path, method: mutation.method, key: mutation.key, body: sanitizeForSession(mutation.body), context: mutation.context || {} })); }
    catch (_) { throw new Error('Pending request storage unavailable'); }
  }
  function clearPersistedMutation() { try { sessionStorage.removeItem(PENDING_MUTATION_KEY); } catch (_) { /* no-op */ } }
  function readPersistedMutation() { try { const raw = sessionStorage.getItem(PENDING_MUTATION_KEY); return raw ? JSON.parse(raw) : null; } catch (_) { clearPersistedMutation(); return null; } }

  async function request(path, options = {}) {
    if (!path.startsWith('/') || path.startsWith('//') || path.includes('\\') || path.includes('://')) throw new Error('API requests must use a same-origin path');
    const method = options.method || 'GET';
    const headers = new Headers({ Accept: 'application/json', ...(options.headers || {}) });
    if (options.body !== undefined) {
      headers.set('Content-Type', 'application/json');
      options.body = JSON.stringify(options.body);
    }
    if (state.csrfToken) headers.set('X-CSRF-Token', state.csrfToken);
    if (options.idempotencyKey) headers.set('Idempotency-Key', options.idempotencyKey);
    let response;
    try {
      response = await fetch(path, { ...options, method, headers, credentials: 'same-origin', redirect: 'error' });
    } catch (error) {
      throw new ApiError('The coordinator could not be reached. Keep this request and try again.', 0, 'network_error', null, method !== 'GET');
    }
    let payload = null;
    try { payload = await response.json(); } catch (_) { /* handled below */ }
    const uncertain = method !== 'GET' && (response.status === 429 || response.status >= 500);
    if (!response.ok) {
      const apiError = payload?.error || {};
      throw new ApiError(apiError.message || `Request failed (${response.status}).`, response.status, apiError.code || 'request_failed', apiError.details, uncertain);
    }
    if (!payload || !Object.prototype.hasOwnProperty.call(payload, 'data')) {
      throw new ApiError('The coordinator returned an invalid response envelope.', response.status, 'invalid_response', null, method !== 'GET');
    }
    state.lastSync = new Date();
    setText($('last-sync'), `Updated ${state.lastSync.toLocaleTimeString([], { hour: 'numeric', minute: '2-digit' })}`);
    return payload;
  }

  function setGlobalAlert(message, kind = 'error', withRetry = false) {
    const target = $('global-alert');
    clear(target); show(target, Boolean(message));
    if (!message) return;
    target.className = `global-alert ${kind}`;
    add(target, el('span', '', message));
    if (withRetry && state.mutation && !state.mutation.inFlight) {
      const retry = el('button', 'button subtle', 'Retry request');
      retry.type = 'button'; retry.addEventListener('click', () => executeMutation(state.mutation));
      add(target, retry);
    }
  }

  function errorMessage(error) {
    if (error instanceof ApiError) {
      if (error.code === 'authentication_required' || error.status === 401) return 'Your session has ended. Sign in again to continue.';
      return error.message;
    }
    return 'Something went wrong. Try again.';
  }

  async function executeMutation(mutation) {
    if (!mutation || mutation.inFlight) return;
    mutation.inFlight = true; renderMutationState();
    setGlobalAlert(`Saving ${mutation.label}…`, 'success');
    try {
      const result = await request(mutation.path, { method: mutation.method, body: mutation.body, idempotencyKey: mutation.key });
      state.mutation = null; clearPersistedMutation(); renderMutationState(); setGlobalAlert('', 'success');
      try { await mutation.onSuccess(result.data); }
      catch (error) { setGlobalAlert(`Saved, but the latest view could not refresh: ${errorMessage(error)}`, 'error'); }
    } catch (error) {
      mutation.inFlight = false;
      if (error.code === 'authentication_required' || error.status === 401) { state.mutation = null; clearPersistedMutation(); signOutLocal(); if (mutation.onError) mutation.onError(error); }
      else if (error.uncertain && mutation.operation) { state.mutation = mutation; setGlobalAlert(`${mutation.label} may still be processing. The original request is retained for a safe retry.`, 'error', true); if (mutation.onError) mutation.onError(error); }
      else if (error.uncertain) { state.mutation = null; if (mutation.onError) mutation.onError(new ApiError('Sign-in may still be processing. Try again after checking the session.', error.status, error.code, error.details, false)); else setGlobalAlert(errorMessage(error), 'error'); }
      else { state.mutation = null; clearPersistedMutation(); if (mutation.onError) mutation.onError(error); else setGlobalAlert(errorMessage(error), 'error'); }
      renderMutationState();
    }
  }

  function startMutation(path, body, label, onSuccess, method = 'POST', onError = null, context = {}) {
    if (state.mutation) return;
    const operation = mutationOperation(path, method);
    state.mutation = { path, body, label, method, key: newKey(), operation, context, onSuccess, onError, inFlight: false };
    if (operation && actorId()) {
      try { persistMutation(state.mutation); }
      catch (_) { state.mutation = null; setGlobalAlert('This browser cannot save pending requests. Enable session storage before making changes.'); return; }
    }
    executeMutation(state.mutation);
  }

  function renderMutationState() {
    const busy = Boolean(state.mutation);
    document.querySelectorAll('form button[type="submit"], [data-mutation="true"]').forEach((button) => { button.disabled = busy; });
    if ($('logout-button')) $('logout-button').disabled = busy;
  }

  function applySession(data) {
    state.actor = data?.actor || data?.principal || null;
    state.csrfToken = data?.csrf_token || data?.csrfToken || state.csrfToken;
    const actorName = state.actor?.name || state.actor?.id || 'Operator';
    setText($('actor-chip'), `${actorName} · ${displayStatus(state.actor?.role || 'operator')}`);
    const admin = state.actor?.role === 'admin';
    show($('admin-nav'), admin);
    show($('loading-view'), false); show($('login-view'), false); show($('dashboard-view'), true);
  }

  function signOutLocal() {
    document.querySelectorAll('dialog').forEach(dialog => dialog.close());
    state.actor = null; state.csrfToken = null; state.projects = []; state.tasks = []; state.detail = null;
    state.credentials = []; state.resources = []; state.resourceCursor = null; state.resourcePagesExtended = false; clear($('resources-list')); clear($('job-evidence-content')); clear($('workflow-content')); state.taskCursor = null; state.projectId = ''; state.selectedTaskId = ''; state.currentView = 'overview';
    clearPersistedMutation();
    setText($('issued-token'), ''); show($('token-reveal'), false); setText($('issue-feedback'), ''); show($('issue-feedback'), false); clear($('credentials-list')); show($('credentials-list'), false);
    if (state.pollTimer) clearInterval(state.pollTimer); state.pollTimer = null;
    show($('dashboard-view'), false); show($('loading-view'), false); show($('login-view'), true);
    $('login-form')?.reset(); $('username')?.focus();
  }

  async function restoreSession() {
    try { const result = await request('/api/v1/me'); applySession(result.data); restorePendingMutation(); await loadProjects(); startPolling(); }
    catch (error) { show($('loading-view'), false); show($('dashboard-view'), false); show($('login-view'), true); if (error.status && error.status !== 401) showLoginError(errorMessage(error)); }
  }

  function showLoginError(message) { const target = $('login-error'); setText(target, message); show(target, Boolean(message)); }

  function showView(view) {
    state.currentView = view;
    ['overview', 'tasks', 'task-detail', 'resources', 'admin'].forEach((name) => show($(`${name}-view`), name === view));
    document.querySelectorAll('.nav-item').forEach((button) => button.classList.toggle('active', button.dataset.view === view || (view === 'task-detail' && button.dataset.view === 'tasks')));
    if (view === 'tasks') $('project-select')?.focus();
  }

  function issuedCredentialFeedback(data) {
    const credentialId = data?.credential_id || data?.id || 'the issued credential';
    if (data?.secret_unavailable) return `This issuance was replayed, but its token cannot be recovered. Revoke ${credentialId}, then issue a replacement with a fresh agent name.`;
    if (!data?.token) return `The token was not returned. Revoke ${credentialId}, then issue a replacement with a fresh agent name.`;
    return 'Credential issued. Copy the token now; it will not be shown again.';
  }

  function restoredMutationCallbacks(operation, context = {}) {
    if (operation === 'workflow_change') return async () => { state.projectId = context.projectId || state.projectId; state.selectedTaskId = context.taskId || state.selectedTaskId; if (state.selectedTaskId) { showView('task-detail'); await loadTaskDetail(); } else await loadTasks(); setGlobalAlert('Workflow update recorded. Inspect the current candidate and next actions.', 'success'); };
    if (operation === 'create_resource') return async () => { await loadResources(); setGlobalAlert('Resource created.', 'success'); };
    if (operation === 'resolve_resource') return async () => { state.projectId = context.projectId || state.projectId; state.selectedTaskId = context.taskId || state.selectedTaskId; showView('task-detail'); await loadTaskDetail(); setGlobalAlert('Resolution recorded. Review the updated job and hold evidence.', 'success'); };
    if (operation === 'create_project') return async () => { await loadProjects(); setGlobalAlert('Project created.', 'success'); };
    if (operation === 'create_task') return async () => { state.projectId = context.projectId || state.projectId; await loadTasks(); setGlobalAlert('Task created.', 'success'); };
    if (operation === 'issue_credential') return async (data) => { showToken(data?.token); await loadCredentials(); setIssueFeedback(issuedCredentialFeedback(data), data?.token ? 'success' : 'error'); };
    if (operation === 'revoke_credential') return async () => { await loadCredentials(); setGlobalAlert('Credential revoked.', 'success'); };
    if (operation === 'logout') return async () => signOutLocal();
    return null;
  }

  function restorePendingMutation() {
    const saved = readPersistedMutation();
    if (!saved) return;
    if (!actorId() || saved.actor_id !== actorId() || !saved.operation || !saved.path || !saved.key || !restoredMutationCallbacks(saved.operation, saved.context)) { clearPersistedMutation(); return; }
    state.projectId = saved.context?.projectId || state.projectId;
    state.mutation = { path: saved.path, body: saved.body || {}, label: `${displayStatus(saved.operation)}`, method: saved.method || 'POST', key: saved.key, operation: saved.operation, context: saved.context || {}, onSuccess: restoredMutationCallbacks(saved.operation, saved.context), onError: null, inFlight: false };
    renderMutationState();
    setGlobalAlert(`A previous ${state.mutation.label.toLowerCase()} may still be processing. Review the result, then retry with the retained request if needed.`, 'error', true);
  }

  function renderSummary() {
    const target = $('overview-summary'); clear(target);
    const active = state.tasks.filter((task) => ['claimed', 'in_progress', 'working', 'active'].includes(taskStatus(task))).length;
    const blocked = state.tasks.filter((task) => taskStatus(task) === 'blocked').length;
    [['Projects', state.projects.length], ['Tasks in view', state.tasks.length], ['Active leases', active], ['Blocked', blocked]].forEach(([label, value]) => {
      const card = el('div', 'summary-card'); add(card, el('span', 'summary-number', value)); add(card, el('span', 'summary-label', label)); add(target, card);
    });
  }

  async function loadProjects(silent = false) {
    if (state.fetching.has('projects')) return;
    state.fetching.add('projects');
    if (!silent) { show($('projects-state'), true); setState($('projects-state'), 'Loading projects…', true); show($('projects-grid'), false); }
    try {
      const projects = listData((await request('/api/v1/projects')).data);
      // Preserve focus and open setup disclosures when polling unchanged data.
      const changed = JSON.stringify(projects) !== JSON.stringify(state.projects);
      state.projects = projects;
      if (!silent || changed) { renderProjects(); fillProjectSelect(); }
      renderSummary();
    }
    catch (error) { if (!silent) { setState($('projects-state'), errorMessage(error), false, true); } }
    finally { state.fetching.delete('projects'); }
  }

  function setState(target, message, loading = false, error = false) {
    clear(target); target.className = `state-panel${error ? ' error' : ''}`; show(target, true);
    if (loading) add(target, el('span', 'spinner')); add(target, el('span', '', message));
  }

  function renderProjects() {
    const target = $('projects-grid'); clear(target);
    if (!state.projects.length) { show(target, false); setState($('projects-state'), 'No projects yet. Create one to give agents a place to work.', false); return; }
    show($('projects-state'), false); show(target, true);
    state.projects.forEach((project) => {
      const card = el('article', 'project-card'); const name = el('h3', '', project.name || 'Unnamed project');
      const repo = el('p', 'repo', project.repository_url || 'Repository not configured');
      const projectId = text(project.id);
      const idLabel = el('p', 'project-id', `Project ID · ${projectId || 'Unavailable'}`);
      const binding = document.createElement('details'); binding.className = 'project-binding';
      const summary = el('summary', '', 'Repository binding');
      const bindingContent = el('div', 'binding-content');
      add(bindingContent, el('p', 'binding-help', 'Add this non-secret file as .agent-coordinator.toml before running the agent CLI.'));
      const snippet = `service_url = ${JSON.stringify(location.origin)}\nproject_id = ${JSON.stringify(projectId)}`;
      add(bindingContent, el('code', 'binding-snippet', snippet));
      const copy = el('button', 'button subtle', 'Copy binding'); copy.type = 'button';
      copy.addEventListener('click', async () => {
        try { await navigator.clipboard.writeText(snippet); setText(copy, 'Copied'); }
        catch (_) { setText(copy, 'Copy unavailable'); }
        window.setTimeout(() => setText(copy, 'Copy binding'), 1600);
      });
      add(bindingContent, copy); add(binding, summary); add(binding, bindingContent);
      const footer = el('div', 'project-card-footer');
      add(footer, el('span', 'project-meta', project.target_branch ? `Branch · ${project.target_branch}` : 'Branch not set'));
      const open = el('button', 'project-open', 'Open tasks →'); open.type = 'button'; open.addEventListener('click', () => openProject(project.id));
      add(footer, open); add(card, name); add(card, repo); add(card, idLabel); add(card, binding); add(card, footer); add(target, card);
    });
  }

  function fillProjectSelect() {
    const select = $('project-select'); if (!select) return; const current = state.projectId; clear(select);
    add(select, el('option', '', 'Choose a project')).value = '';
    state.projects.forEach((project) => { const option = el('option', '', project.name || project.id); option.value = text(project.id); add(select, option); });
    select.value = state.projects.some((project) => text(project.id) === current) ? current : '';
  }

  function openProject(id) { state.projectId = text(id); state.taskCursor = null; state.tasks = []; $('project-select').value = state.projectId; $('new-task-button').disabled = false; $('refresh-tasks').disabled = false; showView('tasks'); loadTasks(); }

  async function loadTasks(silent = false, append = false) {
    // Keep later pages readable until the operator explicitly refreshes them.
    if (silent && state.hasExtraTaskPages) return;
    const requestedProjectId = state.projectId;
    if (!requestedProjectId || (state.inflight.tasks?.projectId === requestedProjectId)) return;
    if (append && !state.taskCursor) return;
    const requestId = ++state.requestSeq.tasks;
    const cursorBefore = state.taskCursor;
    state.inflight.tasks = { projectId: requestedProjectId, requestId };
    const project = state.projects.find((item) => text(item.id) === requestedProjectId);
    setText($('tasks-subtitle'), project?.name ? `${project.name} · current work queue` : 'Current work queue');
    if (!silent && !append) { state.taskCursor = null; state.tasks = []; show($('tasks-list'), false); setState($('tasks-state'), 'Loading tasks…', true); }
    if (append) { $('load-more-tasks').disabled = true; setText($('tasks-page-status'), 'Loading more tasks…'); }
    const query = append && cursorBefore ? `?cursor=${encodeURIComponent(cursorBefore)}` : '';
    try {
      const page = (await request(`/api/v1/projects/${encodeURIComponent(requestedProjectId)}/tasks${query}`)).data;
      if (requestedProjectId !== state.projectId || state.requestSeq.tasks !== requestId) return;
      const items = listData(page);
      if (append) { const byId = new Map(state.tasks.map((item) => [text(item.id), item])); items.forEach((item) => byId.set(text(item.id), item)); state.tasks = Array.from(byId.values()); }
      // A refresh re-reads page one and drops appended pages so stale snapshots
      // never contribute to the live summary or get mixed into a later cursor.
      else state.tasks = items;
      state.hasExtraTaskPages = append;
      state.taskCursor = page?.next_cursor || null;
      renderTasks(); renderSummary();
    }
    catch (error) { if (requestedProjectId === state.projectId && state.requestSeq.tasks === requestId && !silent) setState($('tasks-state'), errorMessage(error), false, true); }
    finally { if (state.inflight.tasks?.requestId === requestId) state.inflight.tasks = null; if (state.projectId === requestedProjectId) $('load-more-tasks').disabled = false; }
  }

  function renderTasks() {
    const target = $('tasks-list'); clear(target); const filter = $('status-filter').value;
    const tasks = filter === 'all' ? state.tasks : state.tasks.filter((task) => taskStatus(task) === filter);
    const loadMore = $('load-more-tasks'); show(loadMore, Boolean(state.taskCursor)); loadMore.disabled = Boolean(state.inflight.tasks); setText($('tasks-page-status'), state.hasExtraTaskPages ? `Showing ${state.tasks.length} loaded tasks. Automatic refresh paused; use Refresh for current status.` : state.taskCursor ? `Showing ${state.tasks.length} loaded tasks` : state.tasks.length ? `${state.tasks.length} task${state.tasks.length === 1 ? '' : 's'}` : '');
    if (!tasks.length) { show(target, false); setState($('tasks-state'), state.tasks.length ? 'No tasks match this status filter. Load more to search the rest of the queue.' : 'No tasks in this project yet. Create the first task.', false); return; }
    show($('tasks-state'), false); show(target, true);
    tasks.forEach((task) => {
      const row = el('button', 'task-row'); row.type = 'button'; row.addEventListener('click', () => openTask(task.id));
      const copy = el('span'); add(copy, el('span', 'task-title', task.title || 'Untitled task')); add(copy, el('span', 'task-description', task.description || 'No description')); const meta = el('span', 'task-meta');
      const badge = el('span', `status-badge ${taskStatus(task)}`, displayStatus(taskStatus(task))); add(meta, badge); add(meta, el('span', 'project-meta', `Rev. ${text(task.revision || 1)}`)); add(row, copy); add(row, meta); add(target, row);
    });
  }

  async function loadTaskDetail(silent = false) {
    const requestedProjectId = state.projectId; const requestedTaskId = state.selectedTaskId; const requestKey = `${requestedProjectId}/${requestedTaskId}`;
    if (!requestedProjectId || !requestedTaskId || state.inflight.detail?.key === requestKey) return;
    const requestId = ++state.requestSeq.detail; state.inflight.detail = { key: requestKey, requestId };
    if (!silent) { show($('task-detail-content'), false); setState($('task-detail-state'), 'Loading task…', true); }
    try {
      const data = (await request(`/api/v1/projects/${encodeURIComponent(requestedProjectId)}/tasks/${encodeURIComponent(requestedTaskId)}`)).data;
      if (requestedProjectId !== state.projectId || requestedTaskId !== state.selectedTaskId || state.requestSeq.detail !== requestId) return;
      state.detail = data; renderTaskDetail();
    }
    catch (error) { if (requestedProjectId === state.projectId && requestedTaskId === state.selectedTaskId && state.requestSeq.detail === requestId && !silent) setState($('task-detail-state'), errorMessage(error), false, true); }
    finally { if (state.inflight.detail?.requestId === requestId) state.inflight.detail = null; }
  }

  function openTask(id) { state.selectedTaskId = text(id); showView('task-detail'); loadTaskDetail(); }

  function renderTaskDetail() {
    const data = state.detail || {}; const task = data.task || data; const project = state.projects.find((item) => text(item.id) === state.projectId);
    setText($('detail-project-label'), project?.name || 'Project'); setText($('task-detail-heading'), task.title || 'Untitled task'); setText($('detail-task-id'), `Task ${task.id || state.selectedTaskId} · Revision ${task.revision || 1}`); setText($('detail-description'), task.description || 'No description provided.');
    const status = taskStatus(task); const badge = $('detail-status'); setText(badge, displayStatus(status)); badge.className = `status-badge ${status}`; setText($('detail-kind'), displayStatus(task.activity_kind || task.kind || 'general'));
    const criteria = $('acceptance-list'); clear(criteria); const items = Array.isArray(task.acceptance_criteria) ? task.acceptance_criteria : [];
    if (!items.length) add(criteria, el('li', 'muted', 'No acceptance criteria recorded.')); else items.forEach((item) => add(criteria, el('li', '', item)));
    renderLease(data, task); renderCheckpoints(data); renderJobEvidence(data); renderWorkflow(data);
    show($('task-detail-state'), false); show($('task-detail-content'), true);
  }

  function renderLease(data, task) {
    const target = $('lease-content'); clear(target); const attempts = Array.isArray(data.attempts) ? data.attempts : []; const current = attempts.find((attempt) => attempt.id === task.current_attempt_id);
    if (!current) {
      const status = taskStatus(task);
      const message = task.current_attempt_id ? 'Ownership details are still syncing. Refresh to reconcile this task.' : ['done', 'canceled', 'superseded'].includes(status) ? `No active lease. This task is ${displayStatus(status).toLowerCase()}.` : status === 'blocked' ? 'No active lease. This task is blocked and needs attention.' : status === 'planned' ? 'No active lease. This task is planned and is not yet admitted.' : ['waiting_review', 'waiting_integration', 'integrating', 'validating'].includes(status) ? 'The submitted candidate is moving through its review and integration activities below.' : 'No active lease. This task is available for eligible work.';
      add(target, el('p', 'muted', message)); return;
    }
    const dl = el('dl'); const remaining = current.lease_remaining_ms !== undefined ? `${Math.max(0, Math.round(Number(current.lease_remaining_ms) / 60000))} min remaining` : current.expires_at ? formatLease(current.expires_at) : 'Lease active';
    [['Owner', current.owner_name || current.owner_id || 'Agent'], ['State', displayStatus(taskStatus(task))], ['Lease', remaining], ['Expires', formatDate(current.expires_at)], ['Generation', current.generation || 1]].forEach(([label, value]) => { const line = el('div', 'lease-line'); add(line, el('dt', '', label)); add(line, el('dd', '', value)); add(dl, line); }); add(target, dl);
  }

  function formatLease(expires) { const remaining = new Date(expires).getTime() - Date.now(); return `${Math.max(0, Math.round(remaining / 60000))} min remaining`; }

  function renderCheckpoints(data) {
    const target = $('checkpoints-content'); clear(target); let checkpoints = Array.isArray(data.checkpoints) ? data.checkpoints : [];
    if (!checkpoints.length && Array.isArray(data.attempts)) data.attempts.forEach((attempt) => { if (Array.isArray(attempt.checkpoints)) checkpoints = checkpoints.concat(attempt.checkpoints); });
    checkpoints = checkpoints.slice().sort((a, b) => new Date(b.created_at || b.at).getTime() - new Date(a.created_at || a.at).getTime());
    if (!checkpoints.length) { add(target, el('p', 'muted', 'No checkpoints have been shared yet.')); return; }
    checkpoints.forEach((point) => { const item = el('div', 'checkpoint'); const top = el('div', 'checkpoint-top'); add(top, el('span', '', point.actor_name || point.owner_id || 'Agent')); add(top, el('time', '', formatDate(point.created_at || point.at))); add(item, top); add(item, el('p', 'checkpoint-summary', point.summary || 'Progress update')); if (point.current_action) add(item, el('p', 'checkpoint-detail', `Current action · ${point.current_action}`)); if (point.next_step) add(item, el('p', 'checkpoint-detail', `Next step · ${point.next_step}`)); if (Array.isArray(point.blockers) && point.blockers.length) add(item, el('p', 'checkpoint-detail', `Blockers · ${point.blockers.join(', ')}`)); add(target, item); });
  }

  function openDialog(kind) {
    const dialog = document.createElement('dialog'); dialog.className = 'form-dialog'; const form = el('form'); form.method = 'dialog';
    const title = kind === 'project' ? 'Create project' : 'Create task'; add(form, el('div', 'dialog-header', ''));
    const header = form.firstChild; add(header, el('h2', '', title)); const close = el('button', 'icon-button', '×'); close.type = 'button'; close.setAttribute('aria-label', 'Close'); close.addEventListener('click', () => dialog.close()); add(header, close);
    const fields = [];
    const field = (label, id, type = 'input', placeholder = '') => { const wrap = el('div', 'dialog-field'); const labelNode = el('label', '', label); labelNode.htmlFor = id; const input = document.createElement(type); input.id = id; input.name = id; input.placeholder = placeholder; add(wrap, labelNode); add(wrap, input); add(form, wrap); fields.push(input); return input; };
    if (kind === 'project') { field('Project name', 'project-name', 'input', 'e.g. Atlas API'); field('Repository URL', 'repository-url', 'input', 'https://…'); field('Target branch', 'target-branch', 'input', 'main'); }
    else { const titleInput = field('Task title', 'task-title', 'input', 'What needs to happen?'); titleInput.required = true; field('Description', 'task-description', 'textarea', 'Give the agent enough context to start.'); field('Acceptance criteria', 'task-criteria', 'textarea', 'One criterion per line'); const kindInput = field('Kind', 'task-kind', 'select'); ['code', 'general'].forEach((value) => { const option = el('option', '', value === 'code' ? 'Code' : 'General'); option.value = value; add(kindInput, option); }); }
    const actions = el('div', 'dialog-actions'); const cancel = el('button', 'button subtle', 'Cancel'); cancel.type = 'button'; cancel.addEventListener('click', () => dialog.close()); const submit = el('button', 'button primary', kind === 'project' ? 'Create project' : 'Create task'); submit.type = 'submit'; add(actions, cancel); add(actions, submit); add(form, actions); dialog.appendChild(form); document.body.appendChild(dialog);
    form.addEventListener('submit', (event) => { event.preventDefault(); if (!form.reportValidity()) return; const value = Object.fromEntries(new FormData(form).entries()); dialog.close(); if (kind === 'project') createProject(value); else createTask(value); });
    dialog.addEventListener('close', () => dialog.remove()); dialog.showModal(); fields[0]?.focus();
  }

  function createProject(value) { startMutation('/api/v1/projects', { name: value['project-name'].trim(), repository_url: value['repository-url'].trim(), target_branch: value['target-branch'].trim() }, 'project creation', async () => { await loadProjects(); setGlobalAlert('Project created.', 'success'); }); }
  function createTask(value) { const projectId = state.projectId; const criteria = value['task-criteria'].split('\n').map((item) => item.trim()).filter(Boolean); startMutation(`/api/v1/projects/${encodeURIComponent(projectId)}/tasks`, { title: value['task-title'].trim(), description: value['task-description'].trim(), acceptance_criteria: criteria, kind: value['task-kind'], priority: 2, depends_on: [], planned: false }, 'task creation', async () => { if (state.projectId === projectId) await loadTasks(); setGlobalAlert('Task created.', 'success'); }, 'POST', null, { projectId }); }

  async function loadCredentials(silent = false) {
    if (state.actor?.role !== 'admin' || state.fetching.has('credentials')) return; state.fetching.add('credentials'); if (!silent) { show($('credentials-list'), false); setState($('credentials-state'), 'Loading credentials…', true); }
    try { state.credentials = listData((await request('/api/v1/admin/credentials')).data); renderCredentials(); }
    catch (error) { if (!silent) setState($('credentials-state'), errorMessage(error), false, true); } finally { state.fetching.delete('credentials'); }
  }

  function renderCredentials() { const target = $('credentials-list'); clear(target); if (!state.credentials.length) { show(target, false); setState($('credentials-state'), 'No agent credentials have been issued.', false); return; } show($('credentials-state'), false); show(target, true); state.credentials.forEach((credential) => { const row = el('div', 'credential-row'); const info = el('div'); add(info, el('div', 'credential-name', credential.name || credential.principal_name || credential.id)); add(info, el('div', 'credential-meta', `Issued ${shortDate(credential.created_at)}`)); if (credential.revoked_at) add(info, el('div', 'credential-revoked', `Revoked ${shortDate(credential.revoked_at)}`)); add(row, info); if (!credential.revoked_at) { const revoke = el('button', 'button danger', 'Revoke'); revoke.type = 'button'; revoke.dataset.mutation = 'true'; revoke.addEventListener('click', () => revokeCredential(credential.id)); add(row, revoke); } add(target, row); }); renderMutationState(); }
  function revokeCredential(id) { startMutation(`/api/v1/admin/credentials/${encodeURIComponent(id)}/revoke`, {}, 'credential revocation', async () => { await loadCredentials(); setGlobalAlert('Credential revoked.', 'success'); }); }

  function startPolling() { if (state.pollTimer) clearInterval(state.pollTimer); state.pollTimer = setInterval(() => { if (!state.actor || state.mutation) return; if (state.currentView === 'overview') loadProjects(true); else if (state.currentView === 'tasks') loadTasks(true); else if (state.currentView === 'task-detail') loadTaskDetail(true); else if (state.currentView === 'admin') loadCredentials(true); else if (state.currentView === 'resources' && !state.resourcePagesExtended) loadResources(true); }, 5000); }

  $('login-form').addEventListener('submit', (event) => { event.preventDefault(); const username = $('username').value.trim(); const password = $('password').value; if (!username || !password) { showLoginError('Enter your username and password.'); return; } showLoginError(''); startMutation('/api/v1/auth/login', { username, password }, 'sign-in', async (data) => { applySession(data); $('password').value = ''; await loadProjects(); startPolling(); }, 'POST', (error) => showLoginError(errorMessage(error))); });
  $('logout-button').addEventListener('click', () => startMutation('/api/v1/auth/logout', {}, 'sign-out', async () => signOutLocal()));
  document.querySelectorAll('.nav-item').forEach((button) => button.addEventListener('click', () => { const view = button.dataset.view; showView(view); if (view === 'tasks' && state.projectId) loadTasks(); if (view === 'admin') loadCredentials(); if (view === 'resources') loadResources(); }));
  $('brand-button').addEventListener('click', () => showView('overview')); $('new-project-button').addEventListener('click', () => openDialog('project')); $('new-task-button').addEventListener('click', () => openDialog('task'));
  $('refresh-projects').addEventListener('click', () => loadProjects()); $('refresh-tasks').addEventListener('click', () => loadTasks()); $('load-more-tasks').addEventListener('click', () => loadTasks(false, true)); $('project-select').addEventListener('change', (event) => { state.projectId = event.target.value; state.taskCursor = null; state.tasks = []; $('new-task-button').disabled = !state.projectId; $('refresh-tasks').disabled = !state.projectId; loadTasks(); }); $('status-filter').addEventListener('change', renderTasks);
  $('back-to-tasks').addEventListener('click', () => showView('tasks')); $('refresh-credentials').addEventListener('click', () => loadCredentials()); $('issue-form').addEventListener('submit', (event) => { event.preventDefault(); const input = $('agent-name'); if (!input.value.trim()) return; startMutation('/api/v1/admin/agents', { name: input.value.trim() }, 'credential issuance', async (data) => { input.value = ''; showToken(data?.token); await loadCredentials(); setIssueFeedback(issuedCredentialFeedback(data), data?.token ? 'success' : 'error'); }); });
  function showToken(token) { setText($('issued-token'), token || 'The token was not returned. Revoke this credential and issue a replacement.'); show($('token-reveal'), true); }
  function setIssueFeedback(message, kind) { const target = $('issue-feedback'); target.className = `inline-alert ${kind}`; setText(target, message); show(target, true); }
  $('clear-token').addEventListener('click', () => { setText($('issued-token'), ''); show($('token-reveal'), false); }); $('copy-token').addEventListener('click', async () => { const token = $('issued-token').textContent; if (!token) return; try { await navigator.clipboard.writeText(token); setIssueFeedback('Token copied to clipboard.', 'success'); } catch (_) { setIssueFeedback('Copy was unavailable. Select the token and copy it manually.', 'error'); } });


  async function loadResources(silent = false, append = false) {
    if (state.fetching.has('resources')) return;
    state.fetching.add('resources');
    const requestedActor = actorId();
    if (!silent) setState($('resources-state'), 'Loading resources…', true);
    try {
      const query = append && state.resourceCursor ? `?cursor=${encodeURIComponent(state.resourceCursor)}` : '';
      const page = (await request(`/api/v1/resources${query}`)).data;
      if (!requestedActor || requestedActor !== actorId()) return;
      const items = listData(page);
      state.resourcePagesExtended = append;
      state.resources = append ? [...new Map([...state.resources, ...items].map(item => [item.id, item])).values()] : items;
      state.resourceCursor = page.next_cursor || null;
      const target = $('resources-list'); clear(target);
      state.resources.forEach(resource => {
        const card = el('article', 'card'); add(card, el('h2', '', resource.key));
        add(card, el('p', 'muted', resource.description || 'No description'));
        add(card, el('p', '', `${resource.held_units ?? 0} held / ${resource.capacity} capacity units`));
        add(card, el('code', 'binding-snippet', resource.id)); add(target, card);
      });
      show($('resources-state'), !state.resources.length);
      if (!state.resources.length) setState($('resources-state'), 'No resources yet. Define a stable key before reserving shared capacity.');
      show($('load-more-resources'), Boolean(state.resourceCursor));
    } catch (error) { if (requestedActor === actorId()) setState($('resources-state'), errorMessage(error), false, true); }
    finally { state.fetching.delete('resources'); }
  }

  function renderJobEvidence(data) {
    const target = $('job-evidence-content'); clear(target);
    const evidence = data.job_evidence || {};
    const jobs = Array.isArray(evidence.jobs) ? evidence.jobs : [];
    const reservations = Array.isArray(evidence.reservations) ? evidence.reservations : [];
    if (evidence.jobs_truncated || evidence.reservations_truncated) add(target, el('p', 'muted', 'Showing up to 50 jobs and 50 reservations, with unresolved evidence first. Use the CLI job and reservation lists to inspect the complete history.'));
    if (!jobs.length && !reservations.length) { add(target, el('p', 'muted', 'No jobs or resource reservations recorded.')); return; }
    jobs.forEach(job => {
      const entry = el('div', 'evidence-entry'); add(entry, el('h3', '', job.label || 'Local job'));
      add(entry, el('p', '', `Producer: ${displayStatus(job.state)} · Observation: ${displayStatus(job.observation_freshness || 'unknown')}`));
      add(entry, el('p', 'muted', `Last observation: ${formatDate(job.last_observed_at)} · Exit: ${job.exit_code ?? 'not reported'}`));
      if (job.inputs_unchanged === false) add(entry, el('p', 'error', 'Source changed during this job; its result does not verify the current checkout.'));
      if (job.reconciled_at) { add(entry, el('p', 'muted', `Operator reconciliation: ${job.reconciliation_reason || 'Recorded'}`)); add(entry, el('p', '', job.reconciliation_evidence || '')); }
      add(entry, el('code', '', `Job ${job.id} · Producer ${job.producer_id}`)); add(target, entry);
    });
    reservations.forEach(reservation => {
      const entry = el('div', 'evidence-entry'); add(entry, el('h3', '', `Resource reservation · ${displayStatus(reservation.state)}`));
      add(entry, el('code', '', reservation.id));
      if (reservation.resolution_evidence) add(entry, el('p', '', `Resolution evidence: ${reservation.resolution_evidence}`));
      (reservation.items || []).forEach(item => add(entry, el('p', 'muted', `${item.key || item.resource_id}: ${item.units} units`)));
      if (!['released','resolved'].includes(reservation.state) && state.actor?.kind === 'human') {
        const button = el('button', 'button subtle', 'Record resource resolution'); button.type = 'button'; button.dataset.mutation = 'true';
        button.addEventListener('click', () => openResourceResolution(reservation)); add(entry, button);
      }
      add(target, entry);
    });
    renderMutationState();
  }

  function openResourceResolution(reservation) {
    const projectId = state.projectId; const taskId = state.selectedTaskId;
    const dialog = el('dialog', 'form-dialog'); const form = el('form');
    add(form, el('h2', '', 'Resolve a physical resource hold'));
    add(form, el('p', 'muted', 'Record how you verified the old producer stopped or was isolated. This permits conflicting work to use the resource. An unreachable workstation is not evidence that it stopped.'));
    for (const [id, label] of [['resolution-reason', 'Reason'], ['resolution-evidence', 'Evidence of termination or isolation']]) {
      const name = el('label', '', label); name.htmlFor = id; const field = el('textarea'); field.id = id; field.name = id; field.required = true; field.maxLength = 4096;
      add(form, name); add(form, field);
    }
    const actions = el('div', 'dialog-actions'); const cancel = el('button', 'button subtle', 'Cancel'); cancel.type = 'button'; cancel.addEventListener('click', () => dialog.close());
    const submit = el('button', 'button primary', 'Record resolution'); submit.type = 'submit'; add(actions, cancel); add(actions, submit); add(form, actions); add(dialog, form); add(document.body, dialog);
    form.addEventListener('submit', event => {
      event.preventDefault(); if (!form.reportValidity()) return;
      const values = new FormData(form); dialog.close();
      startMutation(`/api/v1/projects/${encodeURIComponent(projectId)}/reservations/${encodeURIComponent(reservation.id)}/resolve`, {reason: values.get('resolution-reason'), evidence: values.get('resolution-evidence')}, 'resource resolution', async () => {
        if (state.projectId === projectId && state.selectedTaskId === taskId) await loadTaskDetail();
        setGlobalAlert('Resource resolution recorded with evidence.', 'success');
      }, 'POST', null, {projectId, taskId});
    });
    dialog.addEventListener('close', () => dialog.remove()); dialog.showModal();
  }

  $('refresh-resources').addEventListener('click', () => loadResources());
  $('load-more-resources').addEventListener('click', () => loadResources(false, true));
  $('resource-form').addEventListener('submit', event => {
    event.preventDefault(); if (!event.currentTarget.reportValidity()) return;
    const body = {key: $('resource-key').value.trim(), capacity: Number($('resource-capacity').value), description: $('resource-description').value.trim()};
    startMutation('/api/v1/resources', body, 'resource creation', async () => { $('resource-form').reset(); await loadResources(); setGlobalAlert('Resource created.', 'success'); });
  });

  const projectPath = (projectId = state.projectId) => `/api/v1/projects/${encodeURIComponent(projectId)}`;
  function workflowDialog(title, description) {
    const dialog = el('dialog', 'form-dialog'); const form = el('form');
    add(form, el('h2', '', title)); if (description) add(form, el('p', 'muted', description));
    const field = (name, label, value = '', kind = 'textarea') => {
      const id = `workflow-${name}`; const caption = el('label', '', label); caption.htmlFor = id;
      const input = el(kind); input.id = id; input.name = name; input.value = text(value); input.required = true; input.maxLength = 8192;
      add(form, caption); add(form, input); return input;
    };
    const finish = (label, submit) => {
      const actions = el('div', 'dialog-actions'); const cancel = el('button', 'button subtle', 'Cancel'); cancel.type = 'button'; cancel.addEventListener('click', () => dialog.close());
      const save = el('button', 'button primary', label); save.type = 'submit'; add(actions, cancel); add(actions, save); add(form, actions);
      form.addEventListener('submit', event => { event.preventDefault(); if (!form.reportValidity() || state.mutation) return; submit(new FormData(form), dialog); });
      add(dialog, form); add(document.body, dialog); dialog.addEventListener('close', () => dialog.remove()); dialog.showModal();
    };
    return { dialog, form, field, finish };
  }
  function workflowMutation(path, body, label, after) {
    const projectId = state.projectId, taskId = state.selectedTaskId;
    startMutation(path, body, label, async data => {
      if (projectId === state.projectId && taskId === state.selectedTaskId && taskId) await loadTaskDetail();
      else if (projectId === state.projectId) await loadTasks();
      if (after) after(data);
    }, 'POST', null, { projectId, taskId });
  }
  function renderWorkflow(data) {
    const target = $('workflow-content'); clear(target); const workflow = data.workflow || {}; const submission = workflow.submission;
    if (!submission) { add(target, el('p', 'muted', 'No current submission. The implementation owner submits the result and acceptance evidence through the CLI.')); return; }
    const candidate = el('div', 'workflow-entry'); add(candidate, el('h3', '', workflow.phase === 'revision_needed' ? 'Previous candidate' : 'Current candidate'));
    add(candidate, el('p', '', submission.summary));
    const details = el('dl');
    [['Submission', submission.id], ['Source revision', submission.candidate_revision], ['Source tree', submission.candidate_tree], ['Task revision', submission.task_revision], ['Policy revision', submission.project_policy_revision]].filter(([,value]) => value !== null && value !== undefined).forEach(([label,value]) => { add(details, el('dt', 'muted', label)); add(details, el('dd', '', value)); });
    add(candidate, details); if (submission.handoff) add(candidate, el('p', '', submission.handoff));
    (submission.acceptance_evidence || []).forEach(item => add(candidate, el('p', '', `${item.criterion}: ${item.evidence}`)));
    if (state.actor?.kind === 'human' && workflow.phase !== 'done' && workflow.phase !== 'revision_needed' && submission.task_id) {
      const reopen = el('button', 'button subtle', 'Reopen for revision'); reopen.type = 'button'; reopen.dataset.mutation = 'true';
      reopen.addEventListener('click', () => {
        const view = workflowDialog('Reopen this candidate', 'This preserves the current evidence and cancels its pending activities. A new submission will need fresh reviews. Live jobs or uncertain publication must be resolved first.');
        view.field('reason', 'Reason for a new revision');
        view.finish('Reopen for revision', (values, dialog) => { dialog.close(); workflowMutation(`${projectPath()}/tasks/${encodeURIComponent(submission.task_id)}/workflow/reopen`, {submission_id:submission.id, reason:values.get('reason')}, 'candidate revision'); });
      }); add(candidate, reopen);
    }
    add(target, candidate);
    (workflow.blockers || []).forEach(item => add(target, el('p', 'inline-alert error', typeof item === 'string' ? item : item.message || item.code)));
    if (workflow.activities_truncated) add(target, el('p', 'muted', 'Showing the latest 100 workflow activities. Older records remain stored.'));
    (workflow.activities || []).forEach(activity => {
      const entry = el('div', 'workflow-entry'); add(entry, el('h3', '', displayStatus(activity.kind)));
      add(entry, el('p', 'muted', `${displayStatus(activity.status)} · ${activity.id}`));
      if (activity.activity_task_id) { const open = el('button', 'button text-button', 'Inspect activity task and jobs'); open.type = 'button'; open.addEventListener('click', () => openTask(activity.activity_task_id)); add(entry, open); }
      const attempt = activity.current_attempt || activity.attempt;
      if (attempt) add(entry, el('p', 'muted', `Owner ${attempt.owner_id} · lease expires ${formatDate(attempt.expires_at)}`));
      if (activity.review) { add(entry, el('p', '', `${displayStatus(activity.review.decision)}: ${activity.review.summary}`)); (activity.review.findings || []).forEach(finding => add(entry, el('p', '', `${displayStatus(finding.severity)}: ${finding.remedy}`))); }
      if (activity.intent) add(entry, el('p', 'muted', `Publication intent: ${activity.intent.observed_target_revision} → ${activity.intent.result_revision}`));
      if (activity.result) add(entry, el('p', '', `Publication: ${displayStatus(activity.result.publication_state)}`));
      if (activity.hold) add(entry, el('p', '', `Target hold: ${displayStatus(activity.hold.state)} · ${activity.hold.canonical_repository_key} · ${activity.hold.target_branch}`));
      if (activity.publication_reconciliation) add(entry, el('p', '', `Reconciled ${displayStatus(activity.publication_reconciliation.disposition)}: ${activity.publication_reconciliation.evidence}`));
      (activity.result?.check_job_ids || []).forEach(id => add(entry, el('p', 'muted', `Check producer: ${id}`)));
      if (activity.authorization) add(entry, el('p', '', `Human authorization: ${activity.authorization.summary}`));
      const action = (label, callback) => { const button = el('button', 'button subtle', label); button.type = 'button'; button.dataset.mutation = 'true'; button.addEventListener('click', callback); add(entry, button); };
      if (state.actor?.kind === 'human' && !['done','completed','canceled'].includes(activity.status)) {
        if (activity.kind === 'human_review' && !(workflow.blockers || []).length) {
          const current = attempt?.state === 'active' && attempt.valid_by_time === true && attempt.owner_authorized === true;
          if (current && attempt.owner_id === actorId() && attempt.session_id === state.actor.session_id) action('Record human review', () => openHumanReview(activity, submission, attempt));
          else if (!current) action('Claim human review', () => workflowMutation(`${projectPath()}/workflow-activities/${encodeURIComponent(activity.id)}/claim`, {expected_submission_id: submission.id, expected_project_policy_revision: submission.project_policy_revision, expected_workflow_policy_revision: submission.workflow_policy_revision}, 'review claim', response => openHumanReview(activity, submission, response.attempt)));
        }
        if (activity.kind === 'integration') {
          if (!activity.authorization && !(workflow.blockers || []).length && workflow.phase === 'integration' && state.projects.find(project => text(project.id) === state.projectId)?.automatic_integration === false) action('Authorize integration', () => openIntegrationAuthorization(activity, submission));
          if (activity.intent && !activity.publication_reconciliation) action('Reconcile publication', () => openPublicationReconciliation(activity, submission));
        }
      }
      add(target, entry);
    });
    (workflow.next_actions || []).forEach(action => add(target, el('p', 'muted', typeof action === 'string' ? action : action.message)));
    renderMutationState();
  }
  function openHumanReview(activity, submission, attempt) {
    if (!attempt) { setGlobalAlert('Refresh the activity to inspect its current ownership.', 'error'); return; }
    const view = workflowDialog('Review this candidate', `This decision applies only to submission ${submission.id}, source ${submission.candidate_revision || 'general task evidence'}. Inspect its acceptance evidence and checks before deciding.`);
    const decision = view.field('decision', 'Decision', '', 'select'); for (const [value,label] of [['changes_requested','Changes requested'],['approved','Approved']]) { const option = el('option', '', label); option.value = value; add(decision, option); }
    view.field('summary', 'Review summary'); const findings = view.field('findings', 'Required remedies — one per line'); findings.required = false;
    decision.addEventListener('change', () => findings.setCustomValidity(''));
    view.finish('Record review', (values, dialog) => {
      const remedies = text(values.get('findings')).split('\n').map(v => v.trim()).filter(Boolean);
      if (values.get('decision') === 'approved' && remedies.length) { findings.setCustomValidity('Required remedies must be resolved before approval. Choose changes requested.'); findings.reportValidity(); findings.addEventListener('input', () => findings.setCustomValidity(''), {once:true}); return; }
      dialog.close(); workflowMutation(`${projectPath()}/workflow-activities/${encodeURIComponent(activity.id)}/review`, {generation: attempt.generation, submission_id: submission.id, decision: values.get('decision'), summary: values.get('summary'), findings: remedies.map(remedy => ({severity:'required',remedy,evidence:text(values.get('summary'))}))}, 'human review');
    });
  }
  function openIntegrationAuthorization(activity, submission) {
    const view = workflowDialog('Authorize this integration', `Permit agents to integrate submission ${submission.id} after required review and checks. This does not publish source or mark the task done.`);
    view.field('summary', 'Authorization reason'); view.finish('Authorize integration', (values, dialog) => { dialog.close(); workflowMutation(`${projectPath()}/workflow-activities/${encodeURIComponent(activity.id)}/authorization`, {submission_id:submission.id, expected_project_policy_revision:submission.project_policy_revision, expected_workflow_policy_revision:submission.workflow_policy_revision, summary:values.get('summary')}, 'integration authorization'); });
  }
  function openPublicationReconciliation(activity, submission) {
    const view = workflowDialog('Reconcile publication', 'Inspect the actual remote target and the previous publisher first. An unreachable workstation does not prove publication stopped. Record what is known; this cannot manufacture a check result.');
    const disposition = view.field('disposition', 'Observed outcome', '', 'select'); for (const [value,label] of [['not_published','Confirmed not published'],['published','Confirmed published'],['target_moved','Target advanced or changed']]) { const option = el('option', '', label); option.value = value; add(disposition, option); }
    view.field('observed_target_revision', 'Observed remote commit', '', 'input'); view.field('observed_target_tree', 'Observed remote tree', '', 'input'); view.field('evidence', 'Evidence, including publisher termination or isolation');
    view.finish('Record reconciliation', (values, dialog) => { dialog.close(); workflowMutation(`${projectPath()}/workflow-activities/${encodeURIComponent(activity.id)}/publication-reconciliation`, {submission_id:submission.id, ...Object.fromEntries(values)}, 'publication reconciliation'); });
  }

  async function openWorkflowSettings() {
    const projectId = state.projectId, currentActor = actorId();
    if (!projectId) { setGlobalAlert('Choose a project before opening its settings.', 'error'); return; }
    try {
      const [policyReply, orientationReply] = await Promise.all([
        request(`${projectPath(projectId)}/workflow-policy`).catch(error => {
          if (error.code === 'workflow_policy_required') return {data:{revision:0,canonical_repository_key:'',required_checks:[]}};
          throw error;
        }),
        request(`${projectPath(projectId)}/orientation`)
      ]);
      if (projectId !== state.projectId || currentActor !== actorId()) return;
      const policy = policyReply.data, project = orientationReply.data.project;
      const view = workflowDialog('Required checks', 'Use the same repository identity for every project and URL alias sharing a Git repository. Check identities, versions, and environments must match the producer registration exactly. Saving a new roster makes existing candidates require reconciliation.');
      const rules = el('button', 'button subtle', `Review: ${displayStatus(project.review_mode)} · ${project.automatic_integration ? 'Automatic integration allowed' : 'Human integration authorization required'}`);
      rules.type = 'button'; rules.addEventListener('click', () => { view.dialog.close(); openReviewSettings(project); }); add(view.form, rules);
      const key = view.field('canonical_repository_key', 'Shared repository identity', policy.canonical_repository_key || '', 'input'); key.maxLength = 255;
      const rows = el('div'); add(view.form, rows); let serial = 0;
      const addRow = (check = {}) => {
        const row = el('div', 'check-row'); row.dataset.row = String(serial++);
        for (const [name,label] of [['identity','Check'],['version','Definition version'],['environment','Environment']]) {
          const wrap = el('div'), id = `check-${row.dataset.row}-${name}`, caption = el('label', '', label), input = el('input');
          caption.htmlFor = id; input.id = id; input.name = name; input.value = check[name] || ''; input.required = true; input.maxLength = 255;
          add(wrap, caption); add(wrap, input); add(row, wrap);
        }
        const remove = el('button', 'button subtle', 'Remove'); remove.type = 'button'; remove.addEventListener('click', () => row.remove()); add(row, remove); add(rows, row);
      };
      const checks = policy.required_checks || []; (checks.length ? checks : [{}]).forEach(addRow);
      const more = el('button', 'button subtle', 'Add required check'); more.type = 'button'; more.addEventListener('click', () => { if (rows.children.length < 100) addRow(); }); add(view.form, more);
      view.finish('Save check roster', (_, dialog) => {
        if (!rows.children.length) { setGlobalAlert('Configure at least one required check.', 'error'); return; }
        const required_checks = Array.from(rows.children).map(row => Object.fromEntries(Array.from(row.querySelectorAll('input')).map(input => [input.name,input.value.trim()])));
        dialog.close(); startMutation(`${projectPath(projectId)}/workflow-policy`, {expected_revision:policy.revision || 0, canonical_repository_key:key.value.trim(), required_checks}, 'check roster', async () => { await loadProjects(); setGlobalAlert('Check roster saved. Agents must read the updated workflow policy.', 'success'); }, 'PUT', null, {projectId});
      });
    } catch (error) { setGlobalAlert(errorMessage(error), 'error'); }
  }
  function openReviewSettings(project) {
    const projectId = state.projectId;
    const view = workflowDialog('Review and integration rules', 'These rules apply to new submissions. Existing candidates remain bound to their recorded policy revision and need explicit reconciliation after a policy change.');
    const mode = view.field('review_mode', 'Required review', '', 'select');
    for (const [value,label] of [['agent','Independent agent'],['human','Human'],['both','Independent agent and human'],['none','No required review']]) { const option = el('option', '', label); option.value = value; add(mode, option); } mode.value = project.review_mode;
    const integration = view.field('automatic_integration', 'Integration authorization', '', 'select');
    for (const [value,label] of [['false','A human must authorize each candidate'],['true','Agents may integrate after required review']]) { const option = el('option', '', label); option.value = value; add(integration, option); } integration.value = String(project.automatic_integration);
    view.finish('Save review rules', (values, dialog) => {
      dialog.close(); startMutation(`${projectPath(projectId)}/policy`, {expected_revision:project.policy_revision, review_mode:values.get('review_mode'), recovery_mode:project.recovery_mode, lease_seconds:project.lease_seconds, rules:project.rules, agent_rule_editing:project.agent_rule_editing, automatic_integration:values.get('automatic_integration') === 'true'}, 'review rules', async () => { await loadProjects(); setGlobalAlert('Review rules saved. Agents must read and acknowledge the updated policy.', 'success'); }, 'PATCH', null, {projectId});
    });
  }
  $('workflow-settings').addEventListener('click', openWorkflowSettings);

  restoreSession();
})();
