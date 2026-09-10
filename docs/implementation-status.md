# Implementation status

The foundation, job/worktree evidence, reviewed completion, shared-record,
operator-workflow, backup/restore, Linux operational controls, and MCP are implemented.
Linux release acceptance passed; see the retained evidence below.
The complete release remains
defined by [PLAN.md](../PLAN.md); this document records current behavior.

## Working now

- Authenticated stateless Streamable HTTP at `/mcp` with 56 typed tools routed
  through the existing REST authorization and mutation checks. A protected native
  launcher shares the exact saved harness identity with a trusted foreground MCP
  client and its native CLI children. See the [MCP guide](mcp-guide.md).

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
- Immutable code/general submissions pinned to task and policy revisions, with
  acceptance evidence, handoffs, and full Git commit/tree identities for code.
  Active jobs and unresolved physical holds prevent submission.
- Independent agent, human, or both review. Contributor principals and sessions
  cannot perform an independent agent review. Findings and decisions stay bound to
  the exact submission; requested changes require a new candidate and fresh reviews.
- An explicit human-managed required-check roster and canonical repository key.
  Integration holds serialize every project sharing that repository/target pair.
  Projects can permit automatic integration or require a human authorization.
- Native integration prepares a candidate-containing result in an isolated worktree,
  validates registered producers against its exact commit/tree and check roster,
  and requires fresh service authority before a compare-and-swap Git publication.
  Once push intent is saved, retry only observes; it cannot launch another push.
- Code tasks become done and release dependents only after required approval,
  known publication, exact successful check receipts, fresh remote observation,
  and resource quiescence. General tasks finish after their required reviews.
- Dashboard submission evidence, review decisions, human review/authorization,
  publication reconciliation, and project review/check settings. Publication
  reconciliation preserves the original evidence and requires a new integration
  activity and fresh validation; it never fabricates a successful check.
- Revisioned lessons, facts, rejected approaches, and checkpoints; applicability,
  explicit cross-project sharing, provenance, usefulness feedback, and immutable
  revision history. Submission lessons and finalized artifact references commit
  atomically with the immutable submission and retain their original revisions.
- Plain-text FTS context search with bounded records, full current binding rules,
  explicit budget/truncation guidance, and current scoped decisions. Rule changes
  record provenance and preserve policy history.
- Scoped decisions with typed allow/deny/defer answers, required actor, exact
  task/policy revisions, environment, conditions, expiry, and reopening history.
  Blocks apply to selection, work, review, integration, and displayed authority;
  inspection, checkpointing, and release remain possible while work is blocked.
- Artifact links and streaming uploads with exact size/SHA-256, configurable quota
  and disk reserve, bounded concurrency, explicit expiry/deletion metadata, and
  safe storage reconciliation. Native upload journals preserve exact bytes and
  keys; retries reauthenticate and downloads verify digest before publishing a
  new file without overwriting existing output.
- Immutable Markdown previews and human-gated historical apply. Stable source
  identities, event/revision conflict checks, and durable historical evidence
  prevent reimports from reopening completed work or completing active attempts.
  Bounded, snapshot-consistent Markdown exports retain complete provenance.
- Dashboard and CLI access to lessons, decisions, context, artifact metadata,
  imports, and exports; the dashboard includes correction, human decision answers,
  rule editing/history, readable import previews, and snapshot downloads.
- Human account creation, revision-checked access changes, self-service password
  changes, browser-session inspection/revocation, and audited host password recovery.
  Agent credential rotation preserves the agent principal and returns the new secret once.
- Full policy editing, planned-task admission, inspected human recovery and blocker
  resolution, with matching native task, policy, objective, and history commands.
- Objectives with required/optional children, revisioned membership frozen once work
  starts, combined dependency-cycle checks, and their own acceptance/review workflow.
- Complete task evidence pagination across 12 record kinds, including associated
  review/integration activities, with bounded pages and scoped insertion snapshots.
- Linux systemd and Caddy examples, locked dependencies, and Linux/Windows CI.
- Online SQLite/artifact backups with exact digest and database verification,
  self-contained snapshots, 24 hourly/30 daily retention, and hourly systemd units.
