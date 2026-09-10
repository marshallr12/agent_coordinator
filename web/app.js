/* Agent Coordinator dashboard. Same-origin API client; no framework required. */
(() => {
  'use strict';

  const $ = (id) => document.getElementById(id);
  const state = {
    actor: null, csrfToken: null, projects: [], tasks: [], credentials: [], resources: [], resourceCursor: null, resourcePagesExtended: false, sharedCursor: null, sharedSeq: 0,
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
    if (['POST','PATCH'].includes(method) && /\/projects\/[^/]+\/(knowledge|decisions|artifacts|imports)(\/|$)/.test(path)) return 'shared_change';
    if (['POST','PATCH'].includes(method) && /\/projects\/[^/]+\/(objectives|claims|attempts|tasks)(\/|$)/.test(path)) return 'operator_work';
    if (method === 'POST' && /\/admin\/operators\/[^/]+\/access$/.test(path)) return 'operator_access';
    if (method === 'POST' && /\/browser-sessions\/[^/]+\/revoke$/.test(path)) return 'browser_revoke';
    if (method === 'POST' && /\/admin\/credentials\/[^/]+\/rotate$/.test(path)) return 'rotate_credential';
    if (method === 'POST' && path === '/api/v1/admin/operators') return 'create_operator';
    if (method === 'POST' && path === '/api/v1/auth/password') return 'change_password';
    if (method === 'POST' && path === '/api/v1/admin/clock/reconcile') return 'clock_change';
    if (method === 'POST' && path.startsWith('/api/v1/admin/restore/')) return 'restore_change';
    if (method === 'POST' && /\/admin\/agents\/[^/]+\/credentials$/.test(path)) return 'rotate_credential';
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
  function clearPersistedMutation(key = null) { try { const saved = sessionStorage.getItem(PENDING_MUTATION_KEY); if (key === null || (saved && JSON.parse(saved).key === key)) sessionStorage.removeItem(PENDING_MUTATION_KEY); } catch (_) { /* no-op */ } }
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
    if (!mutation || mutation.inFlight || mutation.awaitingSecret) return;
    mutation.inFlight = true; renderMutationState();
    setGlobalAlert(`Saving ${mutation.label}…`, 'success');
    try {
      const result = await request(mutation.path, { method: mutation.method, body: mutation.body, idempotencyKey: mutation.key });
      state.mutation = null; clearPersistedMutation(mutation.key); renderMutationState(); setGlobalAlert('', 'success');
      try { await mutation.onSuccess(result.data); }
      catch (error) { setGlobalAlert(`Saved, but the latest view could not refresh: ${errorMessage(error)}`, 'error'); }
    } catch (error) {
      mutation.inFlight = false;
      if (error.code === 'authentication_required' || error.status === 401) {
        if (['logout', 'change_password'].includes(mutation.operation)) clearPersistedMutation(mutation.key);
        state.mutation = null; signOutLocal(true); if (mutation.onError) mutation.onError(error);
      }
      else if (error.uncertain && mutation.operation) { state.mutation = mutation; setGlobalAlert(`${mutation.label} may still be processing. The original request is retained for a safe retry.`, 'error', true); if (mutation.onError) mutation.onError(error); }
      else if (error.uncertain) { state.mutation = null; if (mutation.onError) mutation.onError(new ApiError('Sign-in may still be processing. Try again after checking the session.', error.status, error.code, error.details, false)); else setGlobalAlert(errorMessage(error), 'error'); }
      else { state.mutation = null; clearPersistedMutation(mutation.key); if (mutation.onError) mutation.onError(error); else setGlobalAlert(errorMessage(error), 'error'); }
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
    document.querySelectorAll('form button[type="submit"], [data-mutation="true"]').forEach((button) => { button.disabled = busy && !(state.mutation?.awaitingSecret && button.closest('form')?.dataset.resumeSecret === 'true'); });
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

  function signOutLocal(preservePending = false) {
    document.querySelectorAll('dialog').forEach(dialog => dialog.close());
    state.mutation = null; state.actor = null; state.csrfToken = null; state.projects = []; state.tasks = []; state.detail = null;
    ++state.sharedSeq; state.sharedCursor = null; clear($('shared-list')); setText($('shared-freshness'), '');
    $('context-search').reset();
    state.credentials = []; state.resources = []; state.resourceCursor = null; state.resourcePagesExtended = false; clear($('resources-list')); clear($('job-evidence-content')); clear($('workflow-content')); state.taskCursor = null; state.projectId = ''; state.selectedTaskId = ''; state.currentView = 'overview';
    if (!preservePending) clearPersistedMutation();
    setText($('issued-token'), ''); show($('token-reveal'), false); setText($('issue-feedback'), ''); show($('issue-feedback'), false); clear($('credentials-list')); clear($('operators-list')); clear($('restore-status')); clear($('clock-status')); clear($('restore-requirements')); clear($('task-operator-actions')); show($('credentials-list'), false);
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
    ['overview', 'tasks', 'task-detail', 'resources', 'admin', 'shared'].forEach((name) => show($(`${name}-view`), name === view));
    document.querySelectorAll('.nav-item').forEach((button) => button.classList.toggle('active', button.dataset.view === view || (view === 'task-detail' && button.dataset.view === 'tasks')));
    if (view === 'tasks') { fillProjectSelect(); $('new-task-button').disabled = !state.projectId; $('refresh-tasks').disabled = !state.projectId; $('project-select')?.focus(); }
  }

  function issuedCredentialFeedback(data) {
    const credentialId = data?.credential_id || data?.id || 'the issued credential';
    if (data?.secret_unavailable) return `This issuance was replayed, but its token cannot be recovered. Revoke ${credentialId}, then issue a replacement with a fresh agent name.`;
    if (!data?.token) return `The token was not returned. Revoke ${credentialId}, then issue a replacement with a fresh agent name.`;
    return 'Credential issued. Copy the token now; it will not be shown again.';
  }

  function restoredMutationCallbacks(operation, context = {}) {
    if (operation === 'clock_change') return async () => { await loadClock(); setGlobalAlert('Clock reconciliation recorded. Expired work still requires recovery.', 'success'); };
    if (operation === 'restore_change') return async () => { await loadRestore(); setGlobalAlert('Restore reconciliation recorded.', 'success'); };
    if (operation === 'create_operator') return async () => { await loadOperators(); setGlobalAlert('Operator account created.', 'success'); };
    if (operation === 'change_password') return async () => { signOutLocal(); showLoginError('Password changed. Sign in with your new password.'); };
    if (operation === 'operator_work') return async () => { state.projectId = context.projectId || state.projectId; state.selectedTaskId = context.taskId || ''; if (state.selectedTaskId) { showView('task-detail'); await loadTaskDetail(); } else { showView('tasks'); await loadTasks(); } setGlobalAlert('Update recorded. Inspect the current task and ownership.', 'success'); };
    if (operation === 'operator_access') return async () => { await loadOperators(); setGlobalAlert('Account access updated.', 'success'); };
    if (operation === 'browser_revoke') return async () => { await restoreSession(); setGlobalAlert('Browser session revoked.', 'success'); };
    if (operation === 'rotate_credential') return async data => { showToken(data.token); await loadCredentials(); setIssueFeedback(data.token ? 'Replacement token issued for the same agent. Copy it now.' : 'This replacement token cannot be shown again. Rotate the replacement credential to obtain a new token.', data.token ? 'success' : 'error'); };
    if (operation === 'shared_change') return async (data) => { state.projectId = context.projectId || state.projectId; showView('shared'); if (context.preview) showImportPreview(data, state.projectId); else { await loadShared(); setGlobalAlert('Shared record saved.', 'success'); } };
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
    if (['create_operator','change_password'].includes(saved.operation)) { restoreSecretRequest(saved); return; }
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
    fillSharedProject();
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
    renderTaskActions(data, task); renderLease(data, task); renderCheckpoints(data); renderJobEvidence(data); renderWorkflow(data);
    show($('task-detail-state'), false); show($('task-detail-content'), true);
  }

  function renderLease(data, task) {
    const target = $('lease-content'); clear(target); const attempts = Array.isArray(data.attempts) ? data.attempts : []; const current = attempts.find((attempt) => attempt.id === task.current_attempt_id);
    if (!current) {
      const status = taskStatus(task);
      const message = task.current_attempt_id ? 'Ownership details are still syncing. Refresh to reconcile this task.' : ['done', 'canceled', 'superseded'].includes(status) ? `No active lease. This task is ${displayStatus(status).toLowerCase()}.` : status === 'blocked' ? 'No active lease. This task is blocked and needs attention.' : status === 'planned' ? 'No active lease. This task is planned and is not yet admitted.' : ['waiting_review', 'waiting_integration', 'integrating', 'validating'].includes(status) ? 'The submitted candidate is moving through its review and integration activities below.' : 'No active lease. This task is available for eligible work.';
      add(target, el('p', 'muted', message)); return;
    }
    const dl = el('dl'); const remaining = taskStatus(task) === 'recovery_required' ? 'Ownership ended; recovery required' : current.lease_remaining_ms !== undefined ? `${Math.max(0, Math.round(Number(current.lease_remaining_ms) / 60000))} min remaining` : current.expires_at ? formatLease(current.expires_at) : 'Lease active';
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

  function renderCredentials() {
    const target = $('credentials-list'); clear(target);
    if (!state.credentials.length) { show(target, false); setState($('credentials-state'), 'No agent credentials have been issued.', false); return; }
    show($('credentials-state'), false); show(target, true);
    const offeredReplacement = new Set();
    state.credentials.forEach(credential => {
      const row = el('div', 'credential-row'), info = el('div');
      add(info, el('div', 'credential-name', `${credential.principal_name || credential.name || credential.id} · ${credential.credential_name || 'initial'}`));
      add(info, el('div', 'credential-meta', `Issued ${shortDate(credential.created_at)}`));
      if (credential.revoked_at) add(info, el('div', 'credential-revoked', `Revoked ${shortDate(credential.revoked_at)}`));
      add(row, info);
      if (!credential.revoked_at) {
        add(row, actionButton('Rotate', () => rotateCredential(credential)));
        const revoke = actionButton('Revoke', () => revokeCredential(credential.id)); revoke.className = 'button danger'; add(row, revoke);
      } else if (credential.principal_id && !offeredReplacement.has(credential.principal_id)) {
        offeredReplacement.add(credential.principal_id);
        add(row, actionButton('New token for this agent', () => replaceAgentCredential(credential)));
      }
      add(target, row);
    });
    renderMutationState();
  }
  function revokeCredential(id) { startMutation(`/api/v1/admin/credentials/${encodeURIComponent(id)}/revoke`, {}, 'credential revocation', async () => { await loadCredentials(); setGlobalAlert('Credential revoked.', 'success'); }); }

  function startPolling() { if (state.pollTimer) clearInterval(state.pollTimer); state.pollTimer = setInterval(() => { if (!state.actor || state.mutation) return; if (state.currentView === 'overview') loadProjects(true); else if (state.currentView === 'tasks') loadTasks(true); else if (state.currentView === 'task-detail') loadTaskDetail(true); else if (state.currentView === 'admin') loadCredentials(true); else if (state.currentView === 'resources' && !state.resourcePagesExtended) loadResources(true); }, 5000); }

  $('login-form').addEventListener('submit', (event) => { event.preventDefault(); const username = $('username').value.trim(); const password = $('password').value; if (!username || !password) { showLoginError('Enter your username and password.'); return; } showLoginError(''); startMutation('/api/v1/auth/login', { username, password }, 'sign-in', async (data) => { applySession(data); $('password').value = ''; restorePendingMutation(); await loadProjects(); startPolling(); }, 'POST', (error) => showLoginError(errorMessage(error))); });
  $('logout-button').addEventListener('click', () => startMutation('/api/v1/auth/logout', {}, 'sign-out', async () => signOutLocal()));
  document.querySelectorAll('.nav-item').forEach((button) => button.addEventListener('click', () => { const view = button.dataset.view; showView(view); if (view === 'tasks' && state.projectId) loadTasks(); if (view === 'admin') { loadCredentials(); loadOperators(); loadRestore(); loadClock(); } if (view === 'resources') loadResources(); if (view === 'shared') { fillSharedProject(); loadShared(); } }));
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
      form.addEventListener('submit', event => { event.preventDefault(); if (!form.reportValidity() || (state.mutation && form.dataset.resumeSecret !== 'true')) return; submit(new FormData(form), dialog); });
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
    if (submission.lessons?.length) recordDetails(candidate, 'Lessons saved with this submission — original revisions', submission.lessons);
    if (submission.artifact_ids?.length) recordDetails(candidate, 'Attached artifact IDs — inspect current availability in Knowledge & evidence', submission.artifact_ids);
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
      if (activity.authorization) add(entry, el('p', '', `${activity.authorization.valid === false ? 'Invalidated authorization' : 'Human authorization'}: ${activity.authorization.summary}`));
      const action = (label, callback) => { const button = el('button', 'button subtle', label); button.type = 'button'; button.dataset.mutation = 'true'; button.addEventListener('click', callback); add(entry, button); };
      if (state.actor?.kind === 'human' && !['done','completed','canceled'].includes(activity.status)) {
        if (activity.kind === 'human_review' && !(workflow.blockers || []).length) {
          const current = attempt?.state === 'active' && attempt.valid_by_time === true && attempt.owner_authorized === true;
          if (current && attempt.owner_id === actorId() && attempt.session_id === state.actor.session_id) action('Record human review', () => openHumanReview(activity, submission, attempt));
          else if (!current) action('Claim human review', () => workflowMutation(`${projectPath()}/workflow-activities/${encodeURIComponent(activity.id)}/claim`, {expected_submission_id: submission.id, expected_project_policy_revision: submission.project_policy_revision, expected_workflow_policy_revision: submission.workflow_policy_revision}, 'review claim', response => openHumanReview(activity, submission, response.attempt)));
        }
        if (activity.kind === 'integration') {
          if ((!activity.authorization || activity.authorization.valid === false) && !(workflow.blockers || []).length && workflow.phase === 'integration' && state.projects.find(project => text(project.id) === state.projectId)?.automatic_integration === false) action('Authorize integration', () => openIntegrationAuthorization(activity, submission));
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

  function fillSharedProject() {
    const select = $('shared-project'); clear(select);
    add(select, el('option', '', 'Choose a project')).value = '';
    state.projects.forEach(project => { const option = el('option', '', project.name); option.value = project.id; add(select, option); });
    select.value = state.projectId;
  }
  function sharedWrite(path, body, label, method = 'POST', after = null) {
    const projectId = state.projectId;
    startMutation(path, body, label, async data => {
      if (projectId !== state.projectId) return;
      if (after) after(data); else await loadShared();
    }, method, null, {projectId});
  }
  async function loadShared(append = false) {
    const projectId = state.projectId, currentActor = actorId(), kind = $('shared-kind').value;
    const seq = ++state.sharedSeq;
    show($('context-search'), kind === 'context');
    if (!append) { state.sharedCursor = null; clear($('shared-list')); }
    show($('load-more-shared'), false);
    if (!projectId) { setState($('shared-state'), 'Choose a project to inspect its records.'); return; }
    if (kind === 'context' && !$('context-query').value.trim()) { setState($('shared-state'), 'Enter search text to retrieve relevant context.'); return; }
    setState($('shared-state'), 'Loading records…', true);
    const query = new URLSearchParams({limit:'50'});
    if (append && state.sharedCursor) query.set('cursor', state.sharedCursor);
    if (kind === 'context') { query.set('q', $('context-query').value); query.set('include_shared', String($('context-shared').checked)); query.set('budget', '65536'); }
    try {
      const page = (await request(`${projectPath(projectId)}/${kind}?${query}`)).data;
      if (seq !== state.sharedSeq || projectId !== state.projectId || currentActor !== actorId()) return;
      const items = listData(page);
      items.forEach(item => renderSharedRecord(item, kind));
      state.sharedCursor = page.next_cursor || null;
      show($('load-more-shared'), Boolean(state.sharedCursor));
      show($('shared-state'), !items.length && !append);
      if (!items.length && !append) setState($('shared-state'), page.instructions_complete === false ? 'The context budget cannot include all binding rules. Increase the budget before relying on this packet.' : 'No matching records.');
      setText($('shared-freshness'), `Read ${new Date().toLocaleTimeString()}. Refresh to inspect changes.${page.truncated ? ' Results were bounded; narrow the search for more context.' : ''}`);
      if (kind === 'context' && page.policy?.rules !== undefined) {
        const rules = el('details', 'card'); add(rules, el('summary', '', 'Current binding project rules')); add(rules, el('pre', '', page.policy.rules)); $('shared-list').prepend(rules);
      }
      renderMutationState();
    } catch (error) { if (seq === state.sharedSeq && currentActor === actorId()) setState($('shared-state'), errorMessage(error), false, true); }
  }
  function recordDetails(parent, title, value) {
    if (value === undefined || value === null) return;
    const details = el('details'); add(details, el('summary', '', title));
    add(details, el('pre', '', typeof value === 'string' ? value : JSON.stringify(value, null, 2))); add(parent, details);
  }
  function renderSharedRecord(item, kind) {
    const record = {...(item.record || item)}, card = el('article', 'card'); record.project_id ||= record.source_project_id;
    add(card, el('h2', '', record.title || record.question || record.display_name || record.filename || record.id));
    add(card, el('p', 'muted', [record.kind || kind, record.status || record.availability || record.state, record.revision ? `Revision ${record.revision}` : '', record.project_id && record.project_id !== state.projectId ? `Shared from project ${record.project_id}` : ''].filter(Boolean).join(' · ')));
    add(card, el('p', 'shared-record-body', record.body || record.snippet || record.rationale || record.summary || ''));
    if (record.applicability) add(card, el('p', '', `Applies when: ${record.applicability}`));
    if (record.answer) recordDetails(card, 'Recorded answer', record.answer);
    recordDetails(card, 'Source and applicability', {id:record.id, provenance:record.provenance, scope:record.scope, source:record.source, conditions:record.conditions, affected_tasks:record.affected_tasks, expires_at:record.expires_at});
    const actions = el('div', 'record-actions');
    const button = (label, run, mutation = false) => { const node = el('button', 'button subtle', label); node.type = 'button'; if (mutation) node.dataset.mutation = 'true'; node.addEventListener('click', run); add(actions, node); };
    const ownProject = !record.project_id || record.project_id === state.projectId;
    if (['knowledge','decisions','artifacts'].includes(kind)) button('Inspect history & details', async () => {
      const projectId = state.projectId, currentActor = actorId();
      try { const data = (await request(`${projectPath(projectId)}/${kind}/${encodeURIComponent(record.id)}`)).data; if (projectId === state.projectId && currentActor === actorId()) { recordDetails(card, 'Full record and history', data); card.lastElementChild.open = true; } }
      catch (error) { setGlobalAlert(errorMessage(error)); }
    });
    if (kind === 'knowledge' && ownProject) {
      button('Correct lesson', () => lessonDialog(record), true);
      button('This helped', () => sharedWrite(`${projectPath()}/knowledge/${encodeURIComponent(record.id)}/feedback`, {expected_revision:record.revision,useful:true,comment:'Marked helpful by an operator.'}, 'lesson feedback'), true);
    }
    if (kind === 'decisions' && ownProject && record.required_actor !== 'agent' && record.status === 'pending') button('Record answer', () => decisionAnswer(record), true);
    if (kind === 'decisions' && ownProject && record.status !== 'pending') button('Reopen with current scope', () => reopenDecision(record), true);
    if (kind === 'artifacts') {
      if (record.availability === 'available' && record.kind === 'upload') { const link = el('a', 'button subtle', 'Download attachment'); link.href = `${projectPath()}/artifacts/${encodeURIComponent(record.id)}/content`; link.download = ''; add(actions, link); }
      if (record.external_url) { try { const url = new URL(record.external_url); if (url.protocol === 'https:' && !url.username && !url.password) { const link = el('a', 'button subtle', 'Open external evidence'); link.href = url.href; link.rel = 'noopener noreferrer'; link.target = '_blank'; add(actions, link); } } catch (_) { /* Show unusable links as metadata only. */ } }
      add(card, el('p', 'muted', `${record.size_bytes ?? 'Unknown'} bytes · ${record.sha256 || 'No content digest'}${record.kind === 'external_link' ? ' · External availability is not verified by the service.' : ''}`));
    }
    add(card, actions); add($('shared-list'), card);
  }
  function lessonDialog(existing = null) {
    if (!state.projectId) { setGlobalAlert('Choose a project first.'); return; }
    const projectId = state.projectId, view = workflowDialog(existing ? 'Correct a lesson' : 'Add a lesson', 'Record what was learned, when it applies, and the evidence behind it. Lessons do not change binding project rules.');
    view.field('title', 'Title', existing?.title || '', 'input').maxLength = 255; view.field('body', 'Lesson', existing?.body || '').maxLength = 32768;
    view.field('applicability', 'When this applies', existing?.applicability || '').required = false;
    view.field('summary', 'Source or reason for this correction', existing?.provenance?.summary || '');
    const status = view.field('status', 'Evidence status', '', 'select');
    for (const value of ['observed','validated','deprecated']) { const option = el('option', '', displayStatus(value)); option.value = value; add(status, option); } status.value = existing?.status || 'observed';
    view.finish(existing ? 'Save correction' : 'Save lesson', (values, dialog) => {
      const body = {title:values.get('title'),body:values.get('body'),status:values.get('status'),scope:existing?.scope || {},tags:existing?.tags || [],applicability:values.get('applicability'),provenance:{...(existing?.provenance || {}),summary:values.get('summary')}};
      if (existing) { body.expected_revision = existing.revision; body.superseded_by_id = existing.superseded_by_id || null; } else { body.kind = 'lesson'; body.collection = 'project'; body.share_across_projects = false; }
      dialog.close(); sharedWrite(`${projectPath(projectId)}/knowledge${existing ? `/${encodeURIComponent(existing.id)}` : ''}`, body, 'lesson', existing ? 'PATCH' : 'POST');
    });
  }
  function decisionAnswer(record) {
    const projectId = state.projectId, view = workflowDialog('Answer a scoped decision', record.question);
    add(view.form, el('p', 'muted', `Environment: ${record.environment || 'Not specified'}. Conditions: ${record.conditions || 'Not specified'}. This answer applies only to the recorded task and policy revisions.`));
    const disposition = view.field('disposition', 'Effect on the scoped work', '', 'select');
    for (const [value,label] of [['defer','Keep blocked — answer later'],['deny','Keep blocked — do not proceed'],['allow','Allow work within the recorded scope']]) { const option = el('option', '', label); option.value = value; add(disposition, option); }
    const answer = view.field('answer', 'Answer', '', 'select'); for (const value of record.options || []) { const option = el('option', '', value); option.value = value; add(answer, option); }
    view.field('rationale', 'Reason and supporting evidence');
    const confirmed = view.field('conditions_confirmed', 'I verified the recorded environment and conditions for allowing work', '', 'input'); confirmed.type = 'checkbox'; confirmed.required = false;
    view.finish('Record answer', (values, dialog) => { dialog.close(); sharedWrite(`${projectPath(projectId)}/decisions/${encodeURIComponent(record.id)}/answer`, {expected_generation:record.generation,disposition:values.get('disposition'),answer:values.get('answer'),rationale:values.get('rationale'),conditions_confirmed:confirmed.checked}, 'decision answer'); });
  }
  function importDialog() {
    if (!state.projectId) { setGlobalAlert('Choose a project first.'); return; }
    const projectId = state.projectId, view = workflowDialog('Preview Markdown import', 'Paste a bounded source section. Unchecked checklist items become planned tasks. Completed items retain closure. Ordinary prose remains historical context. Review the preview before applying.');
    view.field('context', 'Stable source identity', '', 'input'); view.field('path', 'Source path', 'HANDOFF.md', 'input'); view.field('git_revision', 'Source Git revision', '', 'input');
    view.field('branch', 'Source branch', '', 'input').required = false; view.field('markdown', 'Markdown source section').maxLength = 60000;
    view.finish('Preview import', (values, dialog) => {
      const body = {source:{context:values.get('context'),git_revision:values.get('git_revision'),observed_at:new Date().toISOString(),branch:values.get('branch'),environment:''},chunks:[{path:values.get('path'),markdown:values.get('markdown')}],historical_mappings:[]};
      dialog.close(); startMutation(`${projectPath(projectId)}/imports/preview`, body, 'import preview', data => showImportPreview(data, projectId), 'POST', null, {projectId,preview:true});
    });
  }
  async function reopenDecision(record) {
    const projectId = state.projectId, currentActor = actorId();
    try {
      const [orientation, detail] = await Promise.all([request(`${projectPath(projectId)}/orientation`), request(`${projectPath(projectId)}/decisions/${encodeURIComponent(record.id)}`)]);
      if (projectId !== state.projectId || currentActor !== actorId()) return;
      const current = detail.data;
      const view = workflowDialog('Reopen this decision', 'The previous answer remains in history. This opens a new question cycle for the same tasks at their current revisions. A new answer is required before work may proceed.');
      recordDetails(view.form, 'Tasks and their current revisions', current.affected_tasks);
      view.field('rationale', 'Why the decision needs to be revisited');
      view.field('environment', 'Environment', current.environment || '').required = false;
      view.field('conditions', 'Conditions', current.conditions || '').required = false;
      const expiry = view.field('expires_at', 'Expiration, if needed', '', 'input'); expiry.type = 'datetime-local'; expiry.required = false;
      view.finish('Reopen decision', (values, dialog) => {
        dialog.close(); sharedWrite(`${projectPath(projectId)}/decisions/${encodeURIComponent(record.id)}/reopen`, {expected_generation:current.generation,policy_revision:orientation.data.policy_revision,affected_tasks:current.affected_tasks.map(task => ({task_id:task.task_id,task_revision:task.current_revision})),rationale:values.get('rationale'),environment:values.get('environment'),conditions:values.get('conditions'),expires_at:expiry.value ? new Date(expiry.value).getTime() : null}, 'decision reopening');
      });
    } catch (error) { setGlobalAlert(errorMessage(error)); }
  }
  async function bindingRules() {
    if (!state.projectId) { setGlobalAlert('Choose a project first.'); return; }
    const projectId = state.projectId, currentActor = actorId();
    try {
      const [orientation, history] = await Promise.all([request(`${projectPath(projectId)}/orientation`), request(`${projectPath(projectId)}/policy/history?limit=50`)]);
      if (projectId !== state.projectId || currentActor !== actorId()) return;
      const project = orientation.data.project;
      const view = workflowDialog('Binding project rules', 'These instructions govern work on this project. Changing them creates a new policy revision; agents must read and acknowledge it before claiming work.');
      view.field('rules', 'Rules', project.rules).maxLength = 32768;
      view.field('provenance', 'Reason and supporting source').maxLength = 4096;
      recordDetails(view.form, 'Policy history — first 50 revisions', history.data);
      view.finish('Save binding rules', (values, dialog) => {
        dialog.close(); startMutation(`${projectPath(projectId)}/policy`, {expected_revision:project.policy_revision,review_mode:project.review_mode,recovery_mode:project.recovery_mode,lease_seconds:project.lease_seconds,rules:values.get('rules'),provenance:values.get('provenance'),agent_rule_editing:project.agent_rule_editing,automatic_integration:project.automatic_integration}, 'binding rules', async () => { await loadProjects(); setGlobalAlert('Binding rules saved with their reason and source.', 'success'); }, 'PATCH', null, {projectId});
      });
    } catch (error) { setGlobalAlert(errorMessage(error)); }
  }
  function showImportPreview(data, projectId) {
    const preview = data.preview || data;
    const view = workflowDialog('Review import preview', 'Apply exactly this preview. If service records change, create and review a fresh preview. Imported instructions do not change project policy.');
    add(view.form, el('p', 'muted', `Source: ${preview.source?.context || 'Unspecified'} · ${preview.source?.branch || 'No branch'} · observed ${preview.source?.observed_at || 'Unknown'}`));
    for (const item of preview.items || []) {
      const card = el('article', 'shared-card');
      add(card, el('h3', '', item.title));
      add(card, el('p', 'muted', `${item.record_kind} · ${item.disposition} · ${item.source_path} / ${item.section_identity}`));
      if (item.evidence) add(card, el('p', '', item.evidence));
      add(view.form, card);
    }
    if (!(preview.items || []).length) add(view.form, el('p', 'muted', 'No records were proposed.'));
    if ((preview.conflicts || []).length) add(view.form, el('p', 'alert', 'Conflicts prevent applying this preview. Inspect the details and correct the source.'));
    if ((preview.unresolved_links || []).length) add(view.form, el('p', 'muted', 'Some source links could not be resolved; inspect them before applying.'));
    recordDetails(view.form, 'Full source, conflicts, and record identities', preview);
    view.finish('Apply preview', (_, dialog) => { dialog.close(); sharedWrite(`${projectPath(projectId)}/imports/${encodeURIComponent(preview.id)}/apply`, {preview_digest:preview.digest,expected_project_event_revision:preview.project_event_revision}, 'Markdown import'); });
  }
  async function exportSnapshot() {
    if (!state.projectId) { setGlobalAlert('Choose a project first.'); return; }
    const projectId = state.projectId, currentActor = actorId();
    try {
      const pages = []; let cursor = null, bytes = 0;
      do { const page = (await request(`${projectPath(projectId)}/exports?limit=200${cursor ? `&cursor=${encodeURIComponent(cursor)}` : ''}`)).data; if (typeof page.markdown !== 'string') throw new ApiError('The export omitted its Markdown.', 200, 'invalid_response'); bytes += new TextEncoder().encode(page.markdown).length; if (bytes > 16 * 1024 * 1024) throw new ApiError('This snapshot exceeds the browser’s 16 MiB download limit. Use the CLI to export individual pages.', 413, 'export_limit'); pages.push(page.markdown); cursor = page.next_cursor; } while (cursor);
      if (projectId !== state.projectId || currentActor !== actorId()) return;
      const url = URL.createObjectURL(new Blob([pages.join('\n')], {type:'text/markdown;charset=utf-8'}));
      const link = el('a'); link.href = url; link.download = `coordinator-${projectId}.md`; add(document.body, link); link.click(); link.remove(); setTimeout(() => URL.revokeObjectURL(url), 1000);
      setGlobalAlert('Snapshot downloaded with provenance and generated-file markers.', 'success');
    } catch (error) { setGlobalAlert(errorMessage(error)); }
  }
  $('shared-project').addEventListener('change', event => { state.projectId = event.target.value; state.taskCursor = null; state.tasks = []; state.selectedTaskId = ''; loadShared(); });
  $('shared-kind').addEventListener('change', () => loadShared());
  $('refresh-shared').addEventListener('click', () => loadShared());
  $('load-more-shared').addEventListener('click', () => loadShared(true));
  $('context-search').addEventListener('submit', event => { event.preventDefault(); loadShared(); });
  $('new-lesson').addEventListener('click', () => lessonDialog());
  $('binding-rules').addEventListener('click', bindingRules);
  $('import-markdown').addEventListener('click', importDialog);
  $('export-markdown').addEventListener('click', exportSnapshot);

  // Operator tools use the same guarded operations as agents.
  function actionButton(label, action, mutation = true) {
    const button = el('button', 'button subtle', label); button.type = 'button';
    if (mutation) button.dataset.mutation = 'true';
    button.addEventListener('click', action); return button;
  }
  function selectField(view, name, label, value, choices) {
    const select = view.field(name, label, '', 'select');
    choices.forEach(([key, caption]) => { const option = el('option', '', caption); option.value = key; add(select, option); });
    select.value = String(value); return select;
  }
  function lines(value) { return text(value).split('\n').map(line => line.trim()).filter(Boolean); }
  async function openProjectPolicy() {
    if (!state.projectId) { setGlobalAlert('Choose a project first.'); return; }
    const projectId = state.projectId, currentActor = actorId();
    try {
      const project = (await request(`${projectPath(projectId)}/orientation`)).data.project;
      if (state.projectId !== projectId || actorId() !== currentActor) return;
      const view = workflowDialog('Project policy', 'Changes create a new policy revision. Agents must reread the policy before claiming work; existing submissions may need reconciliation.');
      selectField(view, 'review_mode', 'Required review', project.review_mode, [['agent','Independent agent'],['human','Human'],['both','Independent agent and human'],['none','No required review']]);
      selectField(view, 'recovery_mode', 'Expired work recovery', project.recovery_mode, [['agent','Agents may inspect and recover'],['manual','A human must inspect and recover']]);
      selectField(view, 'automatic_integration', 'Integration authorization', project.automatic_integration, [['false','Human authorization for each candidate'],['true','Agents may integrate approved candidates']]);
      selectField(view, 'agent_rule_editing', 'Agent policy changes', project.agent_rule_editing, [['false','Only humans may change binding rules'],['true','Agents may change binding project rules']]);
      const lease = view.field('lease_seconds','Ownership lease (seconds)',project.lease_seconds,'input'); lease.type = 'number'; lease.min = '30'; lease.max = '3600';
      const rules = view.field('rules','Binding rules',project.rules); rules.maxLength = 32768; rules.required = false;
      view.field('provenance','Reason and supporting source').maxLength = 4096;
      view.finish('Save policy', (values, dialog) => {
        const body = {expected_revision:project.policy_revision, review_mode:values.get('review_mode'), recovery_mode:values.get('recovery_mode'), automatic_integration:values.get('automatic_integration') === 'true', agent_rule_editing:values.get('agent_rule_editing') === 'true', lease_seconds:Number(values.get('lease_seconds')), rules:values.get('rules'), provenance:values.get('provenance')};
        dialog.close(); startMutation(`${projectPath(projectId)}/policy`, body, 'project policy', async () => { await loadProjects(); setGlobalAlert('Project policy saved.', 'success'); }, 'PATCH', null, {projectId});
      });
    } catch (error) { setGlobalAlert(errorMessage(error)); }
  }
  function renderTaskActions(data, task) {
    const target = $('task-operator-actions'); clear(target);
    if (!task.current_attempt_id && ['open','planned'].includes(task.lifecycle) && ['ready','planned','blocked'].includes(taskStatus(task)) && !task.activity_kind) add(target, actionButton('Edit task', () => editTask(task, data)));
    if (!task.current_attempt_id && task.blocked_reason) add(target, actionButton('Resolve blocker', () => {
      const projectId = state.projectId, taskId = task.id;
      const view = workflowDialog('Resolve saved blocker', task.blocked_reason);
      view.field('reason','Resolution evidence');
      view.finish('Record resolution', (values, dialog) => { dialog.close(); startMutation(`${projectPath(projectId)}/tasks/${encodeURIComponent(taskId)}/unblock`, {expected_revision:task.revision,reason:values.get('reason')}, 'blocker resolution', () => loadTaskDetail(), 'POST', null, {projectId,taskId}); });
    }));
    if (taskStatus(task) === 'recovery_required' && !task.activity_kind) add(target, actionButton('Inspect for recovery', () => claimRecovery(task)));
    const attempt = (data.attempts || []).find(item => item.id === task.current_attempt_id);
    if (attempt && attempt.owner_id === actorId() && attempt.session_id === state.actor?.session_id && attempt.state === 'active' && taskStatus(task) !== 'recovery_required') {
      if (attempt.mode === 'recovery') add(target, actionButton('Review recovery evidence', () => recoveryEvidence(task, attempt)));
      add(target, actionButton('Renew my ownership', () => workflowMutation(`${projectPath()}/attempts/${encodeURIComponent(attempt.id)}/renew`, {generation:attempt.generation}, 'ownership renewal')));
      add(target, actionButton('Release with handoff', () => releaseOwnedAttempt(attempt)));
    }
    if (task.objective_id) add(target, actionButton('View objective children', () => showObjective(task.objective_id), false));
    if (task.parent_objective_id) add(target, actionButton('View parent objective', () => showObjective(task.parent_objective_id), false));
    renderMutationState();
  }
  function editTask(task, data) {
    const projectId = state.projectId, taskId = task.id;
    const view = workflowDialog('Edit task', 'Only unowned work can be edited. Admitting planned work makes it eligible when its dependencies and other requirements are satisfied.');
    view.field('title','Title',task.title,'input').maxLength = 300;
    const description = view.field('description','Description',task.description); description.required = false; description.maxLength = 32768;
    view.field('criteria','Acceptance criteria — one per line',(task.acceptance_criteria || []).join('\n')).maxLength = 205000;
    const dependencies = task.depends_on || data.depends_on || data.dependencies || [];
    view.field('depends_on','Prerequisite task IDs — one per line',dependencies.map(item => typeof item === 'string' ? item : item.prerequisite_id || item.id).join('\n')).required = false;
    const priority = view.field('priority','Priority (0 highest, 3 lowest)',task.priority,'input'); priority.type = 'number'; priority.min = '0'; priority.max = '3';
    selectField(view,'planned','Admission',task.lifecycle === 'planned',[['true','Keep planned'],['false','Admit to ready queue']]);
    view.finish('Save task', (values, dialog) => {
      dialog.close(); startMutation(`${projectPath(projectId)}/tasks/${encodeURIComponent(taskId)}`, {expected_revision:task.revision,title:values.get('title'),description:values.get('description'),acceptance_criteria:lines(values.get('criteria')),depends_on:lines(values.get('depends_on')),priority:Number(values.get('priority')),planned:values.get('planned') === 'true'}, 'task edit', () => loadTaskDetail(), 'PATCH', null, {projectId,taskId});
    });
  }
  async function claimRecovery(task) {
    const projectId = state.projectId, taskId = task.id, currentActor = actorId();
    try {
      const orientation = (await request(`${projectPath(projectId)}/orientation`)).data;
      if (state.projectId !== projectId || state.selectedTaskId !== taskId || actorId() !== currentActor) return;
      const view = workflowDialog('Begin recovery inspection', 'This reserves an inspection attempt. Inspect saved source, checkpoints, and still-running jobs before deciding how work continues. Unknown jobs keep their resource holds.');
      recordDetails(view.form,'Current project rules',orientation.project);
      view.finish('Claim recovery inspection', (_, dialog) => { dialog.close(); startMutation(`${projectPath(projectId)}/claims`, {task_id:taskId,expected_task_revision:task.revision,mode:'recovery',policy_revision:orientation.project.policy_revision,instruction_version:orientation.instruction_version}, 'recovery inspection', async data => { await loadTaskDetail(); if (data.current_authority?.valid === false) setGlobalAlert('The saved claim no longer grants ownership. Inspect the current task.'); }, 'POST', null, {projectId,taskId}); });
    } catch (error) { setGlobalAlert(errorMessage(error)); }
  }
  async function recoveryEvidence(task, attempt) {
    const projectId = state.projectId, currentActor = actorId();
    try {
      const evidence = (await request(`${projectPath(projectId)}/attempts/${encodeURIComponent(attempt.id)}`)).data;
      if (projectId !== state.projectId || task.id !== state.selectedTaskId || actorId() !== currentActor) return;
      const view = workflowDialog('Resolve recovery inspection', 'Inspect the original worktree or remote checkpoint and account for every producer. The service checks unresolved job and resource holds before allowing work to resume.');
      recordDetails(view.form,'Inspection ownership',evidence);
      recordDetails(view.form,'Recent saved work, checkpoints, and job holds',state.detail);
      add(view.form,actionButton('Browse older task evidence',taskHistory,false));
      selectField(view,'disposition','Continue work', 'resume', [['resume','Resume saved work'],['restart','Restart implementation after inspection']]);
      view.field('summary','Inspection findings and next steps');
      for (const [name,label] of [['saved_work_checked','I checked saved work and its source checkpoints'],['running_jobs_checked','I checked still-running and uncertain jobs']]) { const input = view.field(name,label,'','input'); input.type = 'checkbox'; input.value = 'checked'; }
      view.finish('Record recovery decision', (values, dialog) => { dialog.close(); workflowMutation(`${projectPath(projectId)}/attempts/${encodeURIComponent(attempt.id)}/recovery-resolution`, {generation:attempt.generation,disposition:values.get('disposition'),summary:values.get('summary'),saved_work_checked:values.get('saved_work_checked') === 'checked',running_jobs_checked:values.get('running_jobs_checked') === 'checked'}, 'recovery decision'); });
    } catch (error) { setGlobalAlert(errorMessage(error)); }
  }
  function releaseOwnedAttempt(attempt) {
    const view = workflowDialog('Release ownership with a handoff', 'Release leaves the task unfinished. All jobs must have terminal evidence and resource reservations must be released first.');
    view.field('summary','Work completed and saved source'); view.field('next_step','Next step');
    selectField(view,'blocked','Next owner',false,[['false','Task may be claimed again'],['true','Leave blocked until the issue is resolved']]);
    view.finish('Release ownership', (values, dialog) => { dialog.close(); workflowMutation(`${projectPath()}/attempts/${encodeURIComponent(attempt.id)}/release`, {generation:attempt.generation,summary:`${values.get('summary')}\nNext step: ${values.get('next_step')}`,blocked:values.get('blocked') === 'true'}, 'ownership release'); });
  }
  function taskHistory() {
    const projectId = state.projectId, taskId = state.selectedTaskId, currentActor = actorId();
    const view = workflowDialog('Complete task history', 'Pages preserve a snapshot of inserted records. Start a fresh history view to include newer records.');
    const kinds = ['attempts','checkpoints','checkouts','jobs','job_observations','resources','artifacts','submissions','reviews','integrations','task_revisions','events'];
    const kind = selectField(view,'kind','Record type','attempts',kinds.map(value => [value,displayStatus(value)]));
    const list = add(view.form, el('div')), more = actionButton('Load more records', () => load(true), false); add(view.form, more); let cursor = null, sequence = 0;
    async function load(append = false) {
      const current = ++sequence, selectedKind = kind.value; more.disabled = true;
      try {
        const page = (await request(`${projectPath(projectId)}/tasks/${encodeURIComponent(taskId)}/history?kind=${encodeURIComponent(selectedKind)}&limit=50${append && cursor ? `&cursor=${encodeURIComponent(cursor)}` : ''}`)).data;
        if (current !== sequence || currentActor !== actorId() || !view.dialog.isConnected) return;
        if (!append) clear(list);
        for (const item of page.items || []) recordDetails(list,`${formatDate(item.occurred_at)} · ${displayStatus(item.relation)} · ${text(item.record.summary || item.record.outcome || item.record.definition?.title || item.task_id).slice(0,300)}`,item.record);
        if (!append && !(page.items || []).length) add(list,el('p','muted','No preserved records of this type.'));
        cursor = page.next_cursor; show(more, Boolean(cursor));
      } catch (error) { if (current === sequence) { if (!append) clear(list); add(list,el('p','error',errorMessage(error))); } }
      finally { if (current === sequence) more.disabled = false; }
    }
    kind.addEventListener('change', () => load()); view.finish('Close', (_,dialog) => dialog.close()); load();
  }
  async function objectives() {
    if (!state.projectId) { setGlobalAlert('Choose a project first.'); return; }
    const projectId = state.projectId, currentActor = actorId();
    const view = workflowDialog('Project objectives', 'Each objective is a general task with its own acceptance criteria. Required child tasks must complete before its own work and review can finish.');
    add(view.form, actionButton('New objective', () => { view.dialog.close(); objectiveDialog(); }));
    const list = add(view.form,el('div')), more = actionButton('Load more objectives', () => load(true), false); add(view.form,more); let cursor = null, busy = false;
    async function load(append = false) {
      if (busy) return; busy = true; more.disabled = true;
      try {
        const page = (await request(`${projectPath(projectId)}/objectives?limit=50${append && cursor ? `&cursor=${encodeURIComponent(cursor)}` : ''}`)).data;
        if (!view.dialog.isConnected || actorId() !== currentActor) return;
        if (!append) clear(list);
        for (const item of page.items || []) add(list,actionButton(`${item.title || item.id} · Required children ${item.completed_required_child_count}/${item.required_child_count} · ${displayStatus(item.work_status)}`, () => { view.dialog.close(); showObjective(item.id || item.task_id); },false));
        if (!append && !(page.items || []).length) add(list,el('p','muted','No objectives yet.'));
        cursor = page.next_cursor; show(more, Boolean(cursor));
      } catch (error) { add(list,el('p','error',errorMessage(error))); }
      finally { busy = false; more.disabled = false; }
    }
    view.finish('Close', (_,dialog) => dialog.close()); load();
  }
  async function showObjective(id) {
    const projectId = state.projectId, currentActor = actorId();
    try {
      const data = (await request(`${projectPath(projectId)}/objectives/${encodeURIComponent(id)}`)).data;
      if (projectId !== state.projectId || actorId() !== currentActor) return;
      const objective = data.objective || data;
      const view = workflowDialog(objective.title || 'Objective', 'Required children gate objective work. The objective needs its own acceptance evidence and required review.');
      for (const child of objective.children || data.children || []) add(view.form,actionButton(`${child.title || child.task_id} · ${child.required ? 'Required' : 'Optional'} · ${displayStatus(child.work_status || child.lifecycle || child.status)}`, () => { view.dialog.close(); openTask(child.task_id); },false));
      add(view.form,actionButton('Open objective task', () => { view.dialog.close(); openTask(objective.task_id || objective.id || id); },false));
      if (!objective.membership_frozen) add(view.form,actionButton('Edit child membership', () => { view.dialog.close(); objectiveDialog({...objective,id,children:objective.children || data.children || []}); }));
      recordDetails(view.form,'Objective record and membership history',data);
      let cursor = objective.membership_history_next_cursor;
      const more = actionButton('Load older membership history', async () => {
        more.disabled = true;
        try {
          const page = (await request(`${projectPath(projectId)}/objectives/${encodeURIComponent(id)}?cursor=${encodeURIComponent(cursor)}&limit=50`)).data;
          if (!view.dialog.isConnected || actorId() !== currentActor) return;
          recordDetails(view.form,'Earlier membership revisions',page.membership_history);
          cursor = page.membership_history_next_cursor; show(more,Boolean(cursor));
        } catch (error) { setGlobalAlert(errorMessage(error)); }
        finally { more.disabled = false; }
      },false); add(view.form,more); show(more,Boolean(cursor));
      view.finish('Close', (_,dialog) => dialog.close());
    } catch (error) { setGlobalAlert(errorMessage(error)); }
  }
  function objectiveDialog(existing = null) {
    const projectId = state.projectId;
    const view = workflowDialog(existing ? 'Edit objective children' : 'New objective', 'Use task IDs from this project. A task has at most one parent objective. Membership freezes once objective work begins.');
    if (!existing) { view.field('title','Title','','input'); view.field('description','Description').required = false; view.field('criteria','Acceptance criteria — one per line'); }
    view.field('required','Required child task IDs — one per line',(existing?.children || []).filter(child => child.required).map(child => child.task_id).join('\n')).required = false;
    view.field('optional','Optional child task IDs — one per line',(existing?.children || []).filter(child => !child.required).map(child => child.task_id).join('\n')).required = false;
    view.finish(existing ? 'Save membership' : 'Create objective', (values, dialog) => {
      const children = [...lines(values.get('required')).map(task_id => ({task_id,required:true})),...lines(values.get('optional')).map(task_id => ({task_id,required:false}))];
      const body = existing ? {expected_revision:existing.objective_revision,children} : {title:values.get('title'),description:values.get('description'),acceptance_criteria:lines(values.get('criteria')),children,priority:2,planned:false};
      dialog.close(); startMutation(`${projectPath(projectId)}/objectives${existing ? `/${encodeURIComponent(existing.id)}/children` : ''}`,body,'objective',async () => { await loadTasks(); setGlobalAlert('Objective saved.', 'success'); },existing ? 'PATCH' : 'POST',null,{projectId});
    });
  }
  $('project-policy-button').addEventListener('click',openProjectPolicy);
  $('task-history-button').addEventListener('click',taskHistory);
  $('objective-list-button').addEventListener('click',objectives);

  async function loadOperators(append = false, cursor = null) {
    if (state.actor?.role !== 'admin') return;
    const currentActor = actorId(), target = $('operators-list');
    try {
      const page = (await request(`/api/v1/admin/operators${cursor ? `?cursor=${encodeURIComponent(cursor)}` : ''}`)).data;
      if (actorId() !== currentActor || state.actor?.role !== 'admin') return;
      if (!append) clear(target);
      for (const operator of page.items || []) {
        const row = el('div','credential-row'); add(row,el('span','',`${operator.name} · ${displayStatus(operator.role)} · ${operator.enabled ? 'Enabled' : 'Disabled'}`));
        add(row,actionButton('Change access', () => {
          const view = workflowDialog(`Access for ${operator.name}`, 'Access changes end this account’s existing browser sessions. The last enabled administrator must remain enabled with administrator access.');
          selectField(view,'role','Role',operator.role,[['operator','Operator'],['admin','Administrator']]);
          selectField(view,'enabled','Account access',operator.enabled,[['true','Enabled'],['false','Disabled']]);
          view.finish('Save access', (values, dialog) => { dialog.close(); startMutation(`/api/v1/admin/operators/${encodeURIComponent(operator.id)}/access`, {expected_revision:operator.revision,role:values.get('role'),enabled:values.get('enabled') === 'true'},'account access',async data => { if (operator.id === actorId() && data.sessions_revoked) { signOutLocal(); showLoginError('Access updated. Sign in again if your account remains enabled.'); } else await loadOperators(); }); });
        }));
        add(row,actionButton('Browser sessions', () => browserSessions(operator.id,operator.name),false)); add(target,row);
      }
      target.querySelector('[data-more-operators]')?.remove();
      if (page.next_cursor) { const more = actionButton('Load more accounts', () => { more.disabled = true; loadOperators(true,page.next_cursor); },false); more.dataset.moreOperators = 'true'; add(target,more); }
      renderMutationState();
    } catch (error) { setGlobalAlert(errorMessage(error)); }
  }
  function createOperator() {
    const view = workflowDialog('Add operator', 'All authenticated people and agents can access every project. Administrators also manage operator accounts and agent credentials.');
    view.field('name','Username','','input').maxLength = 100;
    selectField(view,'role','Role','operator',[['operator','Operator'],['admin','Administrator']]);
    const password = view.field('password','Initial password (at least 12 characters)','','input'); password.type = 'password'; password.autocomplete = 'new-password'; password.maxLength = 1024; password.minLength = 12;
    const confirmation = view.field('confirmation','Confirm initial password','','input'); confirmation.type = 'password'; confirmation.autocomplete = 'new-password';
    view.finish('Create account', (values, dialog) => {
      if (values.get('password') !== values.get('confirmation')) { confirmation.setCustomValidity('Passwords must match.'); confirmation.reportValidity(); confirmation.addEventListener('input', () => confirmation.setCustomValidity(''),{once:true}); return; }
      const body = {name:values.get('name'),role:values.get('role'),password:values.get('password')};
      dialog.close(); startMutation('/api/v1/admin/operators',body,'operator creation',async () => { await loadOperators(); setGlobalAlert('Operator account created.', 'success'); });
    });
  }
  async function myAccount() {
    const currentActor = actorId();
    try {
      const operator = (await request('/api/v1/auth/account')).data.operator;
      if (actorId() !== currentActor) return;
      const view = workflowDialog('My account', `${operator.name} · ${displayStatus(operator.role)}. Changing your password signs out every browser session for this account.`);
      add(view.form,actionButton('View my browser sessions', () => { view.dialog.close(); browserSessions(operator.id,operator.name); },false));
      const current = view.field('current_password','Current password','','input'); current.type = 'password'; current.autocomplete = 'current-password'; current.maxLength = 1024;
      const next = view.field('new_password','New password (at least 12 characters)','','input'); next.type = 'password'; next.autocomplete = 'new-password'; next.maxLength = 1024; next.minLength = 12;
      const confirmation = view.field('confirmation','Confirm new password','','input'); confirmation.type = 'password'; confirmation.autocomplete = 'new-password';
      view.finish('Change password and sign out', (values, dialog) => {
        if (values.get('new_password') !== values.get('confirmation')) { confirmation.setCustomValidity('Passwords must match.'); confirmation.reportValidity(); confirmation.addEventListener('input', () => confirmation.setCustomValidity(''),{once:true}); return; }
        const body = {expected_revision:operator.revision,current_password:values.get('current_password'),new_password:values.get('new_password')};
        dialog.close(); startMutation('/api/v1/auth/password',body,'password change',async () => { signOutLocal(); showLoginError('Password changed. Sign in with your new password.'); }, 'POST', error => { if (error.status === 401) showLoginError('This session could not authorize the password change. Sign in again. If an earlier attempt had an uncertain response, try the new password.'); else setGlobalAlert(error.uncertain ? 'The password change may have succeeded. Retry the retained request, or reload and sign in with the new password to inspect your account.' : errorMessage(error),'error',error.uncertain); });
      });
    } catch (error) { setGlobalAlert(errorMessage(error)); }
  }
  function browserSessions(principalId, name) {
    const currentActor = actorId(), view = workflowDialog(`Browser sessions for ${name}`, 'Revoking a session signs out that browser. This does not replace or recreate task ownership.');
    const list = add(view.form,el('div')), more = actionButton('Load more sessions', () => load(true),false); add(view.form,more); let cursor = null, busy = false;
    async function load(append = false) {
      if (busy) return; busy = true; more.disabled = true;
      try {
        const page = (await request(`/api/v1/browser-sessions?principal_id=${encodeURIComponent(principalId)}${append && cursor ? `&cursor=${encodeURIComponent(cursor)}` : ''}`)).data;
        if (!view.dialog.isConnected || actorId() !== currentActor) return;
        if (!append) clear(list);
        for (const session of page.items || []) {
          const row = el('div','shared-card'); add(row,el('p','',`${session.current ? 'This browser' : session.id} · Created ${formatDate(session.created_at)} · Expires ${formatDate(session.expires_at)}`));
          if (session.revoked_at) add(row,el('p','muted',`Revoked ${formatDate(session.revoked_at)}`));
          else add(row,actionButton('Revoke session', () => startMutation(`/api/v1/browser-sessions/${encodeURIComponent(session.id)}/revoke`,{},'browser session revocation',async () => { if (session.current) { view.dialog.close(); signOutLocal(); } else await load(); })));
          add(list,row);
        }
        cursor = page.next_cursor; show(more,Boolean(cursor)); renderMutationState();
      } catch (error) { add(list,el('p','error',errorMessage(error))); }
      finally { busy = false; more.disabled = false; }
    }
    view.finish('Close', (_,dialog) => dialog.close()); load();
  }
  function rotateCredential(credential) {
    const view = workflowDialog('Rotate agent credential', 'The replacement retains the same agent identity. Revoking the old credential ends its sessions and task authority; recovery must inspect saved work and jobs.');
    view.field('name','Replacement credential name',credential.credential_name || credential.name || credential.principal_name || 'replacement','input').maxLength = 100;
    selectField(view,'revoke_old','Old credential','true',[['true','Revoke when replacement is issued'],['false','Keep active for a staged transition']]);
    view.finish('Issue replacement token', (values,dialog) => { dialog.close(); startMutation(`/api/v1/admin/credentials/${encodeURIComponent(credential.id)}/rotate`,{name:values.get('name'),revoke_old:values.get('revoke_old') === 'true'},'credential rotation',restoredMutationCallbacks('rotate_credential')); });
  }
  function restoreSecretRequest(saved) {
    if (saved.path === '/api/v1/auth/password') {
      clearPersistedMutation();
      setGlobalAlert('A password change had an uncertain response. If your old session ended, sign in with the new password and inspect My account. Passwords were not saved in this browser.'); return;
    }
    const view = workflowDialog('Resume account creation', 'Re-enter the original initial password to retry the saved request. The request key, username, and role are preserved; the password was not saved.');
    add(view.form,el('p','',`${saved.body.name} · ${displayStatus(saved.body.role)}`));
    const password = view.field('password','Original initial password','','input'); password.type = 'password'; password.autocomplete = 'new-password';
    // Reserve the original operation until the operator supplies its missing secret.
    state.mutation = { ...saved, label:'operator creation', inFlight:false, awaitingSecret:true };
    view.finish('Retry original creation', (values,dialog) => {
      const body = {...saved.body,password:values.get('password')}; dialog.close();
      state.mutation = {...saved,body,label:'operator creation',onSuccess:restoredMutationCallbacks('create_operator'),onError:null,inFlight:false};
      executeMutation(state.mutation);
    });
    view.form.dataset.resumeSecret = 'true';
    renderMutationState();
    view.dialog.addEventListener('close', () => {
      if (state.mutation?.awaitingSecret) {
        setGlobalAlert('Account creation remains pending. Re-enter the original password to resume it.');
        add($('global-alert'), actionButton('Re-enter original password', () => restoreSecretRequest(saved), false));
      }
    });

  }
  $('account-button').addEventListener('click',myAccount);
  $('new-operator-button').addEventListener('click',createOperator);
  $('refresh-operators').addEventListener('click',() => loadOperators());

  let clockSequence = 0;
  async function loadClock() {
    if (state.actor?.role !== 'admin') return;
    const currentActor = actorId(), sequence = ++clockSequence;
    try {
      const data = (await request('/api/v1/admin/clock')).data;
      if (sequence !== clockSequence || currentActor !== actorId()) return;
      const target = $('clock-status'); clear(target);
      const paused = data.clock_state.status !== 'ready';
      add(target, el('p', '', paused
        ? 'Coordination is paused because the server clock moved backward. Correct and verify the host clock before reconciling this incident. Expired ownership and preserved holds still require recovery.'
        : 'No unresolved server clock incident. Task leases use protected server time.'));
      recordDetails(target, 'Clock status and incident evidence', data);
      if (paused) add(target, actionButton('Reconcile corrected clock', () => {
        const view = workflowDialog('Reconcile server clock', 'Record how trustworthy server time was restored. The service checks its stored time boundary before clearing this pause; old ownership is not revived.');
        view.field('reason','Clock correction evidence').maxLength = 2000;
        view.finish('Record clock reconciliation', (values, dialog) => {
          dialog.close(); startMutation('/api/v1/admin/clock/reconcile', {incident_id:data.clock_state.incident_id,reason:values.get('reason')}, 'clock reconciliation', restoredMutationCallbacks('clock_change'));
        });
      }));
      renderMutationState();
    } catch (error) { if (sequence === clockSequence && currentActor === actorId()) setGlobalAlert(errorMessage(error)); }
  }
  $('refresh-clock').addEventListener('click', () => loadClock());

  let restoreSequence = 0;
  async function loadRestore(cursor = null) {
    if (state.actor?.role !== 'admin') return;
    const currentActor = actorId(), sequence = ++restoreSequence;
    const target = $('restore-status'), list = $('restore-requirements');
    try {
      const data = (await request(`/api/v1/admin/restore?limit=50${cursor ? `&cursor=${encodeURIComponent(cursor)}` : ''}`)).data;
      if (sequence !== restoreSequence || currentActor !== actorId()) return;
      if (!cursor) { clear(target); clear(list); }
      const service = data.service_state, restoreId = service.restore_id;
      if (!cursor) {
        add(target, el('p', '', service.coordination_state === 'restore_reconciliation'
          ? 'Coordination is paused after restore. Inspect preserved holds, stop the old installation, and reconcile work since the snapshot before resuming.'
          : 'No restore reconciliation is required. Clock, task ownership, and physical resource checks still apply.'));
        if (data.restore) {
          add(target, el('p', 'muted', `${data.restore.inspected}/${data.restore.required_inspections} preserved holds inspected · Old installation ${data.restore.old_installation_fenced ? 'fenced' : 'not yet confirmed stopped'} · Snapshot gap ${data.restore.post_snapshot_gap_reconciled ? 'reconciled' : 'not yet reconciled'}`));
          recordDetails(target, 'Restore record and reconciliation evidence', data.restore);
        }
        if (service.coordination_state === 'restore_reconciliation') {
          if (!data.restore?.old_installation_fenced) add(target, actionButton('Record old installation stopped', () => restoreEvidence(restoreId, 'old-installation-fenced', 'Old installation stopped', 'Record how the original service was stopped or fenced so it cannot grant competing ownership.')));
          if (!data.restore?.post_snapshot_gap_reconciled) add(target, actionButton('Record work since the snapshot', () => restoreEvidence(restoreId, 'post-snapshot-gap', 'Reconcile the snapshot gap', 'Compare source checkpoints, publications, jobs, and completed work after the snapshot. Record what was lost, recovered, or remains uncertain.')));
          add(target, actionButton('Resume coordination', () => restoreEvidence(restoreId, 'finish', 'Resume coordination after restore', 'Every preserved hold must have an inspection record, and both installation fencing and the snapshot gap must be documented. Resuming does not release any physical or integration hold.', true)));
        }
      }
      list.querySelector('[data-more-restore]')?.remove();
      for (const requirement of data.items || []) {
        const row = add(list, el('div', 'shared-card'));
        recordDetails(row, `${displayStatus(requirement.kind)} · ${requirement.target_id} · ${requirement.inspected ? 'Inspected' : 'Needs inspection'}`, requirement);
        const evidenceTask = requirement.detail?.task_id || requirement.detail?.subject_task_id || requirement.detail?.activity_task_id;
        if (evidenceTask && requirement.project_id) add(row, actionButton('Open task evidence', () => { state.projectId = requirement.project_id; openTask(evidenceTask); }, false));
        if (service.coordination_state === 'restore_reconciliation' && !requirement.inspected) add(row, actionButton('Record inspection', () => {
          const view = workflowDialog('Inspect preserved hold', 'Check the actual workstation, producer, resource, or Git target. This record documents inspection; it does not stop a job, release a hold, or invent a result.');
          recordDetails(view.form, 'Preserved hold', requirement);
          selectField(view, 'disposition', 'Observed disposition', 'unknown', [['unknown','Still uncertain'],['held','Still held'],['released','Release was independently verified']]);
          view.field('evidence', 'Inspection evidence').maxLength = 4096;
          view.finish('Save inspection', (values, dialog) => { dialog.close(); startMutation('/api/v1/admin/restore/inspections', {restore_id:restoreId,kind:requirement.kind,target_id:requirement.target_id,disposition:values.get('disposition'),evidence:values.get('evidence')}, 'restore inspection', restoredMutationCallbacks('restore_change')); });
        }));
      }
      if (data.next_cursor) { const more = actionButton('Load more preserved holds', () => { more.disabled = true; loadRestore(data.next_cursor); }, false); more.dataset.moreRestore = 'true'; add(list, more); }
      renderMutationState();
    } catch (error) { if (sequence === restoreSequence && currentActor === actorId()) { setGlobalAlert(errorMessage(error)); list.querySelector('[data-more-restore]')?.removeAttribute('disabled'); } }
  }
  function restoreEvidence(restoreId, action, title, explanation, finish = false) {
    const view = workflowDialog(title, explanation);
    view.field('evidence', finish ? 'Reason to resume' : 'Evidence').maxLength = finish ? 2000 : 4096;
    view.finish(finish ? 'Record reconciliation and resume' : 'Save evidence', (values, dialog) => {
      dialog.close(); const body = {restore_id:restoreId}; body[finish ? 'reason' : 'evidence'] = values.get('evidence');
      startMutation(`/api/v1/admin/restore/${action}`, body, 'restore reconciliation', restoredMutationCallbacks('restore_change'));
    });
  }
  function replaceAgentCredential(credential) {
    const view = workflowDialog('New credential for an existing agent', 'This issues a fresh token while preserving the agent identity and contributor history. Existing revoked tokens remain invalid. The new secret is displayed once.');
    view.field('name','Credential name','after-recovery','input').maxLength = 100;
    view.finish('Issue new token', (values, dialog) => { dialog.close(); startMutation(`/api/v1/admin/agents/${encodeURIComponent(credential.principal_id)}/credentials`, {name:values.get('name')}, 'credential replacement', restoredMutationCallbacks('rotate_credential')); });
  }
  $('refresh-restore').addEventListener('click', () => loadRestore());

  restoreSession();
})();
