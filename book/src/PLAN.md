# Agent coordination service: discovery and implementation plan

Status: preserved release design and decision record. Product choices were
resolved with the operator, and the body records the intended first release and
the implementation sequence as it was planned. Consult
[implementation status](docs/implementation-status.md) for current behavior and
accepted evidence; proposed names and historical milestone language below are
not a claim that a particular interface is available.

Revised 2026-09-09 after reviewing SithBit, Submission, their memory, and local
and global hooks. See [the evidence report](docs/repository-review.md).

## Goal

Allow autonomous agents on different workstations to coordinate work on a shared
project without independently selecting the same task. Preserve task outcomes,
handoffs, and useful lessons so subsequent agents can work from shared context.
Remain independent of agent vendors, model providers, and Git hosting providers.

Planning and clarification are complete. The operator subsequently authorized
implementation. See [implementation status](docs/implementation-status.md) for
working service/client behavior and remaining milestones; the full design below
continues to define the release target. No production deployment has occurred.

## Requirements supplied by the operator

- Agents start in a project directory associated with a project name and usually
  a Git repository.
- One service instance must coordinate **multiple projects simultaneously**,
  with agents working on different projects at the same time.
- Workstations connect over the **public internet using HTTPS**.
- Install the server as a native Linux service managed by systemd, behind an
  HTTPS reverse proxy.
- The service is the **authoritative record** for tasks, handoffs, and lessons,
  with Markdown import and export.
- **Every authenticated person and agent has access to every project.**
  There are no project-specific access grants in the first release.
- People use local operator accounts with passwords. Agents use separately
  issued, revocable API tokens.
- Agents can create and claim tasks autonomously; each project configures
  whether completion requires review.
- An expired task can be recovered by another agent after checking saved work
  and still-running jobs. Projects can instead require manual recovery.
- Code tasks count as complete and unblock dependencies only after required
  review, integration into the target branch, and validation of the integrated
  result.
- The service coordinates existing harnesses through API, CLI, and optional
  hooks. Local runners report jobs; remote agent launch/supervision is out of
  scope for the first release.
- The CLI and local job reporter support Linux and native Windows.
- The first release includes the HTTP API, web dashboard, and CLI with
  human-readable and JSON output. The MCP endpoint was implemented as item 6.1;
  TUI remains deferred.
- Each project can require independent agent review, human review, or both;
  the default review mode is independent agent review.
- Each project can allow agents to integrate approved, validated work
  automatically or require human authorization for integration.
- Each project may authorize agents to change binding project rules without
  human approval, in addition to publishing and correcting shared lessons.
- Store bounded log/report uploads in the service and source checkpoints in Git
  remotes; also support artifact links. Source-worktree bundles are not required.
- Test the first release for up to 20 projects, 50 simultaneous agent sessions,
  and 100,000 historical tasks across the service.
- Provide hourly backups, retaining 24 hourly and 30 daily copies, documented
  off-server copying, and a one-hour restore target.
- The production host is undecided. Use Ubuntu 24.04 LTS, x86_64, 2 CPU cores,
  and 4 GB RAM as the engineering test baseline, not a selected production host.
- Existing orientation and work records include AGENTS.md / CLAUDE.md,
  HANDOFF.md, BACKLOG.md, and DURABLE-RECORD.md. The request also mentions
  BACKOFF.md; support it as an additional configurable import filename without
  requiring it to mean the same thing as BACKLOG.md.
- Agents discover work, obtain ownership, report progress and health, and record
  completion results through the service.
- Other agents can distinguish active long-running work from potentially
  abandoned work and continue appropriately.
- Repository instructions should be short. An initial service connection should
  supply the detailed workflow and API usage instructions.
- Authentication is required. Unauthenticated clients receive safe instructions
  to show their human operator for configuring access.
- Operators need a web interface, CLI, and/or TUI to inspect projects, tasks,
  status, and outcomes.
- Preferred stack: Rust, Axum, SQLite; vanilla JavaScript and modern CSS for a
  web interface, with Alpine.js if it simplifies the implementation.
- Confirmed during discovery: separate Git worktrees per implementation task,
  with one integration step at a time into the target branch in the first release.

## Workspace observations

- The project directory was empty when discovery began.
- It was not an initialized Git repository.
- No applicable AGENTS.md or CLAUDE.md was found in the directory or its parents.
- Reference-project review found an existing phased, parallel workflow in
  SithBit and a simpler migration/branch workflow in Submission. The service
  needs to represent both without requiring a particular agent harness.

