# First-release scope and remaining decisions

Status: planning. Confirmed choices are in [PLAN.md](../PLAN.md). This document
separates remaining product choices from engineering defaults the implementer
can specify without a separate interview question for every field or timeout.

## Product choices still needed

| Choice | Recommendation | Why the answer matters |
| --- | --- | --- |
| Integration authority | Agents may integrate when project policy and required review allow it; humans manage standing permissions | Determines whether unattended code work can reach completion |
| Shared knowledge | Project lessons plus an explicitly curated common collection; agents publish lessons, humans approve binding policy | Supports reuse while preserving provenance and instruction authority |
| Recoverable artifacts | Git remote for source checkpoints; service accepts bounded logs/reports, not full worktrees or build directories | Determines cross-workstation access, storage, and backup requirements |
| Initial operating size | Establish expected projects, concurrent sessions, and stored history | Sets measurable load and pagination targets |
| Operations | Establish supported server OS/architecture, backup frequency/retention, and acceptable downtime | Sets packaging and recovery acceptance targets |

Integration authority, knowledge autonomy, and artifact storage questions have
been presented. Ask the remaining choices in
small groups, combining closely related operational facts in one free-text
question when appropriate. Installation as a native Linux systemd service,
HTTPS, all-project access, authentication method, agent task creation/claiming,
worktree isolation, recovery mode, and integrated completion are already settled.
Linux and native Windows clients, HTTP API/web/CLI interfaces, and configurable
agent/human/both review are also confirmed. MCP and TUI are deferred.

## Proposed engineering defaults

These are recommendations for the implementation specification, not additional
operator confirmations. They may be adjusted when the product answers require it.

| Area | Default |
| --- | --- |
| API | Versioned JSON HTTP API; stable error codes, explicit next actions, cursor pagination, JSON-capable CLI |
| Task edits | Revision-checked updates; atomic claims and transitions; no generic ownership/status overwrite |
| Selection | Explicit project, ready prerequisites, compatible capabilities, priority, then oldest-ready time and stable ID |
| Task structure | Simple tasks by default; optional parent objectives and acyclic dependencies; separate review/integration activities referencing a submitted candidate |
| Deduplication | Stable source IDs, idempotency, search-before-create, durable duplicate/supersession links; no required model service |
| Agent context | Required current policy plus bounded relevant tasks/lessons; paginated history; no silent truncation of mandatory instructions |
| Knowledge retrieval | Full-text search, project/component/version tags, provenance, correction and supersession; optional cross-project search |
| Local rules | Repository instructions remain applicable; imported memory is evidence/context until explicitly adopted as policy |
| Markdown migration | Preview then apply; configurable filenames and source directories; stable import mappings; no automatic ready-task creation from unstructured historical prose |
| Dashboard updates | Periodic refresh with last-updated time; persistent attention queue for blockers/decisions; external email/chat/push integrations deferred unless requested |
| Local adapters | Vendor-neutral connect/resume/checkpoint/job-report operations with documented hook examples; no automatic alteration of global harness hooks |
| Permissions | Separate administrator, human operator, and agent operations; no project ACLs; decisions claiming human authorization require a human principal |
| Evidence | Immutable submissions, explicit check roster, exact input and integrated revision, producer identity, artifact accessibility |
| API retries | Stable mutation keys and transactional receipts; uncertain external effects require reconciliation |
| Persistence | SQLite on local storage, short transactions, migrations, audit events, tested backup/restore |
| Browser stack | Vanilla JavaScript modules and CSS, served by Axum; use Alpine only if implementation demonstrates a simplification |

Heartbeat timing and helper behavior need a concrete technical contract before
implementation. Propose a one-minute reporting interval and ten-minute renewable
ownership window, configurable per project within server limits. The local
helper must be attached to a known harness lifecycle or a bounded declared job
wait; an orphaned helper cannot keep renewing forever. A stale-progress warning
is separate from lease expiry. Validate these defaults against the long-running
SithBit gates before treating them as release defaults.

For Markdown input, treat BACKOFF.md as a configurable additional filename.
Support BACKLOG.md directly. This avoids making a possible filename typo block
the design while still supporting a distinct file if one exists.

## Required specification before coding

1. Resolve the product choices above and explicitly list deferred features.
2. Define task/activity states, ownership transitions, reviewer independence,
   recovery permissions, and exact completion guards.
3. Define request/response schemas and complete HTTP/CLI examples, including
   authentication help, resume, claim conflicts, and expired authority.
4. Define database constraints and transaction boundaries, including revision
   races, interrupted requests, and resource recovery holds.
5. Define local worktree/job handling on selected platforms and the practical
   limits of coordinating processes that the service does not control.
6. Define the first-run operator flow, backup/restore procedure, and a measurable
   two-workstation acceptance exercise.

Implementation readiness means these contracts agree and have testable outcomes.
It does not require selecting every crate version, cosmetic UI detail, or table
index in advance. Those are implementation choices validated during the relevant
milestone.
