# Working on Agent Coordinator

Read README.md and book/src/docs/implementation-status.md first. Use the live
Agent Coordinator service for task selection, ownership, progress, and completion;
HANDOFF.md and BACKLOG.md are historical/reference context, not the work queue.
Follow CLAUDE.md's automatic startup and claim procedure for the next working
session, including its two preferred setup tasks. book/src/PLAN.md and the contract
documents in book/src/docs define the intended release; implemented
features are listed separately. Do not describe planned endpoints as working.

Use Rust/Axum/SQLite and embedded vanilla JavaScript/CSS. Keep credentials outside
the repository and never print tokens, proofs, passwords, request headers, or SQL
bind values. Do not enable remote execution by the service.

For concurrent implementation, assign disjoint files and separate Git worktrees.
Use a separate Cargo target directory inside each worktree; concurrent builds
from different source trees must not share compiled crate metadata.
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
browser. For documentation changes, build with the pinned mdBook version and run the
documentation link/package checks documented in the book. Edit canonical chapters
in book/src; root AGENTS.md, HANDOFF.md, BACKLOG.md, and DURABLE-RECORD.md remain
authoritative and are included directly in the book.
Native Windows claims require actual Windows CI evidence. Update the
handoff and backlog with completed behavior, tests, limitations, and next steps.

This repository's .agent-coordinator.toml binds it to the deployed service and
Agent Coordinator project. On this Windows workstation, use
`./scripts/coordinator.ps1 -Session <unique-stable-harness-name> connect --json`.
Reuse that session name for the same harness; independent harnesses need distinct
names. Credentials belong in the protected user configuration, never this repo.
Read the complete returned orientation before claiming a task. Do not change
task implementation code before a successful claim; renew, checkpoint, and
release using the exact attempt ID and generation. If credentials or service
access are unavailable, report the setup error rather than silently working
outside coordination. Never print secrets or invent ownership/completion records.