## Proposed foundation

These engineering choices implement the confirmed requirements. Tunable defaults
are specified in the linked contracts and acceptance plan.

1. A central HTTP service owns coordination state. Workstations use an API;
   they never access a shared SQLite database file directly.
2. Provide a versioned JSON API with complete request/response examples and
   concise, versioned agent instructions. A CLI can expose the same operations
   in both human-readable and JSON formats. MCP support was added as backlog
   item 6.1; TUI remains deferred; core functionality does not require either.
3. Claiming a task atomically verifies eligibility and records ownership in one
   database transaction. Listing work does not reserve it. Both “claim this
   task” and “claim next eligible task” can use the same eligibility rules.
4. Separate the durable task from each execution attempt. Keep previous
   ownership, checkpoints, failures, and results when a new attempt starts.
5. An attempt has an authenticated owner, a session identity, an expiring lease,
   and an ownership generation. Heartbeats renew only a still-valid lease.
   Updates from an expired or superseded attempt cannot change current work.
6. Use service time for lease decisions. Evaluate validity on every ownership
   operation; correctness must not depend on a background cleanup timer.
7. Distinguish a health heartbeat from evidence of progress. A running helper
   process alone must not be interpreted as proof that an agent is advancing.
8. Retryable mutations use idempotency keys. An interrupted response must not
   cause duplicate task creation, an extra claim, or repeated completion records.
9. Save completion, handoff, linked lessons, and the task transition together.
   Do not lose context between independently submitted updates.
10. Record Git branch, commit, and optional review URL as evidence. The service
    must not claim it can stop an offline process or guarantee exactly-once Git
    pushes, deployments, or other actions outside its own database.
11. Keep searchable lessons with provenance and revision history. Distinguish
    agent observations from policy adopted by a human or an authorized agent.
    Relevant context should fit a bounded response instead of dumping every
    historical record.
12. Return useful next actions and recovery instructions with API responses,
    particularly for authentication failures, conflicts, and expired leases.
13. Treat implementation, verification, review, integration, and deployment as
    separately evidenced activities. Task completion requirements are explicit;
    a local test pass or a branch commit must not imply integration or deployment.
14. Bind shared records to stable project IDs and source revisions, independent
    of workstation paths or vendor-specific memory locations. Preserve scoped
    decisions and completed/rejected-task records through archival and import.

The proposed ownership and recovery contract is developed further in
[docs/coordination-contract.md](docs/coordination-contract.md). It deliberately
defines the selected agent-driven recovery and integrated-completion rules.
Lease timing and reviewer separation are specified as engineering defaults there.
Password accounts, separate agent API tokens, and project-configurable agent or
human review are confirmed.

The design-stage first-connection flow and short repository snippet are in
[docs/onboarding-contract.md](docs/onboarding-contract.md). See the
[current CLI guide](docs/CLI.md) for implemented commands.

The engineering draft in [docs/workflow-spec.md](docs/workflow-spec.md) specifies
task lifecycles, derived work statuses, activity eligibility, and completion
transactions, consistent with the confirmed product decisions.
The proposed wire interface is in [docs/api-contract.md](docs/api-contract.md),
including agent sessions, claim/renewal examples, and actionable error responses.
Persistence constraints, operation permissions, and release checks are in
[docs/implementation-spec.md](docs/implementation-spec.md).

## Decision log