- Fresh-directory restore invalidates credentials, passwords, sessions, reporters,
  ownership, and integration authorization before publication. A pause requires
  hold inspections, old-installation fencing, and post-snapshot gap reconciliation.
  Unknown jobs and physical/integration holds remain preserved. The dashboard
  records the checklist and can issue a fresh token for the same agent identity.

- Durable clock rollback detection expires task and reporter authority while
  preserving physical and integration holds. The dashboard and host command
  record time reconciliation; restored snapshots preserve their creation-time
  cutoff. Concurrent read authentication can share a newly committed clock sample,
  while every mutation retains its own writer-lock time and ownership checks.
- Bounded host maintenance compacts expired receipt results and redundant old
  running-observation summaries. Permanent request identities and semantic
  history remain intact. Daily systemd units provide scheduled maintenance.
- Known-prefix version-1 snapshots from schema 12 onward verify without source
  changes and migrate only inside a private restore copy. Native release tooling
  verifies package checksums, offline links, systemd/HTTPS installation, and
  reproducibility. The packaged server passed the sustained constrained workload below.

## Deliberate limits of this milestone

Completion is enforced through workflow activities; there is no unrestricted
status edit. Configure a canonical repository key and at least one required check
before submitting code. Check identity, version, and environment are registered
metadata, and all workstation evidence remains a client attestation. A malicious
credential can lie about source or a producer; this service is not remote attestation.
Independent review rejects every contributing principal as well as its sessions;
it cannot establish whether separately enrolled credentials use the same model.

Jobs and submissions require clean committed source snapshots. The CLI does not
upload source: publish candidate checkpoints to an appropriate Git remote and
fetch them on the receiving workstation before review or integration. Logs remain bounded on the workstation and can be uploaded explicitly as
service artifacts; publishing source checkpoints remains a Git operation. A guardian lost after launch may leave an unknown
result even if the process has exited. Reconnection never invents an exit result;
inspect the journal, surviving process, and physical resource before reconciliation.

Lifecycle-hook adapters remain later work. Workstation reports are
client attestations, not remote filesystem inspection or hardware fencing.
Supported commands do their work in the foreground; a launcher exiting does not
verify completion of detached child or external work. Retain holds for that work.

Human administrators manage human access and agent credentials through the dashboard.
Host recovery requires access to the service host and records the supplied reason.
A lost token response never replays the secret: rotate the replacement credential
again using its returned identity. Account-creation retries retain their original
key and require the original password, which the dashboard never persists; the
same administrator can authenticate again to reconcile that request safely.

Instruction version 7 adds clock reconciliation and receipt-retention guidance
to the existing restore, operator, worktree/job, completion, and shared-record
instructions.
Existing sessions must fetch and acknowledge the new instructions before new claims.

JSON requests default to 1 MiB and can be configured lower; artifact bytes use a
separate 16 MiB hard limit. Task details return the latest 50 attempts,
100 checkpoints, 50 checkouts, and bounded job/resource evidence; old records remain stored, with complete history
pagination available separately. Mutation receipts replay for 30 days; expired keys
remain reserved so a late retry cannot duplicate an old operation. Artifact retention cleanup and storage quota enforcement are implemented;
host maintenance compacts replay payloads and redundant old health summaries.
Permanent semantic events, task/handoff/lesson history, and request identities are
retained; this bounds maintenance work, not total historical storage.

Lease timing uses protected service time. Hosts should maintain synchronized time;
a material rollback pauses new authority until reconciliation, and a restore
invalidates existing authority. A rollback during downtime that remains above
the last durable observation cannot be detected without a trusted external clock.
Deployment hardware, public hostname, off-server backup destination, and the real
Windows workstation acceptance remain installation or operator inputs.

## Evidence and next work

The server tests cover authentication boundaries, revocation, session isolation,
instruction versions, project boundaries, dependency-cycle rollback, a concurrent
claim race, exact deadline expiry, recovery fencing, stale response replay, and
ownership persistence across restart. The combined smoke exercise tests the built
CLI and service with two independent harnesses and two projects, isolated worktree
preparation, duplicate-free reconnect, job evidence, and explicit capacity release.
Job tests exercise global capacity, reporter authority after task/session loss,
parent revocation, bounded renewal, terminal observations, and human reconciliation.

