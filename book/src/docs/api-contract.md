# Proposed HTTP and CLI contract

Status: design, not an implemented API. Names and examples below are the proposed
first-release interface. Implementation will publish matching OpenAPI schemas,
CLI help, and service-delivered agent instructions from shared contract types.

## Conventions

Use `/api/v1` for the JSON API over HTTPS. IDs are opaque strings. Project IDs
are explicit in project operations; there is no global current project. Dates
are UTC RFC 3339 values; revision and ownership-generation fields are integers.
Pagination uses opaque cursors with a proposed default of 50 and maximum of 200
items. Responses include `request_id` and `server_time`.

`server_time` is the UTC wall-clock observation when the response is emitted,
for correlation and display. It can be behind protected coordinator time during
a clock incident; never calculate lease authority as `expires_at - server_time`.
Use server-computed `lease_remaining_ms` and `renew_after_seconds`, subtract local
monotonic request elapsed time and a safety margin, and honor explicit authority
validity flags. See [clock safety](clock-safety-contract.md).

Authenticate agents with `Authorization: Bearer <agent-token>`. Browser sessions
use the cookie/CSRF contract in [onboarding-contract.md](onboarding-contract.md).
Never put authentication or session secrets in query strings. Resolve
authentication before disclosing whether a requested project/record exists.

Every retryable mutation requires an `Idempotency-Key` generated and persisted
before the first request. The key is scoped to principal, method, and operation
path. Its stored request fingerprint includes relevant input, expected revisions,
session identity, and a verifier for any submitted secret, never raw secrets.
Reusing the key with different input is a conflict. Receipt access still checks
current authentication and authorization.

Propose retaining full mutation receipts for at least 30 days, plus compact
principal/operation/key/fingerprint tombstones after that. A retry after receipt
expiry returns `idempotency_receipt_expired` and reconciliation instructions;
it cannot execute the old operation as new. A principal's tombstones can be
removed when the principal is permanently retired and all credentials revoked.

Use these HTTP/error families:

| HTTP status | Representative code | Client action |
| --- | --- | --- |
| 400 | `invalid_request` | Correct the indicated field |
| 401 | `authentication_required` | Show public setup help or reconnect with configured credentials |
| 403 | `operation_not_permitted` | Show the missing operation permission; do not re-enroll an already authenticated client |
| 404 | `record_not_found` | Reconcile the authenticated project's binding/reference |
| 409 | `claim_conflict`, `revision_conflict`, `lease_expired`, `policy_changed` | Follow the supplied current-state and next-action links |
| 413 | `payload_too_large` | Reduce/split the report or use an external artifact link |
| 422 | `requirements_unsatisfied` | Address explicit blockers or validation details |
| 429 | `rate_limited` | Honor `Retry-After`; a delayed retry never extends an expired lease |
| 503 | `temporarily_unavailable` | Back off, retain the original mutation key, and observe the existing lease deadline |

An error body contains `error.code`, a concise `error.message`, structured
`error.details`, `error.next_actions`, and `retryable`. Never recommend blind
retries for expired authority, denied permission, or missing verification.

## Public help, credentials, and sessions

`GET /api/v1/info` returns product/API/instruction versions and public setup help.
`GET /api/v1/help/authentication` returns human-readable enrollment steps and
non-secret configuration examples. Neither returns private project information.
An unauthenticated private request returns the same useful help reference:

```json
{
  "request_id": "request-example",
  "server_time": "2026-09-09T18:00:00Z",
  "error": {
    "code": "authentication_required",
    "message": "Configure this workstation's agent credential, then reconnect.",
    "details": {"help_path": "/api/v1/help/authentication"},
    "next_actions": [{"action": "show_operator_setup_help"}],
    "retryable": false
  }
}
```