| Decision | Proposed default | Status |
| --- | --- | --- |
| Project concurrency | Multiple projects active simultaneously in one service instance | **Confirmed by operator, 2026-09-09** |
| Project access | Every authenticated person and agent can access every project | **Confirmed by operator, 2026-09-09** |
| Network exposure | Public internet, using HTTPS | **Confirmed by operator, 2026-09-09** |
| Authoritative records | Service owns tasks, handoffs, lessons; import Markdown and export snapshots | **Confirmed by operator, 2026-09-09** |
| Authentication method | Local password accounts and separately issued, revocable agent API tokens | **Confirmed by operator, 2026-09-09** |
| Agent autonomy | Agents create and claim tasks; each project configures required review | **Confirmed by operator, 2026-09-09** |
| Working-directory arrangement | Separate worktree per implementation task; serialize integration into the target branch | **Confirmed by operator, 2026-09-09** |
| Coordination boundary | Coordinate existing harnesses through API, CLI, optional hooks; local runners report jobs | **Confirmed by operator, 2026-09-09** |
| Server installation | Native Linux service managed by systemd, behind an HTTPS reverse proxy | **Confirmed by operator, 2026-09-09** |
| Expired-task recovery | Agent-driven recovery after checking prior work/jobs; manual mode configurable per project | **Confirmed by operator, 2026-09-09** |
| Overall completion | Required review, integration into target branch, and validation of integrated result | **Confirmed by operator, 2026-09-09** |
| Client platforms | Linux and native Windows | **Confirmed by operator, 2026-09-09** |
| Initial interfaces | HTTP API, web dashboard, CLI with JSON output, and MCP endpoint (item 6.1); defer TUI | **Confirmed by operator, 2026-09-09** |
| Review authority | Per-project choice of independent agent, human, or both; independent agent default | **Confirmed by operator, 2026-09-09** |
| Integration authority | Agents integrate when policy/review/checks allow; projects may require human authorization | **Confirmed by operator, 2026-09-09** |
| Knowledge autonomy | Agents maintain lessons; projects may also delegate binding-rule changes without human approval | **Confirmed by operator, 2026-09-09** |
| Artifact storage | Bounded service uploads for logs/reports, Git remotes for source checkpoints, optional artifact links | **Confirmed by operator, 2026-09-09** |
| Initial operating size | 20 projects, 50 simultaneous agent sessions, 100,000 historical tasks | **Confirmed by operator, 2026-09-09** |
| Server baseline | Ubuntu 24.04 LTS, x86_64, 2 CPU cores, 4 GB RAM | **Engineering default; operator's production host is undecided** |
| Backup/recovery targets | Hourly backups; 24 hourly and 30 daily copies; documented off-server copying; one-hour restore target | **Confirmed by operator, 2026-09-09** |

Repository evidence informed these choices. A policy or permission recorded for
a past task in a reference project is not a new permission for this service or
a future task.

## Multiple-project operation

Confirmed scope: a single deployment can coordinate SithBit, Submission, and
other projects concurrently. All authenticated users and agents can access all
projects. Projects separate work context, not audiences. Administrative actions
and attempt ownership follow the operation-level permission matrix in
[implementation-spec.md](docs/implementation-spec.md).

- Every task, attempt, decision, evidence record, and project-specific lesson
  belongs to an explicit project. Claims use the repository's project binding
  or an explicit project argument; there is no service-wide "current project".
- Project configuration controls its workflow and review requirements. A
  credential works across all projects. Each operation checks authentication,
  any required role/ownership, and consistency of its project/record references.
- The operator dashboard shows an overview of all projects after login and
  filters into each project's tasks, active work, decisions, and knowledge.
- Integrations into different repository targets can proceed concurrently.
  A blocked task or resource in one project does not block unrelated projects.
  If projects intentionally share a repository target or physical resource,
  they must share its canonical reservation identity rather than evade its
  concurrency limit through different project names.
- Knowledge retains its source-project association for relevance and provenance,
  while every authenticated caller can read across projects. A proposed common
  collection holds reusable lessons with source links; it is not an access boundary.
- Work and long-running checks take place outside database transactions.
  Coordination transactions stay short so an active project does not hold the
  service's database transaction open for the duration of its work.

## Revisions based on the repository review

The evidence IDs below refer to [the review report](docs/repository-review.md).

1. **Phases and dependencies belong in the first-release model (E1).** A task
   may have child tasks and explicit dependencies. Model implementation, review,
   and integration as typed work when separately owned; a simple task need not
   use all types. One primary attempt owns each task, so independent reviewers
   use their own linked review tasks. Parent completion depends on required
   child outcomes and its own acceptance criteria. Prevent dependency cycles.
2. **Task readiness needs an evidence check (E2, E3).** Record the candidate's
   source, observed repository revision, acceptance criteria, and whether it is
   still undone. After claiming, check this against the actual checkout before
   changing code. If work is already delivered, reconcile the record with the
   commit and acceptance evidence through an authorized transition. Keep stable
   duplicate, supersession, rejection, and closure links; do not reuse old IDs.
3. **External jobs have their own records (E6).** A test run outlives an observer
   or agent turn. Store job/run ID, host and checkout identity, producer status,
   last observation, start/end, exit status, and artifact references. Separate
   running, succeeded, failed, interrupted, and unknown. An observation timeout
   cannot trigger a restart. A job heartbeat does not automatically renew an
   implementation lease or prove useful agent progress.
