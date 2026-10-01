# HANDOFF — Full agent autonomy for Agent Coordinator (S6 shipped; host bootstrap next)

Written 2026-09-25 by the planning session (Claude Opus 5.5, lead) at the end of a review and
multi-agent planning discussion. **Read this file first**; it is self-contained enough to start
execution, and links everything else.

## Current resume point (2026-09-29 evening; 2026-09-30 UTC)

The user explicitly disabled the Agent Coordinator workflow for this session. Do not create,
claim or update service tasks; use this handoff for progress. This session override takes
precedence over U14's earlier dogfood approval.

- `origin/main` and production are now `1e8aebb9d67d1bde4c3b2de36c34703f63fcfd28`.
  S6 was reviewed, shipped and deployed this session; no coordinator tasks were created or claimed.
- The clean starting checkout was `autonomy-plan` at `b5d76de`. It contains planning history
  and the older prototype, rather than the current product implementation. Work from the
  newly created branch `autonomy/s6`, worktree `~/src/worktrees/agent-coordinator-s6`, based
  on `origin/main` `4a72018`. Keep its Cargo target directory inside that worktree.
- Corrected the planning branch's pre-existing formatting failure in
  `crates/server/build.rs`; formatting checks now pass on both checkouts. The formatting
  correction was recorded in the previous handoff update. S6 had no product changes at that point.
  The pinned mdBook 0.5.4 build and documentation source/package/local-link checks passed.
- **Next phase: P4 S6**, following `planning/autonomy/p4-design.md` §§4–5: integrator host
  setup on oracle-1, a production shadow day, required-check flip-rate evidence, cutover
  preflight, full rulesets, then `integration_owner=integrator`. P3b follows S6; the optional
  shadow cost run is not a prerequisite.
- **U4 ruleset test complete (2026-09-29):** with the user's explicit permission, created
  public `marshallr12/agentc-ruleset-test`, active ruleset `app-only-test` (id `24210180`,
  `update` rule, sole bypass = App `5127380`, mode `always`). A real Git push authenticated
  as `marshallr12` was rejected with `GH013: Cannot update this protected ref`, both before
  and after adding the App bypass; the remote tip stayed unchanged. An App installation
  token then successfully pushed the same candidate `1547f6534183978fe4d373cc63229eb55657e7fe`,
  verified by remote read-back. The token was restricted to test-repo Contents write and
  revoked after use. U4 does not require moving to an organization. Evidence and candidate
  bundle are outside the repository at `~/.local/share/agent-coordinator-autonomy/ruleset-test/`.
- **App registered and test-installed:** `marshallr12-agentc-integrator`, id `5127380`,
  installation id `166293403`, private, no webhook,
  Contents/Actions write and Checks/Administration/Metadata read. The user connected Brave
  and completed GitHub's authentication and manifest-registration steps. The manifest helper
  transferred the generated key directly from memory over SSH to oracle-1:
  `/etc/agentc/integrator-app.pem`, verified `root:root`, mode `0400`; no PEM was saved on
  this workstation. The SSH connection is the user's Bash alias `oracle` (`ssh oracle-1`
  itself does not resolve). The user completed the test-only installation in Brave. The App
  now has selected-repository access only to `marshallr12/agent_coordinator`: **the user saved
  this change on 2026-09-29, and browser read-back showed GitHub's update confirmation and
  exactly one selected repository; `agentc-ruleset-test` is removed**.
  Production rulesets and coordinator ownership have not been changed. Temporary host test
  files were removed, and the local manifest-registration helper was stopped.
  Never request or copy the App private key into this checkout;
  keep it on oracle-1. Host setup and credential installation remain user steps.
- **S6 source shipped/deployed (2026-09-30 UTC):** product candidate `1e8aebb` in the
  existing `autonomy/s6` worktree, incorporating `14e2fda`, `e042e26` and `291b680`.
  Fresh-context review used **gpt-6-astra/xhigh** (model-select score 8, security review).
  Review reproduced a false-green preflight for expired/revoked ordinary task and review
  attempts whose recovery is derived rather than persisted. Fixed by requiring zero
  `tasks.current_attempt_id`; added an expired-task/review regression (five ops tests pass).
  No remaining concrete S6 shipping blocker was found.
  Read-only live roster revision 3 confirmed `linux-validation`, `documentation`,
  `dependency-audit`; their exact GitHub job names/paths are now committed in
  `.agent-coordinator/roster.toml`. App access remains complete.
  Local gate: 502 workspace tests, format, warnings-denied Clippy, locked audit, workspace
  build, service/CLI smoke, backup/restore smoke, pinned documentation checks and five ops
  regressions passed. CI runs **36662950828** and **36662950832** passed on the exact SHA.
  `main` was fast-forwarded and remote read-back verified. Release acceptance
  **36663055767** passed Linux package/systemd/HTTPS, five-minute load and Windows packaging.
  Binaries-only production deploy: all seven preflight counts zero before deployment and
  after stopping; health and public build identity verified; backup, maintenance, transfer
  and timers passed. Stop/start 03:23:02–03:23:25 UTC (health at 03:23:26).
  Rollback binaries use suffix `-0.1.1-4a72018`; verified old snapshot
  `20260930T032256.016Z-88af7c3f-0b71-4134-8da5-92be7292612f`, new snapshot
  `20260930T032326.636Z-fdd008a6-28f0-42ed-b119-9aca91aaedb7`.
  Workstation CLI upgraded and verified at `1e8aebb`; rollback remains
  `~/.local/bin/agent-coordinator.rollback`. Explicit-path compatibility is green.
  This noninteractive shell resolves an older `/usr/local/bin/agent-coordinator`;
  use the verified `~/.local/bin/agent-coordinator` path for product commands.
  Evidence/logs/scripts/archive: `~/.local/share/agent-coordinator-autonomy/release-1e8aebb/`.
  Live policy still revision **6**, `integration_owner=agent`; no ruleset changes.
- **oracle-1 bootstrap verified (2026-09-30 04:28 UTC):** owner completed installation,
  configuration and credential setup. Shadow unit enabled/running since **04:28:08 UTC**;
  first successful target observation **04:28:12 UTC**, zero restarts and no observed errors.
  App and coordinator source files are `root:root` mode `0400`; config is mode `0644`.
  Live `@run` unit is inactive. Shadow reads main `1e8aebb` and successful Actions checks;
  missing `required_status_checks` is expected before full rulesets. Only Target records have
  been observed: candidate computation/publication remains unexercised on production.
  Startup journal evidence: `~/.local/share/agent-coordinator-autonomy/release-1e8aebb/shadow-start.log`.
  **Earliest 24-hour checkpoint: 2026-10-01 04:28:08 UTC / 00:28:08 EDT.** Verify continuous
  coverage, errors/restarts and any WouldPush records then; elapsed time alone is not a pass.
  **24-hour checkpoint read 2026-10-01 04:59 UTC: polling PASS.** Unit active since 04:28:08,
  0 restarts, `Result=success`, memory peak 31 MiB (budget 256 MiB), no kernel OOM lines, `@run`
  inactive. 2726 records over 24 h 31 min: 2721 `Target` (shadow mode, T0 and main `1e8aebb`,
  `missing_rules=["required_status_checks"]` as expected, `pending_reverts=0`), poll median 32 s,
  max gap 47 s. 2720 Target polls listed 11 check runs, all `success`. One poll (13:01:05 UTC) listed
  zero checks; confirm in the live candidate test that an empty check list fails closed. 5 `Error` records,
  all `GitHub replied 502 Bad Gateway` (10:12, 13:23, 13:26, 17:25, 20:33 UTC), each followed by a
  normal poll. **No `WouldPush`:** the queue had no approved candidates, so candidate computation
  and publication remain unexercised; this proves polling only. Journal copy (outside Git):
  `~/.local/share/agent-coordinator-autonomy/release-1e8aebb/shadow-day-journal.log`.
  Remaining S6 gates: live candidate test, pinned-main stability sample, full rulesets,
  zero drain preflight, write credential, human ownership switch and canary.
- **oracle-1 build preparation (completed):** ARM64, systemd 259, Rust 1.98.1; separate clean detached
  checkout `/home/ubuntu/src/worktrees/agent-coordinator-s6-release` at `1e8aebb`.
  Host-native integrator release build passed; binary SHA256
  `4018b8cee4e1bda30b64da585fc899bb1b35a0b177b50c508d8f1bb46cce01fa`.
  `/home/ubuntu/integrator-shadow.toml` contains the verified production project/App ids and
  runtime credential paths; the native binary parsed it successfully. No secret was copied.
  **Owner bootstrap commands (completed):**
  ```sh
  cd ~/src/worktrees/agent-coordinator-s6-release
  sudo INTEGRATOR="$PWD/target/release/agentc-integrator" bash deploy/agentc/integrator-host-setup.sh
  sudo install -o root -g root -m 0644 ~/integrator-shadow.toml /etc/agentc/integrator.toml
  ```
  Owner issued a **read-only, integrator-class** credential in the dashboard and saved CLI-format
  TOML to `/etc/agentc/integrator-credentials.toml`, `root:root` mode `0400`, without putting
  its token in chat or Git. The shadow service is now started.
  Later write-credential issue, rulesets and ownership switch remain user steps per the design.
  **S6 remains incomplete:** still needs 24-hour production shadow, reviewed live candidate
  test, >=20 required-job attempts on pinned main each <2% non-success, zero drain preflight,
  full rulesets, write credential, human ownership switch and canary. Canonical instructions:
  `book/src/docs/integrator-cutover.md` in the S6 worktree.
- The P3a shadow poller is stopped, and the old P2/P3a/P4 worktrees are gone. Historical
  commands below that refer to those worktrees must not be run unchanged.

First local commands for the next session:
```sh
cd ~/src/worktrees/agent-coordinator-s6
export CARGO_TARGET_DIR="$PWD/target"
git status --short
cargo fmt --all -- --check
```
Read the design from the planning checkout: `~/src/agent_coordinator/planning/autonomy/p4-design.md`.
The previous deployed product gate passed on `4a72018` (see §11). The S6 candidate verification
is recorded above; production is now at `1e8aebb`. Follow the S6 runbook for live prerequisites.

## Wave-set session (2026-10-01)

The user again disabled the Agent Coordinator workflow for this session; do not create or claim
service tasks. The S6 cutover chain (24-hour shadow check due from 2026-10-01 04:28 UTC, live
candidate test, stability sample, rulesets, write credential, ownership switch, canary) stays
serial operations work and is not a lane. This wave-set runs alongside it.

- **Build tree:** a fresh worktree `~/src/worktrees/agent-coordinator-hardening` on new branch
  `hardening/wave1` from freshly fetched `origin/main` (`1e8aebb`), `CARGO_TARGET_DIR` inside it.
  Lane paths below are relative to that tree; this living doc stays in the `autonomy-plan` checkout.
- **Staleness:** six scouts each reported `live` with negative-controlled git-log and tree probes;
  nothing was struck.
- **Serialized (not in this wave-set):** R-P3b.2, .3, .4 and R-P3b.5(b)(c) share
  `deploy/agentc/host-setup.sh`, `containment-suite.sh` and `crates/supervisor` with lane B;
  R-P3b.5(d) shares `crates/server` with lane A and needs a user ruling on read-only reviewer
  credentials creating subagent identities. Scout findings worth keeping:
  - R-P3b.2: the integrator never pushes candidate refs; full rulesets A/B (user step) do not
    protect `refs/agent-coordinator/candidates/*`, so a separate-uid helper is still required.
    Decisions: helper credential (not the integrator App), helper uid, ref allow-list.
  - R-P3b.3: the reviewer's Bash is plain `Bash`, so isolation must be OS-level. Decisions:
    third uid vs bubblewrap, what counts as candidate code (`scripts/verify_ui.mjs` reads the
    staging login), clone/target sharing, staging-port access.
  - R-P3b.4: `meta skuid` matches the sender, not the listener's owner, so no nft-only rule
    protects listeners; only per-uid network namespaces fully fix it. Decide (a) netns,
    (b) port slices, (c) accept and document; spike `input`-hook skuid on mxmini first.
  - R-P3b.5(d): `allowed_for_read_access` admits any `POST /api/v1/sessions`, and
    `resolve_subagent` creates identities without an access check.
- **Host facts (2026-10-01):** `host-setup.sh` ran on mxmini during P2 (§11); both
  `/var/lib/agentc/{impl,rev}/claude-config` are empty: no agent-account Claude login exists yet.

### Wave plan (consumed by /wave-run)

