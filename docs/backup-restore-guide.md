# Backup and restore operations

This guide covers the native Linux backup timer, verified off-server copies, and
an offline restore. A backup is useful only after verification. A restore remains
paused until a recovered administrator inspects preserved holds, fences the old
installation, and reconciles work that may have happened after the snapshot.

The examples use these installation paths:

- live data: `/var/lib/agent-coordinator`
- database: `/var/lib/agent-coordinator/coordinator.sqlite3`
- backup repository: `/var/lib/agent-coordinator-backups`
- service environment: `/etc/agent-coordinator/service.env`

Run the shell command blocks with `set -eu` so an error stops the procedure.
Replace the uppercase incident and snapshot placeholders before running them.

Keep the backup repository outside the live data and artifact roots. The backup
command rejects a nested repository because recursive capture cannot produce a
self-contained recovery set.

## Install and monitor the hourly timer

Install `deploy/agent-coordinator-backup.service` and
`deploy/agent-coordinator-backup.timer` under `/etc/systemd/system`. Create the
backup repository for the service account before starting the timer:

```sh
sudo install -d -o agent-coordinator -g agent-coordinator -m 0700 \
  /var/lib/agent-coordinator-backups
sudo install -o root -g root -m 0644 \
  deploy/agent-coordinator-backup.service \
  deploy/agent-coordinator-backup.timer \
  /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl start agent-coordinator-backup.service
```

