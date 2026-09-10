# Proposed coordination contract

Status: implementation design, supporting [the plan](../PLAN.md). No runtime
implementation exists yet. This document defines correctness rules for the
selected policies and identifies tunable engineering defaults.

Revised after [the repository/hook review](repository-review.md). The operator
has confirmed separate worktrees per implementation task and one integration
step at a time into the target branch for the first release, and simultaneous
support for multiple projects in one service instance.

Agent-driven recovery after checking saved work and running jobs is confirmed,
with a per-project manual alternative. Code-task completion requires applicable
review, target-branch integration, and validation of the integrated result.

## Project boundaries

Project identity is explicit in claims, sessions' work context, and every
project-owned record. **All authenticated people and agents can access all
projects**, as confirmed by the operator. There are no project-specific grants.
An operation checks authentication, applicable operation/ownership permissions,
and the project membership of referenced records. Database constraints and
service checks reject mismatched task/attempt/evidence relationships. Search,
events, imports, exports, artifacts, and aggregate views follow the same rules.

An agent working on project A cannot change its context accidentally because
another agent selected project B; there is no global active-project setting.
The same principal can deliberately select any project. After authentication,
the dashboard can aggregate all projects. Project context prevents accidental
mixing of work; it is not a visibility restriction between authenticated users.

Claims and jobs in different projects can be active simultaneously. Integration
and resource reservations block only their canonical resource scope. Projects
sharing the same repository target or physical resource must resolve it to a
common reservation identity. Cross-project knowledge retrieval preserves source
provenance and is available to every authenticated caller. Access to projects
does not reveal credential secrets or confer ownership of another agent's attempt.

## Separate work, attempts, and authority

A **task** describes an intended outcome and its acceptance criteria. It belongs
to a stable project ID; a project display name or local checkout path is not
sufficient identity. Repository aliases can help discover a configured project,
but an ambiguous match must not silently select one.

An **attempt** records one agent session's effort on that task: owner, workstation
label, start time, checkpoints, branch/commit evidence, and eventual outcome.
An expired attempt remains part of history when another attempt takes over.

Tasks can be linked to a parent objective and prerequisites. Review and
integration may be separate typed tasks with their own attempts; independent
reviewers do not borrow an implementer's ownership grant. Dependency edges must
be acyclic and cannot change in a way that silently makes active work eligible
under a different set of prerequisites. A parent is complete only after its
required children and its own acceptance criteria are satisfied.

A **lease** grants temporary authority over an attempt. Its deadline is set by
the service. An opaque attempt ID and an increasing ownership generation identify
the grant; neither substitutes for authentication. Session authorization must
prevent two sessions using the same workstation credential from silently acting
as one another. The credential/session mechanism is specified in
[api-contract.md](api-contract.md).

At most one current attempt can own a task. Task state and current-attempt linkage
must agree within the same transaction. Generic task edits cannot bypass this
rule or overwrite an active attempt's ownership.

## Claim and update rules

1. A candidate list is informational and may become stale immediately.
2. A claim transaction checks authentication, authorization, task eligibility,
   dependency state, and any configured concurrency restrictions. It creates
   one attempt, advances the ownership generation, and records the lease.
3. A competing claim returns a conflict or chooses another eligible task. It
   never receives authority over the existing attempt.
4. Every ownership-dependent mutation verifies the current attempt, generation,
   authenticated session, allowed transition, and unexpired lease together with
   the mutation. Server time must be sampled after acquiring the write
   transaction, so lock-wait time cannot make an expired lease appear valid.
5. A lease is valid only while protected coordinator time sampled under the
   writer lock is less than `expires_at`. Equality is expired. The response's
   observational wall-clock `server_time` is not an authority clock.
   Renewal is allowed only before expiry. Retrying a renewal cannot resurrect
   an expired attempt.
6. Terminal attempts reject fresh mutations. The system preserves their results
   and checkpoints as history. A late agent may need a separate recovery-note
   operation, if selected for the release; such a note cannot complete current
   work, extend a lease, or unblock dependent tasks.
7. Submission saves the result, handoff, referenced/new lessons, attempt outcome,
   and chosen task transition in one transaction. An implementation submission
   advances to required review or integration; it cannot mark the code task
   done. Overall completion requires the configured review, integration, and
   validation of the integrated result. Non-code tasks use their applicable
   acceptance requirements.