Browser sign-in/out and password-change endpoints operate on local human
accounts. Admin endpoints manage principals and issue/revoke tokens. First-admin
creation and account recovery use a host-local command, never public enrollment.
All issued agent tokens have agent identity; an agent token cannot record a human
review. A project may opt into `allow_subagent_reviews` through its human-managed
policy. Omission in a policy update preserves the current setting; the default is
false and delegated agent rule editing cannot change it. Session registration
accepts optional `subagent: {project_id, name, parent_session_id}`. The server
returns `subagent_identity_id` and the registration fields on session reads.
Names map to durable identities within a project and principal; parent identity
is immutable, and a new identity requires an active same-principal parent.
An owning attempt can register delegated helpers using checkpoint
`contributor_session_ids`. Contributor exclusion is enforced at review claim and
decision, across sessions of the same subagent identity. This is a trusted harness
policy; shared credentials cannot attest independent reasoning. See the
[CLI examples](CLI.md#subagent-identities-and-reviews).

An agent token cannot record a human
approval even when a human operator created it.

Credential issuance displays a newly generated token once. Its mutation receipt
retains the issued credential ID and metadata, never a replayable token value.
If the issuing response is lost, replay returns the original issuance identity
with `secret_unavailable` and instructions to revoke that unused credential and
issue a replacement. It cannot silently create a second token or claim the old
secret can be recovered from its verifier.

For a harness session, the client generates and saves a session ID and a random
session proof before `POST /api/v1/sessions`. Send the proof in
`X-Coordinator-Session-Proof`; store only its verifier on the service. Return the
session ID and state without echoing the proof. Identical retries can recover the
session without a stored replayable secret. The session records its principal,
issuing credential, workstation, harness label/version, and declared capabilities.

Subsequent ownership operations send `X-Coordinator-Session` and the proof header
in addition to the agent token. A session belongs to its issuing credential;
revocation invalidates its authority. `GET /api/v1/sessions/{id}` reconciles it;
`POST /api/v1/sessions/{id}/close` closes it explicitly. Compaction and ordinary
turn completion do not close it or create another session automatically.

The CLI implements this as `agent-coordinator connect`, returning orientation
and current ownership. It reads repository binding and protected local session
state. First connection creates a session; resume reconciles the saved one. The
underlying HTTP workflow remains fully documented for clients without the CLI.

## Orientation and claiming

`GET /api/v1/projects` lists all projects for every authenticated principal.
`GET /api/v1/projects/{project_id}/orientation` returns current rules, policy and
instruction versions, session work, recovery candidates, blockers/decisions,
relevant lessons, candidate tasks, and next actions. Required rules may require
pagination; `instructions_complete: false` prevents new work until acknowledged.
Workflow subjects include service-known precondition blockers and hints for
candidate merge preflight and reopen or agent revise.
`POST /api/v1/sessions/{id}/instruction-acknowledgments` records the project,
policy/instruction revisions, and required section IDs the client has received
and read. Claims require the current complete acknowledgment. This establishes
protocol acknowledgment, not proof that a model understood the prose.

`GET /api/v1/projects/{project_id}/preconditions/{task_or_activity_id}` is a
read-only inspection of current unmet claim/review/integration preconditions.
Task detail and orientation workflow subjects surface the same service-known
blockers. Results are observations and can become stale immediately; guarded
claim and mutation endpoints remain authoritative. Code integration also returns
a local merge-preflight hint: the service cannot inspect workstation Git or
determine whether an immutable candidate conflicts with the current remote target.
A candidate whose submission is superseded or whose task's judged fields changed
must be reopened by an operator or revised by an agent (`reason_code`
`requirements_changed`). Refusals and preconditions that only a human can clear keep
`operation_not_permitted` and add `details.required_actor: "human"` and a
`details.gate` name, or `required_actor: "human"` on the precondition.

`GET /api/v1/projects/{project_id}/next?role=implementer|reviewer` is a read-only
answer to "what should a launch of this role do now". It walks the same
candidates and preconditions a claim would check and returns at most one
`action` (`claim_task`, `recover_task` or `claim_review`) with an exact `call`
template (method, path and body) and the equivalent CLI command. Recoverable
work comes before new work; reviews are ordered by subject priority and must be
independent of the caller. `caller_steps` lists preconditions the caller clears
itself (acknowledging current instructions); `human_queue` counts inspected
candidates that only a human can unblock, and `skipped` counts every blocking
precondition code seen. For every role, `human_queue_items` lists the project's open integrator
reports that need a human, each with `required_actor: "human"`, the report kind
as `gate`, the report and its resolve `call`. With no action,
`retry_after_seconds` suggests a poll interval. Read-access credentials may call it, so a shadow supervisor can log
what it would launch without holding write authority.

`GET /api/v1/projects/{project_id}/state-wait` accepts `target_kind=task|activity|job`,
`target_id`, an `after_state_token` returned by task detail/precondition inspection,
activity detail/precondition inspection, or individual job detail, and optional `timeout_seconds` from
1 through 30 (default 15). It polls at 500 ms intervals, releases its database
connection between reads, and returns immediately when the state token changes.
On timeout it returns the latest snapshot with `timed_out: true`. This is an
opt-in long poll for one task `work_status`, workflow activity, or job state; it
does not hold a transaction, reserve resources, or renew ownership. Expected
detection latency is at most about 500 ms plus request/database scheduling.
Tokens from task and activity details include caller-visible eligibility and
unmet-precondition state, so an instruction acknowledgment can wake a task wait
without changing `work_status`. Job tokens use persisted job state and exclude
derived observation age and freshness, so those display values do not wake a
wait by themselves.

`POST /api/v1/projects/{project_id}/claims` accepts exactly one task ID or a
next-eligible selector. For an explicit task, include its expected revision.
For next-eligible work, supply permitted kinds, capabilities, and the policy/
instruction versions read. The server applies the same eligibility checks to
both forms. `mode` is `work` or `recovery`.

```json
{
  "task_id": "task-example",
  "expected_task_revision": 4,
  "mode": "work",
  "policy_revision": 3,
  "instruction_version": "1"
}
```

A successful claim returns the task, attempt ID, ownership generation, server
lease deadline, remaining lease duration sampled before sending the response,
renewal recommendation, and worktree/resource preparation steps. The client
subtracts elapsed monotonic request time and a safety margin from the returned
remaining duration. It does not assume its wall clock matches the service.

A specific competing claim returns 409 with current status and alternatives.
Next-eligible selection with no available work returns 200 with `claim: null`,
structured queue reasons, and a suggested next check. An empty queue is normal.
Retrying a successful claim retrieves its original receipt and current authority
status; it cannot return a different task as a substitute.

CLI equivalents are `tasks list`, `claim --task <id> --revision <n>`, and
`claim --next`, with explicit project binding and JSON output available.

## Deterministic integrator (P4, additive)

A project's human-managed policy carries `integration_owner`: `agent` (default;
agents integrate through the workflow activities above) or `integrator`. Only a
human may change it; omitting it on `PATCH …/policy` keeps the current value.
The integrator authenticates with an agent credential of class `integrator`
(see the operator access contract). That class may call only the routes below,
and only that class may call them, except that any reader may list integrator
reports and only a human resolves one.

