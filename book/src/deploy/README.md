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
`settings.json` and an empty root-owned mode 0444 `CLAUDE.md`; it holds no
login. The root-owned mode 0444 Cargo seed at
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
ownership. It rejects symlinks, unexpected Claude config entries, and linked or
non-regular Claude files with an owner-repair instruction. A leftover
`claude-config/.credentials.json` from the retired `claude auth login` flow is
overwritten with zeros (`shred`, where installed) and deleted, and the
installer logs the removal. Protected seeds are replaced
with fresh root-owned inodes. Existing shared role Cargo caches are unused and
are left for owner cleanup. Run the installer only after stopping role processes;
this phase does not fence an already running agent.

### Claude token for agent accounts

Agent accounts authenticate Claude with a long-lived token from
`claude setup-token` (decision U27), never with `claude auth login`. Claude
refreshes a `claude auth login` credential only after creating the lock
directory `.oauth_refresh.lock` in its configuration directory. `claude-config`
is root-owned and read-only to the role, so the lock fails and that login
expires within hours. Making `claude-config` writable would reopen the
persistence hole the protected seed layout closes (R-P3b.1): a role could then
plant settings or instructions that later launches load.

As the owner, run the pinned binary once per role, so each role has its own
token that can be revoked alone:

```sh
/opt/agentc/bin/claude setup-token
sudo install -o root -g agentc-impl -m 0440 /dev/stdin /var/lib/agentc/impl/claude-token
```

`install` waits on standard input without a prompt: paste the token, then
press Ctrl-D twice without pressing Enter, which stores the token alone.
Repeat with `agentc-rev` and `/var/lib/agentc/rev/claude-token`. The file sits in the role parent, outside
`claude-config`, as `root:agentc-<role>` mode 0440: the role can read it but not
change or replace it. At spawn the supervisor reads it and passes it to Claude
as `CLAUDE_CODE_OAUTH_TOKEN`; no credential file is mounted into the sandbox.
The supervisor refuses a launch whose token file is missing, is not owned by
root and the role's agent group with mode 0440, or, once trailing whitespace is
dropped, is empty, longer than 4096 bytes, or holds any whitespace, control or
non-ASCII byte. Preflight also reports a leftover
`claude-config/.credentials.json` as a problem.

The token is inference-only and long-lived. Renew it with `claude setup-token`
and the same `install` command before it expires. If it is ever exposed, revoke
it at claude.ai and install a new one. `host-setup.sh`
never creates or prints a token. On each run it holds an existing root-owned
one at `root:agentc-<role>` 0440, and refuses a symlink, a multiply-linked file
or a token with any other owner with an owner-repair instruction. It warns when
the token is the containment suite's leftover dummy, comparing only a file of
exactly the dummy's size. `--uninstall` overwrites each token with zeros and
deletes it.

## Claude runtime write confinement

B3 wraps Claude launches in root-owned, non-setuid Bubblewrap. The default
binary is `/usr/bin/bwrap`; the owner installs a compatible distribution package
(Bubblewrap 0.8 or newer). Launch preflight checks the binary and its protected
ancestors and exercises the required kernel features. Missing user/PID namespace,
nested-userns disabling, or `close_range(CLOSE_RANGE_CLOEXEC)` support blocks the
launch. The installer does not change kernel policy or grant setuid privileges.

Ubuntu 24.04 and newer set `kernel.apparmor_restrict_unprivileged_userns=1`.
Its `bwrap-userns-restrict` profile strips capabilities from everything
`/usr/bin/bwrap` starts, so the reviewer's nested candidate sandbox fails
preflight (`unpriv_bwrap` denies `sys_admin`). A local override cannot lift that
deny. Run host-setup with `APPARMOR_BWRAP=1` to opt in to an agentc-only
Bubblewrap instead:

- it copies `/usr/bin/bwrap` to `/opt/agentc/bin/bwrap` as `root:agentc-bwrap`
  with mode `0750`, and adds only `agentc-impl` and `agentc-rev` to that group;
- it loads `/etc/apparmor.d/agentc-bwrap`, whose children inherit it;
- it sets `bubblewrap` in `supervisor.toml` to the copy.

The host-wide restriction stays on for every other account. The copy does not
follow distribution updates, so re-run host-setup after a Bubblewrap upgrade.
The containment suite fails while the copy differs from `/usr/bin/bwrap`, or
while the group holds anyone but the two role accounts. A setup run without
`APPARMOR_BWRAP=1` removes the copy, the profile and the group, as does
`--uninstall`.

The opt-in gives the role accounts what an unrestricted host (such as Debian)
already allows: a user namespace with capabilities inside it. Claude launches
stay fenced: the implementer sandbox and the reviewer's nested candidate
sandbox both disable further user namespaces, and Codex reviewers are
refused. Codex implementers, which run without Bubblewrap, can reach the copy
like any program on a Debian host can reach `/usr/bin/bwrap`.

The host root and run directory are read-only. The role's other runs are hidden:
an empty tmpfs replaces its `runs/` directory and only the launch's own run is
bound back. The implementer's current clone
is writable; the reviewer's clone remains read-only. Each launch separately
mounts its home, Cargo home, coordinator state, temporary directory, and build
directory writable. Their parents stay read-only, so an agent cannot rename the
Cargo directory around its read-only config overlay. The root Cargo seed and
persistent Claude settings are mounted over the per-launch Cargo config and
both persistent and generated Claude settings. `CLAUDE.md` remains read-only.
Nothing in the persistent Claude directory is writable, and the launch gets
its login only as `CLAUDE_CODE_OAUTH_TOKEN`. Stdout and stderr use private files opened by the supervisor;
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
Claude configuration stays readable for shell snapshots and holds no login; the
role's `claude-token` lies under the hidden state directory, and
`CLAUDE_CODE_OAUTH_TOKEN` is unset. Coordinator state, other runs and clones, Codex state and the
verification login stay hidden; provider, GitHub and TypeSafe key variables are
unset; procfs is read-only and no further user namespace can be created. Each
command gets its own PID namespace, so a process it backgrounds ends with it.
Preflight starts the nested sandbox once and fails closed if the kernel refuses.

