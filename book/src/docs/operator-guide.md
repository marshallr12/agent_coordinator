# Operating projects and accounts

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

### Choosing required checks

The required-check roster is a release gate for that project, not a catalogue of
every platform the repository can build on. The default coordinator roster uses
Linux validation, documentation, and dependency-audit producers. Native Windows
client coverage is retained in CI but is not a default release-blocking roster
entry. Add a native Windows check when a task changes Windows-specific behavior,
when releasing a Windows package, or when an operator needs that extra assurance.

Finish or reconcile submitted candidates before changing the roster. A ready task
without a submission will use the new roster when it is submitted. A candidate in
review or integration is pinned to its original roster revision, and an active
integration hold must be finished or reconciled before the service permits a
roster change.

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
