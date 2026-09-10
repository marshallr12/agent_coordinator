# Linux release installation

The binary Linux release is built and tested for Ubuntu 24.04 LTS on x86-64.
Other Linux distributions can build the locked Rust workspace from source, but
they are outside this binary package's acceptance claim. The package contains
the service, native CLI, systemd units, configuration and Caddy examples, and a
complete copy of the repository's Markdown guidance. Migrations and dashboard
assets are embedded in the server binary.

The release workflow also builds a native Windows x86-64 CLI archive and runs
its client, CLI, and local-runner tests on a Windows GitHub runner. This CI result
does not establish the separate Windows workstation acceptance milestone.

## Verify and inspect the archive

Download the archive and its adjacent `.sha256` file from the same reviewed
release run. Verify before extracting:

```sh
sha256sum --check agent-coordinator-0.1.0-linux-x86_64.tar.gz.sha256
tar -tzf agent-coordinator-0.1.0-linux-x86_64.tar.gz
```

Replace `0.1.0` with the release version. The archive has one top-level directory
and contains no links. Its internal `SHA256SUMS` covers every packaged file except
the manifest itself. Extract it into a staging directory and verify that manifest
from the package root:

```sh
mkdir package-staging
tar -xzf agent-coordinator-0.1.0-linux-x86_64.tar.gz -C package-staging
cd package-staging/agent-coordinator-0.1.0-linux-x86_64
sha256sum --check SHA256SUMS
```

Do not pipe an unverified archive into a privileged extraction command. Keep the
package and its checksums with the release evidence used for the installation.

## Install the service

The examples use `/usr/local/bin`, `/etc/agent-coordinator`, and
`/var/lib/agent-coordinator`. Adapt all unit and configuration paths together if
the installation uses different locations.

```sh
sudo useradd --system --user-group --home-dir /var/lib/agent-coordinator \
  --shell /usr/sbin/nologin agent-coordinator
sudo install -d -o root -g agent-coordinator -m 0750 /etc/agent-coordinator
sudo install -d -o agent-coordinator -g agent-coordinator -m 0700 \
  /var/lib/agent-coordinator /var/lib/agent-coordinator-backups
sudo install -o root -g root -m 0755 bin/agent-coordinator-server \
  bin/agent-coordinator /usr/local/bin/
sudo install -o root -g agent-coordinator -m 0640 deploy/service.env.example \
  /etc/agent-coordinator/service.env
```

Edit `/etc/agent-coordinator/service.env`. Set the exact public HTTPS origin and
retain the loopback listener. The example contains no credentials. Keep the
SQLite database, WAL files, artifact directory, and backups private because they
contain authentication verifiers and work evidence.

Initialize the first administrator at a hidden terminal prompt:

```sh
sudo -u agent-coordinator /usr/local/bin/agent-coordinator-server \
  --database /var/lib/agent-coordinator/coordinator.sqlite3 \
  --public-origin https://coordinator.example.com \
  init-admin --username admin
```

The command supports `--password-stdin` for a protected automation pipe. Never
put the password in arguments, an environment file, shell history, or logs.

Install and start the service, hourly backup timer, and daily maintenance timer:

```sh
sudo install -o root -g root -m 0644 deploy/agent-coordinator.service \
  deploy/agent-coordinator-backup.service deploy/agent-coordinator-backup.timer \
  deploy/agent-coordinator-maintenance.service deploy/agent-coordinator-maintenance.timer \
  /etc/systemd/system/
sudo systemd-analyze verify /etc/systemd/system/agent-coordinator.service \
  /etc/systemd/system/agent-coordinator-backup.service \
  /etc/systemd/system/agent-coordinator-backup.timer \
  /etc/systemd/system/agent-coordinator-maintenance.service \
  /etc/systemd/system/agent-coordinator-maintenance.timer
sudo systemctl daemon-reload
sudo systemctl enable --now agent-coordinator.service
sudo systemctl start agent-coordinator-backup.service
sudo systemctl --no-pager --full status agent-coordinator.service
sudo systemctl --no-pager --full status agent-coordinator-backup.service
```