Codex has no equivalent per-command hook, so Codex reviewer launches are refused.
Candidate code can no longer read the long-lived verification login. Instead,
per run (decisions U18 and U22), the supervisor signs in to the staging
coordinator with that login before the harness starts. It hands candidate
commands only the new browser session's cookie, in a read-only
`$RUN/verification-session.json`, and signs the session out when the launch
ends. A missed sign-out, such as a supervisor crash, is bounded by the
coordinator's fixed 12-hour session lifetime. If sign-in fails, the launch
fails. The reviewer harness itself no longer sees the logins either: its
sandbox mounts an empty tmpfs over the reviewer's `verification` directory.
Loopback staging is reached directly; any other staging host must use https
and is reached through the egress proxy. This uses the Agent Coordinator's
own login API, so it serves coordinator staging only.

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
cleanup. The root containment suite also uses mock harnesses. For each role it
runs one real `agentc-supervisor launch` from a temporary root-owned bin
directory whose `claude` is a mock: inside the launch's network namespace it
must reach the egress proxy through the relay (for the reviewer, also from a
candidate command) and must not reach the network directly. A second bin
directory whose supervisor cannot relay must fail preflight. A role with no
`claude-token` gets a dummy one (`root:agentc-<role>` mode 0440) for the run,
so these mock legs pass preflight before the owner installs real tokens. On
exit the suite removes only the dummies it created, and only while they still
hold the dummy text. If that cleanup never ran (SIGKILL, power loss), the next
run reports the leftover dummy as a `FAIL` with the command that removes it. The
suite never prints a token and compares an existing one with the dummy only
when its size matches. If installing a dummy fails, the suite removes its
partial file and stops. For each role the
suite checks the token's owner and mode, that only that role can read it and
nobody can write it, that the launch received `CLAUDE_CODE_OAUTH_TOKEN`, and
that a reviewer candidate command can neither read the token file nor see that
variable. With a dummy it prints a `NOTE` that real-token authentication is not
exercised. For every role it checks that `claude-config` refuses new entries,
including the `.oauth_refresh.lock` directory. Host installation,
authenticated Claude runs, and nested harness/browser sandbox compatibility
remain owner verification work. No authenticated harness or
native Codex success is claimed by these tests.

The real-Bubblewrap tests (the `sandbox` unit tests and the `launch_relay` and
`netns_relay` integration tests) run `/usr/bin/bwrap` unless
`AGENTC_TEST_BWRAP` names another binary. With `--cargo-test`, the suite sets it
to `/opt/agentc/bin/bwrap` when host-setup installed that copy, because Ubuntu's
AppArmor restriction refuses the unconfined `/usr/bin/bwrap` to `agentc-impl`.
Inside `codex sandbox`, Bubblewrap cannot nest: the uid mapping hides its root
ownership and user namespaces are unavailable. The suite's Codex leg therefore
sets `AGENTC_TEST_NESTED_SANDBOX=1`, and those tests return early with a
`note: skipping <test>` line, written straight to stderr so it appears in the
leg's log without `--nocapture`. The suite fails the leg if that log has no such
note. `CODEX_SANDBOX` or
`CODEX_SANDBOX_NETWORK_DISABLED` has the same effect. Without one of these
variables, a Bubblewrap that cannot run fails the tests; the launch-time
Bubblewrap checks are unchanged.

### The in-launch gate

The supervisor also sets `AGENTC_TEST_NESTED_SANDBOX=1` in the environment of
every launch, implementer and reviewer, Claude and Codex, so the gate a role
runs inside its sandbox skips what that sandbox cannot provide. Besides the
real-Bubblewrap tests (no nested user namespaces there), the `push_helper`
tests and the `preflight` push-socket test skip because their fixtures live
under `/tmp` for short socket paths, and a launch's `/tmp` is the host's,
mounted read-only. The `sandbox` mount-audit test skips because it binds a
socket deep under `$TMPDIR`, and a launch's long `$TMPDIR` pushes that path
past the kernel's 108-byte limit. Each skip prints its
`note: skipping <test>: <reason> (AGENTC_TEST_NESTED_SANDBOX is set)` line.

