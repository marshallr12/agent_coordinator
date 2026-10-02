# Service deployment examples

These examples configure one service process with SQLite on a local filesystem.
Caddy terminates HTTPS; the service accepts only loopback listener addresses and
ignores forwarded headers. For checksum verification, package layout, supported
binary platform, upgrade boundaries, and removal, start with the
[Linux release installation guide](../docs/linux-installation.md).

The installable examples remain canonical files in the repository rather than
book chapters:

- [server service](https://github.com/marshallr12/agent_coordinator/blob/main/deploy/agent-coordinator.service)
- [hourly backup service](https://github.com/marshallr12/agent_coordinator/blob/main/deploy/agent-coordinator-backup.service)
  and [timer](https://github.com/marshallr12/agent_coordinator/blob/main/deploy/agent-coordinator-backup.timer)
- [daily maintenance service](https://github.com/marshallr12/agent_coordinator/blob/main/deploy/agent-coordinator-maintenance.service)
  and [timer](https://github.com/marshallr12/agent_coordinator/blob/main/deploy/agent-coordinator-maintenance.timer)
- [service environment example](https://github.com/marshallr12/agent_coordinator/blob/main/deploy/service.env.example)
- [Caddy configuration example](https://github.com/marshallr12/agent_coordinator/blob/main/deploy/Caddyfile.example)

1. Verify and extract the reviewed release package. Install its
   `agent-coordinator-server` and `agent-coordinator` binaries under
   `/usr/local/bin`. Assets and migrations are embedded in the server.
2. Create an unprivileged `agent-coordinator` system account and group, a
   `/var/lib/agent-coordinator` directory owned by that account with mode 0700,
   and `/etc/agent-coordinator` owned by root.
3. Copy `service.env.example` to `/etc/agent-coordinator/service.env`, set the
   real HTTPS origin, and protect it with mode 0640 and group `agent-coordinator`.
4. As the service account, initialize the administrator before starting the
   service (the password is entered through a hidden prompt):

   ```sh
   sudo -u agent-coordinator /usr/local/bin/agent-coordinator-server \
     --database /var/lib/agent-coordinator/coordinator.sqlite3 \
     --public-origin https://coordinator.example.com \
     init-admin --username admin
   ```

   Automation may add `--password-stdin` and supply the password through a
   protected pipe. There is deliberately no password command-line argument.
   Initialization succeeds only on an empty installation; it is not an account
   recovery command. Use **My account** for password changes, or the audited host
   `recover-operator-password` command described in [the operator guide](../docs/operator-guide.md)
   for account recovery.
5. Install `agent-coordinator.service` under `/etc/systemd/system`, reload
   systemd, and enable/start the service. Install Caddy with the matching public
   hostname from `Caddyfile.example`. Permit public HTTPS to Caddy and keep port
   8080 private. Configure DNS and normal certificate issuance for that hostname.
   Validate the actual configuration with `caddy validate --config /etc/caddy/Caddyfile`
   before reloading Caddy; see its [request-body size limit documentation](https://caddyserver.com/docs/caddyfile/directives/request_body).
6. Open the HTTPS site, sign in, and issue a named agent credential. The token
   is displayed once. If that response is lost, retry the same request/key to
   recover its identity, then rotate that credential under **Access** to issue
   a replacement for the same agent principal. A token is never replayed.
7. Create the separate backup repository and install the hourly backup units:

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

   Confirm that the first oneshot succeeded and verify its reported snapshot.
   Timer activation alone is not backup evidence. Configure an alert for a failed
   backup unit and stale verified-snapshot age. See the
   [backup and restore guide](../docs/backup-restore-guide.md) for retention,
   advisory-locked off-server copies, restore authority invalidation, and the
   required recovery exercise.

   Enable the persistent schedule only after the first snapshot passes
   verification:

   ```sh
   sudo systemctl enable --now agent-coordinator-backup.timer
   ```

Install the [daily maintenance timer](../docs/retention-contract.md#daily-maintenance-timer)
for bounded replay/health payload compaction. Permanent task and lesson history
remains stored. Follow the [clock recovery guide](../docs/clock-safety-contract.md)
if the service reports a clock reconciliation pause.

For local development only, use
`--public-origin http://127.0.0.1:8080 --allow-insecure-loopback`. The explicit
opt-in permits a non-Secure development cookie solely for a loopback origin.
HTTPS uses a Secure, HttpOnly, SameSite=Strict cookie with a 12-hour absolute
lifetime and a `__Host-` prefix. Browser writes require the exact configured
Origin, `X-CSRF-Token`, and `Idempotency-Key` headers. Login requires the origin
but has no idempotency receipt because it creates a fresh browser session.

Password hashes use Argon2id with 19 MiB memory, two iterations, and one lane,
matching the [OWASP password-storage minimum reviewed on 2026-09-09](https://cheatsheetseries.owasp.org/cheatsheets/Password_Storage_Cheat_Sheet.html).
Password checks run away from async workers, with at most two concurrent checks.
Sign-in is limited to five attempts per username and 30 total attempts per minute,
using a bounded in-process table and monotonic time. Restart resets that limiter;
use proxy/network controls for internet-scale traffic. The service does not trust
client-supplied forwarding headers to bypass its global limit.

The server stores only credential/session verifiers and redacts issued tokens
from mutation receipts and events. Do not enable HTTP access logs that record
headers, bodies, or query strings. Protect the database, its WAL files, artifacts,
and every backup snapshot as credentials. Database initialization uses mode 0600;
systemd uses umask 0077. The installed backup repository is local protection only.
Do not claim host-loss protection until a complete destination copy has passed
destination-side verification, and do not claim the one-hour restore target until
the documented recovery exercise has measured it end to end.


## Supervised launch seed layout

`deploy/agentc/host-setup.sh` installs the B2 protected seed layout. The state
root and each role parent are root-owned. Role parents and `claude-config/`
are mode 0750 with the role group; their ancestors must be root-owned and
not group/world-writable. Each mutable child (`runs/`, `clones/`, `home/`,
`codex-home/`, `coordinator/`, `verification/`, `downloads/`) is separately
role-owned mode 0700. A role cannot replace the protected config directory.

The persistent Claude directory contains only root-owned, mode 0444 generated
`settings.json`, an empty root-owned mode 0444 `CLAUDE.md`, and the role-owned
mode 0600 `.credentials.json`. The root-owned mode 0444 Cargo seed at
`/etc/agentc/cargo-config.toml` contains exactly:

```toml
[net]
git-fetch-with-cli = false
```

Preparation requires that baseline and copies it to each run's private
`state/cargo/config.toml`. Each run gets separate mode 0700 home, coordinator
state, and Cargo registry/git caches; there is no writable shared-cache fallback.
Launch preflight checks seed contents, ownership, all protected ancestors,
private state permissions, and absolute canonical paths below the selected
role's `runs/` and `clones/`. Public preparation rejects symlink escapes and
linked generated outputs before writing them. Only the five most recent known
terminal state directories are retained; interrupted or unknown runs remain.

The installer seals an existing real role parent without recursively changing
ownership. It rejects symlinks, unexpected Claude config entries, and unsafe
credential files with an owner-repair instruction. Protected seeds are replaced
with fresh root-owned inodes. Existing shared role Cargo caches are unused and
are left for owner cleanup. Run the installer only after stopping role processes;
this phase does not fence an already running agent.

Claude authentication is an owner bootstrap step in a separate private directory.
Import only the credential file with its required owner and mode; never copy
other harness state or make the protected directory writable for login. The
supervisor does not read or copy credential bytes. **Pinned-harness credential
refresh remains unverified:** this layout permits in-place file updates, but
rejects refresh implementations that create a temporary file and rename it.
The containment suite checks file write permission, not successful authentication
or refresh. That live proof is required before using this layout for launches.

## Claude runtime write confinement

B3 wraps Claude launches in root-owned, non-setuid Bubblewrap. The default
binary is `/usr/bin/bwrap`; the owner installs a compatible distribution package
(Bubblewrap 0.8 or newer). Launch preflight checks the binary and its protected
ancestors and exercises the required kernel features. Missing user/PID namespace,
nested-userns disabling, or `close_range(CLOSE_RANGE_CLOEXEC)` support blocks the
launch. The installer does not change kernel policy or grant setuid privileges.

The host root and run directory are read-only. The implementer's current clone
is writable; the reviewer's clone remains read-only. Each launch separately
mounts its home, Cargo home, coordinator state, temporary directory, and build
directory writable. Their parents stay read-only, so an agent cannot rename the
Cargo directory around its read-only config overlay. The root Cargo seed and
persistent Claude settings are mounted over the per-launch Cargo config and
both persistent and generated Claude settings. `CLAUDE.md` remains read-only.
Only the persistent credential file is writable, retaining the in-place refresh
limitation above. Stdout and stderr use private files opened by the supervisor;
run metadata, prompt, lifecycle markers, and output paths remain read-only to
filesystem writes from the sandbox. Claude's stdin is a sealed Linux memory
snapshot of at most 16 MiB, with writing, growth, shrinking, and seal changes
disabled. The host prompt file descriptor is closed before spawn, so reopening
fd 0 through procfs cannot modify the original prompt. Oversized prompts or
unavailable memfd sealing fail closed; Codex's stdin behavior is unchanged.

New local clones use `--no-hardlinks`. Preflight refuses multiply-linked files,
special files, or cross-device descendants inside writable trees. Mount roots and seed
paths cannot contain symlinks. Repository symlinks are retained, but their targets
remain subject to the mount boundary. Before exec, the supervisor marks every
descriptor above stderr close-on-exec so inherited host file or directory handles
cannot bypass read-only mounts. Claude gets a fresh PID namespace and procfs,
an isolated `/dev`, no capabilities, no-new-privileges, and a new terminal
session. Implementer launches also get disabled nested user namespaces and a
read-only procfs; reviewer launches keep both available only so they can start
the candidate sandbox below. Namespace teardown kills detached
children; only an observed wrapper exit makes the run eligible for retention.

Each Claude launch also gets its own network namespace (R-P3b.4), so it cannot
reach any host loopback listener, whether the other agent account's or the owner's.
It still needs two host services, which the supervisor relays into the namespace
on their usual loopback ports. One is the egress proxy (`egress_listen`). The
other is a verifying reviewer's staging coordinator, when its URL is on loopback.
For each service, the launching supervisor listens on a Unix socket in
`$RUN/net` and connects only to that service's address, so the host firewall
still applies. Inside the namespace, `agentc-supervisor netns-relay` listens on
the same port, forwards to the socket and runs the harness. Nothing else crosses
into the namespace; a launch's own test servers stay private to it. Relayed ports
must be IPv4 loopback addresses with a port of at least 1024, because the relay
binds them unprivileged. A staging URL that names loopback in a form the relay
cannot carry faithfully is refused rather than silently left unrelayed: userinfo,
IPv6, shorthand IPv4 such as `127.1`, or a default or privileged port. Preflight
requires the supervisor binary in `bin_dir` to be root-owned, starts a namespace
once, and runs that binary's relay with the launch's ports, which refuses an
older binary without the relay. Codex launches are not wrapped and still use the host
namespace, so the firewall keeps its loopback ephemeral-range rule for them. This protects processes launched through
the supervisor. An already running, unconfined process with the same host uid
must be stopped before adoption; it is not fenced by another process's mounts.
Codex retains its existing native workspace-write profile and persistent
`CODEX_HOME`; these B3 checks do not establish additional Codex confinement.

## Reviewer candidate-code isolation

A reviewer builds and tests the implementer's candidate, so its Bash commands
may run hostile `build.rs`, test or UI-script code. Claude reviewer launches set
`CLAUDE_CODE_SHELL_PREFIX` to a generated `$RUN/candidate-shell`, read-only inside
the launch, and Claude Code runs every Bash command through it. Each command runs
in a second, nested Bubblewrap: the whole state directory is replaced by an empty
tmpfs, and only the current clone (read-only), the candidate's own home and
temporary directory, the run's Cargo home and build directory (writable), and
`verification.json` return. The
Claude configuration stays readable for shell snapshots, but its credential file
reads as empty. Coordinator state, other runs and clones, Codex state and the
verification login stay hidden; provider, GitHub and TypeSafe key variables are
unset; procfs is read-only and no further user namespace can be created. Each
command gets its own PID namespace, so a process it backgrounds ends with it.
Preflight starts the nested sandbox once and fails closed if the kernel refuses.

Codex has no equivalent per-command hook, so Codex reviewer launches are refused.
Candidate code can no longer read the long-lived verification login; reviewer UI
checks that need a staging login wait for a per-run short-lived login handed to
the candidate (decision U18), which is not implemented yet.

Candidate code gets a private home mounted over the harness's `$HOME`, so planted
`.gitconfig` or `.profile` files never reach the harness's own git and login
shells. The build and Cargo directories remain shared with the harness, so a
candidate command could move the harness's tracked working directory into one of
them, where a git command the harness runs outside the prefix might apply a
candidate-written filter with the reviewer's login. To prevent that,
`candidate-shell` resets the tracked directory to the clone after every command.
It runs a command only if the command ends with Claude Code's
`pwd -P >| <file>` step naming a file directly inside the harness's `$RUN/tmp`,
and otherwise refuses it with exit status 126. Once the nested sandbox has
exited and none of its processes remain, it writes the clone's path to that
file. Candidate commands get their own `$RUN/candidate-tmp` as `TMPDIR`, and the
harness's `$RUN/tmp` is an empty, throwaway tmpfs inside the nested sandbox, so no
candidate process, including a concurrent background command, can rewrite the
file. As a result, `cd` does not persist between reviewer Bash commands. An
instrumented owner run (a git shim logging its working directory) must still
confirm this against the pinned Claude Code release.

Local tests exercise actual Bubblewrap with a mock Bash harness, including
cross-run writes, seed replacement, hard links, inherited descriptors, and child
cleanup. The root containment suite also uses a mock shell. Host installation,
authenticated Claude runs, nested harness/browser sandbox compatibility, and
credential refresh remain owner verification work. No authenticated harness or
native Codex success is claimed by these tests.
