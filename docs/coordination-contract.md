# Proposed coordination contract

Status: provisional design, supporting [the discovery plan](../PLAN.md).
No pending operator decision is treated as accepted here. This document defines
candidate correctness rules that can support the different policy choices.

Revised after [the repository/hook review](repository-review.md). The operator
has confirmed separate worktrees per implementation task and one integration
step at a time into the target branch for the first release, and simultaneous
support for multiple projects in one service instance.

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
as one another. The credential/session mechanism remains an open design choice.

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
5. A lease is valid only while `server_now < expires_at`. Equality is expired.
   Renewal is allowed only before expiry. Retrying a renewal cannot resurrect
   an expired attempt.
6. Terminal attempts reject fresh mutations. The system preserves their results
   and checkpoints as history. A late agent may need a separate recovery-note
   operation, if selected for the release; such a note cannot complete current
   work, extend a lease, or unblock dependent tasks.
7. Completion saves the result, handoff, referenced/new lessons, attempt outcome,
   and chosen task transition in one transaction. Whether that transition means
   awaiting review or done remains a project-policy decision.

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

Adapter events distinguish a turn ending, context compaction, session
disconnection, explicit relinquishment, and task completion. Repeated connect or
resume events reconcile existing session/attempt/job IDs first. They cannot
implicitly claim additional work. A compact/resume hook must not fast-forward a
checkout underneath an active mutation or check of that checkout.

Expiration detection is mandatory for the requested service. What happens next is
still open: automatic requeue, an agent-driven recovery claim, human release, or
a per-project policy. The last checkpoint and artifact location should accompany
any recovery candidate so its next owner can inspect prior work before repeating it.

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
