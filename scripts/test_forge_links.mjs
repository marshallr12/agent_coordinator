// Drives the task-detail forge links in headless Chrome against a fixture service. Run with CHROME_BIN set.
import { createServer } from 'node:http';
import { mkdtemp, readFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { spawn } from 'node:child_process';
import { once } from 'node:events';
import net from 'node:net';

const root = resolve(import.meta.dirname, '..');
const SHA = '0123456789abcdef0123456789abcdef01234567';
const SHA256 = 'a'.repeat(64);
const TARGET_SHA = 'fedcba9876543210fedcba9876543210fedcba98';
const LOCAL_JOB = 'job-local-1', REMOTE_JOB = 'job-remote-2';
const fixtureActor = { id: 'fixture-operator', name: 'Fixture operator', role: 'admin', kind: 'human', session_id: 'fixture-browser' };
let repositoryUrl = 'https://github.com/example-org/example-repo.git';
let candidateRevision = SHA;
let detailReads = 0;
const job = (id, extra = {}) => ({ id, producer_id: 'producer-1', label: `Check ${id}`, state: 'succeeded', observation_freshness: 'fresh', last_observed_at: '2026-10-06T00:00:00Z', exit_code: 0, inputs_unchanged: true, source_revision: SHA, ...extra });
const baseTask = { id: 'forge-task', title: 'Forge link fixture', description: 'Synthetic.', revision: 1, kind: 'code', priority: 2, lifecycle: 'open', work_status: 'in_review', acceptance_criteria: [], attempts: [], checkpoints: [] };
const subjectTask = () => ({
  ...baseTask,
  job_evidence: { jobs: [job(LOCAL_JOB)], reservations: [] },
  workflow: {
    phase: 'integration', blockers: [],
    submission: { id: 'submission-1', kind: 'code', summary: 'Fixture candidate', candidate_revision: candidateRevision, candidate_ref: 'refs/ac/candidate', task_id: baseTask.id, task_revision: 1, project_policy_revision: 1 },
    activities: [{
      id: 'activity-1', kind: 'integration', status: 'running', activity_task_id: 'forge-activity-task',
      intent: { observed_target_revision: TARGET_SHA, result_revision: SHA },
      result: { publication_state: 'published', result_revision: SHA, check_job_ids: [LOCAL_JOB, REMOTE_JOB] },
    }],
  },
});
const activityTask = () => ({ ...baseTask, id: 'forge-activity-task', title: 'Integration activity', job_evidence: { jobs: [job(REMOTE_JOB, { source_revision: 'not-a-sha' })], reservations: [] }, workflow: { activities: [] } });

function assert(condition, message) { if (!condition) throw new Error(message); }
async function unusedPort() {
  const probe = net.createServer(); probe.listen(0, '127.0.0.1'); await once(probe, 'listening');
  const { port } = probe.address(); await new Promise((done) => probe.close(done)); return port;
}
function json(response, data) {
  response.writeHead(200, { 'Content-Type': 'application/json', 'Cache-Control': 'no-store' });
  response.end(JSON.stringify({ data }));
}
async function fixtureServer() {
  const files = {
    '/': ['web/index.html', 'text/html; charset=utf-8'], '/index.html': ['web/index.html', 'text/html; charset=utf-8'],
    '/app.js': ['web/app.js', 'text/javascript; charset=utf-8'], '/style.css': ['web/style.css', 'text/css; charset=utf-8'],
  };
  const server = createServer(async (request, response) => {
    const url = new URL(request.url, 'http://fixture.invalid');
    if (url.pathname === '/api/v1/me') return json(response, { actor: fixtureActor, csrf_token: 'fixture-csrf' });
    if (url.pathname === '/api/v1/projects') return json(response, { items: [{ id: 'fixture-project', name: 'Fixture project', target_branch: 'main', repository_url: repositoryUrl }] });
    if (url.pathname === '/api/v1/projects/fixture-project/orientation') return json(response, { project: { id: 'fixture-project', policy_revision: 1 }, policy_revision: 1 });
    if (url.pathname === '/api/v1/projects/fixture-project/tasks') return json(response, { items: [{ ...baseTask }], next_cursor: null });
    if (url.pathname === `/api/v1/projects/fixture-project/tasks/${baseTask.id}`) { detailReads += 1; return json(response, subjectTask()); }
    if (url.pathname === '/api/v1/projects/fixture-project/tasks/forge-activity-task') return json(response, activityTask());
    const file = files[url.pathname];
    if (!file) { response.writeHead(404); response.end(); return; }
    response.writeHead(200, { 'Content-Type': file[1], 'Cache-Control': 'no-store' });
    response.end(await readFile(join(root, file[0])));
  });
  server.listen(0, '127.0.0.1'); await once(server, 'listening');
  return server;
}
async function waitFor(fetchUrl, predicate, description) {
  const deadline = Date.now() + 10000;
  while (Date.now() < deadline) {
    try { const response = await fetch(fetchUrl); if (response.ok) { const value = await response.json(); if (predicate(value)) return value; } } catch (_) { /* Chrome is still starting. */ }
    await new Promise((done) => setTimeout(done, 50));
  }
  throw new Error(`Timed out waiting for ${description}`);
}
async function removeProfile(profile) {
  for (let attempt = 0; attempt < 3; attempt += 1) {
    try { await rm(profile, { recursive: true, force: true }); return; }
    catch (error) { if (attempt === 2 || error.code !== 'ENOTEMPTY') throw error; await new Promise((done) => setTimeout(done, 100)); }
  }
}

// What the candidate revision, publication revisions and the local job's source revision render as.
const READ_LINKS = `(() => {
  const describe = (scope) => [...document.querySelectorAll(scope + ' .forge-revision')].map((node) => ({
    text: node.textContent, links: [...node.querySelectorAll('a')].map((a) => ({ text: a.textContent, href: a.href, target: a.target, rel: a.rel })),
  }));
  return { candidate: describe('#workflow-content .workflow-entry dl'), activity: describe('#workflow-content .workflow-entry p'), job: describe('#job-evidence-content') };
})()`;

async function main() {
  const server = await fixtureServer();
  const serverPort = server.address().port, debugPort = await unusedPort();
  const profile = await mkdtemp(join(tmpdir(), 'agent-coordinator-forge-test-'));
  const executable = process.env.CHROME_BIN || (process.platform === 'win32' ? 'C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe' : 'chromium');
  const chrome = spawn(executable, ['--headless=new', '--disable-gpu', '--no-first-run', '--no-default-browser-check', '--disable-background-networking', `--remote-debugging-port=${debugPort}`, `--user-data-dir=${profile}`], { stdio: 'ignore', windowsHide: true });
  let socket, launchError;
  chrome.on('error', (error) => { launchError = error; });
  try {
    const version = await waitFor(`http://127.0.0.1:${debugPort}/json/version`, (value) => Boolean(value.webSocketDebuggerUrl), 'headless Chrome');
    const connect = async (url) => {
      const connection = new WebSocket(url); await once(connection, 'open');
      let nextId = 1; const pending = new Map();
      connection.addEventListener('message', ({ data }) => { const message = JSON.parse(data); const callback = pending.get(message.id); if (callback) { pending.delete(message.id); callback(message); } });
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
    const targets = await waitFor(`http://127.0.0.1:${debugPort}/json/list`, (items) => items.find((item) => item.id === target.targetId)?.webSocketDebuggerUrl, 'headless Chrome page target');
    connection = await connect(targets.find((item) => item.id === target.targetId).webSocketDebuggerUrl);
    socket = connection.connection;
    const { send } = connection;
    const evaluate = async (expression) => {
      const result = await send('Runtime.evaluate', { expression, awaitPromise: true, returnByValue: true });
      if (result.exceptionDetails) throw new Error(`${result.exceptionDetails.exception?.description || result.exceptionDetails.text} IN: ${expression.slice(0, 300)}`);
      return result.result.value;
    };
    const waitPage = async (expression, label) => {
      const deadline = Date.now() + 10000;
      while (Date.now() < deadline) { if (await evaluate(expression)) return; await new Promise((done) => setTimeout(done, 30)); }
      throw new Error(`Timed out waiting for ${label}`);
    };
    await send('Page.enable');
    // The page's 5 s poll runs only when the test calls __tick(), like a poll that fires on demand.
    await send('Page.addScriptToEvaluateOnNewDocument', { source: `
      (() => {
        const realSet = window.setInterval.bind(window), realClear = window.clearInterval.bind(window);
        let next = 1000000; window.__polls = new Map();
        window.setInterval = (callback, ms, ...args) => { if (ms === 5000) { const id = ++next; window.__polls.set(id, callback); return id; } return realSet(callback, ms, ...args); };
        window.clearInterval = (id) => { if (window.__polls.delete(id)) return; realClear(id); };
        window.__tick = () => { for (const callback of [...window.__polls.values()]) callback(); };
      })();
    ` });
    const openSubjectTask = async () => {
      await send('Page.navigate', { url: `http://127.0.0.1:${serverPort}/` });
      await waitPage("document.querySelector('.project-open')", 'fixture project');
      await evaluate("document.querySelector('.project-open').click()");
      await waitPage("document.querySelector('#tasks-list .task-row')", 'fixture queue');
      await evaluate("document.querySelector('#tasks-list .task-row').click()");
      await waitPage("document.querySelector('#workflow-content .workflow-entry dl') && document.querySelector('#job-evidence-content .evidence-entry')", 'fixture detail');
    };
    const expectLinked = (revisions, sha, base, label) => revisions.forEach((revision) => {
      if (revision.text.startsWith(sha)) {
        assert(revision.links.length === 2 && revision.links[0].text === sha && revision.links[1].text === 'checks', `${label}: ${sha} lacks a commit link and a checks link: ${JSON.stringify(revision)}`);
        assert(revision.links[0].href === `${base}/commit/${sha}` && revision.links[1].href === `${base}/commit/${sha}/checks`, `${label}: wrong hrefs ${JSON.stringify(revision.links)}`);
        revision.links.forEach((link) => assert(link.target === '_blank' && link.rel.split(' ').sort().join(' ') === 'noopener noreferrer', `${label}: external link lacks target=_blank and rel=noopener noreferrer: ${JSON.stringify(link)}`));
      }
    });
    const expectPlain = (revisions, label) => revisions.forEach((revision) => assert(revision.links.length === 0, `${label}: unexpected link ${JSON.stringify(revision)}`));

    // GitHub HTTPS and SSH forms link the candidate, the publication revisions and the job source revision.
    for (const url of ['https://github.com/example-org/example-repo.git', 'https://github.com/example-org/example-repo', 'https://github.com/example-org/example-repo/', 'git@github.com:example-org/example-repo.git', 'ssh://git@github.com/example-org/example-repo.git']) {
      repositoryUrl = url; await openSubjectTask();
      const links = await evaluate(READ_LINKS), base = 'https://github.com/example-org/example-repo';
      assert(links.candidate.length === 1 && links.candidate[0].links.length === 2, `${url}: candidate revision is not linked: ${JSON.stringify(links.candidate)}`);
      expectLinked(links.candidate, SHA, base, `${url} candidate`);
      assert(links.activity.length === 3, `${url}: expected intent (2) and result revisions, saw ${links.activity.length}`);
      expectLinked(links.activity, SHA, base, `${url} publication`); expectLinked(links.activity, TARGET_SHA, base, `${url} target`);
      assert(links.activity.every((revision) => revision.links.length === 2), `${url}: a publication revision is not linked`);
      assert(links.job.length === 1 && links.job[0].links.length === 2, `${url}: job source revision is not linked`);
      expectLinked(links.job, SHA, base, `${url} job`);
    }
    // A 64-digit object name links as well.
    candidateRevision = SHA256; repositoryUrl = 'https://github.com/example-org/example-repo.git'; await openSubjectTask();
    expectLinked((await evaluate(READ_LINKS)).candidate, SHA256, 'https://github.com/example-org/example-repo', 'sha-256 candidate');
    assert((await evaluate(READ_LINKS)).candidate[0].links.length === 2, 'A 64-digit revision was not linked.');

    // Unrecognised hosts, credentials and malformed addresses never link, but still show the revision as text.
    candidateRevision = SHA;
    for (const url of ['https://gitlab.com/example-org/example-repo.git', 'git@gitlab.com:example-org/example-repo.git', 'https://github.com.evil.example/example-org/example-repo', 'https://github.com/example-org', 'https://github.com/example-org/example-repo/tree/main', 'https://token@github.com/example-org/example-repo.git', 'https://user:secret@github.com/example-org/example-repo.git', 'ssh://deploy@github.com/example-org/example-repo.git', 'ssh://git:secret@github.com/example-org/example-repo.git', 'https://github.com/example-org/example-repo.git?token=x', 'https://github.com/../example-repo', 'http://github.com/example-org/example-repo.git', 'not a url', '']) {
      repositoryUrl = url; await openSubjectTask();
      const links = await evaluate(READ_LINKS);
      expectPlain([...links.candidate, ...links.activity, ...links.job], `repository ${JSON.stringify(url)}`);
      assert(links.candidate.length === 1 && links.candidate[0].text === SHA, `repository ${JSON.stringify(url)}: candidate revision text was lost`);
      assert(await evaluate("document.querySelectorAll('#workflow-content a, #job-evidence-content a').length") === 0, `repository ${JSON.stringify(url)}: an anchor was rendered`);
      assert(!(await evaluate("document.querySelector('#workflow-content').innerHTML + document.querySelector('#job-evidence-content').innerHTML")).includes('secret'), 'A credential reached the task detail.');
    }
    // Malformed revisions on a GitHub repository: too short, too long, non-hex, padded and injected values.
    repositoryUrl = 'https://github.com/example-org/example-repo.git';
    for (const bad of ['0123456789abcdef', SHA.slice(0, 39), `${SHA}0`, `${'z'.repeat(40)}`, ` ${SHA}`, `${SHA}/../x`, `${SHA}?x=1`, 'HEAD', `${SHA.slice(0, 39)}\n`]) {
      candidateRevision = bad; await openSubjectTask();
      const links = await evaluate(READ_LINKS);
      expectPlain([...links.candidate, ...links.activity.filter((revision) => revision.text === bad)], `revision ${JSON.stringify(bad)}`);
      assert(links.candidate.length === 1 && links.candidate[0].links.length === 0, `revision ${JSON.stringify(bad)} was linked`);
    }
    candidateRevision = SHA;

    // Check-job navigation: a job in this task's evidence moves focus to its entry...
    await openSubjectTask();
    assert(await evaluate("[...document.querySelectorAll('#workflow-content button.forge-job')].map((b) => b.textContent).join()") === `${LOCAL_JOB},${REMOTE_JOB}`, 'Check job IDs are not buttons.');
    assert(await evaluate("document.querySelector('#workflow-content button.forge-job').type") === 'button' && await evaluate("document.querySelector('#workflow-content button.forge-job').getAttribute('aria-label')") === `Show evidence for check job ${LOCAL_JOB}`, 'Check job button lacks its type or label.');
    await evaluate("document.querySelector('#workflow-content button.forge-job').focus()");
    await send('Input.dispatchKeyEvent', { type: 'keyDown', key: 'Enter', code: 'Enter', windowsVirtualKeyCode: 13, text: '\r' });
    await send('Input.dispatchKeyEvent', { type: 'keyUp', key: 'Enter', code: 'Enter', windowsVirtualKeyCode: 13 });
    await waitPage(`document.activeElement.id === 'job-evidence-${LOCAL_JOB}'`, 'focus on the local job entry (keyboard activation)');
    assert(await evaluate("document.activeElement.closest('.job-evidence-card') !== null && document.activeElement.textContent.includes('Check job-local-1')"), 'Focus landed on the wrong job entry.');
    assert(await evaluate("(() => { const r = document.activeElement.getBoundingClientRect(); return r.bottom > 0 && r.top < window.innerHeight; })()"), 'The job entry was not scrolled into view.');
    assert(await evaluate("document.querySelector('#task-detail-heading').textContent") === baseTask.title, 'Focusing a local job navigated away.');
    assert(await evaluate("getComputedStyle(document.activeElement).outlineStyle !== 'none'"), 'The focused job entry has no visible focus indicator.');
    // ...and a job owned by the activity task opens that task and lands on its entry.
    await evaluate("[...document.querySelectorAll('#workflow-content button.forge-job')].find((b) => b.textContent === 'job-remote-2').click()");
    await waitPage("document.querySelector('#task-detail-heading').textContent === 'Integration activity'", 'activity task');
    await waitPage(`document.activeElement.id === 'job-evidence-${REMOTE_JOB}'`, 'focus on the activity task job entry');
    // Non-SHA source revisions in the activity task's job stay plain text.
    expectPlain((await evaluate(READ_LINKS)).job, 'activity job');

    // Phone width: no horizontal overflow with the controls shown.
    await openSubjectTask();
    await send('Emulation.setDeviceMetricsOverride', { width: 375, height: 844, deviceScaleFactor: 1, mobile: true });
    assert(await evaluate('document.documentElement.scrollWidth <= window.innerWidth'), 'Task detail overflows at phone width with forge links.');
    await send('Emulation.clearDeviceMetricsOverride');

    // Re-rendering identical data leaves the sections untouched (no DOM mutation).
    await evaluate(`window.__mutations = 0; window.__observer = new MutationObserver((records) => { window.__mutations += records.length; });
      window.__observer.observe(document.querySelector('#workflow-content'), { subtree: true, childList: true, attributes: true, characterData: true });
      window.__observer.observe(document.querySelector('#job-evidence-content'), { subtree: true, childList: true, attributes: true, characterData: true });`);
    const reads = detailReads;
    await evaluate('window.__tick()');
    const deadline = Date.now() + 5000;
    while (detailReads === reads && Date.now() < deadline) await new Promise((done) => setTimeout(done, 30));
    assert(detailReads > reads, 'The poll did not re-read the task detail.');
    await new Promise((done) => setTimeout(done, 300));
    assert(await evaluate('window.__mutations') === 0, 'A refresh with unchanged data mutated the forge-linked sections.');
    console.log('PASS: headless Chrome verified forge links for GitHub HTTPS/SSH, plain text for other hosts, credentials and malformed revisions, new-tab link attributes, and check-job focus and navigation.');
  } finally {
    socket?.close();
    if (chrome.pid && chrome.exitCode === null && process.platform === 'win32') {
      const taskkill = spawn('taskkill', ['/PID', String(chrome.pid), '/T', '/F'], { stdio: 'ignore', windowsHide: true });
      await once(taskkill, 'exit'); await once(chrome, 'exit');
    }
    if (chrome.pid && chrome.exitCode === null) { const exited = once(chrome, 'exit'); chrome.kill('SIGTERM'); await exited; }
    await new Promise((done) => server.close(done));
    await removeProfile(profile);
    if (launchError) throw launchError;
  }
}

main().catch((error) => { console.error(`FAIL: ${error.message}`); process.exitCode = 1; });
