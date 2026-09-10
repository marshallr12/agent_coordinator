# Implementation handoff — 2026-09-10

Backlog item 4 is implemented and reviewed locally: operator account/session
management, password recovery, agent credential rotation, policy editing,
objective grouping, full task history, inspected operator recovery, and native
commands. All 129 workspace tests, warnings-denied Clippy, formatting, workspace
build, JavaScript checks, the full smoke exercise, and browser verification passed.
Required Linux/Windows CI is pending for this milestone; integrate only after it passes.
Then continue sequentially with item 5, backups and restore authority invalidation.

Instruction version 5 describes the implemented workflow. Existing harnesses must
read and acknowledge it before new claims. Objective membership freezes after work
begins; required children must complete before parent work, and the parent still
has its own acceptance and review. History pages preserve evidence from associated
review/integration activities. Row-based cursors must be invalidated if restore or
future maintenance changes their identifiers.

Main-agent review corrected browser retry persistence across sign-in, original
password re-entry, secret-free storage, narrow account-creation replay across new
browser sessions, objective revision use, recovery handoff fields, and current
credential/ownership labels. The same-admin replay helper is limited to account
creation; task and agent mutations retain session-bound fingerprints. Current
browser authorization and administrator status are checked under the writer lock.

Browser verification used disposable data, including a lost response after account
creation, session expiry, successful same-key replay without a duplicate, account
and token controls, inspected recovery, objectives, and phone layout. See
[implementation status](docs/implementation-status.md) for evidence and limits and
[operator guide](docs/operator-guide.md) for usage. No production account was changed.

Item 3 remains validated by [CI run 34431398271](https://github.com/marshallr12/agent_coordinator/actions/runs/34431398271).
Each concurrent worktree must retain its own Cargo target directory.

Complete remaining items through 6.2 without operator intervention unless missing
information is essential. Item 6 is Linux acceptance; MCP is 6.1 and mdBook is 6.2.
The operator will commence native Windows workstation acceptance as final item 7;
keep Windows CI checks but do not substitute them for that exercise. No production
deployment, load benchmark, or backup/restore rehearsal has occurred yet.