- `GET /api/v1/projects/{project_id}/integrator/queue` lists approved code
  subjects in integration whose submission is current and whose judged task
  fields still match the pinned digest, by subject priority and then time in
  integration. Each item carries the candidate revision, tree and ref, the
  reviewed base, the pinned task digest and any results already recorded for it
  (each with `authority_expires_at`, set while its push authority is outstanding,
  so the integrator can observe that result before pinning a newer one);
  the response adds the current required-check roster and `targets`, the
  distinct `{repository_url, target_branch}` pairs the project integrates
  into: the project's configured repository and branch first, then any other
  pair a listed item's submission pinned. The integrator watches every target
  each cycle, with or without queued items. Each item also carries `reverted`:
  `{revert_task_id, result_id, repository_url, target_branch, landing_range}`
  for every result of the project on the item's repository and target branch
  that a revert task (not canceled) targets, so the integrator can refuse a
  no-op (`r` equal to `t0`) whose candidate commits intersect a reverted
  landing. The response adds `reverts`, the revert tasks awaiting a mechanical
  candidate (see Reverts below): `{id, task_id, result_id, r, title, priority,
  repository_url, target_branch, target}`, where `id` is the revert task id
  and `target` is `{submission_id, result_id, original_task_id, r, t0, c,
  landing_range, reason, evidence}`. Each call records the
  integrator heartbeat. Projects still owned by agents answer
  `integration_owned_by_agents`.
