# Implementation status

The first executable foundation is implemented. The complete release remains
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
- Linux systemd and Caddy examples, locked dependencies, and Linux/Windows CI.

## Deliberate limits of this foundation

There is no task completion endpoint yet. Submission snapshots, independent
review, serialized target-branch integration, and integrated-result validation
must be implemented together before code tasks can become done and unblock their
dependents. The stored review/integration settings reserve the intended policy;
they do not imply those workflows already execute.

Local job reporters, named resource holds, automatic worktree preparation,
verification evidence, source-checkpoint publishing, shared lessons/search,
decision records, Markdown imports/exports, artifact storage, lifecycle hooks,
and backup/restore are later milestones. Recovery currently records the agent's
inspection attestation. It cannot yet reconcile producer identities automatically.

Human accounts currently have first-admin initialization only; password changes,
account recovery, additional human administration, and token replacement for an
existing agent principal remain work. A lost issuance response can recover the
credential identity, but never its secret: revoke it and enroll a fresh name.

JSON requests are limited to 256 KiB. Task details return the latest 50 attempts,
100 checkpoints, and 50 checkouts; old records remain stored, with complete history
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
CLI and service with two independent harnesses and two projects.

The dashboard has been checked in a real browser at desktop and phone widths.
CI defines native Windows client tests; their existence alone is not evidence that
Windows has passed. No production deployment, 100,000-task benchmark, off-server
backup, or restore rehearsal has occurred.

Continue with [BACKLOG.md](../BACKLOG.md) and [HANDOFF.md](../HANDOFF.md).