The timer starting successfully is not proof of a usable backup. Inspect the
oneshot result and verify its reported snapshot as described in
[the backup and restore guide](backup-restore-guide.md). Inspect maintenance
remaining-work flags and monitor failed units as described in
[the retention contract](retention-contract.md#daily-maintenance-timer). After
verifying the first backup, run maintenance and then enable both schedules:

```sh
sudo systemctl start agent-coordinator-maintenance.service
sudo systemctl --no-pager --full status agent-coordinator-maintenance.service
sudo systemctl enable --now agent-coordinator-backup.timer agent-coordinator-maintenance.timer
```

Persistent timers can immediately run missed schedules when first enabled. The
manual checks above establish first-run evidence before enabling those schedules.

## Configure HTTPS

Install Caddy through its documented distribution channel, copy
`deploy/Caddyfile.example` to `/etc/caddy/Caddyfile`, replace the example hostname,
and validate before reload:

```sh
sudo caddy validate --config /etc/caddy/Caddyfile
sudo systemctl reload caddy
curl --fail --show-error https://coordinator.example.com/healthz
```

The hostname must match `COORDINATOR_PUBLIC_ORIGIN`. Keep the service listener on
loopback and expose only the HTTPS proxy. Do not disable certificate verification.
For an internal Caddy CA, explicitly install its root certificate on each client;
Caddy documents both [`tls internal`](https://caddyserver.com/docs/caddyfile/directives/tls)
and its [persistent data location](https://caddyserver.com/docs/conventions).
The automated Ubuntu acceptance uses a disposable internal CA and supplies that
CA to both its HTTPS client and native CLI.

Sign in through HTTPS, create a project, issue a separately named agent
credential, and run `agent-coordinator connect` from a bound repository. Follow
[the CLI guide](CLI.md) for the binding and protected local credential state.

## Upgrade and rollback boundary

Before upgrading, stop both timers and wait for or stop any running backup and
maintenance units. Use the currently installed matching binary to create and
verify a backup. Stop the main service, stage and checksum the new binaries, and
replace both binaries together. Start the service and check HTTPS health, sign-in,
and native client reconnect. Run and verify a new backup and inspect a maintenance
result before restarting both timers. Keep all scheduled and active host commands
stopped while replacing binaries; stopping only the main service is insufficient.
The new server may migrate the database on startup;
the new binary deliberately rejects an older schema for backup rather than
silently upgrading it during a backup command. Version-1 snapshots from schema 12 onward
can be verified and restored by this release when their migration history is an
exact known prefix. Restore upgrades its private copy before invalidating old
authority; it leaves the original snapshot unchanged.

Do not roll an old binary back over a database after a newer migration. Restore
the pre-upgrade snapshot into a fresh directory with a matching compatible binary
and follow the authority reconciliation procedure instead. Preserve the failed
state for diagnosis. The service never automatically reverses migrations.

## Removal

Disable and stop both timers, then stop any already-running backup or maintenance
unit before stopping the main service. Disabling a timer alone does not stop its
active oneshot. Use this order:

```sh
sudo systemctl disable --now agent-coordinator-backup.timer agent-coordinator-maintenance.timer
sudo systemctl stop agent-coordinator-backup.service agent-coordinator-maintenance.service
sudo systemctl disable --now agent-coordinator.service
```

Remove the unit files and binaries, then run `sudo systemctl daemon-reload`.
Keep data and backups until their retention or incident requirements have been
reviewed. Removing the package does not authorize deleting
`/var/lib/agent-coordinator` or `/var/lib/agent-coordinator-backups`.

The release workflow's systemd exercise is disposable and runs only on an Ubuntu
24.04 GitHub-hosted runner. It uses unique account, unit, port, and filesystem
names, exercises start and restart through trusted HTTPS, and removes those
resources afterward. It is release evidence, not evidence of a production
installation, public hostname, off-server backup destination, or completed
Windows workstation exercise.

Release archives are deterministic for fixed input binaries, documentation,
version, and `SOURCE_DATE_EPOCH`. CI performs two clean release builds into
separate target directories, packages each result, and requires byte-identical
archives and checksum files. This checks the complete native release output on
the named runner image; keep the run identity with release evidence.
