# Completion workflow contract

Status: implementation contract for backlog item 2. The service never runs Git or
accepts a caller-authored assertion that a required check passed. Every mutation
uses an idempotency key and rechecks authority after obtaining SQLite's writer
lock. Replaying a durable result does not renew a lease or refresh any authority
field in the saved response.

## Policy and immutable submissions

`GET /api/v1/projects/{project}/workflow-policy` returns
`{project_id, revision, canonical_repository_key, required_checks}`. A required check is
`{identity, version, environment}`; all three strings are stable exact-match
identities. A human changes the roster with
`PUT /api/v1/projects/{project}/workflow-policy` and
`{expected_revision, required_checks}`. Normal setup derives the shared repository
identity from the project's URL and reuses an existing saved identity. Public
GitHub HTTPS, `git@github.com:owner/repo` and `ssh://git@github.com/owner/repo`
forms share an identity, including optional `.git` suffixes and trailing slashes.
GitHub owner/repository case is ignored. Other URLs have distinct exact-URL
identities; custom SSH hosts are not guessed to be equivalent.

The optional `canonical_repository_key` is an advanced administrator alias
configuration. Existing clients may resend an unchanged saved key. An
administrator can assign the same key to a custom alias before it has workflow
evidence, after verifying both URLs refer to the same repository. An existing
binding cannot change after submissions, while its project has an integration
hold, or when changing it would split a binding shared by other projects.
Equivalent URLs with conflicting historical bindings fail closed and require
explicit reconciliation; saved submissions and holds are never rewritten.
Omitting the key on an existing project preserves its binding even if its URL
has changed. The current project API does not expose a repository-URL edit.
The target branch remains the project's validated Git ref. The roster must
contain 1–100 unique, nonempty checks. The
workflow-policy revision is independent of the existing
project policy revision. A code submission pins both. There is no implicit empty
roster and an agent cannot create, weaken, or replace it.

`POST /api/v1/projects/{project}/attempts/{attempt}/submissions` accepts the
following common fields:

```json
{
  "generation": 1,
  "task_revision": 2,
  "project_policy_revision": 3,
  "workflow_policy_revision": 4,
  "kind": "code",
  "summary": "bounded result summary",
  "acceptance_evidence": [{"criterion": "exact criterion text", "evidence": "bounded evidence"}],
  "handoff": "bounded handoff",
  "repository": "canonical configured repository URL",
  "base_revision": "full source revision",
  "candidate_revision": "full candidate revision",
  "candidate_tree": "full candidate tree"
}
```

For `kind: "general"`, omit repository/base/candidate fields and send a workflow
policy revision of `0`; project policy and task revision are still pinned. Every
current acceptance criterion must appear exactly once. Submission requires the
current unexpired owner, a matching kind/revision/policy, and quiescent subject
work: no retained reservations and no nonterminal or uncertain jobs. It atomically
ends the implementation attempt, stores the immutable submission, records the
contributor session, blocks ordinary subject claims, and creates the applicable
review and integration activities. The response is
`{submission, subject_task_id, activities, next_actions}`.

A code submission creates review activities from the pinned `review_mode`,
plus one `integration` activity. The modes are `agent`, `human`, `both`, `either`,
and `none`. `both` requires an `agent_review` and a `human_review` approval.
`either` creates one shared `either_review` activity: an eligible independent
agent or a human may claim it, and one approval satisfies the review requirement.
Only one session can own that slot at a time. Requesting changes requires a new
submission; another reviewer cannot override the decision on the old candidate.
Agent independence is enforced both when claiming and when deciding this shared
review, including the existing opt-in rules for non-contributing subagents.
The existing agent-only, human-only and no-review modes retain their behavior.
A general submission
creates only its configured reviews; with no review it completes atomically.
Activities have stable IDs and linked internal task IDs. They are returned from
`GET /api/v1/projects/{project}/tasks/{task}/workflow` and
`GET /api/v1/projects/{project}/workflow-activities/{activity}`. Internal activity
tasks are excluded from ordinary candidate selection and must be claimed through
the workflow API.

## Activity authority and review

`POST /api/v1/projects/{project}/workflow-activities/{activity}/claim` accepts
`{expected_submission_id, expected_project_policy_revision,
expected_workflow_policy_revision}`. It returns `{activity, attempt,
lease_remaining_ms, renew_after_seconds, current_authority}`. Review activity
claims enforce actor type. Agent review also rejects every principal/session
recorded as a contributor to any revision of the subject task; contributor history
survives releases, recoveries, changes requested, and later submissions. Integration
claims require all current approvals and, if automatic integration is disabled, a
current human authorization. Integration claims atomically acquire the exclusive
hold for the project's canonical repository key plus target branch. The hold is
global across projects and survives attempt/session/credential expiry.

Expired or revoked activity ownership respects the project's recovery mode.
In manual mode, agents cannot take over. A human inspects saved work and physical
resources. Before publication intent exists, the human may reopen an expired,
revoked, or policy-stale candidate; this fences its linked attempts and requires a
new submission and new reviews. Live current owners must release first, and
unresolved jobs/resources always block reopening. After publication intent, use
publication reconciliation. Completed workflows cannot be reopened.