- `POST /api/v1/projects/{project_id}/integrator/results` pins one integration
  result `r` (with its tree, the target tip `t0` and tree it was computed on, the
  landing range and the roster read from `t0`) per submission and `t0`. Sending
  the same result again returns the stored one; a different `r` for the same pair
  is `result_conflict`, and a `c` that is not the submission's candidate is
  `candidate_changed`. A new no-op result (`r` equal to `t0`) whose `c` or
  landing range shares a commit with the landing range, on the same
  repository and target branch, of a result a revert task (not canceled)
  targets is refused with `candidate_reverted_in_history`
  (`details.reverted_results` and the shared `details.commits`): re-land
  candidates must be new commits. Results that are not no-ops are unaffected. The roster is
  `{"required_checks":[{"identity","check_name","workflow_path","workflow_blob"}]}`.
- `POST /api/v1/projects/{project_id}/integrator/receipts` records one GitHub
  Actions check run observed on `r` (`head_sha` must equal `r`, otherwise
  `receipt_head_mismatch`). A run attempt is recorded once; a different outcome
  for it is `receipt_conflict`. The response names the deciding run for that
  check and workflow blob: the latest attempt of the latest run.

- `POST /api/v1/projects/{project_id}/integrator/push-authority` with
  `{result_id}` authorizes pushing `r`. It refuses `checks_not_passed` (details
  list `pending` and `failed` roster checks) unless every roster check has a
  deciding run with the roster's workflow blob that concluded `success`,
  `skipped` or `neutral` (as a GitHub required check passes); `protected_check_missing`
  when the roster drops a check in the project's workflow policy;
  `stacked_on_unapproved` when the landing range carries commits of another
  task's submission that is neither approved nor integrated; `observation_required`
  while another result of the submission holds authority; and
  `integration_target_held` while another integration holds the target. If an
  approving reviewer contributed to another task whose commits the landing range
  carries, the approval is voided, a replacement review is queued (that reviewer
  cannot claim it) and the response is `{"granted":false,"refusal":"approver_is_contributor"}`.
  Otherwise it takes the submission's integration hold and returns
  `{granted:true, r, t0, deciding_runs, roster_ids, protected_ids, expires_at}`;
  authority stays outstanding until the next observation.
- `POST /api/v1/projects/{project_id}/integrator/observations` with
  `{result_id, tip, ancestry, evidence}` records what the integrator saw at the
  target; the service cannot reach the Git host, so it trusts this attestation.
  `contained` (r is in the tip) completes the subject and its integration and
  readies dependents (`published`, or `already_contained` when `r` equals `t0`;
  `published_after_reopen` if a human reopened the submission meanwhile); a
  published revert whose reason is `defect` also proposes its re-land task;
  `equal_t0` (`not_published`) and `moved` (`target_moved`, compute a new result
  on the new tip) end the authority and release the hold. Never infer publication
  from a push exit code.
