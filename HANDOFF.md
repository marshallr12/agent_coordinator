# Core hardening wave — 2026-10-02

## Follow-up session — 2026-10-02 (daytime)

The user again disabled the Agent Coordinator workflow and asked for the next
backlog items. Local commits only; nothing pushed, deployed or changed on hosts.

- `09a2b78` TypeSafe main-service-only secret file (wave-plan A-P4): the main
  unit adds `EnvironmentFile=-/etc/agent-coordinator/typesafe.env` after
  `service.env`; backup/maintenance units unchanged; install and backup guides,
  `service.env.example` and the install smoke updated. Independently CONFIRMED.
  The out-of-repo `release-next/deploy-pre.sh` now aborts before stopping
  anything unless `typesafe.env` holds a nonempty key and warns if `service.env`
  still has a key line (previous copy `deploy-pre.sh.pre-typesafe-env`). **The
  next deploy therefore needs the user's VM step first:** install
  `typesafe.env` (root:root 0600) with the key, install the new unit,
  daemon-reload, remove the key line from `service.env`, restart.
- `8b24b8e` C1 routing resolved as a new phase: literal URL snapshots, rewrite
  refusal, plus refusal of a configured remote named like a snapshot (the old
  blocker), ASCII-only host match and exact `.git` suffix. Two repairs, after
  which the final verifier found no routing bypass. Tests in
  `scripts/ship_routing_test.py` (real local Git, mocked transport).
- `8c2cdc6` exact-ref readback: `ls-remote` suffix matching let
  `refs/heads/a/refs/heads/main` be read back as main. Pre-existing on main;
  the user chose to fix it as a separate item. Independently CONFIRMED.
- `417dbca` R-P3b.5(d), decision U20: read-only credentials keep their own
  top-level session but any registration naming a subagent is refused (403)
  before writes. Full gate on it: fmt, warnings-denied Clippy, 546 tests,
  zero ignored (`r5d-*.log`). Independently CONFIRMED. Untested: replay
  ordering (no test pins the check above the replay branch).
