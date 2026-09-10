# Backup and restore storage contract

Status: implemented storage engine. Authority invalidation, maintenance-mode
reconciliation, server command wiring, timers, and the measured recovery exercise
are integrated separately.

## Commands and callable interface

The host-local server commands are:

```text
agent-coordinator-server ... backup --repository BACKUP_REPOSITORY
agent-coordinator-server backup-verify --snapshot SNAPSHOT_DIRECTORY
agent-coordinator-server restore --snapshot SNAPSHOT_DIRECTORY \
  --destination ABSENT_DATA_DIRECTORY --reason REASON
```

The engine exposes these asynchronous Rust functions:

```rust
create_backup(state: &AppState, repository: &Path) -> anyhow::Result<Value>
verify_backup(snapshot: &Path) -> anyhow::Result<Value>
restore_backup(snapshot: &Path, destination: &Path, reason: &str)
    -> anyhow::Result<Value>
```

`backup` requires an existing configured database opened read-only by command
wiring. It does not create, migrate, or write the live database. `backup-verify`
does not open the configured live database. `restore` runs before normal live
database opening and accepts only a destination directory that does not exist.

Successful backup output includes `snapshot_id`, the absolute completed
`snapshot_path`, `created_at`, `database_bytes`, `artifact_count`,
`artifact_bytes`, `snapshot_bytes`, and retention counts and bytes. Successful
verification returns the same identity and byte counts with `verified: true`.
Successful restore returns `snapshot_id`, the absolute `data_directory` and
`database_path`, artifact counts and bytes, and the authority invalidation
result. Paths are host-local operational output and are not written into the
snapshot manifest.

## Consistent snapshot and artifact hold

SQLite `VACUUM main INTO ?` produces the database image at a consistent point
while the service remains online and includes committed data represented by the
WAL. Copying a live main database file by itself is unsupported. The destination
database file must be absent.

Before starting that SQLite snapshot, backup obtains a shared advisory lock on:

```text
<database-filename>.artifacts/.gc.lock
```

It retains the lock through database verification and copying every finalized
artifact named by the copied database. Uploads and ordinary database writes can
continue. Every physical artifact deletion and orphan cleanup attempts an
exclusive, nonblocking lock after any database writer transaction has ended. If
backup holds the shared lock, cleanup leaves the bytes in place for a later
bounded reconciliation pass. A process exit releases the operating-system lock.

The manifest selects only upload artifacts whose copied row is `finalized` and
whose retention is live at the snapshot timestamp, or which are pinned. A file
must be regular, no larger than 16 MiB, and exactly match the recorded size and
lowercase SHA-256 digest. Missing or changed bytes fail the snapshot before
publication. External-link artifacts remain in SQLite and require no local blob.

## Repository and publication

The repository layout is:

```text
BACKUP_REPOSITORY/
  .lock
  .staging/
    <timestamp>-<snapshot-uuid>.partial/
  snapshots/
    <timestamp>-<snapshot-uuid>/
      database.sqlite3
      blobs/
        <artifact-storage-uuid>.blob
      manifest.json
      COMPLETE
```

Each snapshot directory is self-contained and immutable. The first release does
not deduplicate bytes across snapshots, so capacity planning must use the
reported `snapshot_bytes` and `retained_bytes`. At maximum retention the union
usually contains up to 53 complete copies because the newest daily snapshot also
belongs to an hourly bucket.

Creation and retention take an exclusive operating-system lock on
`BACKUP_REPOSITORY/.lock`. Verification and restore take a shared lock when the
snapshot is in that repository's `snapshots` or `.staging` directory and the lock
already exists. A standalone copied snapshot is verified without creating files,
directories, or changing permissions in its parent; the caller must keep such a
standalone directory immutable during verification.