The calendar timer targets the start of every UTC hour, adds at most 60 seconds of
random delay, and catches up once after downtime. The oneshot service and the
repository's exclusive advisory lock prevent overlapping backup or retention
work. The online SQLite backup does not require stopping the main service. These
settings follow the official
[systemd timer semantics](https://www.freedesktop.org/software/systemd/man/latest/systemd.timer.html)
for calendar accuracy, randomized delay, and persistent catch-up.

Keep `service.env` limited to the non-secret server settings shown in the deploy
example. The backup unit has no network access and performs no off-server transfer;
configure transfer credentials only in the separate operator-selected mechanism.

Verify the first snapshot as described below. Only after it passes verification,
enable the persistent schedule:

```sh
sudo systemctl enable --now agent-coordinator-backup.timer
```

Inspect the actual result rather than treating timer activation as proof of a
backup:

```sh
systemctl list-timers agent-coordinator-backup.timer
sudo systemctl status agent-coordinator-backup.service
sudo journalctl -u agent-coordinator-backup.service --since today
```

Configure monitoring for a failed oneshot unit and for the age of the latest
verified snapshot. A later successful run does not erase the need to investigate
an earlier disk-space or integrity failure.

The command writes into `.staging` and publishes a snapshot only after verifying
its SQLite image, every selected artifact, its manifest, and its completion
marker. A completed snapshot is:

```text
/var/lib/agent-coordinator-backups/snapshots/<UTC-time>-<snapshot-id>/
  database.sqlite3
  blobs/<artifact-storage-id>.blob
  manifest.json
  COMPLETE
```

Directories use mode 0700 and files use mode 0600. Failed partial snapshots are
not usable backups. Backup creation preserves existing completed snapshots if a
new snapshot cannot be completed. The engine holds a shared artifact-cleanup lock
from before the online SQLite snapshot through blob copy and verification. Cleanup
uses a nonblocking exclusive lock and defers while a backup is selecting bytes;
the backup does not keep a database writer transaction open while copying blobs.
Do not replace this with a filesystem copy of the live database that omits its WAL.

Retention keeps the union of:

- the latest completed snapshot in each of the newest 24 represented UTC-hour
  buckets; and
- the latest completed snapshot in each of the newest 30 represented UTC-day
  buckets.

Multiple manual backups in one hour do not consume multiple hourly buckets. An
hour or day without a successful backup does not create a fictitious retained
copy. Snapshot directories are self-contained, so pruning one does not remove
bytes needed by another retained snapshot.

## Verify and copy a snapshot off-server

Run verification with the same reviewed server version used for the installation:

```sh
sudo -u agent-coordinator /usr/local/bin/agent-coordinator-server \
  backup-verify \
  --snapshot /var/lib/agent-coordinator-backups/snapshots/SNAPSHOT_DIRECTORY
```

Verification rejects a missing `COMPLETE`, manifest mismatches, changed sizes or
digests, SQLite integrity or foreign-key failures, and incompatible schema data.
It does not open or change the configured live database. Restore likewise reads
only its selected snapshot and writes only its absent destination.

Copy one whole self-contained snapshot while holding the repository's shared
advisory lock at `/var/lib/agent-coordinator-backups/.lock`. The following example
uses an already mounted, operator-selected destination. Its `.staging` and
`snapshots` directories must be on the same filesystem so the final rename is
atomic:

```sh
source_snapshot=/var/lib/agent-coordinator-backups/snapshots/SNAPSHOT_DIRECTORY
destination_repository=/mnt/operator-selected-backup/agent-coordinator
snapshot_name=$(basename -- "$source_snapshot")
destination_stage="$destination_repository/.staging/$snapshot_name.partial"

sudo install -d -o agent-coordinator -g agent-coordinator -m 0700 \
  "$destination_repository"
sudo -u agent-coordinator install -d -m 0700 \
  "$destination_repository/.staging" "$destination_repository/snapshots"
sudo test ! -e "$destination_stage"
sudo test ! -e "$destination_repository/snapshots/$snapshot_name"
sudo -u agent-coordinator install -d -m 0700 "$destination_stage"
sudo -u agent-coordinator flock --shared \
  /var/lib/agent-coordinator-backups/.lock \
  sh -c 'cp -a -- "$1/database.sqlite3" "$1/blobs" "$1/manifest.json" "$2/" && cp -a -- "$1/COMPLETE" "$2/"' \
  backup-copy "$source_snapshot" "$destination_stage"

sudo -u agent-coordinator /usr/local/bin/agent-coordinator-server \
  backup-verify --snapshot "$destination_stage"
sudo sync -f "$destination_repository"
sudo -u agent-coordinator flock --exclusive "$destination_repository/.lock" \
  mv --no-clobber --no-target-directory -- \
  "$destination_stage" "$destination_repository/snapshots/$snapshot_name"
sudo test ! -e "$destination_stage"
sudo sync -f "$destination_repository"
```

The destination is not counted as protected until destination-side verification
succeeds and the staging directory is published. For an SSH, object-storage, or
other transfer, preserve the same order: hold the source lock for the complete
read, transfer every file, place `COMPLETE` last in a private staging location,
verify at the destination, and then publish atomically. Supply authentication,
transport encryption, and encryption at rest through that external mechanism. Do
not put its credentials in the repository, unit file, command line, or service
environment.
Record the local snapshot time and verified off-server copy time separately.
Hourly local backups do not provide hourly host-loss protection unless verified
off-server transfer also runs at least hourly.

## Back up before a schema upgrade

Keep the exact currently installed binary until the upgrade is accepted. Before a
schema-changing upgrade, enter maintenance, stop the service, and use that old
binary to create and verify a snapshot of the old schema:

```sh
old_binary=/operator/protected/path/agent-coordinator-server-OLD_VERSION
sudo -u agent-coordinator "$old_binary" \
  --database /var/lib/agent-coordinator/coordinator.sqlite3 \
  backup --repository /var/lib/agent-coordinator-backups
sudo -u agent-coordinator "$old_binary" \
  backup-verify \
  --snapshot /var/lib/agent-coordinator-backups/snapshots/SNAPSHOT_DIRECTORY
```

The backup command opens an existing database at its exact expected migration
level and never upgrades it. A new binary can reject the old schema, so do not
install it first and then rely on it to create the pre-migration recovery point.
Apply the new release's migration only after the old-format snapshot passes
verification. Preserve the matching old executable with the snapshot until the
upgrade recovery window closes.

## Restore into a fresh data directory

Use an available compatible host and a reviewed binary compatible with the
snapshot manifest. Never restore over the live directory. Keep the old
installation and its public endpoint fenced throughout the operation so two
installations cannot act on the same external work.

1. Stop the backup timer and main service, block ordinary traffic at the reverse
   proxy, and prevent any old service instance from restarting. Keep a protected
   HTTPS path available only to the recovery administrator for the later sign-in:

   ```sh
   sudo systemctl stop agent-coordinator-backup.timer
   sudo systemctl stop agent-coordinator.service
   ```

2. Preserve the current data directory separately. Select a completed snapshot
   and verify it before restore:

   ```sh
   sudo -u agent-coordinator /usr/local/bin/agent-coordinator-server \
     backup-verify --snapshot /var/lib/agent-coordinator-backups/snapshots/SNAPSHOT_DIRECTORY
   ```

3. Choose an absent staging destination on the same filesystem as the eventual
   live directory. Record a specific incident or exercise reason, then restore:

   ```sh
   sudo install -d -o agent-coordinator -g agent-coordinator -m 0700 \
     /var/lib/agent-coordinator-restore
   sudo test ! -e /var/lib/agent-coordinator-restore/RESTORE_INCIDENT
   sudo -u agent-coordinator /usr/local/bin/agent-coordinator-server \
     restore \
     --snapshot /var/lib/agent-coordinator-backups/snapshots/SNAPSHOT_DIRECTORY \
     --destination /var/lib/agent-coordinator-restore/RESTORE_INCIDENT \
     --reason '<audited restore reason>'
   ```

   Restore refuses an existing destination. It verifies the complete snapshot
   before mutation, creates `coordinator.sqlite3` and its artifact tree with
   private permissions, runs SQLite integrity and foreign-key checks, invalidates
   restored authority, checkpoints the staged database, and only then publishes
   the complete destination directory.

4. With the service still stopped, move the old live directory aside and promote
   the verified staging directory. Keep the sibling backup repository in place:

   ```sh
   sudo mv --no-clobber --no-target-directory -- \
     /var/lib/agent-coordinator \
     /var/lib/agent-coordinator.pre-restore-RESTORE_INCIDENT
   sudo test ! -e /var/lib/agent-coordinator
   sudo mv --no-clobber --no-target-directory -- \
     /var/lib/agent-coordinator-restore/RESTORE_INCIDENT \
     /var/lib/agent-coordinator
   sudo test ! -e /var/lib/agent-coordinator-restore/RESTORE_INCIDENT
   sudo sync -f /var/lib
   sudo chown -R agent-coordinator:agent-coordinator \
     /var/lib/agent-coordinator
   ```

5. Start the service while ordinary client traffic remains blocked. The restored
   service starts in a global pause. All restored browser sessions, agent
   credentials, agent sessions, reporter proofs, attempts, and leases are invalid.
   Restored human passwords are randomized and all human accounts are disabled.
   A fresh restore epoch also rejects cursors issued before the restore. Resource
   and integration holds remain recorded for inspection.

   ```sh
   sudo systemctl start agent-coordinator.service
   ```

6. Recover exactly one existing administrator from the host. Enter the password
   through the hidden prompt, or use a protected standard-input pipe:

   ```sh
   sudo -u agent-coordinator /usr/local/bin/agent-coordinator-server \
     --database /var/lib/agent-coordinator/coordinator.sqlite3 \
     recover-operator-password \
     --username EXISTING_ADMIN \
     --reason '<audited restore administrator recovery reason>'
   ```

   If password recovery reports `clock_reconciliation_required`, correct host
   time and follow [host clock reconciliation](clock-safety-contract.md#administrator-api)
   first, then retry password recovery. Restoring a database does not erase a
   captured clock incident.

7. Sign in as that administrator and enumerate the bounded restore requirements:

   The examples below show route and body shapes. Use the dashboard or an
   authenticated administrator client that supplies the normal Origin, CSRF, and
   idempotency protections for mutations.

   ```text
   GET /api/v1/admin/restore?limit=200
   GET /api/v1/admin/restore?cursor=<next-cursor>&limit=200
   ```

   For every returned hold, inspect current physical and external state and save
   one disposition with evidence:

   ```json
   POST /api/v1/admin/restore/inspections
   {
     "restore_id": "<restore-id>",
     "kind": "resource_hold",
     "target_id": "<hold-id>",
     "disposition": "unknown",
     "evidence": "what was inspected and where"
   }
   ```

   `kind` is `resource_hold` or `integration_hold`; `disposition` is `held`,
   `released`, or `unknown`.

   An inspection records the finding; finishing restore does not automatically
   release a preserved hold. Use the normal human job, resource, or publication
   reconciliation operation when evidence supports a later state change.

8. Prevent the old installation from serving or writing permanently, then attest
   that fencing with evidence:

   ```json
   POST /api/v1/admin/restore/old-installation-fenced
   {"restore_id":"<restore-id>","evidence":"host, process, proxy, and storage fencing evidence"}
   ```

9. Compare external effects newer than the snapshot. At minimum, inspect remote
   Git targets for preserved integration work, workstation job/process state,
   physical resources, and off-service artifact state. Reconcile each uncertain
   effect through its normal guarded workflow, then attest the completed review:

   ```json
   POST /api/v1/admin/restore/post-snapshot-gap
   {"restore_id":"<restore-id>","evidence":"snapshot cutoff and reconciliation evidence"}
   ```

10. After both attestations and every captured hold inspection are recorded,
    finish restore:

    ```json
    POST /api/v1/admin/restore/finish
    {"restore_id":"<restore-id>","reason":"operator reviewed restored authority and external effects"}
    ```

    This atomically lifts the global pause. Inspection records do not change
    hold state: held resources remain held until a separate guarded operation
    releases or resolves them. During the pause, ordinary mutations remain
    rejected; administrator recovery actions and guarded human reconciliation
    remain available. Issue fresh credentials for existing agent principals only
    after deciding that they should reconnect. Credential creation returns a token
    once; store it outside the repository. Confirm that old cookies, passwords,
    tokens, session proofs, reporter proofs, and attempt generations are rejected.

    A fresh credential for an existing agent principal uses:

    ```text
    POST /api/v1/admin/agents/<principal-id>/credentials
    {"name":"post-restore workstation credential"}
    ```

    Recover any additional human account from the host before enabling it; never
    treat its pre-restore password as valid authority.

11. Restore ordinary proxy access and restart the backup timer only after service
    readiness, new administrator authentication, old-authority rejection, and the
    restore checklist have all been observed.

    ```sh
    sudo systemctl start agent-coordinator-backup.timer
    ```

## Exercise and recovery-time evidence

The recovery-time target is one hour from starting this procedure on an available
compatible host with access to a completed snapshot and host credentials, through
verified service availability and one recovered client. A rehearsal must record:

- start and finish times, binary/schema versions, snapshot ID and snapshot time;
- snapshot/database/artifact byte counts and destination verification result;
- time to restore, recover the administrator, inspect holds, and reconcile the
  post-snapshot gap;
- rejection of representative old browser, agent, reporter, and attempt authority;
- successful authentication by one recovered client and the restore-finish event;
- storage throughput, failures, manual steps, and external dependencies.

Do not claim the one-hour target from design estimates or local backup cadence.
New host procurement, unavailable off-server storage, and recovery of external
credentials are outside the measured compatible-host procedure and must be named
separately in a real incident report.