Task revisions should protect editable descriptions and acceptance criteria
against lost updates independently of attempt generations. If acceptance criteria
change during work, record which revision the attempt used and require explicit
reconciliation before accepting completion against changed criteria.

## Worktree and integration contract

An implementation task records its own worktree, branch, base revision, and
workstation identity before code changes. The local path is descriptive metadata;
another workstation cannot assume it can read that path. Registration alone does
not prove isolation: the client must resolve the checkout/worktree identity and
reject accidental reuse by another active implementation task. An existing dirty
checkout is never silently reset, stashed, or swept into a new task.

Integration is a separately owned operation. Acquire an exclusive resource keyed
by stable project/repository binding and target branch. Check current candidate
revision, target revision, required review, and action authorization; record the
base and result revision and validation of the integrated result. A moved target
requires reconciliation and validation of the new result. A branch-only result
cannot satisfy an integration dependency merely by being labeled "done".

Keep the integration reservation until its outcome is known. If the coordinator
cannot tell whether an interrupted publish succeeded, report an uncertain result
and reconcile the remote revision before permitting another conflicting action.
The coordinator cannot fence arbitrary Git commands; actual branch updates still
need the Git-side concurrency check and compliant local workflow. Remote hosting
APIs are optional, so the core stores who reported/verified each revision and how,
rather than presenting client-reported merges as independently verified facts.

### Avoid circular completion requirements

A submitted implementation and a completed deliverable are separate facts. If
overall completion includes review and integration, those activities must become
eligible from the submitted candidate and applicable review evidence. They
cannot wait for the implementation task's overall `done` state: that task is
itself waiting for their results.

Ordinary task dependencies mean the prerequisite's required outcome is complete.
Workflow activities instead reference an immutable submission ID and candidate
revision. A review accepts or rejects that submission; integration consumes its
accepted evidence. These are explicit workflow relationships, not an exception
that silently treats unfinished prerequisites as done.

If the candidate changes, create a new submission. Previous reviews and checks
remain historical and do not automatically apply to it. The readiness check
and integration transaction must agree on the current submission, required
policy revision, candidate revision, and observed target revision.

Non-code tasks can finish against their own acceptance evidence without a Git
integration step. A parent objective waits for required children and its own
criteria; it does not create artificial worktrees or branches. The final state
names must preserve the selected distinction between submission and completion.

## Review policy and independence

Projects can require agent review, human review, both, or disable required review.
The selected default is independent agent review. Review requirements are pinned
to a versioned policy and an immutable submission. A review records the actor,
session when applicable, candidate revision, findings, decision, and check
evidence inspected. The service rejects a review of an obsolete submission as an
approval of the current one.

Proposed independence rule: an agent reviewer uses a separate review session
and attempt, with no recorded contribution to the candidate under review. It
may use the same model vendor or workstation; neither determines independence.
A separately configured reviewer principal can provide additional separation,
but rotating a token alone does not make the implementer an independent reviewer.
Retain contributor attribution across recovery attempts and resubmissions.
The protocol enforces recorded separation; it cannot prove a model's reasoning
was independent or detect falsely declared agent-session lineage.

Human approval must be recorded by an authenticated human principal. An agent
can request it and attach context, but cannot submit a human decision on that
person's behalf. For a project requiring both types, each required approval must
apply to the same current submission. A missing reviewer leaves the work waiting
with a clear reason; it does not silently weaken the policy.

Review findings have stable IDs and severity, a concrete remedy, and resolution
evidence. Rejection returns the implementation to a revision-needed state without
deleting prior submissions. Required unresolved findings prevent integration;
fixing the code creates a new submission and invalidates old approval
applicability. The exact number of required reviewers and escalation thresholds
are configuration defaults to specify before implementation.

## External jobs and evidence

A test, build, or deployment run is a **job**, separate from an agent attempt.
Register a unique job ID and producer identity before launch. Keep the runner
instance/host, owning attempt, checkout/snapshot, permitted operation, invocation
description, timestamps, latest observation, and terminal producer result.
Raw environment values, credentials, and arbitrary command output are not
required metadata and must not be collected automatically.

