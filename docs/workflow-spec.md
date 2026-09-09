# Proposed workflow specification

Status: design draft. This specifies engineering behavior beneath the confirmed
product choices in [PLAN.md](../PLAN.md). Projects can permit automatic integration
or require human authorization, and can delegate binding-rule changes to agents.
The service stores bounded logs/reports; source checkpoints remain in Git remotes.

## Task outcome and available work

A task represents an outcome. Its kind is `code`, `general`, `objective`,
`agent_review`, `human_review`, or `integration`. Agents normally create code or
general tasks. An objective groups required children. Review and integration
tasks are linked workflow activities, created transactionally from a submission
and the applicable project policy, with stable identities preventing duplicates.

Persist a task lifecycle of `planned`, `open`, `done`, `canceled`, or `superseded`.
Expose a derived work status alongside it, so agents do not have to infer
readiness from raw records:

| Work status | Meaning and next action |
| --- | --- |
| `planned` | Outcome is recorded but not admitted to the work queue |
| `ready` | The task can be claimed; a candidate listing grants no ownership |
| `in_progress` | A current unexpired attempt owns this task |
| `waiting_review` | A submitted candidate needs one or more linked reviews |
| `waiting_integration` | Review requirements are satisfied; the linked integration task is pending |
| `integrating` | The linked integration attempt is preparing or publishing a result |
| `validating` | The integrated result is being checked; the deliverable is still incomplete |
| `blocked` | An explicit dependency, decision, capability, resource, or verification issue prevents the next action |
| `recovery_required` | An expired owner or uncertain external effect requires reconciliation |
| `done` | This task kind's required outcome is accepted |
| `canceled` | Work was deliberately stopped; this does not satisfy a dependency |
| `superseded` | A replacement/duplicate relationship closes the record; the replacement is explicit |

The response also includes structured blockers, current attempt, submission,
linked activities, and exact next actions. It never uses a status string as a
substitute for these facts. These statuses describe the selected task: a code
task can be waiting for review while its linked review task is in progress.

Status is derived from authoritative records, including server-time lease
validity. The database must not rely on a periodic job to change `in_progress`
to `recovery_required` before refusing a stale update. Transactional projections
may speed dashboards but cannot grant authority or hide expired ownership.
The API does not accept arbitrary writes to derived status.

An expired current attempt or unresolved external outcome takes precedence over
`ready`. A task waiting for an active linked review/integration displays that
phase and retains any blockers as separate fields. No task becomes complete
merely because all owners have released their attempts.

## Admission and task selection

Task admission requires a title, concrete outcome, acceptance criteria, kind,
project, priority, source/provenance, and any required capabilities or resources.
A code task also needs its repository/target binding. Use a checklist of explicit
criteria; do not require the service to interpret arbitrary prose as executable
tests. Planned tasks can hold incomplete drafts.

Default priorities are urgent, high, normal, and low. Within a project, selection
filters eligibility and requested task kinds/capabilities, then orders by
priority, oldest-ready time, and stable ID. Ready time resets only when work
actually becomes eligible again, not on a heartbeat or cosmetic edit. There is
no agent-vendor preference or required scheduling model.

Agents search existing and recently closed tasks before creating new work.
An optional stable external source key is unique within its declared source
namespace. Similar titles produce suggestions, not automatic task identity.
The service can prevent duplicate claims and repeated source imports; it cannot
guarantee that separately worded outcomes never overlap.

Normal dependency edges require the prerequisite task to be `done`. Cancellation
and supersession produce an explicit blocker and replacement reference; they do
not silently satisfy the old edge. Human-authorized dependency edits retain
history. Objectives cannot complete until required children and their own
acceptance criteria are satisfied. Reject parent and dependency cycles, including
concurrent edits that would create a cycle only when combined.

## Attempts and checkpoints

Each attempt belongs to one task, authenticated session, workstation, and
ownership generation. Its state is `active`, `submitted`, `released`, `blocked`,
`expired`, or `canceled`. Only an active, current, unexpired attempt can perform
ownership-dependent mutations. Historical attempts are retained.

For code work, claim first, then prepare/register the separate worktree and
checkpoint the starting repository state before editing. A preparation failure
releases or blocks the attempt with a reason; it does not leave an unreported
reservation. Lease renewal can continue during preparation.