- `POST /api/v1/projects/{project_id}/integrator/revise` with
  `{submission_id, reason_code, evidence, result_id?, moved_by_result_id?}`
  sends a candidate back to its author; only `conflict`, `check_failed` and
  `reverted_in_history` are accepted, never while push authority is
  outstanding (`observation_required`). `reverted_in_history` sends back a
  candidate whose no-op result `results` refuses with `candidate_reverted_in_history`;
  its `evidence` names the shared commits. The service verifies it: the
  submission's candidate commit, or the landing range of a result recorded
  for it, must share a commit with the landing range, on the same repository
  and target branch, of a result a revert task (not canceled) targets,
  otherwise `not_reverted_in_history`. It counts toward the limits below
  like any integrator revise and never serializes.
  A `conflict` revise may name in `moved_by_result_id` the result whose landing
  moved the target: the newest result the integrator published on that target
  since the candidate's reviewed base. The service resolves it to that
  result's task only when it is a result of this project with a `published`
  observation, and otherwise ignores it. The response carries
  `serialized_after` (a task id, or null), `park_reason` (or null) and, under
  `revise`, the resolved `landing_task_id`. A subject parks for agents (the
  unmet `revise_limit_reached` precondition; agent claims are refused with that
  code) once it has, in the last 24 hours, three agent revises not serialized
  after a landing, or six agent revises of any kind. A revise that leaves the
  subject below the first limit only records the landing. The revise that
  would reach it is serialized when it is a `conflict` citing a landing task
  that no serialized revise of the subject cited in the last 24 hours: the
  subject gains a dependency on the landing task (already satisfied when that
  task is done), the revise applies without counting toward the limit, and
  `serialized_after` names the landing task, so the subject stays claimable.
  Otherwise that revise applies and parks the subject, and `park_reason` says
  why: the reason is not `conflict` (`not_a_conflict`), the landing is missing
  or unresolved (`landing_unknown`), is the subject's own task
  (`landing_is_subject`), is cited by a serialized revise in the window
  (`landing_repeated`, a persistent cycle), or would close a dependency cycle
  (`dependency_cycle`). A revise that reaches the cap of six revises
  reports `serialized_cap`. A revise of a subject that is already parked is
  refused with `details.gate: "revise_limit_reached"` and `details.park_reason`
  (`subject_parked`, or `serialized_cap`). A `check_failed` revise must name
  in `result_id` a result of that submission on which one roster check, under
  the roster's workflow blob, is reproduced: its deciding run concluded
  `failure` or `timed_out`, and at least two of its attempts did. Otherwise
  it is refused with `check_failure_not_reproduced` (details give each roster
  identity's deciding conclusion and failure count under `checks`), or not
  found when the result is not the submission's.
- `POST /api/v1/projects/{project_id}/integrator/reports` with
  `{kind, dedupe_key, task_id?, submission_id?, result_id?, details}` records a
  finding for the digest. `kind` is `privilege_gate`, `flaky`, `fix_target`,
  `unreviewed_landing`, `target_rewritten` or `ruleset_missing`; a
  `privilege_gate` report must name its `result_id`. The first report per kind
  and `dedupe_key` wins: sending it again returns the stored report, whatever
  its `details`, including any resolution. The service sets `requires_human`
  for `privilege_gate`, `target_rewritten` and `ruleset_missing`, and for any
  report whose `details.blocks_subject` is `true` (the integrator sets it on
  `fix_target` reports and on `flaky` reports that leave a subject blocked:
  `no_result` and `rerun_refused`), so a subject never waits unseen. The
  integrator raises `privilege_gate`, failing closed, when any path that
  differs between the target tip and the result lies under `.github/`, is the
  `.github` entry itself, `.gitmodules` or a `CODEOWNERS` file, or is in
  the local-action scope read from the target tip (paths compared ignoring
  letter case). That scope holds the directories named by `uses: ./<dir>` in
  files under `.github/` and in the referenced actions' definitions, the paths a referenced action's `main`, `pre`,
  `post` and local `image` name, and the targets of symbolic links under
  `.github/` or a scope directory. Reading the scope from the target tip is
  safe because every change the result makes under `.github/` is gated
  anyway. A `uses` value the integrator cannot read as a plain path (an
  alias, an escape, an expression) makes the whole repository the scope
  (`unparsed_local_action_reference`), and a gated path that cannot be read
  is `unreadable`. The report lists each gated path with reason codes that
  hint at what changed (for example `adds_secrets`, `removes_permissions`,
  `triggers_changed`, `changed_ci_definition` or `changed_local_action`);
  the codes do not decide the gate. It pushes that result only once
  the report shows `allowed: true`. It raises `ruleset_missing` (once per
  freeze episode, which ends when the rules are back) and `target_rewritten`
  when it freezes a target. Its tip monitor compares each target's tip with
  the tip it recorded last: a tip that does not descend from it is
  `target_rewritten`; a forward move the integrator did not make itself is an
  out-of-band landing. A result's own landing range (the commits from the
  tip `t0` of that result up to its `r`) counts as the integrator's when the
  service granted this integrator push authority for that result; every
  other commit of the move is out-of-band. If any of those
  carries an agent trailer (a `Claude-Session` trailer, or a
  `Co-authored-by` trailer naming Claude or Codex or the address
  `noreply@anthropic.com` or `noreply@openai.com`), it raises
  `unreviewed_landing`, keyed by target, `from` and `to`, with `details`
  `{repository_url, target_branch, from, to, integrator_results, flagged,
  flagged_count, unflagged_count}`; `flagged` lists flagged commits as
  `{sha, subject, trailers}`, cut to fit the details size limit
  (`flagged_count` is the total). The
  report does not block anything; it feeds post-hoc review through the
  digest. Commits without an agent trailer are the user's own work and are
  not reported. Trailer detection is best effort and not a security
  control: a session can strip its trailers, and commits of unknown
  provenance are not detected. When a move has flagged commits, its new tip
  becomes the recorded tip only once the report is stored or refused
  (`409`/`403`); until then the target's items wait
  (`tip_move_unsettled`), so nothing is integrated on that tip, and the next
  cycle reports the whole move from the recorded tip. Flake attribution counts a check's attempts on
  `r` as GitHub does: `success`, `skipped` and `neutral` pass, `failure` and
  `timed_out` fail, and any other conclusion (such as `cancelled`) has no
  result; the latest attempt decides. When it failed for the first time or
  had no result, the integrator reruns that workflow run's failed jobs
  (never a run whose jobs all passed; the GitHub App needs the
  `actions: write` permission) and posts a receipt for every attempt. A pass
  after a failure is a `flaky` report and the result proceeds. A check that two
  reruns of its run leave undecided (its latest attempt has no result, or is
  its only failure), or a rerun request GitHub refuses, is a `flaky` report with `verdict` `no_result` or `rerun_refused`
  and skips the subject; `flaky` reports are keyed by result, check and
  verdict (a refused rerun by result and run). A failure is reproduced once
  the deciding attempt and at least one earlier attempt failed; it revises
  the author with `check_failed` only when the same check's latest attempt
  with a result on the target tip passed, and waits while the tip's run is
  still going. When that attempt failed, or the tip has no result for the
  check, the integrator raises `fix_target` with `verdict` `target_failing`
  or `target_unverified` (one per target, tip, verdict and check) and skips
  the subject until the tip moves or the check passes there.
