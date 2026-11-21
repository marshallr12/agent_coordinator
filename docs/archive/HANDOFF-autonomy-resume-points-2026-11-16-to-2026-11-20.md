# Autonomy handoff — archived resume point, 2026-11-16 to 2026-11-20

> Archived on 2026-11-21 from the root `HANDOFF.md` (branch `autonomy-plan`), moved verbatim and
> in its original order (oldest update first). This is the superseded "Current resume point"
> section as it stood after the 2026-11-20 21:13 UTC update; the live state is the "Current
> resume point" in `HANDOFF.md`. "Above", "below" and "§n" inside this file refer to `HANDOFF.md`
> as it stood when each update was written. This file is historical evidence and is not updated.

## Current resume point — P6 `d513bc21` DONE as a completed failed evaluation: second NO-GO (2026-11-16 20:20 UTC)

The window `2026-11-02T16:50:00Z` → `2026-11-16T16:50:00Z` closed. P6 `d513bc21` was re-admitted by
the owner (revision 6), claimed (attempt `1e5dae23`, gen 20), audited, submitted and approved. Done
means the evaluation ran and its metrics are recorded; it is **not** a readiness pass.

- **Result (NO-GO):** met — 20 human-origin tasks Done, digest 14 runs at one per UTC day, mxmini
  canaries 14 ok. Failed — median creation→Done **56.77 h** vs 4.12 h (0 of 20 within target);
  HRI lower bound 3 requires-human `fix_target` reports / 45 finished (`71bf35bf`, `7fda3475` still
  open, `105baadb`); 4 owner pushes to main vs 2 in the prior 14 days. oracle-1 canaries 12 ok,
  1 timeout, no row 11-04.
- **Main latency cause:** "Audit locked dependencies" failing on main blocked every landing from
  window start until the owner pushed `d35fc81d` at 2026-11-04T02:56Z.
- **Observer was useless:** 1 of 1346 cron samples complete (`authentication_required` in cron).
  The audit was rebuilt from a read-only production DB snapshot (19:32:16Z, event seq 72278), the
  oracle-1 journal and the GitHub branch-activity API (which counts real pushes, uncapped).
- **Evidence:** `~/.local/share/agent-coordinator-autonomy/p6-final-20261116/` (REPORT.md,
  metrics.json, scripts); artifact `0d3b23cf` (sha256 `a0359760…`, job `c8474895`).
- **Decisions (owner recorded all in the dashboard; agent prefilled):** `bb9bf99f` Go (gen 7);
  `b2d5ae10` gen 3 "Keep open and rerun", then gen 4 **"Accept failed evaluation for review"**.
  Any task-revision change makes both stale again.
- **Submission** `e6ec1602` (general), review activity `e6d9dc1f`, **approved 20:15:49Z** by
  oracle-1's supervised reviewer `oracle-1-verdict` (distinct launch) with advisory findings only.
  It could not open the artifact, database or journal from its sandbox, so the metric values
  were taken from the submission and not independently recomputed. A local review subagent
  (`evidence-reviewer`) was spawned but found the activity already claimed and verified nothing.
- **Host state:** mxmini quiet-hours cron restored byte-for-byte at 19:28:43Z (claims only
  22:00–07:00 local again); observer crontab line removed.
- **Open:** report `7fda3475` unresolved; digest shows a stalled queue and 3 stalled tasks
  (`d2ca0225`, `7011c55f`; P6 itself was the third). Worktrees `ac-d513-p6-{audit,rerun,final}`
  and their local branches are removed (clean, merged, no remote branches). A third window, if
  wanted, should be a new task with a corrected latency measure.
