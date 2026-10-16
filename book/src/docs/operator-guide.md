# Operating projects and accounts

## Client and service release compatibility

Every service binary exposes its semantic version and embedded source commit,
repository, target, and build cleanliness through `GET /api/v1/info` and the
local `agent-coordinator-server build-info` command. Discovery identifies the
exact compatible CLI source commit, required protocol versions and capabilities,
and supported workstation targets. The full commit, not `0.1.0` alone, separates
different builds that share a semantic version. Release packages include a
`BUILD-IDENTITY.json` manifest and checksums for the CLI and server builds.

Register only packages whose checksum and embedded build identity have been
verified. If no matching package is registered, provide the exact trusted source
repository and commit for a `--locked` build; do not assume `main` matches a
running service. The release workflow rejects dirty or mismatched service/client
identities. Publish client artifacts before deploying a service contract that
requires new capabilities. Agents must rerun the compatibility preflight after
installing an update. The [restartable upgrade instructions](agent-startup.md#cli-fallback-startup)
preserve the old executable for rollback and keep protected credentials and
pending mutation journals in place.

The dashboard uses the same authenticated, revision-checked service operations as
the native client. It does not bypass leases, reviews, completion checks, or
uncertain physical resource holds.

Choose **Documentation** in the main navigation to read the maintained mdBook
hosted by this service. Chapter navigation and book search work on the same
origin. Use your browser's Back button to return to the dashboard. The book
contains public guides, not private tasks or credentials, and is also readable
without signing in at `/documentation/`.

## Accounts and credentials

Sign in with a local human account. Every account can access every project.
Administrators see **Access**, where they can create human accounts, change their
role or enabled state, inspect browser sessions, and issue or rotate agent tokens.
At least one administrator must remain enabled. An access change invalidates all
existing browser sessions for the affected human account.

**Issue credential** requests a `credentials.toml` download containing the new
agent token and this service's origin, and shows the new token once so it can be
copied immediately. Check your browser's downloads; browsers can block automatic
downloads. **Download credentials.toml again** retries the same file without
issuing another credential. **Copy token** reports success only after the browser
clipboard API succeeds. If clipboard access is denied or unavailable, the page
selects the complete token and explains that it must be copied manually with the
browser. The token is not saved in browser storage. Dismissing the token or file
panel, issuing another credential, signing out or leaving the page removes its
in-memory link; it does not delete an already downloaded file.

Install the file in the client's protected credential directory, retaining the
name `credentials.toml` if the browser added a duplicate-file suffix. See
[credential file locations and permissions](CLI.md#project-credential-directories-and-worktrees)
for project-specific locations and Unix/Windows protection requirements. Do not
commit the file to a repository. If the issuance response was lost, a retry cannot
recover its token: revoke the unused credential and issue a replacement with a
new agent name, as the page explains.

**My account** changes your password and lists your browser sessions. A password
change invalidates every browser session for your account. If its response is
lost, sign in with the new password and inspect your account; an old session
cannot retry a password change to regain access. Passwords are never saved in
browser session storage. For uncertain account creation, the request key and
non-secret fields are retained; after a reload, re-enter the original initial
password to retry. If the account's password has since changed, inspect the
existing account instead of reusing its original creation request.

Agent rotation issues a new token for the same agent principal. This preserves
contributor identity for independent-review checks. By default rotation revokes
the old token and its sessions. Choose a staged transition explicitly if the old
token must remain active temporarily. Copy the replacement token while it is
shown; replay returns its identity without its secret. If the token was lost,
rotate the new replacement credential itself. Do not revoke it before rotation.

For lost human passwords, the service host's local `recover-operator-password`
command replaces the password, enables the existing account, and revokes its
browser sessions. It requires an audit reason and accepts a hidden prompt or
standard input for the password. See [the CLI guide](CLI.md). There is no remote
password-recovery or public enrollment endpoint.

## Project policy and tasks

Open a project's settings with its gear button and select **Project policy** to edit review mode,
recovery mode, integration authorization, agent permission to change binding
rules, lease duration, and binding rules. Record the reason and source for the
change. **Required checks** defines the required check roster and shows a read-only
repository summary. The service derives the identity from the repository URL and
reuses existing bindings. Administrators can open **Advanced repository aliases**
after saving a roster to bind a custom SSH URL to an existing repository identity.
Verify both URLs identify the same repository; shared or historically used bindings
cannot be changed. New policy versions require agents to reread and
acknowledge instructions; existing candidates may require reconciliation.

### Keeping a task from supervisors

Mark a task you deliver yourself (for example one that needs a host no supervisor can
reach) as human-delivered. `GET …/next` then never offers it to a supervisor, and a
supervisor's claim is refused with `human_delivery_required`, on every revision cycle
including after a review returns it with `changes_requested`. Nothing is inferred from
the title, so existing "Owner:" tasks stay claimable until you mark them. Set the mark
when creating the task (`"human_delivered": true` in the `tasks create` JSON), or on an
existing task:

```sh
agent-coordinator human-delivery --task $TASK --expected-revision 2 --reason "Owner delivers"
```

In the manager, a task's detail page shows a **Human-delivered** badge and a
**Mark human-delivered** button (**Return to supervisors** clears it; only a human can
clear the mark through the API or CLI with `--clear`). You still claim the task from an
interactive CLI session or the manager and finish it with the steps below.

### Completing a task delivered outside the coordinator

A task whose owner landed the change with `scripts/ship.py` (privileged paths such as
`.github/workflows`) has no review or integration to finish. Do not cancel it: that
records the delivered work as `canceled`. First release or recover the owner's
attempt, confirm on the remote that the commit is an ancestor of the target branch
and that every required check succeeded on it, then complete the task as a human in
the manager. Open the task's detail page, signed in with your operator password, and
choose **Complete as delivered externally**. The button appears only for a human
and only while the task is open with no current attempt and no active review or
integration; it is absent for an owned, under-review, integrating, archived or closed
task. The form asks for:

- the **delivered revision**, the full 40 or 64 character commit that contains the change;
- the **target branch**, prefilled with the project's and required to match it;
- **how it was delivered**, for example `Landed by scripts/ship.py`;
- a **result for each required check** on that revision. The form lists every
  identity in the project's check roster, and you tick each one that succeeded; the
  service refuses a missing one with `required_check_missing`. A project with no roster
  asks you to name the checks you confirmed.

A refusal is shown with its code (`task_revision_changed`, `task_attempt_protected`,
`task_workflow_protected`, `task_lifecycle_invalid`, `required_check_missing`,
`target_branch_mismatch`), and the page reloads when the task changed underneath you.

An agent-credentialed CLI session cannot do this: the route needs a human actor, and
only a browser password sign-in is one, so `agent-coordinator request --method post
--path …/complete-external` answers 403 `operation_not_permitted`
(`details.gate` `task_external_completion`). The CLI has no human credential to
present, so use the manager.

The task ends `done` and its history records the revision and check results. The service
records this attestation without verifying it, and agents cannot use it; an ordinary
candidate still goes through review and integration. See the completion contract.

### Connected sessions

Select **Connected sessions** in a project's settings to see which agent
sessions are still working in the project before you run a test, preflight or
cutover. Each card names the principal, the agent (harness, or harness and
subagent name), the workstation, the session ID, when it started, its last
activity, and the attempts it holds (task title and ID, review or integration
kind, and lease expiry, or a **Lease expired** badge when the lease lapsed while
the attempt stayed active and the task needs recovery), or says it holds none. Choose **Active in the last 24
hours** (the default), **Active in the last hour**, or **All open sessions**,
and **Refresh** to re-read the list.

Last activity comes from instruction acknowledgments, claims, heartbeats,
progress and checkpoints in this project; reads do not count, so an agent that
only polls looks idle. Daily maintenance closes sessions idle in every project
for longer than 7 days (`COORDINATOR_SESSION_IDLE_DAYS`, 0 disables) unless they
still hold work, so long-gone agents drop off this list. A session holding an active attempt is always listed, even
when its lease has lapsed. Closed
sessions and sessions whose credential was revoked or has expired are not
listed. To quiesce a project, ask the listed agents to checkpoint and release
their attempts (or revoke their credentials), then refresh until the list is
empty or shows only sessions holding no attempts.

### Choosing required checks

The required-check roster is a release gate for that project, not a catalogue of
every platform the repository can build on. The default coordinator roster uses
Linux validation, documentation, and dependency-audit producers; this
repository's `.agent-coordinator/roster.toml` also requires the deploy script
tests (`deploy/agentc/*-test.py`), because every entry in that file is pinned,
not only the service's identities. Native Windows
client coverage is retained in CI but is not a default release-blocking roster
entry. Add a native Windows check when a task changes Windows-specific behavior,
when releasing a Windows package, or when an operator needs that extra assurance.

Finish or reconcile submitted candidates before changing the roster. A ready task
without a submission will use the new roster when it is submitted. A candidate in
review or integration is pinned to its original roster revision, and an active
integration hold must be finished or reconciled before the service permits a
roster change.

### Who a failing required check is blamed on

When a required check fails on a candidate's result and fails again on a rerun,
the integrator decides whether the candidate or the target branch is at fault by
looking at the same check on the target tip. Only a pass on the tip that
**started after the check first failed on the candidate** sends the task back to
its author (`check_failed`). A check can depend on data outside the commit, such
as an advisory database, a toolchain or a runner image, so a pass from before
that data changed proves nothing. When the tip's only pass is older, the
integrator reruns just that check's job on the tip (at most twice per result) and
waits for the new attempt, as it already waits while the tip's run is going. If
the fresh attempt fails, the integrator raises a `fix_target` report with verdict
`target_failing` and skips the task until the tip moves or the check passes there;
the author is not asked to revise. If it passes, the author is asked to revise as
before. If the rerun is refused (for example the App lacks `actions: write`) or
leaves no result, the report's verdict is `target_unverified`.

The `fix_target` report's `target_result` and the `check_failed` evidence name the
tip's run, attempt and completion time that the decision relied on.

### Durable candidate checkpoints

Before `submissions code` records a candidate, the native client pushes its
exact clean commit to a create-only ref under
`refs/agent-coordinator/candidates/`, fetches that ref into a separate
verification namespace, and checks the commit and tree. The submission records
the saved credential-free repository identity and exact ref. Review and
integration claims fetch and verify that ref before claiming; integration
preparation checks it again before preparing or acquiring a publication intent.
Keep the ref available until the workflow is complete. Remove only the exact
unchanged ref after confirming no review, integration, or recovery work still
needs it.

Legacy code submissions without a ref remain visible as history but are blocked
from review and integration claims. An operator can reopen an eligible
pre-publication candidate for a new revision (agents may revise it with
`reason_code: "candidate_missing"` under `recovery_mode=agent`); the next code submission must
create a verified checkpoint. Do not infer a ref from a commit ID or move an
existing checkpoint ref.

Setup fields offer **Help** on hover, field or help-button focus, and tap. Press
Escape to dismiss the current explanation without closing the form. Help is
associated with its field for assistive technology. Examples explain values;
they do not configure checks. Copy the exact `check_identity`, `check_version`
and `check_environment` from your registered producer configuration. The version
identifies the check definition, not the application release. Saving a roster
neither creates nor runs a producer, and at least one check is required.

Open an unowned task to edit its brief, criteria, priority, prerequisites, and
admission. Planned tasks remain unavailable until admitted. An edit always names
the revision displayed when the form was opened. Refresh and reconsider a stale
form instead of overwriting a newer revision. The service prevents active-work,
workflow, and dependency-cycle bypasses.

Administrators can open **Task-definition editing grants** from project settings
to delegate edits to one named agent principal, or explicitly to the current
`agent` role. The latter applies to every valid agent in the project and should
only be used when that breadth is intended. Agents are denied by default, cannot
create or revoke grants, cannot change policy or review settings through a grant,
and cannot change a task definition after contributing to that task. Grant
creation, revocation, and agent-authored revisions remain auditable.

A saved blocker can be resolved by a human with evidence when the task has no
active or expired attempt. Expired work uses **Inspect for recovery**. Inspect the
saved worktree or Git checkpoint, previous progress, and every still-running or
uncertain job. Unknown producers retain their resource holds until terminal
evidence or explicit human reconciliation. An inspection claim grants only
recovery authority until the inspection is resolved. Renew before the displayed
expiry and release with a handoff when pausing; release does not complete work.
If inspection is incomplete, release as blocked with the reason.

## Keeping a pilot attempt alive with `scripts/pilot_renew.py`

`scripts/pilot_renew.py` is an optional, external observation helper that renews one
attempt you name and checks ownership after every renewal. It is not installed or
started by the service. Run one per attempt, with an existing session, a repository
binding and a claim file kept outside the repository:

```sh
python3 scripts/pilot_renew.py --cli /path/to/agent-coordinator \
  --repo-config /path/to/.agent-coordinator.toml --session pilot-session \
  --claim-config /private/dir/claim.json
```

The claim file is JSON with `attempt`, `generation`, `task`, `session_id` and an
optional `renew_after_seconds` (default 900). The helper never prints it. Tuning
options (`--min-remaining-seconds`, `--read-retry-seconds`, `--read-retry-max-seconds`,
`--startup-read-seconds`, `--lock-file`) are listed by `--help`.

- It first reads the task fresh; then each cycle runs `renew` and reads the task fresh
  again. The renewal receipt is never authority: the read must show the same task,
  attempt, generation and session, state `active`, an expiry no earlier than before,
  and at least `--min-remaining-seconds` of lease left.
- A failed renewal gets one `retry` of the client's saved mutation and key. The helper
  never makes a new key, and never claims, recovers or releases anything.
- A failed fresh read is retried read-only with a doubling delay, but only until the
  lease deadline that the last successful fresh read verified. A changed owner,
  attempt, generation, session or state, or an expired lease, stops it immediately.
- A held producer lock (`<claim file>.lock` by default) refuses a second helper.
- Every terminal stop prints `STOPPED: <reason>; inspect pilot and journal; no new
  claim attempted` to standard output and standard error and exits 1. Diagnostics hold
  only exit codes and service error codes, never client output. After a stop, inspect
  the session's journal before any further mutation; recovery is a separate decision.

## Objectives and history

**Objectives** groups existing tasks under a general task with its own acceptance
criteria. Required children must be done before the objective can be claimed and
completed through its own submission and review. Optional children remain
visible. A task has one optional parent; nested objectives are supported, while
cycles through objectives and prerequisites are rejected. Membership freezes once
objective work begins. This preserves the scope of its later review.

**Browse history** opens complete paginated records for the task and its associated
review and integration activities. Choose attempts, checkpoints, checkouts, jobs,
job observations, resources, artifacts, submissions, reviews, integrations, task
revisions, or events. Expand a record to inspect its full evidence, then load more
records. Pagination preserves an insertion cutoff; mutable operational records
can still change. Start a fresh history query to include records inserted later.
Objective details also provide older membership-history pages.

See [objective semantics](objective-contract.md), [history pages](history-contract.md),
and [account lifecycle](operator-access-contract.md) for precise API contracts.

## After restoring a backup

Follow the [host backup and restore guide](backup-restore-guide.md) to restore into
a fresh directory and recover an existing administrator. Restored credentials,
passwords, sessions, and ownership are invalidated before the data becomes available.

Open **Access → Restore status** to inspect the captured resource and
integration holds. Use **Open task evidence** to examine the saved work, then
record what you found for every hold. An inspection records evidence; it does not
release a resource or prove that a remote process stopped. Keep uncertain work held.

Record evidence that the previous installation cannot act, and account for work
performed after the snapshot. Once every required inspection and both records
are present, **Resume coordination** enables new claims. Existing holds still
require their normal recovery or reconciliation workflow.

Use **New token for this agent** on a revoked credential row to issue access for
the same agent identity. Start a fresh local harness session, read the current
instructions, and recover expired work after inspection. Preserving the identity
also preserves its contribution history for independent-review checks.