4. **Verification evidence describes an exact input (E1, E6, E7).** Record
   commit/tree or dirty-snapshot digest, gate-definition version, environment,
   required checks, actual checks, skipped checks with reasons, terminal status,
   and relevant log/result digests. A subset pass, advisory formatter, or killed
   run cannot satisfy a full verification requirement. Changed inputs invalidate
   applicability, not the historical record. A path on another workstation is
   labeled local-only unless a separately accessible artifact is published.
5. **Resource admission complements worktree isolation (E7).** Support named
   exclusive resources and bounded local build slots with workstation, checkout,
   or shared project/environment scope. Client-declared dependency/conflict
   information determines which work interferes; file-name disjointness alone
   is not sufficient. Atomic acquisition prevents partial reservation deadlocks.
   An expired task lease does not prove an external job released a resource.
6. **Decisions and blockers are structured (E8, E9).** Distinguish dependency,
   missing capability, human decision, upstream release, verification failure,
   and permission blockers. Preserve rationale, affected tasks, reopening
   condition, observation time, and answer. During unattended work, park a
   blocked task and continue eligible work. An empty eligible queue is a normal
   stop, with a reason and a suggested next check, not a reason to invent work.
7. **Separate knowledge from standing policy (E4, E5, E9).** Store practical
   lessons, validated facts, decisions, rejected alternatives, and current
   checkpoints as distinct record kinds. Include scope, provenance, applicable
   versions, supersession, and reopening conditions. Start with full-text search
   and tags; optional semantic retrieval must not require one model vendor.
   Promotion into binding policy requires the appropriate authority. Repository
   instructions continue to express local rules; imported memory cannot silently
   override them. Track useful/corrected lessons to improve retrieval over time.
8. **Migration must reconcile, not just copy Markdown (E2–E5).** Provide a
   previewed import from configurable paths, including SAVERS.md, nested handoffs,
   archives, and multiple memory directories. Record unresolved links and
   conflicting statements. Preserve original source plus proposed structured
   records. Historical, struck, consumed, or rejected entries are not ready
   tasks. Stable import identities include source context, not just an item
   number. Re-import cannot reopen closed work or overwrite newer service state.
9. **Hooks are optional local adapters (E6–E8).** Define a vendor-neutral
   lifecycle: connect/resume, claim, checkpoint, observe a job, submit, release,
   and disconnect. Repeated startup/compaction reconnects to an existing attempt;
   it does not claim twice. A turn-end hook does not declare completion or kill
   work. Keep formatters and command guards local. Report actual adapter
   capabilities/version rather than assuming an installed script is active.
   Imported hook text is never executed by the service. Recovery instructions
   must respect applicable local rules and expose a policy conflict rather than
   repeatedly recommend an action that the caller's harness refuses.
10. **Autonomy has scope and stop conditions (E8, E9).** Keep a durable record of
    what the operator authorized, for which task/action/environment/revision, and
    any limits. A service login or task claim does not confer deployment or data
    mutation authority. Preserve valid grants without repeated questions. Retry
    limits count by stable finding/defect class across attempt or phase renaming;
    time/work limits and escalation thresholds are configurable project policy.

## Proposed minimum data model

Relational constraints, indexes, permissions, transactions, and retention defaults
are specified in [implementation-spec.md](docs/implementation-spec.md).

| Record | Purpose |
| --- | --- |
| Project and repository binding | Stable identity, repository aliases, integration target, local configuration reference |
| Principal, credential, workstation, session | Authentication, operation roles, ownership identity, adapter capabilities; all authenticated callers can access all projects |
| Task, task revision, dependency | Outcome, acceptance criteria, priority, type, parent, readiness, current state |
| Attempt and lease | One effort's owner, deadline, generation, checkpoints, outcome |
| Checkout | Host-local path, worktree/branch, base revision, snapshot identity; paths are metadata, not remote access |
| Resource and reservation | Scoped exclusive/capacity constraints, holder, validity, recovery state |
| External job and evidence | Producer identity, observations, terminal result, input revision, coverage, artifact accessibility |
| Review finding and integration result | Candidate revision, independent findings, resolution, merge/base/result revisions |
| Decision, authorization, blocker | Rationale, action scope, conditions, answer/actor, affected tasks, reopening signal |
| Knowledge record and revision | Lesson/fact/policy/decision/rejection, provenance, scope, applicability, supersession |
| Source import and source mapping | Original content identity, proposed classification, reconciliation and unresolved links |
| Event and idempotency receipt | Auditable transitions and safe retries; mutable projections committed with their events |

