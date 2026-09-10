# Implementation status

The foundation and job/worktree evidence milestone are implemented. The complete release remains
defined by [PLAN.md](../PLAN.md); this document records current behavior.

## Working now

- Axum HTTP service with embedded vanilla JavaScript/CSS dashboard; SQLite WAL,
  foreign keys, migrations, transactional event records and mutation receipts.
- Local administrator initialization and password sign-in; Argon2id hashes,
  protected browser cookies, origin/CSRF checks, bounded login attempts, and
  human-admin issuance/revocation of separately named agent credentials.
- Multiple projects visible to every authenticated principal. Human operators
  create projects and control delegation. Agents create tasks and can change
  project rules when explicitly delegated; they cannot grant themselves permissions.
- Tasks with acceptance criteria, priorities, immutable definition revisions,
  planned/open lifecycle, same-project prerequisites and cycle rejection.
  Listings are observations. A transactional claim is the only ownership grant.
- Separate harness sessions with random persisted proofs bound to the issuing
  credential. One current attempt per task, monotonic ownership generations,
  renewable deadlines, separate heartbeat/progress timestamps, and revocation
  checks after acquiring the database writer lock.
- Checkpoints and release handoffs; release requeues or blocks a task and never
  marks it complete. Expired/revoked ownership requires a recovery claim and an
  explicit saved-work/job inspection record. A project can restrict recovery to
  humans. The human can resolve and release inspected work for an agent to claim.
- Clean-checkout registration with workstation/identity conflict checks and
  readable checkout metadata. Each implementation attempt should use its own
  worktree. The service records client attestations; it does not inspect a remote
  filesystem or execute Git itself.
- Public authentication help and service-delivered project instructions with
  version acknowledgments. Task/project/event lists use bounded cursor pages.
- Native CLI with origin-bound credentials, separate local harness sessions,
  JSON output, durable pending requests, and same-key retries. The dashboard
  supports project/task creation, task and checkpoint inspection, and credential
  administration; it has responsive layouts and no third-party scripts.
- Native worktree preparation with durable intent, remote identity and clean-source
  checks. Repeating preparation reconciles the same checkout; it never resets or
  deletes existing work.
- Global named capacity resources and atomic multi-resource reservations shared
  across projects. Holds survive task expiry, credential revocation, session loss,
  and missing observations. Ordinary release/recovery cannot bypass unresolved work.
- Locally launched jobs with stable producer identities, a durable launch journal,
  bounded local logs, and a detached guardian. Reconnect observes the existing job;
  uncertain launch intent never authorizes another producer. Linux boot/start time
  and Windows process creation time distinguish process identity from a reused PID.
- Scoped job reporter credentials, ordered idempotent observations, and optional
  bounded task renewal tied to a specific live harness. Observation authority and
  task ownership are separate. The service never launches workstation processes.
- Dashboard resource administration and task job/hold evidence, including explicit
  human reconciliation with recorded termination/isolation evidence.
- Linux systemd and Caddy examples, locked dependencies, and Linux/Windows CI.

## Deliberate limits of this milestone

There is no task completion endpoint yet. Submission snapshots, independent
review, serialized target-branch integration, and integrated-result validation
must be implemented together before code tasks can become done and unblock their
dependents. The stored review/integration settings reserve the intended policy;
they do not imply those workflows already execute.

Jobs currently require a clean committed source snapshot; dirty snapshots and
exact-source verification receipts belong to the submission milestone. Logs stay
on the workstation and are bounded; service artifact uploads and source-checkpoint
publishing remain later work. A guardian lost after launch may leave an unknown
result even if the process has exited. Reconnection never invents an exit result;
inspect the journal, surviving process, and physical resource before reconciliation.

Shared lessons/search, decision records, Markdown imports/exports, lifecycle-hook
adapters, and backup/restore remain later milestones. Workstation reports are
client attestations, not remote filesystem inspection or hardware fencing.
Supported commands do their work in the foreground; a launcher exiting does not
verify completion of detached child or external work. Retain holds for that work.

Human accounts currently have first-admin initialization only; password changes,
account recovery, additional human administration, and token replacement for an
existing agent principal remain work. A lost issuance response can recover the
credential identity, but never its secret: revoke it and enroll a fresh name.

Instruction version 2 includes the implemented worktree/resource/job sequence.
Existing sessions must fetch and acknowledge the new instructions before new claims.

JSON requests are limited to 256 KiB. Task details return the latest 50 attempts,
100 checkpoints, 50 checkouts, and bounded job/resource evidence; old records remain stored, with complete history
pagination still to be added. Mutation receipts replay for 30 days; expired keys
remain reserved so a late retry cannot duplicate an old operation. Retention cleanup
and storage-quota enforcement are not implemented.

Lease timing uses the server clock. Hosts should maintain synchronized time; a
restore or server clock rollback does not yet invalidate all existing authority.
The foundation is suitable for development exercises while release recovery and
completion safeguards are being built.

## Evidence and next work

The server tests cover authentication boundaries, revocation, session isolation,
instruction versions, project boundaries, dependency-cycle rollback, a concurrent
claim race, exact deadline expiry, recovery fencing, stale response replay, and
ownership persistence across restart. The combined smoke exercise tests the built
CLI and service with two independent harnesses and two projects, isolated worktree
preparation, duplicate-free reconnect, job evidence, and explicit capacity release.
Job tests exercise global capacity, reporter authority after task/session loss,
parent revocation, bounded renewal, terminal observations, and human reconciliation.

The dashboard has been checked in a real browser at desktop and phone widths.
Native Windows client, CLI, and local runner tests, plus Linux workspace checks
and the service/CLI smoke test, passed for implementation commit `342f393` in
[CI run 34419243344](https://github.com/marshallr12/agent_coordinator/actions/runs/34419243344).
The same run passed the locked dependency audit.
No production deployment, 100,000-task benchmark, off-server
backup, or restore rehearsal has occurred.

Continue with [BACKLOG.md](../BACKLOG.md) and [HANDOFF.md](../HANDOFF.md).
