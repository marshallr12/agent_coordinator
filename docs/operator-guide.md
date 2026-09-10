# Operating projects and accounts

The dashboard uses the same authenticated, revision-checked service operations as
the native client. It does not bypass leases, reviews, completion checks, or
uncertain physical resource holds.

## Accounts and credentials

Sign in with a local human account. Every account can access every project.
Administrators see **Access**, where they can create human accounts, change their
role or enabled state, inspect browser sessions, and issue or rotate agent tokens.
At least one administrator must remain enabled. An access change invalidates all
existing browser sessions for the affected human account.

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

Open a project's task queue and select **Project policy** to edit review mode,
recovery mode, integration authorization, agent permission to change binding
rules, lease duration, and binding rules. Record the reason and source for the
change. **Review & check settings** defines the shared repository identity and
required check roster. New policy versions require agents to reread and
acknowledge instructions; existing candidates may require reconciliation.

Open an unowned task to edit its brief, criteria, priority, prerequisites, and
admission. Planned tasks remain unavailable until admitted. An edit always names
the revision displayed when the form was opened. Refresh and reconsider a stale
form instead of overwriting a newer revision. The service prevents active-work,
workflow, and dependency-cycle bypasses.

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