- `GET /api/v1/projects/{project_id}/integrator/reports` lists a project's
  reports, newest first, for any authenticated reader. `open=true` keeps only
  unresolved ones; `limit` is 1 to 1000 (default 200); `before=<report_id>`
  continues after that report, and each page's `next_before` is the cursor of
  the following page (null on the last).
- `POST /api/v1/projects/{project_id}/integrator/reports/{report_id}/resolve`
  with `{note, decision?}` is human-only (other callers get the
  `integrator_report_resolution` gate). A `privilege_gate` report needs
  `decision` `allow` (its result may be pushed; the report then shows
  `allowed: true`) or `deny`; other kinds take no decision. A resolved report
  is `report_already_resolved`.
- `GET /api/v1/projects/{project_id}/next?role=integrator` returns the queue head
  as an `integrate` action (integrator credentials only).

On integrator-owned projects the agent and human integration routes (claiming or
authorizing an integration activity, publication intents, integration results,
finalize, both publication reconciliations) refuse with
`integration_owned_by_integrator` (`details.required_actor` is `integrator`). An
agent revise that arrives while push authority is outstanding is recorded and
answered with `revise_deferred: true`: it applies if the push does not land, and
becomes a follow-up task if it does; for `author_withdraw` the follow-up is a
revert task (priority 0, reason `author_withdraw`, the withdraw request as its
evidence, review required), or the open revert of that result when one exists.
Only a `published` observation creates that revert; after `already_contained`
nothing new landed, and the follow-up is an ordinary task.
A human reopen always applies immediately. Until a project is switched,
nothing in the agent integration path changes.

### Reverts