Producer status and observation freshness are separate fields. A previously
reported running job with an unreachable observer is now unobserved; it is not
proved stopped. The observer may reconnect to the same job with the necessary
authorization. An observer's permission to attach observations is not permission
to renew a superseded task lease, write code, or complete the current task.

A terminal result identifies the producer's exit status, not a wrapper/waiter's
exit status or a parsed success banner. Evidence specifies:

- The exact commit/tree, or an explicit dirty-snapshot digest and changed paths.
- Gate/check definition revision, relevant configuration/toolchain fingerprint,
  environment identity, start/end, and whether inputs remained stable.
- The required check roster and actual per-check outcomes: pass, fail, skipped,
  or interrupted, with reasons and result/log references.
- Who produced and, when required, who independently reviewed the evidence.
- Whether artifacts are accessible to other authorized workstations, retained
  only locally, or unavailable; optional digests permit identity checks.

A later mutation invalidates evidence applicability to the new snapshot. A
snapshot-to-commit equivalence assertion requires an actual matching tree/content
check. Historical evidence remains immutable. Missing coverage and unknown or
interrupted runs cannot be transformed into a pass by a successful wrapper.

## Scoped resources and disconnected producers

Task ownership prevents two owners of one task. Resource admission prevents
different tasks from concurrently using an incompatible shared resource.

Resources have a stable scope and key: workstation-local build capacity,
checkout-local mutable state, or a project/environment resource such as a shared
fixture database or integration branch. Different isolated worktrees need not
lock the same source file globally, but their shared fixture or build environment
may still conflict. Each configured reservation is exclusive or consumes units
from a declared capacity. Acquire a required set atomically, or none of it;
return blockers rather than retaining a partial set while waiting for the rest.

Resource validity is checked transactionally with operations that depend on it.
Dynamic capacity readings are dated observations, not a promise that other
processes will not consume memory or disk. Local locks and environment-specific
guards still protect against processes outside the coordinator.

Lease expiry does not prove the resource is physically free. Where an old job
can still interfere, a reservation enters a recovery hold until a trusted local
observation, fencing mechanism, or authorized operator action resolves it. The
hold blocks the conflicting resource, not unrelated work. The final policy must
define who can clear it and what evidence is needed, rather than treating every
expired reservation as safe to reuse.

## Interrupted requests and retries

Retryable mutations carry an idempotency key scoped to the authenticated caller
and operation. Store a fingerprint of the request and the operation's result in
the same transaction as the mutation.

- An identical retry refers to the original operation and attempt; it does not
  create another task, claim another task, or extend the lease a second time.
- Reusing a key with different request data produces a clear conflict.
- Authentication and current authorization still apply when replaying a result.
- An old successful claim receipt is historical evidence, not proof of current
  authority. The client checks current ownership and the deadline before acting.
- A retry of completed submission can acknowledge the original successful
  submission without reopening its attempt or changing current task state.

The final API must define receipt retention and behavior for retries outside the
supported window. Clients must not silently generate a new key after an uncertain
claim response; they must resolve the original operation first.

## Health, progress, and recovery

Track these separately:

| Signal | Meaning | Does not establish |
| --- | --- | --- |
| Last heartbeat | A session supplied an accepted health update | Useful progress or successful execution |
| Last progress checkpoint | Agent reported a milestone, blocker, or current action | Independently verified correctness |
| Lease deadline | Latest time through which ownership is granted | That an offline process has actually stopped |
| Result evidence | Agent supplied checks, changes, and artifact references | Review approval or merge unless separately recorded |

A background helper is optional and needs an explicit lifecycle contract. It must
not renew forever after its associated agent session has ended. Long tool calls,
agent thinking time, workstation sleep, and network loss must be considered when
choosing heartbeat intervals and expiry. The service can expose stale progress
separately from an expired lease without assuming that silence proves a crash.

Proposed defaults are a one-minute heartbeat and ten-minute task lease. A local
renewal helper receives a separate, narrowly scoped reporter credential; it can
renew the named attempt and report authorized jobs, but cannot create checkpoints,
submit work, change policy, or extend its own reporting window. The active agent
explicitly declares the action being watched and its reporting-window deadline.
Propose a one-hour maximum window initially, adjustable by the operator for long
gates. Renewal cannot extend beyond that deadline or a revoked session/attempt.