```yaml
wave_set: TypeSafe hardening ∥ R-P3b.1 Claude-profile write confinement ∥ ship.py hardening
capabilities: [bin:cargo>=1.98.1, bin:python3>=3.11, bin:mdbook=0.5.4, bin:bash>=5, bin:git, write:worktree-target, disk:>=60G-free]
build_tree: {path: ~/src/worktrees/agent-coordinator-hardening, branch: hardening/wave1, base: origin/main 1e8aebb}
workspace_gate: >-
  cargo fmt --all -- --check && cargo clippy --workspace --all-targets --locked -- -D warnings &&
  cargo test --workspace --locked && cargo build --workspace --locked && python3 scripts/smoke.py &&
  python3 scripts/backup_smoke.py && python3 scripts/check_docs.py --mdbook "$(command -v mdbook)"
lanes:
  - lane: A
    item: TypeSafe hardening (concurrency cap, circuit breaker, main-service-only secret file)
    paths: [crates/server/src/context_rerank.rs, crates/server/src/context_rerank/tests.rs,
            crates/server/tests/context_rerank.rs, crates/server/src/main.rs,
            deploy/agent-coordinator.service, deploy/service.env.example,
            scripts/linux_install_smoke.py,
            book/src/docs/knowledge-contract.md, docs/knowledge-contract.md,
            book/src/docs/linux-installation.md, docs/linux-installation.md,
            book/src/docs/backup-restore-guide.md, docs/backup-restore-guide.md]
    out_of_repo: [~/.local/share/agent-coordinator-autonomy/deploy-pre.sh,
                  ~/.local/share/agent-coordinator-autonomy/release-next/deploy-pre.sh]  # main loop only
    hot_files: [Cargo.lock]
    resources: []   # mock TypeSafe servers bind 127.0.0.1:0
    capabilities: [bin:cargo>=1.98.1, bin:python3>=3.11, bin:mdbook=0.5.4, write:worktree-target]
    phases:
      - {id: A-P1, goal: "Concurrency cap: ContextRerankConfig gains max_in_flight (default 4); ContextReranker holds a tokio Semaphore; over-cap calls use try_acquire and return Unchanged::Skipped(\"busy\") without calling TypeSafe", done_when: ["unit test: with the cap held, rerank returns Skipped(\"busy\") and the Mock hit count does not increase", "existing context_rerank unit tests pass unchanged", "struct-literal config sites compile via ..Default::default()"], module_gate: "cargo test -p coordinator-server --lib context_rerank", model: sonnet/medium}
      - {id: A-P2, goal: "Circuit breaker: failure_threshold (default 3) consecutive failures open the breaker for open_for (default 60 s); while open, Skipped(\"circuit_open\") with no call; after the window one half-open probe; success closes it, failure re-opens it", done_when: ["unit test: 3 failing Mock responses then a 4th call returns Skipped(\"circuit_open\") with no extra hit", "unit test: after the window (injected clock or short config) one probe is sent; success resets the counter", "a success between failures resets the consecutive count"], module_gate: "cargo test -p coordinator-server --lib context_rerank", model: sonnet/high}
      - {id: A-P3, goal: "End-to-end through AppState: saturation and an open breaker both return context in FTS order and log the skip reason", done_when: ["e2e test: concurrent context requests beyond the cap all succeed, with the over-cap results in FTS order", "e2e test: a failing TypeSafe mock opens the breaker and later requests skip it"], module_gate: "cargo test -p coordinator-server --test context_rerank", model: sonnet/medium}
      - {id: A-P4, goal: "Secret file: agent-coordinator.service adds EnvironmentFile=-/etc/agent-coordinator/typesafe.env (root:root 0600) after service.env; backup/maintenance units unchanged; no example file shipped; service.env.example, linux-installation and backup-restore-guide (book/src and docs copies) name the file and the cap/breaker behaviour; knowledge-contract states over-cap/open-breaker calls keep FTS order; linux_install_smoke rewrites the new path", done_when: ["grep shows EnvironmentFile=-/etc/agent-coordinator/typesafe.env only in deploy/agent-coordinator.service", "python3 scripts/check_docs.py --mdbook \"$(command -v mdbook)\" exits 0", "scripts/linux_install_smoke.py rewrites /etc/agent-coordinator/typesafe.env alongside service.env"], module_gate: "python3 scripts/check_docs.py --mdbook \"$(command -v mdbook)\"", model: sonnet/low}
    main_loop_after_A-P4: "Edit both out-of-repo deploy-pre.sh copies: check TYPESAFE_API_KEY in /etc/agent-coordinator/typesafe.env and exit non-zero before stopping anything when it is missing or empty. VM cutover is a user step: install typesafe.env, deploy the new unit, daemon-reload, remove the key line from service.env, restart."
  - lane: B
    item: R-P3b.1 Claude-profile write confinement
    paths: [crates/supervisor/src/profile.rs, crates/supervisor/src/launch.rs,
            crates/supervisor/src/preflight.rs, crates/supervisor/src/role_settings.rs,
            crates/supervisor/src/config.rs, crates/supervisor/src/confine.rs,
            deploy/agentc/host-setup.sh, deploy/agentc/containment-suite.sh,
            deploy/README.md, book/src/deploy/README.md]
    hot_files: [crates/supervisor/src/main.rs, Cargo.lock]   # main.rs: mod line and module doc only
    resources: [mxmini root host: /var/lib/agentc, nft table inet agentc, proxy 3128, staging 18080 (B-P6 only, user-run)]
    capabilities: [bin:cargo>=1.98.1, bin:bash>=5, write:worktree-target]
    phases:
      - {id: B-P1, goal: "Containment-suite write probes (red first): as each agent uid, a planted claude-config/settings.json, $CARGO_HOME/config.toml rustc-wrapper, $HOME dotfile, or write into another launch's clone/run must not reach the next launch", done_when: ["new check functions exist and are called from main()", "bash -n deploy/agentc/containment-suite.sh exits 0"], module_gate: "bash -n deploy/agentc/containment-suite.sh", model: sonnet/high}
      - {id: B-P2, goal: "Per-launch state: profile::environment points HOME, CARGO_HOME and AGENT_COORDINATOR_HOME at 0700 dirs under $RUN/state; CARGO_HOME holds the root-owned pinned config.toml seed plus symlinks to a persistent per-role registry/git cache (D2); prepare_run creates and seeds them; keep the last 5 runs' state per role and prune older ones at prepare (D3)", done_when: ["unit test: two prepare_run calls yield distinct HOME/CARGO_HOME paths", "unit test: CARGO_HOME/config.toml equals the seed and registry/git resolve to the shared cache", "unit test: a sixth run prunes the oldest state dir"], module_gate: "cargo test -p agentc-supervisor", model: opus/high}
      - {id: B-P3, goal: "Claude login (D1): CLAUDE_CONFIG_DIR stays the persistent per-role claude-config, holding only the agent-writable credentials file; settings.json and CLAUDE.md there are root-owned seeds; per-launch state never holds a credential copy", done_when: ["unit test: environment() maps CLAUDE_CONFIG_DIR to the per-role dir and HOME to the per-launch dir", "unit test: the rendered user-scope settings equal role_settings::render output"], module_gate: "cargo test -p agentc-supervisor", model: opus/high}
      - {id: B-P4, goal: "Preflight refuses a launch when claude-config/settings.json or CLAUDE.md is not root-owned or differs from the seed, when the cargo config seed differs, or when per-launch dirs are not 0700; problems are reported, never skipped", done_when: ["unit tests (tempdir, problem-reported direction): edited settings.json, extra CLAUDE.md, edited cargo config and a 0755 per-launch dir each produce a preflight problem", "missing_containment_is_reported_not_skipped extended and passing"], module_gate: "cargo test -p agentc-supervisor preflight", model: opus/high}
      - {id: B-P5, goal: "host-setup.sh: root-owned seeds (claude settings.json, CLAUDE.md placeholder, cargo config.toml) installed into the persistent dirs, agent-writable only for the credentials file and the cargo cache; create_dirs/next_steps/--uninstall updated; idempotent re-run", done_when: ["bash -n deploy/agentc/host-setup.sh exits 0", "next_steps still prints the per-account claude auth login command with CLAUDE_CONFIG_DIR"], module_gate: "bash -n deploy/agentc/host-setup.sh", model: sonnet/high}
      - {id: B-P6, goal: "Host proof on mxmini (user runs sudo): re-run host-setup with this tree's release binaries, log in agentc-impl and agentc-rev with claude auth login, run the containment suite with --cargo-test", done_when: ["containment suite reports PASS for every check, including the B-P1 probes", "a credentials file exists in each claude-config and settings.json there is root-owned"], module_gate: "sudo deploy/agentc/containment-suite.sh --cargo-test (user)", model: main-loop+user}
      - {id: B-P7, goal: "Prose: deploy/README.md and book/src/deploy/README.md, host-setup.sh and containment-suite.sh headers, supervisor module docs (main.rs, profile.rs environment(), role_settings.rs header) describe per-launch state and the root-owned seeds", done_when: ["prose-lint.sh passes on the changed lines", "check_docs.py exits 0"], module_gate: "python3 scripts/check_docs.py --mdbook \"$(command -v mdbook)\"", model: haiku/medium}
  - lane: C
    item: ship.py hardening (R-P3b.5a)
    paths: [scripts/ship.py, scripts/ship_test.py, CONTRIBUTING.md]
    hot_files: []
    resources: []
    capabilities: [bin:python3>=3.11, bin:git]
    phases:
      - {id: C-P1, goal: "require_fast_forward ancestor-checks git rev-parse FETCH_HEAD from the fetch it just ran (no separate ls-remote tip); gh run list gets --repo OWNER/NAME derived from the remote URL", done_when: ["ship_test: a tip that moves between fetch and check cannot pass the ancestor check (mocked run)", "ship_test: every gh invocation carries --repo"], module_gate: "python3 -m unittest scripts/ship_test.py", model: sonnet/medium}
      - {id: C-P2, goal: "wait_for_checks gets an overall deadline (in-code default, overridable flag) covering the completed-wait loop, not only the appear wait; CONTRIBUTING's ship paragraph states it", done_when: ["ship_test: runs that never complete hit the overall deadline and sys.exit with a message naming the SHA", "existing appear-timeout behaviour unchanged"], module_gate: "python3 -m unittest scripts/ship_test.py", model: sonnet/medium}
serializes:
  - {item: "R-P3b.2 candidate-push helper", behind: "lane B (host-setup/containment-suite/supervisor) + user decisions (helper credential, uid, ref allow-list)"}
  - {item: "R-P3b.3 reviewer candidate-code isolation", behind: "lane B + user decisions (third uid vs bwrap, what counts as candidate code)"}
  - {item: "R-P3b.4 cross-uid loopback", behind: "lane B + design decision (netns vs port slices vs accept) + mxmini skuid spike"}
  - {item: "R-P3b.5(b)(c) launch no_new_privs/process cleanup, --uninstall", behind: "lane B (launch.rs, profile.rs, host-setup.sh)"}
  - {item: "R-P3b.5(d) read-only credentials creating subagent identities", behind: "lane A (crates/server) + user ruling on reviewer subagents"}
decisions:
  - {q: "Wave-set composition", answer: "TypeSafe ∥ R-P3b.1 ∥ ship.py (2026-10-01, user)"}
  - {q: "Rerank cap/breaker", answer: "cap 4, over-cap skips; 3 consecutive failures open for 60 s, then one half-open probe"}
  - {q: "TypeSafe secret file", answer: "/etc/agent-coordinator/typesafe.env root:root 0600, no shipped example, deploy-pre.sh aborts when the key is missing"}
  - {q: "D1 Claude login location", answer: "persistent per-role claude-config holding only credentials; settings/CLAUDE.md root-owned and preflight-verified"}
  - {q: "D2 CARGO_HOME", answer: "per-launch CARGO_HOME with root-owned config seed; shared persistent registry/git cache"}
  - {q: "D3 per-launch state retention", answer: "keep the last 5 per role; prune at prepare"}
  - {q: "Builder models", answer: "per-phase table above (manual model-select scoring; TypeSafe scoring not run)"}
user_steps:   # /wave-run pauses and gives the user exact commands at each; agents never run root or production steps
  - {when: "before B-P6", who: user, what: "on mxmini: sudo re-run deploy/agentc/host-setup.sh with this tree's release binaries; claude auth login for agentc-impl and agentc-rev (command printed by next_steps); sudo deploy/agentc/containment-suite.sh --cargo-test; paste the output"}
  - {when: "after lane A lands on main and is deployed", who: user, what: "on the production VM: install /etc/agent-coordinator/typesafe.env (root:root 0600) with the key, install the new unit, daemon-reload, remove the TYPESAFE_API_KEY line from service.env, restart; agent then verifies health and the rerank log line"}
  - {when: "every push to main or production deploy", who: user, what: "explicit go-ahead at that moment (public repo; U9 deploys still need the preflight at 0)"}
  - {when: "wave-set close", who: user, what: "read the summary; decide R-P3b.2-.4 and R-P3b.5(d) for the next wave-set"}
deferred_host_work: "oracle-1 supervised-host setup (host-setup.sh) waits for P3b bring-up after R-P3b.1-.4 land, ideally after S6 cutover; checked 2026-10-01: agentc-impl/agentc-rev absent there, only the integrator is installed"
preflight: {run: 2026-10-01T04:56:55Z, result: green, method: "manual (repo has no scripts/preflight.sh): cargo/rustc 1.98.1, python3 3.11.2, mdbook 0.5.4, bash 5.2, git 2.39, 124G free, worktrees dir writable, both deploy-pre.sh copies present; shellcheck absent so shell gates use bash -n"}
```

## 1. What was asked

