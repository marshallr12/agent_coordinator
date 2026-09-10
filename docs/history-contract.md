# Task history contract

Task history is a read-only, project-scoped view of durable service evidence. It
does not change task state and does not grant workflow authority.

## Route

`GET /api/v1/projects/{project_id}/tasks/{task_id}/history`

Query parameters:

- `kind` is required and is one of `attempts`, `checkpoints`, `checkouts`,
  `jobs`, `job_observations`, `resources`, `artifacts`, `submissions`,
  `reviews`, `integrations`, `task_revisions`, or `events`.
- `limit` is optional, defaults to 50, and must be from 1 through 200.
- `cursor` is the opaque `next_cursor` returned by the preceding page. Clients
  must not parse or alter it.

The response `data` has this shape:

```json
{
  "project_id": "project UUID",
  "task_id": "requested task UUID",
  "subject_task_id": "subject task UUID",
  "kind": "attempts",
  "snapshot": "opaque insertion snapshot identity",
  "items": [
    {
      "kind": "attempts",
      "relation": "subject",
      "task_id": "task UUID for this record",
      "occurred_at": "RFC 3339 timestamp or null",
      "record": {}
    }
  ],
  "next_cursor": "opaque cursor or null"
}
```

A workflow activity task resolves to its subject task. Pages requested for the
subject or any of its workflow activity tasks traverse the same task graph.
`relation` distinguishes subject evidence, workflow activity evidence, and other
subject evidence such as artifacts attached through a submission. Every SQL
relationship is also constrained to the requested project.

`reviews` include the activity, immutable decision, and findings. `integrations`
include the activity, authorization, target hold, publication intent, integration
result, reconciliation, and the complete job records named by the result's exact
check-job identifiers. `submissions` include the recorded acceptance evidence,
handoff, artifact identifiers, and immutable knowledge revisions. No record text
is silently shortened.

## Bounds and cursor behavior

Each page is limited by both its requested item count and a 256 KiB serialized
data budget. If one complete record cannot fit, the service returns conflict code
`history_record_too_large` instead of returning partial evidence.

The first request captures the greatest insertion row identifier for that kind.
Later pages keep this cutoff, so records inserted concurrently are excluded and
there are no gaps or duplicates among rows that remain present. `snapshot` names
this insertion cutoff. It is not an immutable state snapshot: durable rows that
the service permits to change may show their newer values on later pages.

The cursor is versioned and bound to the project, requested task, history kind,
cutoff, last row, and service cursor epoch. Reusing it with another project, task, kind, or snapshot
returns conflict code `history_cursor_mismatch`. Malformed cursors return a bad
request.

Version 2 cursors include the service cursor epoch, which rotates before restored
data is published. A cursor from before restore is rejected; restart the history
query. Version 1 cursors from earlier service releases must also be replaced.
Backup uses `VACUUM INTO` on a separate image and leaves live row identifiers
unchanged. Maintenance must not run an in-place `VACUUM` while cursors remain
valid; any procedure that rewrites row identifiers must rotate the cursor epoch.

## Sensitive fields

History preserves operational evidence but excludes authentication material.
Attempt session and credential identifiers, reporter identities and proof hashes,
job observation request hashes, submission contributor sessions, review sessions,
and artifact storage keys are omitted. Event data is parsed as structured JSON
and recursively removes fields whose names identify authorization, cookies,
credentials, passwords, proofs, request hashes, secrets, sessions, or tokens.
Service-created events currently store no authentication material in event data.

## Integration wiring

The core crate must declare and re-export `history`:

```rust
pub mod history;
pub use history::*;
```

The server crate must declare `pub mod history;` and merge `history::routes()`
into the application router. Migration `0011_history_indexes.sql` must run after
the existing workflow, knowledge, import, artifact, and operator migrations.

## Retained observation payloads

Maintenance may clear the summary of an old, exactly redundant intermediate job
observation. History exposes `payload_compacted_at` as an RFC 3339 timestamp, or
`null` when the payload is intact. The row, sequence, process identity, state, and
request identity remain stored. First/last observations, state transitions, and
nonduplicate progress summaries remain intact. See [retention](retention-contract.md).

Event pages build the requested task’s related record identities, then use the
project/record index. They include task-, job-, and submission-associated artifact
mutations. Returned pages remain bounded; query work for a task with exceptionally
deep evidence grows with that task’s own history, rather than unrelated project
events. The release volume benchmark does not model 100,000 records on one task.