The helper stops renewing when its observed harness instance exits, when authority
is lost, or when the declared window ends. A job continuing beyond the window
retains its own job record and resource holds; it does not silently keep the
agent's task owned. The agent can explicitly establish a fresh window while its
authority is still valid. Manual HTTP clients can renew directly without a helper.
This supports long tool calls while bounding an orphaned helper's effect.

Display stale progress separately, with a proposed 15-minute warning threshold.
A declared long job shows its expected deadline and observation freshness, so
an expected wait is distinguishable from unexplained silence. Warnings do not
automatically mark failure, restart jobs, or release resources.

Adapter events distinguish a turn ending, context compaction, session
disconnection, explicit relinquishment, and task completion. Repeated connect or
resume events reconcile existing session/attempt/job IDs first. They cannot
implicitly claim additional work. A compact/resume hook must not fast-forward a
checkout underneath an active mutation or check of that checkout.

Expiration makes a task a recovery candidate, rather than immediately ready for
fresh implementation. Under the selected default, another agent may atomically
claim recovery authority. A project may instead require operator release before
that recovery claim. Both modes retain the expired attempt and its checkpoints.

The recovery packet includes prior submissions, worktree/branch and published
revision references, known jobs, observation freshness, and resource holds. The
recovering agent checks what has already been delivered, which saved work is
accessible, and whether prior jobs can still interfere. It records a disposition:
resume saved work, reconcile already-delivered work, restart with a reason, or
remain blocked awaiting an observation or operator decision.

Recovery inspection is distinct from permission to start conflicting work. An
unreachable workstation or a missing heartbeat does not prove its jobs stopped.
An unresolved external job keeps its conflicting resource reserved. A recovery
attempt may continue only when the selected action's resource and evidence
requirements are satisfied. Expiry of a recovery attempt permits another recovery
claim; it does not clear those holds. Deadlines and ownership generations prevent
two recovery agents from both recording an authoritative disposition.

Disconnected clients must not start new coordinated work. While a previously
granted lease is still valid, behavior depends on the final offline-work policy.
After authority expires or is lost, instructions must direct clients to stop
ownership-dependent changes and reconcile with the service. Client deadlines
should be conservative and account for request delay; simply comparing server
timestamps with an unsynchronized workstation clock is insufficient.

This is a cooperative protocol. The service can reject stale database writes;
it cannot stop local computation or retroactively prevent a Git push by an old
agent. A policy for publishing, merging, and external side effects is necessary
before promising stronger end-to-end guarantees.

## Scenarios the implementation must demonstrate

| Scenario | Required result |
| --- | --- |
| Two workstations claim one eligible task simultaneously | One receives ownership; the other receives no authority over that task |
| Claim succeeds but its response is lost | Retrying identifies the original attempt, without claiming more work |
| Renewal waits for the database lock past the deadline | It fails as expired; the old request start time grants no exception |
| Expired owner reports completion after takeover | Current task and new attempt remain unchanged |
| Heartbeat races with recovery | One transaction wins according to validity and recovery policy; two current owners are impossible |
| Completion races with takeover | Either valid completion ends the work first, or the old owner is rejected |
| Completion races with an acceptance-criteria edit | Revision checks force reconciliation; neither change silently overwrites the other |
| Completion fails midway through storage | No partial completion, handoff, or lesson insertion becomes visible |
| Helper stays alive while agent stops making progress | UI distinguishes heartbeat freshness from progress freshness |
| Service restarts | Persistent attempts and deadlines remain consistent; elapsed downtime does not renew leases |
| Old backup is restored | An explicit restore procedure invalidates pre-restore authority before serving work |
| Operator revokes access during a task | Future calls and result replays respect revocation; task disposition follows an explicit recovery policy |
| Two implementations register the same editable checkout | Refuse the second registration; require a separate worktree |
| Two integrations target the same branch | Only one holds the integration reservation; the next rechecks the target revision |
| A code task waits for its own review and integration | Those activities become eligible from its submission; no circular wait for the task's overall completion |
| A candidate changes after review approval | Preserve old evidence and require review/check applicability to the new submission |
| A job observer dies while its producer continues | Record observation loss, preserve job identity, and reconnect before considering a restart |
| A task lease expires while its job occupies a shared fixture | Prevent conflicting resource reuse until recovery resolves the producer's state |
| A hook reports success after a failed formatter/check | Advisory hook success does not satisfy verification evidence |
| A gate passes, then code changes before submission | Require evidence applicable to the submitted snapshot |
| A required check is skipped because its environment is missing | Record the blocker/gap; do not report complete verification |
| A task is imported again from an old open-backlog paragraph | Preserve the current closed/superseded record; surface any source conflict |
| A task waits for a human decision | Retain its checkpoint and decision linkage while other eligible work continues |
| Two agents claim tasks in different projects | Both can own and work their respective tasks simultaneously |
| An authenticated caller deliberately selects a different project | Permit project access; still require the operation's role and current attempt ownership where applicable |
| A caller attaches project B's evidence to a project A attempt as if it belonged to A | Reject the mismatched relationship; preserve provenance for explicitly supported cross-project references |
| One project is waiting on an integration reservation | Other projects remain eligible unless they share that same canonical target/resource |

