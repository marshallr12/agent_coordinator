/* Agent Coordinator dashboard. Same-origin API client; no framework required. */
(() => {
  'use strict';

  const $ = (id) => document.getElementById(id);
  const TASK_AUTO_REFRESH_KEY = 'agent-coordinator.task-auto-refresh';
  function savedTaskAutoRefresh() {
    try { return localStorage.getItem(TASK_AUTO_REFRESH_KEY) === 'true'; } catch { return false; }
  }
  const state = {
    actor: null, csrfToken: null, projects: [], tasks: [], credentials: [], resources: [], resourceCursor: null, resourcePagesExtended: false, sharedCursor: null, sharedSeq: 0,
    projectId: '', selectedTaskId: '', currentView: 'overview', detail: null, credentialDownloadUrl: null,
    taskPages: [], taskPageIndex: 0, taskPageSize: 25, taskView: 'queue', taskAutoRefresh: savedTaskAutoRefresh(), taskQueueLoaded: false, queueTasks: [], completedTasks: [], completedLoaded: false, completedPageIndex: 0, inflightCompleted: false, mutation: null, fetching: new Set(), inflight: { projects: false, tasks: null, detail: null, credentials: null },
    requestSeq: { projects: 0, tasks: 0, detail: 0, credentials: 0 }, attachmentTaskKey: '', attachmentRequestSeq: 0, attachmentsLoaded: false, attachmentBusy: false, pollTimer: null, lastSync: null
  };

  const PENDING_MUTATION_KEY = 'agent-coordinator.pending-mutation';
  const ATTACHMENT_DB_NAME = 'agent-coordinator-attachment-journal';
  const ATTACHMENT_STORE_NAME = 'batches';
  const MAX_ATTACHMENT_BYTES = 16 * 1024 * 1024;
  const MAX_ATTACHMENT_BATCH_BYTES = 64 * 1024 * 1024;
  const MAX_ATTACHMENT_FILES = 10;
  const THEME_KEY = 'agent-coordinator.theme';
  function applyTheme(theme) {
    const selected = ['light', 'dark', 'system'].includes(theme) ? theme : 'system';
    document.documentElement.dataset.theme = selected;
    try { localStorage.setItem(THEME_KEY, selected); } catch { /* Theme still applies for this page. */ }
  }
  try { applyTheme(localStorage.getItem(THEME_KEY) || 'system'); } catch { applyTheme('system'); }

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

  function openAttachmentDatabase() {
    if (!globalThis.indexedDB) return Promise.reject(new Error('This browser cannot save upload bytes for a safe retry.'));
    return new Promise((resolve, reject) => {
      const open = indexedDB.open(ATTACHMENT_DB_NAME, 1);
      open.onupgradeneeded = () => open.result.createObjectStore(ATTACHMENT_STORE_NAME, { keyPath: 'id' });
      open.onsuccess = () => resolve(open.result);
      open.onerror = () => reject(new Error('Could not open the protected browser upload journal.'));
    });
  }

  async function storeAttachmentBatch(batch) {
    const db = await openAttachmentDatabase();
    return new Promise((resolve, reject) => {
      const transaction = db.transaction(ATTACHMENT_STORE_NAME, 'readwrite');
      transaction.objectStore(ATTACHMENT_STORE_NAME).put(batch);
      transaction.oncomplete = () => { db.close(); resolve(); };
      transaction.onerror = () => { db.close(); reject(new Error('Could not save upload bytes and retry keys.')); };
      transaction.onabort = () => { db.close(); reject(new Error('Could not save upload bytes and retry keys.')); };
    });
  }

  async function readAttachmentBatches() {
    const db = await openAttachmentDatabase();
    return new Promise((resolve, reject) => {
      const transaction = db.transaction(ATTACHMENT_STORE_NAME, 'readonly');
      const request = transaction.objectStore(ATTACHMENT_STORE_NAME).getAll();
      request.onsuccess = () => resolve(request.result || []);
      request.onerror = () => reject(new Error('Could not read pending attachment uploads.'));
      transaction.oncomplete = () => db.close();
      transaction.onerror = () => { db.close(); reject(new Error('Could not read pending attachment uploads.')); };
    });
  }

  async function deleteAttachmentBatch(id) {
    const db = await openAttachmentDatabase();
    return new Promise((resolve, reject) => {
      const transaction = db.transaction(ATTACHMENT_STORE_NAME, 'readwrite');
      transaction.objectStore(ATTACHMENT_STORE_NAME).delete(id);
      transaction.oncomplete = () => { db.close(); resolve(); };
      transaction.onerror = () => { db.close(); reject(new Error('Could not clear the completed upload journal.')); };
      transaction.onabort = () => { db.close(); reject(new Error('Could not clear the completed upload journal.')); };
    });
  }

  const actorId = () => text(state.actor?.id || state.actor?.principal_id);
  const mutationOperation = (path, method) => {
    if (['POST', 'PUT', 'PATCH'].includes(method) && /\/projects\/[^/]+\/(workflow-policy|policy|workflow-activities\/[^/]+\/[^/]+|tasks\/[^/]+\/workflow\/reopen)$/.test(path)) return 'workflow_change';
    if (['POST','PATCH'].includes(method) && /\/projects\/[^/]+\/(knowledge|decisions|artifacts|imports)(\/|$)/.test(path)) return 'shared_change';
    if (['POST','PATCH','DELETE'].includes(method) && /\/projects\/[^/]+\/(objectives|claims|attempts|tasks)(\/|$)/.test(path)) return 'operator_work';
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
    if ($('account-button')) $('account-button').disabled = busy;
  }

  function applySession(data) {
    state.actor = data?.actor || data?.principal || null;
    state.csrfToken = data?.csrf_token || data?.csrfToken || state.csrfToken;
    const actorName = state.actor?.name || state.actor?.id || 'Operator';
    setText($('account-button'), actorName);
    const admin = state.actor?.role === 'admin';
    show($('admin-nav'), admin);
    show($('loading-view'), false); show($('login-view'), false); show($('dashboard-view'), true);
  }

  function signOutLocal(preservePending = false) {
    document.querySelectorAll('dialog').forEach(dialog => dialog.close());
    state.mutation = null; state.actor = null; state.csrfToken = null; state.projects = []; state.tasks = []; state.detail = null;
    ++state.sharedSeq; state.sharedCursor = null; clear($('shared-list')); setText($('shared-freshness'), '');
    $('context-search').reset();
    state.credentials = []; state.resources = []; state.resourceCursor = null; state.resourcePagesExtended = false; clear($('resources-list')); clear($('job-evidence-content')); clear($('workflow-content')); resetTaskPages(); state.projectId = ''; state.selectedTaskId = ''; state.currentView = 'overview';
    if (!preservePending) clearPersistedMutation();
    clearCredentialDownload();
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
    ['overview', 'project', 'tasks', 'archived', 'task-detail', 'resources', 'admin', 'shared'].forEach((name) => show($(`${name}-view`), name === view));
    show($('task-view-tabs'), view === 'tasks' || view === 'archived');
    updateTaskViewTabs();
    document.querySelectorAll('.nav-item').forEach((button) => button.classList.toggle('active', button.dataset.view === view || (view === 'project' && button.dataset.view === 'overview') || (view === 'task-detail' && button.dataset.view === 'tasks')));
    if (view === 'tasks') { fillProjectSelect(); $('back-to-project').disabled = !state.projectId; $('new-task-button').disabled = !state.projectId; $('refresh-tasks').disabled = !state.projectId; $('project-select')?.focus(); }
  }

  function issuedCredentialFeedback(data) {
    const credentialId = data?.credential_id || data?.id || 'the issued credential';
    if (data?.secret_unavailable) return `This issuance was replayed, but its token cannot be recovered. Revoke ${credentialId}, then issue a replacement with a fresh agent name.`;
    if (typeof data?.token !== 'string' || !data.token) return `The token was not returned. Revoke ${credentialId}, then issue a replacement with a fresh agent name.`;
    return 'Credential issued. Download requested; check your browser’s downloads. If needed, use Download credentials.toml again below.';
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
    if (operation === 'issue_credential') return async (data) => { downloadIssuedCredential(data); await loadCredentials(); };
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
    const visibleTasks = state.taskView === 'completed' ? state.tasks : state.tasks.filter((task) => taskStatus(task) !== 'done');
    const active = visibleTasks.filter((task) => ['claimed', 'in_progress', 'working', 'active'].includes(taskStatus(task))).length;
    const blocked = visibleTasks.filter((task) => taskStatus(task) === 'blocked').length;
    [['Projects', state.projects.length], ['Tasks in view', visibleTasks.length], ['Active leases', active], ['Blocked', blocked]].forEach(([label, value]) => {
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
      const header = el('div', 'project-card-header');
      const settings = el('button', 'button project-settings-button'); settings.type = 'button';
      const gear = el('span', '', '⚙'); gear.setAttribute('aria-hidden', 'true'); add(settings, gear);
      settings.setAttribute('aria-label', `Project settings for ${project.name || projectId}`);
      settings.title = 'Project settings'; settings.addEventListener('click', () => openProjectSettings(project.id));
      add(header, name); add(header, settings);
      const footer = el('div', 'project-card-footer');
      add(footer, el('span', 'project-meta', project.target_branch ? `Branch · ${project.target_branch}` : 'Branch not set'));
      const open = el('button', 'project-open', 'Open tasks →'); open.type = 'button';
      open.setAttribute('aria-label', `Open tasks for ${project.name || projectId}`);
      open.addEventListener('click', () => openProject(project.id));
      card.addEventListener('click', (event) => {
        if (event.target.closest('button, a, details, input, select, textarea')) return;
        const selection = window.getSelection();
        if (selection && !selection.isCollapsed && (card.contains(selection.anchorNode) || card.contains(selection.focusNode))) return;
        openProject(project.id);
      });
      add(footer, open); add(card, header); add(card, repo); add(card, idLabel); add(card, footer); add(target, card);
    });
  }

  function fillProjectSelect() {
    const select = $('project-select'); if (!select) return; const current = state.projectId; clear(select);
    add(select, el('option', '', 'Choose a project')).value = '';
    state.projects.forEach((project) => { const option = el('option', '', project.name || project.id); option.value = text(project.id); add(select, option); });
    select.value = state.projects.some((project) => text(project.id) === current) ? current : '';
    const archive = $('archive-project-select'); if (archive) { const prior=archive.value; clear(archive); add(archive,el('option','','Choose a project')).value=''; state.projects.forEach(project=>{const option=el('option','',project.name||project.id);option.value=text(project.id);add(archive,option);}); archive.value=state.projects.some(project=>text(project.id)===prior)?prior:(state.projects.some(project=>text(project.id)===current)?current:''); }
    fillSharedProject();
  }

  function openProject(id) {
    state.projectId = text(id); state.selectedTaskId = ''; resetTaskPages(); state.taskView = 'queue'; updateTaskViewTabs();
    showView('tasks'); loadTasks();
  }
  function openProjectSettings(id) {
    const project = state.projects.find(item => text(item.id) === text(id));
    if (!project) { showView('overview'); return; }
    if (state.projectId !== text(id)) resetTaskPages();
    state.projectId = text(id); state.selectedTaskId = '';
    setText($('project-heading'), `${project.name || 'Unnamed project'} settings`);
    setText($('project-repository'), `${project.repository_url || 'Repository not configured'} · ${project.target_branch || 'Branch not set'}`);
    const binding = $('project-binding-content'); clear(binding);
    const snippet = `service_url = ${JSON.stringify(location.origin)}\nproject_id = ${JSON.stringify(state.projectId)}`;
    add(binding, el('code', 'binding-snippet', snippet));
    const download = el('button', 'button subtle', 'Download'); download.type = 'button';
    download.addEventListener('click', () => {
      const file = new Blob([snippet], {type: 'application/toml;charset=utf-8'});
      const url = URL.createObjectURL(file);
      const link = el('a'); link.href = url; link.download = '.agent-coordinator.toml';
      add(document.body, link); link.click(); link.remove();
      window.setTimeout(() => URL.revokeObjectURL(url), 1000);
      setText(download, 'Downloaded');
      window.setTimeout(() => setText(download, 'Download'), 1600);
    });
    add(binding, download);
    showView('project'); $('project-heading').focus();
  }
  $('back-to-projects').addEventListener('click', () => showView('overview'));
  $('back-to-project').addEventListener('click', () => showView('overview'));
  $('project-review-button').addEventListener('click', async () => {
    const projectId = state.projectId;
    try {
      const reply = await request(`${projectPath(projectId)}/orientation`);
      if (projectId === state.projectId && state.currentView === 'project') openReviewSettings(reply.data.project);
    } catch (error) { setGlobalAlert(errorMessage(error), 'error'); }
  });

  function resetTaskPages() {
    state.taskPages = []; state.taskPageIndex = 0; state.tasks = []; state.queueTasks = []; state.taskQueueLoaded = false;
    state.completedTasks = []; state.completedLoaded = false; state.completedPageIndex = 0;
  }

  function showTaskPage(index, updateSummary = true) {
    if (state.taskView === 'completed') {
      const pageCount = Math.ceil(state.completedTasks.length / state.taskPageSize);
      state.completedPageIndex = Math.max(0, Math.min(index, pageCount - 1));
      const start = state.completedPageIndex * state.taskPageSize;
      state.tasks = state.completedTasks.slice(start, start + state.taskPageSize);
      renderTasks(); if (updateSummary) renderSummary(); return;
    }
    state.taskPageIndex = index; const page = state.taskPages[index];
    state.tasks = page?.items || [];
    renderTasks(); if (updateSummary) renderSummary();
  }

  function rebuildQueuePages() {
    const filter = $('status-filter').value;
    const tasks = state.queueTasks.filter((task) => filter === 'all' || taskStatus(task) === filter);
    state.taskPages = [];
    for (let start = 0; start < tasks.length; start += state.taskPageSize) {
      state.taskPages.push({ items: tasks.slice(start, start + state.taskPageSize) });
    }
  }

  async function loadTasks(silent = false) {
    if (state.taskView === 'completed') { await loadCompletedTasks(silent); return; }
    const requestedProjectId = state.projectId;
    if (!requestedProjectId || state.inflight.tasks?.projectId === requestedProjectId) return;
    const requestedPageIndex = state.taskPageIndex;
    const requestId = ++state.requestSeq.tasks;
    state.inflight.tasks = { projectId: requestedProjectId, requestId };
    const project = state.projects.find((item) => text(item.id) === requestedProjectId);
    setText($('tasks-subtitle'), project?.name ? `${project.name} · current work queue` : 'Current work queue');
    if (!silent && !state.queueTasks.length) { show($('tasks-list'), false); setState($('tasks-state'), 'Loading tasks…', true); }
    try {
      const allTasks = []; let cursor = null;
      do {
        const params = new URLSearchParams({ limit: '200', exclude_done: 'true' });
        if (cursor) params.set('cursor', cursor);
        const page = (await request(`/api/v1/projects/${encodeURIComponent(requestedProjectId)}/tasks?${params}`)).data;
        if (requestedProjectId !== state.projectId || state.requestSeq.tasks !== requestId) return;
        allTasks.push(...listData(page)); cursor = page?.next_cursor || null;
      } while (cursor);
      const queueTasks = allTasks.filter((task) => taskStatus(task) !== 'done');
      const changed = !state.taskQueueLoaded || JSON.stringify(state.queueTasks) !== JSON.stringify(queueTasks);
      state.taskQueueLoaded = true;
      if (changed) {
        state.queueTasks = queueTasks;
        rebuildQueuePages();
        const pageIndex = Math.min(requestedPageIndex, Math.max(0, state.taskPages.length - 1));
        showTaskPage(pageIndex, false);
      }
    }
    catch (error) { if (requestedProjectId === state.projectId && state.requestSeq.tasks === requestId && !silent) setState($('tasks-state'), errorMessage(error), false, true); }
    finally { if (state.inflight.tasks?.requestId === requestId) state.inflight.tasks = null; renderTaskPagination(); }
  }

  async function loadCompletedTasks(silent = false) {
    const requestedProjectId = state.projectId;
    if (!requestedProjectId || state.inflightCompleted) return;
    state.inflightCompleted = true;
    if (!silent && !state.completedTasks.length) { show($('tasks-list'), false); setState($('tasks-state'), 'Loading completed tasks…', true); }
    const completed = []; let cursor = null;
    try {
      do {
        const params = new URLSearchParams({ limit: '200' });
        if (cursor) params.set('cursor', cursor);
        const page = (await request(`/api/v1/projects/${encodeURIComponent(requestedProjectId)}/tasks?${params}`)).data;
        completed.push(...listData(page).filter((task) => taskStatus(task) === 'done'));
        cursor = page?.next_cursor || null;
      } while (cursor && requestedProjectId === state.projectId);
      if (requestedProjectId !== state.projectId) return;
      const changed = !state.completedLoaded || JSON.stringify(state.completedTasks) !== JSON.stringify(completed);
      state.completedLoaded = true; state.completedTasks = completed;
      state.completedPageIndex = Math.min(state.completedPageIndex, Math.max(0, Math.ceil(completed.length / state.taskPageSize) - 1));
      if (state.taskView === 'completed' && changed) showTaskPage(state.completedPageIndex, false);
    } catch (error) {
      if (requestedProjectId === state.projectId && !silent) setState($('tasks-state'), errorMessage(error), false, true);
    } finally {
      state.inflightCompleted = false;
      if (state.taskView === 'completed') renderTaskPagination();
    }
  }

  function updateTaskViewTabs() {
    const selected = state.currentView === 'archived' ? 'archived' : state.taskView === 'completed' ? 'completed' : 'queue';
    [['queue', 'task-queue-tab'], ['completed', 'completed-tasks-tab'], ['archived', 'archived-tasks-tab']].forEach(([view, id]) => {
      const tab = $(id), active = selected === view;
      tab.setAttribute('aria-selected', String(active)); tab.tabIndex = active ? 0 : -1;
    });
    $('tasks-view').setAttribute('aria-labelledby', selected === 'completed' ? 'completed-tasks-tab' : 'task-queue-tab');
    const completed = state.taskView === 'completed';
    show($('status-filter-label'), !completed);
    setText($('tasks-heading'), completed ? 'Completed tasks' : 'Task queue');
    const project = state.projects.find((item) => text(item.id) === state.projectId);
    setText($('tasks-subtitle'), project?.name ? `${project.name} · ${completed ? 'completed tasks' : 'current work queue'}` : completed ? 'Completed tasks' : 'Current work queue');
  }

  function selectTaskView(view) {
    state.taskView = view; updateTaskViewTabs();
    if (view === 'archived') {
      $('archive-project-select').value = state.projectId;
      showView('archived'); loadArchivedTasks();
    } else {
      showView('tasks');
      if (view === 'completed') {
        // The task list is shared between views. Select the completed page
        // immediately so an unchanged cached result cannot leave queue rows
        // visible while (or after) the refresh completes.
        showTaskPage(state.completedPageIndex);
        loadCompletedTasks();
      } else showTaskPage(state.taskPageIndex);
    }
  }

  async function showLastTaskPage() {
    const count = state.taskView === 'completed' ? Math.ceil(state.completedTasks.length / state.taskPageSize) : state.taskPages.length;
    showTaskPage(count - 1);
  }

  function renderTaskPagination() {
    const completed = state.taskView === 'completed';
    const count = completed ? Math.ceil(state.completedTasks.length / state.taskPageSize) : state.taskPages.length;
    const current = completed ? state.completedPageIndex : state.taskPageIndex;
    show($('task-pagination'), Boolean(count)); $('tasks-first-page').disabled = !current; $('tasks-previous-page').disabled = !current;
    $('tasks-next-page').disabled = completed ? state.inflightCompleted || current + 1 >= count : Boolean(state.inflight.tasks) || current + 1 >= count;
    $('tasks-last-page').disabled = completed ? state.inflightCompleted || current + 1 >= count : Boolean(state.inflight.tasks) || current + 1 >= count;
    const numbers = $('tasks-page-numbers'); clear(numbers);
    for (let index = 0; index < count; index++) { const page = actionButton(String(index + 1), () => showTaskPage(index), false); page.classList.add('pagination-page'); page.setAttribute('aria-current', index === current ? 'page' : 'false'); page.disabled = index === current; add(numbers, page); }
  }

  function renderTasks() {
    const target = $('tasks-list'); clear(target); const filter = $('status-filter').value;
    const tasks = state.tasks;
    const pageCount = state.taskView === 'completed' ? Math.ceil(state.completedTasks.length / state.taskPageSize) : state.taskPages.length;
    const pageIndex = state.taskView === 'completed' ? state.completedPageIndex : state.taskPageIndex;
    const taskTotal = state.taskView === 'completed' ? state.completedTasks.length : state.queueTasks.filter((task) => filter === 'all' || taskStatus(task) === filter).length;
    renderTaskPagination(); setText($('tasks-page-status'), pageCount ? `Page ${pageIndex + 1} of ${taskTotal} tasks` : '');
    if (!tasks.length) {
      show(target, false);
      const empty = state.taskView === 'completed' ? (state.inflightCompleted ? 'Loading completed tasks…' : 'No completed tasks in this project yet.') : state.queueTasks.length ? 'No tasks match this status filter.' : 'No tasks in this project yet. Create the first task.';
      setState($('tasks-state'), empty, state.taskView === 'completed' && state.inflightCompleted); return;
    }
    show($('tasks-state'), false); show(target, true);
    tasks.forEach((task) => {
      const status = taskStatus(task);
      const row = el('article', `task-row ${status}`);
      row.setAttribute('role', 'button'); row.setAttribute('tabindex', '0');
      row.setAttribute('aria-label', `Open task ${task.title || task.id}`);
      const open = () => openTask(task.id);
      row.addEventListener('click', (event) => {
        const selection = window.getSelection();
        if (selection && !selection.isCollapsed && (row.contains(selection.anchorNode) || row.contains(selection.focusNode))) return;
        open();
      });
      row.addEventListener('keydown', (event) => {
        if (event.key === 'Enter' || event.key === ' ') { event.preventDefault(); open(); }
      });
      const title = el('span', 'task-main'); add(title, el('span', 'task-title', task.title || 'Untitled task'));
      const identity = el('span', 'task-queue-id'); add(identity, el('span', '', 'Task ID · ')); add(identity, el('span', 'task-id-value', task.id));
      const created = el('time', 'task-created project-meta', `Created ${formatDate(task.created_at)}`);
      if (task.created_at && !Number.isNaN(new Date(task.created_at).getTime())) created.dateTime = task.created_at;
      const meta = el('span', 'task-meta');
      const badge = el('span', `status-badge ${status}`, displayStatus(status)); add(meta, badge);
      const description = el('span', 'task-description', task.description || 'No description');
      add(row, title); add(row, identity); add(row, created); add(row, meta); add(row, description); add(target, row);
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

  function openTask(id, origin = 'tasks') { state.selectedTaskId = text(id); state.detailOrigin = origin; showView('task-detail'); loadTaskDetail(); }
  async function loadArchivedTasks() {
    const projectId = $('archive-project-select').value || state.projectId;
    if (!projectId) { setText($('archived-state'), 'Choose a project to load archived tasks.'); show($('archived-state'), true); show($('archived-list'), false); return; }
    $('archive-project-select').value = projectId;
    setText($('archived-state'), 'Loading archived tasks…'); show($('archived-state'), true); show($('archived-list'), false);
    try {
      const tasks = []; let cursor = null;
      do { const params=new URLSearchParams({limit:'200'}); if(cursor) params.set('cursor',cursor); const page=(await request(`/api/v1/projects/${encodeURIComponent(projectId)}/tasks/archived?${params}`)).data; tasks.push(...listData(page)); cursor=page?.next_cursor||null; } while(cursor && projectId === ($('archive-project-select').value || state.projectId));
      if (projectId !== ($('archive-project-select').value || state.projectId)) return;
      const target = $('archived-list'); clear(target);
      if (!tasks.length) { setText($('archived-state'), 'No archived tasks in this project.'); return; }
      tasks.forEach(task => {
        const row = el('article', `task-row archived-task-row ${task.lifecycle}`);
        row.setAttribute('role', 'button'); row.setAttribute('tabindex', '0'); row.setAttribute('aria-label', `Open archived task ${task.title || task.id}`);
        const open = () => { state.projectId = projectId; openTask(task.id, 'archived'); };
        row.addEventListener('click', open);
        row.addEventListener('keydown', event => { if (event.key === 'Enter' || event.key === ' ') { event.preventDefault(); open(); } });
        const title = el('span', 'task-title', task.title || 'Untitled task');
        const meta = el('span', 'task-meta', `Archived · ${displayStatus(task.lifecycle)}`);
        const description = el('span', 'task-description', task.description || 'No description');
        add(row, title); add(row, meta); add(row, description); add(target, row);
      });
      show($('archived-state'), false); show(target,true);
    } catch(error) { setText($('archived-state'),errorMessage(error)); show($('archived-state'),true); }
  }

  function selectTaskDetailText(id) {
    const source = $(id); const selection = window.getSelection();
    if (!source || !selection) return;
    const range = document.createRange(); range.selectNodeContents(source); selection.removeAllRanges(); selection.addRange(range);
  }
  function setTaskDetailCopyFeedback(message, kind) {
    const target = $('detail-copy-feedback'); target.className = `task-detail-copy-feedback ${kind || ''}`; setText(target, message); show(target, Boolean(message));
  }
  async function copyTaskDetailValue(button, label, sourceId) {
    const value = button.dataset.copyValue;
    if (!value) return;
    try { await navigator.clipboard.writeText(value); setTaskDetailCopyFeedback(`${label} copied to clipboard.`, 'success'); }
    catch (_) { selectTaskDetailText(sourceId); setTaskDetailCopyFeedback(`Clipboard access was unavailable. ${label} is selected; copy it with your browser.`, 'fallback'); }
  }

  function renderTaskDetail() {
    const data = state.detail || {}; const task = data.task || data; const project = state.projects.find((item) => text(item.id) === state.projectId);
    const title = task.title || 'Untitled task'; const taskId = text(task.id || state.selectedTaskId);
    setText($('detail-project-label'), project?.name || 'Project'); setText($('task-detail-heading'), title); setText($('detail-task-id-value'), taskId); setText($('detail-task-revision'), task.revision || 1); setText($('detail-description'), task.description || 'No description provided.');
    $('copy-task-name').dataset.copyValue = title; $('copy-task-id').dataset.copyValue = taskId; setTaskDetailCopyFeedback('', '');
    const status = taskStatus(task); const badge = $('detail-status'); setText(badge, displayStatus(status)); badge.className = `status-badge ${status}`; setText($('detail-kind'), displayStatus(task.activity_kind || task.kind || 'general'));
    const criteria = $('acceptance-list'); clear(criteria); const items = Array.isArray(task.acceptance_criteria) ? task.acceptance_criteria : [];
    if (!items.length) add(criteria, el('li', 'muted', 'No acceptance criteria recorded.')); else items.forEach((item) => add(criteria, el('li', '', item)));
    renderTaskActions(data, task); renderLease(data, task); renderCheckpoints(data); renderJobEvidence(data); renderWorkflow(data);
    show($('task-detail-state'), false); show($('task-detail-content'), true);
    loadTaskAttachments(state.projectId, taskId);
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

  function attachmentStatus(message, error = false) {
    const target = $('task-attachments-state'); setText(target, message); target.className = error ? 'inline-alert error' : 'muted';
  }

  function renderTaskAttachments(records, pending, hasMore = false) {
    const list = $('task-attachments-list'); clear(list);
    if (!records.length) add(list, el('p', 'muted', 'No files are attached to this task yet.'));
    records.forEach((record) => {
      const entry = el('div', 'task-attachment-entry');
      add(entry, el('strong', '', record.display_name || 'Attachment'));
      add(entry, el('p', 'muted', `${record.media_type || 'Unknown type'} · ${record.size_bytes ?? 'Unknown'} bytes · ${displayStatus(record.availability || record.state || 'unknown')}`));
      if (record.availability === 'available' && record.kind === 'upload') {
        const link = el('a', 'button subtle', 'Download attachment');
        link.href = `${projectPath(state.projectId)}/artifacts/${encodeURIComponent(record.id)}/content`;
        link.download = record.display_name || 'attachment';
        add(entry, link);
      } else if (record.external_url) {
        try {
          const url = new URL(record.external_url);
          if (url.protocol === 'https:' && !url.username && !url.password) {
            const link = el('a', 'button subtle', 'Open attachment link'); link.href = url.href; link.target = '_blank'; link.rel = 'noopener noreferrer'; add(entry, link);
          }
        } catch (_) { /* Keep invalid links as metadata only. */ }
      }
      add(list, entry);
    });
    if (hasMore) add(list, el('p', 'muted', 'More records are available through Browse history.'));
    const retry = $('retry-task-attachments');
    retry.hidden = !pending.length;
    retry.disabled = state.attachmentBusy || Boolean(state.mutation);
    retry.textContent = pending.length === 1 ? 'Retry pending upload' : `Retry ${pending.length} pending uploads`;
  }

  async function loadTaskAttachments(projectId, taskId, force = false) {
    const key = `${projectId}/${taskId}`, requestedActor = actorId();
    if (!force && state.attachmentTaskKey === key && state.attachmentsLoaded) return;
    state.attachmentTaskKey = key; state.attachmentsLoaded = false;
    const requestId = ++state.attachmentRequestSeq;
    attachmentStatus('Loading task attachments…');
    try {
      const [history, batches] = await Promise.all([
        request(`${projectPath(projectId)}/tasks/${encodeURIComponent(taskId)}/history?kind=artifacts&limit=50`),
        readAttachmentBatches()
      ]);
      if (requestId !== state.attachmentRequestSeq || projectId !== state.projectId || taskId !== state.selectedTaskId || requestedActor !== actorId()) return;
      const records = (history.data.items || []).map((item) => item.record).filter(Boolean);
      const pending = batches.filter((batch) => batch.project_id === projectId && batch.task_id === taskId && batch.actor_id === requestedActor);
      renderTaskAttachments(records, pending, Boolean(history.data.next_cursor));
      attachmentStatus(records.length ? `${records.length} attachment${records.length === 1 ? '' : 's'} available in task history.` : 'No files are attached to this task yet.');
      if (pending.length) attachmentStatus('A saved upload is waiting to finish. Retry uses the saved bytes and request keys.', true);
      state.attachmentsLoaded = true;
    } catch (error) {
      if (requestId !== state.attachmentRequestSeq || projectId !== state.projectId || taskId !== state.selectedTaskId || requestedActor !== actorId()) return;
      state.attachmentsLoaded = false;
      attachmentStatus(`Could not load task attachments: ${errorMessage(error)}`, true);
    }
  }

  function safeAttachmentName(value) {
    let name = text(value).replace(/[\\/\u0000-\u001f\u007f]/g, '_') || 'attachment';
    const encoder = new TextEncoder();
    while (encoder.encode(name).length > 255) name = Array.from(name).slice(0, -1).join('');
    return name || 'attachment';
  }

  async function attachmentDigest(file) {
    if (!globalThis.crypto?.subtle) throw new Error('This browser cannot calculate a secure file digest. Open the coordinator over HTTPS and try again.');
    const digest = await crypto.subtle.digest('SHA-256', await file.arrayBuffer());
    return Array.from(new Uint8Array(digest), (byte) => byte.toString(16).padStart(2, '0')).join('');
  }

  async function makeAttachmentBatch(projectId, taskId, files) {
    let totalBytes = 0;
    if (files.length > MAX_ATTACHMENT_FILES) throw new Error(`Choose no more than ${MAX_ATTACHMENT_FILES} files at once.`);
    const records = [];
    for (const file of files) {
      if (file.size > MAX_ATTACHMENT_BYTES) throw new Error(`${file.name} exceeds the 16 MiB per-file limit.`);
      totalBytes += file.size;
      if (totalBytes > MAX_ATTACHMENT_BATCH_BYTES) throw new Error('The combined selection exceeds the 64 MiB upload limit.');
      const mediaType = /^[A-Za-z0-9!#$&^_.+-]+\/[A-Za-z0-9!#$&^_.+-]+$/.test(file.type) ? file.type : 'application/octet-stream';
      const filename = safeAttachmentName(file.name);
      const sha256 = await attachmentDigest(file);
      const reservationBody = { filename, media_type: mediaType, size_bytes: file.size, sha256, task_id: taskId, job_id: null, retention_days: 90, pinned: false };
      records.push({ filename, media_type: mediaType, size_bytes: file.size, sha256, blob: file.slice(0, file.size, mediaType), reservation_body: reservationBody, reservation_key: newKey(), upload_key: newKey(), artifact_id: null, upload_path: null, uploaded: false });
    }
    return { id: newKey(), project_id: projectId, task_id: taskId, actor_id: actorId(), files: records, created_at: new Date().toISOString() };
  }

  async function putAttachmentBytes(path, record) {
    if (!path.startsWith('/api/v1/') || path.startsWith('//') || path.includes('\\') || path.includes('://')) throw new Error('The upload path is not a safe same-origin API path.');
    const headers = new Headers({ Accept: 'application/json', 'Content-Type': record.media_type, 'Idempotency-Key': record.upload_key });
    if (state.csrfToken) headers.set('X-CSRF-Token', state.csrfToken);
    let response;
    try { response = await fetch(path, { method: 'PUT', headers, body: record.blob, credentials: 'same-origin', redirect: 'error' }); }
    catch (_) { throw new ApiError('The file upload may still be processing. Retry it with the saved bytes and key.', 0, 'network_error', null, true); }
    let payload = null;
    try { payload = await response.json(); } catch (_) { /* handled below */ }
    const uncertain = response.status === 429 || response.status >= 500;
    if (!response.ok) {
      const apiError = payload?.error || {};
      throw new ApiError(apiError.message || `Upload failed (${response.status}).`, response.status, apiError.code || 'upload_failed', apiError.details, uncertain);
    }
    if (!payload || !Object.prototype.hasOwnProperty.call(payload, 'data')) throw new ApiError('The coordinator returned an invalid upload response.', response.status, 'invalid_response', null, true);
    return payload.data;
  }

  async function processTaskAttachmentBatch(batchId) {
    const batch = (await readAttachmentBatches()).find((item) => item.id === batchId);
    if (!batch) return;
    if (batch.project_id !== state.projectId || batch.task_id !== state.selectedTaskId || batch.actor_id !== actorId()) throw new Error('Open the same task with the same account to resume this upload.');
    for (const record of batch.files) {
      if (record.uploaded) continue;
      attachmentStatus(`Uploading ${record.filename}…`);
      if (!record.artifact_id) {
        const reserved = (await request(`${projectPath(batch.project_id)}/artifacts/uploads`, { method: 'POST', body: record.reservation_body, idempotencyKey: record.reservation_key })).data;
        record.artifact_id = reserved.artifact?.id;
        record.upload_path = reserved.upload_path;
        if (!record.artifact_id || !record.upload_path) throw new Error('The upload reservation response was incomplete. The saved request is ready to retry.');
        await storeAttachmentBatch(batch);
      }
      await putAttachmentBytes(record.upload_path, record);
      record.uploaded = true;
      await storeAttachmentBatch(batch);
    }
    await deleteAttachmentBatch(batch.id);
  }

  async function runTaskAttachmentBatches(batches) {
    if (state.attachmentBusy || state.mutation) return;
    state.attachmentBusy = true;
    $('upload-task-attachments').disabled = true; $('retry-task-attachments').disabled = true; $('task-attachment-files').disabled = true;
    try {
      for (const batch of batches) await processTaskAttachmentBatch(batch.id);
      await loadTaskAttachments(state.projectId, state.selectedTaskId, true);
      attachmentStatus('Attachments uploaded and saved with this task. Agents can inspect them in task history and download the original files.');
      setGlobalAlert('', 'success');
    } catch (error) {
      attachmentStatus(`Upload paused. The saved bytes and request keys are retained for retry: ${errorMessage(error)}`, true);
      await loadTaskAttachments(state.projectId, state.selectedTaskId, true);
      setGlobalAlert('The attachment upload did not finish. Retry the saved upload to resume safely.', 'error');
    } finally {
      state.attachmentBusy = false; $('upload-task-attachments').disabled = Boolean(state.mutation); $('task-attachment-files').disabled = Boolean(state.mutation);
      $('retry-task-attachments').disabled = Boolean(state.mutation);
    }
  }

  async function uploadTaskAttachments() {
    if (state.attachmentBusy || state.mutation || !state.selectedTaskId) return;
    const files = Array.from($('task-attachment-files').files || []);
    if (!files.length) { attachmentStatus('Choose one or more files first.', true); return; }
    attachmentStatus('Saving upload bytes and request keys…');
    try {
      const batch = await makeAttachmentBatch(state.projectId, state.selectedTaskId, files);
      await storeAttachmentBatch(batch);
      $('task-attachment-files').value = '';
      await runTaskAttachmentBatches([batch]);
    } catch (error) { attachmentStatus(errorMessage(error), true); }
  }

  async function retryTaskAttachments() {
    if (state.attachmentBusy || state.mutation) return;
    try {
      const batches = (await readAttachmentBatches()).filter((batch) => batch.project_id === state.projectId && batch.task_id === state.selectedTaskId && batch.actor_id === actorId());
      if (!batches.length) { attachmentStatus('No pending uploads need a retry.'); return; }
      await runTaskAttachmentBatches(batches);
    } catch (error) { attachmentStatus(errorMessage(error), true); }
  }

  function renderCheckpoints(data) {
    const target = $('checkpoints-content'); clear(target); let checkpoints = Array.isArray(data.checkpoints) ? data.checkpoints : [];
    if (!checkpoints.length && Array.isArray(data.attempts)) data.attempts.forEach((attempt) => { if (Array.isArray(attempt.checkpoints)) checkpoints = checkpoints.concat(attempt.checkpoints); });
    checkpoints = checkpoints.slice().sort((a, b) => new Date(b.created_at || b.at).getTime() - new Date(a.created_at || a.at).getTime());
    if (!checkpoints.length) { add(target, el('p', 'muted', 'No checkpoints have been shared yet.')); return; }
    checkpoints.forEach((point) => { const item = el('div', 'checkpoint'); const top = el('div', 'checkpoint-top'); add(top, el('span', '', point.actor_name || point.owner_id || 'Agent')); add(top, el('time', '', formatDate(point.created_at || point.at))); add(item, top); add(item, el('p', 'checkpoint-summary', point.summary || 'Progress update')); if (point.current_action) add(item, el('p', 'checkpoint-detail', `Current action · ${point.current_action}`)); if (point.next_step) add(item, el('p', 'checkpoint-detail', `Next step · ${point.next_step}`)); if (Array.isArray(point.blockers) && point.blockers.length) add(item, el('p', 'checkpoint-detail', `Blockers · ${point.blockers.join(', ')}`)); add(target, item); });
  }

  function openDialog(kind) {
    const dialog = document.createElement('dialog'); dialog.className = 'form-dialog'; setupHelpDismissal(dialog); const form = el('form'); form.method = 'dialog';
    const title = kind === 'project' ? 'Create project' : 'Create task'; add(form, el('div', 'dialog-header', ''));
    const header = form.firstChild; add(header, el('h2', '', title)); const close = el('button', 'icon-button', '×'); close.type = 'button'; close.setAttribute('aria-label', 'Close'); close.addEventListener('click', () => dialog.close()); add(header, close);
    const fields = [];
    const field = (label, id, type = 'input', placeholder = '') => { const wrap = el('div', 'dialog-field'); const labelNode = el('label', '', label); labelNode.htmlFor = id; const input = document.createElement(type); input.id = id; input.name = id; input.placeholder = placeholder; add(wrap, labelNode); add(wrap, input); add(form, wrap); fields.push(input); return input; };
    if (kind === 'project') { field('Project name', 'project-name', 'input', 'e.g. Atlas API'); field('Repository URL', 'repository-url', 'input', 'https://…'); field('Target branch', 'target-branch', 'input', 'main'); setupFieldHelp(form); }
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

  function startPolling() { if (state.pollTimer) clearInterval(state.pollTimer); state.pollTimer = setInterval(() => { if (!state.actor || state.mutation) return; if (state.currentView === 'overview') loadProjects(true); else if (state.currentView === 'tasks' && state.taskAutoRefresh) loadTasks(true); else if (state.currentView === 'task-detail') loadTaskDetail(true); else if (state.currentView === 'admin') loadCredentials(true); else if (state.currentView === 'resources' && !state.resourcePagesExtended) loadResources(true); }, 5000); }

  $('login-form').addEventListener('submit', (event) => { event.preventDefault(); const username = $('username').value.trim(); const password = $('password').value; if (!username || !password) { showLoginError('Enter your username and password.'); return; } showLoginError(''); startMutation('/api/v1/auth/login', { username, password }, 'sign-in', async (data) => { applySession(data); $('password').value = ''; restorePendingMutation(); await loadProjects(); startPolling(); }, 'POST', (error) => showLoginError(errorMessage(error))); });
  document.querySelectorAll('button.nav-item').forEach((button) => button.addEventListener('click', () => { const view = button.dataset.view; showView(view); if (view === 'tasks' && state.projectId) loadTasks(); if (view === 'admin') { loadCredentials(); loadOperators(); loadRestore(); loadClock(); } if (view === 'resources') loadResources(); if (view === 'shared') { fillSharedProject(); loadShared(); } }));
  $('brand-button').addEventListener('click', () => showView('overview')); $('new-project-button').addEventListener('click', () => openDialog('project')); $('new-task-button').addEventListener('click', () => openDialog('task'));
  $('refresh-projects').addEventListener('click', () => loadProjects()); $('refresh-tasks').addEventListener('click', () => loadTasks());
  $('tasks-auto-refresh').checked = state.taskAutoRefresh;
  $('tasks-auto-refresh').addEventListener('change', (event) => {
    state.taskAutoRefresh = event.target.checked;
    try { localStorage.setItem(TASK_AUTO_REFRESH_KEY, String(state.taskAutoRefresh)); } catch { /* Keep the current-page preference if storage is unavailable. */ }
  });
  $('task-queue-tab').addEventListener('click', () => selectTaskView('queue'));
  $('completed-tasks-tab').addEventListener('click', () => selectTaskView('completed'));
  $('archived-tasks-tab').addEventListener('click', () => selectTaskView('archived'));
  $('task-view-tabs').addEventListener('keydown', (event) => {
    const tabs = [...$('task-view-tabs').querySelectorAll('[role="tab"]')], index = tabs.indexOf(event.target);
    if (index < 0 || !['ArrowLeft', 'ArrowRight', 'Home', 'End'].includes(event.key)) return;
    event.preventDefault();
    const next = event.key === 'Home' ? 0 : event.key === 'End' ? tabs.length - 1 : (index + (event.key === 'ArrowRight' ? 1 : tabs.length - 1)) % tabs.length;
    tabs[next].focus(); tabs[next].click();
  });
  $('tasks-first-page').addEventListener('click', () => showTaskPage(0));
  $('tasks-previous-page').addEventListener('click', () => showTaskPage(Math.max(0, (state.taskView === 'completed' ? state.completedPageIndex : state.taskPageIndex) - 1)));
  $('tasks-next-page').addEventListener('click', () => showTaskPage((state.taskView === 'completed' ? state.completedPageIndex : state.taskPageIndex) + 1));
  $('tasks-last-page').addEventListener('click', showLastTaskPage);
  $('tasks-page-size').addEventListener('change', (event) => { state.taskPageSize = Number(event.target.value); if (state.taskView === 'completed') showTaskPage(state.completedPageIndex); else { rebuildQueuePages(); showTaskPage(0); } });
  $('project-select').addEventListener('change', (event) => { state.projectId = event.target.value; resetTaskPages(); state.taskView = 'queue'; updateTaskViewTabs(); $('new-task-button').disabled = !state.projectId; $('refresh-tasks').disabled = !state.projectId; loadTasks(); });
  $('status-filter').addEventListener('change', () => { state.taskPageIndex = 0; rebuildQueuePages(); showTaskPage(0); });
  $('back-to-tasks').addEventListener('click', () => { if (state.detailOrigin === 'archived') { showView('archived'); loadArchivedTasks(); } else showView('tasks'); }); $('copy-task-name').addEventListener('click', () => copyTaskDetailValue($('copy-task-name'), 'Task name', 'task-detail-heading')); $('copy-task-id').addEventListener('click', () => copyTaskDetailValue($('copy-task-id'), 'Task ID', 'detail-task-id-value')); $('refresh-credentials').addEventListener('click', () => loadCredentials()); $('issue-form').addEventListener('submit', (event) => { event.preventDefault(); const input = $('agent-name'); if (!input.value.trim()) return; startMutation(`/api/v1/admin/agents`, { name: input.value.trim() }, 'credential issuance', async (data) => { input.value = ''; downloadIssuedCredential(data); await loadCredentials(); }); });
  $('refresh-archive').addEventListener('click',loadArchivedTasks); $('archive-project-select').addEventListener('change',loadArchivedTasks);
  function showToken(token) { clearCredentialDownload(); setText($('issued-token'), token || 'The token was not returned. Revoke this credential and issue a replacement.'); show($('token-reveal'), true); }
  function clearCredentialDownload() {
    if (state.credentialDownloadUrl) URL.revokeObjectURL(state.credentialDownloadUrl);
    state.credentialDownloadUrl = null;
    $('download-credential').removeAttribute('href');
    setText($('credential-download-name'), ''); show($('credential-download'), false);
    setText($('issue-feedback'), ''); show($('issue-feedback'), false);
  }
  function downloadIssuedCredential(data) {
    clearCredentialDownload(); setText($('issued-token'), ''); show($('token-reveal'), false);
    if (data?.secret_unavailable || typeof data?.token !== 'string' || !data.token) {
      setIssueFeedback(issuedCredentialFeedback(data), 'error'); return;
    }
    try {
      showToken(data.token);
      const contents = `[[credentials]]\norigin = ${JSON.stringify(window.location.origin)}\ntoken = ${JSON.stringify(data.token)}\n`;
      state.credentialDownloadUrl = URL.createObjectURL(new Blob([contents], {type:'application/toml;charset=utf-8'}));
      const link = $('download-credential'); link.href = state.credentialDownloadUrl;
      setText($('credential-download-name'), `Credential file for ${data.name || data.principal_id || 'this agent'}`);
      show($('credential-download'), true); link.click();
      setIssueFeedback(issuedCredentialFeedback(data), 'success');
    } catch (_) {
      if (state.credentialDownloadUrl) setIssueFeedback('The credential was issued, but automatic download could not start. Use Download credentials.toml again below.', 'error');
      else setIssueFeedback(`The credential was issued, but its file could not be prepared. Revoke ${data.credential_id || data.id || 'this credential'} and issue a replacement.`, 'error');
    }
  }
  $('clear-credential-download').addEventListener('click', clearCredentialDownload);
  window.addEventListener('pagehide', clearCredentialDownload);
  function setIssueFeedback(message, kind) { const target = $('issue-feedback'); target.className = `inline-alert ${kind}`; setText(target, message); show(target, true); }
  function selectIssuedToken() {
    const token = $('issued-token'), selection = window.getSelection();
    if (!selection) return;
    const range = document.createRange(); range.selectNodeContents(token); selection.removeAllRanges(); selection.addRange(range);
  }
  $('clear-token').addEventListener('click', () => { setText($('issued-token'), ''); show($('token-reveal'), false); });
  $('copy-token').addEventListener('click', async () => {
    const token = $('issued-token').textContent;
    if (!token) return;
    try {
      await navigator.clipboard.writeText(token);
      setIssueFeedback('Token copied to clipboard.', 'success');
    } catch (_) {
      selectIssuedToken();
      setIssueFeedback('Clipboard access was unavailable. The token is selected; copy it with your browser.', 'error');
    }
  });


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
    const dialog = el('dialog', 'form-dialog'); setupHelpDismissal(dialog); const form = el('form');
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
  let helpSerial = 0;
  const SETUP_HELP = {
    'project-name': 'A readable name for this project in the dashboard. It does not rename the Git repository. Example only: Atlas API.',
    'repository-url': 'Use the Git clone URL for this project. Supported GitHub HTTPS and SSH aliases share a repository identity automatically. Custom SSH host aliases can be linked by an administrator in Advanced repository aliases after the check roster is saved.',
    'target-branch': 'The Git branch where reviewed code will be integrated. Example only: main. Use the actual branch in your repository; creating a project does not create the branch.',
    repository_identity: 'The repository identity groups URL aliases of the same Git repository so projects coordinate integration to the same target. Normal setup derives it from the URL and preserves saved bindings. Administrators can link custom aliases after verifying they refer to the same repository.',
    canonical_repository_key: 'Copy the saved identity from a project using the same repository. This advanced administrator setting links aliases that cannot be inferred, such as custom SSH hosts. Existing workflow evidence and shared bindings prevent unsafe changes.',
    identity: 'Copy check_identity exactly from the producer configuration used with jobs run. Example only: workspace-tests. A producer must be configured, registered and run on a workstation or CI runner; saving this roster does not create or run it.',
    version: 'Copy check_version exactly from the registered producer. This identifies the check definition, such as its commands and configuration, not the application version. Example only: v1. A different definition version needs a matching roster entry.',
    environment: 'Copy check_environment exactly from the registered producer. This label identifies its validation environment; the service does not choose or start a runner from this value. Example only: linux-ci. Use the actual configured label, including its case.',
    review_mode: 'Independent agent requires a reviewer who did not contribute. Human requires a person. Both requires both reviews; Either accepts one independent agent or human review. None removes the review requirement. Required code checks still apply.',
    automatic_integration: 'When allowed, agents may integrate a candidate after required reviews and checks pass. Otherwise a human must authorize each candidate before integration. This setting does not create or run checks.',
    allow_subagent_reviews: 'When enabled, a separately registered project subagent may review its parent’s work only if it did not contribute. Otherwise the reviewer must be an independently enrolled agent. Human reviews always require a person.',
    recovery_mode: 'Choose who may inspect and recover expired work. Expiry does not prove a workstation job stopped. The selected agent or human must inspect saved work and any remaining jobs or resource holds before continuing.',
    agent_rule_editing: 'Allow agents to change the binding rules text for this project. Humans still control review, integration, recovery and lease settings. Leave this off when rule changes need an operator.',
    lease_seconds: 'How long task ownership lasts without renewal, from 30 to 3600 seconds. Example only: 600 means ten minutes. Agents must renew before expiry; reading a task or saving a checkpoint does not renew ownership.',
    rules: 'Instructions every agent working on this project must follow, such as required validation and repository conventions. Saving a change creates a new policy revision that agents must read and acknowledge.',
    provenance: 'Explain why the policy is changing and cite the request, decision or other supporting source so later workers can understand it. Use your actual reason and source.'
  };
  const helpDismissals = new WeakMap();
  function setupHelpDismissal(dialog) {
    const dismissVisible = () => {
      let dismissed = false;
      for (const panel of dialog.querySelectorAll('[role="tooltip"]')) {
        const dismiss = helpDismissals.get(panel);
        if (!panel.hidden && dismiss) { dismiss(); dismissed = true; }
      }
      return dismissed;
    };
    dialog.addEventListener('keydown', event => {
      if (event.key === 'Escape' && dismissVisible()) { event.preventDefault(); event.stopPropagation(); }
    }, true);
    dialog.addEventListener('cancel', event => { if (dismissVisible()) event.preventDefault(); });
  }
  function contextualHelp(target, control, label, description, panelTarget = null) {
    const wrapper = el('div', 'contextual-help');
    const button = el('button', 'button text-button', panelTarget ? 'Help' : `Help: ${label}`); button.type = 'button'; button.setAttribute('aria-label', `Help: ${label}`);
    const help = el('p', 'help-text', description); help.id = `context-help-${++helpSerial}`; help.hidden = true; help.setAttribute('role', 'tooltip');
    button.setAttribute('aria-controls', help.id); button.setAttribute('aria-expanded', 'false'); button.setAttribute('aria-describedby', help.id);
    control.setAttribute('aria-describedby', [control.getAttribute('aria-describedby'), help.id].filter(Boolean).join(' '));
    let pinned = false, controlFocused = false, buttonFocused = false, hideTimer;
    const hovered = new Set();
    const update = () => { const visible = pinned || hovered.size > 0 || controlFocused || buttonFocused; show(help, visible); button.setAttribute('aria-expanded', String(visible)); };
    for (const element of [control, wrapper, help]) {
      element.addEventListener('mouseenter', () => { clearTimeout(hideTimer); hovered.add(element); update(); });
      element.addEventListener('mouseleave', () => { hovered.delete(element); clearTimeout(hideTimer); hideTimer = setTimeout(update, 150); });
    }
    control.addEventListener('focus', () => { controlFocused = true; update(); });
    control.addEventListener('blur', () => { controlFocused = false; update(); });
    button.addEventListener('focus', () => { buttonFocused = true; update(); });
    button.addEventListener('blur', () => { buttonFocused = false; update(); });
    button.addEventListener('click', () => { pinned = !pinned; hovered.clear(); controlFocused = buttonFocused = false; update(); });
    const dismiss = () => { clearTimeout(hideTimer); pinned = controlFocused = buttonFocused = false; hovered.clear(); update(); };
    helpDismissals.set(help, dismiss);
    const dismissHere = event => {
      if (event.key === 'Escape' && !help.hidden) { event.preventDefault(); event.stopPropagation(); dismiss(); }
    };
    wrapper.addEventListener('keydown', dismissHere); control.addEventListener('keydown', dismissHere);
    add(wrapper, button);
    if (panelTarget) { help.classList.add('check-help-panel'); add(panelTarget, help); }
    else add(wrapper, help);
    add(target, wrapper); return wrapper;
  }
  function setupFieldHelp(form) {
    for (const control of form.querySelectorAll('input[name], select[name], textarea[name]')) {
      const description = SETUP_HELP[control.name]; if (!description) continue;
      const label = form.querySelector(`label[for="${control.id}"]`)?.textContent || control.name;
      const wrapper = contextualHelp(control.parentElement, control, label, description); control.after(wrapper);
    }
  }
  function workflowDialog(title, description) {
    const dialog = el('dialog', 'form-dialog'); setupHelpDismissal(dialog); const form = el('form');
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
    const help = (input, label, description) => contextualHelp(form, input, label, description);
    return { dialog, form, field, help, finish };
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
    const guidance = {
      review: 'Work has been submitted for review. The submission is a saved version of the result and its evidence; edits require a new submission. ' + ((workflow.activities || []).some(activity => activity.kind === 'either_review') ? 'An independent agent or a human may claim this review. One approval satisfies the review requirement; requesting changes requires a new submission. Claiming reserves the review and does not approve the work.' : (workflow.activities || []).some(activity => activity.kind === 'human_review') ? 'Review the evidence below, then claim an available human review to approve it or request changes. Claiming reserves the review for you; it does not approve the work.' : 'An independent agent must claim the required review and approve the work or request changes.'),
      integration: 'The submitted code is awaiting integration: an agent must validate the required checks, publish the result, and finalize the task. If shown, Authorize integration gives permission to proceed.',
      revision_needed: 'Changes are needed. An agent must claim the task, revise the work, and submit a new version for review. The previous submission stays in the history.',
      done: 'This submission has completed the required workflow.'
    };
    if (guidance[workflow.phase]) add(target, el('p', 'inline-alert', guidance[workflow.phase]));
    const candidate = el('div', 'workflow-entry'); add(candidate, el('h3', '', workflow.phase === 'revision_needed' ? 'Previous candidate' : 'Current candidate'));
    add(candidate, el('p', '', submission.summary));
    const details = el('dl');
    [['Submission', submission.id], ['Source revision', submission.candidate_revision], ['Source tree', submission.candidate_tree], ['Candidate remote identity', submission.candidate_remote], ['Candidate checkpoint ref', submission.candidate_ref], ['Task revision', submission.task_revision], ['Policy revision', submission.project_policy_revision]].filter(([,value]) => value !== null && value !== undefined).forEach(([label,value]) => { add(details, el('dt', 'muted', label)); add(details, el('dd', '', value)); });
    if (submission.kind === 'code' && !submission.candidate_ref) add(candidate, el('p', 'inline-alert warning', 'Legacy submission has no durable candidate checkpoint. Ask an operator to reopen it before another workstation reviews or integrates its code.'));
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
      const action = (label, callback, help) => { const button = el('button', 'button subtle', label); button.type = 'button'; button.dataset.mutation = 'true'; button.addEventListener('click', callback); add(entry, button); if (help) contextualHelp(entry, button, label, help); };
      if (state.actor?.kind === 'human' && !['done','completed','canceled'].includes(activity.status)) {
        if (['human_review', 'either_review'].includes(activity.kind) && !(workflow.blockers || []).length) {
          const current = attempt?.state === 'active' && attempt.valid_by_time === true && attempt.owner_authorized === true;
          if (current && attempt.owner_id === actorId() && attempt.session_id === state.actor.session_id) action('Record human review', () => openHumanReview(activity, submission, attempt));
          else if (!current) action('Claim human review', () => workflowMutation(`${projectPath()}/workflow-activities/${encodeURIComponent(activity.id)}/claim`, {expected_submission_id: submission.id, expected_project_policy_revision: submission.project_policy_revision, expected_workflow_policy_revision: submission.workflow_policy_revision}, 'review claim', response => openHumanReview(activity, submission, response.attempt)), 'Reserve this review for your signed-in session and open the review form. Read the submission and acceptance evidence first. You will make a separate decision to approve or request changes. Code still needs integration and required checks after approval.');
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
    view.help(decision, 'Decision', 'Choose Approved when the submitted evidence satisfies the acceptance criteria. Choose Changes requested when more work is needed; a revised submission will need fresh review. Approval completes a general task after all required reviews; code must also pass integration.');
    const summary = view.field('summary', 'Review summary');
    view.help(summary, 'Review summary', 'Describe what you inspected and why you approve or request changes. Refer to acceptance criteria, results, or evidence links. For example: Checked the reported test results; the keyboard interaction still needs verification.');
    const findings = view.field('findings', 'Required remedies — one per line'); findings.required = false;
    view.help(findings, 'Required remedies', 'For changes requested, describe each required fix on its own line so the next agent knows what to change. Leave this empty when approving.');
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
    const view = workflowDialog('Reconcile publication', 'Inspect the actual remote target and the previous publisher first. Agent reconciliation is limited to a fresh exact match for the saved base or intended result after the old publisher is stopped and all producers and reservations are quiescent. Changed targets, missing evidence, and uncertainty require this human review. Reconciliation cannot publish or manufacture a check result.');
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
      const view = workflowDialog('Required checks', 'Configure at least one required check. Saving this roster does not create or run checks. Changing it makes existing candidates require reconciliation. The examples in Help are explanations, not configured checks.');
      const rules = el('button', 'button subtle', `Review: ${displayStatus(project.review_mode)} · ${project.automatic_integration ? 'Automatic integration allowed' : 'Human integration authorization required'}`);
      rules.type = 'button'; rules.addEventListener('click', () => { view.dialog.close(); openReviewSettings(project); }); add(view.form, rules);
      add(view.form, el('p', 'repo repository-summary', `Repository: ${project.repository_url}`));
      const repositorySummary = add(view.form, el('p', 'muted repository-summary', policy.canonical_repository_key ? `Saved identity: ${policy.canonical_repository_key}` : 'Identity will be assigned automatically when you save the roster.'));
      contextualHelp(view.form, repositorySummary, 'Repository identity', SETUP_HELP.repository_identity);
      if (state.actor?.role === 'admin' && policy.revision) {
        const aliases = el('button', 'button subtle', 'Advanced repository aliases'); aliases.type = 'button';
        aliases.addEventListener('click', () => { view.dialog.close(); openRepositoryAliases(projectId, project, policy); }); add(view.form, aliases);
        const grants = el('button', 'button subtle', 'Task-definition editing grants'); grants.type = 'button';
        grants.addEventListener('click', () => { view.dialog.close(); openTaskDefinitionGrants(projectId); }); add(view.form, grants);
      }
      const rows = el('div'); add(view.form, rows); let serial = 0;
      const addRow = (check = {}) => {
        const row = el('div', 'check-row'); row.dataset.row = String(serial++); const helpPanels = document.createDocumentFragment();
        for (const [name,label] of [['identity','Check'],['version','Definition version'],['environment','Environment']]) {
          const wrap = el('div'), id = `check-${row.dataset.row}-${name}`, caption = el('label', '', label), input = el('input');
          caption.htmlFor = id; input.id = id; input.name = name; input.value = check[name] || ''; input.required = true; input.maxLength = 255;
          add(wrap, caption); add(wrap, input); contextualHelp(wrap, input, label, SETUP_HELP[name], helpPanels); add(row, wrap);
        }
        const remove = el('button', 'button subtle', 'Remove'); remove.type = 'button'; remove.addEventListener('click', () => row.remove()); add(row, remove); add(row, helpPanels); add(rows, row);
      };
      const checks = policy.required_checks || []; (checks.length ? checks : [{}]).forEach(addRow);
      const more = el('button', 'button subtle', 'Add required check'); more.type = 'button'; more.addEventListener('click', () => { if (rows.children.length < 100) addRow(); }); add(view.form, more);
      view.finish('Save check roster', (_, dialog) => {
        if (!rows.children.length) { setGlobalAlert('Configure at least one required check.', 'error'); return; }
        const required_checks = Array.from(rows.children).map(row => Object.fromEntries(Array.from(row.querySelectorAll('input')).map(input => [input.name,input.value.trim()])));
        dialog.close(); startMutation(`${projectPath(projectId)}/workflow-policy`, {expected_revision:policy.revision || 0, required_checks}, 'check roster', async () => { await loadProjects(); setGlobalAlert('Check roster saved. Agents must read the updated workflow policy.', 'success'); }, 'PUT', null, {projectId});
      });
    } catch (error) { setGlobalAlert(errorMessage(error), 'error'); }
  }
  function openRepositoryAliases(projectId, project, policy) {
    const view = workflowDialog('Advanced repository aliases', 'Administrator setup for clone URLs that cannot be inferred, such as a custom SSH host. Copy the saved identity from a project using the same repository. Confirm both URLs refer to that repository before saving. Changes are refused when they would split a shared binding or alter existing workflow evidence.');
    add(view.form, el('p', 'repo repository-summary', `Repository: ${project.repository_url}`));
    const key = view.field('canonical_repository_key', 'Repository identity to share', policy.canonical_repository_key, 'input'); key.maxLength = 255;
    setupFieldHelp(view.form);
    view.finish('Save repository alias', (_, dialog) => {
      dialog.close(); startMutation(`${projectPath(projectId)}/workflow-policy`, {expected_revision:policy.revision, canonical_repository_key:key.value.trim(), required_checks:policy.required_checks}, 'repository alias', async () => { await loadProjects(); setGlobalAlert('Repository alias saved. Agents must read the updated workflow policy.', 'success'); }, 'PUT', null, {projectId});
    });
  }
  async function openTaskDefinitionGrants(projectId) {
    try {
      const currentActor = actorId(), reply = await request(`${projectPath(projectId)}/task-definition-grants`);
      if (projectId !== state.projectId || currentActor !== actorId()) return;
      const view = workflowDialog('Task-definition editing grants', 'Only administrators can grant or revoke this authority. A principal grant names one agent. The current agent-role grant applies to every valid project agent, so use it only when that wider scope is intended. Grants never permit policy, review, or self-contributed task-definition changes.');
      const grants = reply.data.items || [];
      if (grants.length) {
        const list = el('div', 'record-list');
        grants.forEach(grant => {
          const row = el('div', 'card'); const target = grant.target_kind === 'role' ? `Role: ${grant.agent_role}` : `Agent: ${grant.agent_principal_id}`;
          add(row, el('p', '', `${target} · ${grant.revoked_at ? 'Revoked' : 'Active'}`)); add(row, el('p', 'muted', `Grant ${grant.id} · revision ${grant.revision}`));
          if (!grant.revoked_at) { const revoke = el('button', 'button subtle', 'Revoke'); revoke.type = 'button'; revoke.addEventListener('click', () => { view.dialog.close(); startMutation(`${projectPath(projectId)}/task-definition-grants/${encodeURIComponent(grant.id)}`, {expected_revision:grant.revision}, 'task-definition grant revocation', () => openTaskDefinitionGrants(projectId), 'POST', null, {projectId}); }); add(row, revoke); }
          add(list, row);
        }); add(view.form, list);
      } else add(view.form, el('p', 'muted', 'No task-definition editing grants are recorded. Agents are denied by default.'));
      const kind = view.field('target_kind', 'Grant target', 'principal', 'select');
      for (const [value,label] of [['principal','One agent principal'],['role','All valid agents (agent role)']]) { const option = el('option', '', label); option.value = value; add(kind, option); }
      const principal = view.field('agent_principal_id', 'Agent principal ID', '', 'input'); principal.maxLength = 128;
      kind.addEventListener('change', () => { principal.disabled = kind.value === 'role'; principal.required = kind.value === 'principal'; }); principal.required = true;
      view.finish('Create grant', (values, dialog) => { const body = values.get('target_kind') === 'role' ? {target_kind:'role',agent_role:'agent'} : {target_kind:'principal',agent_principal_id:values.get('agent_principal_id').trim()}; dialog.close(); startMutation(`${projectPath(projectId)}/task-definition-grants`, body, 'task-definition grant', () => openTaskDefinitionGrants(projectId), 'POST', null, {projectId}); });
    } catch (error) { setGlobalAlert(errorMessage(error), 'error'); }
  }
  function openReviewSettings(project) {
    const projectId = state.projectId;
    const view = workflowDialog('Review and integration rules', 'These rules apply to new submissions. Existing candidates remain bound to their recorded policy revision and need explicit reconciliation after a policy change.');
    const mode = view.field('review_mode', 'Required review', '', 'select');
    for (const [value,label] of [['agent','Independent agent'],['human','Human'],['both','Independent agent and human'],['either','Independent agent or human'],['none','No required review']]) { const option = el('option', '', label); option.value = value; add(mode, option); } mode.value = project.review_mode;
    const integration = view.field('automatic_integration', 'Integration authorization', '', 'select');
    selectField(view, 'allow_subagent_reviews', 'Who may perform agent review?', project.allow_subagent_reviews || false, [['false','A separate credential principal'],['true','Also allow a registered, non-contributing subagent']]);
    for (const [value,label] of [['false','A human must authorize each candidate'],['true','Agents may integrate after required review']]) { const option = el('option', '', label); option.value = value; add(integration, option); } integration.value = String(project.automatic_integration);
    setupFieldHelp(view.form);
    view.finish('Save review rules', (values, dialog) => {
      dialog.close(); startMutation(`${projectPath(projectId)}/policy`, {expected_revision:project.policy_revision, review_mode:values.get('review_mode'), recovery_mode:project.recovery_mode, lease_seconds:project.lease_seconds, rules:project.rules, agent_rule_editing:project.agent_rule_editing, automatic_integration:values.get('automatic_integration') === 'true', allow_subagent_reviews:values.get('allow_subagent_reviews') === 'true'}, 'review rules', async () => { await loadProjects(); setGlobalAlert('Review rules saved. Agents must read and acknowledge the updated policy.', 'success'); }, 'PATCH', null, {projectId});
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
      const rules = view.field('rules', 'Rules', project.rules); rules.maxLength = 32768; rules.required = false;
      bindingRulesGuidance(rules);
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
  $('shared-project').addEventListener('change', event => { state.projectId = event.target.value; resetTaskPages(); state.selectedTaskId = ''; loadShared(); });
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
  function taskIconButton(label, icon, action) {
    const button = actionButton('', action);
    const paths = {
      archive: '<rect x="4" y="5" width="16" height="4" rx="1"/><path d="M6 9v10h12V9m-8 4h4"/>',
      cancel: '<circle cx="12" cy="12" r="9"/><path d="m9 9 6 6m0-6-6 6"/>',
      edit: '<path d="M12 20h9"/><path d="M16.5 3.5a2.1 2.1 0 0 1 3 3L9 17l-4 1 1-4Z"/>',
      resolve: '<path d="m5 12 4 4L19 6"/><circle cx="12" cy="12" r="9"/>'
    };
    button.classList.add('task-action-icon'); button.title = label; button.setAttribute('aria-label', label);
    button.innerHTML = `<svg viewBox="0 0 24 24" aria-hidden="true">${paths[icon]}</svg>`;
    return button;
  }
  function selectField(view, name, label, value, choices) {
    const select = view.field(name, label, '', 'select');
    choices.forEach(([key, caption]) => { const option = el('option', '', caption); option.value = key; add(select, option); });
    select.value = String(value); return select;
  }
  function lines(value) { return text(value).split('\n').map(line => line.trim()).filter(Boolean); }
  function bindingRulesGuidance(control) {
    const guidance = el('p', 'help-text binding-rules-guidance', 'Binding rules are project-wide instructions that agents must follow. Use them for recurring requirements, such as “Run the workspace tests before submitting code” or “Keep credentials outside the repository.” Put requirements for one task in that task’s description and acceptance criteria. Leave this field empty if no additional project rules are needed. Agents receive these rules with the project instructions; saving changes requires them to read and acknowledge the new policy before claiming work. This text does not run checks or replace the review and integration settings.');
    guidance.id = `binding-rules-guidance-${++helpSerial}`;
    control.setAttribute('aria-describedby', guidance.id); control.after(guidance);
  }
  async function openProjectPolicy() {
    if (!state.projectId) { setGlobalAlert('Choose a project first.'); return; }
    const projectId = state.projectId, currentActor = actorId();
    try {
      const project = (await request(`${projectPath(projectId)}/orientation`)).data.project;
      if (state.projectId !== projectId || actorId() !== currentActor) return;
      const view = workflowDialog('Project policy', 'Changes create a new policy revision. Agents must reread the policy before claiming work; existing submissions may need reconciliation.');
      selectField(view, 'review_mode', 'Required review', project.review_mode, [['agent','Independent agent'],['human','Human'],['both','Independent agent and human'],['either','Independent agent or human'],['none','No required review']]);
      selectField(view, 'recovery_mode', 'Expired work recovery', project.recovery_mode, [['agent','Agents may inspect and recover'],['manual','A human must inspect and recover']]);
      selectField(view, 'automatic_integration', 'Integration authorization', project.automatic_integration, [['false','Human authorization for each candidate'],['true','Agents may integrate approved candidates']]);
      selectField(view, 'agent_rule_editing', 'Agent policy changes', project.agent_rule_editing, [['false','Only humans may change binding rules'],['true','Agents may change binding project rules']]);
      selectField(view, 'allow_subagent_reviews', 'Who may perform agent review?', project.allow_subagent_reviews || false, [['false','A separate credential principal'],['true','Also allow a registered, non-contributing subagent']]);
      const lease = view.field('lease_seconds','Ownership lease (seconds)',project.lease_seconds,'input'); lease.type = 'number'; lease.min = '30'; lease.max = '3600';
      const rules = view.field('rules','Binding rules',project.rules); rules.maxLength = 32768; rules.required = false;
      bindingRulesGuidance(rules);
      view.field('provenance','Reason and supporting source').maxLength = 4096;
      setupFieldHelp(view.form);
      view.finish('Save policy', (values, dialog) => {
        const body = {expected_revision:project.policy_revision, review_mode:values.get('review_mode'), recovery_mode:values.get('recovery_mode'), automatic_integration:values.get('automatic_integration') === 'true', agent_rule_editing:values.get('agent_rule_editing') === 'true', allow_subagent_reviews:values.get('allow_subagent_reviews') === 'true', lease_seconds:Number(values.get('lease_seconds')), rules:values.get('rules'), provenance:values.get('provenance')};
        dialog.close(); startMutation(`${projectPath(projectId)}/policy`, body, 'project policy', async () => { await loadProjects(); setGlobalAlert('Project policy saved.', 'success'); }, 'PATCH', null, {projectId});
      });
    } catch (error) { setGlobalAlert(errorMessage(error)); }
  }
  function renderTaskActions(data, task) {
    const target = $('task-operator-actions'); clear(target);
    const completionPending = data.workflow?.submission && data.workflow.phase !== 'revision_needed';
    if (state.actor?.kind === 'human' && !task.activity_kind && !task.current_attempt_id) {
      if (task.archived_at) add(target, actionButton('Restore archived task', () => taskLifecycle(task,'restore','Restore this task to the main task queue? Its lifecycle state and history will be preserved.')));
      else {
        if (['open','planned','canceled'].includes(task.lifecycle)) add(target, taskIconButton('Archive task', 'archive', () => taskLifecycle(task,'archive','Archive this task? It will leave the main task queue and remain available in Archived tasks.')));
        if (['open','planned'].includes(task.lifecycle)) add(target, taskIconButton('Cancel task', 'cancel', () => taskLifecycle(task,'cancel','Cancel this task? It will remain in the task queue with canceled status.')));
        if (['planned','canceled'].includes(task.lifecycle)) add(target, actionButton('Delete task', () => taskLifecycle(task,'delete','Remove this task from task views? This is a soft delete that retains task and audit history. The service refuses if attempts, workflow, objective, or dependency history exist.')));
      }
    }
    if (!completionPending && !task.current_attempt_id && ['open','planned'].includes(task.lifecycle) && ['ready','planned','blocked'].includes(taskStatus(task)) && !task.activity_kind) add(target, taskIconButton('Edit task', 'edit', () => editTask(task, data)));
    if (!completionPending && !task.activity_kind && task.lifecycle === 'open' && !task.current_attempt_id && task.blocked_reason) add(target, taskIconButton('Resolve blocker', 'resolve', () => {
      const projectId = state.projectId, taskId = task.id;
      const view = workflowDialog('Resolve saved blocker', 'Use this after the problem that stopped work has been fixed. Record what changed and how you checked it. This clears the saved blocker so an agent can claim the task when its other requirements are met.');
      add(view.form, el('h3', '', 'What stopped work'));
      add(view.form, el('p', 'saved-blocker', task.blocked_reason));
      const reason = view.field('reason','What changed and how you verified it'); reason.maxLength = 4096;
      reason.placeholder = 'Example: Repository access was restored. Verified that the assigned workstation can fetch the required branch.';
      view.help(reason, 'Resolution evidence', 'Write the concrete fix and the observation, check result, or link that confirms work can resume. Use your actual evidence, not the example. This records the resolution in task history; it does not approve submitted work or mark the task complete.');
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
  function taskLifecycle(task, action, confirmation) {
    const projectId=state.projectId, taskId=task.id;
    const view=workflowDialog(({archive:'Archive task',restore:'Restore task',cancel:'Cancel task',delete:'Delete task'})[action],confirmation);
    view.field('reason','Reason for this action');
    view.finish(({archive:'Archive task',restore:'Restore task',cancel:'Cancel task',delete:'Delete task'})[action],(values,dialog)=>{
      dialog.close(); const path=`${projectPath(projectId)}/tasks/${encodeURIComponent(taskId)}`;
      const callback=async()=>{if(action==='delete'){showView('tasks');await loadTasks();}else await loadTaskDetail();};
      startMutation(action==='delete'?path:`${path}/${action}`,{expected_revision:task.revision,reason:values.get('reason')},`task ${action}`,callback,action==='delete'?'DELETE':'POST',null,{projectId,taskId});
    });
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
  $('upload-task-attachments').addEventListener('click', uploadTaskAttachments);
  $('retry-task-attachments').addEventListener('click', retryTaskAttachments);
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
      const themeGroup = el('fieldset', 'theme-selector'); add(themeGroup, el('legend', '', 'Website theme'));
      const savedTheme = (() => { try { return localStorage.getItem(THEME_KEY) || 'system'; } catch { return 'system'; } })();
      [['light','Light'],['dark','Dark'],['system','System']].forEach(([value,label]) => {
        const choice = el('label', 'theme-choice'); const radio = document.createElement('input'); radio.type = 'radio'; radio.name = 'site-theme'; radio.value = value; radio.checked = savedTheme === value;
        radio.addEventListener('change', () => applyTheme(value)); add(choice, radio); add(choice, el('span', '', label)); add(themeGroup, choice);
      });
      add(view.form, themeGroup);
      add(view.form,actionButton('View my browser sessions', () => { view.dialog.close(); browserSessions(operator.id,operator.name); },false));
      add(view.form,actionButton('Sign out', () => { view.dialog.close(); startMutation('/api/v1/auth/logout', {}, 'sign-out', async () => signOutLocal()); }));
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
