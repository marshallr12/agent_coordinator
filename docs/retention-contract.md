# Storage retention and maintenance contract

Status: implemented host-local maintenance engine. Server command wiring and the
mutation replay check are integrated with the clock-safety work separately.

## Host command and bounds

The explicit host command is:

```text
agent-coordinator-server ... maintenance \
  --batch-size 500 --max-batches 20
```

It calls:

```rust
maintenance::run_maintenance(
    state: &AppState,
    options: MaintenanceOptions,
) -> anyhow::Result<serde_json::Value>
```

`batch_size` defaults to 500 and is limited to 1–1,000 receipt rows and 1–1,000
old observation rows per transaction. `max_batches` defaults to 20 and is
limited to 1–100. The command reports its run ID and timestamps, cutoff,
affected rows, observation rows inspected, batch count, limits, and whether
receipt results or old observation rows remain to inspect. An operator can run
it again when either remaining flag is true. It never runs `VACUUM`; ordinary
backup and database-size operations remain separate and maintenance does not
hold one writer transaction across the whole invocation.

After each committed database batch, the command invokes one existing bounded
artifact reconciliation pass and reports only the number of passes attempted.
Each pass considers at most 100 expired/deleted artifact rows and 100 staging or
orphan entries. Artifact deletion takes the store's nonblocking exclusive GC
lock and rechecks current database state, so an online backup safely defers the
physical deletion. A pass count does not claim that every eligible file was
deleted; service startup and upload reconciliation provide additional bounded
opportunities. Read-only backup and verification commands never schedule this
cleanup.

Every batch obtains `BEGIN IMMEDIATE` before sampling the clock and changing
rows. A detected rollback is committed as a clock incident and stops the
command. An existing clock incident also stops maintenance without compaction.
The cutoff remains fixed at the safe time captured for the run, so advancing wall
time during a multi-batch command cannot change which receipts are eligible.
Each invocation and its aggregate effects are recorded in `maintenance_runs`.
The run record is updated in the same short transactions as its compaction
effects; a process interruption can therefore leave an inspectable `running`
record without misreporting completed batches.

## Permanent receipt tombstones

A mutation receipt's result is replayable through 30 days. Maintenance selects
only receipts for which `safe_now - created_at` is strictly greater than 30 days.
It replaces `result_json` with JSON `null` and records `compacted_at`. It never
deletes or changes:

- `principal_id`
- operation and idempotency key
- request fingerprint
- authority epoch
- original creation time

Those columns form a permanent tombstone. Mutation lookup first checks the
authority epoch and fingerprint, then returns
`idempotency_receipt_expired` for a matching compacted receipt. A changed request
still returns `idempotency_conflict`, and a pre-restore key still returns
`request_from_previous_restore`. No tombstoned key can execute again. Result
compaction therefore saves potentially large response bodies without weakening
exactly-once mutation identity.

## Redundant producer observations

Producer observation rows and sequence identities remain permanent. The engine
may clear only the `summary` of an old, strictly redundant intermediate
`running` observation. Both its immediately preceding and following observations
must have exactly the same state, PID, process-start identity, exit/input fields,
and summary. The row must be older than the same 30-day cutoff and cannot be the
first or last observation for its reporter.

`payload_compacted_at` explicitly identifies this elision. The original
`request_hash`, reporter, sequence, producer identity, state, process identity,
result fields, and observation time remain unchanged. The current `jobs.summary`
projection is never cleared. First and last evidence, terminal states, `unknown`,
state transitions, changed summaries, and nonempty progress transitions are
always retained. Request replay identity therefore remains trustworthy even
when a redundant middle display payload is gone.

Each batch first selects at most `batch_size` old, previously uninspected rows
through the retention-scan index. Only those rows run the neighbor checks, and
all selected rows receive `retention_checked_at` whether or not their summaries
are eligible. The indexed remaining check looks only for old uninspected rows.
Thus a history containing arbitrarily many distinct progress messages cannot
turn one batch into an unbounded scan under the SQLite writer lock.

Summary elision is conservative and best effort. Clearing one eligible summary
can prevent an adjacent row considered in a later batch from proving that its
original neighboring summary matched. The engine leaves such a row intact; it
does not retain hidden copies of display text merely to maximize compaction.
The compacted count therefore reports actual cleared summaries and does not
promise that every duplicate middle summary will eventually be removed.
While rows remain uninspected, `remaining.observation_payloads` is a
conservative compatibility flag; `remaining.observation_rows_to_inspect`
states its precise meaning.

## Records retained in full

Maintenance does not delete or rewrite semantic events, projects, task
definitions or revisions, dependencies, attempts, checkpoints, handoffs,
checkouts, resource holds, jobs, submissions, artifact references, knowledge or
lesson revisions, decisions, reviews, integration evidence, objectives, restore
evidence, or closed import identities. It does not delete artifact files;
artifact expiry and deletion continue through the artifact store's database
recheck and backup-aware GC lock.

The first release intentionally favors trustworthy provenance over reclaiming
every byte. Event payloads are already compact, and receipt tombstones still use
one bounded row per mutation. This policy bounds work per invocation but does
not promise a storage ceiling for permanent semantic history. Deployment sizing
and the 100,000-task load test must include those identity and audit rows.