Both role contracts define that in-launch gate: `cargo fmt --all -- --check`,
`cargo clippy --workspace --all-targets --locked -- -D warnings` and
`cargo test --workspace --locked` (or the repository's own equivalents), plus
any other gate script that can run in the sandbox, with each nested skip
reported as skipped rather than passed. The reviewer judges "full gate green"
by that gate. The skipped tests are not dropped: the project's required checks run
them on an ordinary runner, and the integrator requires those checks green on the
integrated revision before it lands anything. Outside a launch, without a
marker, every one of these tests runs and fails loudly when its host resource
is missing.

To reproduce a launch's view locally, run the workspace tests in a read-only
Bubblewrap root as your own account, with the variable set and only the
target, home and temporary directories writable, for example
`bwrap --unshare-user --disable-userns --unshare-net --ro-bind / / --proc /proc --dev /dev --bind <target> <target> --bind <tmp> <tmp> --setenv TMPDIR <tmp> --setenv AGENTC_TEST_NESTED_SANDBOX 1 cargo test --workspace --locked --no-fail-fast`
from the checkout (prebuild the tests first, since the sandbox has no
network), or under `codex sandbox` as the containment suite's Codex leg does.

### Reviewer headless browser

A verifying reviewer gets the configured `browser` as `CHROME_BIN`. Host-setup
installs Playwright's Chromium headless shell for it and writes its path to
`browser` in `supervisor.toml`. The shell is pinned to Playwright v1.63.0's
`chromium-headless-shell` (Chrome for Testing 153.0.8010.12, browser revision
1243) for x86-64 and ARM64:

| Architecture | Download | SHA-256 |
| --- | --- | --- |
| x86-64 | <https://cdn.playwright.dev/builds/cft/153.0.8010.12/linux64/chrome-headless-shell-linux64.zip> | `a9da028861a0cf789ff25c2fed45f5f1aaf969ed9247835b6a7821a4f7af9d1d` |
| ARM64 | <https://cdn.playwright.dev/builds/cft/153.0.8010.12/linux-arm64/chrome-headless-shell-linux-arm64.zip> | `d433c45172c7836e38124fe545f767b02210bfb43a6262f08a297473a8e91c99` |

The x86-64 zip is byte-identical to Google's Chrome for Testing build of the
same version. Host-setup downloads the zip as root, refuses it unless the
checksum matches, and unpacks it `root:root` (directories `0755`, files
`0644`) under `/opt/agentc/browsers/1243/`, with a `.manifest` of every file's
SHA-256. A re-run re-hashes the tree against that manifest (about two seconds),
reinstalls it if any file changed, is missing or was added, and deletes other
revisions. When the dynamic loader cannot
resolve one of the shell's libraries, host-setup installs Playwright's Chromium
runtime package list with `apt-get` (the `t64` names on Ubuntu 24.04+ and
Debian 13+), plus `fonts-liberation`. With `APPARMOR_BWRAP=1` it also loads
`/etc/apparmor.d/agentc-browser`. Like Ubuntu's own profile for Chrome, that
profile is unconfined except that it allows user namespaces, so the shell can
start its own sandbox under the restriction. It attaches only to the pinned
executable, but grants user namespaces to every account that runs it. So, as
with the agentc Bubblewrap copy, host-setup first makes the executable
`root:agentc-bwrap` mode `0750`, runnable only by `agentc-impl` and
`agentc-rev`, and then loads the profile. Implementers keep access for their
own UI checks. Without `APPARMOR_BWRAP=1`, host-setup unloads the profile first
and then makes the executable `root:root` mode `0755`.

`browser` is chosen in this order:

1. the pinned headless shell, when installed;
2. a non-snap `/usr/bin/chromium`, `/usr/bin/chromium-browser` or
   `/usr/bin/google-chrome`;
3. the in-code default, `/usr/bin/chromium`.

A snap wrapper is never chosen. Ubuntu's `chromium-browser` is one: a snap
cannot run as `agentc-rev` outside a login session. Set `HEADLESS_SHELL=0` to
skip the shell, for example on a host without UI verification: host-setup then
deletes `/opt/agentc/browsers` and the profile, so `browser` falls back to a
system browser. Architectures other than x86-64 and ARM64 get the same
treatment. `--uninstall` also removes both. The containment suite fails if
`browser` is a snap wrapper. It also fails if the pinned shell's mode is wrong:
`750 root:agentc-bwrap` with the profile loaded, else `755 root:root`. With the
profile, it also fails if `agentc-egress` or `agentc-push` can run the shell.
With a staging coordinator running, the suite also has `agentc-rev` render the
staging dashboard with that browser.

## Implementer candidate-push helper

An implementer launch publishes its candidate through `agentc-push`, a helper
that runs as its own unprivileged account and alone can read the push App key
(decision U25). Root starts both the helper and the launch with
`agentc-supervisor launch-root`, which takes the same launch arguments as
`launch` plus `--task <id>` (required for an implementer):

```sh
sudo /opt/agentc/bin/agentc-supervisor launch-root --role implementer --harness claude \
  --clone <clone> --run <run> --task <task-id>
```

`launch-root` refuses to run unless it is root, and checks the host first:
the installed `agentc-supervisor` must be root-owned and not
group/world-writable and the role account must exist; for an implementer, so
must the helper binary, its root-owned configuration file and the helper
account. It then creates a per-launch
directory, `<state_dir>/push/<session-id>/` (root, mode 0711), which holds
`sock/` (helper account and implementer group, mode 2750, so the helper's 0660
socket `sock/push.sock` is reachable only by the helper and implementer
accounts and root) and `work/`
(helper account, mode 0700, the helper's home and private repository). Only
root writes `<state_dir>/push`, so no agent account can rename or replace a
path root creates or removes there. The run directory must already exist and
the published socket path may be at most 93 bytes, because the helper first
binds a slightly longer staging name.

It digests every token in the implementer's coordinator credential files,
`$RUN/state/coordinator/credentials.toml` and
`<state_dir>/impl/coordinator/credentials.toml` when present, and passes the
SHA-256 digests to the helper as `--known-digest`, so a candidate carrying that
token is refused. These files and the helper's log, `$RUN/push-helper.log`
(root-owned, mode 0600), are opened one path component at a time without
following symlinks. The helper runs as the helper account with only `PATH`,
`LANG` and `HOME` (its `work/` directory) set and `--parent-pid` naming
`launch-root`, which waits up to 10 seconds for the socket and fails the launch
if the helper exits first. The launch then runs as the implementer, with only
`PATH`, `LANG` and the role's home as `HOME`, as `agentc-supervisor launch
--session-id <id> --task <task-id> --push-socket <socket>`. The session id is
also the helper's launch id, so the candidate ref is
`refs/agent-coordinator/candidates/<task-id>/<session-id>`; it publishes one
commit there and never moves it. `checkpoint --push-wip` requests instead go to
create-only refs `refs/agent-coordinator/candidates/wip/<task-id>/<session-id>/<sha>`,
which the helper also derives itself, never updates or deletes, and which do not
spend the candidate. The request cannot name any other ref. `launch` checks
that the socket is an implementer's, absolute, at most 93 bytes long and a
socket in a launch's `sock/` directory directly under `<state_dir>/push`, and
exports it as `AGENT_COORDINATOR_CANDIDATE_PUSH_SOCKET`. A Claude launch's
sandbox replaces `<state_dir>/push` with an empty tmpfs and binds back only its
own socket directory, read-only, so it cannot reach another launch's helper.

Codex launches are not sandboxed this way: they run in the host namespace as
the implementer account, which is in every implementer socket directory's
group, so file permissions alone would let a Codex implementer reach the
helper of any other implementer launch running at the same time (each run's
`.state-started` file holds its session id). The helper therefore serves only
its own launch: for every connection it reads the client's process id from
the socket (`SO_PEERCRED`) and walks the parent ids in `/proc` up to the
`launch-root` that spawned it, refusing any other client with "connection is
not from this helper's launch" before minting a token or reading the request.
Leftover processes stay inside that tree, since `launch` is their subreaper. A
client whose process id or ancestry cannot be read is refused. The check names
the client process at connect time; with no pidfd for the peer on the
supported kernels, a client that exits and whose process id is reused by a
process of the launch in between is not excluded, which a separate launch
cannot arrange on purpose. The check reads `/proc`, so a host that mounts it
with `hidepid` makes the helper refuse every client: safe, but every push fails.
Codex implementers share one account in the host namespace, so on a kernel
with `kernel.yama.ptrace_scope=0` one could attach to a process of a
concurrent launch and connect from inside that launch's tree. Ubuntu sets
`ptrace_scope` to 1, but Debian leaves it at 0 (mxmini reads 0), so set
`kernel.yama.ptrace_scope=1` in `/etc/sysctl.d/` on every supervised host before
running Codex implementers concurrently.

Every harness, Codex included, starts with `no_new_privs`, so no setuid,
setgid or file-capability binary can raise its privileges. The `launch`
command is the child subreaper of its harness: processes the harness leaves
running, even detached with `setsid` or a double fork, are re-parented to it
and killed, whole tree at once, once the harness exits and before the run is
marked finished, or as soon as a launch is refused (a preflight probe's
leftovers included). The supervisor reports on standard error what it
found, by command name, in one informational line such as
`agentc-supervisor: reaped 3 exited launch processes (bwrap x3)`. Exited
processes are expected: each Bubblewrap run (the two preflight sandbox
probes, the project setup when one is configured, and the harness) exits as
soon as its PID-namespace init reports the command's status, usually without
reaping that init. By then the init has exited and the kernel has killed
everything else in the namespace; the init re-parents to `launch` as a
zombie and is only reaped, so a Claude launch reports up to one per
Bubblewrap run. A `killed N leftover launch processes (…)` part instead
names processes that were still running, such as a Codex harness's detached
children, and is worth a look when it recurs. Preflight version checks and
sandbox probes also run with `no_new_privs`, so a probe passes only if the
launch itself can run.
`launch-root` is a subreaper too, so a Codex harness that kills its own
`launch` process (they share an account) still has its leftovers killed when
`launch-root` finishes.

When the launch ends, whatever its outcome, `launch-root` sends the helper
SIGTERM, kills it if it has not exited within 5 seconds, removes the per-launch
directory and exits with the launch's exit code; a failure to stop the helper
or remove the directory is reported as a warning and does not replace that
code. If `launch-root` itself is killed, the helper ends with it (it asks for a
parent-death signal). The next implementer `launch-root` removes any
per-launch directory whose lock no running `launch-root` holds, and any
half-built `.new-` directory. It sweeps and builds while holding
`<state_dir>/push/.sweep` locked, so concurrent `launch-root` runs never sweep
each other's directories. A reviewer launch gets no helper: `launch-root` only runs
it as the reviewer account.

The `[push_helper]` table in `/etc/agentc/supervisor.toml` overrides the
in-code defaults shown below. The helper binary must be root-owned with
root-owned, non-group-writable parents; on Debian `/usr/local/bin` is often
group `staff` and group-writable, so the default is beside the other pinned
binaries. `deploy/agentc/host-setup.sh` installs it there and writes this table
above the `KEEP` line, where each re-run replaces it:

```toml
[push_helper]
program = "/opt/agentc/bin/agentc-push"
# config = "/etc/agentc/push.toml"
# user = "agentc-push"
```

### Setting up the helper host

`deploy/agentc/host-setup.sh` prepares everything except the App key:

- the `agentc-push` system account, with its own group, no home and no login
  shell. If the account already exists, the script stops with an
  owner-repair message unless its uid is not 0 and not that of `agentc-impl`,
  `agentc-rev` or `agentc-egress`, its shell is `nologin` or `false`, and its
  only group is `agentc-push`;
- `/opt/agentc/bin/agentc-push` (root, mode 0755), copied from `PUSH`, which
  defaults to `agentc-push` beside `SUPERVISOR`;
- `<state_dir>/push` (root, mode 0711);
- `/etc/agentc/push.toml` (root, group `agentc-push`, mode 0640), written only
  when absent, from `PUSH_APP_ID` (default 5168037), `PUSH_INSTALLATION_ID`
  (default 167333814) and `PUSH_REPOSITORY` (default `REPO_URL`, this
  repository); `private_key` keeps its default, `/etc/agentc/push-app.pem`. An
  existing file is never rewritten; the script stops unless it is a root-owned,
  single-link regular file that is not group/world-writable and `agentc-push`
  can read;
- if `/etc/agentc/push-app.pem` exists, mode 0400 and then owner
  `agentc-push:agentc-push`, so only the helper account can read it. The script
  never reads, prints, copies, creates or fetches the key; it refuses a symlink
  or a hard-linked key.

The helper account is not in the agent firewall's uid set: it reaches
`api.github.com` and `github.com` directly, not through the egress proxy.

The owner's one-time steps on the helper host:

1. Build the binaries, including the helper (part of the `agentc-integrator`
   package):

   ```sh
   cargo build --release --locked -p agentc-supervisor -p agentc-integrator -p coordinator-cli
   ```

2. Place the push App's private key at `/etc/agentc/push-app.pem` (root, mode
   0400), never in the repository. Each host gets its own key generated in the
   App's settings.
3. Run `sudo SUPERVISOR=target/release/agentc-supervisor
   CLI=target/release/agent-coordinator deploy/agentc/host-setup.sh` (or re-run
   it after placing the key, which hands the key to `agentc-push`). Without a
   key it prints that step instead; launches still start, but every candidate
   push fails.
4. Run `sudo deploy/agentc/containment-suite.sh`.

`host-setup.sh --uninstall` first, while the agent firewall is still in
place, retires the agent and `agentc-push` accounts: it disables their
lingering, kills every process running under them (real or effective uid),
and removes their crontabs, `at` jobs and the files they own in `/tmp`,
`/var/tmp` and `/dev/shm` (symlinks themselves, never their targets), then
deletes them. It then stops the egress service, retires the egress account
the same way, removes the firewall and, by explicit path, the files and
directories the script installs. It warns about anything it could not remove. It keeps
`/etc/agentc/push-app.pem` for the owner to delete: once `/etc/agentc` and its
parents are root-owned and not group/world-writable, a single-link regular key
is returned to root (mode 0400), and a symlinked or hard-linked one is left
untouched and reported. It also leaves anything else in `/opt/agentc`,
`/var/lib/agentc` and `/etc/agentc`, such as the integrator's binary, state, configuration and keys,
or an owner-placed `shadow-credentials.toml`. Each of those directories is
removed only once empty.

Local tests run `launch-root`'s steps as the test's own account against stub
helper and supervisor programs. Switching accounts needs root, so the root
containment suite covers it. Its push leg runs real implementer `launch-root`s
from a temporary bin directory whose `claude` is a mock and whose helper
configuration cannot mint a token (no key file, an unreachable https API), so
nothing reaches GitHub. With the helper serving, it checks:

- the helper process runs as `agentc-push` with exactly that account's groups;
- the socket is mode 0660 `agentc-push:agentc-impl`, in a mode 2750
  `sock/` directory of a mode 0711 root launch directory;
- `agentc-impl` can connect to the socket and `agentc-rev` cannot;
- inside the sandbox, the mock harness gets
  `AGENT_COORDINATOR_CANDIDATE_PUSH_SOCKET`, can connect to it, and sees no
  launch directory under `<state_dir>/push` but its own, holding only `sock/`.

After the launch it checks that `launch-root` exited 0, the launch directory is
gone and the helper exited. A first `launch-root`, killed with SIGKILL while its
harness waits, must take its helper with it and leave its directory, which the
next `launch-root` must sweep. When `/etc/agentc/push-app.pem` exists, it must be
mode 0400 `agentc-push:agentc-push`, readable by `agentc-push` and by neither
agent account; without it those checks are skipped.

## Bringing up a supervised host

This is the owner's ordered checklist for preparing a host to run supervised
launches, such as oracle-1 as the primary pilot host (decision U6). Steps
that have background link to the section that explains it. All steps that
need root are the owner's; an agent only prepares commands and reads the
pasted output. Stop at the first step that fails.

Prerequisites: the owner's pinned `claude` and `codex` at `~/.local/bin`
(host-setup always installs both, so `codex` is required even when only Claude
runs), Bubblewrap 0.8 or newer, and, for UI verification, `node` on `PATH` or
named by `NODE=`. The installer downloads the pinned reviewer
[headless browser](#reviewer-headless-browser) and may `apt-get` its libraries.

1. **Build on the host from one clean revision.** The published release
   packages are x86-64 only, so an ARM64 host such as oracle-1 builds its own
   binaries. Use a clean, detached checkout of the current `main`:

   ```sh
   git -C ~/src/agent_coordinator fetch origin
   git -C ~/src/agent_coordinator worktree add --detach ~/src/worktrees/agentc-host origin/main
   cd ~/src/worktrees/agentc-host && export CARGO_TARGET_DIR=$PWD/target
   cargo build --release --locked -p agentc-supervisor -p agentc-integrator -p coordinator-cli
   ```

   The `agentc-integrator` package also builds the `agentc-push` helper. An
   integrator already installed on the host
   ([integrator cutover](../docs/integrator-cutover.md)) keeps its own binary,
   state and configuration; nothing here touches them.
2. **Stop role processes,** then run the installer. On Ubuntu 24.04 and newer,
   opt in to the agentc-only AppArmor profile
   ([write confinement](#claude-runtime-write-confinement)):

   ```sh
   sudo APPARMOR_BWRAP=1 SUPERVISOR=target/release/agentc-supervisor \
     CLI=target/release/agent-coordinator deploy/agentc/host-setup.sh
   ```

   It must exit 0. Keep its printed manual steps.
3. **Per-role Claude tokens.** Check that `/var/lib/agentc/impl/claude-token` and
   `/var/lib/agentc/rev/claude-token` are `root:agentc-<role>` mode 0440. If a
   role has none, or the installer warned about a leftover dummy, install one
   ([Claude token](#claude-token-for-agent-accounts)). Codex roles log in
   with the `codex login` command the installer printed.
4. **Push App key.** Each host gets its own key, generated in the push App's
   settings. Place it at `/etc/agentc/push-app.pem` (root, mode 0400), then
   re-run step 2 so it is handed to `agentc-push`
   ([helper host](#setting-up-the-helper-host)).
5. **Coordinator credentials.** In the dashboard, issue `class=supervised`
   credentials for the project: write access for the implementer and read
   access for the reviewer. Save each as
   `/var/lib/agentc/<role>/coordinator/credentials.toml` (mode 0600, owned
   by the role).
6. **Staging for UI verification.** The simplest setup runs the staging
   coordinator on this host, which candidates reach on loopback through the
   launch relay; an https staging coordinator on another host is reached
   through the egress proxy instead
   ([reviewer isolation](#reviewer-candidate-code-isolation)).
   Run `deploy/agentc/staging.py up` as the owner, run the
   commands `deploy/agentc/staging.py credentials` prints, and add its
   `[verification.<project-id>]` entry below the `KEEP` line in
   `/etc/agentc/supervisor.toml`.
7. **Containment suite,** including the build-under-containment leg:

   ```sh
   sudo deploy/agentc/containment-suite.sh --cargo-test
   ```

   Pass means rc 0 and no `FAIL`. A `NOTE` about a dummy token means step 3
   is unfinished. `SKIP` is acceptable only for a leg whose input this host
   lacks by design; record the reason.
8. **Record the evidence** outside Git. Paste the installer's and the suite's
   summaries, with the binaries' commit, into the bring-up task, then remove
   the build worktree with `git worktree remove`.

A host prepared this way can run `agentc-supervisor launch-root` by hand, or
claim work unattended with the live loop below.

## Live supervisor loop

`agentc-supervisor run` (as root) is the P3b pilot-core loop for one host.
Each poll it:

1. refuses to claim while the filesystem holding `/var/lib/agentc` has less
   than `[run] min_free_mib` free (default 20480);
2. reads `next` for the implementer with
   `/var/lib/agentc/impl/coordinator/credentials.toml`, for the project named
   by `.agent-coordinator.toml` on the mirror's `[run] branch` (default
   `main`), or by `[run.binding]` when the host configuration sets it
   ([staging](#running-the-loop-against-staging));
3. for a `claim_task` suggestion, fetches `/var/lib/agentc/mirror.git` and,
   as `agentc-impl`, clones its branch head to `impl/clones/<session>` and
   prepares `impl/runs/<session>`. The clone gets a repository-local commit
   identity from `[run] git_name` and `git_email` (default `agentc
   implementer` / `agentc-impl@agentc.invalid`), since the agent may not run
   `git config`. The loop copies the credential into the run's
   coordinator state (`state/coordinator/credentials.toml`, or
   `state/coordinator/<project_name>/config/credentials.toml` when the
   binding sets `project_name`, which is where the CLI then looks);
4. as `agentc-impl`, connects a coordinator session named after the launch
   and claims the task with `agent-coordinator claim`; the launch gets the
   same session through `AGENT_COORDINATOR_SESSION`;
5. writes the launch record `/var/lib/agentc/launches/<session>.json` (the
   attempt, its generation and this boot's id). Then, in the same session as
   `agentc-impl`, it runs `agent-coordinator worktree prepare`, which adds
   the attempt's checkout as a worktree of the clone at
   `impl/clones/<session>/agentc-checkout` (branch `agentc/<session>`, at
   the cloned revision; the clone's `info/exclude` hides it) and registers
   it for the attempt, so `submissions code --checkout` accepts it. It writes
   the prompt: the implementer contract
   (`crates/supervisor/contracts/implementer.md`, at most 1,500 words) with
   the attempt, its generation and the checkout filled in, the task title
   inside `<task-title>` tags and the repository's `AGENTS.md` and
   `CONTRIBUTING.md` appended inside `<repository-instructions>` tags as
   data. Closing tags inside the data are defused, whatever their case. The
   agent commits in the checkout and submits with `submissions code`, which
   publishes the candidate through the push helper. A failed registration or
   prompt releases the attempt with a handoff. The loop then spawns `launch-root` in a
   process group of its own, and adds the launch's pid and `/proc` start time
   to the record;
6. renews the attempt while the launch runs (see below), and releases it with
   a handoff summary once the launch exits or fails to start; an attempt the
   agent already submitted or released is left as it is;
7. removes the clone and the run directory once the run is terminal (or never
   started), then the record; a started run without a terminal record is kept
   for recovery;
8. rewrites `/var/lib/agentc/heartbeat.json` (poll count, time, outcome).

Renewal is progress-gated. At the service's `renew_after_seconds` cadence the
loop renews only while `launch-root` is alive, `$RUN/events.jsonl` (the
harness's event stream) changed in the last 15 minutes, the
attempt's last checkpoint is under 60 minutes old, and the launch is within
`[run] budget_minutes` (default 240). The checkpoint age comes from the
service's own claim and renew responses (`last_progress_at` against
`expires_at` less `lease_remaining_ms`), so it needs no extra call and no
local clock. Once a gate fails, the loop logs why, stops renewing and
drains the launch: SIGTERM to its process group, SIGKILL after `[run]
drain_seconds` (default 30), then a release whose handoff names the reason.
A hung harness therefore cannot hold the loop. A renewal the service
refuses for good (error `lease_expired`, `operation_not_permitted` or
`record_not_found`: the agent submitted or released the attempt, or it
expired or changed hands) drains the launch the same way at once. The loop
reads the attempt's state from the task: an attempt the agent submitted
counts as success and is not released (its cost is still recorded), while
any other ended attempt gets the usual release with the reason in its
handoff. Every other renewal failure (a network error, a 5xx, an unresolved
CLI journal) is logged with the CLI's exit code, error code and bounded
message, and retried at the next cadence under the same gates. A host suspend counts as
elapsed time: after a suspend longer than 15 minutes the launch is drained
on resume (its lease has usually lapsed during the suspend anyway). A project
can also cap attempts on the service with the policy's `max_attempt_seconds`.

Before each poll the loop settles the records an earlier poll or loop left.
A launch recorded on this boot whose pid still runs with the recorded start
time may be alive: it is never respawned, and the loop claims nothing while
it lives. A record without a pid (the loop died between writing the record
and spawning) counts as alive for two minutes. Any other recorded launch has
its attempt released (unless that was done) and its clone, run and record
removed. After five failed releases the loop logs an error and leaves that
record alone until it restarts. To clear a stuck record by hand, check that
no `launch-root` for its session runs, release the attempt with
`agent-coordinator release` (or let the lease lapse), then remove
`/var/lib/agentc/launches/<session>.json`,
`/var/lib/agentc/impl/clones/<session>` and
`/var/lib/agentc/impl/runs/<session>`.

SIGTERM or SIGINT drains the loop the same way: it claims nothing new, drains
the running launch, releases the attempt with a handoff checkpoint and exits.
The unit uses `KillMode=mixed`, so systemd signals only the loop and kills
what is left only after it exits.

`agentc-impl` owns `coordinator/` and `runs/`, so root never follows a
symlink there. The credential is read, and the run's credential copy and
prompt are created, one path component at a time from the root-owned
`/var/lib/agentc/impl` without following a symlink. The file read must be a
regular, single-link file owned by `agentc-impl` of at most 64 KiB; a FIFO
is refused without blocking. Errors name the path, never the contents. The
instruction files are read as size-bounded blobs from the root-owned mirror
at the cloned revision, never from the clone. The event stream's modification
time is read the same way, from a regular file owned by `agentc-impl`, and
so are the run's `.state-started` and `.state-terminal.json` markers. The
clone and run are removed through a directory opened without following
symlinks. Launch records live in the root-owned state directory. Every
coordinator command run as `agentc-impl` (connect, claim, renew, release)
reads the repository binding from `/var/lib/agentc/coordinator-binding.toml`
(`AGENT_COORDINATOR_REPO_CONFIG`), a root-owned copy of the mirror's
`.agent-coordinator.toml` (or of `[run.binding]`), never the clone's
role-writable copy. The loop refuses to start when the binding's origin is
plain `http` to a host that is not loopback, or plain `http` on loopback
without `[run] allow_insecure_loopback = true`; only then do its own client,
its role commands and the reviewer's commands get
`AGENT_COORDINATOR_ALLOW_INSECURE_LOOPBACK`. An `https` origin never does,
whatever the setting.

### Admission, cost and project setup

After the disk check, each poll admits a claim only when all of these hold:

- the kill switch is not set: while `/var/lib/agentc/kill-switch` (`[health]
  kill_switch`) exists, in any form, the loop claims nothing, so creating it
  stops new claims within one poll. A path that cannot be checked (any error
  but "not found") counts as set. A running launch is not interrupted;
- the implementer has spent less than `[health] implementer_daily_usd`
  (default 150; 0 disables) in the last 24 hours, by the cost ledger;
- a vendor is usable. The `[run]` harness comes first, then the optional
  `[health.fallback]` (harness, model, effort). A vendor is skipped while a
  rate limit marks it exhausted, once its credential has expired, or when its
  own sign-in check fails: `claude auth status` with the role's token, or
  `codex login status`, run as `agentc-impl`. Only the check's exit code is
  used. A Codex vendor is also skipped for a project with a setup command
  (see below). The launch runs, and connects its session, with the chosen
  vendor, which its launch record keeps.

Once a day the loop logs a warning for each credential expiring within
`[health] expiry_warn_days` (default 14). A Claude token's expiry is its
`claude-token` file's modification time plus `token_lifetime_days` (default
365); Codex refreshes its own login.

When a launch ends, the loop reads its `$RUN/events.jsonl`. A rejected Claude
`rate_limit_event`, a failed result reporting a 429 or usage limit, or a Codex
`error` or `turn.failed` naming one marks that vendor exhausted in
`/var/lib/agentc/vendors.json` until the reset time the event names, else for
`[health] exhausted_minutes` (default 60); the following polls route to the
fallback, or refuse, until then. Tool output is never inspected. The launch's
tokens and dollars (Claude's own `total_cost_usd`; Codex usage priced with the
`[shadow.prices]` table, or "unpriced") are appended to
`/var/lib/agentc/costs.jsonl` with the project, task, session and attempt,
and the release handoff ends with the same figure. A launch the agent already
submitted or released keeps only the ledger entry. Recovery does the same for
a launch that ended while no loop watched it, before removing its run, under
the vendor its record names; the record notes that the cost is in the ledger,
so a retried release never counts it twice.

A reviewer launch's cost is recorded the same way, as one `role` `rev` row
(task under review, reviewer session, tokens, dollars) once the launch ends,
so the reviewer's `[health] reviewer_daily_usd` cap counts it. The loop keeps a
root-owned record, `/var/lib/agentc/reviews/<session>.json`, from before the
launch spawns until its cost is in the ledger and its run is removed; a record
left by a supervisor that stopped meanwhile is settled on the next poll, and
the ledger holds at most one reviewer row per session.

A host may configure a project setup command, keyed by coordinator project id:

```toml
[setup.<project-id>]
command = ["cargo", "fetch", "--locked"]
cache_paths = ["/var/cache/agentc/cargo"]
timeout_seconds = 900
```

`launch` runs the command in the clone after preflight and before the
harness, inside the launch's own Bubblewrap sandbox, environment and network
namespace, with its output in `$RUN/setup.log`. A non-zero exit or a timeout
refuses the launch. Each cache path must be an existing absolute directory,
reached without symlinks and owned by `agentc-impl`; it is bound writable, at
the same path, into the sandbox of the setup and of the harness. Codex
launches have no Bubblewrap boundary, so admission never routes a project
with a setup command to Codex, and `launch` refuses such a launch. The repository cannot add a command or a cache:
both come only from the host's configuration.

### Reviewer launches

With `[run] reviewer = true` each poll first takes one review. Root reads
reviewer `next`, the subject task and its submission with the reviewer
principal's write credential, `/var/lib/agentc/verdict/home/credentials.toml`
(a root-owned mode 0700 directory, so no launch can read it; the loop refuses
to start otherwise). It connects a fresh session there and claims the review
with `agent-coordinator reviews claim`, which verifies the candidate in a
root-owned clone of the mirror. It fetches the candidate ref into the mirror
as `refs/heads/agentc-review/<session>` and, as `agentc-rev`, clones it to
`rev/clones/<session>` and prepares `rev/runs/<session>`. The prompt is the
reviewer contract (`crates/supervisor/contracts/reviewer.md`) with the
criteria and submission as JSON data whose `<` are escaped, then the base
revision's instruction files. The reviewer launch gets no coordinator
credential and is killed after 45 minutes, inside the unrenewed one-hour lease.

The reviewer judges the in-launch gate (formatter, linter and tests) with
each nested-sandbox skip reported as skipped. Any other failure is a finding
unless the reviewer proves it pre-existing: it reruns the failing test(s) on
the base revision, for example in a temporary worktree, and cites both results.
A failure that also occurs on the base is a `pre-existing failure:` entry in
`findings` (test name, candidate result, base result); it does not by itself
block approval and is posted as advisory. One that passes on the base, or that
could not be run there, requests changes.

The launch must end with the structured verdict of the review schema:
`decision`, `summary`, `findings`, `criteria_evidence` and
`amendment_decision` (null unless the submission carries an `ac_amendment`).
Root reads Codex's `last.md` or the `structured_output` of Claude's final
`result` event. An approval needs non-empty evidence for every acceptance
criterion (the amended ones when it accepts the amendment, M5). A verdict
that fails this check, cannot be parsed, or that the service refuses is not
posted: the review is released and queued again. Otherwise root posts it with
`reviews decide`, findings as `required` (changes requested) or `advisory`
(approval), the evidence in the summary, and `review_independence =
distinct_launch` recorded on the decision. The `amendment_decision` is sent
only when the submission carries an `ac_amendment`; an approval must decide
it, and with requested changes an `accepted` amendment is left undecided (the
service accepts only `rejected` there). Before claiming, root checks that the
subject's current submission is the one `next` offered; if the claim reply
names no attempt, root releases the activity's current attempt rather than
hold the review for its lease. After `[run] review_attempts` (default 3)
failed verdicts in a row for one submission, the loop stops claiming it and
logs why, until it restarts. The clone, run and mirror branch are removed
after every review. No review is taken while the reviewer has reached `[health]
reviewer_daily_usd` (default 50) in the ledger below; reviewer launches do not
record their cost there yet, so today that cap counts no reviewer spend.

`--once` polls a single time. Recovery claims, continuation claims for work
longer than `max_attempt_seconds` and the other-vendor audit sample are not
part of this loop yet.
`host-setup.sh` installs the `agentc-run` systemd unit without enabling it.
It requires the firewall and egress units and restarts after a crash. To opt
in:

```sh
sudo systemctl enable --now agentc-run
```

On start, the loop removes terminal runs that an earlier loop left behind,
so do not keep evidence from hand-run implementer launches under
`impl/runs/` while the unit runs.

### Attention budget: digest and canary

`deploy/agentc/attention.py` reads the service as the supervisor's credential
(a file holding only the bearer token):

```sh
attention.py digest --url URL --project ID --token-file TOKEN \
    --mail-to you@example.org --smtp-host localhost
attention.py canary --url URL --project ID --token-file TOKEN \
    --heartbeat /var/lib/agentc/heartbeat.json --ntfy-topic TOPIC --max-hri 3
```

`host-setup.sh` installs the script as `/opt/agentc/bin/attention.py` and
schedules both from systemd timers (hosts without systemd get the script and
environment file only). The canary checks `/healthz`, the supervisor's own `next` call (10 s budget), that the
heartbeat is under 300 s old, and, with `--max-hri`, the HRI count. A failing
check sends one ntfy page and stays quiet until it recovers; a page that could
not be delivered exits 2 and is retried by the next run. `NTFY_TOKEN`, when set,
authenticates to ntfy. `deploy/agentc/attention-test.py` tests both against
local fake servers.

#### Timers and settings

| Unit | Runs | Default schedule |
| --- | --- | --- |
| `agentc-canary.timer` → `agentc-canary.service` | `attention.py canary` | 2 minutes after boot, then every 10 minutes (`OnUnitActiveSec`) |
| `agentc-digest.timer` → `agentc-digest.service` | `attention.py digest` | daily (`OnCalendar=daily`, `Persistent=true`, so a missed run happens at boot) |

Both services are oneshot units that run as root (the agent firewall filters
only the two agent uids, so they can reach ntfy, SMTP and the coordinator),
under `NoNewPrivileges`, `ProtectSystem=strict` and `PrivateTmp`, writing only
under `/var/lib/agentc` (the canary's paged-set file
`/var/lib/agentc/canary-state.json`). Change a schedule by re-running
host-setup with `CANARY_INTERVAL=5min` or `DIGEST_CALENDAR='*-*-* 07:30:00'`
(any `OnUnitActiveSec` or `OnCalendar` value).

Settings live in `/etc/agentc/attention.env`, which host-setup writes once and
never overwrites. Every option has a default in `attention.py`, so the file
shows those entries commented out; a command-line flag still wins over the
environment. The owner supplies only:

- `ATTENTION_NTFY_TOPIC`: the ntfy topic the canary pages (required for the
  canary timer). Add `NTFY_TOKEN` for a protected topic.
- `ATTENTION_SMTP_HOST` and `ATTENTION_MAIL_TO`: mail the digest; without them
  the digest only prints to the journal (`journalctl -u agentc-digest`).
- the coordinator token file, `/etc/agentc/attention-token`: the supervisor's
  bearer token alone, installed with
  `sudo install -o root -g agentc-impl -m 0440 /dev/stdin /etc/agentc/attention-token`.
  host-setup holds it at `root:agentc-impl` 0440 and refuses a token that is
  not root-owned.

Defaulted entries (commented in the file): `ATTENTION_URL`
(`https://agents.sithbit.com`), `ATTENTION_PROJECT`, `ATTENTION_TOKEN_FILE`,
`ATTENTION_HEARTBEAT` (`/var/lib/agentc/heartbeat.json`),
`ATTENTION_HEARTBEAT_MAX_AGE` (300 seconds), `ATTENTION_NTFY_URL`
(`https://ntfy.sh`), `ATTENTION_STATE`, `ATTENTION_MAX_HRI` (unset: no HRI
check), `ATTENTION_HOURS` (24), `ATTENTION_SMTP_PORT` (25) and
`ATTENTION_MAIL_FROM`.

host-setup enables a timer only once what it needs exists (the token file; for
the canary also a non-empty `ATTENTION_NTFY_TOPIC`), so an unconfigured host
does not fail every ten minutes. After filling in the file and the token,
re-run host-setup (or `sudo systemctl enable --now agentc-canary.timer
agentc-digest.timer`). `host-setup.sh --uninstall` stops the timers and removes
the four units, `attention.env`, `attention.py` and the canary state file; the
token file is kept and handed back to `root:root` 0400, like the push key.
`deploy/agentc/host-setup-test.py` checks the generated units and the
uninstall list without root.

#### Testing that paging works

Force a failing check and confirm the page arrives:

1. Point a one-off canary run at a heartbeat file that does not exist, with
   its own paged-set file, using the same environment file:

   ```sh
   sudo systemd-run --wait --pipe \
     -p EnvironmentFile=/etc/agentc/attention.env \
     -p Environment=ATTENTION_HEARTBEAT=/nonexistent \
     -p Environment=ATTENTION_STATE=/var/lib/agentc/canary-test.json \
     /usr/bin/python3 -I /opt/agentc/bin/attention.py canary
   ```

   It exits 1, prints `canary: supervisor: supervisor heartbeat unreadable
   (FileNotFoundError)` and sends one ntfy page titled "agentc canary failed".
2. Run it again: it stays quiet, because that failure was already paged. Delete
   `/var/lib/agentc/canary-test.json` when done.
3. With the timer's real settings, stop the loop (`sudo systemctl stop
   agentc-run`); within 5 minutes plus one timer period the heartbeat is stale,
   `systemctl start agentc-canary.service` exits 1 and pages. Restart
   `agentc-run` afterwards. A page that could not be delivered exits 2 and is
   retried by the next run.

Use a separate `ATTENTION_STATE` file for hand tests so they do not rearm or
mask the timer's own paged set.

### Running the loop against staging

`[run.binding]` in `/etc/agentc/supervisor.toml` replaces the mirror's
binding without editing the mirror or any clone:

```toml
[run]
allow_insecure_loopback = true

[run.binding]
service_url = "http://127.0.0.1:18080"
project_id = "<staging project id>"
# project_name = "<credential directory>"
```

All three sides use it: the loop's own `next`, its role commands (connect,
claim, renew, release), and the reviewer side. For an implementer launch the
loop claimed, `launch` also exports `AGENT_COORDINATOR_REPO_CONFIG` (the
root-owned copy) to the harness, so the agent's own CLI calls go to the same
coordinator rather than the clone's `.agent-coordinator.toml`, and
`AGENT_COORDINATOR_ALLOW_INSECURE_LOOPBACK=true` only for a permitted
loopback `http` origin. A Claude launch reaches a loopback coordinator
through the launch relay (endpoint `coordinator`); Codex reaches it directly
through the firewall's staging port. With `project_name`, the loop copies the
implementer's credential into `<project_name>/config/` of the run's
coordinator state and the reviewer principal's into
`verdict/home/<project_name>/config/`; the source files stay where they are.
At startup the loop removes any other `verdict/home/<name>/` holding a copy
of the reviewer principal's credential, so a changed or removed
`project_name` leaves no stale copy behind; it never follows a symlink there.

The loop keeps cloning from `/var/lib/agentc/mirror.git`, and the push helper
keeps publishing candidates to its GitHub repository. Because the CLI reads a
published candidate back from the project's `repository_url`, which must
also equal the clone's origin, the staging project must use the mirror's
origin URL as its repository; `staging.py project` creates such a project.
The bootstrap project's local bare remote is not reachable from launches.
Staging candidates therefore land in the GitHub repository under
`refs/agent-coordinator/candidates/<staging task id>/<session>`, never on a
branch; nothing integrates them, since the integrator works only with the
production coordinator.

The owner's steps for one claim-to-submission run on mxmini (root steps
need `sudo`; stop at the first failure):

1. Install binaries built from a `main` that includes `[run.binding]` (steps
   1-2 of [bringing up a host](#bringing-up-a-supervised-host)), and keep the
   loop's unit stopped: `sudo systemctl stop agentc-run`.
2. Start staging from the same build:
   `deploy/agentc/staging.py up`.
3. Install the staging credentials with the commands
   `deploy/agentc/staging.py credentials` prints. They replace each role's
   `credentials.toml`; to keep a production entry, merge the two
   `[[credentials]]` entries into one file (entries are keyed by origin).
4. Create the pilot project with the mirror's origin as its repository:

   ```sh
   ORIGIN=$(sudo git -C /var/lib/agentc/mirror.git config --get remote.origin.url)
   deploy/agentc/staging.py project --repository-url "$ORIGIN"
   ```

   It also gives the project the placeholder required-check roster
   `staging-diff-check:v1:any` (a code submission needs a workflow policy;
   `--required-check IDENTITY:VERSION:ENVIRONMENT`, repeatable, replaces
   it) and prints an `AGENTC_STAGING_BINDING=...` line and the
   `[run.binding]` table. Add that table, with `allow_insecure_loopback = true` under
   `[run]`, below the `KEEP` line of `/etc/agentc/supervisor.toml`. Leave
   `[run] reviewer` off for this run.
5. Create one trivial task in it as the staging owner (use the printed
   binding path):

   ```sh
   export AGENTC_STAGING_BINDING=<printed path>
   deploy/agentc/staging.py cli owner --json tasks create --input - <<'EOF'
   {"title": "Staging pilot: add staging-pilot.txt",
    "description": "Create staging-pilot.txt at the repository root holding the single line `staging pilot`. Change nothing else. This staging pilot task needs only `git diff --check` as its gate. Push the candidate and submit.",
    "acceptance_criteria": ["staging-pilot.txt holds exactly `staging pilot`", "No other file changes"]}
   EOF
   ```

   Note the task id it prints.
6. Run one poll as root and keep its output:

   ```sh
   sudo /opt/agentc/bin/agentc-supervisor run --once 2>&1 | tee ~/staging-pilot-run.log
   ```

   It claims the task, launches, renews while the harness works, releases
   the attempt if the agent did not submit, and cleans up; it returns when
   the launch ends. It must report `Launched { task: "<task id>", exit_code: 0 }`.
7. Record the evidence in the task's "Knowledge & evidence", outside Git:
   - claim and submission: `deploy/agentc/staging.py cli owner --json
     request --method get --path
     /api/v1/projects/<project id>/tasks/<task id>/workflow` shows the
     attempt and a submission with `candidate_ref` and
     `candidate_revision`;
   - launch: the run log above, `sudo cat /var/lib/agentc/heartbeat.json`, and
     the task's line in `sudo tail -n 3 /var/lib/agentc/costs.jsonl`;
   - push candidate: `git ls-remote "$ORIGIN"
     'refs/agent-coordinator/candidates/<task id>/*'` names the
     `candidate_revision`.
8. Restore production: remove `[run.binding]` and `allow_insecure_loopback`
   from `/etc/agentc/supervisor.toml` and put back the production
   `credentials.toml` files if step 3 replaced them. The next loop start
   reinstalls the mirror's binding.

To check that the unit starts after a reboot without claiming anything,
set the kill switch first, then reboot:

```sh
sudo touch /var/lib/agentc/kill-switch
sudo systemctl enable agentc-run
sudo reboot
# after the reboot:
systemctl is-active agentc-run
sudo journalctl -b -u agentc-run --no-pager | head -n 20
sudo cat /var/lib/agentc/heartbeat.json
```

`active`, and a heartbeat written after the boot with a `Refused` outcome
naming the kill switch, is the evidence. Remove the kill switch (and disable
the unit) as the pilot plan requires afterwards.
