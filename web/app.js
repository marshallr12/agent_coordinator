/* Agent Coordinator foundation dashboard. Same-origin API client; no framework required. */
(() => {
  'use strict';

  const $ = (id) => document.getElementById(id);
  const state = {
    actor: null, csrfToken: null, projects: [], tasks: [], credentials: [],
    projectId: '', selectedTaskId: '', currentView: 'overview', detail: null,
    taskCursor: null, mutation: null, fetching: new Set(), inflight: { projects: false, tasks: null, detail: null, credentials: null },
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
    catch (_) { /* Storage can be disabled; in-memory retry remains available. */ }
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
      response = await fetch(path, { ...options, method, headers, credentials: 'same-origin' });
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
    if (operation && actorId()) persistMutation(state.mutation);
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
    state.actor = null; state.csrfToken = null; state.projects = []; state.tasks = []; state.detail = null;
    state.credentials = []; state.taskCursor = null; state.projectId = ''; state.selectedTaskId = ''; state.currentView = 'overview';
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
    ['overview', 'tasks', 'task-detail', 'admin'].forEach((name) => show($(`${name}-view`), name === view));
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
    try { state.projects = listData((await request('/api/v1/projects')).data); renderProjects(); fillProjectSelect(); renderSummary(); }
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
      const repo = el('p', 'repo', project.repository_url || 'Repository not configured'); const footer = el('div', 'project-card-footer');
      add(footer, el('span', 'project-meta', project.target_branch ? `Branch · ${project.target_branch}` : 'Branch not set'));
      const open = el('button', 'project-open', 'Open tasks →'); open.type = 'button'; open.addEventListener('click', () => openProject(project.id));
      add(footer, open); add(card, name); add(card, repo); add(card, footer); add(target, card);
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
    const requestedProjectId = state.projectId;
    if (!requestedProjectId || (state.inflight.tasks?.projectId === requestedProjectId)) return;
    if (append && !state.taskCursor) return;
    const requestId = ++state.requestSeq.tasks;
    const cursorBefore = state.taskCursor;
    const hadLoadedPages = silent && Boolean(cursorBefore) && state.tasks.length > 0;
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
      if (append) state.tasks = state.tasks.concat(items);
      else if (hadLoadedPages) { const byId = new Map(state.tasks.map((item) => [text(item.id), item])); items.forEach((item) => byId.set(text(item.id), item)); state.tasks = Array.from(byId.values()); }
      else state.tasks = items;
      state.taskCursor = page?.next_cursor || null;
      renderTasks(); renderSummary();
    }
    catch (error) { if (requestedProjectId === state.projectId && state.requestSeq.tasks === requestId && !silent) setState($('tasks-state'), errorMessage(error), false, true); }
    finally { if (state.inflight.tasks?.requestId === requestId) state.inflight.tasks = null; if (state.projectId === requestedProjectId) $('load-more-tasks').disabled = false; }
  }

  function renderTasks() {
    const target = $('tasks-list'); clear(target); const filter = $('status-filter').value;
    const tasks = filter === 'all' ? state.tasks : state.tasks.filter((task) => taskStatus(task) === filter);
    const loadMore = $('load-more-tasks'); show(loadMore, Boolean(state.taskCursor)); loadMore.disabled = Boolean(state.inflight.tasks); setText($('tasks-page-status'), state.taskCursor ? `Showing ${state.tasks.length} loaded tasks` : state.tasks.length ? `${state.tasks.length} task${state.tasks.length === 1 ? '' : 's'}` : '');
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
    const status = taskStatus(task); const badge = $('detail-status'); setText(badge, displayStatus(status)); badge.className = `status-badge ${status}`; setText($('detail-kind'), text(task.kind || 'general').toUpperCase());
    const criteria = $('acceptance-list'); clear(criteria); const items = Array.isArray(task.acceptance_criteria) ? task.acceptance_criteria : [];
    if (!items.length) add(criteria, el('li', 'muted', 'No acceptance criteria recorded.')); else items.forEach((item) => add(criteria, el('li', '', item)));
    renderLease(data, task); renderCheckpoints(data);
    show($('task-detail-state'), false); show($('task-detail-content'), true);
  }

  function renderLease(data, task) {
    const target = $('lease-content'); clear(target); const attempts = Array.isArray(data.attempts) ? data.attempts : []; const current = attempts.find((attempt) => attempt.id === task.current_attempt_id) || attempts.find((attempt) => !attempt.closed_at && !['closed', 'released', 'expired'].includes(attempt.state));
    if (!current) {
      const status = taskStatus(task);
      const message = task.current_attempt_id ? 'Ownership details are still syncing. Refresh to reconcile this task.' : ['done', 'canceled', 'superseded', 'submitted'].includes(status) ? `No active lease. This task is ${displayStatus(status).toLowerCase()}.` : status === 'blocked' ? 'No active lease. This task is blocked and needs attention.' : status === 'planned' ? 'No active lease. This task is planned and is not yet admitted.' : 'No active lease. This task is available for eligible work.';
      add(target, el('p', 'muted', message)); return;
    }
    const dl = el('dl'); const remaining = current.lease_remaining_ms !== undefined ? `${Math.max(0, Math.round(Number(current.lease_remaining_ms) / 60000))} min remaining` : current.expires_at ? formatLease(current.expires_at) : 'Lease active';
    [['Owner', current.owner_name || current.owner_id || 'Agent'], ['State', displayStatus(current.state || 'active')], ['Lease', remaining], ['Expires', formatDate(current.expires_at)], ['Generation', current.generation || 1]].forEach(([label, value]) => { const line = el('div', 'lease-line'); add(line, el('dt', '', label)); add(line, el('dd', '', value)); add(dl, line); }); add(target, dl);
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

  function startPolling() { if (state.pollTimer) clearInterval(state.pollTimer); state.pollTimer = setInterval(() => { if (!state.actor || state.mutation) return; if (state.currentView === 'overview') loadProjects(true); else if (state.currentView === 'tasks') loadTasks(true); else if (state.currentView === 'task-detail') loadTaskDetail(true); else if (state.currentView === 'admin') loadCredentials(true); }, 5000); }

  $('login-form').addEventListener('submit', (event) => { event.preventDefault(); const username = $('username').value.trim(); const password = $('password').value; if (!username || !password) { showLoginError('Enter your username and password.'); return; } showLoginError(''); startMutation('/api/v1/auth/login', { username, password }, 'sign-in', async (data) => { applySession(data); $('password').value = ''; await loadProjects(); startPolling(); }, 'POST', (error) => showLoginError(errorMessage(error))); });
  $('logout-button').addEventListener('click', () => startMutation('/api/v1/auth/logout', {}, 'sign-out', async () => signOutLocal()));
  document.querySelectorAll('.nav-item').forEach((button) => button.addEventListener('click', () => { const view = button.dataset.view; showView(view); if (view === 'tasks' && state.projectId) loadTasks(); if (view === 'admin') loadCredentials(); }));
  $('brand-button').addEventListener('click', () => showView('overview')); $('new-project-button').addEventListener('click', () => openDialog('project')); $('new-task-button').addEventListener('click', () => openDialog('task'));
  $('refresh-projects').addEventListener('click', () => loadProjects()); $('refresh-tasks').addEventListener('click', () => loadTasks()); $('load-more-tasks').addEventListener('click', () => loadTasks(false, true)); $('project-select').addEventListener('change', (event) => { state.projectId = event.target.value; state.taskCursor = null; state.tasks = []; $('new-task-button').disabled = !state.projectId; $('refresh-tasks').disabled = !state.projectId; loadTasks(); }); $('status-filter').addEventListener('change', renderTasks);
  $('back-to-tasks').addEventListener('click', () => showView('tasks')); $('refresh-credentials').addEventListener('click', () => loadCredentials()); $('issue-form').addEventListener('submit', (event) => { event.preventDefault(); const input = $('agent-name'); if (!input.value.trim()) return; startMutation('/api/v1/admin/agents', { name: input.value.trim() }, 'credential issuance', async (data) => { input.value = ''; showToken(data?.token); await loadCredentials(); setIssueFeedback(issuedCredentialFeedback(data), data?.token ? 'success' : 'error'); }); });
  function showToken(token) { setText($('issued-token'), token || 'The token was not returned. Revoke this credential and issue a replacement.'); show($('token-reveal'), true); }
  function setIssueFeedback(message, kind) { const target = $('issue-feedback'); target.className = `inline-alert ${kind}`; setText(target, message); show(target, true); }
  $('clear-token').addEventListener('click', () => { setText($('issued-token'), ''); show($('token-reveal'), false); }); $('copy-token').addEventListener('click', async () => { const token = $('issued-token').textContent; if (!token) return; try { await navigator.clipboard.writeText(token); setIssueFeedback('Token copied to clipboard.', 'success'); } catch (_) { setIssueFeedback('Copy was unavailable. Select the token and copy it manually.', 'error'); } });

  restoreSession();
})();