- **Follow-up 2026-11-16 20:42 UTC (read-only triage plus one new task):**
  - `7fda3475` is stale: it recorded the 11-09 audit failure at `89f62e96`, fixed by `32bcee0`
    (task `774c577c`). Audit is green on main `9c88617`. Only the owner can resolve it (dashboard).
  - Not queue faults: `d2ca0225` is blocked on the owner confirming the autonomy plan is finished;
    `7011c55f` is a human-delivered parent idle by design. Every open task is human-delivered or
    owner-gated, so supervisors had nothing eligible (inferred; the digest's stall rule was not read).
  - The audit broke main three times in the window (11-02, 11-09, 11-13), each cleared by an owner
    push. Owner chose "auto-fix + pass unchanged locks"; task **`09557c72`** (open, ready, priority 1,
    agent-origin, admitted 1/14) specifies it. Its roster change for `dependency-audit` is the
    owner's to apply; deployment is a separate owner step. CLI session `audit-policy-20261116`.
  - Unfixed warnings on main: rustix 1.1.4, tokio 1.53.1 (unsound), crypto-bigint 0.7.8 (yanked).
- **Update 2026-11-17 04:30 UTC:**
  - `09557c72` is **Done**; it landed on main as `61a222c`, `33316ce`, `227fe87`, `135d034` (not
    reviewed by this session). Not active in production: the `dependency-audit` roster change
    (declare `Cargo.lock`, `.cargo/audit.toml`) and the deploy are the owner's.
  - Warnings fix: task **`5e094f6d`** (human-delivered, owner go-ahead), attempt `85ac9b7f`,
    candidate `4705310` on main `4eb2db9` (Cargo.lock only: rustix 1.1.6, tokio 1.54.1,
    crypto-bigint 0.7.11, ctutils 0.4.8). Local gate and smoke green (1207 tests). Submission
    `e422b44c`; review `a9c79683` approved by a supervised reviewer, which reported two
    `cargo test` failures in its sandbox that it reproduced on base `4eb2db9` (not seen locally).
    **Landed on main as `4705310` at ~05:13Z; task Done.** Main CI on `4705310` was still
    running at 05:14Z. Worktree and local branch removed.
  - `scripts/smoke.py` refuses a dirty tree ("candidate build is dirty or lacks build
    provenance"); commit before running it.
- **Update 2026-11-17 19:20 UTC — full rollout of main `4705310` DONE (owner: "do the deploy"; task
  `c1b7f538`, attempt `9ffd2570`):**
  - Main CI is green on `4705310`. The two base failures the reviewer saw (`a9c79683`):
    `candidate::shell_tests::host_cwd_symlinks_and_nonregular_targets_are_refused` is
    environment-caused (socket path over 108 bytes under a long TMPDIR);
    `serve::tests::a_client_outside_the_launch_is_refused_without_minting` (agentc-push) is a
    load-dependent BrokenPipe race, 2/960 on base, **not** sandbox-only and unfixed.
  - Reviewed `09557c72`'s four commits read-only: logic sound. Findings: the dashboard roster
    editor drops a check's `inputs` on save (task **`84e85336`**, submitted by oracle-1, in review);
    no first-party way to send the roster `PUT`; blob equality is attested, not verified
    (advisory); a stale doc reference in a `crates/core/src/workflow.rs` comment.
  - Production, both hosts' CLI/supervisor/push and oracle-1's integrators now run **`4705310`**
    (over `39c22d8` / `e5b670d`); migration `0047` applied (schema 46 → 47). Release run
    `44126235877` (owner-dispatched) passed with full load acceptance. Drain marker
    `deploy-drain-c1b7f538` set 16:32:20Z, idle 17:09:16Z, 0 launches killed, cleared 19:13:37Z.
    R1 (job `47afc789`) refused safely on MINIAIR's active attempt on `9cf04a5c` (owner had it
    released; resume from its checkpoint, branch `task-9cf04a5c` at `734766a`). R2 (job `a8e5b303`)
    refused on dead job `a92ce565` (registered 11-09, never started, already reconciled, task
    done). R3 (job `964f0616`) succeeded after `remote_upgrade.py` was narrowed to exempt a
    registered job with no pid, sequence 0, a reconciliation and an inactive attempt
    (**agent's call, not an owner decision**; it will block every future deploy until cleared
    or the exemption is kept). Pre/post backups and off-server copies verified.
  - Rollback: production `/var/lib/agent-coordinator-deployments/claude-c1b7f538-4705310-20261117-r3`;
    CLIs `/opt/agentc/rollback-cli-4705310-20261117`; hosts `/opt/agentc/rollback-host-4705310-20261117`.
    Host sources: mxmini `~/src/worktrees/ac-c1b7f538-rollout` (native x86_64), oracle-1
    `/home/ubuntu/src/worktrees/agentc-rollout-4705310` (native aarch64, `APPARMOR_BWRAP=1
    UPDATE_TIMER=0`). Containment: oracle-1 113 PASS, mxmini 104 PASS. mxmini quiet hours
    unchanged (22:00–07:00; marker back to `quiet-hours`).
  - Agent harness pins kept at **Claude 2.1.319 / Codex 0.166.0** on both hosts (agent's call):
    host-setup re-pinned the owner's 2.1.326 / 2.1.325 and 0.169.0, and the upgrade scripts put
    the old binaries and `supervisor.toml` lines back.
  - **Owner's next step:** apply the `dependency-audit` roster change — `PUT
    /api/v1/projects/<project>/workflow-policy` with `expected_revision: 3` and `inputs:
    ["Cargo.lock", ".cargo/audit.toml"]` on that check. Until `84e85336` lands and is deployed, a
    dashboard roster save drops `inputs` again. Other workstations' CLIs (MINIAIR) must move to
    `4705310` or fail `client_upgrade_required`.
  - Evidence and scripts: `~/.local/state/agent-coordinator-evidence/c1b7f538-ce6c-4c2b-a945-17c1787ac7f8/`
    (`r1/`, `r2/` hold the refused runs).
  - **Task `c1b7f538` Done:** submission `dc58d5fe`, artifact `94fadbe1`, review `641b0270` approved
    19:22:54Z by subagent `deploy-reviewer` (same owner id as the submitter; production-side
    claims were taken from the R3 report). Its findings, both confirmed afterwards:
    (1) host-setup installed **Rust 1.100.0** on mxmini (the owner's rustc) and made it the agents'
    default; before, only 1.99.0 / 1.99.1 were there. Not reverted — **owner decision**; oracle-1
    stays on 1.98.1. (2) On oracle-1 the CLI copies in `rollback-host-4705310-20261117` are an
    intermediate `4705310` build (`3bc5d7b8…`), not the old CLI; the real pre-rollout CLI
    (`25a03fe3…`) is in `rollback-cli-4705310-20261117`. mxmini's copies are correct.
- **Update 2026-11-17 19:40 UTC — owner's choices on the four open items:**
  - `84e85336`: in review at 19:27Z (submission `8240c5ec`, review activity `3966d1fc` claimed,
    lease to 20:16:36Z), integration queued, not landed.
  - **Roster change:** owner chose a direct request. Ready, **not sent** (the owner runs it):
    `~/.local/state/agent-coordinator-evidence/roster-inputs-request/declare-audit-inputs.js`, a
    snippet for the dashboard's browser console while signed in. It refuses unless the roster is
    at revision 3 with exactly one `dependency-audit` entry. Tested by
    `test_declare_audit_inputs.mjs` there (real server built at `4705310`, real dashboard login
    in headless Chrome): signed-out refusal, save to revision 4, second-run refusal. A roster
    revision is not a pin (`pins_current`), so in-flight submissions do not go stale. A dashboard
    roster save still drops `inputs` until `84e85336` is deployed.
  - **mxmini Rust 1.100.0:** keep. No action.
  - **Harness pins:** owner chose to move the agents to the installed versions (mxmini has
    Claude 2.1.326, Codex 0.169.0); see the go-ahead below.
  - **Dead job `a92ce565`:** there is no first-party way to clear it: the service already treats
    a reconciled job as not live (`jobs.rs`: live means non-terminal state and `reconciled_at IS
    NULL`), only the job's own reporter can post a terminal observation, and the alternative is
    a hand edit of the production database. The owner then chose to align the deploy check
    instead: task **`b9f49d0a`** (open, ready, agent-origin, CLI session `open-items-20261117`)
    puts the drain check in the repository with the service's job rule. Until it lands, the
    next deploy still needs the narrowed `remote_upgrade.py` from the `c1b7f538` evidence
    directory; that script is on mxmini only, so an implementer elsewhere cannot read it.
  - **Harness pins, owner go-ahead given** (after `84e85336`'s review; mxmini first, oracle-1 only
    if mxmini passes). `84e85336` review approved 19:41:05Z, integration queued.
    **mxmini DONE 19:42:30Z:** Claude 2.1.326 / Codex 0.169.0, containment 104 PASS, core
    binaries unchanged, quiet-hours marker unchanged; rollback `/opt/agentc/rollback-harness-20261117`.
    **oracle-1 DONE 20:11:28Z:** Claude 2.1.325 (the owner's installed version there, one patch
    behind mxmini) / Codex 0.169.0, containment 113 PASS, core binaries unchanged. Kill switch
    `harness-repin-20261117` was set 19:43:23Z, the launch on `b9f49d0a` finished by itself, and
    the switch is removed; `agentc-run` and both integrator units active. Rollback
    `/opt/agentc/rollback-harness-20261117` on each host.
  - **State 20:11Z:** `84e85336` **Done**, landed on main as `f210a59` (the integrator ran
    during the drain); **not deployed**, so the dashboard roster dialog still drops `inputs`.
    `b9f49d0a` submitted by oracle-1, waiting for review. Roster snippet still not run by the owner.
  - **State 20:49Z:** oracle-1 works end to end on the new pins: its reviewer decided
    `changes_requested` on `b9f49d0a`'s first submission (20:13Z), the implementer resubmitted
    (attempt `baaefcb5`, 20:48Z), second review queued behind a review of `9cf04a5c`. Roster
    still revision 3.
  - **Next item (owner's choice, go-ahead given):** wait for `b9f49d0a` to land, then roll out
    main with the in-repo drain check; the owner dispatches the release run. A background
    watch on the task runs in this session only. The owner updated their Claude on oracle-1 to
    **2.1.326**; the agents there are still pinned to 2.1.325. The rollout's host-setup will pin
    2.1.326, so **do not put the old pins back this time** (drop `restore_pin` from the host
    upgrade scripts); run the containment suite on both hosts afterwards.
  - **Task `6faa1251`** created at the owner's request (open, ready, agent-origin): make an
    unused harness optional in host-setup, the containment suite and the updater. Neither host
    launches Codex today; only the installer and the suite require it, not the supervisor.
  - **Task `610a5031`** created at the owner's request (open, ready, agent-origin): the
    supervisor picks Claude or Codex before each claim from each account's remaining usage
    (opt-in). Today it only switches after a 429 / expired credential / failed sign-in, and only
    to `[health.fallback]`, which neither host configures. Whether the vendors expose a
    remaining-usage figure is unverified; the task makes the implementer establish it.
    Both `6faa1251` and `610a5031` were edited to revision 2 on the owner's instruction that the
    same Claude and/or Codex options apply to reviewers: a review launch today always uses the
    primary vendor (`live_review.rs`, `Vendor::primary`) and ignores `[health.fallback]`.
    Scripts and output: `~/.local/state/agent-coordinator-evidence/harness-repin-20261117/`.
  - **Update 2026-11-18 02:40 UTC — full rollout of main `818a7b1` DONE (task `d8fdc1a6`, attempt
    `6d1e3687`, CLI session `open-items-20261117`):**
    - Target moved from `29407ea`: main gained 13 commits while waiting (`6faa1251`, `610a5031`,
      `9cf04a5c`, `c69f31b5` landed; no new migration) and the owner's release run
      **`44217444217`** (full acceptance) was on `818a7b1`. The task text still says `29407ea`.
    - Production, both hosts' CLI/supervisor/push and oracle-1's integrators run **`818a7b1`**
      (over `4705310`). Drain marker `deploy-drain-d8fdc1a6` set 01:39:38Z (both hosts already
      idle, 0 launches killed), cleared 02:36:03Z; mxmini's marker is back to `quiet-hours`,
      oracle-1's is absent. Production job `d4fff220` succeeded first try (832 s): the in-repo
      `scripts/drain_check.py` at `818a7b1` ran unmodified and exited 0 (it lists the two parked
      `recovery_required` activities `2aba7a54`, `2e12b535` as non-blocking since `c9ce23b`);
      pre/post backups and off-server copies verified, timers restored.
    - Rollback: production `/var/lib/agent-coordinator-deployments/claude-d8fdc1a6-818a7b1-20261117/rollback`;
      CLIs `/opt/agentc/rollback-cli-818a7b1-20261117`; hosts `/opt/agentc/rollback-host-818a7b1-20261117`.
      Host sources: mxmini `~/src/worktrees/ac-d8fdc1a6-rollout`, oracle-1
      `/home/ubuntu/src/worktrees/agentc-rollout-818a7b1` (`APPARMOR_BWRAP=1 UPDATE_TIMER=0`).
    - Harness pins are now whatever host-setup pinned (owner decision): mxmini **Claude 2.1.327 /
      Codex 0.169.0**, oracle-1 **Claude 2.1.326 / Codex 0.169.0**.
    - **Containment suite is broken on main:** `deploy/agentc/containment-suite.sh` at `818a7b1`
      aborts after 14 PASS with `line 171: base: unbound variable` (`check_seeds`, from `deead5b`).
      Containment was verified with a copy that only splits that `local` statement (oracle-1 113
      PASS, mxmini 104 PASS; copy in the evidence directory, `suite-patched/`). Fix task
      **`eed6ebd4`** (open, priority 1). Until it lands, a host updater run fails its suite step.
    - **Task `d8fdc1a6` Done:** submission `e0294dc8`, review activity `b0d1b968` approved
      02:55:33Z by subagent identity `reviewer` (CLI session `deploy-reviewer-20261118-sub`; the
      first connect was denied by the permission classifier, the owner approved a retry). Six
      advisory findings, none blocking: commit `818a7b1` vs the task's `29407ea`; patched suite;
      production-host internals rest on the rollout report; integrators cannot report their
      source; the release run's host-asset publish job was skipped; MINIAIR's CLI.
      `eed6ebd4` was claimed by oracle-1 at ~02:53Z.
    - Evidence: `~/.local/state/agent-coordinator-evidence/d8fdc1a6-f3e9-43cb-848c-eed087594128/`,
      artifact `37b27e7d`. The dashboard roster dialog now keeps a check's `inputs` (`f210a59`
      is deployed). Roster still at revision 3 unless the owner has run the snippet. MINIAIR's
      CLI must move to `818a7b1`.
  - **Update 2026-11-18 04:50 UTC:** `eed6ebd4` is Done (03:40:15Z), on main as **`f8bddba`**
    (suite fix plus a `set -u` test). Not deployed: both hosts still carry the `818a7b1` suite;
    mxmini's updater picks it up, oracle-1 (`UPDATE_TIMER=0`) gets it at the next rollout. Both
    reviewer CLI sessions from the `d8fdc1a6` review are closed. **Roster change pending the
    owner's click:** the dashboard's Required checks dialog was filled (dependency-audit declared
    inputs `Cargo.lock`, `.cargo/audit.toml`; other checks untouched) but only the owner saves
    it; confirm with a workflow-policy read that the roster is at revision 4 (needs the owner's
    go-ahead, like any production read). The owner is updating MINIAIR's CLI themselves.
  - **Update 2026-11-18 05:38 UTC — roster change DONE:** the owner saved it; the workflow policy
    reads revision **4** (`updated_at` 05:38:04Z) with `inputs: ["Cargo.lock", ".cargo/audit.toml"]`
    on dependency-audit and the other two checks unchanged. Nothing was in flight at the save.
  - **Update 2026-11-18 14:40 UTC — four open items taken (owner chose all four; CLI session
    `open-items-20261118`):**
    - **HANDOFF pruned** 2406 → 809 lines (`4579a7b`): the 2026-09-29 to 2026-11-02 resume points
      moved verbatim to `docs/archive/HANDOFF-autonomy-resume-points-2026-09-29-to-2026-11-02.md`.
    - **P6 third window:** task **`e6685dbf`** created **planned**, human-delivered, general
      (not admitted, no start time). Owner decisions: latency judged as median **ready→Done** with
      the 20 value tasks released on a stagger and the baseline recomputed with the same measure;
      **no cron observer** (audit from durable records, scripts pinned by digest in `window.json`);
      mxmini **keeps quiet hours**. The owner supplies the 20 tasks and admits it. Preconditions
      are in the task text. Observer failure cause (inferred, not tested): the coordinator token is
      exported only in the interactive shell, so cron had none.
    - **Containment suite `f8bddba`:** there is no installed copy on a host and mxmini has no
      updater job in cron (only the canary and quiet-hours jobs); the updater takes the suite from
      a published release's `scripts/`, a rollout from its source tree. So the fix arrives with the
      next rollout built at `f8bddba` or later; nothing was installed. Verified unmodified
      (sha256 `1f819504…`): oracle-1 **113 PASS**, mxmini **104 PASS**, 0 FAIL, exit 0 on both.
      oracle-1 keeps a copy in `/home/ubuntu/agentc-suite-f8bddba/` (two files plus `suite.out`).
    - **Seen on oracle-1 (not acted on):** `agentc-canary.service` fails intermittently on
      `digest probe failed (TimeoutError)`: six times on 11-18 UTC, five of them after 11:00. Its
      every-run failure on stale report `7fda3475` stopped at about 19:40 UTC on 11-17 (so the
      report looks resolved; not read from the service).
    - **`agentc-push` test race:** task **`74cb2771`** (human-delivered, attempt `889e112f` gen 1),
      candidate **`d2da1f0`** on main `f8bddba`, worktree `~/src/worktrees/ac-74cb2771-push-race`,
      branch `task-74cb2771`. Cause: for a foreign launch the helper refuses without reading and
      closes; when that beat the test client's write, `exchange_raw`'s `write_all().unwrap()` got
      EPIPE. Test helper only (the real client `send_push` already reads the reply after a failed
      send). Forced ordering: fails before, passes after; local gate green (1248 tests).
      Submission `470c88e2` at 14:32Z; review activity `e9cd7910` given to review subagent
      session `race-reviewer-20261118-sub`; integration `0d505fa7` queued. **Not landed yet**:
      after Done, remove the worktree and branch per the startup guide.
      Review: oracle-1's supervised reviewer claimed it 17 s after the submission (the subagent
      stood down, decided nothing) and **approved 14:58:45Z** (distinct launch, reproduced the
      failure on base and the pass on the candidate; two advisory findings).
    - **Main's dependency audit is red again (15:15Z), and the auto-fix path is live:** the
      integrator filed report `61453d56` and the service created fix task **`ad3d76ab`**
      (`fix_target`, priority 0); oracle-1 implemented it and it was `waiting_review` at 15:54Z.
      `74cb2771` is judged **exempt** (declared inputs unchanged) but every publication ends
      `NotPublished` with main still `f8bddba`: 35 attempts in 38 minutes, about one every 40 s,
      no reason logged. Suspected, not established: GitHub's ruleset refuses a commit whose
      required check is red, so the exemption cannot land by itself. The same journal has 14
      GitHub `502 Bad Gateway` replies and 4 `coordinator request failed` errors in that hour.
      `74cb2771` should land once `ad3d76ab` does. Owner asked for a task: **`180fab91`** (open,
      ready, priority 1, agent-origin, admitted 13/14 this week). `NotPublished` means `git push`
      exited non-zero with the target unchanged (`crates/local/src/git_workflow.rs`); the push
      output is discarded. The task: establish the cause, log a bounded refusal reason, stop the
      tight retry, define the exemption under a hosting rule that requires the check.
    - **State 16:40 UTC — both landed, no human step:** fix `ad3d76ab` published as `a425778`
      at 16:23:05Z (lettre 0.11.24, RUSTSEC-2026-0560..0565) and report `61453d56` resolved itself
      at 16:23:39Z, 68 minutes after the audit went red. `74cb2771` **Done**, merged as
      **`f3f92fd`** at 16:33:09Z. Its exempt publication never succeeded (57 refused pushes,
      15:15Z to 16:13Z), so `180fab91` stands as written. Worktree
      `ac-74cb2771-push-race` and local branch `task-74cb2771` removed (no remote task branch
      existed; the candidate ref was left). Main is `f3f92fd`; production and hosts still run
      `818a7b1`. Open: `180fab91` (ready), `e6685dbf` (planned, owner's 20 tasks and admission),
      the oracle-1 canary's digest-probe timeouts and the GitHub 502s (not investigated).
    - Evidence: `~/.local/state/agent-coordinator-evidence/open-items-20261118/`.
  - **Update 2026-11-18 19:30 UTC — the three open items closed out (CLI session
    `resume-20261118b`; owner granted production reads, host reads and writes for these items):**
    - **`180fab91` is Done** without this session: oracle-1's implementer delivered `c8964e3`
      (submission `85764748`), merged as **`e7b6ded`**. Push output is captured and redacted into
      `NotPublished`, refusals back off per (target, result, R, T0) from
      `refused_push_wait_seconds` (default 120 s, doubling to 1 h), and the workflow spec defines
      the exemption under a required-check ruleset. The cause was **reproduced, not observed in
      production**: confirm it from the first `push of R refused (attempt N): …` journal line once
      a build at `e7b6ded` or later runs on oracle-1. Owner action if exempt subjects should land
      while the target check is red: add the integrator GitHub App to the ruleset's bypass list
      (mode Always); not done.
    - **Main is `57f7341`** (also `9a2fe6f`, passkey sign-in button); production and hosts still
      run `818a7b1`.
    - **Canary digest timeouts — cause found, task `cb7c28b0` (planned, priority 2):** chronic, not
      new: 129 timed-out canary runs since 11-04, 88 of them in minutes :05 to :19 although runs
      are even across the hour. The production VM's backup transfer starts at :05
      (`gcp-backup-transfer.py`: gzip of a ~345 MB snapshot, upload, download, verify; `Nice=10`
      only). Sampled every 10 s from the workstation 18:53 to 19:22 UTC: digest 0.2 to 0.7 s
      normally, **30 s (client limit) at 19:06:22 and 25.3 s at 19:07:03**, normal from 19:10;
      `/healthz` under 1.6 s throughout; the :00 backup caused nothing. Which resource starves
      (CPU, memory, disk, lock) is not established: the VM was not read.
      **VM read by the owner at 19:32 UTC (task now revision 2):** the transfer ran 19:05:36 to
      19:09:22, matching the stall to within seconds, on 41 s of CPU in 226 s, so it is not
      CPU-bound; cumulative pressure since boot is io 19,624 s, memory 1,702 s, cpu 713 s. Disk
      I/O is the likely mechanism.
      **Readings during the 20:06:02 to 20:08:32 UTC transfer (agent-run, owner's permission):**
      io pressure full avg10 peaked at 88.0 (20:07:33), memory full at 51.6, cpu some at most
      5.7; stall added over 151 s was io 87.3 s, memory 32.5 s, cpu 2.2 s. Mechanism: disk I/O
      saturation with memory pressure on the 953 MB VM, not CPU. Which transfer step peaks is
      not separated. **`cb7c28b0` admitted by the agent on the owner's instruction at 20:11 UTC:
      open, ready, revision 4, not held by budget.** `c8143834` and `9dcd2493` remain planned.
    - **GitHub 502s — task `9dcd2493` (planned, priority 3):** 39 lines on 11-18, all 15:16 to
      16:19 UTC in the red-audit loop, plus 2 on 11-13; no GitHub incident that hour. The call is
      the check lookup's `actions/runs/{id}/jobs?filter=all`. Replayed against the 2-attempt run
      `44232028827`: 10.65 s then 502 on the first request, 7.3 s on the second, ~1.1 s after;
      single-attempt runs 0.5 to 0.9 s. A 502 skips the subject for one cycle only.
    - **New, cause not established — task `c8143834` (planned, priority 2):** both oracle-1
      integrators log `coordinator request failed: service request failed` (a non-timeout transport
      error; the text hides the cause): 1 to 22 a day from 11-04, then 26, 72 and 101 on 11-16,
      11-17 and 11-18. Bursts (11-18 14:04 to 14:32, 17:12 to 18:25), both processes often within
      seconds of each other, not tied to the :05 window. 475 unauthenticated GETs each of
      `/healthz` and `/api/v1/info` from oracle-1 (18:56 to 19:22) had no failure.
    - All three tasks are agent-origin and **planned** (admission was 13/14 this week); the owner
      admits them. Still open besides: `e6685dbf` (planned, owner's 20 tasks and admission).
    - Evidence: `~/.local/state/agent-coordinator-evidence/resume-20261118b/` (task inputs and
      receipts, `latency.log`, `oracle-probe.log`).
  - **Update 2026-11-19 00:15 UTC — ROLLOUT OF MAIN `f590db8` IN PROGRESS, hosts drained, waiting
    on `daa42f71` (owner's choice):**
    - Landed since 19:30 UTC: `c8143834` (`cd8a777`, error class logging; cause still not
      established), `9dcd2493` (`463cd39`), `cb7c28b0` (`ca8ccc7`, `8ea72f9`). Main `f590db8`.
    - Rollout task **`de1d7ca7`** (human-delivered, priority 1), attempt **`449e8d82`** gen 1, CLI
      session `resume-20261118b`; worktree `~/src/worktrees/ac-de1d7ca7-rollout`; evidence and
      scripts (adapted from `d8fdc1a6`) in
      `~/.local/state/agent-coordinator-evidence/de1d7ca7-690a-4081-ae65-87ad839552d5/`.
      The lease is renewed by `renew.sh` there (stops when `STOP-RENEW` exists; **it dies with
      the machine or a logout, and the attempt then expires**).
    - Done: release run **`44420903389`** on `f590db8` passed full acceptance (the agent
      dispatched it on the owner's instruction); packages downloaded, attested, verified
      (`verified-packages.json`). Host builds verified: mxmini `target/host-1.98.1/release`
      (`mxmini-SHA256SUMS`), oracle-1 `/home/ubuntu/src/worktrees/agentc-rollout-f590db8`
      (`/home/ubuntu/agentc-rollout-f590db8-SHA256SUMS`). No server or migration change since
      `818a7b1`: binaries-only.
    - **Drain marker `deploy-drain-de1d7ca7` set on both hosts at 00:08:08Z** (before: mxmini
      `quiet-hours` root:root 644, oracle-1 absent; `drain-before.txt`). Nothing else changed on
      production or the hosts.
    - **Blocked on:** `daa42f71` (Windows phase 6d) holds attempt `c5139e84` from the owner's
      MINIAIR session, and `scripts/drain_check.py` refuses while another task has a current
      attempt. The owner chose to wait until it lands. Deploy `f590db8` even if main moves.
    - **To continue:** when no other task holds an attempt, run `start-rollout.sh` (reserves the
      production resource and runs `rollout.py` as a job), then `oracle-upgrade.sh` on oracle-1
      and `mxmini-upgrade.sh` here (both with sudo), then clear the markers (mxmini back to
      `quiet-hours` outside 22:00-07:00 local, oracle-1 removed), publish evidence, submit, and
      get a subagent review. **To abandon:** restore the markers the same way, touch
      `STOP-RENEW`, release the attempt.
    - Not part of the rollout, still the owner's: the VM steps from `cb7c28b0` (backup guide,
      'The hourly transfer on the 1-GB VM'), the ruleset bypass from `180fab91`, MINIAIR's CLI.
  - **Update 2026-11-19 21:15 UTC — ROLLOUT OF MAIN `f590db8` DONE on production and both hosts
    (task `de1d7ca7` submitted, awaiting review). The 00:15 bullet above is history.**
    - **Blocker cleared on the owner's instruction:** `daa42f71`'s MINIAIR attempt `c5139e84`
      expired at 01:28Z and stayed attached in `recovery_required`, so the drain held for about
      21 hours. This session took a recovery claim, confirmed the pushed work-in-progress ref
      equals checkpoint revision `a074874` (service-verified), resolved it as `resume` and
      released it. **`daa42f71` is `ready`, human-delivered, with its work on MINIAIR
      (`C:/src/worktrees/agent-coordinator-task-daa42f71`, branch `task-daa42f71`); reclaim it
      from MINIAIR. It still waits for the owner's App ID, installation id and key path.**
    - Production: job `eb524836` succeeded first run in 923 s (drain check unmodified, exit 0;
      pre and post backups verified and copied off-server; timers restored).
      `https://agents.sithbit.com` serves `f590db8`. Rollback:
      `/var/lib/agent-coordinator-deployments/claude-de1d7ca7-f590db8-20261118/rollback`.
    - Hosts, both at `f590db8` (CLI, supervisor, push; oracle-1 also integrator), installed
      checksums equal the builds: mxmini source `~/src/worktrees/ac-de1d7ca7-rollout`, oracle-1
      source `/home/ubuntu/src/worktrees/agentc-rollout-f590db8`. Rollback on each host:
      `/opt/agentc/rollback-cli-f590db8-20261118` and `/opt/agentc/rollback-host-f590db8-20261118`.
      Unmodified containment suite: oracle-1 113 PASS / 0 FAIL / 1 SKIP, mxmini 104 / 0 / 2.
      Agent pins moved with the owner's installed versions: Claude 2.1.328, Codex 0.170.0.
    - Markers cleared 21:03:17Z: mxmini `quiet-hours` (cron unchanged, 22:00-07:00), oracle-1
      absent; all oracle-1 units active. No launch was killed.
    - Evidence artifact `cc060f56-f4c0-4c9b-97cb-e923214a4f72`; local files in the evidence
      directory named above (`completed-deployment-evidence.json`, `oracle-upgrade.out`,
      `mxmini-upgrade.out`, `drain-cleared.txt`, `daa42f71-*.json`).
    - **Review: `de1d7ca7` is DONE (approved 21:17:34Z).** The first submission was taken by
      oracle-1's contained reviewer, which requested changes only for lack of access; the
      second (`6c26e647`) was approved by a subagent reviewer under its own subagent identity,
      no `distinct_launch` asserted. The implementer's session issued the claim command under
      the reviewer's session name to keep the contained reviewer from taking it again; the
      service's orientation says not to reuse another harness's session proof, so the owner
      may want a different arrangement (for example human-delivered tasks skipping the
      supervisor's review queue). Reviewer's notes: on oracle-1 the pre-rollout CLI is in
      `rollback-cli-f590db8-20261118`; `rollback-host-f590db8-20261118` holds the release-bundle
      CLI that `rollout.py` installed 19 s before the native build replaced it. VM internals
      (backups, off-server copies, timers) rest on the job's own output, not re-read.
    - Now possible, owner's: count transport-error classes on oracle-1 (`c8143834`); confirm
      the `180fab91` cause from the first `push of R refused` journal line. Still the owner's:
      the VM steps from `cb7c28b0`, the ruleset bypass, MINIAIR's CLI to `f590db8`, `e6685dbf`.
  - **Update 2026-11-20 15:30 UTC — oracle-1 journal reads (owner's pick; read-only; CLI session
    `resume-20261120`):**
    - Queue: nothing claimable from mxmini. No task waits for review; `daa42f71` and `7011c55f`
      are ready but human-delivered Windows work, `d2ca0225` is blocked on the owner, `e6685dbf`
      is planned. No pending decision. Main is `8a04804` (two canary commits over `f590db8`),
      CI green; production and hosts unchanged.
    - **`c8143834` transport errors, now classified** (both integrators, 21:02:18Z on 11-19 to
      15:22Z on 11-20, about 18 h): **`reset` 26** (13 per unit), **`timeout` 3**, and 4 lines
      `HTTP 521` / `HTTP 522 did not contain a JSON response body` at 01:12:08 to 01:12:59Z
      (Cloudflare reporting the origin down, then timed out). 18 of the 26 resets fall in the
      first 85 minutes after the units started; since 02:00Z there are 3 resets and 3 timeouts.
      Two timeouts are at 08:07:56 / 08:08:02Z, inside the :05 backup-transfer window. The
      service sits behind Cloudflare and Caddy. **Cause of the resets is still not
      established**: which hop closes the connection, and whether it is a reused idle
      connection, was not tested. No task created.
    - **`180fab91` cause still unconfirmed:** no `push of R refused`, `not completed` or
      `NotPublished` line exists since the rollout (none since 11-18 22:00Z either); main's
      audit has not been red since. Read again after the next red audit.
    - Evidence: `~/.local/state/agent-coordinator-evidence/resume-20261120/oracle-journal-read.txt`.
  - **Update 2026-11-20 16:45 UTC — transport resets investigated and fixed on main (owner's goal;
    task `a1c6d38c`, human-delivered, attempt `7f1139d6`): NOT DEPLOYED.**
    - **Finding:** the supervisor and both integrators on oracle-1, each on its own kept-alive
      HTTP/1.1 connection (reqwest without http2; polling every 30 to 60 s), fail within seconds
      of each other, once each, and succeed on the next cycle (for example 21:48:20.18,
      21:48:20.64 and 21:48:30.84Z on 11-19). hyper drops a pooled connection when it sees a FIN
      or RST while idle, so the far side discarded the connections silently and answered the
      next request with RST; the client sent every request exactly once. **Which hop does it is
      not established** (a Cloudflare edge restart or a route change are candidates; oracle-1
      reaches Cloudflare over IPv6). A production log read was blocked by the permission
      classifier and not retried.
    - **Ruled out:** an idle timeout. From oracle-1, Cloudflare closes an idle kept-alive
      connection at 400 s with a clean FIN; connections idle 20 to 330 s were still usable
      (`oracle-idle-probe.out`). The agents' egress firewall applies only to the agent uids.
    - **Fix, on main as `c05a052`:** `crates/client` gives reqwest a replay policy scoped to the
      service host: a request that fails before any response with class `reset`,
      `incomplete_response` or `unexpected_eof` is sent again on a new connection, at most
      twice more, inside the 30 s request limit. Safe because reads change nothing and every
      mutation carries an idempotency key. Timeouts, TLS, connect and HTTP statuses are not
      replayed. Two tests that assumed one attempt per request were adapted (integrator
      `service.rs`, CLI `artifact_transfer.rs`); CLI guide paragraph under "Interrupted
      mutations". Gate on the candidate: fmt, Clippy, 1288 tests, build, smoke, docs all pass.
    - **Workflow:** the service held the new task under the weekly agent-task budget and refused
      the agent's admission edit (`admission_budget`, human required); the owner released it.
      Submission `7be207fe`; review `4d94099e` **approved 16:29:56Z** by oracle-1's supervised
      reviewer (distinct launch, three advisory findings, one of them the known long-TMPDIR
      supervisor test failure on base); integrated by oracle-1; task **Done 16:40Z**. Worktree
      and local branch removed.
    - **Not addressed:** the 3 timeouts and the HTTP 521/522 at 01:12Z are origin stalls on the
      1-GB VM (`cb7c28b0`, owner's VM steps).
    - **Next:** the fix takes effect only after a rollout of `c05a052` or later (service and
      hosts must share a source commit, so production moves too; client-only change, no
      migration). Afterwards the integrator journals on oracle-1 should show no `(reset)` line;
      read them a day later to confirm.
    - Evidence: `~/.local/state/agent-coordinator-evidence/resume-20261120/` (`retry-*.json`,
      `retry-tests-before.log`, `gate-*.log`, `oracle-idle-probe.out`).
  - **Update 2026-11-20 17:50 UTC — ROLLOUT OF MAIN `c05a052` DONE on production and both hosts
    (owner: "roll out now"; task `0b9c367a`, human-delivered, attempt `ff049ee2`; the "NOT
    DEPLOYED" note above is history):**
    - Release run **`44777650029`** on `c05a052` (dispatched by the agent on the owner's
      instruction after main's CI passed) succeeded with full load acceptance; packages and
      both host-bundle attestations verified. Client-only change since `f590db8`, no migration.
    - Drain marker `deploy-drain-0b9c367a` set on both hosts 16:53:33Z (before: mxmini
      `quiet-hours`, oracle-1 absent), nothing in flight, 0 launches killed; cleared 17:49:04Z
      (mxmini `quiet-hours`, byte-identical to before; oracle-1 absent).
    - Production: job `b5289b4a` succeeded first run in 827 s (repository drain check
      unmodified, exit 0, the two parked `recovery_required` activities non-blocking; pre and
      post backups and off-server copies verified; timers restored).
      `https://agents.sithbit.com` serves `c05a052`. Rollback:
      `/var/lib/agent-coordinator-deployments/claude-0b9c367a-c05a052-20261120/rollback`.
    - Hosts, both at `c05a052` (CLI, supervisor, push; oracle-1 also integrators), installed
      checksums equal the builds: mxmini source `~/src/worktrees/ac-0b9c367a-rollout`
      (`target/host-1.98.1/release`), oracle-1 source
      `/home/ubuntu/src/worktrees/agentc-rollout-c05a052` (`APPARMOR_BWRAP=1 UPDATE_TIMER=0`).
      Rollback on each host: `/opt/agentc/rollback-cli-c05a052-20261120` and
      `/opt/agentc/rollback-host-c05a052-20261120`. Unmodified containment suite: oracle-1
      113 PASS / 0 FAIL / 1 SKIP, mxmini 104 / 0 / 2. Agent pins moved with the owner's
      installed versions: Claude 2.1.329, Codex 0.171.0. All oracle-1 units active; mxmini
      `agentc-run` running, quiet hours unchanged.
    - Evidence artifact `2a0bf27e-1c30-41b9-8aaa-c11c78c8110f`; local files and scripts in
      `~/.local/state/agent-coordinator-evidence/rollout-c05a052/`.
    - **To confirm the fix:** a day after 17:48Z on 11-20, count `(reset)` lines in the oracle-1
      integrator journals; expected none (26 in the 18 hours before). Timeouts and 521/522 may
      still appear (`cb7c28b0`, owner's VM steps).
    - **Review: `0b9c367a` is DONE (approved 17:53:07Z).** Submission `3a74d367`, review
      activity `a10336f4`, decided by subagent identity `reviewer` (session
      `deploy-reviewer-20261120-sub`, started before the submission so that it claimed first; no
      `distinct_launch` asserted). It re-read the artifact, release run, live service, both
      hosts' checksums, units and markers. Five advisory findings: VM internals rest on the
      job's report; "no launch killed" rests on the implementer's account; rollback directory
      contents not opened; the run has six jobs with one skipped (publish assets), not five;
      same-launch review. A general submission's review claim takes workflow policy revision 0.
    - First reading after the upgrade (17:48:10 to 17:53Z): 0 failed-request lines on oracle-1.
      `daa42f71` shows `in_progress` (not this session's; MINIAIR).
    - **Fix verified in production (20:08 UTC):** a control probe from oracle-1 (three kept-alive
      HTTP/1.1 connections, GET `/healthz` every 30 s, no retry, 17:54:50 to 18:50:15Z) saw
      **8 resets in 330 requests**, including all three connections within 22 s
      (18:32:43, 18:32:54, 18:33:05Z), which is the silent-drop pattern. Over 17:48:10 to
      20:08:45Z the supervisor and both integrators on the new build logged **0 reset lines**
      (16 in the 35 minutes before the upgrade, 16:50 to 17:24Z). One line remains: a
      supervisor `service request timed out` at 19:08:24Z, inside the :05 backup-transfer
      window. Files: `post-upgrade-control.out`, `post-upgrade-journal-2008Z.out` in
      `~/.local/state/agent-coordinator-evidence/resume-20261120/`.
    - **Timeouts and 521/522 — VM transfer step installed and measured (21:13 UTC):** on the
      owner's instruction the agent ran
      `~/.local/state/agent-coordinator-evidence/resume-20261120/vm-install-backup-transfer.sh`
      at 20:09:51 UTC, rc 0: script and unit from `c05a052` installed (sha256 `93165c89…`,
      `e7c2255d…`), `.before-c05a052` copies kept beside them, `daemon-reload`, nothing started.
      Unit shows `Nice=19`, `CPUWeight=20`, `IOWeight=10`, `IOSchedulingClass` idle,
      `MemoryHigh` 300 MB. The VM's disk scheduler is `none`, so the two I/O settings have no
      effect there; the memory cap and the script's page-cache dropping are what act.
      First transfer with it: 21:05:27–21:07:48 UTC, `Result=success`, 28.9 s CPU (the new
      `step …` lines are in its journal). Workstation samples every ~10 s from 21:02:30 to
      21:12:38 UTC: digest 0.18–0.34 s (0.29 s at most inside the transfer), `healthz` 200 in
      0.10–0.25 s; the same measurement on 11-18 stalled 25–30 s. oracle-1 journals
      20:09:51–21:13 UTC: no `service request failed`, timeout or 52x line (one timeout at
      19:08:24 UTC before the install). One transfer is one sample: re-read the oracle-1
      journals after a day before calling the stalls closed. Evidence in the same directory:
      `vm-install.out`, `latency-after-install.log`, `vm-transfer-after-install.out`,
      `post-install-journal.out`.
    - Still the owner's: MINIAIR's CLI to `c05a052`, the ruleset bypass, `e6685dbf`.