## Shared lessons and delegated project rules

Agents may publish and correct lessons, including lessons useful across projects.
The selected policy also allows a project to delegate binding-rule changes to
agents without human approval. Store that permission separately from policy
text; check it before accepting each revision. A rule revision is an explicit,
attributed operation, not a side effect of importing a lesson or rendering Markdown.

Knowledge records contain a concise statement, explanation, applicability tags
and versions, source task/submission/revision, supporting evidence, author, and
created/updated times. Corrections and supersession preserve earlier versions and
their links. A disputed lesson stays inspectable with its dispute visible; it
does not silently become mandatory guidance.

Provide project-scoped search and explicit cross-project/common retrieval to all
authenticated callers. A common lesson preserves its original source rather than
duplicating detached copies. Rank current applicable records using full-text
match, component/version tags, and bounded usefulness feedback. Feedback changes
ranking, never permission or policy status. No embeddings service or model API is
required for the first release.

Orientation separates mandatory current rules from suggested lessons. Required
rule updates are versioned and visible to active agents. Local AGENTS.md/CLAUDE.md
instructions still apply; a service rule cannot silently erase a local rule or
resolve a conflict by pretending the local instruction was never present.

## Artifact storage and portable recovery

Store bounded log/report uploads in the service, alongside external artifact
links. Code checkpoints use Git remotes. Record source accessibility honestly:
local-only uncommitted edits cannot be recovered from a different workstation
merely because their path appears in a checkpoint. Source-worktree bundles and
build-directory uploads are outside the selected first-release scope.

Artifact metadata includes ID, project/task/job association, original display
name, media type, byte size, SHA-256 digest, author, retention, and availability.
Use generated storage paths, never client filenames as filesystem paths. Stage
and size-check uploads, finish and durably store the file, then commit accessible
metadata. Incomplete uploads are not downloadable; orphan-file cleanup and
backup/restore reconcile filesystem state with database references.

Enforce per-file and aggregate storage quotas before and during uploads. Retain
metadata and a deletion/expiry reason after removing file bytes, so historical
evidence does not appear to have an accessible artifact when it no longer does.
Uploads are explicit; the reporter does not automatically collect credentials,
environment variables, source trees, or every raw log from a workstation.

Downloads require authentication. Treat uploaded files as untrusted attachments,
not executable same-origin web pages; any inline preview must render safe text.
The server stores external links without fetching arbitrary URLs. Cross-machine
access to a linked resource is reported rather than assumed from its existence.

## Duplicate work beyond a shared task ID

Atomic claims address two agents choosing the same recorded task. They do not
address agents creating two different tasks for the same requested change, or
different tasks that touch conflicting code.

Proposed controls, strengthened by the repository review:

- Stable import/source IDs and uniqueness constraints for repeated imports;
  numeric item labels alone are insufficient across sessions/documents.
- Search-before-create guidance and explicit duplicate/related-task links.
- An after-claim check that acceptance criteria are not already satisfied on the
  relevant repository revision, with an evidence-based reconciliation outcome.
- Durable closure, rejection, and supersession records consulted by selection
  and import, including reopening conditions for intentionally deferred work.
- Dependencies and optional component/resource reservations for known conflicts.
- The confirmed separate-worktree and serialized-integration rules above.

Natural-language similarity can suggest duplicates, but cannot establish task
identity reliably. Semantic search or model-based deduplication is an optional
feature, not a prerequisite for atomic ownership.