The engine creates directories with mode `0700` and files with mode `0600` on
Unix, rejects symbolic links, creates files without overwrite, streams through a
64 KiB buffer, and syncs files and directories. It writes `COMPLETE` only after
the database, blobs, and manifest are durable. `COMPLETE` binds the snapshot ID
and exact manifest digest. The fully assembled and verified partial directory is
renamed into `snapshots` and the parent is synced. Consumers count only a
directory with a valid `COMPLETE`; partial directories are never usable backups.

Creation verifies SQLite integrity, foreign keys, the exact successful migration
version/description/checksum set, and exact agreement between the manifest and
all eligible artifact rows before publication. Verification repeats those checks
as well as every recorded size and digest. A failed assembly removes only its
generated partial directory and neither publishes nor prunes.
If a separately completed snapshot publishes but retention later fails, output
contains `warning.code: retention_failed`; the new and prior usable snapshots
remain available for inspection.

## Fixed safety bounds

The first-release storage bounds are explicit constants:

- 8 GiB maximum SQLite image
- 1,000,000 finalized artifact entries
- 64 MiB maximum manifest before deserialization
- 16 MiB per artifact blob
- 256 MiB free-space reserve at the backup or restore destination
- 64 KiB streaming copy/hash buffers
- 45-minute operation deadline enforced by SQLite progress callbacks during
  snapshot and integrity work, and checked during streamed copies, hashing, and
  restore publication

The database size is checked from SQLite page count before `VACUUM INTO` and from
the result afterward. Free space is checked before the database image and each
artifact copy. The system service also bounds the whole host-local command,
including authority invalidation and operating-system I/O that an SQLite callback
cannot interrupt. The 45-minute engine limit supports the one-hour recovery
target but does not establish that target until the documented exercise measures
verification, copying, invalidation, administrator recovery, service start, and
one recovered client on the release host.

## Retention

After successful publication, retention sorts completed snapshots by their
recorded UTC timestamp. It keeps the newest successful snapshot in each of the
newest 24 represented UTC hour buckets and the newest successful snapshot in each
of the newest 30 represented UTC day buckets. The retained set is the union. A
corrupt manifest or completion marker aborts retention before deletion. Removal
is restricted to complete snapshot directories outside that union and occurs
under the exclusive repository lock.

## Fresh-path restore

Restore first verifies the source completion marker, manifest, database, and all
blobs without touching the configured live database. It creates a private sibling
staging directory next to the requested absent destination and builds:

```text
ABSENT_DATA_DIRECTORY/
  coordinator.sqlite3
  coordinator.sqlite3.artifacts/
    blobs/<first-two-storage-key-characters>/<storage-key>.blob
    staging/
```

The copied database must match the immutable snapshot manifest before authority
invalidation. The engine then opens only the staged database, calls
`restore::invalidate_restored_state`, checkpoints and closes SQLite, rechecks
database integrity, foreign keys, and the current migration set, and re-hashes
every restored artifact. The database checksum is expected to differ after
authority invalidation. Finally it syncs the tree, atomically renames the whole
staging directory to the absent destination, and syncs the parent. It never
overwrites or renames the old installation.

The old installation must remain stopped and fenced. The authority layer rotates
epochs, revokes restored credentials and sessions, expires attempt authority,
marks in-flight jobs uncertain, retains physical and integration holds, and
requires explicit reconciliation before coordination resumes. Those mutations
and their audit contract are defined by the restore authority implementation.

## Off-server copying

An off-server copy must contain the entire self-contained snapshot directory.
Authentication and encryption belong to the operator-selected transfer system;
credentials must not enter command arguments, manifests, logs, or the repository.
For a repository copy, hold a shared source `.lock`; hold an exclusive destination
`.lock`, copy database/blobs/manifest into a fresh `.staging` directory, and copy
`COMPLETE` last. Release the destination lock, run `backup-verify` on that partial
directory, then reacquire exclusive lock and atomically rename it into
`snapshots` without overwrite. Record local creation separately from verified
off-server copy time. Hourly local snapshots alone do not provide one-hour host
loss protection.