Workflow regressions also exercise independent review, concurrent conflicting
review decisions, exact-source checks, a cross-project integration claim race,
revoked ownership, manual recovery at the deadline, and complete publication
reconciliation followed by a replacement integration and fresh checks.

The dashboard was checked with disposable data at desktop and phone widths:
human review and completion, code review and authorization, check-roster editing,
stale-candidate recovery, preserved history, responsive layout, and sign-out.

Linux workspace checks, the real service/CLI smoke exercise, native Windows
client/CLI/local-runner tests, and the locked dependency audit passed for workflow
implementation commit `5e6f19b` in
[CI run 34424382043](https://github.com/marshallr12/agent_coordinator/actions/runs/34424382043).
The same revision passed all 84 local workspace tests, warnings-denied Clippy,
formatting, JavaScript syntax checks, and the complete smoke exercise.
No production deployment or actual off-server transfer is claimed. Later
capacity and recovery evidence appears below.

Continue with [BACKLOG.md](../BACKLOG.md) and [HANDOFF.md](../HANDOFF.md).

### Shared-record review evidence

All 114 workspace tests pass locally (the full suite plus focused checks after
the final context/selection additions), along with warnings-denied Clippy,
formatting, JavaScript syntax, workspace build, and the final service/CLI smoke.
The combined workspace tests cover atomic submission lessons/artifacts, scoped
work selection, decision expiry and stale authority replay, knowledge correction
and feedback, FTS applicability, historical import conflicts and reimports,
export bounds/provenance, artifact quota/expiry/revocation/concurrent cleanup,
and native transfer integrity. The complete built smoke exercise also covers
knowledge correction/context, human-required decisions, historical apply,
provenance-bearing export, exact upload retry, and download bytes/no-overwrite.

Browser verification with disposable data covered literal HTML as inert text,
lesson revision 2 and feedback, denied/reopened/allowed decisions, policy-change
invalidation, planned/done imported task states, context search, rule provenance,
artifact links, export, project selection, and responsive layout without
horizontal overflow at approximately 390 CSS pixels. No browser errors were
reported. Implementation commit `ca59ee0` passed Linux format/Clippy/tests/build,
the full service/CLI exercise, native Windows client/CLI/local tests, and the
locked dependency audit in
[CI run 34431398271](https://github.com/marshallr12/agent_coordinator/actions/runs/34431398271).
The earlier CI link above applies to the prior milestone.

The remaining release sequence is Linux
acceptance, MCP (6.1), mdBook (6.2), and operator-initiated Windows acceptance (7).


### Operator-workflow review evidence

All 129 workspace tests pass, along with warnings-denied Clippy, formatting,
JavaScript syntax checks, the workspace build, and the complete built service/CLI
smoke exercise. Additional regressions cover account access races, the last
administrator, password/session invalidation, same-principal token rotation,
reauthenticated account-creation replay, objective gates/cycles/revisions, and
complete history pagination with exact workflow evidence and credential redaction.

Browser checks covered policy changes, task admission, history revisions,
objective membership, inspected recovery, blocked release and human resolution,
account disabling, token rotation, session revocation, password change and new
sign-in. A proxy dropped an account-creation response after commit; after session
expiry and reauthentication, re-entering the original password recovered exactly
one account and one receipt. Final embedded assets matched their source, no browser
errors were reported, and the phone view fit 375 CSS pixels without overflow.

History cursors use insertion row identifiers. Future database maintenance must
preserve those identifiers or explicitly invalidate outstanding cursors; never
silently reuse them after an in-place VACUUM or a restore.

Implementation commit `865365d` passed Linux workspace checks and smoke, native
Windows client/CLI/local-runner tests, and the dependency audit in
[CI run 34440915688](https://github.com/marshallr12/agent_coordinator/actions/runs/34440915688).
The final audit clarification labels host password recovery's initiator separately
from its target account and passed a focused regression and workspace Clippy.

### Backup and restore review evidence

All 140 workspace tests pass, along with warnings-denied Clippy, formatting,
JavaScript checks, the workspace build, and the full existing smoke exercise.
New regressions cover exact manifest/database artifact membership, digest damage,
missing and symlinked files, retention buckets, artifact cleanup locks, repository
overlap rejection, standalone verification without parent mutation, atomic
no-overwrite publication, and read-only backup schema checks.

Restore tests cover old credential/session/reporter rejection, globally reserved
pre-restore mutation keys, repeated restore of a paused snapshot, preserved holds
and unknown jobs, complete reconciliation gates, and fresh manual integration
authorization with prior evidence retained. Independent source review found no
remaining authority blocker; the main agent reviewed and tested the combined code.

The built service/CLI restore exercise completed in 6.9 seconds after the final
storage changes. It captured a live database and pinned artifact, verified a
copied bundle, refused an existing destination, stopped the original service,
restored into an absent directory, rejected old access, recovered the administrator,
preserved a checkpoint and hold, completed reconciliation, and reconnected the
same agent principal with a fresh token/session and a higher recovery generation.
The copied bundle was another local directory, not a real off-server destination.

Browser checks covered task evidence links, multiline inspection evidence, both
reconciliation attestations, resuming coordination without releasing a hold, and
same-agent credential replacement with the secret cleared before inspection.
The layout fit 375 CSS pixels without horizontal overflow; no browser errors
were reported. The fixture and browser tab were closed afterward.

Snapshots are full independent copies and can consume roughly 53 times live
database/artifact storage, plus working space. Engine limits and the cooperative
45-minute deadline are documented in the backup contract. The rehearsal is not
a production-size restore benchmark or proof of host-loss protection. Installation
requires destination-side verification and a measured recovery exercise.

Backup/restore implementation `283d9d0` passed Linux workspace checks and both
smoke exercises, native Windows client/CLI/local-runner tests, and the dependency
audit in [CI run 34445152452](https://github.com/marshallr12/agent_coordinator/actions/runs/34445152452).

### Linux operations acceptance evidence

Candidate `a16d879` passed all 160 workspace tests, formatting, warnings-denied
Clippy, build, both service/CLI smoke exercises, native Windows client/local-runner
checks, and dependency audit in
[standard CI](https://github.com/marshallr12/agent_coordinator/actions/runs/34459987850).
[Release CI](https://github.com/marshallr12/agent_coordinator/actions/runs/34459987847)
passed byte-identical native Linux/Windows packages, Ubuntu 24.04 systemd/HTTPS
installation, restart/reconnect, both timers, and verified backup.

The capacity job downloaded the accepted Linux package and verified its archive
and executable identities. That exact server completed 90,000 requests over
30 minutes at 50 requests/second with 20 projects, 50 sessions, and 100,000
historical tasks. Overall p95 was 20.974 ms and p99 was 21.695 ms. Every operation
met the 500 ms p95 limit; there were no unexpected errors, all 50 sessions renewed
360 times, and exact ownership and artifact checks passed. The shared two-CPU /
4-GiB / no-swap scope peaked at 664.1 MiB; server RSS peaked at 84.6 MiB.
The concurrent upload and online backup overlapped during traffic. The captured
snapshot restored with integrity, history, and authority invalidation verified
in 6.511 seconds; that measurement stops at the reconciliation pause.

The actual prior executable upgraded a schema-12 database to schema 16 while
preserving principals, credentials, sessions, an active attempt, and checkpoints.
Instruction version 6 was rejected for new claims until version 7 acknowledgment.
Its old snapshot verified unchanged and restored with the new executable while
invalidating old credentials, sessions, and ownership. Historical search remained
available. The separate small end-to-end restore smoke passed in 7.3 seconds.
Browser checks covered clock pause, multiline recovery evidence, reconciliation,
and a 375-pixel layout without console errors or overflow.

Main-agent review is complete. The [retained acceptance evidence](linux-capacity-evidence.md)
includes the complete sanitized capacity report and its identity. The next documentation item is mdBook 6.2. The operator will commence the actual Windows
workstation exercise as item 7; CI does not replace it. Production deployment,
off-server transport, and full operator-led recovery remain installation checks.