An authenticated agent may reconcile only when the exact current candidate and
policy are unchanged, current scoped decisions are resolved, a fresh observation
of the pinned repository branch exactly matches the immutable pre-publication
base or intended result, the old publisher is confirmed stopped, the integration
owner is no longer live, and no registered job is live or uncertain and no
reservation remains held. Agent reconciliation cannot publish or create check
receipts. A moved target, missing durable intent or local journal, conflicting
result, or any ambiguous producer remains human-gated with the target hold
preserved. The native CLI serializes its local journal and records its evidence;
the service stores this as a client attestation and does not verify remote Git or
process state itself. Both human and agent reconciliation retain the original
record and create a fresh integration activity under the same target hold.

Activity attempts renew through the existing
`POST /api/v1/projects/{project}/attempts/{attempt}/renew` endpoint and use existing
checkout, reservation, and job endpoints. They release through
`POST /api/v1/projects/{project}/workflow-activities/{activity}/release` with
`{generation, summary, blocked:false}`; a release after publication intent is
refused. Inspect and reconcile that publication instead; reconciliation creates
a replacement activity and retains the historical result. Workflow state changes
always require the activity ID as an additional candidate/type guard.

`POST /api/v1/projects/{project}/workflow-activities/{activity}/review` accepts:

```json
{
  "generation": 1,
  "submission_id": "uuid",
  "decision": "approved",
  "summary": "bounded review summary",
  "findings": [{"severity": "required", "remedy": "bounded remedy", "evidence": "bounded evidence"}]
}
```

`decision` is `approved` or `changes_requested`; severity is `required` or
`advisory`. The current, unexpired linked attempt and exact current submission are
required. A decision is immutable. Changes requested completes that review
activity, cancels the other pending activities, makes the subject eligible for a
new revision attempt, and keeps all old submissions, approvals, findings, and
contributors. A later submission creates fresh activities; old approvals do not
transfer. The subject's task and project policy revisions must still match the
submission for approval to affect readiness.

## Checks and integration

Required check receipts are selected by job ID; clients do not submit check
outcomes. A selectable job must belong to the current integration activity attempt,
have a terminal producer state of `succeeded`, exit code `0`,
`inputs_unchanged: true`, no reconciliation amendment, and exact source revision
and tree matching the intended integrated result. Each job registered for a check
also carries immutable `check_identity`, `check_version`, and `check_environment`
fields that exactly match one pinned required-check entry. One current producer
receipt must cover every roster entry, with no duplicate identity tuple. Candidate
jobs cannot satisfy integrated-result checks merely because candidate and result
happen to share a commit label.

When project policy disables automatic integration, a human records intent with
`POST /api/v1/projects/{project}/workflow-activities/{activity}/authorization` and
`{submission_id, expected_project_policy_revision,
expected_workflow_policy_revision, summary}`. Authorization is immutable and
applies only to that current candidate and policy pair.

After claiming integration and registering an isolated checkout, the owner calls
`POST /api/v1/projects/{project}/workflow-activities/{activity}/publication-intent`:

```json
{
  "generation": 1,
  "submission_id": "uuid",
  "observed_target_revision": "full target revision before publication",
  "observed_target_tree": "full target tree before publication",
  "result_revision": "full intended integrated revision",
  "result_tree": "full intended integrated tree"
}
```

The service saves one immutable intent before any Git-side compare-and-swap. The
client must independently verify that the remote target still equals
`observed_target_revision` before publishing. Repeating with a new key cannot
replace an intent; retry uses the original key.

A fresh activity GET returns `publication_allowed` and
`qualifying_check_job_ids`. Publishing is allowed only while the exact candidate,
policies, approvals, authorization, activity lease, canonical hold, intent, and
one successful stable-input exact-result producer per required roster entry are
all current. Publication intent alone is not permission to publish. Once any
integration result exists, publication is no longer allowed for that activity.

`POST /api/v1/projects/{project}/workflow-activities/{activity}/integration-result`
accepts `{generation, submission_id, publication_state, observed_target_revision,
result_revision, result_tree, check_job_ids, summary}` where publication state is
`published`, `not_published`, or `uncertain`. Values must match the saved intent.
An uncertain result may omit checks, is durable, makes the activity
recovery-required, and retains
the global hold. It never completes work or releases the hold. A known
`not_published` result also retains the hold until explicit reconciliation.

A human reconciles with
`POST /api/v1/projects/{project}/workflow-activities/{activity}/publication-reconciliation`
and `{submission_id, disposition, observed_target_revision, observed_target_tree,
evidence}`, where disposition is `published`, `not_published`, or `target_moved`.
`target_moved` records that the intended result was published but a later external
actor advanced the target before finalization; it closes the old activity and hold
and creates a fresh integration activity for the same approved candidate. The new
activity requires fresh authorization and checks against its new result. Reconciliation
cannot invent check success. Confirmed publication must match the intent's exact
result revision/tree. Confirmed nonpublication permits an explicit new integration
attempt after the old hold is closed in the same transaction.

