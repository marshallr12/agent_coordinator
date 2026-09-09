# Working on Agent Coordinator

Read README.md, HANDOFF.md, BACKLOG.md, and docs/implementation-status.md first.
PLAN.md and the contract documents define the intended release; implemented
features are listed separately. Do not describe planned endpoints as working.

Use Rust/Axum/SQLite and embedded vanilla JavaScript/CSS. Keep credentials outside
the repository and never print tokens, proofs, passwords, request headers, or SQL
bind values. Do not enable remote execution by the service.

For concurrent implementation, assign disjoint files and separate Git worktrees.
Integrate one reviewed commit at a time. Preserve other worktrees and uncommitted
changes. Use smaller subagents for bounded tasks when appropriate.

Every database mutation must obtain the SQLite writer lock before checking the
current clock, credential/session validity, generation, task ownership, and policy.
Record the effect, idempotency receipt, and event in that same transaction. Never
hold a transaction across network or process work. A retry must reuse its saved
request and key; a receipt must not imply renewed ownership. Protect native
client state against simultaneous processes and interrupted writes.

Run cargo fmt, workspace Clippy with warnings denied, and meaningful workspace
tests. For service/client changes, build the workspace and run scripts/smoke.py.
For UI changes, run node --check web/app.js and verify the running page in a
browser. Native Windows claims require actual Windows CI evidence. Update the
handoff and backlog with completed behavior, tests, limitations, and next steps.

This repository is not yet bound to a running coordination service. Do not invent
a service URL, enrollment token, task completion record, or production deployment.