- `709d277` R-P3b.3, decision U18: Claude reviewer Bash commands run through
  `CLAUDE_CODE_SHELL_PREFIX` = generated `$RUN/candidate-shell` in a nested
  bwrap (state dir tmpfs; clone ro; tmp/target/Cargo rw; private
  `$RUN/candidate-home` over `$HOME`; Claude config ro with login masked; env
  credentials unset; own pid/ipc/uts/user ns; no nested userns; ro /proc).
  Reviewer outer keeps userns + rw /proc for nesting; Codex reviewers refused.
  Two repairs (unpinned inner flags; shared `$HOME` let planted
  `.gitconfig`/`.profile` reach Claude Code's out-of-prefix git/login shell).
  Final review INCONCLUSIVE; user chose commit and track. Full gate: fmt,
  Clippy, 550 tests, docs (`r3-*.log`). One unrelated flake seen once:
  `namespace_teardown_kills_detached_descendants_on_exit_and_wrapper_kill`.
  **Open, blocks first live launch (own repair budget):** candidate can move
  the harness's tracked cwd into shared tmp/target/Cargo dirs; an out-of-prefix
  `git status` there could run a candidate filter with the reviewer login
  (trigger unconfirmed; site gated by CLAUDE_CODE_AUTO_MODE_GIT_STATUS). Fix:
  candidate-shell resets the cwd file to the clone, plus an instrumented owner
  run (git shim logging cwd). Also unverified on a real host: prefix applies to
  background Bash, fd inheritance into the prefix, shell snapshots, UI checks
  (need U18's per-run short-lived staging login, not built).
- Gate: ship tests (18), routing tests (36), pinned docs check and py_compile
  passed; no Rust/Cargo change since the full gate on `0beabe8`. Logs:
  `~/.local/share/agent-coordinator-autonomy/core-20261002/followup-*.log`.
- One verifier accidentally ran an https `git fetch` to github.com; it stopped
  at the credential prompt. Nothing was pushed.
- Known false refusals by design: https URLs with userinfo (token insteadOf)
  and ssh→https insteadOf with pushInsteadOf back.

- `6f19eb4` R-P3b.3 cwd residual: candidate-shell refuses (126, before
  running) a command not ending in `pwd -P >| <plain file in $RUN/tmp>`;
  after the nested sandbox exits it writes the clone path to that file and
  returns the command's status. Candidates get `$RUN/candidate-tmp` as
  TMPDIR; the harness's `$RUN/tmp` is a throwaway tmpfs inside, so no
  candidate (incl. background) can rewrite the file. `cd` no longer persists.
  Full gate: fmt, Clippy, 554 tests, docs (`r3cwd-*.log`); two mutations red.
  Independent red-team CONFIRMED (real-bwrap probe with delayed and setsid
  writers, dash/bash parsing table, four mutations). Owner-run checks: the
  pinned CLI's cwd file is directly in TMPDIR with that suffix, background
  and snapshot invocations carry it, and no harness file under `$RUN/tmp` is
  handed to Bash. Hardening note: the final write follows a symlink, but only
  the harness can create entries in `$RUN/tmp`.
- Decision U21 (refines U19): R-P3b.4 uses a supervisor port bridge
  (`--unshare-net`, relay only proxy 3128 and staging 18080 over per-launch
  Unix sockets); pasta/passt is not installed on mxmini.

- `52191ce` R-P3b.4 per U21: Claude launches run with `--unshare-net`; the
  supervisor relays only the proxy and a verifying reviewer's loopback
  staging port (host half: Unix sockets in `$RUN/net` -> that TCP address,
  under the firewall; namespace half: hidden `netns-relay` listens on the
  same ports and runs the harness). Ports must be IPv4 loopback >= 1024;
  unfaithful loopback staging URLs are refused. Preflight requires a
  root-owned supervisor binary and runs its relay once. Supervisor is now
  lib + bin; `tests/launch_relay.rs` drives a real wrapped reviewer launch
  through both halves. Full gate: fmt, Clippy, 566 tests, docs
  (`r4b-*.log`). Red-team round 1 DISPUTED (host half untested, silent
  staging forms, ports < 1024 unbindable); one repair, round 2 CONFIRMED.
  Residuals: mutants "launch::run skips start_host" and "check skips
  probe_relay" survive (paths need root-owned binaries) -> add a root
  containment-suite leg running a real `launch` whose mock harness reaches
  the proxy via the relay (`check_claude_sandbox` currently cuts args at the
  first `--`, so it never runs the relay). Codex launches keep the host
  namespace and the firewall ephemeral range. Silent fail-closed staging
  forms remain (percent-encoded hosts, `0.0.0.0`, `0x`). Owner-run: host
  relay under the real nft rule; Claude Code and Chromium behind the relay.
  Pre-existing flake `namespace_teardown_kills_detached_descendants...` hit
  once under full parallel load (0/15 both here and at HEAD afterwards).

- `f1edadd` containment-suite relay leg: per role, a real
  `agentc-supervisor --config <tmp> launch` through a temporary root-owned
  `/opt/agentc/suite-bin.*` (copied supervisor + mock `claude`; config = the
  installed one with `bin_dir` prepended) must reach the proxy via the relay,
  fail direct egress, and (reviewer) reach the proxy from a candidate
  command; a stub bin dir (`/bin/false` supervisor) must make preflight
  report `namespace relay probe failed`. Red-team round 1 DISPUTED (pipefail
  made the stub check always fail); fixed by capturing preflight output,
  plus cleanup/error-path fixes; helpers exercised locally with stubs. Docs
  check passed; no Rust change. **Not run:** needs root and the new
  supervisor installed on mxmini (installed one predates the relay), so the
  first `sudo deploy/agentc/containment-suite.sh` after install is the owner
  step that proves it. Staging relay is not in this leg (no login fixture);
  `tests/launch_relay.rs` covers it.

- `e47c46f` U22 per-run staging session (refines U18): new
  `staging_login.rs`. After `state.started`, a verifying Claude reviewer
  launch signs in to staging from the supervisor (login file never enters a
  sandbox), writes only `{url, cookie}` to `$RUN/verification-session.json`
  (create_new 0600; ro-bind into the candidate sandbox), signs out after the
  harness (also on spawn/wait failure, and if writing the file fails).
  Sign-in failure fails the launch. Loopback per `relay::staging_address`
  (direct, `.resolve` to the relay address); otherwise https via the egress
  proxy. Reviewer harness gets a tmpfs over `rev/verification`.
  `verification.json` names `session_file` (not `credential_file`);
  `verify_ui.mjs` sets the cookie via CDP (form login kept for hand-written
  descriptions). Gate: fmt, Clippy, 573 tests, 0 ignored, docs (`u22r-*.log`).
  Two mutations red (candidate bind, harness hide). Local e2e against a real
  staging server + Chrome: `/me` 200 with the cookie, `verify_ui` OK, 401
  after sign-out (temporary test, not committed). Red-team round 1 DISPUTED
  (`.localhost` mismatch with relay, harness could read logins, http
  off-host, long functions); repaired; round 2 CONFIRMED. Residuals: no
  sign-out on panic/SIGKILL (12h session bound); a cookie holder has operator
  power on staging during the run; staging login rate limit 5/min/user and
  10-session cap may bite bursts of reviewer launches; https `__Host-` cookie
  via CDP untested; trailing-dot Origin vs the server's origin check
  untested.

- `598c77a` R-P3b.2 phase 1 (U17/U23/U24): `coordinator_local::candidate_push`,
  both sides of the per-launch helper protocol (JSON request line + u64
  length-prefixed bundle; one JSON reply line with stable refusal codes).
  The helper checks the pack with `index-pack --strict` (git 2.39.5 applies
  neither `fetch.fsckObjects` nor `transfer.fsckObjects` to bundle fetches),
  imports by OID only, re-scans, and pushes only
  `refs/agent-coordinator/candidates/<task>/<launch>` under an intent + lease
  guard (foreign refs never adopted; intent cleared on definite failure).
  The outgoing secret scan now also reads raw commit objects (messages,
  idents, signatures, any `encoding` header) — **behaviour change on
  `checkpoint_candidate` too**, accepted by the main loop. Gate: fmt, Clippy,
  598 tests, docs (`p2c-*.log`). Red team: DISPUTED twice (foreign-ref
  adoption, reply size, unscanned messages; then encoding-header bypass,
  stale intent), round 3 CONFIRMED. Residuals: helper repo disk growth
  (pack stored twice, refused packs kept); first push fetches full
  prerequisite history; EBCDIC-style re-encoding is obfuscation, not caught.

- `bb5294b` R-P3b.2 phase 3: with `AGENT_COORDINATOR_CANDIDATE_PUSH_SOCKET`
  (absolute path; blank = unset) a code submission refuses `--candidate-ref`,
  checks the base commit, pre-scans with the caller's credential digest
  (the helper cannot know it), sends, then reads the helper's ref back
  (exact commit + tree). Codes: refused (per RefusalCode), connection
  failure incl. EOF-before-reply → 7 retryable (helper retry is
  idempotent), pre-send local error → 2, protocol violation → 5. Red team
  DISPUTED twice (classification, Windows dead code, escapes, env parsing;
  then EOF-before-reply), round 3 CONFIRMED. Residual: the CLI records any
  prefixed ref already holding the exact commit (no launch identity
  client-side; bind server-side if needed); no socket timeout on the client.
- `2132b4c` R-P3b.2 phase 2: `agentc-push serve [--config /etc/agentc/push.toml]
  --socket --task --launch --work-dir [--known-digest]... [--parent-pid]`
  (integrator crate, now lib + 2 bins; example `crates/integrator/push.example.toml`;
  `app_id`, `installation_id`, `repository` required). Mints only after a
  valid preamble, budget burst 3 / 1 per 20 s, token scoped to the repo +
  contents:write, askpass via self with `GIT_CONFIG_PARAMETERS`/global/system
  neutralised and env cleared to PATH/HOME/LANG/LC_ALL, revoked always;
  refuses root; PDEATHSIG (follows the spawning *thread*); socket 0660 via a
  0700 staging dir + hard link. Red team DISPUTED once (mint per empty
  connection, inherited Git env, chmod window), round 2 CONFIRMED.
  **Phase 4 must:** spawn from a launch-lived thread with `env_clear()`,
  pass `--parent-pid`, keep socket paths < 108 bytes, remove a stale socket
  after SIGKILL, and settle one-submission-per-launch (the helper moves its
  single ref on resubmission). Full gate on the combined tree: fmt, Clippy,
  641 tests, 0 ignored, docs (`p2f-*.log`); one ETXTBSY flake in
  `candidate::shell_tests` once, 3/3 reruns green.

- `f55cdbc` R-P3b.2 phase 4a, U26 (narrows U24): the helper creates its ref
  only when absent and never moves it; same-commit retry (incl. intent
  recovery) accepted; any other commit → `candidate_already_published`
  (early, before the bundle, once a lease exists); CLI exit 5. Red team
  CONFIRMED first round. Lows: a crash after the push but before the lease, plus a foreign
  deletion, lets a second commit through (X was never acknowledged); corrupt
  lease reads as absent (fails closed as bundle_invalid); the refusal does not
  name the published commit. A refused second commit still costs one mint.
- `d8a9b80` R-P3b.2 phase 4b, U25: `agentc-supervisor launch-root` (root).
  Per-launch dir `<state_dir>/push/<session>/` (push root 0711 root-only;
  `sock/` agentc-push:<impl primary gid> 2750; `work/` agentc-push 0700) —
  not under `$RUN` because `runs/` is 0700 agentc-impl. Helper as agentc-push
  via setgroups/setgid/setuid in pre_exec (verifies setuid(0) fails), env
  PATH/LANG/HOME=work, `--parent-pid`, digests of implementer-owned
  `credentials.toml` tokens; ≤10 s socket wait; launch child as agentc-impl
  with `--push-socket --task --session-id`; SIGTERM→5 s→SIGKILL; dir removed;
  teardown failures warn and keep the launch's exit code. Sweep of stale
  dirs (incl. abandoned `.new-*`) under `push/.sweep` flock. Claude sandbox
  now masks `<state_dir>/push` and `<state_dir>/<role>/runs` (tmpfs) and binds
  back only its own socket dir and `$RUN`. Ref = `.../candidates/<task>/<session-id>`.
  Red team: DISPUTED (cross-launch socket reach via leaked session ids;
  sweep race), then N1 lexical socket path + first-run `push/` race; all
  repaired; full gate fmt, Clippy, 669 tests, 0 ignored, docs (`p4bf-*.log`).
  **Known gap (documented):** Codex implementers run in the host namespace as
  agentc-impl and can reach concurrent launches' sockets — run Codex
  implementers one at a time. Unverified without root: the real uid switch,
  setgid inheritance onto the socket, agentc-rev refused, `host_problems`
  inside `launch_root`. A SIGKILLed launch-root leaves the launch child running
  without its helper (next run sweeps the dir).

- (phase 5, this commit's parent) host-setup/containment-suite/docs:
  `agentc-push` account strictly checked (uid≠0, not shared with
  impl/rev/egress, nologin/false, only its own group); binary at
  `/opt/agentc/bin/agentc-push` (new default; `/usr/local/bin` is root:staff
  2775 on Debian and fails `protected_executable`); `/var/lib/agentc/push`
  root 0711; `/etc/agentc/push.toml` root:agentc-push 0640 written only when
  absent (App 5168037 / installation 167333814); key chowned to agentc-push
  0400 if present (symlink/hardlink refused). `--uninstall` now removes only
  host-setup's own paths (the integrator survives — previously it
  `rm -rf`'d `/opt/agentc`, `/var/lib/agentc`, `/etc/agentc`). Suite push leg
  uses a non-minting sentinel config. Red team CONFIRMED + L1-L4 hardening.
  **Nothing here has run as root.** `agentc-push` has no egress firewall
  (direct to GitHub).

R-P3b.2 is code-complete. **User steps (root):** on oracle-1 from this branch:
`cargo build --release --locked -p agentc-supervisor -p agentc-integrator -p coordinator-cli`,
`sudo SUPERVISOR=target/release/agentc-supervisor CLI=target/release/agent-coordinator deploy/agentc/host-setup.sh`,
`sudo deploy/agentc/containment-suite.sh`; same on mxmini (key checks SKIP;
also owes the f1edadd relay leg). Remaining backlog: owner Claude
host/auth/refresh/browser proof (incl. the `pwd -P` suffix), S6 cutover chain.

- `7b8d3a1` R-P3b.5(b)(c) (2026-10-02 late; coordinator workflow still off,
  local commit only). (b) `no_new_privs` on every harness, preflight
  `--version` and sandbox probe; `reaper::reaped` makes `launch` and
  `launch-root` child subreapers that SIGKILL every descendant (pidfd,
  parent re-checked, whole tree per round) after the harness and before
  `.state-terminal.json`, and again if the launch errors. (c) `--uninstall`
  retires impl/rev/push accounts first (linger off, `pkill -u`/`-U` with a
  final re-check, crontab, `at` jobs, own files in /tmp, /var/tmp, /dev/shm,
  userdel), then stops `agentc-egress` (Restart=always) before retiring its
  account, then drops the firewall. Gate: fmt, Clippy, 674 tests, docs,
  `bash -n`; mutation controls m1-m5 red. Red team: two DISPUTED rounds, all
  findings fixed (last one, the egress restart race, fixed after round 2 and
  verified with shell stubs only). Residual LOW: the `reaped` call sites in
  `main.rs`/`push_helper.rs` are untested wiring. **Unverified without root:**
  the whole uninstall path, cross-uid kills from `launch-root`. Flake seen
  once: `coordinator-cli` `launcher_lock_excludes_another_launcher_but_allows_native_journal_lock`
  (passed 6 reruns; untouched crate).

The user disabled the Agent Coordinator workflow and authorized overnight local
implementation with subagents. Cutoff: 2026-10-02 08:00 America/New_York
(12:00 UTC). No production changes, credential issuance, rulesets, ownership
switch, host root changes or Git pushes are part of this wave.

Integration tree: `~/src/worktrees/agent-coordinator-core`, branch
`hardening/core-20261002`, based on freshly fetched `origin/main` `1e8aebb`.
Each builder has its own `agent-coordinator-core-{typesafe,confinement,ship}`
worktree, branch and Cargo target directory. The primary owns integration,
checkpoint commits, gates and this handoff. Preserve the planning checkout
`~/src/agent_coordinator` and existing `autonomy/s6` worktree.

User decisions:

- Isolated writable Cargo caches per launch. Under disk pressure, prune expired
  terminal launch state first; any more efficient fallback must preserve write
  isolation. Shared writable download caches are not authorized.
- After two evidence-led repair attempts on a disputed phase, record its blocker
  and continue independent lanes.
- At most three concurrent subagents. Builders: TypeSafe `gpt-6.1-sol/high`,
  Claude confinement `gpt-6-astra/xhigh`, ship.py `gpt-6.1-sol/medium`. Fresh
  verifiers are at least as capable; security verification uses `gpt-6-astra/xhigh`.
  The deadline repair reused the idle Astra/xhigh builder; A3 uses Sol/high.

Current phase state:

- A1 concurrency cap (default 4) independently verified after one bounds repair;
  `249ea62` here (`d8155bb` in its lane). A2 circuit breaker independently
  verified: three consecutive completed failures open it for 60 seconds, followed
  by one half-open probe; stale completions cannot alter newer generations.
  `60af14a` here (`f20a47d` in its lane). A3 application-context fallback/order,
  deterministic recovery and redacted-log tests independently verified and
  integrated as `0beabe8` (`7649f6a` in its lane): 30 library tests, seven
  integration tests, warnings-denied scoped Clippy, format and pinned docs passed.
- B1 per-launch state and terminal-only retention independently verified:
  `1f8e96a` here (`487e2c5` in its lane). B2 protected seeds and validation before
  writes independently verified: `1f9063a` here (`e44a701` in its lane).
  B3 Claude-only OS write confinement independently verified after one repair:
  `1383e0c` here (`69a3c75` in its lane). It uses bubblewrap, narrow writable
  mounts, protected seeds, descriptor fencing, a bounded sealed prompt snapshot
  and descendant cleanup. Codex keeps its existing native sandbox/auth behavior.
  Supervisor focused gate: 55 tests, warnings-denied Clippy, format, shell syntax
  and pinned docs passed on Rust 1.98.1. Actual owner-run host containment,
  authenticated Claude credential refresh and nested browser compatibility remain
  unverified. Root-owned Claude config parents prohibit temp-file/rename refresh;
  owner bootstrap must import only the credential file from a separate login dir.
  Do not treat local mock proofs as operational readiness.
- C1 fetched-tip ancestry and exact GitHub repository routing is **BLOCKED**
  after two repair attempts. Do not integrate its uncommitted changes from
  `agent-coordinator-core-ship`. Its 26 tests pass, but an independent real Git
  resolution probe found that a remote whose name is a saved URL can redirect
  fetch/push/readback to an unchecked repository without any `url.*` rewrite.
  Example: `remote.publish.url=https://github.com/Checked/Repo.git` and
  `remote.https://github.com/Checked/Repo.git.url=https://github.com/Unchecked/Repo.git`.
  `validated_remote("publish")` approves Checked while literal URL operations
  resolve to Unchecked. No actual transport was used in the probe. Earlier
  repairs fixed GH_HOST, mutable aliases and chained URL rewrites; respect the
  user retry limit and retain this disputed work for a future phase.
- C2 independently verified after one repair and integrated as `70c7c55`
  (`06736bb` in its clean lane). Default overall Actions deadline is 1800 seconds,
  appearance window remains 300 seconds, individual queries are capped at 300
  seconds and bounded by remaining deadlines. Positive finite huge overrides do
  not overflow subprocess polling; timeouts name the full SHA and stop final
  push. Eighteen unit tests and four independent full-flow checks passed.
  Tree `agent-coordinator-core-ship-deadline`, branch
  `hardening/ship-deadline-20261002`, contains no disputed C1 changes.

Final integration gate on product commit `0beabe8` passed on stable Rust 1.99.0:
format, warnings-denied workspace/all-target Clippy, all 545 workspace tests
(33 suites, zero ignored), locked workspace build, disposable loopback service/CLI
smoke, backup/restore smoke, all 18 shipping deadline tests, pinned mdBook 0.5.4
source/package/local-link checks, and dependency audit (372 dependencies).
The focused phase gates also passed on Rust 1.98.1. No dependency version changed;
Cargo.lock adds the already-resolved libc dependency to the supervisor only.
The gate cleared ambient coordinator configuration and the TypeSafe key; no paid
provider call or production service mutation occurred. Baseline had 502 tests.

Evidence is outside Git under
`~/.local/share/agent-coordinator-autonomy/core-20261002/`: see
`integrated-gate-results.json` and `integrated-{fmt,clippy,tests,build,smoke,backup-smoke,ship-tests,docs,audit}.log`.
The accepted lane worktrees are clean; the disputed routing tree is intentionally
uncommitted. The original S6 tree was preserved. No branches were pushed and no
production deployment or root host change was attempted.

Next session: start with this handoff and `git status --short` in this integration
worktree. Review `git log --oneline origin/main..HEAD` and the source diff before
requesting authorization to ship. Do not ship through the disputed routing phase.
Future work needs a new routing phase, TypeSafe main-service-only secret-file
preparation, owner Claude host/authentication/refresh/browser proof, and the
remaining supervisor security decisions. The two-repair cutoff applied to C1;
do not silently resume a third repair in the completed core wave.

S6 cutover and the later supervisor security queue remain separate work. The
planning handoff records the 24-hour polling pass but no live candidate evidence.

## Historical S6 implementation checkpoint — 2026-09-29

The user disabled the Agent Coordinator workflow for this session. No service
work was created, claimed or updated. Continue in branch `autonomy/s6`, worktree
`~/src/worktrees/agent-coordinator-s6`, based on `main` `4a72018`; keep Cargo
output in this worktree's `target`. The autonomy planning handoff/design remain
in `~/src/agent_coordinator` on `autonomy-plan` (`fbce216` at session start).

S6 local implementation adds `agentc-integrator shadow [--once]`, a pre-cutover
queue view (`?shadow=true`, integrator credential only, no live heartbeat),
root-owner host bootstrap, a read-only drain preflight, required-check sample
analysis and the canonical [cutover runbook](docs/integrator-cutover.md).
Shadow uses separate local state, computes R and reads checks; no service
publication writes, Git pushes or job reruns occur. `WouldPush` is provisional
(`authority_verified=false`). Revert computation is still live-only. Missing
rules are logged before cutover; missing roster mappings fail the shadow pass.

Production remains unchanged. S6 is **not complete**. Required next steps:

- App repository access completed and browser-verified 2026-09-29: App `5127380`,
  installation `166293403`, now selects only `marshallr12/agent_coordinator`;
  the test repo is removed. GitHub displayed the saved-update confirmation.
  U4 App-only push protection passed in the previous session. The App key stays on oracle-1 at `/etc/agentc/integrator-app.pem`,
  root-owned `0400`; never copy it to the workstation. Connect via the user's
  `oracle` Bash alias; `ssh oracle-1` does not resolve locally.
- Review/ship/deploy S6 service and build the host-native integrator at the
  reviewed revision. Owner runs `deploy/agentc/integrator-host-setup.sh` with
  sudo, sets project/config and installs a read-access integrator credential.
- Land a target roster mapping the exact service identities to the three GitHub
  job names/paths in the runbook; the current target has no roster file.
- Run/retain a continuous 24-hour production shadow and a reviewed live candidate
  test. Collect about 20 all-job attempts per required check on pinned main;
  each observed non-success rate must be <2%. Then zero drain preflight, owner
  rulesets, credential replacement, ownership switch and canary. Do not infer
  any of these from the local regression tests. P3b follows completed S6.

Local verification: 502 workspace tests passed, warnings-denied workspace
Clippy passed, pinned documentation source/package/local-link checks passed,
locked dependency audit passed, four cutover-ops regressions passed, shell
syntax and generated systemd unit validation passed. The rendered runbook was
inspected in Brave. The full workspace build, service/CLI smoke and backup/restore smoke passed
on clean committed candidate `14e2fda`; the final focused shadow-queue test also
passed after adding the missing-project check. This record update changes
only documentation. Product code remains locally committed, not pushed or deployed.
Local gate logs use `/tmp/agentc-s6-*.log`; cutover credentials/evidence stay
outside Git. No remote push or production deployment has occurred.

---

# Implementation handoff — 2026-09-14

## MCP-first bootstrap

Startup now prefers a configured authenticated MCP connection and uses the native
CLI as fallback. Public discovery schema 2 separates coordination from local
workstation capabilities and gives explicit missing-client and ownership-safe
transition procedures. MCP's own instructions no longer require a native launcher.
The native `session adopt-mcp` command validates and adopts an existing quiescent
MCP session using protected environment values; it never claims or renews work.
CLAUDE.md remains a minimal bootstrap. Deployment/test evidence belongs to the
live service task, not a local task queue.

## Service discovery migration

The public info endpoint now embeds the canonical `book/src/docs/agent-startup.md`
guide as `data.agent_startup` (schema version 1). It describes portable credential
configuration and native CLI workflows. Private task/project contents remain
behind authentication. CLAUDE.md is a seven-line discovery bootstrap; AGENTS.md
contains only its pointer. Engineering requirements moved to CONTRIBUTING.md.
BACKLOG.md and its book wrapper were removed; historical milestones are in the
implementation-history chapter and pending acceptance criteria remain in the live
service. Agents need no local backlog or handoff to select and claim work.

Validation and deployment evidence will be recorded with the live migration task.

## Setup usability follow-up

The two setup usability items and the Copy token investigation are now Code
tasks in the live Agent Coordinator project. Read their current state from the
service; this document is historical context, not the work queue.
The repository binding names project
`fe95a6c5-2aad-463f-8446-4366d9a281c7` at `https://agents.sithbit.com`.
The verified Windows CLI from release run 34891009689 is installed under
`%LOCALAPPDATA%/AgentCoordinator/bin`; `scripts/coordinator.ps1` supplies the
binding and requires an explicit, stable harness session name. The operator
saved the `codex-miniair` token in protected Windows configuration after manually
copying it. Native CLI authentication, complete orientation, reading all three
tasks, and reading the required-check roster passed on 2026-09-14 without using
admin browser authentication. No task was claimed during setup verification.
CLAUDE.md and AGENTS.md now direct future working sessions to the live service,
automatically selecting and claiming eligible work according to live priorities.
The Copy token investigation was created through the external browser as
`4b68f1c7-7ed2-42cb-b8cc-3ce0a1dfaefb`; its evidence distinguishes the user's
manual failure from possible automation clipboard isolation.

Initial operator setup exposed unclear project policy and required-check fields.
The live task queue tracks accessible explanatory tooltips, examples, and guidance
on matching producer registrations. A second item removes redundant repository
identity entry by deriving it from the saved URL while preserving existing
identities and shared integration holds. These are pending service task records;
the deployed UI is unchanged. Implement and verify the live acceptance criteria
before claiming tooltip support.

## Google Cloud deployment

The application from deployment commit `019ffb6` is
installed on the `agent-coordinator` e2-micro VM in `sithbit-19b44`,
`us-east1-b` (South Carolina). It has a 30-GB standard persistent boot disk,
deletion protection, 1 GiB swap, and reserved IPv6 `2600:1900:4020:671::`.
The temporary installation IPv4 address was removed. The earlier empty
central-region VM, disk, address, subnet, and backup bucket were removed.
No unrelated project resources were changed.

The service, Caddy, hourly local backups, daily maintenance, and hourly
download-verified Cloud Storage backup transfer are running. The origin is
live at `https://agents.sithbit.com`. Cloudflare proxies its AAAA record and
applies Full (strict) TLS through a hostname-specific configuration rule.
Browser Integrity Check is disabled for this API hostname after it blocked
Python clients with error 1010. The administrator is `admin`;
its initial password is in the operator's protected local deployment directory,
outside this repository, and was never printed. No repository is enrolled or
bound to this service yet.

Installed package identity from [release run 34891009689](https://github.com/marshallr12/agent_coordinator/actions/runs/34891009689):

- Archive SHA-256: `199c254e0c52b99eaf932b7a41a7c68c37f777688c5db33c3e921f75cdce6699`.
- Server SHA-256: `06f10eb72d4a0ac173ed795786048552b31e5c1aee294078afa37714b238e720`.
- CLI SHA-256: `4ae4d9e3e0e51c2035f4de8ac6676412b45f97064f19edd668645dd66f45b716`.

The Linux package build/systemd/HTTPS and native Windows release jobs passed,
as did local Linux package layout/checksum/link checks. Formatting, warnings-
denied Clippy, workspace tests/build, both smoke exercises, native Windows tests,
and dependency audit passed in [coordination run 34891009230](https://github.com/marshallr12/agent_coordinator/actions/runs/34891009230).
The initial deployment check found the newly published RUSTSEC-2026-0285 in
rustls 0.23.44; deployment commit `019ffb6` updates it to 0.23.45. An earlier
release Windows timing-test failure was not reproduced by the patched runs.
The server binary is byte-identical before and after the client TLS update.
That exact server passed the 30-minute release-size load/restore job in
[run 34888396240](https://github.com/marshallr12/agent_coordinator/actions/runs/34888396240).
The second run's redundant load job was cancelled after its Linux and Windows
package jobs passed; the second run is not claimed as an aggregate green run.
This CI workload is not an e2-micro capacity measurement.

Live checks confirmed the rendered browser sign-in page, public HTTPS from
Windows and the VM, administrator login/logout, Secure cookies, CSRF rejection,
unauthenticated project denial, synchronized time, restart health, and IPv6
access to package mirrors and Cloud Storage after public IPv4 removal.
A complete backup was uploaded to the private
`sithbit-19b44-agent-coordinator-backups-east` bucket, downloaded, and verified.
An independent download on the Ubuntu 24.04 WSL workstation matched SHA-256
`552b597b95fd0e7360760235f71073c4f3bf030e3c43fa5ab77b2da0a52e8650` and restored
snapshot `94f708d6-83f6-4f37-a1c3-ca2d8edbb46f` into an absent directory in
0.425 seconds, ending at `restore_reconciliation`. This is an initial,
artifact-free database rehearsal, not a production-size recovery benchmark.

See [Google Cloud installation](docs/deploy-gcp-e2-micro.md) for
resources, backup-transfer behavior, and cost limits. Free-tier eligibility
depends on total billing-account usage; no zero-cost guarantee is claimed.

## Previous implementation acceptance

Backlog items 6.1 (MCP), 6.2 (mdBook), and 7 (native Windows workstation
acceptance) are implemented, exercised, and reviewed. The numbered release
backlog and hosted-CI validation follow-up are complete. Item 7 used the actual
native Windows workstation and a separate Linux workstation; it did not
substitute native Windows CI or package inspection for the physical exercise.

## Native Windows workstation acceptance

The accepted `agent-coordinator 0.1.0` Windows x86-64 CLI from
[release run 34459987847](https://github.com/marshallr12/agent_coordinator/actions/runs/34459987847)
ran natively on workstation `MINIAIR` against a disposable service on Linux
workstation `mxmini` through a temporary Cloudflare HTTPS tunnel. The downloaded
Windows archive SHA-256 was
`be5e96ec6f8843a63c8be319d9b4f9745f4e0870bebbbc160c2c24f69dc32a1d`,
matching its adjacent checksum; the exercised executable SHA-256 was
`40240c59aeaf012aba6721f378e9dec57419ceb8833aec0187391c3bccb1111d`.

The exercise used distinct Windows and Linux principals and sessions across two
isolated projects. Windows claimed, checkpointed, and submitted project-two work;
a same-principal agent review was rejected, while the independent Linux principal
claimed and approved the exact submission. Project-one work remained isolated.
At a published barrier, both workstations attempted the same project-one task and
revision. Linux obtained generation 2 ownership and Windows received the expected
`claim_conflict`; Linux checkpointed the result and released the task ready. In a
separate recovery case, Windows checkpointed with a dedicated session, that session
was closed, and Linux inspected and released the retained work without reviving the
Windows session.

Sanitized cross-workstation evidence is retained in
[completion commit `c017a69`](https://github.com/marshallr12/agent_coordinator/commit/c017a69152aaf52778bc3e48825446b700a18903).
The encrypted one-time handoff branch was removed after Windows decrypted it. The
disposable public service and tunnel were shut down after acceptance. This proves
the scoped native two-workstation workflow; it is not a production deployment,
permanent public endpoint, hardware attestation, or off-server backup exercise.

## MCP completion

The authenticated stateless `/mcp` endpoint exposes 56 closed, typed tools through
the same guarded REST handlers. Connections, discovery, ping, and receipt replay
never renew ownership. The native `mcp-client` launcher securely shares one saved
harness session with a trusted foreground MCP client and its native CLI children.
Git, local producers, and binary transfer remain native operations. The launcher
requires an absolute executable path and does not supervise background clients.
No service-side execution or OAuth discovery is introduced.

Candidate `cd9a633` passed all 182 local workspace tests, formatting,
warnings-denied Clippy, build, and both smoke exercises. Linux workspace/smoke,
native Windows client/CLI/local-runner tests, and the dependency audit passed in
[CI run 34468585302](https://github.com/marshallr12/agent_coordinator/actions/runs/34468585302).
The integrated `677253a` also passed
[CI run 34468821269](https://github.com/marshallr12/agent_coordinator/actions/runs/34468821269).
Official SDK tests exercise modern and legacy protocols over real TCP; wire tests
cover revocation, races, policies, shared REST receipts, and generation/lease guards.
See the [MCP guide](docs/mcp-guide.md) for configuration and compatibility limits.

## Documentation completion

Canonical guides, contracts, the plan, and README content now live in `book/src`.
The original docs/README/PLAN entry paths remain short compatibility links.
Root AGENTS.md, HANDOFF.md, BACKLOG.md, and DURABLE-RECORD.md remain authoritative;
their book chapters include them at build time. Edit those root files directly.

The pinned mdBook 0.5.4 build covers all 35 chapters. Main review verified retained
acceptance evidence, proposal/current-status distinctions, source references,
rendered internal links and fragments, search, live includes, and desktop/390-pixel
phone layout without page overflow or JavaScript errors. Release packages retain
bounded Markdown sources and book configuration, without generated HTML.

Package checks passed for layout, checksums, permissions, offline links, explicit
member/size limits, and building the book from an extracted archive. The final
local structural package used stripped copies of the current local debug binaries;
it is documentation validation, not new release-binary or capacity acceptance.
Local-file navigation and search also passed with networking disabled.

After the repository became public, hosted validation was retried on main commit
`ebf72f7`. The pinned mdBook build and documentation checks passed in
[documentation run 34487173043](https://github.com/marshallr12/agent_coordinator/actions/runs/34487173043).
Linux formatting, warnings-denied Clippy, all workspace tests, the workspace build,
both service/CLI smoke exercises, native Windows client/CLI/local-runner tests,
and the locked dependency audit passed in
[coordination run 34487175859](https://github.com/marshallr12/agent_coordinator/actions/runs/34487175859).
These runs supersede the earlier failed-to-start billing-blocked attempts; no job
steps ran in those attempts, so they remain historical scheduling failures rather
than test results.

Use `mdbook build` or `python3 scripts/check_docs.py` from a source checkout; output
is `target/book/index.html`. See [book maintenance](docs/documentation.md).
The retained [Linux acceptance evidence](docs/linux-capacity-evidence.md) remains
unchanged and identifies its exact accepted package and server. Schema version is
16 and instruction version is 7. No production deployment or actual off-server
transfer is claimed. Host/domain/backup-destination choices remain installation
inputs. Keep separate Cargo targets for concurrent worktrees.