Agents use the separate
`POST /api/v1/projects/{project}/workflow-activities/{activity}/agent-publication-reconciliation`
operation with `{attempt_id, generation, submission_id, disposition, canonical_repository_key,
target_branch, observed_target_revision, observed_target_tree, observed_at,
local_journal_verified, publisher_stopped, evidence}`. `observed_at` is Unix
milliseconds and must be no more than two minutes old. Both boolean fields must
be true. The disposition can only be `published` when both
identities equal the immutable intended result, or `not_published` when both
equal the saved pre-publication target. The endpoint also requires an expired or
revoked integration owner, no registered live/uncertain job, no held reservation,
current candidate/policy/decisions, and the still-held original target. Moved or
missing targets, an unavailable local journal, a running publisher, or any other
uncertainty stay human-gated. The service cannot independently inspect Git or
prove that a workstation process stopped; agent evidence remains a client
attestation. The native CLI obtains its protected journal lock and remote
observation before it sends this mutation.

For a known published result, call
`POST /api/v1/projects/{project}/workflow-activities/{activity}/finalize` with
`{generation, submission_id, observed_target_revision, observed_target_tree}`.
These last fields are a fresh post-publication remote observation and must equal
the intended result revision and tree. Finalization verifies the exact current candidate,
task revision, project policy revision, workflow-policy roster, approvals,
authorization, intent/result, exact remote observation, source-bound job receipts,
activity authority, and quiescence of both subject and integration tasks. It then
atomically completes the integration activity and subject task, closes the global
hold, and makes dependencies eligible. There is no generic completion endpoint.

## Local worktree cleanup after completion

After a fresh task read confirms the subject is `done`, the completing agent
removes its task-specific implementation and integration worktrees following
[the startup cleanup procedure](agent-startup.md#remove-completed-task-worktrees).
Submission, review approval, and publication alone do not trigger removal. Preserve
the main parent checkout, any main target checkout, unrelated worktrees and branches,
and any tree with unsaved evidence or live/uncertain work. Save evidence first,
verify exact resolved paths and registered Git identities, then use
`git worktree remove` from outside the tree without forcing deletion. Report
removed paths or retained paths and blockers honestly. This is local guidance;
the service never deletes workstation files or bypasses completion requirements.

After successful worktree removal, delete only that completed task's local and
remote branches using the same linked procedure. Verify exact ownership and refs,
full merge into the intended target, and no remaining worktree use. Preserve
main, parent, default, configured target, host-protected, shared, and unrelated branches. Local
deletion uses `git branch -d`; remote deletion requires an explicit expected-ref
lease against the freshly observed remote commit. A refusal or uncertain result
requires inspection and truthful retention reporting, never forced or bulk cleanup.
Conclusive absence means a ref is already removed; report it without recreating
it and apply the guards to remaining refs. Lookup failure does not prove absence.
Inspect existing completed tasks in the authorized project with these same gates;
record each removed or retained worktree and branch separately.

## Coordination hooks

The coordination module must call these workflow hooks while holding its existing
writer transaction:

- `guard_normal_claim(connection, project_id, task_id)` rejects internal activity
  tasks and subjects with a current submission; `record_contributor(connection,
  task_id, actor_id, session_id, now)` runs
  when normal or recovery work is claimed and again before owned edit/checkout.
- `guard_subject_mutation(connection, project_id, task_id)` rejects
  definition/dependency edits while a
  submission is current. Policy changes may proceed, but immediately make pinned
  submissions stale until an authorized new submission/reconciliation.
- `guard_release_or_recovery(connection, project_id, task_id)` preserves workflow
  integration holds and
  refuses recovery/requeue paths that would bypass a current submission or
  publication uncertainty.
- activity checkout/reservation/job registration calls `guard_activity_work(connection,
  project_id, activity_task_id, now)` to reject stale candidates and policies while
  still allowing checkpoint, renewal, release, and late reporter observations.
- task detail/status reads call `workflow_snapshot(connection, project_id, task_id,
  now)` and expose its
  submission, activities, blockers, and next actions. Ordinary ready lists exclude
  task IDs present as `workflow_activities.activity_task_id`.

The jobs module must accept optional check identity/version/environment at job
registration and store them immutably. It must expose a server-side receipt lookup;
workflow completion never accepts caller-authored status JSON.

Normal attempts pin `attempts.task_revision` and `attempts.policy_revision` when
claimed. Submission compares those stored values to the current task and project
and to the request. Existing pre-migration attempts have null pins and must be
released and reclaimed; callers cannot choose a newer revision at submission.

An authenticated human may cancel a stale current submission with
`POST /api/v1/projects/{project}/tasks/{task}/workflow/reopen` and
`{submission_id, reason}`. This requires subject and activity quiescence, refuses
uncertain or known publication, cancels pending activities, preserves all evidence,
closes any safely releasable hold, and makes ordinary revision work eligible.