A revert task undoes one published integration result R. It is an ordinary
`code` task whose task view carries `revert`: `{result_id, submission_id,
original_task_id, r, reason, evidence, review_required, mode, not_mechanical,
rejection, reland_task_id, created_by, created_at, candidates}` (null on other
tasks). `candidates` lists the integrator's attested candidates `{t0,
submission_id, candidate_commit, candidate_tree, attestation, attested_by,
attested_at}`, so reviewers see what they judge. Every task
view also carries `reverted_by`, the id of the newest revert of that task
that is not canceled (or null); the original task stays `done`. No admission
limit applies to reverts.

- `POST /api/v1/projects/{project_id}/reverts` with `{result_id, reason,
  evidence?, note?, priority?}` creates a revert on an integrator-owned
  project. `reason` is `defect`, `author_withdraw`, `audit_rejection` or
  `human`; `priority` defaults to 1. `result_id` must name a result of this
  project (otherwise not found) with a `published` observation whose task is
  `done` (otherwise `result_not_published`). A second revert of a result
  whose revert task is still planned or open is `revert_exists`
  (`details.revert_task_id`); reverting a revert's own result is allowed. A
  human's revert needs no review and records a `revert.escaped_defect_canary`
  event. An agent's revert needs a reason other than `human` (otherwise the
  `revert_without_evidence` human gate) and non-empty `evidence`, text or
  JSON (otherwise `revert_evidence_required`); its candidate needs the
  project's reviews and at least an agent review even when `review_mode` is
  `none`. That review judges the decision to revert and its evidence, not the
  inverse diff. The creator (for an automatic revert, the author whose
  withdraw lost to the push) and every contributor of the reverted task are
  recorded as contributors to the revert, so none of them can review it
  (`reviewer_not_independent`).
- While `mode` is `mechanical` only the integrator produces the candidate:
  claiming or unblocking the task is refused with `revert_awaits_integrator`,
  it is skipped by next-eligible claims, and a revise of its candidate blocks
  it again and returns it to the queue's `reverts`. Apart from the
  integrator's candidate or not-mechanical report, its only exit is a human
  canceling the task. The revert task stays
  blocked while it waits for its candidate and is listed under the queue's
  `reverts`. The integrator computes the candidate
  itself: `git revert -m 1 R` on the current tip, or the landed range when R
  landed fast-forward.
- `POST /api/v1/projects/{project_id}/integrator/reverts/{task_id}/candidate`
  with `{t0, candidate_commit, candidate_tree, mechanical: true,
  candidate_ref?}` records that candidate as the revert's code submission,
  created by the integrator with the attestation `mechanical` and based on
  `t0`. The submission enters review (an agent's revert) or integration (a
  human's) and then follows the integrator path above. Sending the same
  commit and tree for the same `t0` again returns the same submission
  (`candidate_submission_id`) while that submission is still the revert's
  candidate in review or integration; a different candidate for that `t0`,
  or any candidate for a `t0` whose earlier candidate is superseded, is
  `revert_candidate_conflict`. Other refusals: `revert_not_mechanical`,
  `revert_not_open`, `revert_claimed`, `revert_candidate_exists` (a candidate
  is already in review or integration) and `workflow_policy_required`.
- `POST /api/v1/projects/{project_id}/integrator/reverts/{task_id}/not-mechanical`
  with `{t0, reason, evidence}` (`reason` is `conflict` or `check_failed`)
  reports that the revert cannot be computed mechanically. The service
  supersedes any candidate still in review or integration (refused with
  `observation_required` while its push authority is outstanding) and turns
  the revert into ordinary implementation work: undo the original's
  behaviour while keeping the changes integrated after it, with review
  required. The same `t0` and `reason` again replay; anything else is
  `revert_not_mechanical`. A capped cascade of further reverts is future
  work.
- A review that requests changes on a mechanical revert's candidate rejects
  the decision to revert: the revert task is canceled, its `rejection`
  records the review (`activity_id`, `reviewer_id`, `summary`,
  `rejected_at`), it leaves the queue and the original's `reverted_by` no
  longer names it. A human may create a new revert. A revert converted to
  implementation work is revised like any code change instead.
- When a revert whose reason is `defect` is observed `published`, the
  service creates a planned (not yet admitted) re-land task seeded with the
  original's description, its candidate reference and its acceptance
  criteria, plus the revert evidence as one more criterion; the revert's
  `reland_task_id` names it.

## Attempt operations

All paths below are under `/api/v1/projects/{project_id}`. Attempts have their
own IDs and ownership generation; ownership-dependent input must name the
expected generation. Authorized historical reads/late notes do not require a
live ownership grant. Human review uses browser authentication and CSRF with a
human-owned review attempt; it does not require an agent-token session proof.

| Operation | Endpoint | Required content |
| --- | --- | --- |
| Inspect current work | `GET /attempts/{id}` | Returns current authority and historical outcome |
| Renew | `POST /attempts/{id}/renew` | Generation and health observation; no invented progress |
| Register checkout | `POST /attempts/{id}/checkout` | Workstation, resolved worktree identity, branch, base revision, local path, clean/dirty state |
| Checkpoint | `POST /attempts/{id}/checkpoints` | Generation, summary, current action, next step, blockers, revision/job references |
| Reserve resources | `POST /attempts/{id}/reservations` | Complete requested resource/unit set; grant all or none |
| Delegate reporting | `POST /attempts/{id}/reporters` | Current generation, permitted attempt/job IDs, bounded reporting deadline, client-generated reporter-proof verifier |
| Submit | `POST /attempts/{id}/submit` | Generation, expected task/policy revisions, outcome evidence, handoff, lessons |
| Relinquish | `POST /attempts/{id}/release` | Generation, final checkpoint, ready/blocked disposition and reason |
| Resolve recovery | `POST /attempts/{id}/recovery-resolution` | Generation, inspected evidence, disposition and outstanding holds |
| Add late information | `POST /attempts/{id}/late-notes` | Historical attribution and text/evidence; grants no current authority |

Checkpointing does not implicitly renew ownership. The CLI may explicitly perform
both operations and must report each outcome. This avoids a retry of an old
checkpoint appearing to grant a fresh lease.

Submitting code includes a candidate repository/base/commit/tree identity and
check evidence. Review submission names the immutable reviewed submission and
decision. Integration submission names the target before/after revisions,
publication observation, and verification of the resulting tree. Shared domain
guards reject content that is inappropriate for the task kind or obsolete input.

## Jobs, artifacts, knowledge, and decisions

| Surface | Operations and behavior |
| --- | --- |
| Tasks | Create, get/list, revision-checked edit/admit, dependencies, cancel, supersede; no unrestricted status write |
| Jobs | Register before local launch; append authorized observations; report producer terminal status; inspect without relaunching |
| Artifacts | Explicit bounded upload, authenticated metadata/download, authorized deletion with retained tombstone; external link registration |
| Knowledge | Create/search/get, revision-checked correction and supersession, usefulness feedback, explicit rule adoption under current delegated permission |
| Decisions | Open a scoped question, inspect pending answers, record an answer under the required actor type, preserve the authorization it conveys |
| Project policy | Get current/history, revision-checked update under human or delegated agent authority |
| Events | Cursor-based authenticated history, filter by project/task, no credential values |
| Imports | Upload/inspect source, preview mappings/conflicts, then explicitly apply a versioned preview |
| Exports | Generate a snapshot at an identified record/event revision with provenance and generated-file markers |

Job observations have a separate authorized reporter identity and monotonically
increasing observation sequence. They do not require a still-active implementation
lease, but do require current reporter authorization. Late running observations
cannot overwrite a terminal result. Corrections to erroneous terminal reports
are explicit attributed amendments, not silent history replacement.

Task submission may reference only finalized artifacts. Knowledge updates made
independently use revision checks; new lessons included in a task submission are
stored atomically with that submission. Import apply must detect service changes
since its preview rather than overwriting them.

## CLI behavior and contract checks

Provide JSON input through `--input <file>` or standard input and JSON output
through `--json`. Native PowerShell and Linux-shell examples use the same payload
files. Keep API tokens/session proofs in the protected client configuration or
explicit process environment, not positional command-line arguments or output.

Use exit 0 for success, 2 for invalid input, 3 for authentication/setup required,
4 for denied operations, 5 for state/ownership conflicts, 6 for unsatisfied
requirements, and 7 for temporary transport/service failures. JSON includes the
stable API error code and remedy. The CLI never generates a new mutation key
merely because a response was lost.

Reporter credentials are subordinate to the issuing principal/agent credential.
They permit only the named observations and, when explicitly delegated, bounded
lease renewal. They cannot create sessions or extend their own scope/deadline.
Revoking the parent credential revokes reporters. Attempt expiry stops delegated
renewal; appropriately authorized job observations may continue without
reactivating the expired attempt. Use a separate bearer credential namespace so
reporter credentials cannot be mistaken for full agent API tokens.

Acceptance includes a complete workflow performed once through the CLI and once
through direct HTTP: connect, read instructions, claim, register checkout,
checkpoint/renew, report a job, submit, review, integrate, retrieve lessons.
Exercise lost responses, two-session ownership isolation, revocation, expiry,
stale policy, no-ready-work, and a reviewer who contributed to the candidate.
Every advertised next action and CLI command must match shipped schemas/help.