Avoid a mandatory custom workflow language, embeddings service, or agent-vendor
SDK. These are conventional records exposed through one API and CLI.

## Proposed agent workflow

1. Read the short repository connection instructions and applicable local rules.
2. Connect using locally configured credentials. Without authentication, receive
   public setup help suitable for showing the human; expose no project records.
3. Resolve the stable project binding and reconnect to an existing session or
   attempt if resuming. Report checkout state and capabilities; do not overwrite
   a dirty checkout or advance one used by an active job.
4. Receive a bounded orientation packet: active work, pending decisions,
   applicable policy, candidate tasks, relevant lessons, and exact next actions.
5. Atomically claim eligible work. If none is available, receive structured
   reasons such as missing capability, pending decision, or work owned elsewhere.
6. For implementation, prepare its separate worktree. Confirm the task is still
   undone on the relevant revision, acquire required resources, and record the
   initial checkpoint before making changes.
7. Report progress, blockers, and job observations. Renew ownership within the
   lease contract. If a tool observer dies, reconnect to the known job before
   considering a new run. If authority expires, preserve recovery information
   without changing the new owner's work.
8. Submit the outcome, exact-revision verification, handoff, and new lessons.
   Required review is separate work with its own owner and evidence.
9. An authorized integration task reserves the target branch, validates the
   candidate and current target, integrates, and records validation of the
   resulting revision. A merge conflict or incomplete validation remains visible.
10. Update task/parent state according to the completion policy. Preserve
    historical records and generate optional Markdown snapshots and a run summary.

## Operator interface proposal

The operator selected both a CLI and a web interface in the first release. The
CLI supports human-readable and JSON output on Linux and native Windows; the web
interface uses vanilla JavaScript and CSS, adding Alpine.js only where it removes
complexity. MCP was implemented as backlog item 6.1; a TUI remains deferred.

The browser should make these questions easy to answer:

- Which projects and tasks are ready, active, blocked, under review, or complete?
- Who owns this attempt, where is its worktree, and when did it last report
  health versus actual progress?
- Is its long-running check still running, unknown, interrupted, or finished?
  What exact code and checks does the evidence cover?
- What decision, permission, capability, or shared resource is blocking progress?
- What was integrated, and what remains only on a branch or workstation?
- Which lessons and rejected approaches apply here, and what supports them?
- Which credentials and workstation registrations are active, and what operations
  may each role perform?

Decision prompts must explain the tradeoff without requiring prior session
shorthand. The service should expose actionable failures and retain answers.

## Implementation readiness

No blocking product questions remain. [docs/release-scope.md](docs/release-scope.md)
records engineering defaults, deferred features, and readiness evidence. The
production hostname, final host, off-server backup destination, and credentials
are installation inputs, and are not required to build and test the service.

The planning output includes the architecture and data model, transaction and
authority rules, workflow/API contracts, native-client behavior, permissions,
migration, shared knowledge, artifact handling, and measurable release checks.
Generated OpenAPI schemas, executable examples, migrations, and automated tests
belong to the implementation milestones below; the documents do not claim they
already exist.

## Proposed implementation milestones

Implement against the linked contracts. Each milestone leaves an executable
slice with the stated evidence and matching generated API/CLI documentation.

1. **Service, persistence, and access:** Rust/Axum server, SQLite migrations,
   principal/operation permissions, public authentication help, authenticated
   bootstrap, HTTPS deployment configuration, structured logs, and health/readiness.
   Demonstrate that unauthenticated callers cannot read project data, and that
   a newly authenticated workstation can access all projects.
2. **Ownership and task state:** typed tasks/dependencies, attempts, conditional
   state transitions, leases, events, idempotency, and a minimal JSON-capable CLI.
   Demonstrate competing claims, dependency cycles/refusals, lost responses,
   expiry, revocation, and the selected recovery policy.
3. **Worktrees, jobs, and integration:** checkout registration, named resource
   admission, external-job observations, verification receipts, review records,
   and serialized integration. Demonstrate observer loss without duplicate work
   and reject completion evidence for changed inputs.
4. **Shared context and migration:** knowledge revisions/search, decision queue,
   authorization scopes, provenance-aware Markdown/memory import and export,
   and bounded orientation/task packets. Exercise representative SithBit and
   Submission fixtures, including closed items and conflicting historical records.
