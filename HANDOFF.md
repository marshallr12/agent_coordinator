# Implementation handoff — 2026-09-09

The initial executable foundation is in the Rust workspace. Authentication,
coordination, web, and native CLI work were developed in separate worktrees and
integrated sequentially. The complete design remains in PLAN.md; product decisions
are settled and should not be asked again. See docs/implementation-status.md for
the working subset and BACKLOG.md for next work.

The important coordination invariant is one current attempt per task, enforced
inside a SQLite immediate write transaction. Revocation and expiry are checked
after the writer lock. An agent session proof is tied to its issuing credential.
Release records a handoff but does not finish code work. Completion remains absent
until review, integration, and integrated validation can be implemented together.

Server regression tests cover concurrent claims, expiry boundaries, dependency
cycles, revocation, session isolation, inspected recovery, stale mutation replays,
and restart persistence. Authentication tests exercise HTTP and the host admin
command. scripts/smoke.py exercises the built service and CLI together with
disposable accounts and data. The real web page passed sign-in, project/task
creation, task-detail display, and desktop/mobile layout inspection.

Validation on the integrated foundation: 37 automated tests passed on Linux
with Rust 1.98.1, workspace Clippy passed with warnings denied, formatting and
JavaScript syntax checks passed, the locked dependency audit reported no known
advisories, and the two-harness service/CLI smoke exercise passed. Final browser
checks also covered a 51-task paginated queue, the Ready/Blocked filters, a blocked
release handoff with no active owner, persistent project setup disclosures, and
sign-out. Pagination pauses automatic refresh while additional pages are being
read; Refresh returns to a current first page.

Use README.md for build/run/check commands and docs/CLI.md for client enrollment.
The service has not been deployed publicly. Host, DNS name, and backup destination
remain installation choices. CI contains Linux and native Windows jobs; check the
actual run before treating Windows as verified. The large operating target and
backup/restore targets have not yet been tested.

Review lessons: a replayed renewal must return current remaining time; lost
responses cannot authorize a new key; client state needs a per-session process
lock and durable publication; environment tokens need a trusted origin independent
of repository configuration. Keep secret issuance responses out of receipts,
events, browser storage, and CLI diagnostics. See DURABLE-RECORD.md.
