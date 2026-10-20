/* Agent Coordinator dashboard. Same-origin API client; no framework required. */
(() => {
  'use strict';

  const $ = (id) => document.getElementById(id);
  const TASK_AUTO_REFRESH_KEY = 'agent-coordinator.task-auto-refresh';
  function savedTaskAutoRefresh() {
    try { return localStorage.getItem(TASK_AUTO_REFRESH_KEY) === 'true'; } catch { return false; }
  }
  const state = {
    actor: null, csrfToken: null, projects: [], tasks: [], credentials: [], credentialsLoaded: false, operators: [], restoreItems: [], resources: [], resourcesLoaded: false, resourceCursor: null, resourcePagesExtended: false, sharedCursor: null, sharedItems: [], sharedRules: undefined, sharedKey: '', sharedLoaded: false, sharedSeq: 0, reportsCursor: null, reports: [], reportsLoaded: false, reportsProjectId: '', reportsSeq: 0, agentSessionsSeq: 0, agentSessionsProjectId: '', agentSessionsLoaded: false, digestSeq: 0, digestProjectId: '', digestLoaded: false, autonomySeq: 0, autonomyProjectId: '', autonomyLoaded: false, autonomySummaries: new Map(),
    projectId: '', projectsRendered: false, selectedTaskId: '', currentView: 'overview', detail: null, detailKey: '', credentialDownloadUrl: null,
    taskPages: [], taskPageIndex: 0, taskPageSize: 25, taskView: 'queue', taskSearch: { queue: '', completed: '' }, taskAutoRefresh: savedTaskAutoRefresh(), taskQueueLoaded: false, queueTasks: [], completedTasks: [], completedLoaded: false, completedPageIndex: 0, inflightCompleted: false, archivedProjects: [], archivedProjectsLoaded: false, archivedTasks: [], archivedLoaded: false, archivedPageIndex: 0, archivedProjectId: '', mutation: null, fetching: new Set(), inflight: { projects: false, tasks: null, detail: null, credentials: null },
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
  // Idempotent writes: each setter touches the DOM only when the value differs, so a refresh
  // that returns unchanged data records no mutation (see book/src/docs/web-ui-refresh.md).
  const setText = (node, value) => {
    if (!node) return;
    const next = text(value);
    if (node.textContent === next) return;
    // Edit a lone text node in place so a selection inside it is not discarded.
    if (node.childNodes.length === 1 && node.firstChild.nodeType === Node.TEXT_NODE) node.firstChild.data = next;
    else node.textContent = next;
  };
  const show = (node, visible) => { if (node && node.hidden === visible) node.hidden = !visible; };
  const setClass = (node, value) => { if (node && node.className !== value) node.className = value; };
  const setAttr = (node, name, value) => { if (node && node.getAttribute(name) !== text(value)) node.setAttribute(name, text(value)); };
  const setProp = (node, name, value) => { if (node && node[name] !== value) node[name] = value; };
  const setData = (node, name, value) => { if (node && node.dataset[name] !== text(value)) node.dataset[name] = text(value); };
  const clear = (node) => { while (node && node.firstChild) node.removeChild(node.firstChild); if (node) patched.delete(node); };
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
  // What a task's status badge shows. `work_status` stays the filterable status; the server's `processing`
  // object says whether anyone is acting on it. A live lease shows a claim, never that a process is running,
  // so the tooltip says so, and a stage the service has no live claim for is never animated. A stage only applies
  // to the work_status it belongs to, so blocked, recovery and terminal statuses are never overridden.
  const STAGE_COPY = {
    in_progress: { status: 'in_progress', label: 'In Progress', icon: 'working', note: 'An agent holds a live lease on this task.' },
    in_review: { status: 'waiting_review', label: 'In Review', icon: 'reviewing', note: 'A reviewer has claimed a required review and holds a live lease.' },
    awaiting_reviewer: { status: 'waiting_review', label: 'Awaiting Reviewer', icon: 'queued', note: 'The review is queued and no reviewer has claimed it yet.' },
    review_lease_expired: { status: 'waiting_review', label: 'Review Lease Expired', icon: 'lapsed', note: 'A reviewer claimed a review but the lease ended; it needs recovery before anyone is reviewing.' },
  };
  const statusIndicator = (task) => {
    const status = taskStatus(task), processing = task?.processing, copy = processing && STAGE_COPY[processing.stage];
    if (!copy || copy.status !== status) return { status, label: displayStatus(status), icon: null, active: false, lines: [] };
    const lines = [copy.note];
    if (processing.active) lines.push('The lease proves a claim, not that the process is running right now.');
    if (processing.stage === 'awaiting_reviewer' && processing.pending_reviews > 1) lines.push(`${processing.pending_reviews} reviews are pending.`);
    for (const holder of processing.holders || []) {
      const role = holder.role === 'reviewer' ? 'Reviewer' : 'Agent';
      lines.push(`${role}: ${holder.name || 'not recorded'}`, `Harness: ${holder.harness || 'not recorded'}`, `Workstation: ${holder.workstation || 'not recorded'}`, `Lease expires: ${holder.lease_expires_at ? formatDate(holder.lease_expires_at) : 'not recorded'}`);
    }
    return { status, label: copy.label, icon: copy.icon, stage: processing.stage, active: Boolean(processing.active), lines };
  };
  let tipCount = 0;
  // Fills `badge` from the task; leaves it untouched when nothing it shows has changed.
  function paintStatusBadge(badge, task) {
    const view = statusIndicator(task), signature = sig(view);
    setClass(badge, `status-badge ${view.status}${view.stage ? ` stage-${view.stage}` : ''}${view.active ? ' is-active' : ''}`);
    if (badge.dataset.paint === signature) return badge;
    badge.dataset.paint = signature; clear(badge);
    if (view.icon) { const icon = el('span', `status-icon ${view.icon}`); icon.setAttribute('aria-hidden', 'true'); add(badge, icon); }
    add(badge, el('span', 'status-label', view.label));
    if (view.lines.length) {
      const id = `status-tip-${++tipCount}`, tip = el('span', 'status-tip'); tip.id = id; tip.setAttribute('role', 'tooltip');
      view.lines.forEach((line) => add(tip, el('span', 'status-tip-line', line)));
      badge.tabIndex = 0; badge.setAttribute('aria-describedby', id); add(badge, tip);
    } else { badge.removeAttribute('tabindex'); badge.removeAttribute('aria-describedby'); }
    return badge;
  }
  const listData = (data) => Array.isArray(data) ? data : Array.isArray(data?.items) ? data.items : [];
  const newKey = () => {
    if (globalThis.crypto?.randomUUID) return crypto.randomUUID();
    return `${Date.now()}-${Math.random().toString(16).slice(2)}`;
  };

  // ---- Shared patching ----------------------------------------------------------------------
  // Every refreshed list is rendered from descriptors {key, sig, build}: `key` identifies the item,
  // `sig` is a string that changes exactly when what is shown changes, and `build()` creates the node.
  // reconcile() keeps a node whose key and sig match, rebuilds one whose sig changed, and adds or removes
  // the rest, so a refresh with unchanged data touches nothing. Sections that are not lists use
  // patchSection(). Both carry focus, selection, open <details> and scroll offsets across a rebuild.
  const sig = (...values) => JSON.stringify(values);
  const patched = new WeakMap();
  function pathWithin(root, node) {
    const path = [];
    while (node && node !== root) {
      const parent = node.parentNode;
      if (!parent) return null;
      path.unshift(Array.prototype.indexOf.call(parent.childNodes, node)); node = parent;
    }
    return node === root ? path : null;
  }
  const nodeAt = (root, path) => path.reduce((node, index) => node?.childNodes[index], root);
  // Remembers what the person is doing inside `root`.
  function captureUi(root) {
    const selection = window.getSelection(), saved = { focus: null, selection: null, details: [...root.querySelectorAll('details')].map((details) => details.open), scrolls: [] };
    if (root.contains(document.activeElement)) saved.focus = pathWithin(root, document.activeElement);
    if (selection && selection.rangeCount && !selection.isCollapsed) {
      const anchor = pathWithin(root, selection.anchorNode), focus = pathWithin(root, selection.focusNode);
      if (anchor && focus) saved.selection = { anchor, anchorOffset: selection.anchorOffset, focus, focusOffset: selection.focusOffset };
    }
    [root, ...root.querySelectorAll('*')].forEach((node) => { if (node.scrollTop || node.scrollLeft) saved.scrolls.push([pathWithin(root, node), node.scrollTop, node.scrollLeft]); });
    return saved;
  }
  // Gives the rebuilt `root` the state captured from the node it replaced.
  function restoreUi(root, saved) {
    [...root.querySelectorAll('details')].forEach((details, index) => { if (saved.details[index] && !details.open) details.open = true; });
    saved.scrolls.forEach(([path, top, left]) => { const node = nodeAt(root, path); if (node) { node.scrollTop = top; node.scrollLeft = left; } });
    if (saved.focus) { const node = nodeAt(root, saved.focus); if (node?.focus) node.focus({ preventScroll: true }); }
    if (saved.selection) {
      const { anchor, anchorOffset, focus, focusOffset } = saved.selection, from = nodeAt(root, anchor), to = nodeAt(root, focus), limit = (node) => node.nodeType === Node.TEXT_NODE ? node.length : node.childNodes.length;
      if (from && to && anchorOffset <= limit(from) && focusOffset <= limit(to)) window.getSelection().setBaseAndExtent(from, anchorOffset, to, focusOffset);
    }
  }
  function reconcile(parent, descriptors) {
    if (!parent) return;
    const used = new Set(), wanted = descriptors.map((item) => {
      let key = text(item.key), copy = 0;
      while (used.has(key)) key = `${text(item.key)}#${++copy}`;
      used.add(key); return { ...item, key };
    });
    const existing = new Map();
    for (const child of [...parent.children]) {
      const key = patched.get(child)?.key;
      if (key !== undefined && used.has(key) && !existing.has(key)) existing.set(key, child); else child.remove();
    }
    const restores = []; let cursor = parent.firstElementChild;
    for (const item of wanted) {
      let node = existing.get(item.key);
      if (node && patched.get(node).sig !== item.sig) {
        const saved = captureUi(node), fresh = item.build();
        patched.set(fresh, { key: item.key, sig: item.sig }); node.replaceWith(fresh);
        if (cursor === node) cursor = fresh;
        node = fresh; restores.push([fresh, saved]);
      } else if (!node) { node = item.build(); patched.set(node, { key: item.key, sig: item.sig }); }
      if (node === cursor) cursor = cursor.nextElementSibling; else parent.insertBefore(node, cursor);
    }
    restores.forEach(([node, saved]) => restoreUi(node, saved));
  }
  // Rebuilds `container`'s content with build(container) only when `signature` changed; returns whether it did.
  function patchSection(container, signature, build) {
    if (!container || patched.get(container)?.sig === signature) return false;
    const saved = captureUi(container); clear(container); build(container);
    patched.set(container, { sig: signature }); restoreUi(container, saved);
    return true;
  }
  // Forgets a container's signature after code outside patchSection() emptied it.
  const resetSection = (container) => { if (container) patched.delete(container); };
  // The <option> descriptors for a select: [value, label] pairs.
  const optionItems = (pairs) => pairs.map(([value, label]) => ({ key: value, sig: sig(label), build: () => { const option = el('option', '', label); option.value = value; return option; } }));

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
    if (method === 'POST' && /\/projects\/[^/]+\/(archive|restore)$/.test(path)) return 'project_archive';
    if (method === 'POST' && /\/projects\/[^/]+\/tasks\/[^/]+\/archive$/.test(path)) return 'archive_completed_task';
    if (['POST', 'PUT', 'PATCH'].includes(method) && /\/projects\/[^/]+\/(workflow-policy|policy|workflow-activities\/[^/]+\/[^/]+|tasks\/[^/]+\/workflow\/reopen)$/.test(path)) return 'workflow_change';
    if (['POST','PATCH'].includes(method) && /\/projects\/[^/]+\/(knowledge|decisions|artifacts|imports)(\/|$)/.test(path)) return 'shared_change';
    if (['POST','PATCH','DELETE'].includes(method) && /\/projects\/[^/]+\/(objectives|claims|attempts|tasks)(\/|$)/.test(path)) return 'operator_work';
    if (method === 'POST' && /\/admin\/operators\/[^/]+\/access$/.test(path)) return 'operator_access';
    if (method === 'POST' && /\/browser-sessions\/[^/]+\/revoke$/.test(path)) return 'browser_revoke';
    if (method === 'POST' && /\/admin\/credentials\/[^/]+\/rotate$/.test(path)) return 'rotate_credential';
    if (method === 'POST' && path === '/api/v1/admin/operators') return 'create_operator';
    if (method === 'POST' && path === '/api/v1/auth/password') return 'change_password';
    if (method === 'POST' && (path === '/api/v1/auth/email' || path === '/api/v1/auth/email/resend')) return 'account_email';
    if (method === 'POST' && path === '/api/v1/admin/clock/reconcile') return 'clock_change';
    if (method === 'POST' && path.startsWith('/api/v1/admin/restore/')) return 'restore_change';
    if (method === 'POST' && /\/admin\/agents\/[^/]+\/credentials$/.test(path)) return 'rotate_credential';
    if (method === 'POST' && path === '/api/v1/resources') return 'create_resource';
    if (method === 'PATCH' && /^\/api\/v1\/resources\/[^/]+$/.test(path)) return 'update_resource';
    if (method === 'DELETE' && /^\/api\/v1\/resources\/[^/]+$/.test(path)) return 'delete_resource';
    if (method === 'POST' && /\/projects\/[^/]+\/reservations\/[^/]+\/resolve$/.test(path)) return 'resolve_resource';
    if (method === 'POST' && /\/projects\/[^/]+\/integrator\/reports\/[^/]+\/resolve$/.test(path)) return 'resolve_integrator_report';
    if (method === 'POST' && path === '/api/v1/projects') return 'create_project';
    if (method === 'POST' && /\/projects\/[^/]+\/tasks$/.test(path)) return 'create_task';
    if (method === 'POST' && path === '/api/v1/admin/agents') return 'issue_credential';
    if (method === 'POST' && /\/admin\/credentials\/[^/]+\/revoke$/.test(path)) return 'revoke_credential';
    if (method === 'POST' && /\/admin\/credentials\/[^/]+\/delete$/.test(path)) return 'delete_credential';
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

  // Starts one mutation and returns whether it started: false while another is pending or when it cannot be saved.
  function startMutation(path, body, label, onSuccess, method = 'POST', onError = null, context = {}) {
    if (state.mutation) return false;
    const operation = mutationOperation(path, method);
    state.mutation = { path, body, label, method, key: newKey(), operation, context, onSuccess, onError, inFlight: false };
    if (operation && actorId()) {
      try { persistMutation(state.mutation); }
      catch (_) { state.mutation = null; setGlobalAlert('This browser cannot save pending requests. Enable session storage before making changes.'); return false; }
    }
    executeMutation(state.mutation);
    return true;
  }

  function renderMutationState() {
    const busy = Boolean(state.mutation);
    document.querySelectorAll('form button[type="submit"], [data-mutation="true"]').forEach((button) => {
      const readOnlyAttachment = button.id === 'upload-task-attachments' && taskDetailReadOnly();
      setProp(button, 'disabled', readOnlyAttachment || (busy && !(state.mutation?.awaitingSecret && button.closest('form')?.dataset.resumeSecret === 'true')));
    });
    setProp($('account-button'), 'disabled', busy);
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
    state.mutation = null; state.actor = null; state.csrfToken = null; state.projects = []; state.projectsRendered = false; state.tasks = []; state.detail = null;
    ++state.sharedSeq; state.sharedCursor = null; state.sharedItems = []; state.sharedRules = undefined; state.sharedKey = ''; state.sharedLoaded = false; clear($('shared-list')); setText($('shared-freshness'), '');
    ++state.reportsSeq; state.reportsCursor = null; state.reports = []; state.reportsLoaded = false; state.reportsProjectId = ''; clear($('reports-list'));
    ++state.agentSessionsSeq; state.agentSessionsProjectId = ''; state.agentSessionsLoaded = false; clear($('agent-sessions-list'));
    ++state.digestSeq; state.digestProjectId = ''; state.digestLoaded = false; clear($('digest-content'));
    ++state.autonomySeq; state.autonomyProjectId = ''; state.autonomyLoaded = false; state.autonomySummaries.clear(); clear($('autonomy-content'));
    $('context-search').reset();
    state.credentials = []; state.credentialsLoaded = false; state.operators = []; state.restoreItems = []; state.archivedLoaded = false; state.archivedProjectId = ''; state.archivedTasks = []; state.archivedProjects = []; state.archivedProjectsLoaded = false; clear($('archived-projects-list')); state.detailKey = ''; clear($('archived-list')); state.resources = []; state.resourcesLoaded = false; state.resourceCursor = null; state.resourcePagesExtended = false; clear($('resources-list')); clear($('job-evidence-content')); clear($('workflow-content')); resetTaskPages(); state.projectId = ''; state.selectedTaskId = ''; state.currentView = 'overview';
    if (!preservePending) clearPersistedMutation();
    clearCredentialDownload();
    setText($('issued-token'), ''); show($('token-reveal'), false); setText($('issue-feedback'), ''); show($('issue-feedback'), false); clear($('credentials-list')); clear($('operators-list')); clear($('restore-status')); clear($('clock-status')); clear($('restore-requirements')); clear($('task-operator-actions')); show($('credentials-list'), false);
    stopPolling();
    show($('dashboard-view'), false); show($('loading-view'), false); show($('login-view'), true);
    $('login-form')?.reset(); $('username')?.focus();
  }

  async function restoreSession() {
    try { const result = await request('/api/v1/me'); applySession(result.data); restorePendingMutation(); await loadProjects(); applyRoute(location.hash); startPolling(); }
    catch (error) { show($('loading-view'), false); show($('dashboard-view'), false); show($('login-view'), true); if (error.status && error.status !== 401) showLoginError(errorMessage(error)); }
  }

  function showLoginError(message, kind = 'error') { const target = $('login-error'); target.classList.toggle('error', kind === 'error'); target.classList.toggle('success', kind === 'success'); setText(target, message); show(target, Boolean(message)); }

  function showView(view) {
    state.currentView = view;
    syncHistory();
    ['overview', 'archived-projects', 'project', 'reports', 'agent-sessions', 'digest', 'autonomy', 'tasks', 'archived', 'task-detail', 'resources', 'admin', 'shared'].forEach((name) => show($(`${name}-view`), name === view));
    show($('task-view-tabs'), view === 'tasks' || view === 'archived');
    updateTaskViewTabs();
    document.querySelectorAll('.nav-item').forEach((button) => button.classList.toggle('active', button.dataset.view === view || (['archived-projects', 'project', 'reports', 'agent-sessions', 'digest', 'autonomy'].includes(view) && button.dataset.view === 'overview') || (view === 'task-detail' && button.dataset.view === 'tasks')));
    if (view === 'tasks') { fillProjectSelect(); setProp($('new-task-button'), 'disabled', !state.projectId); setProp($('refresh-tasks'), 'disabled', !state.projectId); $('project-select')?.focus(); }
  }

  // Address history: every main view has a hash route, so the browser's back and forward buttons walk the
  // views visited. showView records the route; a hash change (back, forward, or an edited link) applies it.
  const PROJECT_PAGES = { project: 'settings', reports: 'reports', 'agent-sessions': 'sessions', digest: 'digest', autonomy: 'autonomy', archived: 'archived' };
  let routing = false;
  function currentRoute() {
    const view = state.currentView, project = state.projectId ? `#/projects/${encodeURIComponent(state.projectId)}` : '';
    if (view === 'tasks') return project ? `${project}/${state.taskView === 'completed' ? 'completed' : 'tasks'}` : '#/tasks';
    if (view === 'task-detail') return project && state.selectedTaskId ? `${project}/task/${encodeURIComponent(state.selectedTaskId)}` : '#/tasks';
    if (PROJECT_PAGES[view]) return project ? `${project}/${PROJECT_PAGES[view]}` : view === 'archived' ? '#/archived' : '';
    if (view === 'archived-projects') return '#/archived-projects';
    return ['shared', 'resources', 'admin'].includes(view) ? `#/${view}` : '';
  }
  function routeUrl(route) { return `${location.pathname}${location.search}${route}`; }
  function syncHistory() {
    if (routing || !state.actor) return;
    const route = currentRoute();
    if (location.hash !== route) history.pushState(null, '', routeUrl(route));
  }
  // The loads a top-level view needs when it is opened.
  function enterTopLevel(view) {
    showView(view);
    if (view === 'tasks' && state.projectId) loadTasks();
    if (view === 'admin') { loadCredentials(); loadOperators(); loadRestore(); loadClock(); }
    if (view === 'resources') loadResources();
    if (view === 'shared') { fillSharedProject(); loadShared(); }
  }
  // Shows the view a hash names. An unknown project or page falls back to the overview.
  function applyRoute(hash) {
    let parts;
    try { parts = text(hash).replace(/^#\/?/, '').split('/').filter(Boolean).map(decodeURIComponent); } catch { parts = []; }
    const [head, id, page, extra] = parts;
    routing = true;
    try {
      if (head === 'projects' && state.projects.some((project) => text(project.id) === id)) {
        if (state.projectId !== id) { resetTaskPages(); state.projectId = id; }
        state.selectedTaskId = ''; state.taskView = 'queue';
        if (page === 'settings') openProjectSettings(id);
        else if (page === 'reports') openReports();
        else if (page === 'sessions') openAgentSessions();
        else if (page === 'digest') openDigest();
        else if (page === 'autonomy') openAutonomy(id);
        else if (page === 'archived') selectTaskView('archived');
        else if (page === 'completed') selectTaskView('completed');
        else if (page === 'task' && extra) openTask(extra);
        else { updateTaskViewTabs(); showView('tasks'); loadTasks(); }
      } else if (head === 'account') { showView('overview'); myAccount(); }
      else if (head === 'archived') selectTaskView('archived');
      else if (head === 'archived-projects') openArchivedProjects();
      else if (head === 'tasks' || head === 'shared' || head === 'resources' || (head === 'admin' && state.actor?.role === 'admin')) enterTopLevel(head);
      else showView('overview');
    } finally { routing = false; }
    if (location.hash !== currentRoute()) history.replaceState(null, '', routeUrl(currentRoute()));
  }
  window.addEventListener('hashchange', () => { if (state.actor && location.hash !== currentRoute()) applyRoute(location.hash); });

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
    if (operation === 'account_email') return async () => { setGlobalAlert('Verification email requested. Open the newest message in your inbox; older links no longer work.', 'success'); };
    if (operation === 'change_password') return async () => { signOutLocal(); showLoginError('Password changed. Sign in with your new password.'); };
    if (operation === 'operator_work') return async () => { state.projectId = context.projectId || state.projectId; state.selectedTaskId = context.taskId || ''; if (state.selectedTaskId) { showView('task-detail'); await loadTaskDetail(); } else { showView('tasks'); await loadTasks(); } setGlobalAlert('Update recorded. Inspect the current task and ownership.', 'success'); };
    if (operation === 'operator_access') return async () => { await loadOperators(); setGlobalAlert('Account access updated.', 'success'); };
    if (operation === 'browser_revoke') return async () => { await restoreSession(); setGlobalAlert('Browser session revoked.', 'success'); };
    if (operation === 'rotate_credential') return async data => { showToken(data.token); await loadCredentials(); setIssueFeedback(data.token ? 'Replacement token issued for the same agent. Copy it now.' : 'This replacement token cannot be shown again. Rotate the replacement credential to obtain a new token.', data.token ? 'success' : 'error'); };
    if (operation === 'shared_change') return async (data) => { state.projectId = context.projectId || state.projectId; showView('shared'); if (context.preview) showImportPreview(data, state.projectId); else { await loadShared(); setGlobalAlert('Shared record saved.', 'success'); } };
    if (operation === 'workflow_change') return async () => { state.projectId = context.projectId || state.projectId; state.selectedTaskId = context.taskId || state.selectedTaskId; if (state.selectedTaskId) { showView('task-detail'); await loadTaskDetail(); } else await loadTasks(); setGlobalAlert('Workflow update recorded. Inspect the current candidate and next actions.', 'success'); };
    if (operation === 'create_resource') return async () => { await loadResources(); setGlobalAlert('Resource created.', 'success'); };
    if (operation === 'update_resource') return async () => { await loadResources(); setGlobalAlert('Resource updated.', 'success'); };
    if (operation === 'delete_resource') return async () => { await loadResources(); setGlobalAlert('Resource deleted.', 'success'); };
    if (operation === 'resolve_resource') return async () => { state.projectId = context.projectId || state.projectId; state.selectedTaskId = context.taskId || state.selectedTaskId; showView('task-detail'); await loadTaskDetail(); setGlobalAlert('Resolution recorded. Review the updated job and hold evidence.', 'success'); };
    if (operation === 'resolve_integrator_report') return async () => { state.projectId = context.projectId || state.projectId; openReports(); setGlobalAlert('Report resolution recorded. Review the current reports.', 'success'); };
    if (operation === 'create_project') return async () => { await loadProjects(); setGlobalAlert('Project created.', 'success'); };
    if (operation === 'project_archive') return async () => { await loadProjects(); await loadArchivedProjects(); setGlobalAlert('Project archive change recorded.', 'success'); };
    if (operation === 'create_task') return async () => { state.projectId = context.projectId || state.projectId; await loadTasks(); setGlobalAlert('Task created.', 'success'); };
    if (operation === 'archive_completed_task') return async () => { removeCompletedTask(context.projectId, context.taskId); };
    if (operation === 'issue_credential') return async (data) => { downloadIssuedCredential(data); await loadCredentials(); };
    if (operation === 'revoke_credential') return async () => { await loadCredentials(); setGlobalAlert('Credential revoked.', 'success'); };
    if (operation === 'delete_credential') return async () => { await loadCredentials(); setGlobalAlert('Credential deleted.', 'success'); };
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
    const visibleTasks = state.taskView === 'completed' ? state.tasks : state.tasks.filter((task) => taskStatus(task) !== 'done');
    const active = visibleTasks.filter((task) => ['claimed', 'in_progress', 'working', 'active'].includes(taskStatus(task))).length;
    const blocked = visibleTasks.filter((task) => taskStatus(task) === 'blocked').length;
    reconcile($('overview-summary'), [['Projects', state.projects.length], ['Tasks in view', visibleTasks.length], ['Active leases', active], ['Blocked', blocked]].map(([label, value]) => ({
      key: label, sig: sig(value), build: () => { const card = el('div', 'summary-card'); add(card, el('span', 'summary-number', value)); add(card, el('span', 'summary-label', label)); return card; }
    })));
  }

  async function loadProjects(silent = false) {
    if (state.fetching.has('projects')) return;
    state.fetching.add('projects');
    if (!silent && !state.projects.length) { show($('projects-state'), true); setState($('projects-state'), 'Loading projects…', true); show($('projects-grid'), false); }
    try {
      const projects = listData((await request('/api/v1/projects')).data);
      if (silent && refreshBlocked('projects-grid')) return;
      // Unchanged data leaves every node alone, so focus and open disclosures survive a poll.
      const changed = JSON.stringify(projects) !== JSON.stringify(state.projects) || !state.projectsRendered;
      state.projects = projects;
      if (changed) { state.projectsRendered = true; renderProjects(); fillProjectSelect(); }
      state.projects.forEach(loadAutonomySummary);
      renderSummary(); if (state.currentView === 'reports') setReportsProjectLabel(); if (state.currentView === 'agent-sessions') setAgentSessionsProjectLabel(); if (state.currentView === 'digest') setDigestProjectLabel(); if (state.currentView === 'autonomy') setAutonomyProjectLabel();
    }
    catch (error) { if (!silent) { setState($('projects-state'), errorMessage(error), false, true); } }
    finally { state.fetching.delete('projects'); }
  }

  // Shows a status panel; an identical panel is left untouched.
  function setState(target, message, loading = false, error = false) {
    setClass(target, `state-panel${error ? ' error' : ''}`); show(target, true);
    const same = target.childNodes.length === target.children.length && target.children.length === (loading ? 2 : 1)
      && target.lastElementChild.tagName === 'SPAN' && target.lastElementChild.textContent === text(message) && (!loading || target.firstElementChild.classList.contains('spinner'));
    if (same) return;
    clear(target); if (loading) add(target, el('span', 'spinner')); add(target, el('span', '', message));
  }

  function renderProjects() {
    const target = $('projects-grid');
    if (!state.projects.length) { reconcile(target, []); show(target, false); setState($('projects-state'), 'No projects yet. Create one to give agents a place to work.', false); return; }
    show($('projects-state'), false); show(target, true);
    reconcile(target, state.projects.map((project) => ({ key: project.id, sig: sig(project), build: () => projectCard(project) })));
  }

  function projectCard(project) {
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
    const summary = el('div', 'autonomy-summary project-meta');
    const autonomy = el('button', 'autonomy-open', 'Autonomy →'); autonomy.type = 'button';
    autonomy.setAttribute('aria-label', `Autonomy view for ${project.name || projectId}`);
    autonomy.addEventListener('click', () => openAutonomy(project.id));
    const summaryText = add(summary, el('span', 'autonomy-summary-text')); summaryText.dataset.projectId = projectId;
    summaryText.title = 'From the project\'s telemetry records and workflow records; open the Autonomy view for each figure\'s source.';
    add(footer, open); add(card, header); add(card, repo); add(card, idLabel); add(card, summary); add(summary, autonomy); add(card, footer);
    fillAutonomySummary(project, summaryText);
    return card;
  }

  // Keeps the three project selects in step with state.projects, touching only the options that changed.
  function fillProjectSelect() {
    const current = state.projectId, known = (id) => state.projects.some((project) => text(project.id) === id);
    const options = optionItems([['', 'Choose a project'], ...state.projects.map((project) => [text(project.id), project.name || project.id])]);
    const select = $('project-select');
    if (select) { reconcile(select, options); setProp(select, 'value', known(current) ? current : ''); }
    const archive = $('archive-project-select');
    if (archive) { const prior = archive.value; reconcile(archive, options); setProp(archive, 'value', known(prior) ? prior : known(current) ? current : ''); }
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
    const snippet = `service_url = ${JSON.stringify(location.origin)}\nproject_id = ${JSON.stringify(state.projectId)}`;
    patchSection($('project-binding-content'), sig(snippet), (binding) => {
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
    });
    show($('archive-project-button'), state.actor?.kind === 'human');
    showView('project'); $('project-heading').focus();
    loadIntegrationOwner(state.projectId);
  }

  // Archived projects: hidden from the project list, restorable by a human operator.
  function openArchivedProjects() {
    showView('archived-projects'); $('archived-projects-heading').focus(); loadArchivedProjects();
  }
  async function loadArchivedProjects() {
    if (state.fetching.has('archived-projects')) return;
    state.fetching.add('archived-projects');
    if (!state.archivedProjectsLoaded) setState($('archived-projects-state'), 'Loading archived projects…', true);
    try {
      const projects = [];
      let cursor = null;
      do { const params = new URLSearchParams({archived: 'true', limit: '200'}); if (cursor) params.set('cursor', cursor); const page = (await request(`/api/v1/projects?${params}`)).data; projects.push(...listData(page)); cursor = page?.next_cursor || null; } while (cursor);
      state.archivedProjects = projects; state.archivedProjectsLoaded = true;
      renderArchivedProjects();
    } catch (error) { if (!state.archivedProjectsLoaded) setState($('archived-projects-state'), errorMessage(error), false, true); else setGlobalAlert(errorMessage(error)); }
    finally { state.fetching.delete('archived-projects'); }
  }
  function renderArchivedProjects() {
    const target = $('archived-projects-list');
    if (!state.archivedProjects.length) { reconcile(target, []); show(target, false); setState($('archived-projects-state'), 'No archived projects.', false); return; }
    show($('archived-projects-state'), false); show(target, true);
    reconcile(target, state.archivedProjects.map((project) => ({ key: project.id, sig: sig(project, state.actor?.kind), build: () => archivedProjectCard(project) })));
  }
  function archivedProjectCard(project) {
    const card = el('article', 'project-card archived-project-card'), projectId = text(project.id);
    add(card, el('h3', '', project.name || 'Unnamed project'));
    add(card, el('p', 'repo', project.repository_url || 'Repository not configured'));
    add(card, el('p', 'project-id', `Project ID · ${projectId || 'Unavailable'}`));
    const footer = el('div', 'project-card-footer');
    add(footer, el('span', 'project-meta', `Archived · ${formatDate(project.archived_at)}`));
    if (state.actor?.kind === 'human') {
      const restore = el('button', 'button subtle', 'Restore'); restore.type = 'button'; restore.dataset.mutation = 'true';
      restore.setAttribute('aria-label', `Restore project ${project.name || projectId}`);
      restore.addEventListener('click', () => restoreProject(project));
      add(footer, restore);
    }
    add(card, footer);
    return card;
  }
  function openArchiveProject() {
    const project = state.projects.find((item) => text(item.id) === state.projectId);
    if (!project || state.actor?.kind !== 'human') { setGlobalAlert('Only a human operator can archive a project.'); return; }
    const view = workflowDialog('Archive project', `Archive ${project.name || project.id}? It leaves the project list and takes no new work until it is restored from Archived projects. Its tasks and records are kept.`);
    view.field('reason', 'Reason');
    view.finish('Archive project', (values, dialog) => {
      dialog.close();
      startMutation(`${projectPath(project.id)}/archive`, {reason: values.get('reason').trim()}, 'project archive', async () => {
        state.archivedProjectsLoaded = false; await loadProjects(); state.projectId = ''; showView('overview');
        setGlobalAlert(`${project.name || 'Project'} archived. Find it under Archived projects.`, 'success');
      }, 'POST', (error) => { if (!error.uncertain) setGlobalAlert(archiveProjectError(error)); }, {projectId: project.id});
    });
  }
  function restoreProject(project) {
    if (state.actor?.kind !== 'human') { setGlobalAlert('Only a human operator can restore a project.'); return; }
    startMutation(`${projectPath(project.id)}/restore`, {reason: 'Restored from the Archived projects view.'}, 'project restore', async () => {
      await loadProjects(); await loadArchivedProjects();
      setGlobalAlert(`${project.name || 'Project'} restored to the project list.`, 'success');
    }, 'POST', (error) => { if (!error.uncertain) setGlobalAlert(archiveProjectError(error)); }, {projectId: project.id});
  }
  function archiveProjectError(error) {
    if (error.code === 'project_active') return 'The project was not archived: it still has claimed tasks, reviews or integrations. Let them finish or release them, then try again.';
    return errorMessage(error);
  }
  $('back-to-projects').addEventListener('click', () => showView('overview'));
  $('archived-projects-button').addEventListener('click', openArchivedProjects);
  $('archived-projects-back').addEventListener('click', () => showView('overview'));
  $('refresh-archived-projects').addEventListener('click', () => loadArchivedProjects());
  $('archive-project-button').addEventListener('click', openArchiveProject);
  $('integration-owner-button').addEventListener('click', openIntegrationOwner);
  $('integrator-reports-button').addEventListener('click', openReports);
  $('back-to-project-settings').addEventListener('click', () => openProjectSettings(state.projectId));
  $('reports-filter').addEventListener('change', () => loadReports());
  $('refresh-reports').addEventListener('click', () => loadReports());
  $('load-more-reports').addEventListener('click', () => loadReports(true));
  $('digest-button').addEventListener('click', openDigest);
  $('digest-back').addEventListener('click', () => openProjectSettings(state.projectId));
  $('refresh-digest').addEventListener('click', () => loadDigest());
  $('autonomy-button').addEventListener('click', () => openAutonomy());
  $('autonomy-back').addEventListener('click', () => openProjectSettings(state.projectId));
  $('refresh-autonomy').addEventListener('click', () => loadAutonomy());
  $('autonomy-window').addEventListener('change', () => loadAutonomy());
  $('agent-sessions-button').addEventListener('click', openAgentSessions);
  $('agent-sessions-back').addEventListener('click', () => openProjectSettings(state.projectId));
  $('agent-sessions-window').addEventListener('change', () => loadAgentSessions());
  $('refresh-agent-sessions').addEventListener('click', () => loadAgentSessions());
  $('project-review-button').addEventListener('click', async () => {
    const projectId = state.projectId;
    try {
      const reply = await request(`${projectPath(projectId)}/orientation`);
      if (projectId === state.projectId && state.currentView === 'project') openReviewSettings(reply.data.project);
    } catch (error) { setGlobalAlert(errorMessage(error), 'error'); }
  });

  function taskMatchesSearch(task, query) {
    const needle = query.trim().toLowerCase();
    return !needle || text(task.title).toLowerCase().includes(needle) || text(task.id).toLowerCase().includes(needle);
  }
  const searchedCompletedTasks = () => state.completedTasks.filter((task) => taskMatchesSearch(task, state.taskSearch.completed));
  const searchedArchivedTasks = () => state.archivedTasks.filter((task) => taskMatchesSearch(task, $('archived-search').value));

  function resetTaskPages() {
    state.taskSearch = { queue: '', completed: '' }; $('tasks-search').value = '';
    state.taskPages = []; state.taskPageIndex = 0; state.tasks = []; state.queueTasks = []; state.taskQueueLoaded = false;
    state.completedTasks = []; state.completedLoaded = false; state.completedPageIndex = 0;
  }

  function showTaskPage(index, updateSummary = true) {
    if (state.taskView === 'completed') {
      const completedTasks = searchedCompletedTasks();
      const pageCount = Math.ceil(completedTasks.length / state.taskPageSize);
      state.completedPageIndex = Math.max(0, Math.min(index, pageCount - 1));
      const start = state.completedPageIndex * state.taskPageSize;
      state.tasks = completedTasks.slice(start, start + state.taskPageSize);
      renderTasks(); if (updateSummary) renderSummary(); return;
    }
    state.taskPageIndex = index; const page = state.taskPages[index];
    state.tasks = page?.items || [];
    renderTasks(); if (updateSummary) renderSummary();
  }

  function rebuildQueuePages() {
    const filter = $('status-filter').value;
    const tasks = state.queueTasks.filter((task) => (filter === 'all' || taskStatus(task) === filter) && taskMatchesSearch(task, state.taskSearch.queue));
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
      if (silent && refreshBlocked('tasks-list')) return;
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
      if (requestedProjectId !== state.projectId || (silent && refreshBlocked('tasks-list'))) return;
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
      setAttr(tab, 'aria-selected', String(active)); setProp(tab, 'tabIndex', active ? 0 : -1);
    });
    setAttr($('tasks-view'), 'aria-labelledby', selected === 'completed' ? 'completed-tasks-tab' : 'task-queue-tab');
    const completed = state.taskView === 'completed';
    show($('status-filter-label'), !completed);
    setProp($('tasks-search'), 'value', state.taskSearch[completed ? 'completed' : 'queue']);
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
    const count = state.taskView === 'completed' ? Math.ceil(searchedCompletedTasks().length / state.taskPageSize) : state.taskPages.length;
    showTaskPage(count - 1);
  }

  // Page-number buttons for a paginated list; `current` is disabled and marked, `select(index)` opens a page.
  function pageButtons(target, count, current, select) {
    reconcile(target, Array.from({ length: count }, (_, index) => ({ key: index, sig: sig(index === current), build: () => {
      const page = actionButton(String(index + 1), () => select(index), false); page.classList.add('pagination-page');
      page.setAttribute('aria-current', index === current ? 'page' : 'false'); page.disabled = index === current; return page;
    } })));
  }

  function renderTaskPagination() {
    const completed = state.taskView === 'completed';
    const count = completed ? Math.ceil(searchedCompletedTasks().length / state.taskPageSize) : state.taskPages.length;
    const current = completed ? state.completedPageIndex : state.taskPageIndex;
    setProp($('tasks-page-size'), 'value', String(state.taskPageSize));
    show($('task-pagination'), Boolean(count)); setProp($('tasks-first-page'), 'disabled', !current); setProp($('tasks-previous-page'), 'disabled', !current);
    const atEnd = completed ? state.inflightCompleted || current + 1 >= count : Boolean(state.inflight.tasks) || current + 1 >= count;
    setProp($('tasks-next-page'), 'disabled', atEnd); setProp($('tasks-last-page'), 'disabled', atEnd);
    pageButtons($('tasks-page-numbers'), count, current, (index) => showTaskPage(index));
  }

  function renderTasks() {
    const target = $('tasks-list'); const filter = $('status-filter').value;
    const tasks = state.tasks;
    const pageCount = state.taskView === 'completed' ? Math.ceil(searchedCompletedTasks().length / state.taskPageSize) : state.taskPages.length;
    const pageIndex = state.taskView === 'completed' ? state.completedPageIndex : state.taskPageIndex;
    const taskTotal = state.taskView === 'completed' ? searchedCompletedTasks().length : state.queueTasks.filter((task) => (filter === 'all' || taskStatus(task) === filter) && taskMatchesSearch(task, state.taskSearch.queue)).length;
    renderTaskPagination(); setText($('tasks-page-status'), pageCount ? `Page ${pageIndex + 1} of ${taskTotal} tasks` : '');
    if (!tasks.length) {
      reconcile(target, []); show(target, false);
      const searching = Boolean(state.taskSearch[state.taskView === 'completed' ? 'completed' : 'queue'].trim());
      const empty = state.taskView === 'completed' ? (state.inflightCompleted ? 'Loading completed tasks…' : state.completedTasks.length && searching ? 'No completed tasks match this search.' : 'No completed tasks in this project yet.') : state.queueTasks.length ? (searching ? 'No tasks match this search or status filter.' : 'No tasks match this status filter.') : 'No tasks in this project yet. Create the first task.';
      setState($('tasks-state'), empty, state.taskView === 'completed' && state.inflightCompleted); return;
    }
    show($('tasks-state'), false); show(target, true);
    const completed = state.taskView === 'completed';
    reconcile(target, tasks.map((task) => ({ key: task.id, sig: sig(completed, task), build: () => taskRow(task, completed) })));
  }

  function taskRow(task, completed) {
    const status = taskStatus(task);
    const stage = statusIndicator(task).stage, row = el('article', `task-row${completed ? ' completed-task-row' : ''} ${status}${stage ? ` stage-${stage}` : ''}`);
    row.setAttribute('role', completed ? 'group' : 'button'); row.setAttribute('tabindex', '0');
    row.setAttribute('aria-label', `Open task ${task.title || task.id}. Status: ${statusIndicator(task).label}`);
    const open = () => openTask(task.id);
    row.addEventListener('click', (event) => {
      const selection = window.getSelection();
      if (selection && !selection.isCollapsed && (row.contains(selection.anchorNode) || row.contains(selection.focusNode))) return;
      open();
    });
    row.addEventListener('keydown', (event) => {
      if (event.target.closest('button')) return;
      if (event.key === 'Enter' || event.key === ' ') { event.preventDefault(); open(); }
    });
    if (completed) {
      const archive = taskIconButton('Archive completed task and move it to Archived Tasks', 'archive', (event) => {
        event.stopPropagation(); archiveCompletedTask(task);
      });
      archive.addEventListener('keydown', event => event.stopPropagation());
      add(row, archive);
    }
    const title = el('span', 'task-main'); add(title, el('span', 'task-title', task.title || 'Untitled task'));
    const identity = el('span', 'task-queue-id'); add(identity, el('span', '', 'Task ID · ')); add(identity, el('span', 'task-id-value', task.id));
    const created = el('time', 'task-created project-meta', `Created ${formatDate(task.created_at)}`);
    if (task.created_at && !Number.isNaN(new Date(task.created_at).getTime())) created.dateTime = task.created_at;
    const meta = el('span', 'task-meta');
    const badge = paintStatusBadge(el('span', 'status-badge'), task); add(meta, badge);
    const description = el('span', 'task-description', task.description || 'No description');
    add(row, title); add(row, identity); add(row, created); add(row, meta); add(row, description);
    return row;
  }

  function removeCompletedTask(projectId, taskId) {
    if (projectId !== state.projectId) return;
    state.completedTasks = state.completedTasks.filter(task => text(task.id) !== text(taskId));
    state.completedLoaded = true;
    state.completedPageIndex = Math.min(state.completedPageIndex, Math.max(0, Math.ceil(state.completedTasks.length / state.taskPageSize) - 1));
    if (state.taskView === 'completed') showTaskPage(state.completedPageIndex);
    setGlobalAlert('Task archived and moved to Archived Tasks.', 'success');
  }

  function archiveCompletedTask(task) {
    if (state.mutation || !task?.id || !state.projectId) return;
    const projectId = state.projectId;
    const taskId = text(task.id);
    startMutation(`${projectPath(projectId)}/tasks/${encodeURIComponent(taskId)}/archive`, {
      expected_revision: task.revision,
      reason: 'Archived from the Completed Tasks view.'
    }, 'archive completed task', () => removeCompletedTask(projectId, taskId), 'POST', null, {projectId, taskId});
  }

  async function loadTaskDetail(silent = false) {
    const requestedProjectId = state.projectId; const requestedTaskId = state.selectedTaskId; const requestKey = `${requestedProjectId}/${requestedTaskId}`;
    if (!requestedProjectId || !requestedTaskId || state.inflight.detail?.key === requestKey) return;
    const requestId = ++state.requestSeq.detail; state.inflight.detail = { key: requestKey, requestId };
    // Only a task that is not already on screen shows the loading panel; a reload of the shown task patches in place.
    if (!silent && state.detailKey !== requestKey) { show($('task-detail-content'), false); setState($('task-detail-state'), 'Loading task…', true); }
    try {
      const data = (await request(`/api/v1/projects/${encodeURIComponent(requestedProjectId)}/tasks/${encodeURIComponent(requestedTaskId)}`)).data;
      if (requestedProjectId !== state.projectId || requestedTaskId !== state.selectedTaskId || state.requestSeq.detail !== requestId) return;
      if (silent && refreshBlocked('task-detail-content')) return;
      state.detail = data; state.detailKey = requestKey; renderTaskDetail(silent);
    }
    catch (error) { if (requestedProjectId === state.projectId && requestedTaskId === state.selectedTaskId && state.requestSeq.detail === requestId && !silent) setState($('task-detail-state'), errorMessage(error), false, true); }
    finally { if (state.inflight.detail?.requestId === requestId) state.inflight.detail = null; }
  }

  function openTask(id, origin = 'tasks') { state.selectedTaskId = text(id); state.detailOrigin = origin; setTaskDetailCopyFeedback('', ''); showView('task-detail'); loadTaskDetail(); }
  async function loadArchivedTasks() {
    const projectId = $('archive-project-select').value || state.projectId;
    if (!projectId) { show($('archived-pagination'), false); setState($('archived-state'), 'Choose a project to load archived tasks.'); reconcile($('archived-list'), []); show($('archived-list'), false); return; }
    setProp($('archive-project-select'), 'value', projectId);
    if (state.archivedProjectId !== projectId) { state.archivedProjectId = projectId; state.archivedPageIndex = 0; state.archivedTasks = []; state.archivedLoaded = false; $('archived-search').value = ''; reconcile($('archived-list'), []); }
    // Cached rows stay on screen while the refresh runs; only a first load shows the loading panel.
    if (!state.archivedLoaded) { show($('archived-pagination'), false); setState($('archived-state'), 'Loading archived tasks…'); show($('archived-list'), false); }
    try {
      const tasks = []; let cursor = null;
      do { const params=new URLSearchParams({limit:'200'}); if(cursor) params.set('cursor',cursor); const page=(await request(`/api/v1/projects/${encodeURIComponent(projectId)}/tasks/archived?${params}`)).data; tasks.push(...listData(page)); cursor=page?.next_cursor||null; } while(cursor && projectId === ($('archive-project-select').value || state.projectId));
      if (projectId !== ($('archive-project-select').value || state.projectId)) return;
      const changed = !state.archivedLoaded || JSON.stringify(tasks) !== JSON.stringify(state.archivedTasks);
      state.archivedTasks = tasks; state.archivedLoaded = true;
      if (changed) renderArchivedTasks();
    } catch(error) { if (!state.archivedLoaded) { setState($('archived-state'),errorMessage(error), false, true); } else setGlobalAlert(errorMessage(error)); }
  }

  function renderArchivedTasks() {
    const target = $('archived-list');
    const archivedTasks = searchedArchivedTasks();
    const pageCount = Math.ceil(archivedTasks.length / state.taskPageSize);
    state.archivedPageIndex = Math.min(state.archivedPageIndex, Math.max(0, pageCount - 1));
    const start = state.archivedPageIndex * state.taskPageSize;
    const pageTasks = archivedTasks.slice(start, start + state.taskPageSize);
    renderArchivedPagination();
    setText($('archived-page-status'), pageCount ? `Page ${state.archivedPageIndex + 1} of ${archivedTasks.length} tasks` : '');
    if (!pageTasks.length) { setState($('archived-state'), state.archivedTasks.length ? 'No archived tasks match this search.' : 'No archived tasks in this project.'); reconcile(target, []); show(target, false); return; }
    reconcile(target, pageTasks.map(task => ({ key: task.id, sig: sig(task), build: () => {
      const row = el('article', `task-row archived-task-row ${task.lifecycle}`);
      row.setAttribute('role', 'button'); row.setAttribute('tabindex', '0'); row.setAttribute('aria-label', `Open archived task ${task.title || task.id}`);
      const open = () => { state.projectId = state.archivedProjectId; openTask(task.id, 'archived'); };
      row.addEventListener('click', open);
      row.addEventListener('keydown', event => { if (event.key === 'Enter' || event.key === ' ') { event.preventDefault(); open(); } });
      const title = el('span', 'task-title', task.title || 'Untitled task');
      const meta = el('span', 'task-meta', `Archived · ${displayStatus(task.lifecycle)}`);
      const description = el('span', 'task-description', task.description || 'No description');
      add(row, title); add(row, meta); add(row, description);
      return row;
    } })));
    show($('archived-state'), false); show(target, true);
  }

  function renderArchivedPagination() {
    const count = Math.ceil(searchedArchivedTasks().length / state.taskPageSize), current = state.archivedPageIndex;
    show($('archived-pagination'), Boolean(count));
    setProp($('archived-first-page'), 'disabled', !current); setProp($('archived-previous-page'), 'disabled', !current);
    setProp($('archived-next-page'), 'disabled', current + 1 >= count); setProp($('archived-last-page'), 'disabled', current + 1 >= count);
    setProp($('archived-page-size'), 'value', String(state.taskPageSize));
    pageButtons($('archived-page-numbers'), count, current, (index) => { state.archivedPageIndex = index; renderArchivedTasks(); });
  }

  function selectTaskDetailText(id) {
    const source = $(id); const selection = window.getSelection();
    if (!source || !selection) return;
    const range = document.createRange(); range.selectNodeContents(source); selection.removeAllRanges(); selection.addRange(range);
  }
  function setTaskDetailCopyFeedback(message, kind) {
    const target = $('detail-copy-feedback'); setClass(target, `task-detail-copy-feedback ${kind || ''}`); setText(target, message); show(target, Boolean(message));
  }
  async function copyTaskDetailValue(source, label) {
    const value = source.textContent; const sourceId = source.id;
    if (!value) return;
    try { await navigator.clipboard.writeText(value); setTaskDetailCopyFeedback(`${label} copied to clipboard.`, 'success'); }
    catch (_) { selectTaskDetailText(sourceId); setTaskDetailCopyFeedback(`Clipboard access was unavailable. ${label} is selected; copy it with your browser.`, 'fallback'); }
  }

  // Task fields that the operator action buttons read; a change to any other field leaves them alone.
  const TASK_ACTION_FIELDS = ['id', 'title', 'description', 'acceptance_criteria', 'priority', 'kind', 'lifecycle', 'work_status', 'status', 'archived_at', 'activity_kind', 'current_attempt_id', 'human_delivered', 'revision', 'blocked_reason', 'depends_on', 'dependencies', 'objective_id', 'parent_objective_id'];
  const pick = (source, fields) => Object.fromEntries(fields.filter((field) => source && field in source).map((field) => [field, source[field]]));

  // Patches each section of the task page from `state.detail`. Sections own their signatures, so the
  // title, brief and buttons stay put while, say, only the checkpoint trail grows.
  function renderTaskDetail(quiet = false) {
    const data = state.detail || {}; const task = data.task || data; const project = state.projects.find((item) => text(item.id) === state.projectId);
    const title = task.title || 'Untitled task'; const taskId = text(task.id || state.selectedTaskId);
    setText($('detail-project-label'), project?.name || 'Project'); setText($('task-detail-heading'), title); setText($('detail-task-id-value'), taskId); setText($('detail-task-revision'), task.revision || 1); setText($('detail-description'), task.description || 'No description provided.');
    const status = taskStatus(task); paintStatusBadge($('detail-status'), task); setText($('detail-kind'), displayStatus(task.activity_kind || task.kind || 'general'));
    const humanDelivered = $('detail-human-delivered'); show(humanDelivered, Boolean(task.human_delivered)); setAttr(humanDelivered, 'title', 'A human delivers this task. Supervisors never receive or claim it.');
    const readOnly = Boolean(task.archived_at || ['done', 'completed'].includes(status));
    setProp($('task-attachment-files'), 'disabled', readOnly || Boolean(state.mutation || state.attachmentBusy));
    setProp($('upload-task-attachments'), 'disabled', readOnly || Boolean(state.mutation || state.attachmentBusy));
    setProp($('retry-task-attachments'), 'disabled', readOnly || Boolean(state.mutation || state.attachmentBusy));
    $('task-attachments-card').classList.toggle('task-attachments-readonly', readOnly);
    const items = Array.isArray(task.acceptance_criteria) ? task.acceptance_criteria : [];
    reconcile($('acceptance-list'), items.length ? items.map((item, index) => ({ key: index, sig: sig(item), build: () => el('li', '', item) })) : [{ key: 'none', sig: '', build: () => el('li', 'muted', 'No acceptance criteria recorded.') }]);
    renderTaskActions(data, task); renderLease(data, task); renderLifecycle(data); renderCheckpoints(data); renderJobEvidence(data); renderWorkflow(data);
    show($('task-detail-state'), false); show($('task-detail-content'), true);
    loadTaskAttachments(state.projectId, taskId, false, quiet);
  }

  function renderLease(data, task) {
    const attempts = Array.isArray(data.attempts) ? data.attempts : []; const current = attempts.find((attempt) => attempt.id === task.current_attempt_id);
    let view;
    if (!current) {
      const status = taskStatus(task);
      view = { message: task.current_attempt_id ? 'Ownership details are still syncing. Refresh to reconcile this task.' : ['done', 'canceled', 'superseded'].includes(status) ? `No active lease. This task is ${displayStatus(status).toLowerCase()}.` : status === 'blocked' ? 'No active lease. This task is blocked and needs attention.' : status === 'planned' ? 'No active lease. This task is planned and is not yet admitted.' : statusIndicator(task).stage === 'in_review' ? 'A reviewer holds a live review lease. The review activities below show who claimed it.' : statusIndicator(task).stage === 'awaiting_reviewer' ? 'No lease. The review is queued and waiting for a reviewer to claim it.' : ['waiting_review', 'waiting_integration', 'integrating', 'validating'].includes(status) ? 'The submitted candidate is moving through its review and integration activities below.' : 'No active lease. This task is available for eligible work.' };
    } else {
      const remaining = taskStatus(task) === 'recovery_required' ? 'Ownership ended; recovery required' : current.lease_remaining_ms !== undefined ? `${Math.max(0, Math.round(Number(current.lease_remaining_ms) / 60000))} min remaining` : current.expires_at ? formatLease(current.expires_at) : 'Lease active';
      view = { rows: [['Owner', current.owner_name || current.owner_id || 'Agent'], ['State', displayStatus(taskStatus(task))], ['Lease', remaining], ['Expires', formatDate(current.expires_at)], ['Generation', current.generation || 1]] };
    }
    // The signature is the rendered text, so a lease that ticks within the same displayed minute changes nothing.
    patchSection($('lease-content'), sig(view), (target) => {
      if (view.message) { add(target, el('p', 'muted', view.message)); return; }
      const dl = el('dl'); view.rows.forEach(([label, value]) => { const line = el('div', 'lease-line'); add(line, el('dt', '', label)); add(line, el('dd', '', value)); add(dl, line); }); add(target, dl);
    });
  }

  // Says why the task was canceled, archived, restored or deleted, by whom and when, and links
  // a canceled task to its replacement and a replacement back to the tasks it replaced. Events
  // recorded before reasons were kept show no reason rather than a guessed one.
  function renderLifecycle(data) {
    const events = Array.isArray(data.lifecycle_events) ? data.lifecycle_events : []; const replaces = Array.isArray(data.replaces) ? data.replaces : [];
    show($('lifecycle-card'), Boolean(events.length || replaces.length));
    const verbs = {canceled: 'Canceled', activity_canceled: 'Integration canceled', archived: 'Archived', restored: 'Restored', deleted: 'Deleted'};
    const taskLink = (id, label) => { const link = el('button', 'button text-button', label || id); link.type = 'button'; link.addEventListener('click', () => openTask(id)); return link; };
    reconcile($('lifecycle-content'), [
      ...events.map((event, index) => ({ key: `event:${event.id ?? `${index}:${event.action}:${event.created_at}`}`, sig: sig(event), build: () => {
        const item = el('div', 'lifecycle-event');
        add(item, el('p', '', `${verbs[event.action] || displayStatus(event.action)} by ${event.actor_name || event.actor_id || 'unknown'} on ${formatDate(event.created_at)}${event.reason_code ? ` (${displayStatus(event.reason_code)})` : ''}${event.reason ? `: ${event.reason}` : ' (no reason was recorded)'}`));
        if (event.replacement_task_id) { const line = el('p', 'lifecycle-replacement', 'Replaced by '); add(line, taskLink(event.replacement_task_id, `task ${event.replacement_task_id}`)); add(item, line); }
        return item;
      } })),
      ...replaces.map((old) => ({ key: `replaces:${old.task_id}`, sig: sig(old), build: () => {
        const item = el('div', 'lifecycle-event'); const line = el('p', '', 'Replaces '); add(line, taskLink(old.task_id, old.title || old.task_id));
        add(line, document.createTextNode(` (canceled by ${old.actor_name || old.actor_id || 'unknown'} on ${formatDate(old.canceled_at)}${old.reason ? `: ${old.reason}` : ''})`)); add(item, line);
        return item;
      } }))
    ]);
  }

  function formatLease(expires) { const remaining = new Date(expires).getTime() - Date.now(); return `${Math.max(0, Math.round(remaining / 60000))} min remaining`; }

  function attachmentStatus(message, error = false) {
    const target = $('task-attachments-state'); setText(target, message); setClass(target, error ? 'inline-alert error' : 'muted');
  }

  function renderTaskAttachments(records, pending, hasMore = false) {
    const list = $('task-attachments-list');
    const items = records.map((record) => ({ key: `record:${record.id}`, sig: sig(record), build: () => {
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
      return entry;
    } }));
    if (!records.length) items.push({ key: 'none', sig: '', build: () => el('p', 'muted', 'No files are attached to this task yet.') });
    if (hasMore) items.push({ key: 'more', sig: '', build: () => el('p', 'muted', 'More records are available through Browse history.') });
    reconcile(list, items);
    const retry = $('retry-task-attachments');
    show(retry, pending.length > 0);
    const task = state.detail?.task || state.detail || {};
    setProp(retry, 'disabled', Boolean(state.attachmentBusy || state.mutation || task.archived_at || ['done', 'completed'].includes(taskStatus(task))));
    setText(retry, pending.length === 1 ? 'Retry pending upload' : `Retry ${pending.length} pending uploads`);
  }

  // Loads the task's attachment records. A quiet reload (a poll of the task page) refetches without
  // announcing it and keeps what is shown if the fetch fails.
  async function loadTaskAttachments(projectId, taskId, force = false, quiet = false) {
    const key = `${projectId}/${taskId}`, requestedActor = actorId();
    const loaded = state.attachmentTaskKey === key && state.attachmentsLoaded;
    if (!force && !quiet && loaded) return;
    if (quiet && !loaded) return;
    state.attachmentTaskKey = key; if (!quiet) state.attachmentsLoaded = false;
    const requestId = ++state.attachmentRequestSeq;
    if (!quiet) attachmentStatus('Loading task attachments…');
    try {
      const [history, batches] = await Promise.all([
        request(`${projectPath(projectId)}/tasks/${encodeURIComponent(taskId)}/history?kind=artifacts&limit=50`),
        readAttachmentBatches()
      ]);
      if (requestId !== state.attachmentRequestSeq || projectId !== state.projectId || taskId !== state.selectedTaskId || requestedActor !== actorId()) return;
      if (quiet && refreshBlocked('task-detail-content')) return;
      const records = (history.data.items || []).map((item) => item.record).filter(Boolean);
      const pending = batches.filter((batch) => batch.project_id === projectId && batch.task_id === taskId && batch.actor_id === requestedActor);
      renderTaskAttachments(records, pending, Boolean(history.data.next_cursor));
      if (pending.length) attachmentStatus('A saved upload is waiting to finish. Retry uses the saved bytes and request keys.', true);
      else attachmentStatus(records.length ? `${records.length} attachment${records.length === 1 ? '' : 's'} available in task history.` : 'No files are attached to this task yet.');
      state.attachmentsLoaded = true;
    } catch (error) {
      if (quiet || requestId !== state.attachmentRequestSeq || projectId !== state.projectId || taskId !== state.selectedTaskId || requestedActor !== actorId()) return;
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
      state.attachmentBusy = false; $('upload-task-attachments').disabled = Boolean(state.mutation || taskDetailReadOnly()); $('task-attachment-files').disabled = Boolean(state.mutation || taskDetailReadOnly());
      $('retry-task-attachments').disabled = Boolean(state.mutation || taskDetailReadOnly());
    }
  }

  async function uploadTaskAttachments() {
    if (state.attachmentBusy || state.mutation || !state.selectedTaskId || taskDetailReadOnly()) return;
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
    if (state.attachmentBusy || state.mutation || taskDetailReadOnly()) return;
    try {
      const batches = (await readAttachmentBatches()).filter((batch) => batch.project_id === state.projectId && batch.task_id === state.selectedTaskId && batch.actor_id === actorId());
      if (!batches.length) { attachmentStatus('No pending uploads need a retry.'); return; }
      await runTaskAttachmentBatches(batches);
    } catch (error) { attachmentStatus(errorMessage(error), true); }
  }

  function taskDetailReadOnly() {
    const task = state.detail?.task || state.detail || {};
    return Boolean(task.archived_at || ['done', 'completed'].includes(taskStatus(task)));
  }

  function renderCheckpoints(data) {
    const target = $('checkpoints-content'); let checkpoints = Array.isArray(data.checkpoints) ? data.checkpoints : [];
    if (!checkpoints.length && Array.isArray(data.attempts)) data.attempts.forEach((attempt) => { if (Array.isArray(attempt.checkpoints)) checkpoints = checkpoints.concat(attempt.checkpoints); });
    checkpoints = checkpoints.slice().sort((a, b) => new Date(b.created_at || b.at).getTime() - new Date(a.created_at || a.at).getTime());
    if (!checkpoints.length) { reconcile(target, [{ key: 'none', sig: '', build: () => el('p', 'muted', 'No checkpoints have been shared yet.') }]); return; }
    reconcile(target, checkpoints.map((point, index) => ({ key: point.id ?? `${index}:${point.attempt_id}:${point.created_at || point.at}`, sig: sig(point), build: () => {
      const item = el('div', 'checkpoint'); const top = el('div', 'checkpoint-top'); add(top, el('span', '', point.actor_name || point.owner_id || 'Agent')); add(top, el('time', '', formatDate(point.created_at || point.at))); add(item, top); add(item, el('p', 'checkpoint-summary', point.summary || 'Progress update'));
      if (point.current_action) add(item, el('p', 'checkpoint-detail', `Current action · ${point.current_action}`)); if (point.next_step) add(item, el('p', 'checkpoint-detail', `Next step · ${point.next_step}`));
      if (Array.isArray(point.blockers) && point.blockers.length) add(item, el('p', 'checkpoint-detail', `Blockers · ${point.blockers.join(', ')}`));
      return item;
    } })));
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
    if (state.actor?.role !== 'admin' || state.fetching.has('credentials')) return; state.fetching.add('credentials'); if (!silent && !state.credentialsLoaded) { show($('credentials-list'), false); setState($('credentials-state'), 'Loading credentials…', true); }
    try {
      const credentials = listData((await request('/api/v1/admin/credentials')).data);
      if (silent && refreshBlocked('credentials-list')) return;
      if (state.credentialsLoaded && JSON.stringify(credentials) === JSON.stringify(state.credentials)) return;
      state.credentials = credentials; state.credentialsLoaded = true; renderCredentials();
    }
    catch (error) { if (!silent) setState($('credentials-state'), errorMessage(error), false, true); } finally { state.fetching.delete('credentials'); }
  }

  function renderCredentials() {
    const target = $('credentials-list');
    if (!state.credentials.length) { reconcile(target, []); show(target, false); setState($('credentials-state'), 'No agent credentials have been issued.', false); return; }
    show($('credentials-state'), false); show(target, true);
    const offeredReplacement = new Set();
    reconcile(target, state.credentials.map(credential => {
      // Only the first revoked credential of an agent offers a replacement, so a row's content depends on the rows before it.
      const offer = Boolean(credential.revoked_at && credential.principal_id && !offeredReplacement.has(credential.principal_id));
      if (offer) offeredReplacement.add(credential.principal_id);
      return { key: credential.id, sig: sig(credential, offer), build: () => {
        const row = el('div', 'credential-row'), info = el('div');
        add(info, el('div', 'credential-name', `${credential.principal_name || credential.name || credential.id} · ${credential.credential_name || 'initial'}${credential.class && credential.class !== 'interactive' ? ` · ${credential.class}` : ''}${credential.access === 'read' ? ' · read-only' : ''}`));
        add(info, el('div', 'credential-meta', `Issued ${shortDate(credential.created_at)}`));
        if (credential.revoked_at) add(info, el('div', 'credential-revoked', `Revoked ${shortDate(credential.revoked_at)}`));
        add(row, info);
        if (!credential.revoked_at) {
          const actions = el('div', 'credential-actions');
          add(actions, taskIconButton('Rotate credential', 'rotate', () => rotateCredential(credential)));
          const revoke = actionButton('Revoke', () => revokeCredential(credential.id)); revoke.className = 'button danger'; add(actions, revoke);
          add(row, actions);
        } else {
          if (offer) add(row, actionButton('New token for this agent', () => replaceAgentCredential(credential)));
          add(row, taskIconButton('Delete revoked credential', 'delete', () => confirmDeleteCredential(credential)));
        }
        return row;
      } };
    }));
    renderMutationState();
  }
  function confirmDeleteCredential(credential) {
    const view = workflowDialog('Delete credential', `Delete the revoked credential “${credential.credential_name || 'initial'}” of ${credential.principal_name || credential.name || credential.id}? It will no longer appear in the Issued credentials list. This cannot be undone.`);
    view.finish('Delete', (values, dialog) => { dialog.close(); deleteCredential(credential.id); });
  }
  function deleteCredential(id) { startMutation(`/api/v1/admin/credentials/${encodeURIComponent(id)}/delete`, {}, 'credential deletion', async () => { await loadCredentials(); setGlobalAlert('Credential deleted.', 'success'); }); }
  function revokeCredential(id) { startMutation(`/api/v1/admin/credentials/${encodeURIComponent(id)}/revoke`, {}, 'credential revocation', async () => { await loadCredentials(); setGlobalAlert('Credential revoked.', 'success'); }); }

  // Polling and background refreshes (including the auto-refresh checkbox) share these rules: nothing
  // refreshes while the tab is hidden, a mutation or upload is pending, or a dialog is open, and a region
  // is left alone while a text field, menu or file chooser inside it has focus. Fields outside the region
  // (the search box above a list, say) are never touched by its refresh, so they do not block it.
  const POLL_MS = 5000;
  function refreshBlocked(regionId = null) {
    if (document.hidden || state.mutation || state.attachmentBusy || document.querySelector('dialog[open]')) return true;
    const focused = document.activeElement, region = regionId && $(regionId);
    return Boolean(region && focused && region.contains(focused) && focused.matches('textarea, select, input:not([type=checkbox]):not([type=radio]):not([type=button])'));
  }
  function pollTick() {
    if (!state.actor || refreshBlocked()) return;
    if (state.currentView === 'overview') loadProjects(true);
    else if (state.currentView === 'tasks' && state.taskAutoRefresh) loadTasks(true);
    else if (state.currentView === 'task-detail') loadTaskDetail(true);
    else if (state.currentView === 'admin') loadCredentials(true);
    else if (state.currentView === 'resources' && !state.resourcePagesExtended) loadResources(true);
  }
  function stopPolling() { if (state.pollTimer) clearInterval(state.pollTimer); state.pollTimer = null; }
  function startPolling() { stopPolling(); if (!document.hidden) state.pollTimer = setInterval(pollTick, POLL_MS); }
  // A hidden tab stops polling; becoming visible refreshes once at once, then resumes the interval.
  document.addEventListener('visibilitychange', () => {
    if (document.hidden) { stopPolling(); return; }
    if (state.actor) { startPolling(); pollTick(); }
  });

  function showForgot(open) { show($('login-form'), !open); show($('forgot-link').parentElement, !open); show($('forgot-form'), open); showLoginError(''); if (open) { $('forgot-email').value = ''; $('forgot-email').focus(); } else $('username').focus(); }
  $('forgot-link').addEventListener('click', () => showForgot(true));
  $('forgot-back').addEventListener('click', () => showForgot(false));
  $('forgot-form').addEventListener('submit', async (event) => {
    event.preventDefault(); const email = $('forgot-email').value.trim(); const button = event.submitter;
    if (!email) { showLoginError('Enter your email address.'); return; }
    showLoginError(''); if (button) button.disabled = true;
    try { await request('/api/v1/auth/password/forgot', { method: 'POST', body: { email } }); showForgot(false); showLoginError('If that address is verified on an account, a reset link is on its way. It works once and expires in 1 hour.', 'success'); }
    catch (error) { showLoginError(errorMessage(error)); }
    finally { if (button) button.disabled = false; }
  });
  $('login-form').addEventListener('submit', (event) => { event.preventDefault(); const username = $('username').value.trim(); const password = $('password').value; if (!username || !password) { showLoginError('Enter your username and password.'); return; } showLoginError(''); startMutation('/api/v1/auth/login', { username, password }, 'sign-in', async (data) => { applySession(data); $('password').value = ''; restorePendingMutation(); await loadProjects(); applyRoute(location.hash); startPolling(); }, 'POST', (error) => showLoginError(errorMessage(error))); });
  document.querySelectorAll('button.nav-item').forEach((button) => button.addEventListener('click', () => { enterTopLevel(button.dataset.view); }));
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
  $('archived-first-page').addEventListener('click', () => { state.archivedPageIndex = 0; renderArchivedTasks(); });
  $('archived-previous-page').addEventListener('click', () => { state.archivedPageIndex = Math.max(0, state.archivedPageIndex - 1); renderArchivedTasks(); });
  $('archived-next-page').addEventListener('click', () => { state.archivedPageIndex = Math.min(Math.ceil(searchedArchivedTasks().length / state.taskPageSize) - 1, state.archivedPageIndex + 1); renderArchivedTasks(); });
  $('archived-last-page').addEventListener('click', () => { state.archivedPageIndex = Math.max(0, Math.ceil(searchedArchivedTasks().length / state.taskPageSize) - 1); renderArchivedTasks(); });
  $('archived-page-size').addEventListener('change', (event) => { state.taskPageSize = Number(event.target.value); $('tasks-page-size').value = String(state.taskPageSize); state.archivedPageIndex = 0; renderArchivedTasks(); });
  $('tasks-page-size').addEventListener('change', (event) => { $('archived-page-size').value = event.target.value; if (state.archivedTasks.length) renderArchivedTasks(); });
  $('project-select').addEventListener('change', (event) => { state.projectId = event.target.value; resetTaskPages(); state.taskView = 'queue'; updateTaskViewTabs(); $('new-task-button').disabled = !state.projectId; $('refresh-tasks').disabled = !state.projectId; syncHistory(); loadTasks(); });
  $('tasks-search').addEventListener('input', (event) => {
    if (state.taskView === 'completed') { state.taskSearch.completed = event.target.value; showTaskPage(0); }
    else { state.taskSearch.queue = event.target.value; rebuildQueuePages(); showTaskPage(0); }
  });
  $('archived-search').addEventListener('input', () => { state.archivedPageIndex = 0; renderArchivedTasks(); });
  $('status-filter').addEventListener('change', () => { state.taskPageIndex = 0; rebuildQueuePages(); showTaskPage(0); });
  $('back-to-tasks').addEventListener('click', () => { if (state.detailOrigin === 'archived') { showView('archived'); loadArchivedTasks(); } else showView('tasks'); }); [['task-detail-heading', 'Task name'], ['detail-task-id-value', 'Task ID']].forEach(([id, label]) => { const source = $(id); source.addEventListener('click', () => copyTaskDetailValue(source, label)); source.addEventListener('keydown', (event) => { if (event.key === 'Enter' || event.key === ' ') { event.preventDefault(); copyTaskDetailValue(source, label); } }); }); $('refresh-credentials').addEventListener('click', () => loadCredentials()); $('issue-form').addEventListener('submit', (event) => { event.preventDefault(); const input = $('agent-name'); if (!input.value.trim()) return; startMutation(`/api/v1/admin/agents`, { name: input.value.trim(), class: $('agent-class').value, access: $('agent-access').value }, 'credential issuance', async (data) => { input.value = ''; downloadIssuedCredential(data); await loadCredentials(); }); });
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
    if (!silent && !state.resourcesLoaded) setState($('resources-state'), 'Loading resources…', true);
    try {
      const query = append && state.resourceCursor ? `?cursor=${encodeURIComponent(state.resourceCursor)}` : '';
      const page = (await request(`/api/v1/resources${query}`)).data;
      if (!requestedActor || requestedActor !== actorId()) return;
      if (silent && refreshBlocked('resources-list')) return;
      const items = listData(page);
      state.resourcePagesExtended = append;
      state.resources = append ? [...new Map([...state.resources, ...items].map(item => [item.id, item])).values()] : items;
      state.resourceCursor = page.next_cursor || null; state.resourcesLoaded = true;
      reconcile($('resources-list'), state.resources.map(resource => ({ key: resource.id, sig: sig(resource), build: () => {
        const row = el('tr');
        add(add(row, el('td')), el('code', '', resource.key));
        add(row, el('td', '', resource.description || 'No description'));
        add(row, el('td', '', text(resource.capacity)));
        add(row, el('td', '', text(resource.held_units ?? 0)));
        add(add(row, el('td')), el('code', '', resource.id));
        const actions = add(row, el('td', 'row-actions'));
        add(actions, taskIconButton(`Edit resource ${resource.key}`, 'edit', () => openResourceEditor(resource)));
        add(actions, taskIconButton(`Delete resource ${resource.key}`, 'delete', () => openResourceDeletion(resource)));
        return row;
      } })));
      if (state.resources.length) show($('resources-state'), false);
      else setState($('resources-state'), 'No resources yet. Define a stable key before reserving shared capacity.');
      show($('load-more-resources'), Boolean(state.resourceCursor));
    } catch (error) { if (requestedActor === actorId() && !(silent && state.resourcesLoaded)) setState($('resources-state'), errorMessage(error), false, true); }
    finally { state.fetching.delete('resources'); }
  }

  function openResourceEditor(resource) {
    const view = workflowDialog('Edit resource', 'Changing the key renames the resource for every project. Capacity cannot drop below the units currently held.');
    const key = view.field('key', 'Resource key', resource.key, 'input'); key.maxLength = 255;
    const capacity = view.field('capacity', 'Capacity units', resource.capacity, 'input'); capacity.type = 'number'; capacity.min = '1'; capacity.max = '1000';
    const description = view.field('description', 'Description', resource.description || '', 'input'); description.required = false; description.maxLength = 2000;
    view.finish('Save resource', (values, dialog) => {
      dialog.close();
      startMutation(`/api/v1/resources/${encodeURIComponent(resource.id)}`, {key: values.get('key').trim(), capacity: Number(values.get('capacity')), description: values.get('description').trim()}, 'resource update', restoredMutationCallbacks('update_resource'), 'PATCH');
    });
  }
  function openResourceDeletion(resource) {
    const view = workflowDialog('Delete resource', `Delete ${resource.key}? This cannot be undone. A resource that any reservation has used cannot be deleted.`);
    view.finish('Delete resource', (_values, dialog) => {
      dialog.close();
      startMutation(`/api/v1/resources/${encodeURIComponent(resource.id)}`, undefined, 'resource deletion', restoredMutationCallbacks('delete_resource'), 'DELETE');
    });
  }

  function renderJobEvidence(data) {
    const target = $('job-evidence-content');
    const evidence = data.job_evidence || {};
    const jobs = Array.isArray(evidence.jobs) ? evidence.jobs : [];
    const reservations = Array.isArray(evidence.reservations) ? evidence.reservations : [];
    const items = [];
    if (evidence.jobs_truncated || evidence.reservations_truncated) items.push({ key: 'truncated', sig: '', build: () => el('p', 'muted', 'Showing up to 50 jobs and 50 reservations, with unresolved evidence first. Use the CLI job and reservation lists to inspect the complete history.') });
    if (!jobs.length && !reservations.length) items.push({ key: 'none', sig: '', build: () => el('p', 'muted', 'No jobs or resource reservations recorded.') });
    jobs.forEach(job => items.push({ key: `job:${job.id}`, sig: sig(job), build: () => {
      const entry = el('div', 'evidence-entry'); add(entry, el('h3', '', job.label || 'Local job'));
      add(entry, el('p', '', `Producer: ${displayStatus(job.state)} · Observation: ${displayStatus(job.observation_freshness || 'unknown')}`));
      add(entry, el('p', 'muted', `Last observation: ${formatDate(job.last_observed_at)} · Exit: ${job.exit_code ?? 'not reported'}`));
      if (job.inputs_unchanged === false) add(entry, el('p', 'error', 'Source changed during this job; its result does not verify the current checkout.'));
      if (job.reconciled_at) { add(entry, el('p', 'muted', `Operator reconciliation: ${job.reconciliation_reason || 'Recorded'}`)); add(entry, el('p', '', job.reconciliation_evidence || '')); }
      add(entry, el('code', '', `Job ${job.id} · Producer ${job.producer_id}`));
      return entry;
    } }));
    const human = state.actor?.kind === 'human';
    reservations.forEach(reservation => items.push({ key: `reservation:${reservation.id}`, sig: sig(reservation, human), build: () => {
      const entry = el('div', 'evidence-entry'); add(entry, el('h3', '', `Resource reservation · ${displayStatus(reservation.state)}`));
      add(entry, el('code', '', reservation.id));
      if (reservation.resolution_evidence) add(entry, el('p', '', `Resolution evidence: ${reservation.resolution_evidence}`));
      (reservation.items || []).forEach(item => add(entry, el('p', 'muted', `${item.key || item.resource_id}: ${item.units} units`)));
      if (!['released','resolved'].includes(reservation.state) && human) {
        const button = el('button', 'button subtle', 'Record resource resolution'); button.type = 'button'; button.dataset.mutation = 'true';
        button.addEventListener('click', () => openResourceResolution(reservation)); add(entry, button);
      }
      return entry;
    } }));
    reconcile(target, items);
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
    allow_relayed_human_answers: 'When enabled, an agent that asked you a human-required decision in its own terminal may record your reply through the CLI. The answer is stored as relayed, with the exact prompt, your reply, the agent session and this policy revision, and is flagged here so you can review or reopen it. Leave this off if only this page should decide.',
    recovery_mode: 'Choose who may inspect and recover expired work. Expiry does not prove a workstation job stopped. The selected agent or human must inspect saved work and any remaining jobs or resource holds before continuing.',
    agent_rule_editing: 'Allow agents to change the binding rules text for this project. Humans still control review, integration, recovery and lease settings. Leave this off when rule changes need an operator.',
    lease_seconds: 'How long task ownership lasts without renewal, from 30 to 3600 seconds. Example only: 600 means ten minutes. Agents must renew before expiry; reading a task or saving a checkpoint does not renew ownership.',
    rules: 'Instructions every agent working on this project must follow, such as required validation and repository conventions. Saving a change creates a new policy revision that agents must read and acknowledge.',
    provenance: 'Explain why the policy is changing and cite the request, decision or other supporting source so later workers can understand it. Use your actual reason and source.',
    integration_owner: 'Agents: a connected agent claims each approved candidate’s integration and publishes it. Integrator service: the integrator computes, checks and pushes approved results itself, and agents cannot claim integration. Only a human may change this. Switching back to agents is the rollback.',
    report_decision: 'Allow lets the integrator push this exact result, including its privileged changes. Deny keeps the result off the target branch. Inspect the changed paths in the report details first.',
    resolution_note: 'Record what you checked or did about this finding, such as a rerun, a fix, or why it is safe. The note is kept with the report for later workers.'
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
  function workflowMutation(path, body, label, after, onError = null) {
    const projectId = state.projectId, taskId = state.selectedTaskId;
    return startMutation(path, body, label, async data => {
      if (projectId === state.projectId && taskId === state.selectedTaskId && taskId) await loadTaskDetail();
      else if (projectId === state.projectId) await loadTasks();
      if (after) after(data);
    }, 'POST', onError, { projectId, taskId });
  }
  function renderWorkflow(data) {
    const target = $('workflow-content'), items = []; const workflow = data.workflow || {}; const submission = workflow.submission;
    if (!submission) { reconcile(target, [{ key: 'none', sig: '', build: () => el('p', 'muted', 'No current submission. The implementation owner submits the result and acceptance evidence through the CLI.') }]); return; }
    // The buttons below read the actor, the phase, the blockers and the project's integration setting.
    const context = sig(state.actor?.kind, actorId(), state.actor?.session_id, workflow.phase, (workflow.blockers || []).length, state.projects.find(project => text(project.id) === state.projectId)?.automatic_integration, submission);
    const guidance = {
      review: 'Work has been submitted for review. The submission is a saved version of the result and its evidence; edits require a new submission. ' + ((workflow.activities || []).some(activity => activity.kind === 'either_review') ? 'An independent agent or a human may take this review. One approval satisfies the review requirement; requesting changes requires a new submission. An agent reserves it by claiming; a human reserves it only when recording the decision. Reserving does not approve the work.' : (workflow.activities || []).some(activity => activity.kind === 'human_review') ? 'Review the evidence below, then start an available human review to approve it or request changes. The review is reserved for you only when you record your decision; closing the form leaves it available.' : 'An independent agent must claim the required review and approve the work or request changes.'),
      integration: 'The submitted code is awaiting integration: an agent must validate the required checks, publish the result, and finalize the task. If shown, Authorize integration gives permission to proceed.',
      revision_needed: 'Changes are needed. An agent must claim the task, revise the work, and submit a new version for review. The previous submission stays in the history.',
      done: 'This submission has completed the required workflow.'
    };
    if (guidance[workflow.phase]) items.push({ key: 'guidance', sig: sig(workflow.phase, (workflow.activities || []).map(activity => activity.kind)), build: () => el('p', 'inline-alert', guidance[workflow.phase]) });
    const buildCandidate = () => {
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
    return candidate;
    };
    items.push({ key: 'candidate', sig: context, build: buildCandidate });
    (workflow.blockers || []).forEach((item, index) => items.push({ key: `blocker:${index}`, sig: sig(item), build: () => el('p', 'inline-alert error', typeof item === 'string' ? item : item.message || item.code) }));
    if (workflow.integration_blocked) {
      const blocked = workflow.integration_blocked;
      items.push({ key: 'integration-blocked', sig: sig(blocked), build: () => {
        const box = el('div', 'inline-alert error');
        add(box, el('strong', '', 'Integration blocked'));
        add(box, el('p', '', [`Report ${displayStatus(blocked.kind)}`, blocked.verdict && `verdict ${blocked.verdict}`, blocked.check_name && `check ${blocked.check_name}`, blocked.target_revision && `target revision ${blocked.target_revision}`].filter(Boolean).join(' · ')));
        add(box, el('p', 'muted', `Integrator report ${blocked.report_id}`));
        return box;
      } });
    }
    if (workflow.activities_truncated) items.push({ key: 'truncated', sig: '', build: () => el('p', 'muted', 'Showing the latest 100 workflow activities. Older records remain stored.') });
    (workflow.activities || []).forEach(activity => items.push({ key: `activity:${activity.id}`, sig: sig(activity, context), build: () => {
      const entry = el('div', 'workflow-entry'); add(entry, el('h3', '', displayStatus(activity.kind)));
      add(entry, el('p', 'muted', `${displayStatus(activity.status)} · ${activity.id}`));
      if (activity.activity_task_id) { const open = el('button', 'button text-button', 'Inspect activity task and jobs'); open.type = 'button'; open.addEventListener('click', () => openTask(activity.activity_task_id)); add(entry, open); }
      const attempt = activity.current_attempt || activity.attempt;
      if (attempt) add(entry, el('p', 'muted', `Owner ${attempt.owner_id} · lease expires ${formatDate(attempt.expires_at)}`));
      if (activity.review) { add(entry, el('p', '', `${displayStatus(activity.review.decision)}: ${activity.review.summary}`)); (activity.review.findings || []).forEach(finding => add(entry, el('p', '', `${displayStatus(finding.severity)}: ${finding.remedy}`))); }
      if (activity.intent) add(entry, el('p', 'muted', `Publication intent: ${activity.intent.observed_target_revision} → ${activity.intent.result_revision}`));
      if (activity.result) add(entry, el('p', '', `Publication: ${displayStatus(activity.result.publication_state)}`));
      if (activity.hold) add(entry, el('p', '', `Target hold: ${displayStatus(activity.hold.state)} · ${activity.hold.canonical_repository_key} · ${activity.hold.target_branch}`));
      if (activity.cancel_reason) {
        const cancel = activity.cancel_reason; const context = cancel.context || {};
        add(entry, el('p', 'inline-alert error', `Canceled (${displayStatus(cancel.code)})${cancel.detail ? `: ${cancel.detail}` : ''}`));
        const revisions = [['Submission base', context.submission_base_revision], ['Target', context.target_revision], ['Candidate', context.candidate_revision]].filter(([, value]) => value).map(([label, value]) => `${label} ${value}`);
        if (revisions.length) add(entry, el('p', 'muted', revisions.join(' · ')));
      }
      if (activity.publication_reconciliation) add(entry, el('p', '', `Reconciled ${displayStatus(activity.publication_reconciliation.disposition)}: ${activity.publication_reconciliation.evidence}`));
      (activity.result?.check_job_ids || []).forEach(id => add(entry, el('p', 'muted', `Check producer: ${id}`)));
      if (activity.authorization) add(entry, el('p', '', `${activity.authorization.valid === false ? 'Invalidated authorization' : 'Human authorization'}: ${activity.authorization.summary}`));
      const action = (label, callback, help) => { const button = el('button', 'button subtle', label); button.type = 'button'; button.dataset.mutation = 'true'; button.addEventListener('click', callback); add(entry, button); if (help) contextualHelp(entry, button, label, help); };
      if (state.actor?.kind === 'human' && !['done','completed','canceled'].includes(activity.status)) {
        if (['human_review', 'either_review'].includes(activity.kind) && !(workflow.blockers || []).length) {
          const current = attempt?.state === 'active' && attempt.valid_by_time === true && attempt.owner_authorized === true;
          if (current && attempt.owner_id === actorId() && attempt.session_id === state.actor.session_id) action('Record human review', () => openHumanReview(activity, submission, attempt));
          else if (!current) action('Start human review', () => openHumanReview(activity, submission, null), 'Open the review form. The review is reserved for your signed-in session only when you record your decision, so closing the form leaves it available to others. Read the submission and acceptance evidence first. Code still needs integration and required checks after approval.');
        }
        if (activity.kind === 'integration') {
          if ((!activity.authorization || activity.authorization.valid === false) && !(workflow.blockers || []).length && workflow.phase === 'integration' && state.projects.find(project => text(project.id) === state.projectId)?.automatic_integration === false) action('Authorize integration', () => openIntegrationAuthorization(activity, submission));
          if (activity.intent && !activity.publication_reconciliation) action('Reconcile publication', () => openPublicationReconciliation(activity, submission));
        }
      }
      return entry;
    } }));
    (workflow.next_actions || []).forEach((action, index) => items.push({ key: `next:${index}`, sig: sig(action), build: () => el('p', 'muted', typeof action === 'string' ? action : action.message) }));
    reconcile(target, items);
    renderMutationState();
  }
  // Opens the review form. With no held `attempt`, the review is claimed only when the
  // decision is recorded, so dismissing the form never leaves the review leased.
  // The service's byte limit for a review summary (crates/server/src/workflow.rs).
  const SUMMARY_BYTES = 16384;
  // The service's byte limit for a finding's evidence, which the form fills from the summary.
  const EVIDENCE_BYTES = 8192;
  // The longest prefix of `value` that fits in `limit` UTF-8 bytes, never splitting a character.
  function truncateBytes(value, limit) {
    const encoder = new TextEncoder(); if (encoder.encode(value).length <= limit) return value;
    let kept = '', used = 0;
    for (const character of value) { const size = encoder.encode(character).length; if (used + size > limit) break; kept += character; used += size; }
    return kept;
  }
  function openHumanReview(activity, submission, attempt) {
    const view = workflowDialog('Review this candidate', `This decision applies only to submission ${submission.id}, source ${submission.candidate_revision || 'general task evidence'}. Inspect its acceptance evidence and checks before deciding.`);
    // An empty first option is the required select's placeholder: no decision is preselected, so the form refuses to record until the reviewer picks one.
    const decision = view.field('decision', 'Decision', '', 'select'); for (const [value,label] of [['','Choose a decision…'],['approved','Approved'],['changes_requested','Changes requested']]) { const option = el('option', '', label); option.value = value; add(decision, option); }
    decision.value = '';
    view.help(decision, 'Decision', 'Nothing is selected until you choose. Choose Approved when the submitted evidence satisfies the acceptance criteria. Choose Changes requested when more work is needed; a revised submission will need fresh review. Approval completes a general task after all required reviews; code must also pass integration.');
    const summary = view.field('summary', 'Review summary'); summary.maxLength = SUMMARY_BYTES;
    // The service bounds the summary in UTF-8 bytes, which a character maxLength cannot express.
    summary.addEventListener('input', () => summary.setCustomValidity(new TextEncoder().encode(summary.value).length > SUMMARY_BYTES ? `The review summary is limited to ${SUMMARY_BYTES} bytes.` : ''));
    view.help(summary, 'Review summary', 'Describe what you inspected and why you approve or request changes. Refer to acceptance criteria, results, or evidence links. For example: Checked the reported test results; the keyboard interaction still needs verification.');
    const findings = view.field('findings', 'Required remedies — one per line'); findings.required = false;
    view.help(findings, 'Required remedies', 'For changes requested, describe each required fix on its own line so the next agent knows what to change. Leave this empty when approving.');
    decision.addEventListener('change', () => findings.setCustomValidity(''));
    view.finish('Record review', (values, dialog) => {
      const remedies = text(values.get('findings')).split('\n').map(v => v.trim()).filter(Boolean);
      if (values.get('decision') === 'approved' && remedies.length) { findings.setCustomValidity('Required remedies must be resolved before approval. Choose changes requested.'); findings.reportValidity(); findings.addEventListener('input', () => findings.setCustomValidity(''), {once:true}); return; }
      // One claim-then-review chain at a time: the global mutation lock clears between the two requests.
      if (view.form.dataset.recording === 'true') return;
      view.form.dataset.recording = 'true'; clearDialogError(view);
      const review = {submission_id: submission.id, decision: values.get('decision'), summary: values.get('summary'), findings: remedies.map(remedy => ({severity:'required',remedy,evidence:truncateBytes(text(values.get('summary')), EVIDENCE_BYTES)}))};
      const done = () => { delete view.form.dataset.recording; dialog.close(); };
      const failed = error => { delete view.form.dataset.recording; showDialogError(view, error); };
      // A request that could not start runs neither callback, so it is reported like a failure.
      const notStarted = () => new ApiError('The request could not start: another request may still be running, or this browser could not save it. Try again.', 0, 'mutation_not_started', null, false);
      // A kept attempt the service no longer honours (lease expired, released or superseded) is forgotten, so the next submit claims afresh instead of retrying the dead attempt forever.
      const reused = error => { if (!error?.uncertain && ['lease_expired', 'ownership_lost'].includes(error?.code)) attempt = null; failed(error); };
      if (attempt) { if (!recordHumanReview(activity, attempt, review, done, reused)) failed(notStarted()); }
      else if (!claimHumanReview(activity, submission, held => {
        // A claim this form could not give back is kept and reused by the next retry instead of claiming again.
        const keep = () => { attempt = held; };
        // A decision that failed after this form's own claim gives the claim back, unless the outcome is uncertain or the session ended.
        const settle = error => { if (!error?.uncertain && error?.status !== 401 && error?.code !== 'authentication_required') { if (!releaseHumanReview(activity, held)) keep(); } else keep(); failed(error); };
        if (!recordHumanReview(activity, held, review, done, settle)) settle(notStarted());
      }, failed)) failed(notStarted());
    });
  }
  // Removes a previous failure note before a retry.
  function clearDialogError(view) { view.form.querySelector('.dialog-error')?.remove(); }
  // Shows `error` inside the still-open form, so the entered decision can be retried.
  function showDialogError(view, error) {
    let note = view.form.querySelector('.dialog-error');
    if (!note) { note = el('p', 'inline-alert error dialog-error'); note.setAttribute('role', 'alert'); view.form.prepend(note); }
    setText(note, `Not recorded: ${errorMessage(error)} Your entries are kept; correct them or try again.`);
  }
  // Claims the review for this session, then hands the new attempt to `after`; returns whether the claim started.
  function claimHumanReview(activity, submission, after, onError) {
    return workflowMutation(`${projectPath()}/workflow-activities/${encodeURIComponent(activity.id)}/claim`, {expected_submission_id: submission.id, expected_project_policy_revision: submission.project_policy_revision, expected_workflow_policy_revision: submission.workflow_policy_revision}, 'review claim', response => {
      if (response?.attempt) after(response.attempt);
      else onError(new ApiError('Refresh the activity to inspect its current ownership.', 0, 'missing_attempt', null, false));
    }, onError);
  }
  // Records the decision under the held review `attempt`, then calls `done`; returns whether the request started.
  function recordHumanReview(activity, attempt, review, done, onError) {
    return workflowMutation(`${projectPath()}/workflow-activities/${encodeURIComponent(activity.id)}/review`, {generation: attempt.generation, ...review}, 'human review', done, onError);
  }
  // Gives back a review claim this form took when its decision could not be recorded; returns whether the release started.
  function releaseHumanReview(activity, attempt) {
    return workflowMutation(`${projectPath()}/workflow-activities/${encodeURIComponent(activity.id)}/release`, {generation: attempt.generation, summary: 'Released: the human review decision could not be recorded.'}, 'review release');
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
    const select = $('shared-project');
    reconcile(select, optionItems([['', 'Choose a project'], ...state.projects.map(project => [text(project.id), project.name])]));
    setProp(select, 'value', state.projects.some(project => text(project.id) === state.projectId) ? state.projectId : '');
  }
  function sharedWrite(path, body, label, method = 'POST', after = null) {
    const projectId = state.projectId;
    startMutation(path, body, label, async data => {
      if (projectId !== state.projectId) return;
      if (after) after(data); else await loadShared();
    }, method, null, {projectId});
  }
  // Loads one page of the chosen record kind. A reload keeps the cards on screen and patches only the
  // records that changed; a different project, kind or search starts from an empty list.
  async function loadShared(append = false) {
    const projectId = state.projectId, currentActor = actorId(), kind = $('shared-kind').value;
    const seq = ++state.sharedSeq, list = $('shared-list');
    show($('context-search'), kind === 'context');
    const shown = sig(projectId, kind, kind === 'context' ? [$('context-query').value, $('context-shared').checked] : null);
    if (!append && state.sharedKey !== shown) { state.sharedKey = shown; state.sharedItems = []; state.sharedRules = undefined; state.sharedLoaded = false; reconcile(list, []); }
    if (!append) state.sharedCursor = null;
    if (!projectId) { setState($('shared-state'), 'Choose a project to inspect its records.'); show($('load-more-shared'), false); return; }
    if (kind === 'context' && !$('context-query').value.trim()) { setState($('shared-state'), 'Enter search text to retrieve relevant context.'); show($('load-more-shared'), false); return; }
    if (!state.sharedLoaded) { show($('load-more-shared'), false); setState($('shared-state'), 'Loading records…', true); }
    const query = new URLSearchParams({limit:'50'});
    if (append && state.sharedCursor) query.set('cursor', state.sharedCursor);
    if (kind === 'context') { query.set('q', $('context-query').value); query.set('include_shared', String($('context-shared').checked)); query.set('budget', '65536'); }
    try {
      const page = (await request(`${projectPath(projectId)}/${kind}?${query}`)).data;
      if (seq !== state.sharedSeq || projectId !== state.projectId || currentActor !== actorId()) return;
      const items = listData(page);
      state.sharedItems = append ? [...state.sharedItems, ...items] : items; state.sharedLoaded = true;
      if (kind === 'context') state.sharedRules = page.policy?.rules;
      reconcile(list, [
        ...(kind === 'context' && state.sharedRules !== undefined ? [{ key: 'rules', sig: sig(state.sharedRules), build: () => { const rules = el('details', 'card'); add(rules, el('summary', '', 'Current binding project rules')); add(rules, el('pre', '', state.sharedRules)); return rules; } }] : []),
        ...state.sharedItems.map((item, index) => ({ key: `${kind}:${(item.record || item).id ?? index}`, sig: sig(item, kind, projectId), build: () => renderSharedRecord(item, kind) }))
      ]);
      state.sharedCursor = page.next_cursor || null;
      show($('load-more-shared'), Boolean(state.sharedCursor));
      if (state.sharedItems.length || append) show($('shared-state'), false);
      else setState($('shared-state'), page.instructions_complete === false ? 'The context budget cannot include all binding rules. Increase the budget before relying on this packet.' : 'No matching records.');
      setText($('shared-freshness'), `Read ${new Date().toLocaleTimeString([], { hour: 'numeric', minute: '2-digit' })}. Refresh to inspect changes.${page.truncated ? ' Results were bounded; narrow the search for more context.' : ''}`);
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
    if (record.answer) {
      add(card, el('p', 'muted', record.answer.relayed
        ? `Relayed answer: an agent (session ${record.answer.actor_session_id}) recorded what a human told it, under policy revision ${record.answer.relay?.policy_revision} enabled by principal ${record.answer.relay?.authorized_by}. Reopen the decision if this is not what you decided.`
        : `Direct answer by ${record.answer.actor_id}.`));
      recordDetails(card, 'Recorded answer', record.answer);
    }
    recordDetails(card, 'Source and applicability', {id:record.id, provenance:record.provenance, scope:record.scope, source:record.source, conditions:record.conditions, affected_tasks:record.affected_tasks, expires_at:record.expires_at});
    const actions = el('div', 'record-actions');
    const button = (label, icon, run, mutation = false) => add(actions, taskIconButton(label, icon, run, mutation));
    const iconLink = (label, icon, href) => { const link = iconize(el('a', 'button subtle'), label, icon); link.href = href; add(actions, link); return link; };
    const ownProject = !record.project_id || record.project_id === state.projectId;
    if (['knowledge','decisions','artifacts'].includes(kind)) button('Inspect history and details of this record', 'history', async () => {
      const projectId = state.projectId, currentActor = actorId();
      try { const data = (await request(`${projectPath(projectId)}/${kind}/${encodeURIComponent(record.id)}`)).data; if (projectId === state.projectId && currentActor === actorId()) { recordDetails(card, 'Full record and history', data); card.lastElementChild.open = true; } }
      catch (error) { setGlobalAlert(errorMessage(error)); }
    });
    if (kind === 'knowledge' && ownProject) {
      button('Correct this lesson', 'edit', () => lessonDialog(record), true);
      button('Mark this lesson as helpful', 'helpful', () => sharedWrite(`${projectPath()}/knowledge/${encodeURIComponent(record.id)}/feedback`, {expected_revision:record.revision,useful:true,comment:'Marked helpful by an operator.'}, 'lesson feedback'), true);
    }
    if (kind === 'decisions' && ownProject && record.required_actor !== 'agent' && record.status === 'pending') button('Record an answer to this decision', 'resolve', () => decisionAnswer(record), true);
    if (kind === 'decisions' && ownProject && record.status !== 'pending') button('Reopen this decision with the current scope', 'reopen', () => reopenDecision(record), true);
    if (kind === 'artifacts') {
      if (record.availability === 'available' && record.kind === 'upload') iconLink('Download this attachment', 'download', `${projectPath()}/artifacts/${encodeURIComponent(record.id)}/content`).download = '';
      if (record.external_url) { try { const url = new URL(record.external_url); if (url.protocol === 'https:' && !url.username && !url.password) { const link = iconLink('Open the external evidence in a new tab', 'external', url.href); link.rel = 'noopener noreferrer'; link.target = '_blank'; } } catch (_) { /* Show unusable links as metadata only. */ } }
      add(card, el('p', 'muted', `${record.size_bytes ?? 'Unknown'} bytes · ${record.sha256 || 'No content digest'}${record.kind === 'external_link' ? ' · External availability is not verified by the service.' : ''}`));
    }
    add(card, actions);
    return card;
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
  const ICON_PATHS = {
    archive: '<rect x="4" y="5" width="16" height="4" rx="1"/><path d="M6 9v10h12V9m-8 4h4"/>',
    cancel: '<circle cx="12" cy="12" r="9"/><path d="m9 9 6 6m0-6-6 6"/>',
    edit: '<path d="M12 20h9"/><path d="M16.5 3.5a2.1 2.1 0 0 1 3 3L9 17l-4 1 1-4Z"/>',
    delete: '<path d="M4 7h16M10 11v6m4-6v6M6 7l1 13h10l1-13M9 7V4h6v3"/>',
    resolve: '<path d="m5 12 4 4L19 6"/><circle cx="12" cy="12" r="9"/>',
    access: '<path d="M12 3 5 6v5c0 4.5 3 8 7 10 4-2 7-5.5 7-10V6Z"/><path d="m9 12 2 2 4-4"/>',
    sessions: '<rect x="3" y="4" width="18" height="12" rx="2"/><path d="M8 20h8m-4-4v4"/>',
    rotate: '<path d="M20 11a8 8 0 0 0-14.9-3M4 4v4h4"/><path d="M4 13a8 8 0 0 0 14.9 3M20 20v-4h-4"/>',
    history: '<circle cx="12" cy="12" r="9"/><path d="M12 7v5l3 2"/>',
    helpful: '<path d="M7 11v9H4v-9Z"/><path d="m7 11 4-8c1.5 0 2.5 1 2.5 2.5V9H19a2 2 0 0 1 2 2.3l-1 7a2 2 0 0 1-2 1.7H7"/>',
    reopen: '<path d="M4 4v5h5"/><path d="M5.5 9A8 8 0 1 1 4 13"/>',
    download: '<path d="M12 3v12m-4-4 4 4 4-4M5 21h14"/>',
    external: '<path d="M14 4h6v6M20 4l-9 9"/><path d="M18 14v5a1 1 0 0 1-1 1H5a1 1 0 0 1-1-1V7a1 1 0 0 1 1-1h5"/>'
  };
  // Labels an icon-only control: the tooltip and the accessible name are the same description.
  function iconize(node, label, icon) {
    node.classList.add('task-action-icon'); node.title = label; node.setAttribute('aria-label', label);
    node.innerHTML = `<svg viewBox="0 0 24 24" aria-hidden="true">${ICON_PATHS[icon]}</svg>`;
    return node;
  }
  function taskIconButton(label, icon, action, mutation = true) {
    return iconize(actionButton('', action, mutation), label, icon);
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
      selectField(view, 'allow_relayed_human_answers', 'Human decisions answered in an agent terminal', project.allow_relayed_human_answers || false, [['false','Only a human may answer in this web UI'],['true','Agents may record answers a human gave them, marked as relayed']]);
      selectField(view, 'allow_subagent_reviews', 'Who may perform agent review?', project.allow_subagent_reviews || false, [['false','A separate credential principal'],['true','Also allow a registered, non-contributing subagent']]);
      const lease = view.field('lease_seconds','Ownership lease (seconds)',project.lease_seconds,'input'); lease.type = 'number'; lease.min = '30'; lease.max = '3600';
      const rules = view.field('rules','Binding rules',project.rules); rules.maxLength = 32768; rules.required = false;
      bindingRulesGuidance(rules);
      view.field('provenance','Reason and supporting source').maxLength = 4096;
      setupFieldHelp(view.form);
      view.finish('Save policy', (values, dialog) => {
        const body = {expected_revision:project.policy_revision, review_mode:values.get('review_mode'), recovery_mode:values.get('recovery_mode'), automatic_integration:values.get('automatic_integration') === 'true', agent_rule_editing:values.get('agent_rule_editing') === 'true', allow_subagent_reviews:values.get('allow_subagent_reviews') === 'true', allow_relayed_human_answers:values.get('allow_relayed_human_answers') === 'true', lease_seconds:Number(values.get('lease_seconds')), rules:values.get('rules'), provenance:values.get('provenance')};
        dialog.close(); startMutation(`${projectPath(projectId)}/policy`, body, 'project policy', async () => { await loadProjects(); setGlobalAlert('Project policy saved.', 'success'); }, 'PATCH', null, {projectId});
      });
    } catch (error) { setGlobalAlert(errorMessage(error)); }
  }

  // Integration owner: a human-only policy setting that the general policy
  // dialogs preserve by omission, so it has its own confirmed control.
  const OWNER_LABELS = {agent:'Agents', integrator:'Integrator service'};
  // The display name for an owner value; unknown values pass through.
  const ownerLabel = owner => OWNER_LABELS[owner] || text(owner) || 'Unknown';
  // One sentence naming the current owner and its policy revision.
  const ownerLine = project => `Current owner: ${ownerLabel(project.integration_owner)} (policy revision ${project.policy_revision}).`;
  // Shows the current owner on the settings page; only humans get the control.
  async function loadIntegrationOwner(projectId) {
    const summary = $('integration-owner-summary');
    show($('integration-owner-button'), state.actor?.kind === 'human');
    // The line already shown for this project stays until the reread replaces it.
    if (summary.dataset.project !== projectId) setText(summary, 'Reading the current integration owner…');
    try {
      const project = (await request(`${projectPath(projectId)}/orientation`)).data.project;
      if (projectId === state.projectId) { setText(summary, ownerLine(project)); setData(summary, 'project', projectId); }
    } catch (error) { if (projectId === state.projectId) { setText(summary, `The current integration owner could not be read: ${errorMessage(error)}`); delete summary.dataset.project; } }
  }
  // Reads the live policy, then opens the owner dialog for a human operator.
  async function openIntegrationOwner() {
    if (state.actor?.kind !== 'human') { setGlobalAlert('Only a human operator can change the integration owner.'); return; }
    const projectId = state.projectId, currentActor = actorId();
    try {
      const project = (await request(`${projectPath(projectId)}/orientation`)).data.project;
      if (projectId === state.projectId && actorId() === currentActor) integrationOwnerDialog(projectId, project);
    } catch (error) { setGlobalAlert(errorMessage(error)); }
  }
  // Builds the dialog: current owner, new owner, provenance and confirmation.
  function integrationOwnerDialog(projectId, project) {
    const view = workflowDialog('Integration owner', 'The integration owner publishes approved code to the target branch. Changing it creates a new policy revision that agents must acknowledge. The service refuses the change while an integration holds the target.');
    add(view.form, el('p', 'current-integration-owner', ownerLine(project)));
    const owner = selectField(view, 'integration_owner', 'New integration owner', project.integration_owner, [['agent','Agents integrate approved candidates'],['integrator','The integrator service integrates approved candidates']]);
    view.field('provenance', 'Reason and supporting source').maxLength = 4096;
    ownerConfirmation(view.form, project.integration_owner, owner);
    setupFieldHelp(view.form);
    view.finish('Change integration owner', (values, dialog) => { dialog.close(); saveIntegrationOwner(projectId, project, values); });
  }
  // Adds a required checkbox and returns it with its caption element.
  function confirmationBox(form, name) {
    const label = el('label', 'checkbox-label confirmation'), box = el('input'), caption = el('span');
    box.type = 'checkbox'; box.name = name; box.id = `workflow-${name}`; box.required = true;
    add(label, box); add(label, caption); add(form, label);
    return { label, box, caption };
  }
  // Requires a different owner and a confirmation naming the exact switch.
  function ownerConfirmation(form, current, select) {
    const { box, caption } = confirmationBox(form, 'confirm_owner');
    const sync = () => {
      select.setCustomValidity(select.value === current ? 'Choose a different owner to change it.' : '');
      setText(caption, `I confirm switching the integration owner from ${ownerLabel(current)} to ${ownerLabel(select.value)}.`);
      box.checked = false;
    };
    select.addEventListener('change', sync); sync();
  }
  // PATCHes the policy with every other current value preserved.
  function saveIntegrationOwner(projectId, project, values) {
    const owner = values.get('integration_owner');
    const body = {expected_revision:project.policy_revision, review_mode:project.review_mode, recovery_mode:project.recovery_mode, lease_seconds:project.lease_seconds, rules:project.rules, agent_rule_editing:project.agent_rule_editing, automatic_integration:project.automatic_integration, allow_subagent_reviews:project.allow_subagent_reviews, integration_owner:owner, provenance:values.get('provenance')};
    startMutation(`${projectPath(projectId)}/policy`, body, 'integration owner', async data => {
      await loadProjects(); if (projectId === state.projectId && state.currentView === 'project') await loadIntegrationOwner(projectId);
      setGlobalAlert(`Integration owner is now ${ownerLabel(data?.integration_owner || owner)}. Agents must read and acknowledge the new policy.`, 'success');
    }, 'PATCH', error => { if (!error.uncertain) setGlobalAlert(ownerChangeError(error)); }, {projectId});
  }
  // Explains a refused owner change; a held integration gets its own message.
  function ownerChangeError(error) {
    if (error.code === 'policy_hold_conflict') return 'The integration owner was not changed: an integration currently holds the target, and publication may already be in progress. Let it finish or reconcile it, then try again.';
    if (error.code === 'revision_conflict') return 'The integration owner was not changed: the project policy changed after you opened the form. Open Change integration owner again to see the current policy.';
    return errorMessage(error);
  }

  // Connected sessions: open agent sessions bound to the project, so an owner can quiesce work.
  const NO_AGENT_SESSIONS = 'No agent sessions are connected to this project in this window.';
  // Opens the connected-sessions page for the current project.
  // The attention digest: what the attention budget did and what needs a person.
  // Showing it to a person records the read the canary watches for.
  function openDigest() {
    if (!state.projectId) { setGlobalAlert('Choose a project first.'); return; }
    setDigestProjectLabel(); showView('digest'); $('digest-heading').focus(); loadDigest();
  }
  function setDigestProjectLabel() {
    const project = state.projects.find(item => text(item.id) === state.projectId);
    setText($('digest-project-label'), project?.name || 'Project');
  }
  // Loads and shows the digest, then records the read; a superseded response is dropped.
  async function loadDigest() {
    const projectId = state.projectId, seq = ++state.digestSeq;
    if (state.digestProjectId !== projectId) { state.digestProjectId = projectId; state.digestLoaded = false; reconcile($('digest-content'), []); }
    if (!state.digestLoaded) setState($('digest-state'), 'Loading the digest…', true);
    try {
      const digest = (await request(`${projectPath(projectId)}/digest`)).data;
      if (seq !== state.digestSeq || projectId !== state.projectId) return;
      renderDigest(digest); state.digestLoaded = true;
      try { await request(`${projectPath(projectId)}/digest/read`, { method: 'POST', body: {}, idempotencyKey: newKey() }); }
      catch (error) { if (seq === state.digestSeq) setState($('digest-state'), `Showing the digest, but recording the read failed: ${errorMessage(error)}`, false, true); }
    } catch (error) { if (seq === state.digestSeq && projectId === state.projectId) setState($('digest-state'), errorMessage(error), false, true); }
  }
  // A task as its name with the id after it; the id alone when the digest carries no name.
  const digestTaskRef = (title, id) => title ? `${text(title)} (task ${text(id)})` : `task ${text(id)}`;
  // One human-required intervention: its code, then the task name and id when it names a task.
  function digestHriLine(item) {
    const taskId = item.task_id || item.report?.task_id;
    if (!taskId) return `${text(item.code)}: ${text(item.title || item.summary || item.gate)}`;
    return `${text(item.code)}: ${item.gate ? `${text(item.gate)} on ` : ''}${digestTaskRef(item.title || item.task_title, taskId)}`;
  }
  function renderDigest(digest) {
    const hri = digest.hri || {}, proceeded = digest.proceeded_decisions || [], pending = digest.pending_reversible_decisions || [];
    const section = (key, title, items, line) => ({ key, sig: sig(title, items), build: () => {
      const card = el('article', 'card report-card'); add(card, el('h2', '', `${title} (${items.length})`));
      if (!items.length) add(card, el('p', 'muted', 'None.'));
      items.forEach(item => add(card, el('p', '', line(item))));
      return card;
    } });
    reconcile($('digest-content'), [
      section('proceeded', `Proceeded on their recommendation in the last ${digest.window_hours} hours`, proceeded, item => `${text(item.question)} → ${text(item.proceeded_with)} at ${formatDate(item.proceeded_at)}. Reopen decision ${text(item.decision_id)} to undo.${(item.affected_tasks || []).length ? ` Affects ${item.affected_tasks.map(task => digestTaskRef(task.title, task.task_id)).join(', ')}.` : ''}`),
      section('pending', 'Will proceed on their own', pending, item => `${text(item.question)} → ${text(item.recommendation)} at ${formatDate(item.proceeds_at)}`),
      section('hri', 'Human-required interventions', hri.items || [], digestHriLine)
    ]);
    setState($('digest-state'), digest.last_read_at ? `Last read ${formatDate(digest.last_read_at)} (before this visit).` : 'This digest had not been read before this visit.');
  }
  // The autonomy view: how much of the project's work ran without a person, each figure with its source.
  const AUTONOMY_NO_SUPERVISOR = 'No supervisor host has reported cost, caps or canary results for this project yet, so only the service\'s own records are shown.';
  const dollars = (value) => value === null || value === undefined ? '—' : `$${Number(value).toFixed(2)}`;
  const utcDay = (ms = Date.now()) => new Date(ms).toISOString().slice(0, 10);
  // A same-origin link to the API record a figure comes from.
  function sourceLink(label, path) {
    const link = el('a', 'source-link', label); link.href = path; link.target = '_blank'; link.rel = 'noopener noreferrer'; return link;
  }
  // "Source: a · b" under a figure.
  function sourceLine(...links) {
    const line = el('p', 'muted source-line', 'Source: ');
    links.forEach(([label, path], index) => { if (index) add(line, document.createTextNode(' · ')); add(line, sourceLink(label, path)); });
    return line;
  }
  // A table of rows; a cell is text or a node.
  function dataTable(headers, rows, className = '') {
    const table = el('table', `data-table ${className}`.trim()), head = add(add(table, el('thead')), el('tr'));
    headers.forEach(header => add(head, el('th', '', header)));
    const body = add(table, el('tbody'));
    rows.forEach(cells => { const row = add(body, el('tr')); cells.forEach(cell => { const td = add(row, el('td')); if (cell instanceof Node) add(td, cell); else setText(td, cell); }); });
    return table;
  }
  const outcomeBadge = (outcome) => el('span', `status-badge ${outcome === 'ok' ? 'report-resolved' : 'report-needs-human'}`, outcome);
  // A task as a button that opens its detail.
  function taskButton(projectId, taskId, title) {
    if (!taskId) return '—';
    const button = el('button', 'button text-button', title ? text(title) : text(taskId)); button.type = 'button';
    button.title = `Task ${taskId}`; button.addEventListener('click', () => { state.projectId = projectId; openTask(taskId, 'tasks'); });
    return button;
  }
  function openAutonomy(id = state.projectId) {
    if (!id) { setGlobalAlert('Choose a project first.'); return; }
    if (state.projectId !== text(id)) resetTaskPages();
    state.projectId = text(id); state.selectedTaskId = '';
    setAutonomyProjectLabel(); showView('autonomy'); $('autonomy-heading').focus(); loadAutonomy();
  }
  function setAutonomyProjectLabel() {
    const project = state.projects.find(item => text(item.id) === state.projectId);
    setText($('autonomy-project-label'), project?.name || 'Project');
  }
  // Loads every source of the view at once; a source that fails shows its error in its own card.
  async function loadAutonomy() {
    const projectId = state.projectId, seq = ++state.autonomySeq, days = Number($('autonomy-window').value), base = projectPath(projectId);
    if (state.autonomyProjectId !== projectId) { state.autonomyProjectId = projectId; state.autonomyLoaded = false; reconcile($('autonomy-content'), []); }
    if (!state.autonomyLoaded) setState($('autonomy-state'), 'Loading autonomy figures…', true);
    const paths = {
      autonomy: `${base}/telemetry/autonomy?days=${days}`,
      costDays: `${base}/telemetry/cost?group_by=day&limit=${days}&since_ms=${Date.now() - days * 86400000}`,
      costTasks: `${base}/telemetry/cost?group_by=task&limit=15&since_ms=${Date.now() - days * 86400000}`,
      canaries: `${base}/telemetry/canaries?limit=20`,
      digest: `${base}/digest`,
    };
    const keys = Object.keys(paths);
    const settled = await Promise.allSettled(keys.map(key => request(paths[key])));
    if (seq !== state.autonomySeq || projectId !== state.projectId) return;
    const sources = {};
    keys.forEach((key, index) => { sources[key] = settled[index].status === 'fulfilled' ? { data: settled[index].value.data, path: paths[key] } : { error: errorMessage(settled[index].reason), path: paths[key] }; });
    state.autonomyLoaded = true; renderAutonomy(projectId, sources, days);
  }
  // One card of the view: its title, a note when a source it needs failed, else its content, then its sources.
  // `used` is the part of the data the card shows, so a change elsewhere leaves the card alone.
  function autonomyCard(key, title, needs, used, sources, build, ...links) {
    const failed = needs.map(name => sources[name]).find(source => source.error);
    return { key, sig: sig(title, failed?.error, used), build: () => {
      const card = el('article', 'card report-card autonomy-card'); add(card, el('h2', '', title));
      if (failed) add(card, el('p', 'inline-alert error', `This figure is unavailable: ${failed.error}`));
      else build(card);
      add(card, sourceLine(...links.map(([label, name]) => [label, sources[name].path])));
      return card;
    } };
  }
  const merged = (...lists) => { const days = new Map(); lists.forEach((list, column) => (list || []).forEach(item => { const row = days.get(item.day) || [0, 0]; row[column] += item.count; days.set(item.day, row); })); return [...days].sort(([a], [b]) => b.localeCompare(a)); };
  function renderAutonomy(projectId, sources, days) {
    const autonomy = sources.autonomy.data || {}, digest = sources.digest.data || {}, costDays = sources.costDays.data || {}, costTasks = sources.costTasks.data || {}, canaries = sources.canaries.data || {};
    const caps = autonomy.caps || [], dayItems = costDays.items || [], canaryItems = canaries.items || [];
    const noSupervisor = sources.autonomy.data && sources.costDays.data && sources.canaries.data && !caps.length && !dayItems.length && !canaryItems.length;
    reconcile($('autonomy-content'), [
      autonomyCard('hri', `Human-required interventions, last ${days} days`, ['autonomy', 'digest'], [autonomy.human_required, autonomy.finished, (digest.hri || {}).count, (digest.hri || {}).stalled_tasks, (digest.hri || {}).stalled_queue], sources, card => {
        const hri = autonomy.human_required || {}, now = digest.hri || {}, finished = autonomy.finished || {};
        add(card, factList([['Interventions', text(hri.count)], ['Finished tasks', text(finished.count)], ['Per finished task', hri.per_finished_task === null || hri.per_finished_task === undefined ? 'No task finished in this window' : Number(hri.per_finished_task).toFixed(2)],
          ['Of which', `${text(hri.integrator_reports)} integrator reports needing a person, ${text(hri.parked_subjects)} subjects parked by the revise limit`], ['Stalled now', `${text((now.stalled_tasks || 0) + (now.stalled_queue || 0))} (not counted above: a stall has no start time in the records)`], ['Waiting for a person now', text(now.count)]]));
        const rows = merged(finished.by_day, hri.by_day);
        if (rows.length) add(card, dataTable(['Day (UTC)', 'Finished', 'Interventions'], rows.map(([day, [done, human]]) => [day, text(done), text(human)])));
        else add(card, el('p', 'muted', 'Nothing finished and no intervention in this window.'));
        add(card, el('p', 'muted', 'Decisions reserved for a person are not counted; the attention digest reports them separately.'));
      }, ['Workflow, report and revise records', 'autonomy'], ['Attention digest', 'digest']),
      autonomyCard('parked', 'Parked and human-required items', ['autonomy', 'digest'], [autonomy.parked, (digest.hri || {}).items], sources, card => {
        const parked = (autonomy.parked || {}).items || [], waiting = (digest.hri || {}).items || [];
        add(card, el('h3', '', `Parked by the revise limit (${parked.length})`));
        if (!parked.length) add(card, el('p', 'muted', 'No task is parked. A task parks when agents revised it three times (not serialized after a landing) or six times in 24 hours.'));
        else add(card, dataTable(['Task', 'Agent revises (24 h)', 'Counted toward the limit'], parked.map(item => [taskButton(projectId, item.task_id, item.title), text(item.agent_revises_24h), text(item.counted_revises_24h)])));
        add(card, el('h3', '', `Waiting for a person (${waiting.length})`));
        if (!waiting.length) add(card, el('p', 'muted', 'Nothing waits for a person.'));
        waiting.forEach(item => add(card, el('p', '', digestHriLine(item))));
      }, ['Task and revise records', 'autonomy'], ['Attention digest', 'digest']),
      autonomyCard('cost', `Cost against the daily caps, last ${days} days`, ['autonomy', 'costDays', 'costTasks'], [caps, costDays, costTasks], sources, card => renderAutonomyCost(card, projectId, caps, costDays, costTasks), ['Reported cost per day', 'costDays'], ['Reported cost per task', 'costTasks'], ['Reported caps', 'autonomy']),
      autonomyCard('reviews', `Review outcomes, last ${days} days`, ['autonomy'], autonomy.reviews, sources, card => {
        const reviews = autonomy.reviews || {};
        if (!reviews.submissions_reviewed) { add(card, el('p', 'muted', 'No submission was reviewed in this window.')); return; }
        add(card, factList([['Submissions reviewed', text(reviews.submissions_reviewed)], ['Approved first pass', `${text(reviews.approved_first_pass)} (a task's first submission with no change requested)`], ['Needed revision', `${text(reviews.revised)} (a reviewer requested changes)`], ['Reviewer disagreements', `${text(reviews.disagreements)} (one reviewer approved, another requested changes)`], ['Review decisions', text(reviews.decisions)]]));
      }, ['Review decisions', 'autonomy']),
      autonomyCard('canaries', 'End-to-end canary history', ['canaries'], canaries, sources, card => {
        const latest = canaries.latest || [];
        if (!canaryItems.length) { add(card, el('p', 'muted', 'No canary result has been reported for this project. A supervisor host reports each end-to-end canary run.')); return; }
        add(card, el('h3', '', 'Latest per host and harness'));
        add(card, dataTable(['Host', 'Harness', 'Outcome', 'When'], latest.map(item => [item.host, item.harness, outcomeBadge(item.outcome), formatDate(item.occurred_at)])));
        add(card, el('h3', '', 'History, newest first'));
        add(card, dataTable(['When', 'Outcome', 'Harness', 'Host', 'Duration', 'Task'], canaryItems.map(item => [formatDate(item.occurred_at), outcomeBadge(item.outcome), item.harness, item.host, `${Math.round(item.duration_seconds)} s`, taskButton(projectId, item.task_id)])));
        if (canaries.next_before) add(card, el('p', 'muted', 'Older runs are available from the source.'));
      }, ['Reported canary results', 'canaries']),
      autonomyCard('digest', 'Digest status', ['digest'], [digest.last_read_at, digest.digest_sender, digest.held_agent_tasks, digest.agent_task_weekly_budget], sources, card => {
        add(card, factList([['Last read', digest.last_read_at ? formatDate(digest.last_read_at) : 'Never read'], ['Designated sender', digest.digest_sender || 'None designated'], ['Last sent', 'Not recorded: the service sees when the digest is read, not when it is sent'], ['Held agent tasks', text((digest.held_agent_tasks || []).length)], ['Weekly agent-task budget', digest.agent_task_weekly_budget ? `${text(digest.agent_task_weekly_budget.admitted_this_week)} of ${text(digest.agent_task_weekly_budget.limit)} admitted this week` : '']]));
        const open = el('button', 'button subtle', 'Open the attention digest'); open.type = 'button'; open.addEventListener('click', openDigest); add(card, open);
      }, ['Attention digest', 'digest']),
    ]);
    if (noSupervisor) setState($('autonomy-state'), AUTONOMY_NO_SUPERVISOR); else show($('autonomy-state'), false);
  }
  // The cap a project's hosts allow a role per day: the sum of reported caps, null when none was reported, 0 when a host has none.
  function combinedCap(caps, field) {
    const values = caps.map(cap => cap[field]).filter(value => value !== null && value !== undefined);
    if (!values.length) return null;
    return values.includes(0) ? 0 : values.reduce((sum, value) => sum + value, 0);
  }
  const capCell = (spent, cap) => cap === null ? dollars(spent) : cap === 0 ? `${dollars(spent)} (no cap)` : `${dollars(spent)} of ${dollars(cap)}${spent > cap ? ' — over' : ''}`;
  function renderAutonomyCost(card, projectId, caps, costDays, costTasks) {
    const dayItems = costDays.items || [], taskItems = costTasks.items || [];
    add(card, el('h3', '', 'Daily caps reported by supervisor hosts'));
    if (!caps.length) add(card, el('p', 'muted', 'No host has reported its daily caps, so spend is shown without a cap.'));
    else add(card, dataTable(['Host', 'Implementation cap', 'Reviewer cap', 'Reported'], caps.map(cap => [cap.host, cap.impl_daily_usd === 0 ? 'None' : dollars(cap.impl_daily_usd), cap.rev_daily_usd === 0 ? 'None' : dollars(cap.rev_daily_usd), formatDate(cap.reported_at)])));
    add(card, el('h3', '', 'Cost per day (UTC)'));
    const implCap = combinedCap(caps, 'impl_daily_usd'), revCap = combinedCap(caps, 'rev_daily_usd');
    if (!dayItems.length) add(card, el('p', 'muted', 'No cost has been reported for this project in this window.'));
    else {
      add(card, dataTable(['Day', 'Implementation', 'Review', 'Total', 'Launches'], dayItems.map(item => [item.day, capCell(item.impl.usd, implCap), capCell(item.rev.usd, revCap), dollars(item.total_usd), text(item.impl.launches + item.rev.launches)])));
      add(card, el('p', 'muted', `Hosts cap the last 24 hours; a UTC day is only an approximation.${caps.length > 1 ? ' With several hosts the caps are added.' : ''} Estimated or unpriced launches: ${text(((costDays.totals || {}).impl || {}).estimated_launches + ((costDays.totals || {}).rev || {}).estimated_launches)} estimated, ${text(((costDays.totals || {}).impl || {}).unpriced_launches + ((costDays.totals || {}).rev || {}).unpriced_launches)} unpriced.`));
    }
    add(card, el('h3', '', 'Cost per task'));
    if (!taskItems.length) add(card, el('p', 'muted', 'No task cost has been reported in this window.'));
    else add(card, dataTable(['Task', 'Implementation', 'Review', 'Total'], taskItems.map(item => [taskButton(projectId, item.task_id, item.title), dollars(item.impl.usd), dollars(item.rev.usd), dollars(item.total_usd)])));
    if (costTasks.truncated) add(card, el('p', 'muted', 'Showing the most recent tasks; the source lists more.'));
  }
  // A compact autonomy summary on a project card, fetched once a minute at most.
  const AUTONOMY_SUMMARY_TTL_MS = 60000;
  function autonomySummaryText(summary) {
    if (summary.error) return 'Autonomy figures are unavailable.';
    const parts = [`HRI ${summary.hri}${summary.perFinished === null ? '' : ` (${summary.perFinished.toFixed(2)} per finished task)`} in 7 days`];
    parts.push(summary.canary ? `Last canary ${summary.canary.outcome}` : 'No canary reported');
    parts.push(summary.today === null ? 'No cost reported' : `Today ${dollars(summary.today)}`);
    return parts.join(' · ');
  }
  function fillAutonomySummary(project, container) {
    const summary = state.autonomySummaries.get(text(project.id));
    if (!summary) { patchSection(container, 'loading', box => add(box, el('span', 'muted', 'Loading autonomy…'))); return; }
    patchSection(container, sig(autonomySummaryText(summary)), box => {
      add(box, el('span', summary.error ? 'muted' : '', autonomySummaryText(summary)));
    });
  }
  async function loadAutonomySummary(project) {
    const id = text(project.id), cached = state.autonomySummaries.get(id), base = projectPath(id);
    if (cached && Date.now() - cached.at < AUTONOMY_SUMMARY_TTL_MS) return;
    if (state.fetching.has(`autonomy-summary-${id}`)) return;
    state.fetching.add(`autonomy-summary-${id}`);
    try {
      const [autonomy, canaries, cost] = (await Promise.all([`${base}/telemetry/autonomy?days=7`, `${base}/telemetry/canaries?limit=1`, `${base}/telemetry/cost?group_by=day&limit=1`].map(path => request(path)))).map(result => result.data);
      const day = (cost.items || [])[0];
      state.autonomySummaries.set(id, { at: Date.now(), hri: autonomy.human_required.count, perFinished: autonomy.human_required.per_finished_task, canary: (canaries.items || [])[0] || null, today: day ? (day.day === utcDay() ? day.total_usd : 0) : null });
    } catch (error) { state.autonomySummaries.set(id, { at: Date.now(), error: errorMessage(error) }); }
    finally { state.fetching.delete(`autonomy-summary-${id}`); }
    document.querySelectorAll('.autonomy-summary-text').forEach(container => { if (container.dataset.projectId === id) fillAutonomySummary(project, container); });
  }
  function openAgentSessions() {
    if (!state.projectId) { setGlobalAlert('Choose a project first.'); return; }
    setAgentSessionsProjectLabel(); showView('agent-sessions'); $('agent-sessions-heading').focus(); loadAgentSessions();
  }
  // Names the project in the sessions header; reruns once projects load after a reload.
  function setAgentSessionsProjectLabel() {
    const project = state.projects.find(item => text(item.id) === state.projectId);
    setText($('agent-sessions-project-label'), project?.name || 'Project');
  }
  // Loads the sessions active in the chosen window; a superseded response is dropped. Cards stay on
  // screen during a reload and only the sessions whose data changed are rebuilt.
  async function loadAgentSessions() {
    const projectId = state.projectId, seq = ++state.agentSessionsSeq;
    if (state.agentSessionsProjectId !== projectId) { state.agentSessionsProjectId = projectId; state.agentSessionsLoaded = false; reconcile($('agent-sessions-list'), []); }
    if (!state.agentSessionsLoaded) setState($('agent-sessions-state'), 'Loading sessions…', true);
    const query = new URLSearchParams({active_within_hours: $('agent-sessions-window').value});
    try {
      const page = (await request(`${projectPath(projectId)}/sessions?${query}`)).data;
      if (seq === state.agentSessionsSeq && projectId === state.projectId) { state.agentSessionsLoaded = true; renderAgentSessions(page); }
    } catch (error) { if (seq === state.agentSessionsSeq && projectId === state.projectId) setState($('agent-sessions-state'), errorMessage(error), false, true); }
  }
  // Renders one card per session, the empty-window message, or a truncation note.
  function renderAgentSessions(page) {
    const items = page.items || [];
    reconcile($('agent-sessions-list'), items.map(session => ({ key: session.session_id, sig: sig(session), build: () => agentSessionCard(session) })));
    if (!items.length) setState($('agent-sessions-state'), NO_AGENT_SESSIONS);
    else if (page.truncated) setState($('agent-sessions-state'), `Showing the ${items.length} most recently active sessions; narrow the window to see fewer.`);
    else show($('agent-sessions-state'), false);
  }
  // One session: who and where, when it started and last acted, and what it holds.
  function agentSessionCard(session) {
    const card = el('article', 'card report-card agent-session-card'); card.dataset.sessionId = session.session_id;
    const header = add(card, el('div', 'card-header')), held = session.held_attempts || [];
    add(header, el('h2', '', session.principal?.name || session.principal?.id || 'Agent'));
    add(header, el('span', `status-badge ${held.length ? 'report-open' : 'report-resolved'}`, held.length ? `Holds ${held.length} attempt${held.length === 1 ? '' : 's'}` : 'Idle'));
    add(card, factList([['Agent', agentKind(session)], ['Workstation', session.workstation_id], ['Session', session.session_id], ['Started', formatDate(session.started_at)], ['Last activity', formatDate(session.last_activity_at)]]));
    add(card, heldAttemptList(held));
    return card;
  }
  // The agent kind: the harness, plus the subagent name for a subagent session.
  const agentKind = session => session.subagent ? `${text(session.harness)} / subagent ${text(session.subagent.name)}` : text(session.harness);
  // The attempts a session holds (task title and id, kind, expiry), or a note that it holds none.
  function heldAttemptList(held) {
    if (!held.length) return el('p', 'muted', 'Holds no attempts');
    const list = el('ul', 'held-attempts');
    held.forEach(attempt => add(list, heldAttemptItem(attempt)));
    return list;
  }
  // One held attempt: task title, a lease-expired badge when its lease lapsed, and its details.
  function heldAttemptItem(attempt) {
    const item = el('li'), title = add(item, el('div', 'held-attempt-title'));
    add(title, el('strong', '', attempt.task_title || attempt.task_id));
    if (attempt.lease_expired) add(title, el('span', 'status-badge report-needs-human', 'Lease expired'));
    add(item, el('span', 'muted', heldAttemptMeta(attempt)));
    return item;
  }
  // A held attempt's task id, kind of work (review, integration or work), and lease expiry unless it already lapsed.
  const heldAttemptMeta = attempt => [attempt.task_id, displayStatus(attempt.activity_kind || attempt.mode), attempt.lease_expired ? 'needs recovery' : `expires ${formatDate(attempt.expires_at)}`].join(' · ');

  // Integrator reports: findings the integrator records; humans resolve them.
  const REPORT_KINDS = {privilege_gate:'Privilege gate', flaky:'Flaky check', fix_target:'Target needs a fix', unreviewed_landing:'Unreviewed landing', target_rewritten:'Target rewritten', ruleset_missing:'Ruleset missing'};
  // The display name for a report kind; unknown kinds get a readable form.
  const reportKind = report => REPORT_KINDS[report.kind] || displayStatus(report.kind);
  // Opens the reports page for the current project.
  function openReports() {
    if (!state.projectId) { setGlobalAlert('Choose a project first.'); return; }
    setReportsProjectLabel(); showView('reports'); $('reports-heading').focus(); loadReports();
  }
  // Names the project in the reports header; reruns once projects load after a reload.
  function setReportsProjectLabel() {
    const project = state.projects.find(item => text(item.id) === state.projectId);
    setText($('reports-project-label'), project?.name || 'Project');
  }
  // Loads one page of reports, newest first, replacing or extending the list. A reload keeps the cards on
  // screen and patches only the ones that changed; a different project starts from an empty list.
  async function loadReports(append = false) {
    const projectId = state.projectId, seq = ++state.reportsSeq;
    if (state.reportsProjectId !== projectId) { state.reportsProjectId = projectId; state.reports = []; state.reportsLoaded = false; reconcile($('reports-list'), []); }
    if (!append) state.reportsCursor = null;
    if (!state.reportsLoaded) { show($('load-more-reports'), false); setState($('reports-state'), 'Loading reports…', true); }
    const query = new URLSearchParams({limit:'50'});
    if ($('reports-filter').value === 'open') query.set('open', 'true');
    if (append && state.reportsCursor) query.set('before', state.reportsCursor);
    try {
      const page = (await request(`${projectPath(projectId)}/integrator/reports?${query}`)).data;
      if (seq === state.reportsSeq && projectId === state.projectId) renderReports(page, append);
    } catch (error) { if (seq === state.reportsSeq) reportsLoadFailed(error, append); }
  }
  // Shows a load error; a failed next page keeps its cursor so Load more can retry.
  function reportsLoadFailed(error, append) {
    setState($('reports-state'), errorMessage(error), false, true);
    show($('load-more-reports'), Boolean(append && state.reportsCursor));
  }
  // Patches the report cards and shows the next-page control.
  function renderReports(page, append = false) {
    state.reports = append ? [...state.reports, ...(page.items || [])] : (page.items || []); state.reportsLoaded = true;
    const human = state.actor?.kind === 'human';
    reconcile($('reports-list'), state.reports.map(report => ({ key: report.id, sig: sig(report, human), build: () => reportCard(report) })));
    state.reportsCursor = page.next_before || null; show($('load-more-reports'), Boolean(state.reportsCursor));
    if (state.reports.length) { show($('reports-state'), false); return; }
    setState($('reports-state'), $('reports-filter').value === 'open' ? 'No open reports. Nothing from the integrator is waiting for a person.' : 'The integrator has not recorded any reports for this project.');
  }
  // One report: kind, state, identities, raw details and the resolve action.
  function reportCard(report) {
    const card = el('article', 'card report-card'); card.dataset.reportId = report.id;
    const header = add(card, el('div', 'card-header'));
    add(header, el('h2', '', reportKind(report)));
    add(header, el('span', `status-badge report-${reportState(report)}`, reportStatus(report)));
    add(card, reportFacts(report));
    const details = add(card, el('details')); add(details, el('summary', '', 'Report details'));
    add(details, el('pre', '', JSON.stringify(report.details ?? {}, null, 2)));
    if (!report.resolved_at && state.actor?.kind === 'human') add(card, actionButton('Resolve report', () => openResolveReport(report)));
    return card;
  }
  // The badge state class: resolved, waiting for a person, or open.
  const reportState = report => report.resolved_at ? 'resolved' : report.requires_human ? 'needs-human' : 'open';
  // A short state label, including a privilege gate's recorded decision.
  function reportStatus(report) {
    if (!report.resolved_at) return report.requires_human ? 'Needs a person' : 'Open';
    if (report.decision) return report.decision === 'allow' ? 'Resolved: allowed' : 'Resolved: denied';
    return 'Resolved';
  }
  // The report's identities and any resolution as a definition list.
  function reportFacts(report) {
    return factList([['Report', report.id], ['Recorded', formatDate(report.created_at)], ['Task', report.task_id], ['Submission', report.submission_id], ['Result', report.result_id], ['Resolved', report.resolved_at && formatDate(report.resolved_at)], ['Resolved by', report.resolved_by], ['Resolution note', report.resolution_note]]);
  }
  // A definition list of the [label, value] pairs that have a value.
  function factList(pairs) {
    const facts = el('dl', 'report-facts');
    pairs.filter(([, value]) => value).forEach(([label, value]) => { add(facts, el('dt', 'muted', label)); add(facts, el('dd', '', value)); });
    return facts;
  }
  // Opens the human resolve form; a privilege gate also needs allow or deny.
  function openResolveReport(report) {
    const projectId = state.projectId;
    const view = workflowDialog(`Resolve ${reportKind(report).toLowerCase()} report`, resolveDescription(report));
    add(view.form, el('p', 'muted', `Report ${report.id}`));
    if (report.kind === 'privilege_gate') privilegeDecisionField(view);
    view.field('resolution_note', 'Resolution note').maxLength = 2000;
    setupFieldHelp(view.form);
    view.finish('Resolve report', (values, dialog) => { dialog.close(); resolveReport(projectId, report, values); });
  }
  // Explains what resolving this kind of report does.
  function resolveDescription(report) {
    if (report.kind === 'privilege_gate') return 'This result changes privileged repository paths, such as workflows or code owners. Allow lets the integrator push it; deny keeps it off the target branch. Inspect the result before deciding.';
    return 'Record what you did about this finding. Resolving closes the report; it does not change the task or the target branch.';
  }
  // Adds the allow/deny select with no preselected value; allow also needs a
  // confirmation that the privileged changes were inspected.
  function privilegeDecisionField(view) {
    const decision = selectField(view, 'report_decision', 'Decision', '', [['', 'Choose a decision…'], ['deny', 'Deny: keep this result off the target branch'], ['allow', 'Allow: the integrator may push this result']]);
    decision.required = true;
    const { label, box, caption } = confirmationBox(view.form, 'confirm_allow');
    setText(caption, 'I inspected the privileged changes in this result and allow the integrator to push them.');
    const sync = () => { const allow = decision.value === 'allow'; show(label, allow); box.required = allow; if (!allow) box.checked = false; };
    decision.addEventListener('change', sync); sync();
  }
  // POSTs the resolution; an already resolved report refreshes the list.
  function resolveReport(projectId, report, values) {
    const body = {note: values.get('resolution_note')};
    if (report.kind === 'privilege_gate') body.decision = values.get('report_decision');
    startMutation(`${projectPath(projectId)}/integrator/reports/${encodeURIComponent(report.id)}/resolve`, body, 'report resolution', async () => {
      if (projectId === state.projectId && state.currentView === 'reports') await loadReports();
      setGlobalAlert(`${reportKind(report)} report resolved.`, 'success');
    }, 'POST', error => reportResolveFailed(projectId, error), {projectId});
  }
  // Explains a refused resolution and reloads the list when it is stale.
  function reportResolveFailed(projectId, error) {
    if (error.uncertain) return;
    setGlobalAlert(error.code === 'report_already_resolved' ? 'This report was already resolved. The list now shows its current state.' : errorMessage(error));
    if (error.code === 'report_already_resolved' && projectId === state.projectId && state.currentView === 'reports') loadReports();
  }
  function renderTaskActions(data, task) {
    const target = $('task-operator-actions'), items = [];
    const completionPending = data.workflow?.submission && data.workflow.phase !== 'revision_needed';
    const ownAttempt = (data.attempts || []).find(item => item.id === task.current_attempt_id);
    // Eligible only when nothing could still change the task: the service refuses the rest with task_attempt_protected, task_workflow_protected or task_lifecycle_invalid.
    const activeWorkflow = (data.workflow?.activities || []).some(activity => ['queued','active','recovery_required'].includes(activity.state)) || ['review','integration'].includes(data.workflow?.phase);
    // Every button closes over the task, so they are rebuilt together, and only when something they use changed.
    const context = sig(pick(task, TASK_ACTION_FIELDS), pick(data, ['depends_on', 'dependencies']), Boolean(completionPending), activeWorkflow, ownAttempt && pick(ownAttempt, ['id', 'owner_id', 'session_id', 'state', 'mode', 'generation']), state.actor?.kind, actorId(), state.actor?.session_id);
    const push = (key, build) => items.push({ key, sig: context, build });
    if (state.actor?.kind === 'human' && !task.activity_kind && !task.current_attempt_id) {
      if (task.archived_at) push('restore', () => actionButton('Restore archived task', () => taskLifecycle(task,'restore','Restore this task to the main task queue? Its lifecycle state and history will be preserved.')));
      else {
        if (['open','planned','canceled'].includes(task.lifecycle)) push('archive', () => taskIconButton('Archive task', 'archive', () => taskLifecycle(task,'archive','Archive this task? It will leave the main task queue and remain available in Archived tasks.')));
        if (['open','planned'].includes(task.lifecycle)) push('cancel', () => taskIconButton('Cancel task', 'cancel', () => taskLifecycle(task,'cancel','Cancel this task? It will be deleted from the task queue, or archived if it has attempt, workflow, objective, or dependency history or a replacement.')));
        if (['planned','canceled'].includes(task.lifecycle)) push('delete', () => actionButton('Delete task', () => taskLifecycle(task,'delete','Remove this task from task views? This is a soft delete that retains task and audit history. The service refuses if attempts, workflow, objective, or dependency history exist.')));
      }
    }
    if (state.actor?.kind === 'human' && !task.activity_kind && !task.archived_at && ['open','planned'].includes(task.lifecycle)) push('human-delivery', () => actionButton(task.human_delivered ? 'Return to supervisors' : 'Mark human-delivered', () => taskHumanDelivery(task)));
    if (state.actor?.kind === 'human' && !task.activity_kind && !task.archived_at && task.lifecycle === 'open' && !task.current_attempt_id && !completionPending && !activeWorkflow) push('complete-external', () => actionButton('Complete as delivered externally', () => completeExternally(task)));
    if (!task.archived_at && !['done', 'completed'].includes(taskStatus(task)) && !completionPending && !task.current_attempt_id && ['open','planned'].includes(task.lifecycle) && ['ready','planned','blocked'].includes(taskStatus(task)) && !task.activity_kind) push('edit', () => taskIconButton('Edit task', 'edit', () => editTask(task, data)));
    if (!completionPending && !task.activity_kind && task.lifecycle === 'open' && !task.current_attempt_id && task.blocked_reason) push('unblock', () => taskIconButton('Resolve blocker', 'resolve', () => {
      const projectId = state.projectId, taskId = task.id;
      const view = workflowDialog('Resolve saved blocker', 'Use this after the problem that stopped work has been fixed. Record what changed and how you checked it. This clears the saved blocker so an agent can claim the task when its other requirements are met.');
      add(view.form, el('h3', '', 'What stopped work'));
      add(view.form, el('p', 'saved-blocker', task.blocked_reason));
      const reason = view.field('reason','What changed and how you verified it'); reason.maxLength = 4096;
      reason.placeholder = 'Example: Repository access was restored. Verified that the assigned workstation can fetch the required branch.';
      view.help(reason, 'Resolution evidence', 'Write the concrete fix and the observation, check result, or link that confirms work can resume. Use your actual evidence, not the example. This records the resolution in task history; it does not approve submitted work or mark the task complete.');
      view.finish('Record resolution', (values, dialog) => { dialog.close(); startMutation(`${projectPath(projectId)}/tasks/${encodeURIComponent(taskId)}/unblock`, {expected_revision:task.revision,reason:values.get('reason')}, 'blocker resolution', () => loadTaskDetail(), 'POST', null, {projectId,taskId}); });
    }));
    if (taskStatus(task) === 'recovery_required' && !task.activity_kind) push('recovery', () => actionButton('Inspect for recovery', () => claimRecovery(task)));
    const attempt = ownAttempt;
    if (attempt && attempt.owner_id === actorId() && attempt.session_id === state.actor?.session_id && attempt.state === 'active' && taskStatus(task) !== 'recovery_required') {
      if (attempt.mode === 'recovery') push('recovery-evidence', () => actionButton('Review recovery evidence', () => recoveryEvidence(task, attempt)));
      push('renew', () => actionButton('Renew my ownership', () => workflowMutation(`${projectPath()}/attempts/${encodeURIComponent(attempt.id)}/renew`, {generation:attempt.generation}, 'ownership renewal')));
      push('release', () => actionButton('Release with handoff', () => releaseOwnedAttempt(attempt)));
    }
    if (task.objective_id) push('objective', () => actionButton('View objective children', () => showObjective(task.objective_id), false));
    if (task.parent_objective_id) push('parent-objective', () => actionButton('View parent objective', () => showObjective(task.parent_objective_id), false));
    reconcile(target, items); renderMutationState();
  }
  function taskHumanDelivery(task) {
    const projectId=state.projectId, taskId=task.id, mark=!task.human_delivered;
    const view=workflowDialog(mark?'Mark human-delivered':'Return to supervisors',mark?'Supervisors will no longer be offered or able to claim this task, including after a review requests changes. You or an interactive CLI session can still claim it, and you can complete it as delivered externally.':'Supervisors may be offered and claim this task again.');
    view.field('reason','Reason for this change');
    view.finish(mark?'Mark human-delivered':'Return to supervisors',(values,dialog)=>{
      dialog.close();
      startMutation(`${projectPath(projectId)}/tasks/${encodeURIComponent(taskId)}/human-delivery`,{expected_revision:task.revision,human_delivered:mark,reason:values.get('reason')},'human delivery mark',()=>loadTaskDetail(),'POST',null,{projectId,taskId});
    });
  }
  // Records a human's attestation that a task's change already reached the target branch outside the coordinator.
  async function completeExternally(task) {
    const projectId=state.projectId, taskId=task.id, currentActor=actorId();
    let roster;
    try {
      roster=(await request(`${projectPath(projectId)}/workflow-policy`).catch(error=>{
        if (error.code==='workflow_policy_required') return {data:{required_checks:[]}};
        throw error;
      })).data.required_checks||[];
    } catch (error) { setGlobalAlert(`The required-check roster could not be read: ${errorMessage(error)}`,'error'); return; }
    if (projectId!==state.projectId||currentActor!==actorId()||taskId!==state.selectedTaskId) return;
    const view=workflowDialog('Complete as delivered externally','Use this only for a change that was already landed on the target branch outside the coordinator, for example with scripts/ship.py. The coordinator cannot reach the repository: it records your statement that the revision is on the target branch and that its required checks succeeded, and does not verify either. Confirm both on the remote first. The task must have no current attempt and no active review or integration.');
    const revision=view.field('delivered_revision','Delivered revision (full commit SHA on the target branch)','','input'); revision.pattern='([0-9a-fA-F]{40}|[0-9a-fA-F]{64})'; revision.maxLength=64; revision.autocomplete='off';
    view.field('target_branch','Target branch',state.projects.find(project=>text(project.id)===projectId)?.target_branch||'','input').maxLength=255;
    view.field('reason','How it was delivered','','textarea').maxLength=4096;
    add(view.form,el('h3','','Required check results on the delivered revision'));
    add(view.form,el('p','muted',roster.length?'Every check in the project roster is listed. Confirm each one succeeded on the delivered revision.':'This project has no required-check roster. Name at least one check that succeeded on the delivered revision.'));
    const rows=el('div','external-checks'); add(view.form,rows);
    const addCheck=(identity='',fixed=false)=>{
      const row=el('div','external-check'), id=`external-check-${rows.children.length}`, name=el('input'), caption=el('label','','Check'), done=el('input'), doneCaption=el('label','','Succeeded');
      name.id=id; name.name='check_identity'; name.value=identity; name.required=true; name.maxLength=255; name.readOnly=fixed; caption.htmlFor=id;
      done.type='checkbox'; done.id=`${id}-success`; done.name='check_success'; done.required=true; doneCaption.htmlFor=done.id;
      const checkName=el('div'); add(checkName,caption); add(checkName,name); add(row,checkName);
      const checkDone=el('div'); add(checkDone,done); add(checkDone,doneCaption); add(row,checkDone);
      if (!fixed) { const remove=el('button','button subtle','Remove'); remove.type='button'; remove.addEventListener('click',()=>row.remove()); add(row,remove); }
      add(rows,row);
    };
    if (roster.length) roster.forEach(check=>addCheck(text(check.identity),true)); else addCheck();
    const more=el('button','button subtle','Add another check'); more.type='button'; more.addEventListener('click',()=>{ if (rows.children.length<100) addCheck(); }); add(view.form,more);
    view.finish('Complete task',(values,dialog)=>{
      const checks=Array.from(rows.querySelectorAll('.external-check')).map(row=>({identity:row.querySelector('input[name=check_identity]').value.trim(),conclusion:'success'}));
      if (!checks.length) { setGlobalAlert('Cite at least one required check result.','error'); return; }
      dialog.close();
      // The task may have been reloaded since the dialog opened, so the freshest revision is the one to assert.
      const latest=state.detail?.task||state.detail, revision=latest&&text(latest.id)===taskId&&latest.revision>task.revision?latest.revision:task.revision;
      startMutation(`${projectPath(projectId)}/tasks/${encodeURIComponent(taskId)}/complete-external`,{expected_revision:revision,delivered_revision:values.get('delivered_revision').trim(),target_branch:values.get('target_branch').trim(),reason:values.get('reason').trim(),checks},'external completion',async()=>{await loadTaskDetail(); setGlobalAlert('Task completed as delivered externally.','success');},'POST',error=>completeExternallyFailed(projectId,taskId,error),{projectId,taskId});
    });
  }
  // Names the service's refusal code, and reloads the task when it changed under the form.
  function completeExternallyFailed(projectId,taskId,error) {
    if (error.uncertain) return;
    setGlobalAlert(error.code?`Not completed (${error.code}): ${errorMessage(error)}`:errorMessage(error),'error');
    if (['task_revision_changed','task_attempt_protected','task_workflow_protected','task_lifecycle_invalid','task_archived'].includes(error.code)&&projectId===state.projectId&&taskId===state.selectedTaskId) loadTaskDetail();
  }
  function taskLifecycle(task, action, confirmation) {
    const projectId=state.projectId, taskId=task.id;
    const view=workflowDialog(({archive:'Archive task',restore:'Restore task',cancel:'Cancel task',delete:'Delete task'})[action],confirmation);
    view.field('reason','Reason for this action');
    view.finish(({archive:'Archive task',restore:'Restore task',cancel:'Cancel task',delete:'Delete task'})[action],(values,dialog)=>{
      dialog.close(); const path=`${projectPath(projectId)}/tasks/${encodeURIComponent(taskId)}`;
      const callback=async()=>{if(action==='delete'||action==='cancel'){showView('tasks');await loadTasks();}else await loadTaskDetail();};
      startMutation(action==='delete'?path:`${path}/${action}`,{expected_revision:task.revision,reason:values.get('reason')},`task ${action}`,callback,action==='delete'?'DELETE':'POST',null,{projectId,taskId});
    });
  }
  function editTask(task, data) {
    const projectId = state.projectId, taskId = task.id;
    const view = workflowDialog('Edit task', 'Only unowned work can be edited. Admitting planned work makes it eligible when its dependencies and other requirements are satisfied.');
    selectField(view,'kind','Kind',task.kind,[['code','Code'],['general','General']]);
    view.field('title','Title',task.title,'input').maxLength = 300;
    const description = view.field('description','Description',task.description); description.required = false; description.maxLength = 32768;
    view.field('criteria','Acceptance criteria — one per line',(task.acceptance_criteria || []).join('\n')).maxLength = 205000;
    const dependencies = task.depends_on || data.depends_on || data.dependencies || [];
    view.field('depends_on','Prerequisite task IDs — one per line',dependencies.map(item => typeof item === 'string' ? item : item.prerequisite_id || item.id).join('\n')).required = false;
    const priority = view.field('priority','Priority (0 highest, 3 lowest)',task.priority,'input'); priority.type = 'number'; priority.min = '0'; priority.max = '3';
    selectField(view,'planned','Admission',task.lifecycle === 'planned',[['true','Keep planned'],['false','Admit to ready queue']]);
    view.finish('Save task', (values, dialog) => {
      dialog.close(); startMutation(`${projectPath(projectId)}/tasks/${encodeURIComponent(taskId)}`, {expected_revision:task.revision,kind:values.get('kind'),title:values.get('title'),description:values.get('description'),acceptance_criteria:lines(values.get('criteria')),depends_on:lines(values.get('depends_on')),priority:Number(values.get('priority')),planned:values.get('planned') === 'true'}, 'task edit', () => loadTaskDetail(), 'PATCH', null, {projectId,taskId});
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
      const evidenceFields = recoveryEvidenceFields(view, task, recordedRevision(state.detail, attempt));
      view.finish('Record recovery decision', (values, dialog) => { dialog.close(); workflowMutation(`${projectPath(projectId)}/attempts/${encodeURIComponent(attempt.id)}/recovery-resolution`, {generation:attempt.generation,disposition:values.get('disposition'),summary:values.get('summary'),...evidenceFields(values)}, 'recovery decision'); });
    } catch (error) { setGlobalAlert(errorMessage(error)); }
  }
  // The attempt a recovery superseded (highest earlier generation) and the revision its latest checkpoint recorded; null for a legacy checkpoint.
  function recordedRevision(detail, attempt) {
    const prior = (detail?.attempts || []).filter(item => item.generation < attempt.generation).sort((a, b) => b.generation - a.generation)[0];
    const latest = prior && (detail.checkpoints || []).find(checkpoint => checkpoint.attempt_id === prior.id);
    return latest?.revision ? { prior, revision: latest.revision } : null;
  }
  // Adds the recovery evidence inputs and returns a function that reads them into the request body:
  // a fetched SHA when the expired attempt recorded a revision, otherwise the two local attestations.
  function recoveryEvidenceFields(view, task, recorded) {
    if (!recorded) {
      for (const [name,label] of [['saved_work_checked','I checked saved work and its source checkpoints'],['running_jobs_checked','I checked still-running and uncertain jobs']]) { const input = view.field(name,label,'','input'); input.type = 'checkbox'; input.value = 'checked'; }
      return values => ({saved_work_checked:values.get('saved_work_checked') === 'checked',running_jobs_checked:values.get('running_jobs_checked') === 'checked'});
    }
    const base = 'refs/agent-coordinator/candidates/wip/', note = el('div', 'recorded-revision'); note.id = 'recovery-recorded-revision';
    add(note, el('p', '', 'The expired attempt recorded this work-in-progress commit:')); add(note, el('code', '', recorded.revision));
    // A supervised launch's WIP ref names the supervisor's launch id, which the service never sees,
    // so it is shown as an ls-remote pattern rather than a guessed exact ref.
    add(note, el('p', '', 'Find its WIP ref (a CLI push, or a supervised-launch push under any launch id) and paste the commit it names:'));
    for (const ref of [`git ls-remote origin '${base}${recorded.prior.id}/${recorded.revision}'`, `git ls-remote origin '${base}${task.id}/*/${recorded.revision}'`]) add(note, el('code', '', ref));
    add(note, el('p', 'muted', 'The service cannot read Git. It only checks that the SHA you enter equals the recorded one, so confirm the fetched ref names it first.'));
    add(view.form, note);
    const input = view.field('fetched_revision','Fetched commit SHA (40 lowercase hex characters)','','input'); input.pattern = '[0-9a-f]{40}'; input.maxLength = 40; input.autocomplete = 'off'; input.spellcheck = false;
    return values => ({fetched_revision:values.get('fetched_revision')});
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
    const list = add(view.form,el('div')), more = actionButton('Load more objectives', () => load(true), false); add(view.form,more); let cursor = null, busy = false, found = [];
    async function load(append = false) {
      if (busy) return; busy = true; more.disabled = true;
      try {
        const page = (await request(`${projectPath(projectId)}/objectives?limit=50${append && cursor ? `&cursor=${encodeURIComponent(cursor)}` : ''}`)).data;
        if (!view.dialog.isConnected || actorId() !== currentActor) return;
        found = append ? [...found, ...(page.items || [])] : (page.items || []);
        reconcile(list, found.length ? found.map(item => ({ key: item.id || item.task_id, sig: sig(item), build: () => actionButton(`${item.title || item.id} · Required children ${item.completed_required_child_count}/${item.required_child_count} · ${displayStatus(item.work_status)}`, () => { view.dialog.close(); showObjective(item.id || item.task_id); },false) })) : [{ key: 'none', sig: '', build: () => el('p','muted','No objectives yet.') }]);
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
      state.operators = append ? [...state.operators, ...(page.items || [])] : (page.items || []);
      const items = state.operators.map(operator => ({ key: `operator:${operator.id}`, sig: sig(operator), build: () => {
        const row = el('div','credential-row'); add(row,el('span','',`${operator.name} · ${displayStatus(operator.role)} · ${operator.enabled ? 'Enabled' : 'Disabled'} · ${operator.email ? `${operator.email} (${operator.email_verified ? 'verified' : 'requires verification'})` : 'No email address'}`));
        const actions = el('div','credential-actions'); add(row,actions);
        add(actions,taskIconButton('Browser sessions', 'sessions', () => browserSessions(operator.id,operator.name),false));
        add(actions,taskIconButton('Change access', 'access', () => {
          const view = workflowDialog(`Access for ${operator.name}`, 'Access changes end this account’s existing browser sessions. The last enabled administrator must remain enabled with administrator access.');
          selectField(view,'role','Role',operator.role,[['operator','Operator'],['admin','Administrator']]);
          selectField(view,'enabled','Account access',operator.enabled,[['true','Enabled'],['false','Disabled']]);
          view.finish('Save access', (values, dialog) => { dialog.close(); startMutation(`/api/v1/admin/operators/${encodeURIComponent(operator.id)}/access`, {expected_revision:operator.revision,role:values.get('role'),enabled:values.get('enabled') === 'true'},'account access',async data => { if (operator.id === actorId() && data.sessions_revoked) { signOutLocal(); showLoginError('Access updated. Sign in again if your account remains enabled.'); } else await loadOperators(); }); });
        }));
        return row;
      } }));
      if (page.next_cursor) items.push({ key: 'more', sig: sig(page.next_cursor), build: () => { const more = actionButton('Load more accounts', () => { more.disabled = true; loadOperators(true,page.next_cursor); },false); more.dataset.moreOperators = 'true'; return more; } });
      reconcile(target, items);
      renderMutationState();
    } catch (error) { setGlobalAlert(errorMessage(error)); }
  }
  function createOperator() {
    const view = workflowDialog('Add operator', 'All authenticated people and agents can access every project. Administrators also manage operator accounts and agent credentials.');
    view.field('name','Username','','input').maxLength = 100;
    selectField(view,'role','Role','operator',[['operator','Operator'],['admin','Administrator']]);
    const email = view.field('email','Email address (a verification link is sent here)','','input'); email.type = 'email'; email.autocomplete = 'off'; email.maxLength = 254;
    const password = view.field('password','Initial password (at least 12 characters)','','input'); password.type = 'password'; password.autocomplete = 'new-password'; password.maxLength = 1024; password.minLength = 12;
    const confirmation = view.field('confirmation','Confirm initial password','','input'); confirmation.type = 'password'; confirmation.autocomplete = 'new-password';
    view.finish('Create account', (values, dialog) => {
      if (values.get('password') !== values.get('confirmation')) { confirmation.setCustomValidity('Passwords must match.'); confirmation.reportValidity(); confirmation.addEventListener('input', () => confirmation.setCustomValidity(''),{once:true}); return; }
      const body = {name:values.get('name'),role:values.get('role'),email:values.get('email').trim(),password:values.get('password')};
      dialog.close(); startMutation('/api/v1/admin/operators',body,'operator creation',async () => { await loadOperators(); setGlobalAlert('Operator account created.', 'success'); });
    });
  }
  async function myAccount() {
    const currentActor = actorId();
    try {
      const account = (await request('/api/v1/auth/account')).data, operator = account.operator, verification = account.email_verification || {};
      if (actorId() !== currentActor) return;
      const view = workflowDialog('My account', `${operator.name} · ${displayStatus(operator.role)}. Changing your password signs out every browser session for this account.`);
      const themeGroup = el('fieldset', 'theme-selector'); add(themeGroup, el('legend', '', 'Website theme'));
      const savedTheme = (() => { try { return localStorage.getItem(THEME_KEY) || 'system'; } catch { return 'system'; } })();
      [['light','Light'],['dark','Dark'],['system','System']].forEach(([value,label]) => {
        const choice = el('label', 'theme-choice'); const radio = document.createElement('input'); radio.type = 'radio'; radio.name = 'site-theme'; radio.value = value; radio.checked = savedTheme === value;
        radio.addEventListener('change', () => applyTheme(value)); add(choice, radio); add(choice, el('span', '', label)); add(themeGroup, choice);
      });
      add(view.form, themeGroup);
      const emailBox = el('fieldset', 'email-settings'); add(emailBox, el('legend', '', 'Email address'));
      const emailStatus = operator.email ? `${operator.email} · ${operator.email_verified ? 'Verified' : 'Requires verification'}` : 'No email address is set.';
      add(emailBox, el('p', '', emailStatus));
      if (operator.email && !operator.email_verified) {
        const failed = verification.send_error ? (verification.send_error === 'mail_not_configured' ? 'This server is not set up to send email.' : 'The last verification email could not be sent.') : '';
        if (failed) add(emailBox, el('p', 'error', failed));
        else if (!verification.sent_at) add(emailBox, el('p', 'muted', 'Verification email is being sent.'));
        else add(emailBox, el('p', 'muted', 'A verification email was sent. Open the link in the newest message.'));
        const resend = actionButton('Resend verification email', () => { resend.disabled = true; view.dialog.close(); startMutation('/api/v1/auth/email/resend', {}, 'verification email', restoredMutationCallbacks('account_email'), 'POST', error => setGlobalAlert(errorMessage(error), 'error', error.uncertain)); });
        const wait = Date.parse(verification.resend_available_at || '') - Date.now();
        if (wait > 0) { resend.disabled = true; add(emailBox, el('p', 'muted', 'Another verification email can be requested after one minute. Reopen this dialog then.')); setTimeout(() => { if (resend.isConnected) resend.disabled = false; }, wait + 250); }
        add(emailBox, resend);
      }
      const emailCaption = el('label', '', operator.email ? 'Change email address' : 'Email address'); emailCaption.htmlFor = 'account-email';
      const emailInput = el('input'); emailInput.id = 'account-email'; emailInput.type = 'email'; emailInput.autocomplete = 'email'; emailInput.maxLength = 254;
      add(emailBox, emailCaption); add(emailBox, emailInput);
      add(emailBox, actionButton('Save email and send verification', () => {
        if (!emailInput.value.trim() || !emailInput.reportValidity()) return;
        const body = {expected_revision:operator.revision,email:emailInput.value.trim()};
        view.dialog.close(); startMutation('/api/v1/auth/email', body, 'email address', restoredMutationCallbacks('account_email'), 'POST', error => setGlobalAlert(errorMessage(error), 'error', error.uncertain));
      }));
      add(view.form, emailBox);
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
    const list = add(view.form,el('div')), more = actionButton('Load more sessions', () => load(true),false); add(view.form,more); let cursor = null, busy = false, sessions = [];
    async function load(append = false) {
      if (busy) return; busy = true; more.disabled = true;
      try {
        const page = (await request(`/api/v1/browser-sessions?principal_id=${encodeURIComponent(principalId)}${append && cursor ? `&cursor=${encodeURIComponent(cursor)}` : ''}`)).data;
        if (!view.dialog.isConnected || actorId() !== currentActor) return;
        sessions = append ? [...sessions, ...(page.items || [])] : (page.items || []);
        reconcile(list, sessions.map(session => ({ key: session.id, sig: sig(session), build: () => {
          const row = el('div','shared-card'); add(row,el('p','',`${session.current ? 'This browser' : session.id} · Created ${formatDate(session.created_at)} · Expires ${formatDate(session.expires_at)}`));
          if (session.revoked_at) add(row,el('p','muted',`Revoked ${formatDate(session.revoked_at)}`));
          else add(row,actionButton('Revoke session', () => startMutation(`/api/v1/browser-sessions/${encodeURIComponent(session.id)}/revoke`,{},'browser session revocation',async () => { if (session.current) { view.dialog.close(); signOutLocal(); } else await load(); })));
          return row;
        } })));
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
      const target = $('clock-status');
      const paused = data.clock_state.status !== 'ready';
      const items = [
        { key: 'summary', sig: sig(paused), build: () => el('p', '', paused
          ? 'Coordination is paused because the server clock moved backward. Correct and verify the host clock before reconciling this incident. Expired ownership and preserved holds still require recovery.'
          : 'No unresolved server clock incident. Task leases use protected server time.') },
        { key: 'evidence', sig: sig(data), build: () => { const holder = el('div'); recordDetails(holder, 'Clock status and incident evidence', data); return holder; } }
      ];
      if (paused) items.push({ key: 'reconcile', sig: sig(data.clock_state.incident_id), build: () => actionButton('Reconcile corrected clock', () => {
        const view = workflowDialog('Reconcile server clock', 'Record how trustworthy server time was restored. The service checks its stored time boundary before clearing this pause; old ownership is not revived.');
        view.field('reason','Clock correction evidence').maxLength = 2000;
        view.finish('Record clock reconciliation', (values, dialog) => {
          dialog.close(); startMutation('/api/v1/admin/clock/reconcile', {incident_id:data.clock_state.incident_id,reason:values.get('reason')}, 'clock reconciliation', restoredMutationCallbacks('clock_change'));
        });
      }) });
      reconcile(target, items);
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
      const service = data.service_state, restoreId = service.restore_id;
      if (!cursor) {
        const items = [{ key: 'summary', sig: sig(service.coordination_state), build: () => el('p', '', service.coordination_state === 'restore_reconciliation'
          ? 'Coordination is paused after restore. Inspect preserved holds, stop the old installation, and reconcile work since the snapshot before resuming.'
          : 'No restore reconciliation is required. Clock, task ownership, and physical resource checks still apply.') }];
        if (data.restore) {
          items.push({ key: 'progress', sig: sig(data.restore.inspected, data.restore.required_inspections, data.restore.old_installation_fenced, data.restore.post_snapshot_gap_reconciled), build: () => el('p', 'muted', `${data.restore.inspected}/${data.restore.required_inspections} preserved holds inspected · Old installation ${data.restore.old_installation_fenced ? 'fenced' : 'not yet confirmed stopped'} · Snapshot gap ${data.restore.post_snapshot_gap_reconciled ? 'reconciled' : 'not yet reconciled'}`) });
          items.push({ key: 'record', sig: sig(data.restore), build: () => { const holder = el('div'); recordDetails(holder, 'Restore record and reconciliation evidence', data.restore); return holder; } });
        }
        if (service.coordination_state === 'restore_reconciliation') {
          if (!data.restore?.old_installation_fenced) items.push({ key: 'fence', sig: sig(restoreId), build: () => actionButton('Record old installation stopped', () => restoreEvidence(restoreId, 'old-installation-fenced', 'Old installation stopped', 'Record how the original service was stopped or fenced so it cannot grant competing ownership.')) });
          if (!data.restore?.post_snapshot_gap_reconciled) items.push({ key: 'gap', sig: sig(restoreId), build: () => actionButton('Record work since the snapshot', () => restoreEvidence(restoreId, 'post-snapshot-gap', 'Reconcile the snapshot gap', 'Compare source checkpoints, publications, jobs, and completed work after the snapshot. Record what was lost, recovered, or remains uncertain.')) });
          items.push({ key: 'finish', sig: sig(restoreId), build: () => actionButton('Resume coordination', () => restoreEvidence(restoreId, 'finish', 'Resume coordination after restore', 'Every preserved hold must have an inspection record, and both installation fencing and the snapshot gap must be documented. Resuming does not release any physical or integration hold.', true)) });
        }
        reconcile(target, items);
      }
      state.restoreItems = cursor ? [...state.restoreItems, ...(data.items || [])] : (data.items || []);
      const rows = state.restoreItems.map(requirement => ({ key: `hold:${requirement.kind}:${requirement.target_id}`, sig: sig(requirement, service.coordination_state, restoreId), build: () => {
        const row = el('div', 'shared-card');
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
        return row;
      } }));
      if (data.next_cursor) rows.push({ key: 'more', sig: sig(data.next_cursor), build: () => { const more = actionButton('Load more preserved holds', () => { more.disabled = true; loadRestore(data.next_cursor); }, false); more.dataset.moreRestore = 'true'; return more; } });
      reconcile(list, rows);
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
    credentialAttributeFields(view, credential);
    view.finish('Issue new token', (values, dialog) => { dialog.close(); startMutation(`/api/v1/admin/agents/${encodeURIComponent(credential.principal_id)}/credentials`, {name:values.get('name'), class:values.get('class'), access:values.get('access')}, 'credential replacement', restoredMutationCallbacks('rotate_credential')); });
  }
  // Adds class and access selects matching the issue form, pre-filled from the
  // principal's current credential so a re-issued token keeps its authority.
  function credentialAttributeFields(view, credential) {
    selectField(view, 'class', 'Class', credential.class || 'interactive', [['interactive', 'Interactive (a person drives the harness)'], ['supervised', 'Supervised (launched by the supervisor)'], ['integrator', 'Integrator (the deterministic integrator only)']]);
    selectField(view, 'access', 'Access', credential.access || 'write', [['write', 'Write'], ['read', 'Read-only (reviewers, shadow hosts)']]);
  }
  $('refresh-restore').addEventListener('click', () => loadRestore());

  restoreSession();
})();