The user asked for this work to be done **outside the Agent Coordinator workflow** ("DO NOT create a
new task for this work"). Goal, in the user's words: *"enable 100% autonomy of agents that work on
projects whose settings allow independent agents to perform required reviews, expired work recovery,
integration authorization, and change binding project rules."* The brief:

1. A comprehensive review: project knowledge and evidence (with history), completed and archived tasks
   (briefing, history, checkpoint trail), git logs, agent prompt history (mostly Codex), and the
   codebase including service-sent instructions and the mdBook docs.
2. A plan with no limits: tooling, database, service and documentation could be rewritten, and a new
   branch or new git project was allowed.
3. At least two Opus 5.5 high-effort subagents critically reviewing the plan as a *group* discussion,
   one a polite devil's advocate, for 1–6 hours, with PoCs allowed (cleaned up afterwards), and a
   transcript file the user can review.
4. A final plan recorded in a handoff with as much of the discussion and reference material as a
   future session needs to execute it faithfully.

## 2. What was done (and not done)

- **Branch:** `autonomy-plan`, created from `main` at `d72a8cb`. Planning files are **committed on this
  branch and pushed to origin** under `planning/autonomy/` (U15 answered). The repo is **public**: review before
  pushing (the review reports quote live task checkpoints, workstation names and VM details, similar
  to what HANDOFF-archive.md already publishes).
- **Service access:** read-only. The auto-mode classifier refused `agent-coordinator connect`
  (creating a session is an external write), so data came from **bearer-token GETs only** (no session
  created, nothing claimed, no task created). Tools: `planning/autonomy/tools/get.py`,
  `fetchall.py`, `fetch2.py` (read `AGENT_COORDINATOR_TOKEN` from the environment, never print it).
  The raw snapshot (53 tasks with full history, 6,153 events, 6 knowledge entries with revisions,
  policy history, the 147 KB Markdown export) is kept **outside the repo** at
  `~/.local/share/agent-coordinator-autonomy/snapshot-2026-09-25/` (mode 0700).
- **Review reports** (Opus subagents, read-only): `planning/autonomy/review/service-data.md`,
  `codebase.md`, `history.md`.
- **Discussion:** 9 rounds, 16:27–17:26 UTC (59 minutes), lead + Dana (devil's advocate) + Sol (systems &
  protocol) + Hari (harness & ops), all Opus 5.5. Full verbatim transcript:
  `planning/autonomy/discussion-transcript.md`. Plan versions: `planning/autonomy/plan-v1.md` … `plan-v4.md`;
  **the final plan is `planning/autonomy/plan-final.md`**.
- **PoCs** (all cleaned up; worktrees, branches and target dirs deleted):
  - Dana: P1-lite in a worktree — scoped pinning + policy-gated reopen/unblock + labelled human gates
    = **62 lines added / 17 deleted + 32-line test**, 1 assertion changed (a rename), **all 155 server
    tests pass**, clippy clean. Full P1 estimate 450–650 LOC incl. tests/docs, 1–2 days.
  - Sol: 54-line Git script (Git 2.39.5) proving roll-forward publication: concurrent same-R
    publishers converge; once the target leaves T0 a lease-T0 push fails forever; a force-push rewind
    is detected (and shows the ABA hazard); `receive.denyNonFastForwards` blocks it. **Finding:
    `git push --force-with-lease=main:T0 R` exits 0 when the remote already equals R even with a stale
    lease — never infer success from a push exit code.**
  - Hari: `codex sandbox` probe (no model): without network, `socket()` fails (explains past
    "loopback tests blocked" / "cannot resolve service host"); with `network_access=true` loopback and
    DNS work and out-of-workspace writes stay denied. GitHub docs + read-only `gh api`: repo is
    personal/public with **no rulesets and no branch protection**; rulesets (`update`, `creation`,
    `deletion`, `non_fast_forward`, `required_status_checks` with `integration_id`) are available on
    public personal repos; merge queue and classic push restrictions are org-only.
- **Not done:** no code changes, no rulesets applied, no service mutations, no user decisions taken.

## 3. The diagnosis in one paragraph

The four autonomy switches were chosen on day one (09-09) and have been on since 09-15/23 (policy rev
6: `review_mode=either`, `allow_subagent_reviews`, `recovery_mode=agent`, `automatic_integration`,
`agent_rule_editing`). **The policy was never the blocker.** Humans were needed because every ordinary
failure landed on a hard-coded human-only operation, and because agents only existed while a human kept
a harness turn alive. Evidence (this project only): the human performed 26 submission reopens (16 pure
policy staleness, 2 of them lease-length only; 5 merge conflicts; 2 legacy refs), 9 publication
reconciliations (0 after agent reconciliation landed on 09-23), 6 unblocks, 4 reservation resolves, 3
roster edits; ≥20 "continue / why have you stopped?" prompts; ≥8 push/submit approvals and ~700 Codex
sandbox escalations; 5 hand-typed "spawn a reviewer" prompts; agent-silent gaps of 32 h / 24 h / 18 h;
P0 tasks unclaimed for 40–144 h; 131/360 check jobs failed (36%, environment not code); 60/71 agent
reviews were same-principal subagents and 44/75 decisions took under 5 minutes; the agent instruction
surface is ~10–11k words before the first claim (startup guide grew 179 → 591 lines in 10 days); the
user routed around the workflow 6 times ("do NOT create a task") and one direct push (9bb9a28) broke a
required check for every candidate. In most dashboard episodes the human click added **authority, not
information** — but not all (Dana): gates are triaged into (a) authority-only, (b) information, (c)
judgement.

## 4. The final plan (summary — details in `planning/autonomy/plan-final.md`)

**Principles:** evolve, don't rewrite; LLMs produce and judge changes, deterministic components do
plumbing; the coordinator's policy is the authority layer and the harness only provides containment;
every new component names what it deletes; items are **[R-Pn]** (required in phase n) or **[F]**
(built only when a measured failure justifies it); threat model = confidently wrong agents (adversarial
resistance limited to blast-radius bounds).

**Success measure:** every task reaches a terminal state (done, or canceled/superseded with agent
rationale) and every mutation came from a supervised credential, the supervisor or the integrator;
HRI/task counts human-attention mutations, parked-task notifications and **stalls**; canaries: value
(human tasks done/week), escaped defects, flake rate, direct pushes, spend, review latency, audit
disagreement.

**Components:**
- **Service P1 changes** (~550–750 LOC incl. tests, CLI, docs) — agent `revise` by **extending
  `workflow/reopen`** with a required `reason{code, evidence}` (closed enum, per-reason actors, the
  existing unresolved-intent guard, rate limit 3 per subject per 24 h then park); scoped pinning
  (candidate + **digest of title/description/acceptance criteria/kind** + monotone review set;
  satisfaction order human ≥ either ≥ agent; roster revision **captured on the publication intent**);
  `reconcile_required_reviews` in the policy-update transaction and at startup; agent
  unblock/cancel/replace; AC amendments approved explicitly by the reviewer; labelled human gates with
  `required_actor`; acks keyed on `(INSTRUCTION_VERSION, sha256(rules))`; thin CLI `revise`, `unblock`,
  `cancel`; a P1 deploy preflight. Known P1-only human gates remain (check failure after an intent;
  target moved after an intent) until P4.
- **Containment (P2)** — `agentc-impl` / `agentc-rev` uids, root-owned firewall and pinned binaries,
  **per-launch Git clones per role** (no shared `.git`, hooks disabled), deterministic harness profiles
  (Claude `dontAsk` + `--permission-prompts none`; Codex `workspace-write` + network +
  `approval_policy=never`), candidate-controlled instruction files never auto-loaded, settings
  preflight, secret-scanning `push-candidate`, Playwright + `verification_env`, staging coordinator,
  `ship` script; service **credential attributes `class` + `access`** (P2/P3a).
- **Supervisor (P3a shadow, P3b live pilot core ≤1.5k lines)** — progress-gated lease renewal, crash
  release, service-verifiable recovery (`checkpoints.revision` column), per-launch clones, project
  `setup`, fresh read-only reviewers with structured verdicts posted by the supervisor, 10%
  cross-vendor audit sample, credential/quota health, per-role contract ≤1.5k words, `next`, decisions
  with 24 h timeout-to-recommendation (reversible only), one daily digest, one end-to-end canary with
  SLO paging, stall-as-HRI. **Host updater, admission control (U12), per-vendor/per-host canaries and
  digest-neglect paging are [R-P6] — in place before dogfood, not the pilot.**
- **Integrator (P4-min, before the pilot)** — deterministic, own uid + GitHub App; pinned result
  `UNIQUE(submission, T0)`; roll-forward reconciliation with service-side ancestry check; I-FF with
  freeze-on-rewrite and a **ruleset watchdog**; revise loses to a landed push; result branches
  `ac/results/<id>`; privilege gate on changed workflows; GitHub Actions receipts bound to (R,
  target's definition blob, Actions app); one deciding run; reproducible flake attribution;
  `candidate_reverted_in_history`; `unreviewed_landing` by agent trailers (best-effort);
  **landing-range contributors + `stacked_on_unapproved`**; serialize-before-park; M6 revert state
  machine; minimal integrator↔service API (plan-final §2.4b); cutover preflight requires **zero
  unresolved publication intents** (no legacy import).
- **Constitution** — human-owned rulesets (A: update/creation/deletion, bypass App + owner; B:
  non-fast-forward + required checks via Actions, no bypass; tag ruleset for `v*`), service switches,
  protected check ids (add-only for agents), admission rule. Rules and roster move to repo files read
  from the target, agent-editable with elevated review and post-hoc notification.

**Phase order (all four participants agreed):** P0 → P1 → P2 ∥ P3a → P4-min → P3b pilot (go/no-go,
5 real tasks, costed) → P6 dogfood (2 weeks). Execution is **hybrid**: direct interactive sessions for
P0, P1, P2 and the P4 cutover (they fix the workflow they would otherwise run through); dogfood through
the coordinator after P1 is deployed **only if the user approves U14**.

## 5. Start here (next session)

**TypeSafe prototype on `autonomy-plan` (2026-09-28): superseded.** The prototype commit `b4f4714` is not in `main`'s history, but its code was re-imported, rewritten and shipped as `main` `4a72018` (§11 rows dated 2026-09-29; production reranking is on). Leave `b4f4714` in place; no cherry-pick or revert is needed. The remaining hardening (secret file, concurrency cap, breaker) is lane A of the 2026-10-01 wave plan above.

**Out-of-band PR #3 (2026-09-28): merged and deployed.** [PR #3](https://github.com/marshallr12/agent_coordinator/pull/3) (`CHECKOUT_SYNC_INSTRUCTIONS`: at session start an agent runs `git pull --ff-only` on a clean tracking checkout, else tells the user) is on `main` `6239233` and live in production (binaries only, 2026-09-28; see §11). Branch new work from a freshly fetched `origin/main`.

**New global skill `coordinator-migrate` (2026-09-28, outside this repo):** `~/.claude/skills/coordinator-migrate/` and `~/.codex/skills/coordinator-migrate/`. It migrates a repo's Markdown memory (HANDOFF/BACKLOG/DURABLE-RECORD/SAVERS/archive/decisions-pending) into coordinator tasks (planned; next-items ready), knowledge and decisions, archives the files to `docs/pre-coordinator/`, and writes a root `AGENTS.md` coordinator section with context guards (selective `/api/v1/info` reads, `context --budget 8192`). Tested read-only against SithBit (preflight, inventory, validate, `apply --dry-run`); **no real migration has run** and the SithBit project is still empty.

**Overrides:** for P0–P2 and the P4 cutover, AGENTS.md's "automatically select and claim eligible
work" and CONTRIBUTING's "record current task progress … in the service" are overridden by the user's
instruction to work outside the coordinator. Do not create or claim coordinator tasks. P0–P1 need no
LLM spend beyond the interactive session.

1. Read, in order: this file → `planning/autonomy/plan-final.md` (§2.2, §2.2-P1 and §6 in full) → `planning/autonomy/review/service-data.md` §3 →
   transcript rounds 2–4 and 6–8. Hari's round-2 launch lines are superseded in part: launch tokens
   are [F] (two uids with static credentials instead), `--add-dir <git-common-dir>` is replaced by
   per-launch clones, and `--setting-sources user` does **not** stop `CLAUDE.md` discovery.
2. **P0 with the user (~15 min).** The SessionStart hook will show `[decisions] 15 pending`. Put
   U0–U14 (§6; U15 is answered) with AskUserQuestion, recommendation first (≤4 per call; ask U0, U9, U14 first).
   Record each answer in §6's Answer column, then delete that entry from
   `.claude/decisions-pending.md` (never commit that file).
   - **On U0 = yes**, show the user this payload, then run it:
     ```
     gh api -X POST repos/marshallr12/agent_coordinator/rulesets --input - <<'JSON'
     {"name":"day0-main","target":"branch","enforcement":"active",
      "conditions":{"ref_name":{"include":["~DEFAULT_BRANCH"],"exclude":[]}},
      "rules":[{"type":"deletion"},{"type":"non_fast_forward"}],"bypass_actors":[]}
     JSON
     gh api repos/marshallr12/agent_coordinator/rules/branches/main --jq '[.[].type]|sort'
     # expect ["deletion","non_fast_forward"]; NEVER verify by force-pushing main
     ```
     This is safe with today's LLM publisher (every guarded publish is a fast-forward). Do **not** add
     `required_status_checks` or `update` yet — they break that publisher until the P4 integrator.
   - **The user** runs the U4 ruleset test on a throwaway public repo (create a ruleset with `update`,
     bypass = a test App; confirm a plain collaborator push is rejected). Do not create repos or Apps.
   - **The user, in the dashboard** (task lifecycle is human-only until P1): cancel sentinel 5655e94d
     with a rationale, and decide dea68719 (deploy archived task pagination) — deploy it (a U9-class
     decision) or cancel it. Leave `/tmp/ac-deployment-worktrees/` alone until they decide; afterwards
     remove those worktrees without force.
3. **P1** on branch `autonomy/p1` created from **`main`** in a fresh worktree; `CARGO_TARGET_DIR`
   **inside** the worktree as CONTRIBUTING.md requires (git-ignored); never `git add -A` (stage explicit
   paths; `planning/` must not enter this branch). Follow the P1 row, §2.2 and **§2.2-P1 (which wins
   on any conflict)** of `planning/autonomy/plan-final.md`. Suggested order (Casey, adjusted):
   labelled gates → digest pin + satisfaction order → `reconcile_required_reviews` → agent
   unblock/cancel → extended `reopen`/`revise` → ack/decision keys → `ac_amendment` → roster captured on
   the intent → CLI subcommands → reopen prose (guidance strings; `grep -rl reopen book/src` finds 13
   files, 4 saying "operator reopen").
   One regression test per fix in `crates/server/tests`. After each step run the exact CI gate:
   `cargo fmt --all -- --check`; `cargo clippy --workspace --all-targets --locked -- -D warnings`;
   `cargo test --workspace --locked`; `cargo build --workspace --locked`; `python3 scripts/smoke.py`;
   `python3 scripts/backup_smoke.py`; for doc changes `python3 scripts/check_docs.py --mdbook
   "$(command -v mdbook)"` (CI pins Rust 1.98.1, mdBook 0.5.4). Checkpoint commit per step.
4. **Tests and staging.** B2 (conflict) and B8 (legacy null ref) are covered in `crates/server/tests`.
   **B1** (lease edit, `review_mode` change) is replayed on a staging coordinator on this host (mxmini).
   **Never let staging inherit the production token** — the CLI's origin comes from the repo binding and
   `AGENT_COORDINATOR_TOKEN`/`AGENT_COORDINATOR_ORIGIN` (production) are exported in this environment:
   ```
   env -u AGENT_COORDINATOR_TOKEN -u AGENT_COORDINATOR_ORIGIN -u AGENT_COORDINATOR_MCP_TOKEN \
     AGENT_COORDINATOR_HOME=<staging>/home AGENT_COORDINATOR_STATE_DIR=<staging>/state \
     AGENT_COORDINATOR_REPO_CONFIG=<staging>/binding.toml AGENT_COORDINATOR_ALLOW_INSECURE_LOOPBACK=true \
     agent-coordinator …
   # binding.toml: service_url = "http://127.0.0.1:<port>"
   # server: COORDINATOR_DATABASE=<staging>/db COORDINATOR_LISTEN=127.0.0.1:<port> COORDINATOR_ALLOW_INSECURE_LOOPBACK=true
   # check first: env | grep -c AGENT_COORDINATOR_TOKEN  (inside the wrapper) must print 0
   ```
   Staging Git remote, if needed: a local bare repo under `<staging>/remote.git` (see
   `scripts/completion_smoke.py:21`). Never create GitHub repos.
5. **Review and ship P1** (the `ship` script arrives in P2). **Every push needs the user's go-ahead at
   that moment** (public repo). Push `autonomy/p1`; wait for *Coordination checks* and *Documentation
   checks* green on that SHA (`gh run watch`); run a **fresh-context subagent review** of the final diff
   and get **the user's explicit OK** (U2 applies to agent-authored guard code); have the user run the
   read-only **preflight** (plan-final §2.2-P1; all counts 0) **before** `git push origin <sha>:main`
   (must fast-forward), and again before the production deploy, which is the user's U9 decision.
6. **Record progress** in §11 Execution log below (phase, step, commit SHA, gate result).

## 6. User decisions pending (recommendation first)

Also parked in `.claude/decisions-pending.md` (uncommitted) so the SessionStart hook surfaces them.
Record answers here.

| # | Decision | Recommendation | Answer (date, choice, note) |
|---|---|---|---|
| U0 | Apply the day-0 ruleset now (`non_fast_forward` + `deletion` on `main`; binds everyone incl. the owner; no bypass) | Yes || 2026-09-25: **yes** (apply now) |
| U1 | Windows: required producer or non-blocking Actions observer | Observer || 2026-09-25: **observer** |
| U2 | Human changes: FF pushes of green SHAs via `ship` under rulesets; pre-merge review for agent paths; post-hoc review only for landings with agent trailers/unknown provenance | Yes || 2026-09-25: **yes** (`ship` + rulesets) |
| U3 | Billing for the unattended pool | API billing (also enables Claude `--bare`); ceiling after the pilot ($5–30/task est.). If subscription: P2 must prove another way to stop candidate `CLAUDE.md` loading, or the pilot runs Codex-only || 2026-09-25: **subscription** (against recommendation) ⇒ P2 must prove candidate `CLAUDE.md` does not load, else pilot is Codex-only |
| U4 | Move repo to a GitHub org | No, unless the ruleset test fails || 2026-09-25: **no**, unless the ruleset test fails; 2026-09-29: **ruleset test PASS** (ordinary-login push rejected; App-token push succeeds on personal public test repo) |
| U5 | Review independence default | `distinct_launch` (separate reviewer uid, landing-range contributors); `distinct_vendor` where both vendors configured || 2026-09-25: **`distinct_launch`** |
| U6 | Hosts | oracle-1 primary supervisor, mxmini secondary (quiet hours), MINIAIR opportunistic, integrator on the e2-micro || 2026-09-25: **as recommended** |
| U7 | GitHub Actions receipts as required default | Yes (native-producer receipts kept as escape hatch) || 2026-09-25: **yes** |
| U8 | Agent rule edits | Autonomous above the constitution with elevated review + post-hoc notification || 2026-09-25: **as recommended** |
| U9 | Coordinator production deploys (incl. dea68719 and the P1 deploy) | Reserved decisions until dogfood data exists || 2026-09-25: **agent-deployable after the preflight** (against recommendation) ⇒ plan §0 Reserved and §2.7 need updating; P1 deploy still needs the preflight at 0 |
| U10 | `requires_human_acceptance` blocks done? | No; non-blocking follow-up || 2026-09-25: **no** (non-blocking follow-up) |
| U11 | Probe spend (~10 prompts, <$1, in P2) and 5-task pilot budget | Approve || 2026-09-25: **approve** (probes + 5-task pilot) |
| U12 | Admission rule for agent-originated work | Always admit reverts/fix-target/de-flake/refusal fixes + 5 agent tasks per week || 2026-09-25: **as recommended** (fixes + 5/week) |
| U13 | Notification channel for digest and pages | User's choice (ntfy/email) || 2026-09-25: **ntfy pages + email digest** |
| U14 | After P1 is deployed, may autonomy work run as coordinator tasks (dogfooding), or stay outside the workflow as originally instructed? | Dogfood || 2026-09-25: **dogfood** after P1 deploy |
| U16 | Integrator host (VM is IPv6-only; GitHub is IPv4-only; see `planning/autonomy/p4-design.md` §0) | oracle-1, uid `agentc-integrator`, integrator attestation + ancestry audit | 2026-09-27: **oracle-1** (supersedes U6's integrator placement) |
| U15 | Commit `planning/autonomy/` on `autonomy-plan` after a redaction pass, or keep it local (repo is public) | Commit after redaction | 2026-09-25: **commit and push** `autonomy-plan` to origin; redaction pass done (private repo name, home paths) |

**Impact of answers that differ from the recommendation (2026-09-25):**
- **U3 = subscription:** `--bare` is unavailable, so P2 must prove (U11 probes) that a candidate's
  `CLAUDE.md`/`.claude/` never loads for Claude launches; if it cannot, the pilot runs Codex-only.
- **U9 = agent-deployable after the preflight:** production deploys are no longer a reserved
  decision. Plan-final §0 "Reserved" and §2.7 are amended by this answer: a deploy may proceed
  without the user once the §2.2-P1 preflight shows all counts 0 (and, from P4, the release's
  required checks are green). Guard-code releases and non-expand migrations still need the
  preflight; the fenced deploy gate stays [F]. For the P1 deploy, §5 step 5's "user's U9
  decision" becomes "agent deploy after a clean preflight".

## 7. Sign-off and risk register

Round 5: Dana, Sol and Hari each **AGREE WITH RESERVATIONS**; all reservations were applied in
`planning/autonomy/plan-final.md` (marked **(final)**): host updater, admission control and per-host canaries moved to
P6 (pilot = core only; P3b realistically 2–3k lines); labelled gates and `fix-target` go to the digest;
decisions are U0–U15 everywhere; trailer detection is best-effort, not a security control;
service-verifiable recovery evidence and "checkpoint SHA is authoritative, refs are transport" restored
as [R-P3]; legacy-intent import replaced by a zero-unresolved-intents cutover preflight; the P1-only
human gate (check failure after intent) listed explicitly; Git isolation between role uids [R-P2]; the
day-0 ruleset is verified by API read-back, **never by force-pushing production `main`**.

| # | Risk (raised by) | Mitigation |
|---|---|---|
| K1 | User stops reading the digest; HRI reads 0 while work and decisions pile up (Dana) | Digest-neglect + value-canary paging |
| K2 | Publication proof depends on ruleset B + single producer, enforced outside our code (Sol) | Ruleset watchdog freezes queue and pages; flake attribution removes the pressure to disable B |
| K3 | Autonomy machinery becomes the user's new operational chore (Hari) | Pilot core only; P6 deferrals; [F] rule; go/no-go judges operating cost |
| K4 | Scope re-growth (lead, after Dana) | [R]/[F] tags; deletions per component; measured exit criteria |
| K5 | Staging commands reaching production: the production token and origin are exported in the executing environment (Robin, round 8) | §5 step 4 staging wrapper unsets both and checks the token count is 0 |

## 8. Discussion history (condensed; verbatim in the transcript)

- **Round 1 (16:27–16:33).** Lead posted plan-v1 (deterministic integrator, supervisor daemon,
  policy-gated agent paths for every human-only transition, fake-agent soak rig as P0). **Dana**:
  metric measures the wrong ledger (chat attention), "done" is the wrong terminal state, not every click
  was authority-only, the problem is shrinking (0 reconciliations after 3590591f), P1 is small and should
  ship first, GitHub Actions already runs Linux + Windows CI, circularity of guards agents can change,
  deployment over-scoped, delete before adding. **Sol**: pinned result + roll-forward + I-FF replaces
  intents/journals/holds; trusted observer; `revise` state machine; scoped pinning; progress ≠
  liveness; content-keyed receipts; delegated config is the biggest escalation risk (roster from
  target, rules are a prompt-injection channel, constitution floor, no self-benefit). **Hari**:
  classifier soft-deny rules (Merge Without Review, Self-Approval, CI Bypass, Production Deploy,
  Self-Modification) block exactly what policy grants, so supervised sessions need deterministic
  containment; unattended sessions must not run as the user (current Codex `danger-full-access`, bare
  `git push` rule); cost is input-token driven ($50 for one stuck task); staging, shadow mode, kill
  switch, push notifications; the deterministic integrator also fixes D6.
- **Round 2 (16:34–16:49).** Lead adopted goal/metric/triage/sequencing/publication/pinning/repo
  rules/Actions receipts/containment. Sol: five *required* integrator changes vs cleanup; asymmetric
  trust (false "published" loses a change ⇒ service verifies ancestry via compare API); PoC. Hari:
  personal repos can enforce integrator-only push via ruleset `update` + App bypass (no org needed);
  direct pushes under required checks need green SHAs ⇒ `ship` replaces the `quick` lane; exact launch
  lines; candidate-controlled `.claude/`/`AGENTS.md` hazard; task-scoped token design. Dana: P1-lite
  PoC measured; circularity is not self-hosting-only (repo roster + Actions running R's workflows);
  every coordinator deploy is effectively a human decision; direct-push canary; pre-merge review (median
  3.9 min).
- **Round 3 (16:51–16:56).** Replays of B1–B15 and history §3 against v2. Sol: satisfaction order,
  serialize-before-park, revise-loses-to-push, result-branch protection, host-advertised resources,
  B5 recovery facts, `attach_candidate_ref`, fencing gate (Q-B), migration preflight (Q-C). Hari:
  silent settings invalidation in `-p`, harness auto-update pinning, Playwright, review slot,
  claim-churn breaker, branch-creation restriction, integrator on the e2-micro, ruleset timing trap
  (day-0 only `non_fast_forward` + `deletion`), phase re-order. Dana: threat model statement, minimum
  protected set incl. **privilege-bearing CI**, keep the self-related-AC guard, M1–M6 gaps, cuts
  (launch tokens deferred to two uids; MCP frozen; AC lint → reviewer rule + U10), tag every FIX as
  [R]/[F]. All four agreed the phase order with a minimal integrator before the pilot.
- **Round 4 (16:58–17:01), pre-mortem.** Hari: silent 31 h stall with muted notifications ⇒ end-to-end
  canary + SLO paging + digest + stall-as-HRI; host updater; hybrid execution. Dana: agents fill the
  queue with their own work and the user loses control ⇒ admission control, value canary, release
  packet; generic-project walk (bootstrap commands, project `setup`, host-owned egress, self-hosting-only
  items); demotions (distinct_vendor default, cooling delay, lease extension). Sol: flake amplification
  ("the queue that ate itself") ⇒ reproducible attribution + flip-rate gate; **self-review by
  stacking** and **silent no-op re-land after revert** (both [R]); M6 state machine; `unreviewed_landing`
  by agent trailers. All three agreed the pre-mortems are one failure ⇒ **attention budget**.
- **Round 5 (17:04–17:06), sign-off.** All three AGREE WITH RESERVATIONS (applied; see §7).
- **Round 6 (17:08–17:12), cold-read audit.** A fresh agent, **Casey**, given only this handoff, found
  that P0 would fail as written (task lifecycle is human-only; no decision covered the ruleset) and that
  landing-range contributors could not be built in P1 (no checkpoint SHA column, no Git in the service).
  Dana, Sol and Hari audited both documents: stale HANDOFF lines, superseded `--add-dir` text, roster
  "at push time" would strand published work (capture on the intent instead), a second P1-only human
  gate, a P1 deploy preflight, exact gate commands and ruleset payload, credential `class`/`access`
  attributes, U0/U14/U15, thresholds, redaction.
- **Round 7 (17:12–17:14), rulings.** The lead ruled on every open item (5a → [R-P4] integrator-enforced;
  extend `reopen` rather than add an endpoint; P1 rate limit only; digest pin; `sha256(rules)` ack key;
  `reconcile_required_reviews`; CLI subcommands; branch hygiene). **No objections from any
  participant.** Consensus reached.
- **Round 8 (17:15–17:23), verification.** A second fresh cold reader, **Robin**, confirmed Casey's
  gaps were closed and P0 runs as written, and found: an **unsafe staging CLI line** (it would have mixed
  the production token with staging), the preflight must also run before the `main` push,
  `reconcile_required_reviews` must skip integrating subjects, the attempt pins must move to the digest,
  a 13-not-10 book-file count, and twelve open questions. Dana, Sol and Hari verified the rulings,
  answered every question (actors per reason, park = labelled refusal, satisfaction by approver class,
  stored `task_digest` (lead ruling over compute-on-read), decision scope, preflight SQL, pre-merge
  review of P1 by a fresh-context subagent + the user's OK, `CARGO_TARGET_DIR` inside the worktree per
  CONTRIBUTING). All applied as **(r8)** in plan-final §2.2-P1 and here. **Discussion closed at
  17:26 UTC with consensus.**

- **Round 9 (17:25–17:26), closing last words to the user.** Dana: approve P1 alone first (~600
  lines); treat everything after it as a separate bet gated by the costed pilot and your go/no-go.
  Hari: never run unattended agents under your own account (global Codex full-access/no-approval, and
  the production token exported in your shell) until P2 containment passes. Sol: if flaky checks tempt
  you to disable ruleset B, fix or quarantine the check instead — disabling B silently voids every
  published/not-published guarantee.

## 9. Reference index

- `planning/autonomy/plan-final.md` — the plan (authoritative).
- `planning/autonomy/plan-v1.md` … `plan-v4.md` — revision history.
- `planning/autonomy/discussion-transcript.md` — verbatim group discussion.
- `planning/autonomy/review/service-data.md` — live-service evidence, B1–B15 catalog, human-intervention
  inventory, task timeline.
- `planning/autonomy/review/codebase.md` — per-capability traces with file:line gates, instruction
  surface counts, docs-vs-behavior gaps.
- `planning/autonomy/review/history.md` — git + Codex/Claude prompt history, intervention frequencies,
  harness constraints, complexity growth.
- `planning/autonomy/tools/` — read-only fetch scripts and the transcript `post.sh` helper.
- Key code: `crates/server/src/workflow.rs` (reopen `:1117`, pinning `:846-857`/`:1430-1447`,
  integration `:2207`/`:2378`/`:2615-2790`), `coordination.rs` (policy PATCH `:264-330`, unblock
  `:1134`, self-related guard `:1093`), `jobs.rs:267` (resources), `crates/local/src/git_workflow.rs`
  (deterministic merge `:884-958`, CAS publish `:708-854`), `crates/cli/src/main.rs:2602` (local
  intent journal), `DURABLE-RECORD.md`, `CONTRIBUTING.md`, `book/src/docs/agent-startup.md`.

## 10. Rules for the executing session

- Follow CONTRIBUTING.md and the user's global preferences (short functions under ~20 lines,
  comment functions, in-code defaults so a new instance runs with no config, mdBook external links via
  a global `additional-js` hook, never the word "knob").
- Do not deploy production, change rulesets, create GitHub Apps, or spend on LLM probes without the
  user's recorded decision (§6).
- `planning/autonomy/` is committed only on `autonomy-plan`, after a redaction pass (private repo names,
  hostnames and VM details not already in HANDOFF-archive.md) and the user's U15 answer; never on `autonomy/p1`.
- Never print tokens, proofs or passwords; the service bearer token is in the environment.
- The attention budget and the measured-failure rule are the guard against re-growing scope: an item
  tagged [F] is built only when a recorded failure justifies it.

## 11. Execution log

| Date | Phase / step | Commit | Gate | Notes |
|---|---|---|---|---|
| 2026-09-25 | Planning complete | committed and pushed on `autonomy-plan` | n/a | Handoff, plan-final, transcript, reviews, tools |
| 2026-09-25 | P0: decisions U0–U14 | — | n/a | All answered (see §6) |
| 2026-09-25 | P0: day-0 ruleset | — | read-back `["deletion","non_fast_forward"]` | Ruleset id 24031302 `day0-main` on `~DEFAULT_BRANCH`, no bypass |
| 2026-09-26 | P1 step 1: labelled human gates | `2658430` | server tests + clippy green | `details.required_actor`/`gate`; top-level code unchanged |
| 2026-09-26 | P1 steps 2–3: digest pin, satisfaction order, `reconcile_required_reviews`, roster on intent | `bb3331b` | full CI gate green | migration 0022 (task_digest, roster_revision) |
| 2026-09-26 | P1 step 4: agent unblock/cancel (+replacement) | `e8ac83f` | server tests + clippy green | |
| 2026-09-26 | P1 step 5: agent `revise` via `workflow/reopen` | `21def97` | full CI gate green | B2 + B8 regression tests |
| 2026-09-26 | P1 step 6: ack/decision keys on rules + judged fields | `68a1678` | full CI gate green | |
| 2026-09-26 | P1 step 7: `ac_amendment` | `0f062f5` | server tests + clippy green | 0022 also adds ac_amendment_json, amendment_decision |
| 2026-09-26 | P1 step 9: CLI `revise`/`unblock`/`cancel` | `b77d3e1` | CLI tests + clippy green | |
| 2026-09-26 | P1 step 10: docs + guidance, INSTRUCTION_VERSION 9 | `983c28f` | full CI gate + `check_docs.py` green (Rust 1.98.1, mdBook 0.5.4) | |
| 2026-09-26 | P1 staging replay (B1 lease edit, B1 review_mode change, revise, unblock, cancel) | `983c28f` | PASS; human events only project.created + policy.updated | disposable loopback coordinator, production env unset |
| 2026-09-26 | P1 fresh-context review + fixes (3 blockers, 5 should-fix, 1 nit) | `0a55699` | full CI gate + docs + staging replay green | agents barred from review/recovery mode edits; safe reconcile; leftover-review, roster, amendment, pin fixes; MCP cancel |
| 2026-09-26 | P1 step 5: push `autonomy/p1` to origin | `0a55699` | CI *Coordination checks* + *Documentation checks* green | Awaiting the user's explicit OK + preflight (all 0) before `main` FF; user pre-authorized pushes of non-`main` branches |
| 2026-09-26 | P0 cleanup: stale worktrees | — | n/a | Removed 8 clean worktrees (incl. `/tmp/ac-deployment-worktrees/*`); deleted the 2 merged deploy branches. Kept unmerged local branches `codex/task-pagination-g3`, `codex/integration-pagination-g3` (69c9f0d/b88bad9 "Reload task queue when page size changes in flight") and `task/redirect-loop-g2` (2 browser-startup test commits) |
| 2026-09-26 | P0: sentinel 5655e94d + dea68719 (user delegated the call) | — | n/a | Both cancel **after the P1 deploy** via agent `cancel` (first dogfood of the new path): 5655e94d = disposable eval sentinel, not needed; dea68719 = superseded because the P1 deploy ships `main`, which already contains e96b11a. `main` FF to 0a55699 pending: classifier blocked the agent push, user to run it |
| 2026-09-26 | P1: `main` fast-forward | `0a55699` | preflight 0/0/0/0 (user); user OK; `d72a8cb..0a55699` | Pushed with the user's explicit permission. Next: production deploy after a second clean preflight, then agent-cancel 5655e94d and dea68719 |
| 2026-09-26 | P1: CI on `main` | `0a55699` | *Coordination checks* + *Documentation checks* green (push) | Production still runs `d72a8cb` (0.1.1, instructions 8), which already contains e96b11a ⇒ dea68719 is moot. Release build + deploy blocked by the auto-mode classifier ("Production Deploy"); awaiting the user |
| 2026-09-26 | P1: production deploy (agent, per U9) | `0a55699` | preflight 0/0/0/0 (python `sqlite3` `mode=ro`; no sqlite3 CLI on VM); schema 21→22; `/healthz` ok; `/api/v1/info` commit 0a55699, instructions 9 | Release run 36240918820 (archive `5f9f8caa…`, server `bd395059…`, CLI `af5ac943…`, MCP `15868f11…`). SSH only via `--tunnel-through-iap`. Old-binary snapshot `20260926T130528.447Z-785e205b…` verified; rollback binaries `/usr/local/bin/*-0.1.1-d72a8cb`; new-binary backup + maintenance + GCS transfer OK; timers rescheduled; staging removed. Downtime ~1 min (13:05:28–13:06:21 UTC). No new GCP resources; $1 alert budget "agent-coordinator free-tier guard" added |
| 2026-09-26 | P1: workstation CLI | `0a55699` | `compatibility` all checks true | Release CLI needs glibc 2.38 (Ubuntu 24.04 build) ⇒ local locked build via `scripts/upgrade_client.py --source-root`; rollback `~/.local/bin/agent-coordinator.rollback` |
| 2026-09-26 | P0 leftovers | — | n/a | 5655e94d already canceled + archived (01:10 UTC); dea68719 already done. Nothing to cancel |
| 2026-09-26 | P2 start (outside the coordinator, per user) | branch `autonomy/p2` from `main` 0a55699, worktree `~/src/worktrees/agent-coordinator-p2` | n/a | P1 worktree removed (clean, merged) |
| 2026-09-26 | P2 step 1: credential `class`/`access` (§2.2 6b) | `845370d` | full CI gate + docs green | migration 0023; guard in `Mutation::begin` (read-only may only open/ack/close own session); `events.credential_class`; rotation inherits; dashboard selects |
| 2026-09-26 | P2 step 2: secret scan on candidate push | `873271c` | full gate + docs green | inside `checkpoint_candidate` (the CLI's only candidate push; no separate `push-candidate` verb needed); scans every outgoing commit patch; own token matched by digest |
| 2026-09-26 | P2 step 3: `scripts/ship.py` | `2cf7cc3` | local bare-repo test of FF/diverged/dirty paths | Actions wait untested until first real use |
| 2026-09-26 | P2 U11 probes (8 LLM calls: 5 Claude Haiku, 3 Codex low) | — | see notes | **Claude `--safe-mode`** (2.1.283) blocks candidate `CLAUDE.md` + project hooks under **subscription** auth while `--settings` permission rules still apply (`git push` denied) ⇒ U3=subscription P2 blocker resolved without `--bare`. Control run: candidate hook ran even in an untrusted dir. Claude sandbox active but `/tmp` writable ⇒ uid is primary containment. **Codex** `-c project_doc_max_bytes=0` suppresses `AGENTS.md`; candidate `.codex/config.toml` ignored (untrusted dir); `workspace-write` denies out-of-tree writes but **allows raw `git push`** ⇒ dead `remote.origin.pushurl` in clones now, ruleset A (P4) is the real guard |
| 2026-09-26 | P2 step 4: `crates/supervisor` (`agentc-supervisor`) | `493a82e` | full gate green | profiles (env fully replaced; egress via proxy env), generated role settings, hardened per-launch clones, preflight, reviewer verdict schema; `build.rs` provenance misses branch-ref moves in worktrees ⇒ gate sets `COORDINATOR_BUILD_COMMIT` on a clean tree |
| 2026-09-26 | P2 step 5: egress allowlist proxy (`agentc-supervisor egress-proxy`) | `52bb1d3` | crate tests + live test (github 200 via proxy, example.com 403) | loopback CONNECT-only :443; firewall will allow agent uids only this proxy, the staging port and loopback 32768-60999 (tests), not other loopback services (Samba, CUPS, rpcbind) |
| 2026-09-26 | P2 step 5b: toolchain/update pins, `egress_allow_extra`, `prepare` | `d02c684` | full gate green; `autonomy/p2` pushed | Codex silently accepts unknown `-c` keys ⇒ containment suite must test behaviour, not config parsing |
| 2026-09-26 | P2 step 6 BLOCKED: `deploy/agentc/host-setup.sh` | — | — | Auto-mode classifier refused writing the root setup script ("Unauthorized Persistence": system users, systemd units, boot-time nft table). Awaiting the user's decision; design in this session's transcript: uids agentc-impl/-rev/-egress, /opt/agentc (pinned claude/codex/CLI/supervisor + rustup toolchain), /var/lib/agentc/<role> 0700, root-owned mirror.git, /etc/agentc/{supervisor.toml,agentc.nft}, units agentc-firewall + agentc-egress, `--uninstall` |
| 2026-09-26 | P2 step 6: `deploy/agentc/host-setup.sh` + `containment-suite.sh` (user approved the file write) | `ae8ecda`, `56067d9` | `bash -n`; `codex sandbox` policy verified on owner uid; CI green on `d02c684` | Not yet run: **the user runs both with sudo** (agent never executes root scripts). System `safe.directory` = mirror path only. Release binaries built in the P2 worktree `target/release/` |
| 2026-09-26 | P2 step 6: first host-setup run (user) | `36d68cc` fix | partial | mxmini runs **sysvinit** (MX Linux), so `systemctl` failed after users/dirs/binaries/toolchain 1.98.1/mirror/config succeeded; setup now installs LSB init scripts when systemd is absent (proxy log `/var/log/agentc-egress.log`). Re-run pending |
| 2026-09-26 | P2 containment exit criterion (user ran setup + suite on mxmini) | `51acdc1` | **containment suite: all 40 checks PASS** incl. `cargo test --workspace` as agentc-impl under uid+firewall and inside the Codex sandbox | Fixes found by the runs: sysvinit services (`36d68cc`), suite keeps cargo logs (`5a7f926`), clones get the canonical `origin` URL via `clone --origin-url` (build provenance test needed `https://`, `51acdc1`). Harness logins and supervised credentials not yet done (needed for P3b, not P2) |
| 2026-09-26 | P2: staging coordinator + UI verification (M2) | `a6481dd`, `ff286ec` | full CI gate green (smoke + backup smoke on a clean committed build); e2e: supervisor `prepare` → `$RUN/verification.json` → `scripts/verify_ui.mjs` signed in as `staging-verifier` and saved screenshot + DOM | `deploy/agentc/staging.py` (up/status/cli/credentials/down/destroy; env stripped of all `AGENT_COORDINATOR_*`/`COORDINATOR_*`); supervisor `[verification.<project>]` + `--project`, reviewer-only login at `/var/lib/agentc/rev/verification/<project>.json`, preflight checks login 0600 + root-owned browser; host-setup pins `node`, detects the browser, keeps entries below a KEEP marker; suite gains a reviewer browser check. Playwright replaced by the repo's existing CDP-over-Chrome pattern (no npm dependency, no registry egress). Chrome aborts if `$TMPDIR` is deeper than the Unix-socket limit (~108 bytes); `$RUN/tmp` is short enough |
| 2026-09-26 | P2: staging running on mxmini | `ff286ec` | owner/impl/rev `connect` ok; impl creates a task; rev refused `operation_not_permitted`, can list | `127.0.0.1:18080`, state `~/.local/state/agentc-staging`, project `1dad5306-cbaf-4e6c-a7ed-20b9f369362d`, server = worktree debug build (pid in `server.pid`; `staging.py down` stops it) |
| 2026-09-26 | P2: release build + staging on it | `cce0342` (binaries from `ff286ec`) | release build ok; staging `compatibility` all true | Found and fixed a `staging.py` bug: `down` matched the pid by binary path, so after the first release build it ignored the running debug server and `up` reported a bind-failed server as healthy. Now matched by database path; `up` refuses a foreign listener. Awaiting the user's sudo steps (step 2 below) |
| 2026-09-26 | **P2 exit**: user re-ran host-setup (release `ff286ec` binaries, node, browser, verification dirs), staging credentials, verification entry, suite | `cce0342` | **containment suite: all checks PASS** incl. reviewer headless browser on staging, cargo test under uid+firewall and inside the Codex sandbox; CI green on `cce0342` | Next: fresh-context review of `origin/main..autonomy/p2` (running), user's OK, preflight, FF `main`, deploy migration 0023 |
| 2026-09-26 | P2 fresh-context review + fixes | `8d2dafa` `81bd199` `a34944d` `1d5bc3d` | full CI gate green (299 tests, smoke, backup smoke, docs) | Fixed: root symlink takeover in host-setup (blocker), secret-scan gaps (merges, binary, names, config, forged refs), DevTools port reachable cross-uid (now pipe), re-issued credential widening read→write, preflight firewall/proxy probes + proxy refuses local addresses. Deferred items below are [R-P3b] |
| 2026-09-26 | P2: `main` fast-forward | `d77c328` | user re-ran host-setup + suite on `1d5bc3d` (all PASS); preflight 0/0/0/0 (user); CI green on `d77c328` (Windows fixture fix after `1d5bc3d` failed natively); `0a55699..d77c328` | Pushed with the user's explicit permission. Release dispatch + deploy of migration 0023 blocked by the auto-mode classifier ("Production Deploy"); awaiting the user |
| 2026-09-26 | P2: production deploy (agent, per U9; user added Bash rules for `gh workflow run`, `gcloud compute ssh agent-coordinator`, `gcloud compute scp`) | `d77c328` | preflight 0/0/0/0 before and after service stop; schema 22→23; `/healthz` ok; public `/api/v1/info` commit d77c328, instructions 9 | Release run 36286387635. Old-binary snapshot `20260927T022547.825Z-63ea9b0e…` verified; new-binary snapshot `20260927T022606.188Z-a771c0ce…` verified; maintenance ok; timers restarted. Rollback binaries `/usr/local/bin/*-0.1.1-0a55699`. Downtime ~5 s (02:25:59–02:26:04 UTC) |
| 2026-09-27 | P2: workstation CLI | `d77c328` | `compatibility` true | `scripts/upgrade_client.py --source-root` from the P2 worktree; rollback `~/.local/bin/agent-coordinator.rollback` |
| 2026-09-27 | P3a step 1: `GET /api/v1/projects/{p}/next?role=implementer\|reviewer` | `4344fd2` | server tests + clippy green | Read-only (read-access credentials may call it); reuses `task_preconditions_snapshot` / `activity_preconditions`; returns one action + exact call template + CLI line, `caller_steps` (instruction ack), `human_queue`, `skipped`. Recovery before new work; reviews by subject priority; independence judged against the caller |
| 2026-09-27 | P3a step 2: `agentc-supervisor shadow` / `shadow-report` + review fixes | `4e71f81` | full CI gate green (309 tests, smoke, backup smoke, docs); disposable staging on :18090 with the read-only rev credential logged an implementer would-launch ($3.92 est.) and reviewer idle | JSONL would-launch log deduped per (project, role); report counts distinct targets. Cost = token profile per role × price (API-equivalent; U3 is subscription); defaults Opus 5.5 impl 4M in/90% cached/80k out, rev 1.2M/20k; `[shadow]` keys are per-key overrides. Fresh-context review: fixed readiness starvation in `next` (regression test), partial-table defaults, report over-count, code-review `--candidate-checkout` hint, archived/recovery-only review candidates, non-fatal log writes. Accepted as-is: `human_queue` counts only inspected candidates; up to ~1k queries per call (fine at shadow cadence). `autonomy/p3a` pushed |
| 2026-09-27 | P3a: `main` fast-forward | `4e71f81` | CI green on `autonomy/p3a`; preflight 0/0/0/0 (user; classifier blocks agent production reads); user OK; `d77c328..4e71f81` | Next: production deploy (binaries only, no migration) after a second clean preflight, then shadow install on mxmini |
| 2026-09-27 | P3a: production deploy (agent, per U9) | `4e71f81` | preflight 0/0/0/0 before and after service stop (agent, exact-command rule); `/healthz` ok; public `/api/v1/info` `source_commit` 4e71f81, not dirty, instructions 9; no migration | Release run 36294497323 (all 3 jobs green; server `2f9bb1fb…`, CLI `952932d1…`, MCP `15868f11…`). Old-binary snapshot `20260927T051119.687Z-2feb25e1…` verified; new-binary snapshot `20260927T051200.901Z-73347980…` verified; maintenance complete; transfer ok; timers restarted. Rollback binaries `/usr/local/bin/*-0.1.1-d77c328`. Downtime ~31 s (05:11:29–05:12:00 UTC). Host scripts: `~/.local/share/agent-coordinator-autonomy/deploy-{pre,swap}.sh` (outside the repo; `REL=~/release-4e71f81` on the VM) |
| 2026-09-27 | P3a: workstation CLI | `4e71f81` | `upgrade_client.py` installed and verified 4e71f81 | `--source-root` = P3a worktree; rollback `~/.local/bin/agent-coordinator.rollback` |
| 2026-09-27 | **P3a shadow started** (05:12 UTC / 01:12 EDT) | `4e71f81` | first poll: 1 project, implementer + reviewer idle, 0 errors | Runs as `marshall` from `~/src/worktrees/agent-coordinator-p3a/target/release/agentc-supervisor shadow` (setsid nohup via `~/.local/share/agent-coordinator-autonomy/start-shadow.sh`); credential `/etc/agentc/shadow-credentials.toml` (user-issued, principal `agentc`, supervised/read, origin agents.sithbit.com, 0600 marshall); log `/var/lib/agentc/shadow/would-launch.jsonl`, stdout `shadow.out`. Does not survive a reboot. **Do not remove the P3a worktree while it runs** |
| 2026-09-27 | Release workflow: 5-minute load run for binaries-only deploys | `7c5079d` | branch CI green; test dispatch 36294875176 green in 14m48s (`Load workload: 300s`); `main` FF `4e71f81..7c5079d` with the user's explicit OK | `gh workflow run release.yml --ref main -f binaries_only_since=<production commit>`; fails if that commit is not an ancestor or any migration changed since; tags, PRs and runs without the input keep 30 min. Production stays on 4e71f81 (diff is release.yml + one book paragraph; the served docs lag until the next deploy) |
| 2026-09-27 | P4 design pass | autonomy-plan | n/a | `planning/autonomy/p4-design.md`: GitHub has no IPv6 ⇒ integrator on oracle-1 (U16), service trusts integrator attestation (no outbound GitHub from the VM); job-level required checks (U1); new crate `crates/integrator`; 6 build steps S1–S6; user steps listed (U4 test, GitHub App, host setup, credential, rulesets at cutover). Worktree `~/src/worktrees/agent-coordinator-p4`, branch `autonomy/p4` from `main` 7c5079d. Plan's `3590591f` = commit `1ba7d7a` |
| 2026-09-28 | Out-of-band TypeSafe context reranking prototype | `autonomy-plan` commit titled `Prototype optional TypeSafe context reranking with SQLite fallback` | `cargo fmt --all`, `cargo check -p coordinator-server`, and workspace Clippy with warnings denied passed; no tests or live TypeSafe call | The server optionally scores up to 40 already selected context items after releasing the SQLite read transaction. Missing key, timeout, HTTP error, invalid/incomplete scores, or excess candidates retain the original order; decisions and item budget are unchanged. The user added a nonempty key entry to `/etc/agent-coordinator/service.env` on the production VM; no value was recorded. **Not on `main`, not deployed:** a future session must decide to incorporate or revert this branch commit, then build and deploy a new product release if incorporated. The VM's requested reboot and live API behavior were not verified in this session. |
| 2026-09-28 | **P3a exit** (checked 15:47 UTC) | `4e71f81` | `shadow-report`: 12 records, 0 errors, 0 would-launch, max human queue 0 | Poller was **dead** (mxmini booted 2026-09-28 ~00:04 UTC; not reboot-safe). Last record 2026-09-27 17:13 UTC; the log is change-only (identical idle records only on process start), so liveness is proven only for 05:12–17:13 UTC (~12 h, span reported 12.0 h), not the planned 24 h. Production had no claimable work throughout. Exit accepted as "poller, credential and `next` endpoint proven; no load data". Restarted 15:48 UTC via `start-shadow.sh` (pid 43458); it now sees a second project `f1cb5dfe…`, also idle. **[R-P3b] gap:** the shadow/supervisor needs a periodic heartbeat record (or `shadow.out` line) and reboot-safe start (init script on sysvinit) so a dead poller is detectable |
| 2026-09-28 | **P4 S1**: integrator service surface (outside the coordinator, per user) | `65e4f58` on `autonomy/p4` (pushed) | full CI gate green (fmt, clippy, 314 tests, build; smoke + backup smoke + `check_docs.py` on the clean committed build) | Migration 0024: `credentials.class`/`events.credential_class` gain `integrator` (both tables rebuilt, rowids and the events sequence preserved, FK check); `projects.integration_owner` (`agent` default, human-only via policy PATCH) + `integrator_last_seen`; `integrator_results` UNIQUE(submission, t0); `integrator_receipts` PK(result, check, run, attempt). Routes (integrator-owned projects, integrator class only; the class may call nothing else, not even sessions): `GET …/integrator/queue` (approved, pins-current code subjects by priority then time in integration, with existing results + current roster; heartbeat), `POST …/integrator/results` (idempotent; `result_conflict`, `candidate_changed`), `POST …/integrator/receipts` (`receipt_head_mismatch`, `receipt_conflict`; latest attempt of latest run decides). Dashboard can issue integrator credentials; api-contract + operator-access docs. Not yet: `next?role=integrator`, refusal of LLM routes on integrator projects (S2). Old either-review migration test needed an explicit `projects` column list |
| 2026-09-28 | **P4 S2**: push authority, observations, integrator revise, LLM-route refusal, `next?role=integrator` (outside the coordinator, per user) | `1e6b695` on `autonomy/p4` (pushed) | full CI gate green (fmt, clippy, all workspace tests, build; smoke + backup smoke + `check_docs.py` on the clean committed build) | 0024 extended in place (not deployed anywhere): `integrator_results` authority + `contributor_tasks_json`, `integrator_observations`, `integrator_revise_requests`, `review_decisions.invalidated_at`. **Deviations from p4-design:** the submission-bound hold reuses `integration_holds` on the submission's own integration activity (one per submission) instead of a new `submission_id` column; roster shape is `{required_checks:[{identity, check_name, workflow_path, workflow_blob}]}`, validated at result time; human reopens apply immediately (only agent revises defer), a later `contained` observation is recorded as `published_after_reopen`; approver-in-contributors voids the approval, queues a replacement review and returns `granted:false` (committed) rather than an error; contributors = other tasks whose submission candidate commit or stored landing range shares a commit with the range. Deferred to S4: `candidate_reverted_in_history` (needs the revert kind), serialize-before-park. New modules `integrator_authority.rs`, `integrator_observe.rs`; `reopen` core factored into `workflow::supersede_submission` |
| 2026-09-28 | Out-of-band: checkout-sync startup instruction + `coordinator-migrate` skill (outside the coordinator, per user) | `6239233` on `startup-checkout-sync` (PR #3, pushed) | fmt, clippy `--locked -D warnings`, `cargo test --workspace --locked`, `check_docs.py --mdbook` green after rebasing onto `origin/main` | First cut from stale local `main` `d72a8cb`; rebased and force-pushed with lease. Not merged, not deployed: ship with the next release deploy. Skill tested read-only on SithBit; no records created |
| 2026-09-28 | **P4 S3**: `crates/integrator` (`agentc-integrator`), outside the coordinator per user | `34d8bbe` on `autonomy/p4` (pushed) | full CI gate green (fmt, clippy, 350 workspace tests, build; smoke + backup smoke + `check_docs.py` on the clean committed build); 29 crate tests incl. 7 end-to-end cycles (real local bare remote, fake checks, in-process mock of the integrator routes with idempotency replay) | Loop: queue → watchdog (branch rules) → X via ls-remote, sticky freeze when X stops descending from the last tip (keyed by URL+branch) → observe any result of the submission still holding authority first → roster from `.agent-coordinator/roster.toml` at X (must cover the service identities; blobs from X) → conflict ⇒ `revise conflict` → R via `git_workflow::prepare_integration` (a reused service result must be reproduced exactly) → privilege pre-check (R changes a roster workflow ⇒ Blocked, skipped) → R create-only to `ac/results/<id>` → one checks poll per cycle (keeps the queue heartbeat) → receipts → failure ⇒ `revise check_failed` → push authority → `publish_prepared` with lease X and a callback that checks R/T0 → **always** an observation after a grant; local intent/worktree discarded after every observation (fixes PushIntent stuck ⇒ `Uncertain`). App JWT signed with `aws-lc-rs` + `rustls-pki-types` (no new lock packages; `jsonwebtoken` would add ~10); askpass = the binary itself (marker env `AGENTC_INTEGRATOR_ASKPASS_CONFIG`, exact host `github.com`, mints a token per Git prompt). Idempotency keys = body digests (+ grant expiry for observations). Service change: queue results carry `authority_expires_at`. Default `checks = "fake"` (no fake file ⇒ nothing publishes). Fresh-context review found 4 blockers (receipt key collision, stuck held authority, replayed observation key, sticky PushIntent) + should-fixes; all fixed with tests. Known flake: `agentc-supervisor` `reachable_target_is_a_firewall_problem` (closed-port race), passed on re-run. **Deferred to S4+:** labelled privilege decision (now just Blocked+skip), flake reruns, tip-move reports, serialize-before-park, revert kind, sweeping intents of old X, unrelated-history/base-not-ancestor ⇒ revise (now an error), token caching across askpass calls. **S6 needs** a real `.agent-coordinator/roster.toml` on `main` mapping the production policy's identities to the three job names |
| 2026-09-28 | PR #3 merged; binaries-only production deploy (agent, per U9; outside the coordinator per user) | `main` `7c5079d..6239233` (user ran the FF push; classifier blocked the agent's) | preflight 0/0/0/0 before and after service stop; `/healthz` ok; public `/api/v1/info` `source_commit` 6239233, not dirty, instructions 9, checkout-sync rule present; workstation CLI `compatible: true` | Release run 36468394697 with `binaries_only_since=4e71f81` (all 3 jobs incl. 5-min load green; server `02c9c613…`, CLI `b5eba27a…`, MCP `15868f11…` unchanged). Old-binary snapshot `20260928T191024.534Z-d90ffefd…` verified; new-binary snapshot `20260928T191100.541Z-0b72318d…` verified; maintenance complete; timers restarted. Rollback binaries `/usr/local/bin/*-0.1.1-4e71f81`. Downtime ~25 s (19:10:33–19:10:58 UTC). Host scripts now per release: `~/.local/share/agent-coordinator-autonomy/release-<sha>/` (scripts + archive, scp'd to VM `~/release-<sha>`), run via user-added exact-command rules (see §5 4e) |
| 2026-09-28 | Branch cleanup (user request) | — | n/a | Deleted 60 local branches fully in `main` (ancestors, or `git cherry` all `-`) and remote `autonomy/p1`, `autonomy/p2`, `autonomy/p3a`, `autonomy/release-short-load`, `startup-checkout-sync`. Remote now: `main`, `autonomy-plan`, `autonomy/p4`. Kept local worktree branches `autonomy/p2`, `autonomy/p3a` and 11 branches with unique commits (`codex/account-menu`, `codex/download-binding`, `codex/integration-pagination-g3`, `codex/item7-barrier-handoff`, `codex/item7-peer-preflight`, `codex/task-lifecycle`, `codex/task-pagination-g3`, `implement/knowledge`, `implement/restore-time-anchor`, `recovery/task-lifecycle-gen6`, `task/redirect-loop-g2`) |
| 2026-09-28 | **P4 S4a**: integrator reports + fail-closed privilege gate (outside the coordinator, per user; built by a phase-builder subagent, 4 red-team rounds) | `8148437` on `autonomy/p4` (pushed) | full CI gate green (fmt, clippy, 395 tests, build; smoke + backup smoke on the clean committed build; `check_docs.py`) | 0024 extended in place: `integrator_reports` (kinds privilege_gate, flaky, fix_target, unreviewed_landing, target_rewritten, ruleset_missing; UNIQUE(project, kind, dedupe_key), first write wins, `ON CONFLICT DO NOTHING`). Routes `POST/GET …/integrator/reports` (list newest first, `limit` ≤1000, `before` cursor) and human-only `POST …/reports/{id}/resolve {note, decision}`; the resolve route is carved out of the integrator-only path rule in `credential_attributes::integrator_operation` (any future human write under `/integrator/` needs the same). `next` gains `human_queue_items` (the `human_queue` count is unchanged because the shadow supervisor reads it). **Privilege gate (decided in the main loop after text scanning was bypassed 3 times):** path-based and fail-closed. Any R change under `.github/`, the `.github` entry itself, `.gitmodules`, or `CODEOWNERS`/`docs/CODEOWNERS` needs a human decision, as does any change in the local-action scope read **from X only** (trust model: X is human-approved and every R change under `.github/` is gated). Scope = `uses: ./…` dirs (human spellings), symlinks, `runs.main/pre/post/image`; an alias, escape or unreadable reference gates the whole repository. The line scan only supplies reason hints. Known limits (accepted residue: CI runs candidate code with the job token, so workflows must stay secret-free with read-only permissions): a checkout `path:` remaps local-action paths; Node `require` walks above the action dir. The service does not enforce the gate at push-authority (integrator-side only). `ruleset_missing` reports are keyed per freeze episode (LoopState) |
| 2026-09-28 | **P4 S4b**: flake attribution (outside the coordinator; phase-builder + 2 red-team rounds) | `9d96c69` on `autonomy/p4` (pushed) | full CI gate green (411 tests; smoke + backup smoke on the clean committed build; `check_docs.py`) | New `flake.rs` (pure verdicts) + `attribution.rs` (loop). Policy (decided in the main loop after the first version stalled on GitHub's `rerun-failed-jobs` semantics): conclusions pass = success/skipped/neutral, fail = failure/timed_out, else no result; **the latest attempt decides** (same as push authority): latest pass ⇒ publish (with a `flaky` report if an earlier attempt failed); latest fail with ≥2 fails ⇒ reproduced; else rerun failed jobs (never a passing run), at most `MAX_RERUNS=2` per run, then Blocked + `flaky` `no_result`. Reproduced ⇒ X's latest completed check with a result: pass ⇒ `revise check_failed` (with `result_id`); fail ⇒ `fix_target` `target_failing`; running ⇒ wait; none ⇒ `fix_target` `target_unverified`. Refused rerun ⇒ Blocked + `flaky` `rerun_refused`, retried each cycle. Reports that block a subject carry `details.blocks_subject=true` ⇒ service sets `requires_human` (human queue). Service: `check_failed` revise needs a failing deciding receipt + ≥2 failures for one roster identity (`check_failure_not_reproduced`); push authority accepts success/skipped/neutral. GitHub listings paginated (untested: no HTTP mock); jobs read with `filter=all`. **GitHub App needs `actions: write`** (p4-design §5 step 3 lists "Actions: read & write" already). Unverified live: how `filter=all` shows non-rerun jobs; whether cancelled/action_required jobs are rerun. Left: `LoopState.reruns` never pruned; no de-flake task wiring |
| 2026-09-28 | **P4 S4c**: tip monitor + `unreviewed_landing` (outside the coordinator; phase-builder + 2 red-team rounds) | `b2cd058` on `autonomy/p4` (pushed) | full CI gate green (428 tests; smoke + backup smoke on the clean committed build; `check_docs.py`) | Queue gains `targets` (project repository/branch first). Each cycle, per target even with no items: ruleset watchdog (episodes now end on idle projects) then tip monitor (new `watch.rs`, `landing.rs`); one ls-remote + fetch per target. Forward move P→X: out-of-band = P..X minus each recorded published result's own landing range `t0..R` (`LoopState.published` = `{r, t0}`, recorded at grant time, bounded 64); any out-of-band commit with an agent trailer (`Claude-Session`, `Co-authored-by` naming claude/codex or noreply@anthropic.com / noreply@openai.com; trailers only) ⇒ one non-blocking `unreviewed_landing` report keyed (target, P, X), details byte-bounded (60,000). **Fail-closed hold:** a move whose report is neither stored nor refused keeps P and holds the target's items (`tip_move_unsettled`), so nothing publishes on an unclassified tip (red-team B1: otherwise the next publish absorbed the agent commit). `observe_and_close` no longer records tips. Known: a permanent hold is visible only in the integrator log (heartbeat stays healthy) — add a held-since signal later; name rule matches any co-author named Claude/Codex (non-blocking); unknown provenance is [F] |
| 2026-09-28 | **P4 S4d**: serialize-before-park (outside the coordinator; phase-builder + 3 red-team rounds) | `46bc40f` on `autonomy/p4` (pushed) | full CI gate green (438 tests; smoke + backup smoke on the clean committed build; `check_docs.py`) | Integrator conflict revises cite `moved_by_result_id` (newest result it published on the target since the candidate's base; `LoopState.published` gains `result_id`). Service (`integrator_serialize.rs`, table `integrator_revises` in 0024): the revise that would reach the 3/24 h park limit **serializes** (subject gains a dependency on the landing task; not counted toward parking) when the landing is a published result of another task, not already cited in the window (A,B,A parks) and adds no cycle; else it applies and parks with `park_reason` (`landing_unknown`, `landing_is_subject`, `landing_repeated`, `not_a_conflict`, `dependency_cycle`, `serialized_cap`). Park = non-serialized agent revises ≥3 **or** all agent revises ≥6 in 24 h. **Behaviour change for all projects:** agent work claims of a parked task are now refused (the explicit-claim bypass is closed; implicit claims page past parked tasks, `CLAIM_PAGE` 20). A new dependency bumps the task revision (agents re-read before claiming). Known: no human action unparks except doing the work or the window lapsing ("until a human acts" wording is loose, pre-existing); a refused integrator revise on a parked subject stalls that queue head until the window lapses (pre-existing); the service cannot verify the cited landing moved the target (bounded by the cap) |
| 2026-09-28 | **P4 S4e**: revert task (M6), service side (outside the coordinator; phase-builder + 2 red-team rounds + follow-ups) | `9325a29` on `autonomy/p4` (pushed) | full CI gate green (453 tests; smoke + backup smoke on the clean committed build; `check_docs.py`) | **A revert is a `code` task + `task_reverts` row, not a new `tasks.kind`** (the kind CHECK would need a tasks-table rebuild). `POST /projects/{p}/reverts {result_id, reason, evidence?}` on integrator-owned projects: human ⇒ no review + `revert.escaped_defect_canary` event; agent ⇒ evidence + review (floor ≥ agent), review judges the decision, not the inverse diff; creator and all contributors of the reverted task cannot review it. Task stays blocked (`revert_awaits_integrator`; humans too, only exit besides the integrator is a human cancel) until the integrator posts `…/integrator/reverts/{id}/candidate` (attested mechanical; synthetic integrator attempt + normal submission) or `…/not-mechanical` (⇒ reviewed implementation task; capped cascade is [F]). `changes_requested` on a mechanical revert cancels it. `author_withdraw` losing to a **published** push ⇒ priority-0 revert (already_contained keeps the ordinary follow-up). No-op result (r==t0) over a reverted landing range of the same repo/branch ⇒ results refuse `candidate_reverted_in_history`; integrator revise reason `reverted_in_history` (service-verified) sends it back. Published `defect` revert ⇒ planned re-land task. Queue gains `reverts[]` and `items[].reverted[]`; task view `revert` (with attestation) + `reverted_by`. Not done: reverts in `next?role=integrator`, dashboard one-click button, refusal inside push authority |
| 2026-09-28 | **P4 S4f**: integrator revert candidates — **S4 complete** (outside the coordinator; phase-builder + 1 red-team round + fixes) | `95c09d0` on `autonomy/p4` (pushed; CI on `9325a29` and earlier S4 commits green) | full CI gate green (472 tests; smoke + backup smoke on the clean committed build; `check_docs.py`) | New `revert.rs` (computation) + `reverts.rs` (loop). Reverts are worked before items, one per cycle, skipped on held/frozen targets; any refusal skips the revert (items still run). Revert of R on tip X in an isolated worktree (`GIT_CONFIG_GLOBAL=/dev/null`, `GIT_CONFIG_NOSYSTEM=1`, hooks off, pinned identity/date ⇒ reproducible): merge R with first parent T0 ⇒ `revert -m 1`, else the whole T0..R range in one step (synthetic commit of R's tree on T0; Git 2.39 lacks `merge-tree --merge-base`). Clean ⇒ create-only push to `refs/agent-coordinator/candidates/reverts/<task>/<X>` + `candidate` (t0 = X). Conflict / R not in X's history / R already undone ⇒ `not-mechanical {conflict}` (never an empty candidate). Reproduced check failure on a mechanical revert (server marker `items[].revert_task_id`) ⇒ `not-mechanical {check_failed}`. No-op re-land meeting `items[].reverted` (or a `results` 409) ⇒ `reverted_in_history` revise; `not_reverted_in_history` ⇒ Blocked + `fix_target` report (`blocks_subject`). Known limit: the target's own `.gitattributes` merge drivers (e.g. `union`) still apply to a "mechanical" revert. Unverified live: GitHub accepting pushes to `refs/agent-coordinator/candidates/reverts/*` (ruleset A must allow the App) |
| 2026-09-29 | Decision: TypeSafe reranking prototype (`b4f4714`) | — | n/a | User: **defer, ship later** — stays on `autonomy-plan` only; not folded into the P4 release. After P4 ships, cherry-pick onto its own branch with tests and a check that the IPv6-only VM can reach TypeSafe, then release separately |
| 2026-09-29 | P4 S5 started (outside the coordinator, per user) | — | — | `deploy/agentc/soak.py` (own instance on :18091, `$XDG_STATE_HOME/agentc-soak`; scripted CLI agents impl-A/B, rev-1/2; real `agentc-integrator` with fake checks) being built by a phase-builder |
| 2026-09-29 | **P4 S5**: integrator staging soak (outside the coordinator; phase-builder + 1 red-team round + main-loop fixes) | `668335e` on `autonomy/p4` (pushed) | soak rc=0 twice in the main loop: default **6/6 PASS, 23 landings, HRI 0, p50 4.6 s** and `--minutes 15 --random-seed 42` **7/7 PASS, 108 landings, HRI 0, p50 4.5 s / p90 15.0 s**; `py_compile`; no Rust changed | `deploy/agentc/soak.py` (own instance :18091, `$XDG_STATE_HOME/agentc-soak`; scripted CLI impl-a/-b, rev-1/-2 (write; the staging `rev` credential is read-only and cannot review), integrator credential; admin PUT workflow-policy `tests`; roster committed to the seed main; fake checks edited live). Scenarios: clean, conflicts (pair + 3-way serialize-before-park, no park), target_moves (owner commit ⇒ roll-forward; agent-trailer commit ⇒ one non-blocking `unreviewed_landing`), check_failures (flaky publishes; reproduced ⇒ `check_failed` revise ⇒ fix lands), crash_restart (4 integrator SIGKILLs incl. push-completed and push-aborted after authority, + server SIGKILL 12 s), reverts (agent revert reviewed by an independent principal; human revert via admin login). Red-team: **blocker** fixed (a refused non-soak `--dir` was still wiped by `teardown` and its server killed; now refused before `try` and teardown needs the marker, victim control passes), `SystemExit` in a scenario now FAILs the report (mutation control), containment covers all landed dispositions, `rm` ⇒ `shutil`. Reports: `planning/autonomy/soak/`. **Not exercised (human by design):** privilege gate, `target_rewritten`, `ruleset_missing` (only in an HRI control), `fix_target`; GitHub checks. Findings for later: `next` `human_queue` counts only candidates before the first eligible one (a parked subject behind eligible work reads 0; soak counts parked from the DB); `FakeChecks::store` is a non-atomic read-modify-write (soak retries; theoretical lost rerun increment) |
| 2026-09-29 | **P4 S5 review**: fresh-context review of `origin/main..autonomy/p4` (3 Opus reviewers: service core + migration, reverts/reports/docs, integrator crate) | — | all three **ship-after-fixes**, no blocker; migration 0024 verified on a populated schema-23 DB (no rollback path: back up before deploy) | User decisions: (a) **keep S4d's agent-claim refusal of parked tasks for all projects** (option B; it enforces the `revise_limit_reached` precondition `main` already shows in the task view); (b) **fix all should-fix findings** before shipping: service (1) deferred revise settled after a human reopen strands the newer submission ⇒ resolve moot; (2) push authority ignores open/denied `privilege_gate` reports; (3) any agent (incl. the reverted author) can cancel/archive a revert ⇒ human-only; (4) revert candidate sent back on an unmoved tip ⇒ `revert_candidate_conflict` forever; (5) an observation of a result not holding authority ends authority on all results; (6) `reject_mechanical` cancel skips the revision bump; integrator (7) per-item/target errors and (8) permanent refusals stall the whole project queue. Deferred nits: stale-tip ancestry exit 128 after a force-push + gc (keep a ref on the last tip), leaked worktree after `held_elsewhere` recovery, local hex/refname validation of service values, `soak.py` teardown robustness, revert evidence `[""]`, landing-range contributors may review a revert, open mechanical reverts after switching owner back to `agent`, api-contract "a human may create a new revert" wording, `next` human-queue undercount, `eligible_items` LIMIT-before-filter, `replay_result` compares only r/c, human reopen leaves `authority_issued_at`, integrator credential can GET every project, deferred revises emit no `submission.reopened`, receipts not filtered by event; **new: a human action to unpark a task** |
| 2026-09-29 | **P4 S5 review fixes** (outside the coordinator; 2 phase-builders (service, integrator) + 1 red-team each, both CONFIRMED; 1 main-loop fix) | `3f9cd12` on `autonomy/p4` (pushed) | full CI gate green (485 tests; fmt, clippy, build, `check_docs.py`; smoke + backup smoke on the clean committed build) | All 8 should-fix findings fixed, each with an `s5_review_*` test mutation-proven red without its fix. New refusals: push authority `privilege_gate_unresolved` / `privilege_gate_denied`; observe `observation_required` / `authority_not_issued`; lifecycle `revert_cancel` (human gate). A moot deferred revise is stored as `resolved_at` set, `resolution` NULL (0024's CHECK allows only applied/follow_up; widen in a later migration). Re-recorded revert candidates overwrite `revert_candidates.submission_id` for that (task, t0). Integrator: `ServiceFailure` (transport, non-coded status, undecodable reply) aborts the cycle; any other per-item/target error ⇒ `Step::Failed`, skipped like Blocked/Refused and retried next cycle; failed targets give their items no tip. Red-team found (pre-existing) `record_published` inserting in memory before a failed save ⇒ next cycle could push unrecorded; fixed by rollback (`a_published_entry_whose_save_failed_is_not_kept`). Soak re-run on `3f9cd12`: 6/6 PASS, 23 landings, HRI 0, p50 5.2 s / p90 15.0 s. Trap: the core build script caches `COORDINATOR_SOURCE_COMMIT=unknown` if once built without `.git`; set `COORDINATOR_BUILD_COMMIT` to refresh |
| 2026-09-29 | **P4 shipped to `main`** (user OK; merge of `main` into `autonomy/p4` first because `6239233` (PR #3) was not in the branch; the earlier "FF-ok" check had printed nothing and was misread) | merge `db620d4`; `main` `6239233..db620d4` (pushed by the user) | full gate green on `db620d4` (485 tests, smoke, backup smoke, docs); CI green on `autonomy/p4` and `main`; preflight **0/0/0/0** (user; the classifier blocks agent `gcloud compute ssh … "sudo python3 -"` as Remote Shell Writes despite the settings rule) | Release run 36579518972 dispatched without `binaries_only_since` (migration 0024). Deploy dir `~/.local/share/agent-coordinator-autonomy/release-db620d4/` (rollback suffix `-0.1.1-6239233`). Rollback after 0024 = restore the old-binary snapshot taken by `deploy-pre.sh` **and** the old binaries (an old binary refuses schema 24). Also: `autonomy-plan` CI *Coordination checks* fails `cargo fmt --check` since at least `22c1c46` (pre-existing, likely the parked TypeSafe prototype `b4f4714`); not on `main` |
| 2026-09-29 | Decision: TypeSafe reranking ships **after** the P4 deploy as its own binaries-only release (user) | — | n/a | Plan: branch `typesafe-rerank` from `main` `db620d4`; apply `b4f4714`'s code and doc hunks (not its planning-file hunk); split into short functions; **server config switch, off by default** (data egress: up to 40 task/knowledge excerpts ≤1,800 chars per context call to api.typesafe.ai, which has AAAA records); tests against a mock TypeSafe; success/latency logging; gate, fresh-context review, CI, user FF `main`, `release.yml` with `binaries_only_since=db620d4`; deploy with the switch off, turn on after a live call is confirmed |
| 2026-09-29 | **P4 service deployed to production** (agent, per U9; the user added exact `release-db620d4` rules, after which the classifier allowed scp/preflight/pre/swap) | `db620d4` | release run 36579518972 green (full 30-min load); archive sha256 OK, server embeds `db620d4…`; preflight 0/0/0/0 before pre and again after stop; `/healthz` ok 6 s after start; `/api/v1/info` `source_commit` `db620d4467b4…`; instructions 9 | Downtime 14:43:39–14:43:57 UTC. Old-binary snapshot `20260929T144331.103Z-e8f0b68d-…` (rollback = restore it + binaries `*-0.1.1-6239233`); new-binary snapshot `20260929T144357.771Z-9bfbbcbc-…` verified; maintenance + timers OK. Workstation CLI upgraded via `upgrade_client.py` from a clean detached worktree `~/src/worktrees/ac-release-db620d4` (rollback `~/.local/bin/agent-coordinator.rollback`). **S5 complete.** Next: TypeSafe follow-up release (in progress on `typesafe-rerank`), then S6 (needs p4-design §5 user steps 2 and 3). `/opt/agentc` CLI is still the P2 build: re-run host-setup before P3b |
| 2026-09-29 | **TypeSafe reranking, release branch** (outside the coordinator; phase-builder + fresh-context red-team CONFIRMED + 1 main-loop fix) | `typesafe-rerank`: `221774f` (import of `b4f4714` code/doc hunks), `903b614` (switch, refactor, tests, docs, `service.env.example`), `a773748` (trim key) — pushed | full gate green on `a773748` (500 tests, smoke, backup smoke, docs); CI green (Windows `coordinator-local` `repeated_run_does_not_spawn_twice_and_inspect_is_nonblocking` flaked once on its 500 ms inspect window; passed on rerun and on `903b614`) | `COORDINATOR_CONTEXT_RERANK=off|typesafe` (default off; off reads no key and builds no client). Key read once at startup, trimmed, `Debug` redacted; keyless `typesafe` warns once and stays off. One shared client: 1 s connect / 3 s total (bounds body read), no redirects, streamed 256 KiB cap. Log line `context rerank` with outcome/reason/candidates/elapsed_ms. Fixed a prototype panic (missing score key). Deferred: the production key sits in `service.env` (which the backup guide reserves for non-secrets and the backup/maintenance units also load) — before enabling, move it to a main-service-only secret file (`EnvironmentFile=-/etc/agent-coordinator/<secret>.env`, unit change); no concurrency cap/circuit breaker (each context call ⇒ one paid call, up to +3 s when TypeSafe is down); CLI subcommands build the client when enabled (harmless) |
| 2026-09-29 | Decision: TypeSafe reranking is enabled **by the key alone** (user; supersedes the off-by-default switch) | `main` at `a773748` (user FF) carries the switch version; its release run 36601265116 was cancelled, not deployed | n/a | Remove `COORDINATOR_CONTEXT_RERANK`; valid `TYPESAFE_API_KEY` at startup ⇒ rerank when possible; none ⇒ one startup warning on the serve path only (admin subcommands silent). Production already has the key in `service.env`, so the next deploy turns reranking on. The deploy key-check (`deploy-pre.sh` refuses to stop the service without a nonempty key line) stays |
| 2026-09-29 | **TypeSafe reranking deployed to production, on** (agent; binaries-only) | `main` `a773748..4a72018` (user FF); `4a72018` key-only enablement | gate green on `4a72018` (501 tests, smoke, backup smoke, docs); CI green; release run 36624295472 green (5-min load, `binaries_only_since=db620d4`); preflight 0/0/0/0 twice; `/api/v1/info` `4a72018a2cfd…`; **0 keyless-rerank warnings** after start (key present) | Downtime 20:25:09–20:25:19 UTC. Old-binary snapshot `20260929T202502.433Z-98093fbb-…` (rollback: binaries `*-0.1.1-db620d4`; no schema change). New-binary snapshot `20260929T202519.426Z-583ecb91-…` verified. Workstation CLI `4a72018` (rollback `~/.local/bin/agent-coordinator.rollback`). Not yet observed: a live `context rerank` log line — check with `sudo journalctl -u agent-coordinator.service \| grep 'context rerank'` on the VM after the first context request (the classifier blocks agent production journal reads). **Deploys now use the fixed dir `release-next`** (4 exact settings rules, never per-release): write `RELEASE` = `<full commit> <archive name> <previous short commit>`; `deploy-pre.sh` deletes other archives, checks the sha256 and that the staged server embeds the commit before stopping anything; local record copy `release-<short>`. Leftover worktrees `ac-release-{db620d4,4a72018}`, `agent-coordinator-typesafe` and branch `typesafe-rerank` (local + origin) removed 2026-09-29 |
| 2026-09-29 | Cleanup (user): P2/P3a/P4 worktrees and branches removed; **P3a shadow poller stopped** | `autonomy/p2` `d77c328`, `autonomy/p3a` `4e71f81`, `autonomy/p4` `db620d4` (all contained in `main`; `origin/autonomy/p4` deleted too) | n/a | Only the main checkout (`autonomy-plan`) remains. HANDOFF commands that `cd ~/src/worktrees/agent-coordinator-p{2,3a,4}` now need a fresh detached worktree of `main` (e.g. `git worktree add --detach ~/src/worktrees/ac-main origin/main`) with `CARGO_TARGET_DIR` inside it. The shadow is no longer running; restart it from a `main` build if P3a data is wanted again (`start-shadow.sh` points at the removed p3a binary) |

### [R-P3b] Deferred from the P2 review (must be done before the first live launch)

Nothing launches unattended before P3b, so P2 ships without these; each is a launch-model change.
1. **Claude-profile write confinement** (review #6): a Claude launch can write anything its uid owns —
   other launches' clones/runs, `$CARGO_HOME/config.toml` (`rustc-wrapper` persists into later builds),
   `$HOME`, and `claude-config/settings.json` (loaded by the next launch via `--setting-sources user`).
   Make settings/`CLAUDE.md`/`CARGO_HOME/config*` root-owned read-only or preflight-verified, give each
   launch its own writable state, and add Claude-profile write probes to the suite.
2. **Push and scan outside the agent's reach** (review #7): the scan runs in the agent's own CLI process,
   `.git/config` is editable, and the deny rules are prefix matches. Safe today only because agent uids
   have no GitHub credential. Before P3 provisions one: ruleset restricting `main` (P4 ruleset A) and a
   candidate-push helper under a separate uid that the agent cannot modify.
3. **Reviewer builds run candidate code with reviewer secrets** (review #8): `cargo test` as
   `agentc-rev` executes the implementer's `build.rs`/tests with the reviewer's token, staging password
   and harness logins readable. Run candidate code under a third uid or a sandbox that hides those paths.
4. **Cross-uid loopback**: the firewall opens loopback 32768-60999 to both agent uids, so each can reach
   the other's (and the owner's) listeners. Narrow it (per-uid network namespace or owner-matched rules).
5. Nits: `ship.py` fetch/ls-remote race and no overall Actions timeout (and `gh run list` default repo);
   launches lack `no_new_privs` and leftover-process cleanup; `--uninstall` does not kill agent processes
   or remove crontabs/`/tmp` files; read-only credentials can still create subagent sessions.

### [Optional before P3b] Shadow cost run

The P3a shadow proved the credential, `next` and the would-launch log, but production had no claimable
work, so it produced **no cost data**; the poller was stopped and its worktree removed on 2026-09-29.
If the P3b go/no-go should be costed from observed load rather than estimates, run the shadow again
**after real tasks are queued** in the pilot project(s), for about a day:
```
git worktree add --detach ~/src/worktrees/ac-main origin/main
cd ~/src/worktrees/ac-main && export CARGO_TARGET_DIR=$PWD/target
COORDINATOR_BUILD_COMMIT=$(git rev-parse HEAD) cargo build --release --locked -p agentc-supervisor
# edit BIN= in ~/.local/share/agent-coordinator-autonomy/start-shadow.sh to this worktree's binary, then:
bash ~/.local/share/agent-coordinator-autonomy/start-shadow.sh
# a day later:
target/release/agentc-supervisor shadow-report     # would-launch per role, estimated $, max human queue
pkill -f '[a]gentc-supervisor shadow'   # the [a] keeps the pattern from matching this shell's own command line
```
The poller is not reboot-safe (check `pgrep -af "agentc-supervisor shadow"` before reading the report).
Skip this step if the pilot's 5-task budget is acceptable without observed load.

### Historical P2–P4 instructions (use the current resume point above)

1. DONE 2026-09-26 (all PASS). For reference, the host setup and suite (re-run after binary changes): Fix any FAIL; common
   causes: a login/auth host missing from the egress allowlist (see `/var/log/agentc-egress.log` on sysvinit hosts or `journalctl -u agentc-egress`,
   then add it to `egress_allow_extra` in `/etc/agentc/supervisor.toml` and restart the unit).
   ```
   cd ~/src/worktrees/agent-coordinator-p2
   sudo SUPERVISOR=target/release/agentc-supervisor CLI=target/release/agent-coordinator deploy/agentc/host-setup.sh
   # per role (impl, rev): harness logins as printed; supervised credentials (impl write, rev read)
   sudo deploy/agentc/containment-suite.sh --cargo-test
   ```
2. DONE 2026-09-26: staging coordinator (`deploy/agentc/staging.py`) and UI verification
   (`verification_env`, `scripts/verify_ui.mjs`). **The user** now re-runs host-setup (new supervisor
   binary, pinned `node`, browser, `verification/` dirs), installs the staging credentials and adds the
   verification entry, then re-runs the suite (it now includes the reviewer browser check):
   ```
   cd ~/src/worktrees/agent-coordinator-p2
   # as the owner, clean tree; the server too, so staging and the installed CLI share one commit
   COORDINATOR_BUILD_COMMIT=$(git rev-parse HEAD) cargo build --release --locked \
     -p agentc-supervisor -p coordinator-cli -p coordinator-server
   deploy/agentc/staging.py down && deploy/agentc/staging.py up   # now serves the release build
   sudo SUPERVISOR=target/release/agentc-supervisor CLI=target/release/agent-coordinator deploy/agentc/host-setup.sh
   deploy/agentc/staging.py credentials      # run the printed sudo commands
   # append below the KEEP line of /etc/agentc/supervisor.toml:
   #   [verification.1dad5306-cbaf-4e6c-a7ed-20b9f369362d]
   #   url = "http://127.0.0.1:18080"
   sudo deploy/agentc/containment-suite.sh --cargo-test
   ```
3. DONE 2026-09-27 (P2 shipped to `main` `d77c328` and deployed). Was: P2 exit = suite all PASS under both profiles. Then ship `autonomy/p2` (fresh-context review, the
   user's OK, preflight, `scripts/ship.py` or FF push) and deploy migration 0023 (U9: agent deploy
   after a clean preflight). P3a (shadow `next`, would-launch log) can start in parallel (no root).
4. **P3a — shadow running; exit pending (next session starts here).** State at 2026-09-27 05:30 UTC:
   `main` = `7c5079d`; production = `4e71f81` (binaries only; schema unchanged at 23); workstation CLI =
   `4e71f81`; shadow poller running since 05:12 UTC on mxmini (see the execution log for paths).
   a. **P3a exit (≥ 2026-09-28 05:12 UTC / 01:12 EDT):** confirm the poller is alive, then summarise:
      ```
      pgrep -af "agentc-supervisor shadow"
      ~/src/worktrees/agent-coordinator-p3a/target/release/agentc-supervisor shadow-report
      tail -n 20 /var/lib/agentc/shadow/shadow.out
      ```
      Record here: span hours, would-launch per role and estimated $ (divide by span for per-day
      figures), max human queue, errors. Production had **no claimable work** at start (both roles
      idle), so a quiet log is expected unless tasks are created; that is still a valid P3a exit
      (the poller, credential and endpoint are proven), but note it — the pilot needs real tasks.
      If the poller died (reboot): `bash ~/.local/share/agent-coordinator-autonomy/start-shadow.sh`.
   b. Then stop it or leave it running (it is cheap: one GET per project per role per minute); it
      must be stopped before the P3a worktree is removed: `pkill -f "agentc-supervisor shadow"`.
   c. Housekeeping: the installed pinned supervisor under `/opt/agentc` is still the P2 build; the
      user re-runs host-setup with the `4e71f81` binaries before P3b (not needed for the shadow):
      ```
      cd ~/src/worktrees/agent-coordinator-p3a
      COORDINATOR_BUILD_COMMIT=$(git rev-parse HEAD) cargo build --release --locked -p agentc-supervisor -p coordinator-cli
      sudo SUPERVISOR=target/release/agentc-supervisor CLI=target/release/agent-coordinator deploy/agentc/host-setup.sh
      sudo deploy/agentc/containment-suite.sh --cargo-test
      ```
   d. **P4-min integrator: design done 2026-09-27 — follow `planning/autonomy/p4-design.md`** (§4 build order S1–S6,
      §5 user steps). Integrator runs on oracle-1 (U16). S1 and S2 are service-only and need nothing
      from the user; the GitHub App (user step 3) is needed before S3's live test.
      **S1 done 2026-09-28 (`65e4f58`); S2 done 2026-09-28 (`1e6b695`); S3 done 2026-09-28
      (`34d8bbe`), see §11. S4a done 2026-09-28 (`8148437`, reports + privilege gate); S4b done (`9d96c69`, flake attribution); S4c done (`b2cd058`, tip monitor); S4d done (`46bc40f`, serialize-before-park); S4e done (`9325a29`, revert service side); S4f done (`95c09d0`) ⇒ **S4 complete**. S5 soak done 2026-09-29 (`668335e`, `deploy/agentc/soak.py`, HRI 0, p50 4.6 s; reports in `planning/autonomy/soak/`). **S5 complete 2026-09-29: shipped (`main` `db620d4`) and deployed.** Was: the user's OK, preflight, FF `main`, and the service deploy of migration 0024, which is **not** binaries-only). Still pending from the user before S6/live tests: p4-design §5 steps 2 (U4 ruleset test) and 3 (GitHub App with Contents rw, **Actions rw** (reruns), Checks r, Administration r, Metadata r; give App id + installation id). (S4 was:  tip monitor + `unreviewed_landing`, serialize-before-park,
      revert kind M6; plus the S3 deferrals listed in §11). S3's **live** GitHub test is still
      pending p4-design §5 user steps 2 (U4 ruleset test) and 3 (GitHub App: give the App id and
      installation id); it can run any time after those, e.g. before S5.
      S2 as specified was:
      push-authority (digest pin, deciding success receipts per roster check with `workflow_blob`
      from T0, 5a contributors, `stacked_on_unapproved`, submission-bound hold via
      `integration_holds.submission_id`), observations (published / not_published / target_moved,
      revise-loses-to-push), refusal `integration_owned_by_integrator` on the LLM integration routes,
      and `next?role=integrator`. New tests go at the end of `crates/server/tests/workflow.rs`
      (helpers `integrator_caller`, `patch_owner`, `result_body`). Smoke needs a clean committed
      build (`COORDINATOR_BUILD_COMMIT=$(git rev-parse HEAD)`).
   e. **Future deploys** (U9, agent-run; last done 2026-09-28 for `6239233`): `gh workflow run release.yml --ref main
      -f binaries_only_since=<production commit>` when no migration changed (else omit the input) →
      copy the previous `~/.local/share/agent-coordinator-autonomy/release-<old>/` scripts into
      `release-<new>/` with `REL=` and the rollback suffix (`-0.1.1-<production commit>`) updated →
      `gh run download` the linux archive into it + `sha256sum --check` → ask the user to add four
      exact rules to `.claude/settings.local.json` (VM zone `us-east1-b`, `--tunnel-through-iap`):
      `gcloud compute scp … --recurse <dir>/release-<new> agent-coordinator:~/` and
      `gcloud compute ssh agent-coordinator … --command "sudo python3 release-<new>/preflight.py"`
      (same for `sudo bash release-<new>/deploy-pre.sh` and `…/deploy-swap.sh`) → scp → preflight →
      pre → preflight → swap → check `/api/v1/info` `build.source_commit` → `upgrade_client.py
      --source-root <clean detached worktree at that commit>`. The classifier blocks the broad
      `gcloud compute ssh agent-coordinator:*` rule ("Production Reads") and agent FF pushes to
      `main` ("Merge Without Review"); the user runs `! git push origin <sha>:main`.