5. **Operator and agent experience:** browser views, full CLI workflows,
   service-delivered instructions, short AGENTS.md/CLAUDE.md snippets, and optional
   lifecycle adapters. Verify that a small agent can complete the documented
   workflow without vendor-specific context or credentials in its prompt.
6. **Operational release:** packaging for selected operating systems,
   backup/restore and authority invalidation, restart/disconnection scenarios,
   appropriate load tests, and a two-workstation end-to-end acceptance exercise.
   Check real behavior against every accepted requirement before release.

## Candidate acceptance scenarios

- Simultaneous eligible claims yield exactly one current owner for a task.
- A valid heartbeat extends the owning attempt; another session cannot renew it.
- A late heartbeat or completion cannot revive an expired/superseded attempt.
- Recovery preserves previous checkpoints and applies the chosen takeover policy.
- Repeating a request after a lost response returns the same logical outcome;
  replaying an old claim never grants fresh authority.
- A failed completion transaction leaves no partial result or state transition.
- Unauthorized clients see setup help without project or credential disclosure.
- Authentication and applicable operation/ownership checks apply to every private
  read/write surface; no project allow-list is required after authentication.
- SithBit and Submission can each have active claims and jobs at the same time;
  an integration reservation in one does not block an unrelated target in the other.
- An authenticated caller can deliberately select either project and read its
  tasks, lessons, events, search results, imports, and permitted artifacts.
- A task operation cannot attach another project's attempt or evidence by
  accident; cross-project sharing uses an explicit authorized operation.
- An authenticated operator sees both projects in the overview, while each agent's
  next-task request stays bound to its selected project.
- A new agent can follow only the repository snippet and returned instructions
  to claim, checkpoint, complete, and retrieve relevant lessons.
- Restarting the service preserves coordination state. Restoring an older
  database invalidates old live sessions/leases before accepting work.
- Stale or disconnected agents are given an explicit stop/reconcile response;
  the service does not imply control over their local processes.
- A repeated startup/resume/compaction event reconnects without a second claim.
- An ended observer/turn does not turn a live test run into a failed task or
  launch another copy. Old completion markers cannot identify a new run.
- A test of an uncommitted snapshot is not attributed to a later commit without
  verified equivalence. Skipped or missing checks cannot satisfy required coverage.
- Two tasks on separate worktrees can proceed independently, while integrations
  into the same project target branch serialize.
- Reservations distinguish workstation-local build slots from a shared test
  environment. Loss of a task lease does not assert that a process has stopped.
- Parent tasks and downstream tasks remain incomplete/unready until their
  declared acceptance requirements and dependency outcomes are satisfied.
- A closed task stays closed across archive/export/re-import; repeated numeric
  labels in different source contexts are not conflated.
- Import can flag a historical branch-only handoff beside later merge evidence,
  retaining both with their dates/revisions and no new implied merge permission.
- A renamed checkout can retrieve the same project knowledge, while broken
  memory links and conflicting versions are reported rather than discarded.
- Pending decisions survive agent sessions; an answer unblocks only applicable
  work and cannot be recorded as a human authorization by an ordinary agent.
- A repeated defect class reaches its configured escalation limit even when
  the agent renames or splits the task. Unrelated ready tasks remain eligible.
- Imported hooks or lessons cannot execute commands, expand credentials' access,
  or automatically promote themselves to trusted policy.

## Technical references checked during discovery

- [SQLite transactions](https://www.sqlite.org/lang_transaction.html): SQLite
  supports one writer at a time; short write transactions can serialize claims.
- [SQLite WAL](https://www.sqlite.org/wal.html): WAL permits readers alongside a
  writer and requires database access on one host, not a network filesystem.
- [Axum documentation](https://docs.rs/axum/latest/axum/): official framework
  documentation for the proposed Rust HTTP implementation.

Dependency versions and security advisories must be checked when implementation
begins; this discovery document does not pin dependencies.

## Additional release sequencing — 2026-09-09

The operator requested an MCP server endpoint as backlog item **6.1**, followed by
consolidating documentation and READMEs into an **mdBook project, item 6.2**.
Both follow Linux acceptance and precede the operator-initiated native Windows
workstation acceptance at **item 7**. Windows CI remains part of implementation
validation and does not replace that physical-workstation exercise. The main
agent reviews each completed backlog item before proceeding to the next.