A checkpoint includes a short summary, current action, progress since the last
checkpoint, next step, blockers, and current branch/revision/job references when
relevant. Heartbeat requests need not fabricate a new progress summary. Save
health time and progress time independently.

Explicit relinquishment records a final checkpoint and an outcome. A request to
cancel an active task revokes service authority but retains jobs/resource holds
until their actual disposition is known. Operators see the distinction between
canceled work and a still-running external job.

## Submission, review, and completion

Submitting a code attempt creates an immutable candidate containing the task
revision, policy revision, repository/base/candidate revisions, result summary,
acceptance evidence, handoff, and linked/new lessons. Artifact references include
their accessibility. The submission ends the implementation attempt and admits
the required review activities atomically.

Reviews become eligible from this candidate, not from the code task's `done`
state. This avoids a dependency cycle. Review tasks use their own ownership,
candidate binding, and attribution. A completed review task means a decision
was recorded; its decision may be approval or changes requested. The code task
uses the decision as a guard, not merely the review task's terminal state.

The review modes are `none`, `agent`, `human`, and `both`, with `agent` as the
selected default. Propose one required approval of each enabled type initially.
Human decisions require human authentication; agent review uses the separation
rules in [the coordination contract](coordination-contract.md).

A changes-requested decision makes a revision attempt eligible, subject to the
project's retry/escalation rules. It does not reopen the completed review task.
A new code submission supersedes the old candidate and creates its own required
activities; old approvals never transfer automatically. Pending obsolete
activities are canceled, while active ones lose authority and retain history.

After the required approvals, a linked integration task becomes eligible under
the selected integration-authorization policy. It reserves the canonical target,
records the observed target and candidate, and prepares the intended result.
Conflicts, a moved target, incomplete checks, and uncertain publication outcomes
have distinct blockers and recovery instructions.

The integration result records its base, resulting commit/tree, publication
observation, verification roster, check outcomes, and reporting actor. Required
checks must apply to the exact integrated tree and configured environment.
Pre-publication testing can count when tree equivalence is verified; checks
whose semantics require a published/deployed state must actually run there.

The final transaction verifies the current candidate and required policy,
applicable approvals, integration evidence, required checks, and ownership.
It completes the integration activity and code task together, records the final
handoff, and reevaluates downstream readiness. Publishing code without satisfying
these checks leaves visible incomplete work. It does not automatically revert
the target or authorize deployment.

General tasks complete from their own acceptance evidence and configured review,
without artificial Git requirements. Objectives complete from their required
children and criteria. They have no implementation worktree of their own.

## Recovery and late information

Recovery is an attempt mode on the affected task, not permission to overwrite
the old attempt. Claiming recovery is atomic and uses a new generation. In manual
projects, an operator must release the recovery hold first. Inspection can read
all retained evidence but cannot bypass reservations held by uncertain jobs.

Record one recovery disposition: resume accessible work, reconcile an already
delivered result, restart with an explanation, or remain blocked. Reconciliation
must satisfy the same completion guards as normal work. A commit found in Git
does not itself establish passed review or verification.

Support append-only late notes and job observations from their authorized
reporters after an attempt expires. They are clearly historical/unaccepted and
cannot renew ownership, approve the current candidate, release another owner's
resources, or complete the task. A current recovery owner may explicitly adopt
applicable evidence after verification.

## Changes to requirements or policy during work

Descriptions, acceptance criteria, dependency sets, and project policy use
revision checks. Cosmetic changes preserve acceptance applicability when marked
as such; substantive changes require explicit reconciliation. An agent cannot
erase or weaken acceptance criteria to make its own submission pass.

A project may explicitly authorize agents to change binding project rules without
human approval. Each change checks the existing delegation, requires the expected
policy revision, and records rationale and actor. Delegation and service credential
administration are separate from editable rule text: changing a rule cannot grant
its author a new administrative identity or fabricate human approval.

A new policy revision prevents new claims/submissions from silently following
the old policy. Existing attempts receive a policy-changed next action and can
checkpoint or relinquish safely; they must reconcile before taking a newly
restricted action. Historical approvals remain tied to their original policy.
The service records the authorized actor's resolution instead of silently grandfathering
or retroactively rewriting every in-progress task.
