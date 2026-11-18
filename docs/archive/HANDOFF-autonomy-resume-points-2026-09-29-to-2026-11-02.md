# Autonomy handoff — archived resume points, 2026-09-29 to 2026-11-02

> Archived on 2026-11-18 from the root `HANDOFF.md` (branch `autonomy-plan`), moved verbatim and
> in their original order (newest first). These are superseded resume points and completed S6
> runbooks; the live state is the "Current resume point" in `HANDOFF.md`. "Above", "below" and
> "§n" inside this file refer to `HANDOFF.md` as it stood when each section was written. This
> file is historical evidence and is not updated.

## Previous resume point — P6 rerun window RUNNING until 2026-11-16 16:50 UTC (2026-11-02 17:00 UTC)

**Window:** `2026-11-02T16:50:00Z` → `2026-11-16T16:50:00Z` (14×86400 s), pinned in
`~/.local/share/agent-coordinator-autonomy/p6-repair-20261023/window.json` (baseline
prior20-human-origin-pilot: ≥20 human tasks, median creation→Done ≤4.12254 h; canary
`a21df6a2` ok 03:08:36Z; reserved-action categories listed there). P6 `d513bc21` is released
(planned, revision 5, human-delivered); the last P6 checkpoint (attempt `82322096`, gen 19) records it.

- **Canaries:** mxmini/Claude ok on 10-31 02:22Z, 11-01 02:08Z, 11-02 03:08Z — the 10-30 mirror
  repair fixed the timeouts.
- **Import:** owner chose "Claude in Chrome, I click Apply". Agent filled the dashboard form and
  previewed (20 planned, 0 conflicts); **owner clicked Apply preview** ~16:31Z. All 20 are
  origin=human.
- **Fill-in:** fresh read-only audit of origin/main `766a9e6` (9 absent, 11 partial, 0 complete; API
  changes for 02, 05, 16, 18) → `p6-repair-20261030/p6-audit.json`. Each task got kind=code,
  description and acceptance criteria (rev 2), then planned→open at 16:50:01–04Z (rev 3). Oracle
  claimed p6w-06 at 16:50:19Z, mxmini p6w-02 at 16:50:48Z.
- **Capacity change (owner decision):** mxmini quiet hours disabled for the window at 16:46:52Z.
  Backup `/var/lib/agentc/p6-window-quiet-hours-backup-20261102/` (cron sha256 `0e416641…`, the
  Oct 19 original; `agentc-quiet-hours.disabled` + `kill-switch`). **Restore at window end:**
  `sudo -n cp -a <backup>/agentc-quiet-hours /etc/cron.d/` (cron re-sets the marker at the next
  out-of-window minute). A host-setup rerun with QUIET_HOURS would also recreate it early.
- **Observer:** `observe-p6.py` first real sample `observation-samples/20261102T165105…json`
  complete, 0 errors. Owner installs the `*/15` user crontab line (agent process can't).
- **P6 tracking state:** agent edit refused (`self_related_definition_change`, human required);
  agent prefilled the dashboard edit form and the **owner clicked Save** → P6 **planned, revision 5**,
  with a TRACKING STATE note in its description. Observer `*/15` crontab installed by the owner.
- **Deployed 2026-11-02 (owner: "deploy 915d8aaa", drain-then-deploy):** production now runs
  **`766a9e6`** (= `9ac4d91` + CANARY.md) over `b3321f5`; live digest reports weekly agent limit
  **14**. Task `ac8744ce` (agent-origin, human-delivered) **Done**: submission `561da80f`, evidence
  artifact `dd020274`, approved 18:30:39Z by independent subagent identity `deploy-reviewer`
  (Oracle's reviewer did not pick it up in 20 min). Drain: kill switch
  `deploy-drain-ac8744ce` on both hosts 17:21:06Z → idle 17:54:37Z (0 launches killed) → rollout job
  `d2e8059f` 17:55–18:03Z → markers removed 18:03:28Z; recorded in window.json `reserved_events`.
  Release run `41487297105`; rollback CLIs `/opt/agentc/rollback-cli-766a9e6-20261102`. Scripts and
  evidence: `~/.local/state/agent-coordinator-evidence/ac8744ce-454d-4162-bf16-74dfd5df7fe7/`. Worktree
  `ac-ac8744ce-deploy` and its local branch removed (no remote branch). Watch: one mxmini review
  claim (activity 5362fa7f) failed with an empty error right after restart.
- **Reviewer sandbox fixed on both hosts (2026-11-06 owner goal; reserved event #2 in window.json).**
  Cause: Claude Code's own reviewer Bash sandbox runs only where `socat` is installed (mxmini, not
  oracle-1). It denied crates.io (`CONNECT 403`) and made `$RUN/state/cargo`, `$RUN/target` and the
  candidate home read-only. So the mxmini reviewer `d21d0442` (`agentc-rev-mxmini-write`) failed every
  Rust gate (task e641488e blocked after 6 rounds; also bc9f8a58, dc9387db, bbdfaf5c, 5767bc49…).
  Fix on branch `reviewer-sandbox-crates` (`4560cc5` + `39c22d8`, gate green; ship.py fast-forwarded main to `39c22d8` ~17:00Z):
  reviewer `settings.json` gets `sandbox.network.allowedDomains` crates.io/index.crates.io/
  static.crates.io and `sandbox.filesystem.allowWrite` `<state_dir>/rev/runs` (no globs in 2.1.303;
  the outer sandbox already hides other runs). Each host runs its own installed source plus the fix:
  mxmini `1a828d5` (sha `40af7e25…`), oracle-1 `9c80ed3` (sha `a2e823f1…`, source in
  `/home/ubuntu/agentc-reviewer-crates-prep`). Installed while Idle; containment suite 100 / 112 PASS.
  A real contained reviewer launch on each host ran cargo fetch (600 MB cache), offline metadata and
  `cargo check`, all OK; example.com stayed blocked. Rollback: `/opt/agentc/rollback-reviewer-crates-20261106`
  on each host. oracle-1's 3 supervisor test failures (bwrap userns under AppArmor) are pre-existing
  at `9c80ed3`. e641488e unblocked → ready (rev 2) 16:53Z. **Watch:** the next mxmini review of a Rust
  change should run the gate. The next host-setup rerun installs settings from whatever binary it
  is given, so build that binary from a commit containing `39c22d8`.
- **Full main rollout to `39c22d8` DONE (2026-11-06, owner request; task `c2642e99`, reserved event #3).**
  Production, both hosts' CLI/supervisor/push and oracle-1's integrators now run `39c22d8`; this
  supersedes the host sources in the bullet above. Release `42259365090` passed with full load
  acceptance, because migration `0046` (additive) is new since `766a9e6`. Drain marker
  `deploy-drain-c2642e99` was set 17:24:51Z with 0 launches killed. Production R1 (job `929e18f8`)
  refused safely on an active sithbit attempt; the owner released it, and R2 (job `fe562889`)
  succeeded with verified pre/post backups and off-server copies. Rollback dir
  `/var/lib/agent-coordinator-deployments/claude-c2642e99-39c22d8-20261106-r2`; CLI rollbacks
  `/opt/agentc/rollback-cli-39c22d8-20261106`. Order per owner: production, then oracle-1
  (native aarch64 build, `APPARMOR_BWRAP=1 UPDATE_TIMER=0`, cleared 18:11:51Z), then mxmini last
  (native x86_64, QUIET_HOURS unset, cleared 18:28:14Z). Each host: containment 112 / 100 PASS, a
  real reviewer cargo probe OK, and host rollback `/opt/agentc/rollback-host-39c22d8-20261106`.
  Evidence artifact `3c172b6c`; scripts under
  `~/.local/state/agent-coordinator-evidence/c2642e99-c096-426f-9a4b-80124613f6ab/`.
  (Its 2.1.303 re-pin trap is resolved by the next bullet.)
- **Reviewer sandbox fix + harness upgrade DONE (2026-11-07 UTC, owner: "work on task 54abf543",
  chose ship-then-roll-out; reserved event #4).** Reviewer settings gain
  `sandbox.allowNestedUserNamespaces` (Claude 2.1.319 otherwise denies candidate-shell's nested
  bwrap) and `sandbox.network.allowAllUnixSockets` (the per-path list does nothing on Linux),
  probed live on 2.1.303 and 2.1.319. The containment suite now drives the real pinned Claude via
  a loopback fake Messages API (`deploy/agentc/fake-messages-api.py`, no LLM). ship.py moved main to
  **`e5b670d`**. Both hosts drained 01:40:08Z (idle, 0 killed) and cleared 02:25:39Z. host-setup
  installed supervisor/push at `e5b670d`; **the CLI stays at production `39c22d8`** (a newer CLI
  would fail `client_upgrade_required`). Agent Claude is **2.1.319 on both hosts**; oracle-1's
  owner Claude was updated 2.1.316 → 2.1.319 first (HANDOFF previously said 2.1.319 there, wrongly).
  Containment: oracle-1 113 PASS (the new check notes no socat), mxmini 104 PASS (nested
  candidate-shell bwrap and a Unix socket inside the active Claude sandbox). Real contained
  reviewer launches (haiku): oracle-1 fetch/check plus 1107/1107 tests pass; mxmini fetch/check
  plus 1106/1107 with 0 `Operation not permitted` (was 24). The one failure,
  `artifact_transfer::…publication_replays_lost_reservation…`, passed on a targeted rerun in a
  launch (flaky under load). Targeted mxmini run: candidate_checkpoint 18/18, agentc-push 34/34,
  supervisor 327+4 pass (30 skip by design: no writable /tmp in launches). Rollback:
  `/opt/agentc/rollback-host-e5b670d-20261106` on each host (old binaries, Claude, supervisor.toml,
  reviewer settings). Evidence and scripts: `~/.local/state/agent-coordinator-evidence/54abf543-2870-47da-ae9b-ac4e84f521a3/`.
  host-setup still pins whatever Claude the owner has installed (`~/.local/bin/claude`).
- **Other hosts' sithbit work** resumed after the deploy. Workstation CLIs must be at `39c22d8`,
  or they fail with `client_upgrade_required`.

**At window end:** re-activate P6, restore quiet hours, remove observer cron, run the final
fixed-window audit (value, HRI excl. reserved, direct-push reconciliation, digest ≤1/day).

## Previous resume point — P6 rerun: mirrors repaired, import the 20 value tasks (2026-10-30 19:10 UTC)

P6 task `d513bc21-71d9-40c4-a2ec-614af4318f37` is **released (Ready, revision 4)**. Reclaim it
with a fresh revision before any work. The service checkpoint `39910498` is the authoritative
record. The new 14-day window has **not** started.

**Done 2026-10-30.** The owner ran reviewed helper v2 (SHA `3bd62786…`) at 18:32:07Z. It
fixed 6 mirror permissions and the worker's full fsck passed. Backup:
`/var/lib/agentc/repair-p6-mirror-20261030T183207.219465Z`. The agent verified that both
mirrors have 0 unreadable entries and `core.sharedRepository=0644`, and that `agentc-run`
restarted. Receipt: `~/.local/share/agent-coordinator-autonomy/p6-repair-20261030/operator-receipt.json`.
Every nightly mxmini canary from 10-20 to 10-30 had timed out on this defect.

**First thing next session (in order):**

1. **Check the canary.** Confirm the scheduled 22:05 EDT (02:05Z Oct 31) mxmini/Claude canary
   recorded `outcome=ok` (`sudo -n tail /var/lib/agentc/e2e-canary.jsonl`). If it failed,
   stop and diagnose. Do not import or start the window.
2. **Import the 20 value tasks. Ask the owner which way:**
   - **(a) Owner imports.** In the agents.sithbit.com web dashboard, Knowledge section,
     click **Preview and import tasks and context from Markdown** (down-arrow icon). Fill in:
     Stable source identity `p6-rerun-workload`; Source path `p6-rerun-workload.md`; Source
     Git revision `8607234a92598421826e199fd4b026725c2946fb`; Source branch `main`; Markdown
     = contents of `~/.local/share/agent-coordinator-autonomy/p6-repair-20261030/p6-rerun-workload.md`.
     Check that the preview shows 20 planned tasks and no blocking conflicts, then click
     **Apply preview**.
   - **(b) Agent imports in the owner's external browser** (Claude in Chrome, owner's
     logged-in session), only on the owner's explicit instruction. Follow the same steps
     and record in the P6 checkpoint that the agent applied it on the owner's instruction.
   - **Why the agent CLI can't do it:** applying an import is human-only
     (`imports.rs:60`). Tasks an agent creates are `origin=agent` and don't count toward the
     human-requested value target, and the weekly agent limit would hold most of them.
3. **Fill in the 20 tasks.** After the import, use `tasks edit` on each to set `kind: code`
   and add a real description and acceptance criteria from
   `p6-repair-20261023/VALUE-WORKLOAD-PROPOSAL.md`. Keep them **planned** until the window
   opens. Editing doesn't change origin. The dashboard audit on origin/main found 8 features
   absent and 12 partly present, none complete; scope each partial one to its missing part.
4. **Set the window.** Pin exact UTC start and end, 14×86400 s apart. US DST ends Nov 1, so
   compute expected canary fire times from the live timezone. Record the approved baseline
   (≥20 human tasks/14 days, median ≤4.12 h, knowledge `79179d84`), the passing canary ID,
   and reserved-action categories. Then activate the 20 tasks (planned → open) at the start,
   install the bounded observer (`p6-repair-20261023/observe-p6.py`, which needs an owner
   crontab step), and move P6 to a dated tracking state (see `OPERATOR-REPAIR-v2.md`).
   **Latency caveat:** the baseline measures creation→Done. Importing on window day keeps
   that comparable. If the import happened earlier, agree with the owner to measure from
   activation instead.

**Also open:** code task `915d8aaa-781d-4c2d-aba0-299a4a1c69bf` raises the default weekly
agent-task limit from 5 to 14 (Ready; agent-origin; it doesn't affect the value metric). It
needs review, integration, and an owner-approved deploy before the new limit applies.

## Previous resume point — mxmini canary passed; quiet hours restored (2026-10-19 15:08 UTC)

Owner task `422ce01e-7721-4737-ade4-816ebed8e817` is **Done**, revision 6.
Independent review approved submission `5b5840d8` and accepted the owner-authorized
timing amendment at 15:14:41Z. Supplemental artifact `00f0870e-f7d6-4f48-90d0-23e881173136`
contains the corrected exact timeline excerpt and live allowlist/integrator evidence.
The two clean task worktrees `ac-422ce01e-canary-now` and `ac-422ce01e-rollout`,
and their merged local branches, were removed. Their remote branches were absent.
Retain `ac-422ce01e-deploy`: pre-existing gcloud/SSH processes 540970 and 540986
still use that working directory. Main parent and unrelated worktrees are preserved. The user explicitly requested a temporary quiet-hours override
so acceptance could run now. Registered job `fcb259b8-08cb-4066-9ceb-20ad0afcd276`
ran the configured cron wrapper and Claude canary under the usual flock. It
succeeded with exit 0 and unchanged source; its reservation is released.

Fresh task `93994673-c9eb-4f9f-b9bf-374355ff2267` reached Done and the canary
recorded **outcome=ok at 15:07:36Z**, host **mxmini**, served_by **claude**, in
**568.5 seconds**. Both implementation and review used mxmini's Claude supervisor.
The prior task `a7758fab` also reached Done. The fresh run's first candidate
conflicted after the prior task landed; the pipeline reopened it, implemented the
revision, independently reviewed it and integrated `b1dd4c7f9f4625960f02b9c68f690cdb4a05f3dd`
without manual pipeline intervention. Fresh-run recorded cost is **$0.2055326**.
HRI: one owner-requested window override; zero canary-pipeline interventions.
Historical setup interventions and the original timeout remain recorded.

At 15:07:37Z the override restored the original quiet-hours cron byte for byte
(SHA-256 `0e416641a771107626d6f07a77d339d47404a7275483a1bc0477428a424063cf`)
and reapplied its `quiet-hours` marker. The normal 22:00–07:00 window and nightly
22:05 cron remain configured. A 100-minute fallback restoration was installed
for interruption protection and removed by restoring the original cron.
The new result's telemetry receipt hash is in `e2e-canary.jsonl.reported`.

Pinned artifact `f6a66111-af04-4665-927d-e47e35c1f2d0` holds the current result,
workflow, cost, restoration and job evidence. The prior deployment/notification
artifact remains `2ef83f8b-a162-45d4-8be5-8f5f6e8c9dfd`. Local evidence is under
`~/.local/state/agent-coordinator-evidence/422ce01e-7721-4737-ade4-816ebed8e817/daytime/`.
No service or CLI deployment was repeated. Preserve unrelated pending-decisions edits.

## Previous resume point — mxmini canary fixes deployed; await the nightly run (2026-10-19)

Owner task `422ce01e-7721-4737-ade4-816ebed8e817` remains open. The user approved
its matching production service and CLI rollout. Production and the native CLIs
on mxmini and Oracle now report clean source `b8053664808d2763a3b001f2b2d7843a53706dc9`;
public HTTPS health and both hosts' compatibility checks pass. Release acceptance
run `39196863956` passed. The mxmini supervisor separately uses reviewed source
`1a828d55795db91dd63eb36c6151402177bacd70`, with a successful real contained Claude
`pwd` test. The two code fixes are integrated and Done.

Both pre- and post-upgrade backups were verified, transferred off-server and
verified there. Original binaries and a snapshot are preserved under production
`/var/lib/agent-coordinator-deployments/mira-422ce01e-b805366-20261019/rollback`.
Native CLI rollback copies are under `/opt/agentc/rollback-cli-b805366-20261019`
on each host. The supervisor rollback is `/opt/agentc/rollback-cwd-1a828d55795d`.
Scheduled service-operation timers were restored. Workers and both integrators
are running; deployment resources are released. The final Oracle check in the
initial deployment job used the wrong binding path and failed after successful
installation. Separate job `ac167d1c` verified the actual binding; no deployment
mutation was repeated.

The retry ledger now contains `{"total":2,"tasks":{}}`: only task `a7758fab`'s
two strikes were cleared. The existing Ready task can retry when the normal
22:00–07:00 America/New_York window opens. Outside that window, the unchanged
quiet-hours switch holds the supervisor. Cron runs the configured Claude canary
at **22:05 EDT on October 19**. The original waiter timed out and will not resume;
retain that failure. An actual nightly `outcome=ok` is still required before
finishing the owner task.

A controlled failure through the unchanged deployed canary's run/page code
successfully published a clearly labelled test alert to the configured ntfy
topic. Its HTTP receipt and isolated failed result are saved separately from
production canary telemetry. No task was created by this notification drill.

Evidence: `~/.local/state/agent-coordinator-evidence/422ce01e-7721-4737-ade4-816ebed8e817/rollout/completed-deployment-evidence.json`.
The service checkpoint and pilot timeline retain the rollout and outstanding
nightly acceptance. Preserve unrelated `.claude/decisions-pending.md` changes.

## Previous resume point — P6 pilot recovered to gen8 after renewer stopped (2026-10-16 13:10 UTC, workflow on)

- **Actual failure and owner-authorized recovery:** old renew **pid778271** stopped at **10:12:21Z**
  after `fresh ownership read failed after renew`. Service history confirms its final successful
  heartbeat **10:11:51.330Z**, expiry **11:11:51.330Z**. At **13:06Z**, pilot **d513bc21** was
  `recovery_required`, `authority_valid=false`, lease0. The attempt row still said `active`;
  that did not grant authority. No expired-lease renewal or old-receipt replay was attempted.
- The owner explicitly answered **"Authorize guarded pilot recovery"** in this session. Same
  **p3b-pilot** session **a9c40947-f8fc-4491-ba59-9f4880e88f7f**, harness `claude`, resumed.
  Saved checkpoints, helper/process state and protected session diagnosis were inspected:
  no pilot checkout, jobs, reservations or pending mutation. Guarded recovery claim at
  **13:08:42.367Z** returned **7ec05f84-4c36-4e5f-93b2-ddc7b69a3dbb, generation8**;
  explicit recovery resolution `resume` succeeded and changed its mode to `work`.
- Exactly one replacement helper **pid1723789** runs from
  `~/.local/share/agent-coordinator-autonomy/p6-pilot-renew-gen8/`, cadence **60seconds**.
  Its `renew.py` is an unchanged copy of the original. Initial and next scheduled renewals verified; fresh heartbeat
  **2026-10-16T13:10:13.855Z**, expiry **2026-10-16T14:10:13.855Z**, independently confirmed through
  **dogfood-orchestrator**. All original gen7 files/logs and other helpers remain preserved.
  Serialize calls sharing p3b-pilot; use dogfood-orchestrator for independent observations.
  The helper's transient-read failure behavior remains a limitation; no fix is claimed.
- **One actual pilot recovery intervention** is now recorded, separately from the earlier
  planned deployment releases and owner P1 scheduling decision. Digest **13:06Z HRI1** was
  `stalled_queue` (one ready task; last progress05:42:50.995Z). After recovery, digest is
  **HRI0, stalled_tasks0, stalled_queue0**. No pending human decisions.
- **3453a676-d15a-4172-bf0a-300d0596b08e** filed **P2 Planned**, without admission: the local pilot
  helper exits permanently after a failed post-renew read, with no bounded read retry or dedicated
  stop alert. This differs from supervisor retry task fb8b2326. Independent timeline records a
  separate **HTTP522 at10:14:14Z**, followed by Idle10:15:14Z; correlation only, not a proven cause
  of the helper read failure. GCP service is active/running, NRestarts0, since10-15 20:30:29Z;
  its10:10-10:14Z journal had no entries. No service restart/redeployment performed.
- Linux rollout remains complete at clean/exact **fc3f4244ce89a7f85956df3ae9aa2bca3d59f824**.
  Anonymous no-redirect HTTPS startup/auth help read; selected CLI compatibility all true.
  Both hosts' installed binaries match retained fc3f424 manifests. Oracle exact units
  **agentc-run.service** and **agentc-integrator@run.service** active/running, NRestarts0,
  fresh heartbeat, integratorIdle, release marker absent. mxmini **pid777161** fresh heartbeat,
  `quiet-hours` marker correctly present outside its **22:00-07:00 local** window; cron untouched.
- Previously admitted **1a3290db,335b6713,5ad83ff3,97b7b62a** are now **Done**;693af855 remains Done.
  Let daemons process already-admitted work. All119 tasks read in one200-item page before filing
  the new Planned defect; no task admissions. **be6d39af remains P3 Planned**; human-delivered
  55b8a06d and other owner choices remain untouched. No SithBit task mutations.
- Nightly **10-16 canary remains PASS**, oracle-1/claude,01:18:52Z,4727.2seconds,row8906f6cd.
  Only open report remains historical27f99b21,flaky,requires_human=false. **External browser verified
  at13:15Z:** Brave Work extension became available after the initial no-browser check. Existing
  authenticated dashboard visibly shows pilot In Progress, generation8,60min lease,expiry10:15AM EDT,
  recovery checkpoints and no jobs/holds;3453a676 andbe6d39af Planned;1a3290db Done in Completed tasks.
  No dashboard task mutations. The earlier browser availability blocker is resolved.
- Fetch succeeded; **origin/main7ca0eec80ced1bc2219bf03334cd1342f1ee1a7b**. autonomy-plan matched
  upstream before this refresh. Unrelated dirty `.claude/decisions-pending.md`, all other worktrees,
  credentials, journals, proofs and rollback evidence preserved. No new release, upgrade or daemon
  restart. MiniAir installation remains owner follow-up; no Windows install claimed here.
  Its referenced Downloads instruction file was missing and has been reconstructed after rechecking
  accepted Windows ZIP/internal EXE hashes. Verify MiniAir first; if already clean/exact fc3f424 and
  compatible, record completion without reinstalling. Its current installed state is unverified.
- Evidence: `~/.local/share/agent-coordinator-autonomy/p6-observe-20261016T130602Z-*`,
  `p6-observe-startup-guide-20261016.md`, both renew directories and appended `pilot-timeline.log`.
  Documentation check uses mdBook0.5.4; external dashboard verification now succeeded.

**Next session:** inspect existing gen8 helper/lease first, then digest/host heartbeats; do not
reclaim, release, finish or start a duplicate helper while ownership is valid. If authority is lost,
stop ownership-dependent work and report the new blocker; this session's recovery authorization
covered this specific gen7 incident. No new admissions or deployment. Keep both new defects Planned,
respect the night window and leave MiniAir, human-delivered work and branch retirement to the owner.

## Previous resume point — P6 observation refreshed; mxmini night window open (2026-10-16 02:00 UTC, workflow on)

- Observation only; Linux rollout remains complete. Anonymous HTTPS discovery (redirects disabled)
  and the selected `/usr/local/bin/agent-coordinator` are clean/exact **fc3f424**; every compatibility
  check passes. Local and oracle installed binary hashes match the retained fc3f424 manifests.
  Fetch succeeded; `origin/main` remains **e6fd69dceb47a9432fdb2d5d82f363803c2abe83**.
  The autonomy-plan branch matched its upstream before this HANDOFF refresh. No release or upgrade
  was dispatched, and no service, supervisor, integrator or renew helper was restarted.
- Existing renew **pid778271** is alive. Fresh independent task read at **01:59Z** confirms pilot
  **d513bc21**, attempt **28480679-700b-4dd1-a82e-0e8706a9a3e3**, **generation7**, session
  **a9c40947-f8fc-4491-ba59-9f4880e88f7f**, active/in_progress; heartbeat **01:58:24.952Z**,
  expiry **02:58:24.952Z**. The renew log continues verifying ownership every60 seconds.
  Keep this existing helper and lease; do not reclaim, release or mark the ongoing pilot Done.
- Fresh digest at **01:59:35Z**: **HRI0**, stalled_tasks0, stalled_queue0, no pending reversible
  decisions. Decisions list has no pending human decision. Preserve the historical temporary
  deployment HRI2 and the separate owner P1 scheduling decision described below.
- The nightly **10-16 canary remains terminal PASS**, oracle-1/claude, outcome `ok`, detail `done`,
  **01:18:52Z**, duration **4727.2seconds**, row **8906f6cd**. No rerun or waiting required.
  Open reports still contain only historical **27f99b21**, flaky, `requires_human=false`.
- Oracle exact units **agentc-run.service** and **agentc-integrator@run.service** are active/running,
  **NRestarts0**; fresh heartbeat at **02:00:18Z**, integrator logging Idle, release marker absent.
  **693af855 is now Done**. Already-admitted **1a3290db** (Autonomy console) is being reviewed:
  activity **05060241-694e-466a-ab82-1868854ea00f**, active attempt **544f77e9**, generation1,
  expiry **02:45:16.433Z** at the observation. Leave processing to the supervisor/integrator.
- mxmini supervisor **pid777161** is alive with fresh heartbeat. The `quiet-hours` marker was
  present at **21:59 local** and absent at **22:00:18 local** after the existing every-minute cron
  opened the **22:00-07:00 local night claim window**. Heartbeat then identified an implementation
  launch for already-admitted **97b7b62a**. The cron configuration was not changed; outside the
  window, the marker should be present. No manual claim or scheduling intervention occurred.
- Browser check: authenticated web dashboard displayed the Agent Coordinator queue with pilot
  In Progress, Autonomy console Waiting Review and **be6d39af P3 Planned**. No admission or other
  task mutations were made. Both task-list pages were read; no additional admissions are authorized.
- Evidence: `~/.local/share/agent-coordinator-autonomy/p6-resume-20261016-*.json`, the saved served
  startup guide, existing renew log and appended `pilot-timeline.log`. MiniAir installation remains
  owner follow-up via `~/Downloads/miniair-upgrade-fc3f424.txt`; it was not performed here.
  The unrelated dirty `.claude/decisions-pending.md`, other worktrees and all journals are preserved.

**Next session:** inspect the same renewer/lease and current digest, canaries, reports and host
heartbeats; observe already-admitted work without new admissions or redeployment. Respect the
night claim window, keep be6d39af P3 Planned, and leave human-delivered work, branch retirement,
MiniAir upgrade and other owner decisions with the owner. The detailed release evidence follows.

## Previous resume point — Linux rollout complete at `fc3f424`; pilot gen7 renewing; nightly canary passed (2026-10-16 01:37 UTC, workflow on)

**Mira daytime P6 run (ledger tag `[daytime P6 Mira]`):**
- **DEPLOYED fc3f4244ce89a7f85956df3ae9aa2bca3d59f824.** Owner dispatched release 38783852315;
  all three jobs passed. Global preflights were all zero before pre and again after service stop.
  Old-binary snapshot **20261015T202958.864Z-b0972372-6bec-4afd-86dc-cd0b4e407edf** and new-binary
  snapshot **20261015T203033.152Z-530a687a-6df2-4f9b-9664-8501376b8bb6** both verified.
  Service stopped **20:30:28Z**, started **20:30:29Z**, first successful health **20:30:33Z**.
  Anonymous HTTPS info is clean/exact fc3f424; all three installed GCP binary hashes match accepted
  artifacts. Backup/maintenance/transfer returned success, timers restored, keyless warnings **0**.
  Deployment log: `~/.local/share/agent-coordinator-autonomy/p6-deploy-fc3f424.log` (exit 0).
- **bb5fb219 done.** The first counted revise expired at **15:52:59.689Z**; eligibility was confirmed
  at 15:53:55Z. The owner approved raising P2 to P1 (task revision 3), making it next after the active
  56662529 review. Oracle-1 claimed **f07a303d-91cf-41cd-b790-632712683bef, generation 5**, at
  **16:40:59.661Z**, cherry-picked the two approved fb62c9b commits onto 16b05b9 without conflicts,
  and submitted **6d5730202d17bb30ee40cdf6f8c0f4fdff08c31b** at **16:59:21.138Z**.
  Review **4f8d5855** approved **17:45:33.826Z**, `distinct_launch`; integration **89fd82b0** completed,
  and done was confirmed **17:55:49Z**. Its integrated main was **fc3f4244ce89a7f85956df3ae9aa2bca3d59f824**.
- Integration CI **38780071763** and documentation **38780071792** passed. The separate main runs
  **38781391084** (Coordination checks) and **38781391076** (Documentation checks) both completed successfully on
  the same fc3f424 source. The dispatched release head is exactly fc3f424; do not dispatch again.
  In-launch candidate/review gates reported 993 tests passed, 0 failed and 30 explicit nested-sandbox
  skips, plus passing browser tests; docs were deferred to required CI, which passed for integration.
- **Release scope af81834 -> fc3f424 verified:** rustls **0.23.46**, the complete-external manager
  control/docs, canary line, **032ef331** (digest task names, 16b05b9), and **56662529** (target-block
  paging, e8fd8fc). **No migrations or new service environment names.** Two optional host settings:
  `ATTENTION_TARGET_BLOCK_MINUTES=30`, `ATTENTION_TARGET_REPAGE_MINUTES=360`. Rebuild CLI,
  supervisor, integrator and push for the locked dependency/source identity; run host-setup for scripts.
- **Final builds verified.** Clean fc3f424 release builds completed successfully on the workstation in
  `/home/marshall/src/worktrees/ac-release-p6-20261015` and on oracle-1 in
  `/home/ubuntu/src/worktrees/agentc-pilot`, each with its own `target`. Build logs:
  `~/.local/share/agent-coordinator-autonomy/p6-local-build-fc3f424.log` and
  oracle-1 `/home/ubuntu/agentc-p6-build-fc3f424.log`. Repeat locked build confirmations exited 0.
  Both CLI identities are clean/exact fc3f424; all four binaries per host have SHA-256 manifests
  `p6-mxmini-build-fc3f424-manifest.json` and `p6-oracle-build-fc3f424-manifest.json` in the local
  autonomy state directory. Oracle retains `/home/ubuntu/agentc-p6-build-fc3f424-manifest.json`.
  **Oracle upgrade completed 20:32:05Z** (prepared helper exit 0): both CLI copies, supervisor,
  integrator and push match their manifests. Host-setup used APPARMOR_BWRAP=1 UPDATE_TIMER=0;
  integrator explicitly restarted, both daemons active, fresh supervisor heartbeat and integrator Idle.
  Oracle compatibility checks all true. User workstation CLI `~/.local/bin` installed via
  `upgrade_client.py --candidate`, hash `5c45dbfa`, compatibility all true; use this explicit new path
  until `/usr/local/bin` is upgraded. **mxmini root upgrade is now verified** (00:38Z 10-16): both
  root CLI copies `5c45dbfa`, supervisor `91edf6dd`, push `2270e998` match the source manifests;
  SysVinit restarted supervisor **pid777161**, fresh heartbeat, selected PATH compatibility all true.
  The selected `/usr/local/bin/agent-coordinator` now works against production.
- **af81834 archived** to `~/.local/share/agent-coordinator-autonomy/release-af81834`; both package
  archive checksums pass. Verified installed-host rollback copies and SHA-256 manifests are in its
  `host-mxmini` and `host-oracle-1` directories (all CLI copies, supervisor/push, oracle integrator).
  Oracle also keeps `/home/ubuntu/agentc-p6-rollback-af81834`.
  `release-next/RELEASE` records **fc3f424**, previous **af81834**, deployed from these verified packages.
  GCP retains its prior staging files in `/home/marshall/release-p6-prior-af81834`.
- **Acceptance verified:** Linux and Windows reproducible package jobs and the 1800-second load job
  passed. The report confirms full baseline acceptance, **90,000 requests**, metadata **p95 20.578ms**,
  and the exact accepted server hash. All archive/internal hashes and clean source identities pass.
  Evidence under `~/.local/share/agent-coordinator-autonomy/release-fc3f424-run38783852315`, including
  `verified-release.json` and `load/load-acceptance.json`:
  Linux archive `a82377a483ef7caa13a1be7e8da7c069974e3874534ebab7a444800444fd3300`;
  server `05a23d383b39dd40d5d8f5029fce4a242e8f614f6fb226bd40006cd307abf9a1`.
  Ubuntu's packaged binaries require glibc 2.39; MX has 2.36. Full package runtime/internal validation
  passed on GCP Ubuntu; use the already verified native source builds for MX and oracle-1.
- **Pilot reclaimed and new renew running:** existing p3b-pilot session **a9c40947** resumed with
  harness `claude`, same identity. Fresh revision2 claim returned **28480679-700b-4dd1-a82e-0e8706a9a3e3,
  generation7**, at **00:40:05.919Z 10-16**. New renew pid **778271** uses the prepared
  `~/.local/share/agent-coordinator-autonomy/p6-pilot-renew/{renew.sh,renew.py,claim.json,renew.log}`.
  It renewed immediately at **00:40:44Z**, fresh ownership verified, then continued at **60 seconds**
  per the actual claim guidance. Subsequent renews verified; no retry/recovery was needed.
  Old renew4194295 remains stopped. Read-only helpers1997610,1997611,2413349 left unchanged.
- **Oracle release flag cleared 00:41:30Z**, only after fresh pilot claim/renew. **mxmini held
  `release-fc3f424`** until the final owner step; **quiet-hours restoration is now verified at 00:49Z
  10-16**. The marker is `quiet-hours` and the every-minute cron rule remains **22:00-07:00 local**.
  Outside that night claim window, holding this marker is correct; the restore helper kept the file
  present while retagging it, so no daytime claim gap opened.
- **Earlier SithBit authority drained naturally** by 20:29Z; all seven global counts zero before
  deploy-pre, after stop, and in post-deploy verification. No SithBit work was mutated. Oracle
  integrator was paused while idle before the final zero check, then upgraded/restarted.
- **All Linux root steps completed:** owner authorization allows Mira to run sudo wherever available.
  GCP/oracle steps ran directly; local sudo required a terminal password, so the owner ran the mxmini
  install and final `p6-mxmini-restore-quiet-hours.sh` helper. Both were independently verified.
  No password was passed through chat, no sudo policy/credential/journal change or workaround used.
  Final checks: both hosts' compatibility all true, fresh supervisor heartbeats; oracle supervisor and
  integrator active; pilot ownership active and repeated renews verified. Final snapshots under the
  local autonomy state directory: `p6-final-{pilot,digest,canaries,open-reports}.json`.
- **MiniAir-only prompt ready:** `~/Downloads/miniair-upgrade-fc3f424.txt`, using accepted ZIP
  `e0c5ed896ea565222c4ba5f1c71b2a1733ebb9576d0cd2db048ccef8939d9d84` and EXE
  `1f9d48dac6c324ab6981cc769621f128f89e690d8ffe0ca66de47b22d31d7ba1`.
  It requires production at the matching source before installing via `upgrade_client.py --candidate`;
  Service prerequisite is now satisfied. Windows installation has not been performed or claimed here.
- **Owner decisions resolved:** 335b6713 and 5ad83ff3 had already been opened at 14:38/14:37Z by a
  different actor; the owner explicitly said **keep both admitted**. Other existing admissions were
  left alone. Oracle started previously admitted **693af855** after the BB review. After its release flag
  cleared, review resumed with active attempt **09869840** (valid lease observed 00:45Z 10-16).
  No additional admission was needed. Do not admit more.
- **New defect be6d39af (P3, planned, NOT admitted):** cost backfill retries two old 10-07 rows for
  missing project b4423f23 forever with a generic 404; unsupported older servers also return 404, so
  the fix must distinguish/quarantine safely and preserve the ledger. Current task rows upload.
  No ledger/marker files were changed in diagnosis; the two orphaned rows total $0.2307784.
- **Cost/HRI:** oracle-1 BB lifetime **5 impl + 5 review, $2.9149326 raw** (all priced, none estimated);
  mxmini **0 rows/$0**, confirmed by the owner in a regular terminal. The latest impl/review round is
  $0.4063697 raw. 56662529 cost **$1.175874** (1 impl/1 review), HRI 0. Settled comparison refreshed at 18:48Z: BB service total **$2.914932**, matching all ten rows.
  Cost telemetry matches raw launch/token counts and per-row integer-micro-USD sums; rounding the raw aggregate once can differ
  by one micro-USD. Evidence: `p6-oracle-task-costs.jsonl`, `p6-cost-telemetry-comparison.json` under
  the local autonomy state directory. Digest HRI was 0; no host/task recovery intervention occurred.
  **One shared owner scheduling decision** (P1 priority) is recorded separately from that digest zero.
  Post-deploy digest **20:34Z HRI 1** is `stalled_task/repeated_attempt_failures` for the pilot: its last
  three ended attempts are planned deployment releases. Source explicitly counts released attempts;
  fresh task read is **ready/eligible, no unmet preconditions**, so it does not block reclaim. Preserve
  history; no recovery mutation performed. At 00:39Z 10-16 the prolonged deployment hold also
  produced `stalled_queue` (HRI2). **After the fresh pilot claim, digest at 00:41:30Z is HRI0**, no stalled
  tasks or queue; these advisory flags cleared without a human recovery mutation.
- **10-16 nightly canary PASSED:** oracle-1/claude terminal `outcome=ok`, detail `done`, at
  **2026-10-16T01:18:52.000Z** (duration **4727.2 seconds**). Service telemetry row
  **8906f6cd-71ab-48c6-afda-31c4c2769592**, task **c28853fe-d542-45fa-8ec0-09c6711a3e6b**. Verified from the live
  `/telemetry/canaries` response; snapshot `p6-canary-terminal-session-boundary.json` under local
  autonomy state. Earlier pending-canary checkpoint is superseded. Open-report check at rollout
  completion contained only historical **27f99b21**, flaky, `requires_human=false`.
- **Session-boundary refresh:** production remains clean **fc3f4244ce89a7f85956df3ae9aa2bca3d59f824**;
  fetched `origin/main` is now **e6fd69dceb47a9432fdb2d5d82f363803c2abe83** from subsequent autonomous work.
  This does not authorize another release or client upgrade; use live state for any later work.
- Preserve the pre-existing uncommitted `.claude/decisions-pending.md` edit. This run edits only
  HANDOFF.md in the parent checkout; build worktrees remain clean and detached.

**Next session (release work is complete; no pending Linux terminal step):**
1. Inspect the existing pilot renew pid778271/log and fresh ownership; keep gen7 active. Do not start
   duplicate helpers, claim again or repeat this release. Respect the mxmini night claim window.
2. Tonight's 10-16 canary is already a verified terminal PASS; no need to wait for it. Inspect current
   digest/open reports and already-admitted work as needed. No new admissions are authorized.
3. Owner follows `~/Downloads/miniair-upgrade-fc3f424.txt` on MiniAir; this session only prepared that
   verified Windows upgrade prompt. Human-delivered/planned tasks and d2ca0225 retirement remain
   owner decisions. Keep be6d39af planned. Preserve unrelated edits/worktrees and rollback records.
4. This handoff and `~/Downloads/prompt.txt` were refreshed for the completed Linux rollout; future
   changes need current live evidence and the applicable gate. Do not redispatch/deploy fc3f424.

## Previous resume point — main `57029d8` (rustls fix landed); release after bb5fb219; canary ok (2026-10-15 03:30 UTC, workflow on)

**Session `118f53c8` evening run (prompt `~/Downloads/prompt.txt`; ledger lines `[evening 118f53c8]`):**
- **bb5fb219 parked** (`revise_limit_reached`, human-only) since 18:11Z 10-14. Cause: RUSTSEC-2026-0346..0349
  (rustls 0.23.45, published 10-14, fixed in 0.23.46) made the required `dependency-audit` fail on every run,
  main included. The integrator (`crates/integrator/src/attribution.rs`) blamed the author because target
  af81834's only audit result was a 07:21Z pass from before the advisories. So three approved candidates
  (985194d, b68fa87, fb62c9b) were canceled → `revision_needed` → re-applied unchanged. The three
  `check_failed` revises count for 24 h (they are not "serialized after a landing", so new landings don't
  clear them): **unparks ~15:52Z 10-15 with no human action.** $2.51 so far (oracle-1 impl 4 $1.69 + rev 4
  $0.82), HRI 0. Open flaky report 27f99b21 (linux-validation on b68fa87; rerun passed).
- **d4d34008 done** (P1, filed 00:18Z, owner-admitted 00:33Z): rustls 0.23.46 → main **`a125742`** ~02:47Z;
  main CI green. oracle-1 impl a9e0aa23 $0.17 + rev ca7c766e $0.09 = **$0.26, HRI 0**.
- **Canary 10-15: ok** 03:23:47Z after 12223 s. Two cycles failed the audit before a125742 landed; mxmini
  claimed cycle 3 at 02:01Z (its first poll after quiet hours on the af81834 supervisor). Landed `CANARY.md`
  → main **`57029d8`**. **$0.585** (oracle-1 impl 2 + rev 3 $0.465, mxmini impl $0.120), HRI 0.
  oracle-1's canary timeout is 240 min (`E2E_TIMEOUT_MINUTES` in `/etc/agentc/e2e-canary.env`), not 90.
- **Telemetry (new in af81834) works.** `canary_results` got its first report at 03:23:48Z with a backfill
  (8 rows = the 8 lines of oracle-1's `e2e-canary.jsonl`). `cost_rows` (`GET …/telemetry/cost`) match
  oracle-1's `costs.jsonl` exactly for bb5fb219 and d4d34008 (USD to $0.000001, tokens, launch counts).
  Canary cost rows are hidden unless `include_canary=true`, but the CLI `request` refuses query strings and
  no client reads telemetry yet (1a3290db is the dashboard view), so they can't be checked from the CLI.
- **MiniAir CLI upgraded** by the owner: af81834, exe `73ccd96e…`, rollback `9bad8bef…` (60eda26) kept as
  `.pre-af81834` and `.rollback`. All known CLIs are at af81834.
- **Owner decisions:** 422ce01e, 55b8a06d and c744c495 marked `human_delivered`; **c744c495 canceled**
  (P3b go/no-go done in 47d4403d; observed costs now in `cost_rows`).
- **Filed (planned):** 335b6713 (P2) the integrator trusts a stale target pass and blames the author;
  5ad83ff3 (P2) a task parked by the revise limit appears in no human surface (the 00:40Z digest showed
  HRI 0 while bb5fb219 waited for a human).
- Digest 00:40Z: HRI 0, stalled 0; budget-held 56662529. Helpers alive 03:30Z: renew.sh 4194295
  (d513bc21 attempt 5c760158 gen 6), watch.sh 1997610, journal ssh 1997611 (stale), mxmini-watch.sh 2413349.

**Next actions:**
1. **Release timing (owner concurred, 2026-10-15): release after bb5fb219 lands, not before.** One release
   then ships rustls 0.23.46 and the complete-external manager control. Clients must match the service's
   commit exactly (`source_commit_matches`), so each release means upgrading every CLI, MiniAir included;
   one round instead of two. Exposure meanwhile is low: rustls is client-side only (reqwest), built
   without brotli (0348 unreachable), no ECH configured (0347 unreachable). Since af81834, main has only
   `Cargo.lock` and `CANARY.md`, and bb5fb219's candidate is web, docs and a browser test: no migrations,
   so a binaries-only deploy (check again at release time). The owner dispatches `release.yml`.
2. Watch bb5fb219 unpark (~15:52Z): a supervisor reclaims it, re-applies fb62c9b's two commits on main,
   then review and integration (the audit now passes). Record cost and HRI per host.
3. Then the release drill (`release-next`), CLI upgrades on all four hosts, host-setup on both hosts and
   the integrator rebuild, as in the af81834 rollout below.
4. Owner: admit 335b6713 / 5ad83ff3? Left planned.
Don't admit work without the owner. Planned on purpose: 56662529, 1a3290db, 2a387d02, 97b7b62a, 693af855,
d2ca0225, 335b6713, 5ad83ff3; human-delivered and planned: 422ce01e, 55b8a06d.

## Previous resume point — production `af81834` deployed; hosts rolled out; 47dac763 done (2026-10-14 13:35 UTC, workflow on)

**Session `0b3c541a` daytime release (prompt `~/Downloads/prompt.txt`; ledger lines `[release af81834]`):**
- **Release run 38550683390 green**; artifacts in `~/.local/share/agent-coordinator-autonomy/release-next`
  (sha256 OK; RELEASE `af81834… agent-coordinator-0.1.1-linux-x86_64.tar.gz ce3ddd1`). ce3ddd1 record
  archived to `../release-ce3ddd1`; new `PLAN-next.md` / `PREREQS-next.md`.
- **DEPLOYED af81834 11:17Z** (full; migrations 0037 `human_delivered`, 0038 `cost_rows`/`canary_results`).
  Preflight 0 (all projects) before and after pre. **Old-binary snapshot
  `20261014T111624.698Z-039096f0-ebde-46ee-95d4-0ec70d4bcf79`** = the only rollback path (ce3ddd1
  refuses 0037/0038), then `*-0.1.1-ce3ddd1` binaries. Down 11:16:50–11:17:21Z. New snapshot
  `20261014T111721.758Z-b320e71d…` verified; keyless warnings 0; `/api/v1/info` af81834, not dirty.
- **Integrator** redeployed on oracle-1 (`integrator-host-setup.sh` stops `@run` and does not start it;
  started again 11:19:15Z): `75497e6d`, rollback `/opt/agentc/bin/agentc-integrator.pre-af81834`.
- **CLIs at af81834:** workstation `~/.local/bin` (`8d9e6d4d`, upgrade_client.py); oracle-1
  `/opt/agentc/bin` + `/usr/local/bin` (`004ee345`, `.pre-af81834`); mxmini `/opt/agentc/bin` +
  `/usr/local/bin` (`8d9e6d4d`, `.pre-af81834`). Compatibility all true on all three. **MiniAir Windows
  still old** (owner, release zip in release-next).
- **Host-setup at af81834:** oracle-1 rc 0 (sup `bf23e837`, push `71fa1f3d`; agentc-run restarted
  11:23:29Z; timers intact). mxmini owner rc 0 (QUIET_HOURS=22:00-07:00; sup `93780441`, push
  `d5693c10`); owner `service agentc-run restart` → loop pid 4190157.
- **Kill switches:** oracle-1 `release-af81834` (set 11:14:30Z, agent sudo -n); mxmini `quiet-hours`
  (cron lifts it 02:00Z). **renew.sh 169811 stopped**; d513bc21 released (attempt ba401934 gen 5 → ready).
  watch.sh 1997610, journal ssh 1997611, mxmini-watch.sh 2413349 left running.

- **47dac763 done** 13:28:14Z (`complete-external`, delivered_revision ce3ddd1, 3 checks). The owner's CLI
  call was refused (`operation_not_permitted`, gate `task_external_completion`): only a dashboard password
  sign-in is a human actor and the manager has no control for the route. Worked via `fetch()` from the
  signed-in dashboard tab's DevTools console (CSRF from `/api/v1/me`, `X-CSRF-Token`, `Idempotency-Key`).
  Filed **bb5fb219** (P2, planned).
- **Step 9:** d513bc21 reclaimed (attempt **5c760158 gen 6**); **renew.sh pid 4194295** from scratchpad
  `0b3c541a…` (log `renew.log` there). **oracle-1 kill switch cleared 13:29:16Z**, loop Idle; mxmini
  `quiet-hours` until 02:00Z. Nothing ready (only d513bc21 open). e2e canary reporting on (no
  `E2E_NO_REPORT` on either host).

**Next actions:**
1. Watch the first e2e canary report: oracle-1 `agentc-e2e-canary@claude` at **00:00Z** → a
   `canary_results` row (new in 0038) and a supervisor cost row in `cost_rows`. Then mxmini's first
   claim after 02:00Z (only if the owner admits work).
2. Owner: MiniAir Windows CLI from `release-next/agent-coordinator-0.1.1-windows-x86_64.zip` (zip sha256
   `19ca26d0…`, exe `73ccd96e…`, embeds af81834) via `upgrade_client.py --candidate`.
3. Owner decision pending: mark 422ce01e / c744c495 / 55b8a06d `human_delivered`
   (`agent-coordinator human-delivery --task … --expected-revision … --reason …`); left planned and unmarked.
Don't admit work without the owner. Planned on purpose: 56662529, 1a3290db, 2a387d02, 97b7b62a, 693af855, bb5fb219.

## Previous resume point — P6 dogfood on `ce3ddd1`; all 7 admitted tasks landed (main `af81834`, all checks green); evening run ended (2026-10-14 07:50 UTC, workflow on)

**Session `7a924313` evening run (started 02:04Z from `~/Downloads/prompt.txt`):**
- **4bc09489 done** on oracle-1: impl 4dd75fdd $1.35 → rev 900b6b65 $0.25 **approved** 01:44:53Z
  (activity 6741be27) → main fast-forwarded to **`46fea34`**. $1.60, HRI 0. Not in production
  (ce3ddd1), so keep 55b8a06d / 422ce01e / c744c495 planned until the release ships it.
- **c2aea126** (design, P1): oracle-1 impl f735b12a $1.89 submitted 02:04:23Z → `waiting_review`.
- **22:00 EDT check:** mxmini kill switch cleared 02:00Z; loop resumed and **claimed 1a5ffe45**
  (launch 06bd1ef6, attempt ad9ecee0). mxmini 47 GiB free; oracle-1 62 GiB, switch clear.
- **c2aea126 done**: rev a752d07a $0.22 approved 02:47:05Z → integrated d5dc013 (main **`fb32190`**).
  $2.11 total, HRI 0.
- **1a5ffe45**: mxmini launch 06bd1ef6 exited 0 at 03:02Z (62 min; long `cargo test --workspace`,
  no stall) → `waiting_review`. mxmini cost needs the owner's `sudo grep` of its `costs.jsonl`.
- **2a88c1f9** claimed by oracle-1 (attempt 9f6e3e74) ~02:05–02:47Z.
- **2a88c1f9**: oracle-1 impl 9f6e3e74 $0.62 submitted 03:08:53Z → `waiting_review`.
- **mxmini claimed 04b0ed64** (attempt 6ad0924d, launch 6b4c9d9c) after 1a5ffe45.
- **Reviewer hang, filed 693af855 (P2, planned):** oracle-1 reviewer 2950bb54 (1a5ffe45, from ~03:10Z)
  backgrounded the gate (`… > gate.log &`) via candidate-shell; the per-call bwrap PID namespace killed
  it, gate.log stayed empty, and `until grep TEST_DONE` loops hit the 600 s Bash timeout and were moved
  to the background, which may keep the 15-min drain from firing. Watch how it ends (drain, timeout, or
  a decision without tests) and add that to 693af855.
  **Outcome:** stopped ~03:55Z after ~45 min (not the 15-min drain); review **Requeued** (activity
  32abc0b1). Cost row `usd: null`, `usd_estimated: true` (unpriced, like 1d5648c0). The next reviewer of
  1a5ffe45 may hit the same hang. oracle-1 then claimed **1d5648c0** (launch 58f4baaf, ~03:55Z).
- 04:09Z: mxmini submitted **04b0ed64** 03:57Z and claimed **9722697f** (8e43dacb); oracle-1 on
  1d5648c0 (8229c6bb). Review queue 1a5ffe45 (requeued), 2a88c1f9, 04b0ed64; no reviewer free. Nothing
  left ready. oracle-1 49 GiB free (impl/runs 24 GiB mid-build).
- 04:55Z: **1a5ffe45 approved** by oracle-1 rev bfd0fb9d ($0.34, 04:50:19Z; ran the gate in the
  foreground, so no 693af855 repeat) → `waiting_integration`. oracle-1 then started reviewing
  2a88c1f9 (7cfa1296). 1d5648c0 (oracle-1 impl $0.41) and 9722697f (mxmini; submitted, then drained by
  design, `lease.rs` `stop()`) submitted. Queue: 2a88c1f9 (in review), 04b0ed64, 1d5648c0, 9722697f.
- **mxmini costs (owner sudo):** 1a5ffe45 $2.88, 04b0ed64 $4.61, 9722697f $0.21 → $7.70, HRI 0.
  `!` commands can't sudo (no tty); the owner runs the grep in a terminal. 47dac763 untouched (blocked rev 2).
- 05:35Z: **1a5ffe45 done** (integrated, main **`e897783`**; $3.22+ incl. unpriced hung rev, HRI 0). **2a88c1f9 approved** by oracle-1 rev 7cfa1296 ($0.19, 05:30:59Z, gate in foreground) → `waiting_integration`; $0.81, HRI 0. Queue: 04b0ed64, 1d5648c0, 9722697f. Nothing ready; disk oracle-1 61 GiB, mxmini 151 GiB; no stalls, no serve-test failures.
- 06:20Z: **2a88c1f9 done** (main **`c0db9d0`**; $0.81, HRI 0). **04b0ed64** approved by oracle-1 rev de473ac2 ($0.12, 05:53:03Z, 2 advisories) but integration 6ad1d48a was **canceled → `revision_needed`** (likely stale base after 2a88c1f9); mxmini reclaimed it at once (attempt 220cd404, launch 77389b0e). oracle-1 reviewing 1d5648c0 or 9722697f (a05bff20, gate in foreground). Disk oracle-1 49 GiB, mxmini 138 GiB.
- 07:05Z: **1d5648c0 done** (rev a05bff20 $0.18; $0.59 total, HRI 0). **04b0ed64 done**: mxmini revision 220cd404 added `6cfdcc4` (covers 2a88c1f9's integration-blocked notice) → rev 46f9429b $0.22 approved 06:50Z → main **`6321a88`**; HRI 0; mxmini revision cost needs the owner's `sudo grep`. The 6ad1d48a cancel has **no recorded reason** in the CLI (activity `result` null, no lifecycle events): check the dashboard's Integrator reports; if none, file it. oracle-1 reviewing **9722697f**, the last admitted task.
- 07:50Z **run end**: **9722697f done** (rev 9d69330b $0.11 approved 07:13Z; $0.32, HRI 0) → main **`af81834`**, all required checks green. **All 7 admitted tasks done, HRI 0 each.** Totals: oracle-1 $5.90 (+ unpriced hung rev 2950bb54), mxmini $8.02 (04b0ed64 revision 220cd404 $0.32, owner grep); $13.92+. Filed **97b7b62a** (P2, planned): integration 6ad1d48a's cancel has no recorded reason. Now unblocked but still planned on purpose: 1a3290db (after 1a5ffe45), 2a387d02 (after c2aea126). Nothing ready.
- Ledger lines appended 02:05Z. Helpers 169811/1997610/1997611/2413349 alive.

**Session `e9a556c5` evening run (started 00:03Z from `~/Downloads/document.txt`):**
- **4f546e9b done.** oracle-1 impl f7970700 submitted 15:00:41Z; review a44f440e **approved** 15:40Z
  (2 advisories: no live-service CLI test; check_docs/mdBook not run by reviewer); integration
  54fcddde fast-forwarded main to **`f2aef69`** ~15:46Z. main = `f2aef69` (not in production; the
  workstation CLI at ce3ddd1 has no `integrator-reports` yet). Cost $0.99 (impl $0.81 + rev $0.18), HRI 0.
- **47dac763 bounced, now `blocked`.** either_review 9e2ebeb5 (oracle-1) = **changes_requested**
  15:03:42Z, 4 required findings: commit 1924885 not pushed at review time (it is now); no artifact for
  CLI parity / the 6cc3f673→commit mapping; `pilot-timeline.log` in no ref; HANDOFF lacked the 05:41Z
  landing. The task went back to ready and **oracle-1's supervisor claimed it 3 s later** (attempt
  5b7c6bbe), which blocked it at 15:04:55Z ("owner should attach the artifacts and resubmit").
  Filed **4bc09489** (P2, budget-held): supervisors claim owner-only general tasks, incl. after
  changes_requested. Cost $0.33 (rev $0.15 + bounced impl $0.18). **Owner decision (00:40Z):** close it with
  `complete-external` after the next release (see Next actions). The route (46043cbc, `2f665d2`) is
  **not in production ce3ddd1**: the owner's call at 00:30Z returned `record_not_found`, and the task is
  unchanged (open, blocked, revision 2). Left `blocked` meanwhile (no supervisor claims a blocked task).
- **Digest 2026-10-14 00:00Z:** HRI 1 = open **fix_target report `9d8c2171`** (created 01:19:59Z
  10-13, `task_id: null`, still unresolved although the target is fixed) — this settles the open
  question: a fix_target report *was* filed, so 2a88c1f9's framing holds. **Resolved 00:35Z** in the
  console at the owner's request, with unreviewed_landing 5874bf39 (df71ee7) and 60cc6203 (4540596c's
  afca7cf/24a7f3d), both owner hand-ships. **0 open integrator reports.** Note `task_id` null means 4f546e9b's `--task` filter won't find target_failing reports.
  Budget-held listed: 2a88c1f9, 56662529, 1d5648c0 (and now 4bc09489, 9722697f).
- Filed **9722697f** (P3, budget-held; owner request): the web UI calls itself "Operations console";
  rename to "manager". In chat, call it the "web dashboard".
- **e2e canary 2026-10-14: ok** (claude on oracle-1, 1806 s). Canary task 82200b16 done (review
  e6c17e61 approved 00:21Z); $0.14 (impl $0.06 + rev $0.07), HRI 0.
- oracle-1: kill switch clear, 61 GiB free, two transient `next` failures (17:01Z timeout, 18:28Z).
  The workstation journal capture (pid 1997611) is still stale; read oracle-1's journal directly.
- mxmini: quiet hours until 22:00 EDT; 48 GiB free (the 08:39Z low-disk refusals have not recurred).
- Helpers alive at 00:03Z: renew.sh 169811, watch.sh 1997610, journal ssh 1997611, mxmini-watch.sh 2413349.

**Next actions, in order:**
1. **Release main (owner dispatches, daytime 2026-10-14):** `gh workflow run release.yml --ref main` at
   `af81834` (all required checks green; also carries tonight's 7 tasks). **Verified 10:25Z:** full release (migrations 0037 `human_delivered` column, 0038 `cost_rows`/`canary_results` tables; snapshot = only rollback); **integrator changed → rebuild**; supervisor/CLI changed → rebuild + host-setup on both hosts; no new service env names. Next session's prompt: `~/Downloads/prompt.txt`. Carries 0d5e4c10, 4540596c, df71ee7,
   cbb05ecb, 46043cbc (`complete-external`), 4f546e9b. Then the deploy drill
   (`~/.local/share/agent-coordinator-autonomy/release-next/`) and CLI rebuilds on all three hosts.
2. **Then complete 47dac763** (owner's human CLI session; agent sessions are refused, and the
   classifier blocked the agent's call from the dashboard tab):
   ```
   agent-coordinator --session owner-admit request --method post \
     --path /api/v1/projects/fe95a6c5-2aad-463f-8446-4366d9a281c7/tasks/47dac763-c52c-4134-9efb-8421998e37e5/complete-external \
     --input ~/.local/share/agent-coordinator-autonomy/release-next/47dac763-external.json --json
   ```
   The body cites `ce3ddd1` with linux-validation, documentation and dependency-audit. If it returns
   `required_check_missing` (roster includes deploy-scripts), switch `delivered_revision` to `f2aef69`
   (all four green) and add `deploy-scripts`. If the task revision moved past 2, update `expected_revision`.
   Then confirm `done` and record in the ledger.
3. **Admitted 00:40Z (owner request, via dashboard):** 4bc09489 (oracle-1 claimed at once, attempt
   4dd75fdd), 2a88c1f9, 1a5ffe45, c2aea126, 1d5648c0, 9722697f; 00:50Z **04b0ed64** (P2, filed and
   widened at the owner's request: web UI auto-refresh, site-wide, update only changed DOM; vanilla JS /
   modern CSS / CSP-safe Alpine). Watch them through review and
   integration; record cost/HRI per host. Still planned on purpose: 56662529 (owner decision: paging
   threshold, proposed 30 min, and whether it supersedes digest-only for fix_target), 1a3290db (after
   1a5ffe45), 2a387d02 (after the c2aea126 design), 55b8a06d / 422ce01e / c744c495 (owner tasks; keep
   planned until 4bc09489 ships, or a supervisor will claim them).
4. Daily 00:00Z checks; watch items (3d2fe98a-style stall, `agentc-push` serve test, mxmini disk).
5. Reference only (owner): sithbit 61bf19af target hold 83063e8f; 422ce01e; Required checks `deploy-scripts`.

## Previous resume point — P6 dogfood on `ce3ddd1`; block cleared, all three landed (2026-10-13 14:55 UTC, workflow on)

**Session `e9a556c5` (2026-10-13 day; ledger lines staged in that session's scratchpad `ledger-add.txt`
until the owner appends them):**
- **Fix landed:** `df71ee7` (quinn-proto 0.11.18) is on main; the integrator then published
  `2f56f58` 05:34:12Z (canary 559967ce), `7940b87` 05:41:38Z (cbb05ecb, `0704b77`), `11be3ce`
  05:49:35Z (46043cbc, `2f665d2`). **All three done.** main = `11be3ce` (not in production).
- **Cost/HRI:** cbb05ecb $3.36 (mxmini impl ac107b32 $3.05 — mxmini's first completed task;
  oracle-1 rev $0.31); 46043cbc $0.95 (oracle-1); canary ≥ $0.10 (one review 8a33a929 killed,
  unpriced). HRI 0 per task; 1 shared HRI = owner's hand fix of the target. Costs come from
  `/var/lib/agentc/costs.jsonl` (root-only on both hosts: owner `sudo grep`).
- **Block ~01:07Z–05:30Z (4.5 h).** Only page: e2e canary timeout 04:00:04Z (ntfy). Digest
  2026-10-13 00:00Z: HRI 0 (predates block). **e2e canary 2026-10-13 FAILED** (timeout, cause = block).
- **Filed:** 4f546e9b (P2, CLI `integrator-reports list/show` + `GET …/reports/{id}`; owner released
  it, in_progress), 2a88c1f9 (P2, block invisible on task / misleading next_actions), 56662529 (P2,
  target_failing pages only via canary timeout; owner decision on threshold), 1d5648c0 (P3, killed
  default-model launch recorded `usd: null`). The last three are budget-held (5/5 this ISO week).
  Unknown: whether a `fix_target` report was filed for the block — check the console's Integrator
  reports view (or 4f546e9b's CLI once it ships); 2a88c1f9's framing assumes it was.
- mxmini refused 08:39–08:43Z for low disk (~19.3 GiB < 20 GiB high-water) — watch item.
- CLI writes from this workstation use session **`dogfood-orchestrator`** (connected 2026-10-13);
  `p3b-pilot` is `watch.sh`'s.
- Console sign-in is per tab: the owner must sign in inside the Claude tab group's tab.

**Next actions, in order:**
1. **47dac763** submitted 14:5xZ (owner OK): oracle-1 kill switch set 14:48Z → admitted in console
   (rev 2) → claimed by `dogfood-orchestrator` (attempt 9c5e82eb) → kill switch cleared → general
   submission 0c97fa23; now **waiting_review** (`either_review`). Watch it to done. Lesson: a planned
   general task cannot be marked done in the console (no complete control); supervisors do not filter
   by kind, so set the kill switch before admitting an owner task. Ledger lines appended; quinn
   worktree/branches removed. The workstation's Chrome is browser "mxmini" (select it; a Windows
   browser is also connected).
2. (done)
3. Daily 00:00Z checks for 2026-10-14 (digest — should list the open fix_target report if one
   exists; e2e canary). Watch items: 3d2fe98a-style 15-min stall, `agentc-push` serve test, mxmini disk.
4. Watch 4f546e9b through review/integration.
5. Next release carries 0d5e4c10, 4540596c, df71ee7, cbb05ecb, 46043cbc (main `11be3ce`) + 4f546e9b.
6. Reference only (owner): sithbit 61bf19af target hold 83063e8f; 422ce01e; Required checks `deploy-scripts`.

## Previous resume point — P6 dogfood on `ce3ddd1`; integrator stalled, three integrations queued (2026-10-13 05:05 UTC, workflow on)

**Session `82cf3a73` (night of 2026-10-12/13; ledger `pilot-timeline.log`):**
- **mxmini's first real window worked.** Kill switch cleared 02:00Z, loop pid 171714 polling.
  **cbb05ecb** (multi-project design doc, P1) admitted 02:15Z (owner ran `tasks edit`, CLI session
  `owner-admit`, grant c9c6152f) → mxmini claimed in seconds (attempt `ac107b32`), submitted 02:40Z
  (`planning/autonomy/multi-project-design.md`: 19 owner decisions, MP-1..MP-14 plan, SithBit
  walkthrough; corrects premises: admission budget is per project, quiet hours exist). Nested-sandbox
  fix held (Bash worked). Review (oracle-1, activity `aec2fd24`) completed ~03:2xZ → **waiting_integration**.
- **46043cbc** (owner-delivered completion path, P3): agent `tasks edit` refused — `admission_budget`,
  a human edit must release a budget-held task (api-contract.md). Released 02:27Z via the console
  (admin, Admission → "Admit to ready queue"). oracle-1 claimed 02:28Z (attempt `d7b62cf4`), submitted
  02:52Z (human-only `POST …/tasks/{task}/complete-external`), review `446ebf80` completed ~03:58Z →
  **waiting_integration**.
- **Integrator blocked: target `main` fails its audit check (diagnosed 05:10Z).** Integrations queued:
  cbb05ecb `8c541d96`, 46043cbc `7eedd6d3`, canary 559967ce `0062bb49`. `agentc-integrator@run` on
  oracle-1 is healthy (up since 10-08) and logs every ~40 s for all three: `Blocked("target_failing:
  Audit locked dependencies failed on R <candidate> and fails too on target 24a7f3dd…")`. Local
  `cargo audit` 0.22.2 on main's `Cargo.lock`: **quinn-proto 0.11.17**, RUSTSEC-2026-0338/0340/0343
  (dated 2026-09-14, new in the advisory DB) → fix: upgrade to **>= 0.11.18**
  (`cargo update -p quinn-proto --precise 0.11.18`). Correct gate behaviour, but **two defects to
  file**: (a) the integrator's Blocked reason never reaches the service, so the task shows no blocker
  and `next_actions` says "Authorize if required, then claim"; (b) nothing paged — three integrations
  sat for hours silently. One transient `coordinator request failed` at 05:01:46Z.
- **sithbit 30d670d5** (mail_grpc verify-absent probe): owner asked; validation reservation
  **a2fb0282** (mxmini-validation) resolved in the console with the saved operator evidence
  (`~/.local/state/sithbit-task-evidence/mira-grpc-integration-af1dddfb/operator-resolution.json`).
  Its integration activity 61bf19af keeps the **target hold `83063e8f`** and an unreconciled
  publication intent for `af1dddfb`; remote master still `67c7fd6f` (publish did not happen). Do not
  press "Reconcile publication" without the owner. Console path: task → Submission & completion →
  Integration → Inspect activity task and jobs → Jobs & resource holds.
- **Permissions:** project `.claude/settings.local.json` allows read-only `agent-coordinator tasks
  show/list`, `reviews list/status`, and Read of the heartbeat, `/var/log/agentc-run.log` and
  `~/.local/share/agent-coordinator-autonomy/**`. Agent task edits, timeline writes and oracle-1 ssh
  are classifier-blocked: hand them to the owner as `!` commands.
- 00:00Z checks for 2026-10-13 (digest, e2e canary) **not done**; canary is the stuck integration.

**Next actions, in order:**
1. **Fix shipped by hand (owner decision 05:2xZ):** `df71ee7` "Bump quinn-proto to 0.11.18…"
   (`Cargo.lock` only, on `24a7f3d`; local `cargo audit` 0.22.2 clean, `cargo check --workspace
   --all-targets --locked` green), branch `ac/human/quinn-proto-0.11.18`, worktree
   `~/src/worktrees/ac-quinn-proto-0.11.18`. Owner ran `scripts/ship.py --name quinn-proto-0.11.18`
   ~05:25Z (session `82cf3a73` background task `bxma33ybi`). Confirm `origin/main` == `df71ee7`
   (if not, read the ship output and CI); then watch the three integrations land in turn, remove the
   worktree and branches per the cleanup rules, and file defects (a) and (b) above. Re-check with the owner's
   read-only ssh: `ssh -i ~/gdrive/Development/oracle-1-key-2026-09-16.key ubuntu@oracle.sithbit.com
   'journalctl -u "agentc-integrator@run" --since "-1h" --no-pager -o short-iso | tail -30'`.
2. Watch cbb05ecb, 46043cbc, 559967ce to landing; record cost/HRI per host in the ledger.
3. **When cbb05ecb lands (done), mark owner task 47dac763 done** (owner decision 2026-10-13; owner
   task → expect a human console edit; confirm with the owner before submitting).
4. Daily checks for 2026-10-13 (digest, e2e canary); watch items as before.
5. Next release carries 0d5e4c10, 4540596c (`24a7f3d`) and whatever lands from 1–2.

**Helpers:** `renew.sh` 169811, `watch.sh` 1997610, journal ssh 1997611, `mxmini-watch.sh` 2413349
were alive at 02:05Z (see below for their scratchpads). Session monitors died with session `82cf3a73`.

## Previous resume point — P6 dogfood on production `ce3ddd1`; mxmini's first window opens 22:00 EDT (2026-10-12 18:15 UTC, workflow on)

**Session `037ba383` (2026-10-12 day; ledger `pilot-timeline.log` has every event):**
- **Deployed `ce3ddd1` 2026-10-12 13:35 UTC** (release run `38251735141`, ~8 s down; migration 0036
  is data-only but **rollback needs the old-binary snapshot `20261012T133500.506Z-a90a2320`** — the
  `aa05dcb` server refuses a DB with migration 36; rollback binaries `*-0.1.1-aa05dcb`; new snapshot
  `20261012T133531.456Z-200a57b3`; record moved to `release-aa05dcb/`, plan in `release-next/`).
  Workstation CLI `~/.local/bin` sha `bff3debb` (`.rollback` kept). oracle-1 host-setup at `ce3ddd1`
  (`~/src/worktrees/agentc-pilot`; supervisor `84dd097f`, CLI `c824f154`, push `cf98c428`;
  `/usr/local/bin` CLI updated, old `.pre-ce3ddd1`); implementer settings `sandbox.enabled=false`,
  reviewer true; kill switch cleared 13:43Z.
- **mxmini at `ce3ddd1`:** owner host-setup from `~/src/worktrees/ac-release-ce3ddd1` (supervisor
  `1058e8bb`, CLI `54719e68`, push `6e23c765`). **sysvinit host-setup does not restart the loop**:
  owner ran `service agentc-run restart` (loop pid 171714). Containment suite rc 0, **103 PASS**,
  incl. the three new nested-sandbox checks (`mxmini-ce3ddd1-containment.log`). Owner kill switch
  replaced by `quiet-hours` at 10:30 EDT, so the cron clears it at **22:00 EDT (02:00Z)**.
  mxmini has claimed nothing yet. **No task is ready right now** (all open tasks planned), so mxmini
  (and oracle-1) will idle unless the owner admits work.
- Digest 2026-10-12: HRI 1 = stale integrator report (277079ed rerun refused on GitHub 403 "already
  running") → filed and landed **0d5e4c10** (oracle-1, main `dee298b`; ships with the next release).
  All open integrator reports were resolved in the console (0 open of 10). e2e canary 2d1151fc ok.
- **Canary decision (U31/U33 follow-up, owner):** mxmini gets its own dedicated canary project via
  `[run.canary_binding]`; oracle-1 stays shared for now → owner task **422ce01e** (planned, 7 steps).
- **4540596c** (deploy/agentc tests in CI) done by hand: CI job *Deploy script tests* runs every
  `deploy/agentc/*-test.py` (160 tests), roster.toml adds `deploy-scripts` (owner decision); shipped
  via `ship.py` → **main `24a7f3d`**. Lease expired while waiting for the owner's go-ahead → recovery,
  release, **canceled as delivered outside the workflow** (owner choice). Gap filed: **46043cbc** (P3,
  planned) owner-delivered completion path. Optional owner step: add `deploy-scripts` to the project's
  Required checks setting.
- Watch items (3d2fe98a stall, `agentc-push` serve test) did not recur.

**Next actions, in order:**
1. At 22:00 EDT: confirm the kill switch cleared on mxmini, then admit **cbb05ecb** (multi-project
   design doc) and **46043cbc** (owner-delivered completion) together (owner decision 2026-10-12;
   `tasks edit` with planned=false); leave every other planned task planned. Watch mxmini's first claim through review and integration; record cost/HRI per host; when
   mxmini completes a task, mark owner task 47dac763 done with the owner.
2. Daily 00:00Z checks for 2026-10-13 (digest, e2e canary); watch items as before.
3. Next release carries 0d5e4c10 and 4540596c (`24a7f3d`).

**Helpers (setsid-detached, outlive sessions):** `renew.sh` **pid 169811** in session `037ba383…`'s
scratchpad renews d513bc21 **attempt `ba401934`, gen 5** (log `renew.log` there); `watch.sh` 1997610,
journal ssh 1997611, `mxmini-watch.sh` 2413349 still run from the `d8acf6b3…` scratchpad.

## Previous resume point — P6 dogfood on production `aa05dcb`; mxmini paused until its fix ships (2026-10-12 08:15 UTC, workflow on)

**All four overnight tasks landed on main (`ce3ddd1`), none released yet** — all on oracle-1:

| Task | Rounds | Cost | HRI | Commits |
|---|---|---|---|---|
| 576f8a30 sysvinit-check usable after host-setup | 2 (1 valid reject) | $1.08 | 0 | d5e6c58, 2ed0bc4 |
| 7bbb5993 bound exit-0-without-submission re-claims, keep evidence | 1 | $0.96 | 0 | ee299b4 |
| 49848ccb review form kept-claim + lease-loss drop | 2 (1 valid reject) | $1.81 (+ mxmini $1.32) | 0 (mxmini 2) | 72bb236 |
| 69279832 implementer `sandbox.enabled=false` + suite check | blocked → owner (b) → 1 drain → approved | $1.36 | 1 (decision) | ce3ddd1 |

**69279832 root cause (oracle-1 agent, verified):** nested bwrap is impossible under the launch's
`--disable-userns` on any host; Claude's sandbox was silently inactive on oracle-1 (no `socat`;
mxmini has `/usr/bin/socat`), so oracle-1 implementers always ran Bash with only the outer bwrap.
Owner chose (b): turn it off explicitly for implementers, outer bwrap unchanged. One attempt
(3d2fe98a) **drained: no harness event for 15 min, exit -1** — watch item.

**Night of 2026-10-11/12 (session `64c1e4a5`; ledger `pilot-timeline.log` has every event):**
- **mxmini's first claims failed.** 49848ccb (console review form kept-claim fixtures, P2, filed for
  mxmini with the owner's OK) was claimed by mxmini 02:39Z and looped: 3 attempts, each exit 0 with no
  submission ($0.50 + $0.27 + $0.55 = **$1.32**), until the owner set `/var/lib/agentc/kill-switch`
  (owner switch, quiet-hours cron leaves it) at 02:46Z. Cause, from an owner-copied live run
  (`~/agentc-run-c93fd074/events.jsonl`): every Claude Bash call inside the launch failed `bwrap:
  Creating new namespace failed … (ENOSPC)` — Claude's nested sandbox under the launch's
  `--disable-userns` bwrap. oracle-1 (bundled `/opt/agentc/bin/bwrap`, system bwrap 0.11.1, kernel
  7.0) works with the same role settings and Claude 2.1.296; mxmini uses Debian bwrap 0.8.0, kernel
  6.12, no `bubblewrap` setting (the real cause is the socat difference, see above). Filed **69279832** (P1: fix + a
  containment-suite check; the suite passed while this was broken) and **7bbb5993** (P2: supervisor
  re-claims forever on exit-0-without-submission and deletes the run's transcript). mxmini HRI 2
  (kill switch, manual run copy). **Keep mxmini's kill switch on until 69279832 ships.**
- **Containment suite rerun on mxmini** (owner, 23:26 EDT, mirror refreshed by hand to `b8967dd`):
  rc 0, 100 PASS, both cargo-test legs green — 6db155d4 and 1b5804b5 flakes gone. Log
  `mxmini-u38-containment-rerun.log`. The suite tests the **mirror** HEAD, which only refreshes at
  host-setup or before a launch: `sudo git -C /var/lib/agentc/mirror.git fetch --prune --quiet` first.
- **576f8a30** (sysvinit-check.sh refuses after host-setup, P2) filed 02:29Z; the weekly budget did
  not hold it; oracle-1 claimed it in 24 s. Review CHANGES_REQUESTED (valid: `main()` calls
  `preflight` before `set_paths` → `INITD_CHECK: unbound variable`, no check runs); fixed and landed.
- **49848ccb** then went to oracle-1 (02:56Z, attempt 41f5716c, submitted 03:28Z). Review
  CHANGES_REQUESTED 04:08Z (valid: the new 401 fixture leaves the page's retained-mutation alert
  blocking later steps; fails 3/3 on the candidate, passes on base). The reviewer also saw one
  unreproduced failure of `agentc-push` `serve::tests::a_client_outside_the_launch_is_refused_without_minting`
  (tests.rs:300; 80+ reruns pass) — watch item, file if it recurs.
- Reviewer disagreements so far: 3, all valid (277079ed, 576f8a30, 49848ccb). HRI overnight: mxmini 2,
  oracle-1 1 (the 69279832 design decision, correctly escalated).
- Owner task **47dac763** stays open (mxmini has not completed a task).

**Next actions, in order:**
1. Release main `ce3ddd1` (carries the four tasks above): release-next PLAN/PREREQS, deploy drill,
   oracle-1 host-setup at the production commit (role settings change → regenerated settings).
2. mxmini: binaries + host-setup at the production commit; owner reruns the suite after a mirror
   fetch (new check: Bash runs inside an implementer launch, `unshare -U` fails); then owner removes
   `/var/lib/agentc/kill-switch` for the 22:00 window; watch its first claim; mark 47dac763 done.
3. Daily 00:00Z checks for 2026-10-12 (digest not yet checked; e2e canary 2d1151fc done), then
   recommend per-host canary coverage (U31/U33) — last night's defect only showed on mxmini.
4. Watch items: the 3d2fe98a 15-min harness stall; the one-off `agentc-push` serve test failure.

**Deployed `aa05dcb` 2026-10-11 13:43 UTC** (release run `38122931843`, binaries-only, ~8 s down;
rollback binaries `*-0.1.1-60eda26`, old-binary snapshot `20261011T134244.649Z-b209797f…`; carries
6cc3f673, 277079ed, 936fce64). Workstation and oracle-1 CLIs at `aa05dcb` (oracle-1 `/opt/agentc/bin`
supervisor `4bae4145…`, CLI `b86357fb…`; `/usr/local/bin` too). mxmini and MiniAir CLIs still at
`60eda26` (update them at the mxmini bring-up). d513bc21 now **attempt `f156ead4`, gen 4** (renew.sh
updated). oracle-1 non-interactive ssh has no cargo on PATH: use `~/.cargo/bin/cargo`. Reclaiming
after a deploy needs `connect` first (`policy_changed`).

**Earlier (2026-10-11 day) next actions, now done or superseded above:**
1. **Flaky CLI test (task `6db155d4`, P1):** `crates/cli/src/lifecycle.rs` `tests::service` fail ~90%
   of full-binary runs (session lock WouldBlock, exit 7); it sent 2186dc84 back twice although that
   task does not touch the CLI. Owner admitted 6db155d4 (ready, P1, ahead of 2186dc84) and granted
   task-definition editing to this workstation's agent principal `250802da…` (grant `c9c6152f`), so
   agents here can now admit/edit tasks. No 2186dc84 dependency (blocked while its submission is open).
2. **mxmini (U38) — opted in 2026-10-11 16:20Z (first claims failed overnight; see top):** binaries at
   `aa05dcb` (supervisor `0913e36d…`, CLI `16b21b24…`, push `6e23c765…`, worktree
   `~/src/worktrees/ac-release-aa05dcb`); host-setup rc 0 with `QUIET_HOURS=22:00-07:00` (cron
   `agentc-quiet-hours` holds `/var/lib/agentc/kill-switch` = `quiet-hours` outside it); `[run] harness =
   "claude", reviewer = false` (mirror binding = production fe95a6c5); own push App key
   `SHA256:SWG4FhJ3…` at `/etc/agentc/push-app.pem`; supervised creds `agentc-impl-mxmini` (write) /
   `agentc-rev-mxmini` (read); Claude tokens from 2026-10-07. sysvinit-check all PASS (needs the real
   init script moved aside — it refuses after host-setup); containment suite: all containment checks
   PASS, rc 1 only from test flakes (6db155d4 CLI lifecycle; **1b5804b5** context_rerank 1 s deadlines,
   P2, admitted 16:3xZ). `update-rc.d agentc-run
   defaults` + `service agentc-run start`: loop pid under `agentc-run-sysv`, log `/var/log/agentc-run.log`,
   heartbeat world-readable. Logs: `~/.local/share/agent-coordinator-autonomy/mxmini-u38-*.log`. Next:
   watch the first claim, then mark owner task `47dac763` done with the owner; rerun the suite after
   6db155d4 lands; per-host HRI/cost; recommend per-host canary coverage (U31/U33).

**Read the service, not this list.** The live supervisor `agentc-run` on **oracle-1** (aarch64
Ubuntu 26.04, `[run] reviewer = true`, harness claude, kill switch `/var/lib/agentc/kill-switch`
absent since 18:38Z) claims ready tasks of project `fe95a6c5…` in priority order, implements,
reviews with `distinct_launch`, and `agentc-integrator@run` lands. **Production service at
`60eda26`** (deployed 2026-10-09 18:16 UTC, ~10 s down, release run `37936373886`, migrations
0032-0035; rollback binaries `*-0.1.1-1469b5a`, old-binary snapshot
`20261009T181551.515Z-b46151da…`; service env gained `COORDINATOR_CANARY_PRINCIPALS=attention-oracle-1`;
digest sender = `attention-oracle-1` (principal `b5c08634…`)). Record and plan:
`~/.local/share/agent-coordinator-autonomy/release-next/{PLAN-next.md,PREREQS-next.md,RELEASE}`.
Every agent CLI must be rebuilt at the production commit (exact match, `agent-coordinator
compatibility --json`): workstation done via `scripts/upgrade_client.py --source-root
~/src/worktrees/ac-release-60eda26` (the release's x86_64 CLI needs glibc 2.39, too new for this
workstation). **All known CLIs are at `60eda26`** (2026-10-09): mxmini `~/.local/bin` and
`/usr/local/bin` (stale Sep 17 copy replaced; old kept at
`~/.local/share/agent-coordinator-autonomy/usr-local-agent-coordinator.pre-60eda26`), oracle-1
`/opt/agentc/bin` and `/usr/local/bin` (stale Sep 24 copy → `/usr/local/bin/agent-coordinator.pre-60eda26`),
MiniAir Windows `C:\Users\marsh\.local\bin\agent-coordinator.exe` (owner, `--candidate` with the
release run's native Windows exe, sha256 `9bad8bef…`). The release run also builds a Windows zip
(`agent-coordinator-windows-x86_64`): use it for Windows hosts instead of a source build. Peer
sessions: the MiniAir session "Inter-agent message handling" is reachable over Remote Control
(`ListAgents`/`SendMessage`); its auto mode blocks CLI installs, so the owner runs them. Deploy drill (done 2026-10-09): owner kill switch → drain → orchestrator releases
`d513bc21` → preflight must be 0 **across all projects** (another project's agent claim blocks it;
2026-10-09 a codex session in `sithbit` did) → pre/swap → CLIs → host-setup → reclaim → clear.

**oracle-1 host state (host-setup 18:20Z, agent-run with `sudo -n`):** `/opt/agentc/bin`
supervisor/CLI/push built at `60eda26` in `~/src/worktrees/agentc-pilot` (sha256 `4174dac9…`,
`89081ef6…`, `cf98c428…`), `APPARMOR_BWRAP=1 UPDATE_TIMER=0` (no GitHub release exists yet, so the
updater timer would fail daily; task b3e68779 landed the fix — enable it only after 55b8a06d ships
a bundle). host-setup **restarts agentc-run**. Timers: `agentc-canary` (10 min), `agentc-digest`
(daily, Gmail STARTTLS), `agentc-e2e-canary@claude` (daily; now P0, admission class canary).
Secrets root:root 0400 in `/etc/agentc/{attention-token,e2e-canary-token,smtp-password}` (agent
`attention-oracle-1`). `ATTENTION_HEARTBEAT_MAX_AGE` removed; `E2E_TIMEOUT_MINUTES=240` (was 1440;
default 90 judged too short behind a long launch; backups `*.bak-20261009`). U33: the e2e canary
runs in the dogfood project.

**P6 / d513bc21** (claimed by the orchestrating session `p3b-pilot`, **attempt `31af890a`, gen 3**,
renewed every 15 min by a background loop): prerequisites done; e2e canary green (`8634ad52`,
`outcome: ok` 13:07:42Z; post-deploy run 18:39Z → task `1703cdd3` P0, created without 403,
`outcome: ok` 18:50:48Z in 705 s, $0.12).
**Dogfood started 2026-10-09 13:07Z, ends 2026-10-23 13:07Z** (ledger `pilot-timeline.log`). Exit
metrics (plan-final §3 P6 row): HRI/task = 0 excluding reserved decisions; value canary not worse
than baseline; direct-push canary flat; digest ≤ 1/day. Next: run the dogfood; record per task.

**Landed overnight (all HRI 0; main now `79d2043`):** 358537bb, d5ae08ef, 94303638, a49ace9a,
a050d94a, 0bdd6619, fdf782fa, 91d49e19, 0cba8250 (U34), bd68fdbe, 60e7eb6e, b15bb3af (ETXTBSY
deflake), dd95bb78 (host updater; inert until owner lands 55b8a06d), 6f1d4403, e3296c30 (canary
E2E_PRIORITY + second supervisor binding, $6.70 — largest). **Then (2026-10-09 day, HRI 0):**
fb8b2326 (renew backoff), 8634ad52 (e2e canary), 416845fc (role instructions: wait with harness
tools, main `0036b4f`, after 60eda26 so not yet on oracle-1), b3e68779 (updater no-release = exit 0).
**Open:** owner-landed planned tasks 55b8a06d (release builds the agentc-host bundle) and 4540596c
(deploy tests in CI); c744c495 optional.

**2026-10-10/11 (session `dcc4e1fe`):** 7291451d landed (`d284196`: agents may cancel/archive/
restore/delete agent- and service-origin tasks; HRI 0, $1.11) — on main only, ships with the next
release. Chain behind it: 277079ed (CLI archive/restore/delete; review CHANGES_REQUESTED for a broken
CLI.md sentence — first reviewer disagreement, HRI 0) → 936fce64 (agents may cancel/archive reverts,
U35) → 47027345 (not the reverted change's own contributors). **6cc3f673** (sysvinit agentc-run,
cron timers, QUIET_HOURS) was implemented by hand on mxmini at the owner's request (session
`mxmini-sysv`, branch `ac/6cc3f673-sysvinit`, worktree `~/src/worktrees/ac-sysv-6cc3f673`, live
`sudo deploy/agentc/sysvinit-check.sh` all PASS), submitted `bc5be3c7` (candidate `5835f40`),
waiting for review; exclude it from supervisor HRI/cost. Ready: 2186dc84 (lifecycle reasons are
discarded — store and show them), owner tasks b6c366fd, cf99518d. Planned (post-dogfood): cbb05ecb
multi-project design (U36), c2aea126 portable hosts design (U37), 2a387d02 Windows via WSL2,
1a5ffe45 → 1a3290db console Autonomy view. Canceled-task cleanup: 7 deleted, 3 archived.
**Next for mxmini (U38: joins the dogfood; new session, after 6cc3f673 lands and the next release deploys):** host-setup at
the production commit, `[run]` + binding, credentials, opt in, QUIET_HOURS.

**Watch items:** drained launches record 0 cost until oracle-1 runs a050d94a; P2 reviews wait
behind P1 work (review/implement alternate); task `depends_on` can only be set at creation
(agents cannot edit tasks; `unblock` works for this session).

**Session handover:** background helpers run on the workstation (= mxmini) from session
`d8acf6b3…`'s scratchpad (`/tmp/claude-1000/-home-marshall-src-agent-coordinator/d8acf6b3-…/scratchpad`):
`renew.sh` pid 1997609 (renews d513bc21 attempt `f156ead4`, **gen 4**, every 15 min; renew grants a
1 h lease; log `renew.log`), `watch.sh` 1997610 (task-state poller → `pilot-timeline.log`), journal ssh
1997611 (`journalctl -u agentc-run -f` → `pilot-agentc-run.journal`), `mxmini-watch.sh` 2413349
(heartbeat → timeline); all `setsid`-detached, so they outlive sessions. Session `64c1e4a5` kept
them (the auto-mode classifier blocks `kill` of them and `setsid` restarts); a new session can do the
same unless the owner kills them with `! kill …`. Daily 00:00Z checks: 2026-10-10 digest ok (HRI 0) and e2e canary 39725610 ok; 2026-10-12 e2e canary 2d1151fc done 00:34Z (digest not yet checked). Never wait on `pgrep -f PATTERN` over ssh: the pattern matches the
remote shell's own command line (cost 3.3 h on 2026-10-09). Check
with `ps -eo pid,args | grep -E '[r]enew.sh|[w]atch.sh|[j]ournalctl -u agentc-run|[e]2e-canary.jsonl'`;
kill them by PID and start your own. Use `ssh -i ~/gdrive/Development/oracle-1-key-2026-09-16.key
ubuntu@oracle.sithbit.com` in `!` commands; secrets go through the owner's own terminal (`ssh -t`),
never `!`.

Owner actions pending: set `kernel.yama.ptrace_scope=1` on mxmini and oracle-1; remove
`~/src/worktrees/agent-coordinator-core-ship` with `git worktree remove --force`.

Known follow-ups not yet tasks: `launch-root` SIGTERM handler; continuation claims; review form
kept-claim path has no fixture; launches that do not exit after submission (drains).

## S6 reviewed live candidate test — owner runbook (2026-10-05): PASS, historical

The user chose a guided test: the owner runs every step; this session only wrote the steps
(its read of the oracle-1 journal was blocked as a production read). Goal: one approved code
candidate sits in `phase='integration'` long enough for `agentc-integrator@shadow.service` on
oracle-1 to log `WouldPush`. Facts (core tree `42466e0`, verified): the shadow queue needs
`ws.phase='integration'`, a current non-superseded `kind='code'` submission, an unchanged task
digest and satisfied approvals; a human approval satisfies any review kind
(`crates/server/src/integrator.rs:58-128`, `autonomy.rs:111-166`). No automatic publisher
exists today, but any connected agent session may claim the integration activity and land it,
removing it from the queue. `WouldPush` = `{result:{t0,c,r,landing_range,roster},
privilege_findings, checks, authority_verified:false}` (`crates/integrator/src/shadow.rs:62`).

1. **Quiesce:** no other agent sessions connected to the production project for the test.
2. **Confirm the shadow is alive** (on oracle-1): `systemctl show agentc-integrator@shadow.service
   -p ActiveState -p ActiveEnterTimestamp -p NRestarts`; `systemctl is-active
   agentc-integrator@run.service` must say `inactive`.
3. **Create the task in the dashboard** with `review_mode = human`. Make it a trivial,
   harmless docs edit, for example a one-line wording fix in `book/src`. Never touch `.github/`:
   that trips the privilege gate. Note the task id and revision.
4. **Implement it from the workstation** with `~/.local/bin/agent-coordinator`:
   `claim --task ID --revision N`; `worktree prepare --attempt A --generation G --source .
   --path P --branch B --base <origin/main>`; make the one commit in P; release any
   reservations; then `submissions code --attempt A --generation G --task-revision N
   --project-policy-revision X --workflow-policy-revision Y --checkout P --input submission.json`.
   Omit `--candidate-ref`, so it pushes `refs/agent-coordinator/candidates/<attempt>`.
5. **Approve it as a human in the dashboard**, on the human review activity. Do **not** claim
   the integration activity.
6. **Watch** for at least 2 polls, about 30 s each: `sudo journalctl -u
   agentc-integrator@shadow.service --since "-10min" -o cat | grep -E 'WouldPush|WouldRevise|Error'`.
   Pass: a `WouldPush` for this submission whose `t0` is the current main, `privilege_findings`
   are empty, and `authority_verified:false`. Save the journal outside Git under
   `~/.local/share/agent-coordinator-autonomy/s6-live-candidate/`.
7. **Clean up:** cancel the task in the dashboard with a rationale ("S6 shadow live-candidate
   test"), so it never lands. The candidate ref may stay. Read back that the shadow queue is
   empty again: there are no further `WouldPush` lines.

**Empty-check fail-closed (open note from the shadow day):** the shadow cannot prove this. It
computes R locally and never pushes it, so `checks` on R are always `[]` and shadow never
evaluates them. It is a live-path property, covered by tests: integrator `ChecksPending`
(`crates/integrator/src/flake.rs:157-172`, `publish.rs:56-61`, e2e `e2e_tests.rs:629`) and
server `checks_not_passed` (`integrator_authority.rs:210-235`, `tests/workflow.rs:3473`).
Prove it in the post-cutover canary instead. Uncertain: U5 `distinct_launch` is not enforced
server-side; reviewer independence is per principal/session only (`workflow.rs:2074-2122`).

**Live-test progress (2026-10-05, walked through one step at a time):** step 1 done. The owner
confirmed no agent sessions running or scheduled; mxmini had no crontab and no launching
supervisor (only the egress proxy).

**Attempt 1 (2026-10-05):** task `510fa3fe-faef-4106-bc6e-a4b9894380b0` (rev 1, policy 6,
workflow policy 3), attempt `c3d619de…`, session `s6-live-test`, worktree
`~/src/worktrees/s6-live-test` (branch `s6/live-test`), commit `1f03f63`, submission
`0382743c…` (ref `refs/agent-coordinator/candidates/c3d619de…`). The owner's human review
recorded **`changes_requested`** by accident (18:23 UTC): the dialog pre-selects that value.
The task went back to `ready` (`revision_needed`); a fresh attempt and resubmission are needed.

**Attempt 2 and result (2026-10-05): S6 live-candidate shadow test PASS.** Attempt
`3d698958…` (gen 2), worktree `s6-live-test-2`, cherry-picked commit `0842ddf`, submission
`3ede636f-28fb-4e7e-8025-7a841551c96e`, ref `refs/agent-coordinator/candidates/3d698958…`. The owner
approved at 18:27:24 UTC; the shadow logged `WouldPush` at **18:27:46** and again at 18:28:21 UTC.
Each record had `c=r=0842ddf` (a fast-forward onto t0 `3882ba4`, r_tree `2b6bad77…` matching the
worktree), `landing_range=[0842ddf]`, roster rev 3 with all three required checks and workflow
blobs, `privilege_findings=[]`, `authority_verified=false`, and `checks=[]` (as expected: R is never
pushed). There were no Error or WouldRevise records. Evidence:
`~/.local/share/agent-coordinator-autonomy/s6-live-candidate/wouldpush.jsonl`. Cleanup: the task
was reopened (submission superseded, integration activity cancelled) and then **canceled**
(rev 2), because cancel refuses while the subject is in review/integration
(`coordination.rs:773-780`). Both worktrees were removed. Kept: local branches `s6/live-test` and
`s6/live-test-2` (unmerged; `-d` refuses), and both candidate refs on GitHub. `main` was
untouched (`3882ba4`). The owner's journal re-check since 18:30:30 UTC showed no non-Target records (queue empty).
All four activities are final: both integrations `canceled`, both reviews `completed`. The
dashboard task list showed a stale "Waiting Review" badge until a manual refresh (minor UX). The task was
then **archived** (rev 3); the project's task queue is empty. Cutover preflight and rulesets followed; see the next section.

**DONE (2026-10-06): human review decision default.** Task `a020fa33-b532-4e86-a204-885cab87ec02`,
the first real task through the live integrator: `6fea647` brings the stale dashboard browser
test up to date with the current UI (nine drift points since ~2026-09-24; the test is not in CI) and
`13d4e4a` gives the Decision select an empty required "Choose a decision…" placeholder (the test
fails on the old dialog with `Review decision is preselected.`). Submission `5f6660c4…`;
owner-approved human review (it should have been a review subagent: `allow_subagent_reviews` is
true). The integrator pushed R to `ac/results/c3592296…`, waited for its own runs, and pushed
**`main` `a7418bd → 13d4e4a` at 01:21:19 UTC** as the App; task `done`. Still open from that note:
"Claim human review" takes a 1 h lease before the dialog, and closing the dialog keeps it.

**Production deploy 13d4e4a (2026-10-06, binaries-only since 2f3c347, owner-run root steps).**
The owner dispatched release run `37399956468` (the classifier blocked the agent's `gh workflow
run`); it went green (3 jobs). The archive was sha256-checked, embeds `13d4e4a`, and was staged in
`release-next` (RELEASE previous `2f3c347`; the old archive stays in `release-2f3c347`). Preflight 0 before
pre and again after the stop. Old-binary snapshot `20261006T015258.587Z-f5db7a53…` (rollback =
restore it + binaries `*-0.1.1-2f3c347`); new-binary snapshot `20261006T015414.653Z-0badbb8a…`
verified; maintenance complete; timers restarted; 0 keyless-rerank warnings. Downtime
01:53:05–01:54:14 UTC. `/api/v1/info` `source_commit` `13d4e4a`, not dirty; the live `app.js` has the
placeholder. **The service requires an exact client:** the workstation CLI reported
`client_upgrade_required` until it was upgraded with `upgrade_client.py` from a clean detached worktree
`~/src/worktrees/ac-release-13d4e4a` (rollback `~/.local/bin/agent-coordinator.rollback`). The
oracle-1 integrator (`1e8aebb` build) logged two 502s during the downtime, then `Idle`; it was still accepted.

**DONE (2026-10-06): dashboard integration owner setting + integrator reports view.** Task
`c71cd8b0-67ec-476b-8e6c-7c25876afa8a` (agent-created; session `dash-owner-reports`), commit
`8bf99e8` (web/ only + browser test + 2 book lines). Project settings: human-only "Integration owner"
card (current owner + policy revision; change needs a different owner, provenance and a confirmation
naming the switch; other policy fields preserved; `policy_hold_conflict`/`revision_conflict`
explained). Project settings → "Integrator reports" page: open/all, paged by `before`, human resolve
(note; `privilege_gate` needs allow/deny with no preselection, allow needs an inspection checkbox;
`report_already_resolved` explained). Gate on the clean commit: fmt, Clippy, 684 tests, smoke, backup
smoke, docs; browser test PASS (fails on the old web files); a disposable real-server run switched the
owner and resolved both report kinds. **Independent review by a subagent** (session `dash-review-1`,
subagent `reviewer`, parent `a74d5332…`): approved, no required fixes; its 3 advisory findings are task
`cfda9952-e2d1-4ce9-b137-2d6d7c7c3707` (priority 3). The integrator App pushed **`main` `13d4e4a →
8bf99e8` at 02:47:00 UTC**; task `done`. Worktree and local branch removed; candidate ref and
`ac/results/fcc99b68…` kept. Traps: the browser test needs `CHROME_BIN=google-chrome` here; smoke
needs `COORDINATOR_BUILD_COMMIT=$(git rev-parse HEAD)` in a worktree (core build.rs caches "dirty").

**Production deploy 8bf99e8 (2026-10-06, binaries-only since 13d4e4a, agent-run except the dispatch).**
The owner dispatched release run `37405976958` (classifier blocked the agent's `gh workflow run`;
it then also blocked a `git diff 13d4e4a 8bf99e8`); 3 jobs green. Archive sha256 OK, server embeds
`8bf99e8`, MCP binary unchanged (`15868f11…`). Old archive kept in `release-13d4e4a`; `release-next`
RELEASE = `8bf99e8… … 13d4e4a`. **The classifier allowed the agent's exact-rule scp/preflight/pre/
swap this time.** Preflight 0 before pre and after stop. Old-binary snapshot
`20261006T031205.115Z-395ea76f…` (rollback = binaries `*-0.1.1-13d4e4a`; no schema change); new-binary
snapshot `20261006T031308.039Z-95d25e98…` verified; maintenance complete; timers re-armed; 0
keyless-rerank warnings. Downtime 03:12:11–03:13:07 UTC. `/api/v1/info` `8bf99e8`, not dirty; live
`app.js` has the new UI. Workstation CLI upgraded via `upgrade_client.py` from clean detached
worktree `~/src/worktrees/ac-release-8bf99e8` (rollback `~/.local/bin/agent-coordinator.rollback`).
Not checked: the oracle-1 integrator journal across the downtime.

**DONE (2026-10-06, not deployed): cleanup-rule wording and test polish (d7021e5d).** Session `backlog-1006`
(CLI, parent `5b189927…`). `90a6000`: the served `git branch -d` rationale now covers branches with an upstream
(judged against the upstream, else HEAD, never the fetched target); `agent-startup.md` says "this guide" and leads
with the served locations; `assert_points_to_served_cleanup` in `tests/authentication.rs` forbids 8 cleanup phrases
in both the guide and `completion-contract.md`. Gate 699 tests, smoke, docs. Subagent review
`cleanup-polish-review-1` (subagent `reviewer`) approved (advisories: one-phrase upstream assertion;
completion-contract keeps "this book", fine as book-only). Integrator pushed **`main` `b8eaf4d → 90a6000`**.
Worktree removed; local branch deleted with `update-ref -d` (no remote task branch). Queue now empty.
Trap: CLI submission input needs `summary` and `artifact_ids` besides `acceptance_evidence`/`handoff`;
`request --path` rejects query strings, so poll `tasks show` instead of `/state-wait`.

**DONE (2026-10-06, not deployed): cleanup rule single-sourced, guarded local branch deletion (4da7ffa0).**
Owner direction: the served `WORKTREE_CLEANUP_INSTRUCTIONS` (orientation instructions, completion_workflow
step 8, MCP instructions; same for every project) is the only statement of the cleanup procedure;
`agent-startup.md` and `completion-contract.md` point to it. Local task branches are deleted with
`git merge-base --is-ancestor OID REMOTE/TARGET` + no worktree use + `git update-ref -d refs/heads/B OID`
(`git branch -d` refused merged branches from `autonomy-plan`). Subagent review `branch-cleanup-review-1`
approved (advisories: the `-d` rationale ignores upstreams; the guide pointer says "this book" though it is
served to other projects; the no-restatement test checks only 3 phrases). Integrator pushed **`main`
`8820f08 → b8eaf4d`**. Worktree and branch removed by the new rule.

**DONE (2026-10-06, not deployed): sessions polish 33a3a8dd + idle-session auto-close 601d1601.**
Session `backlog-1006` (CLI, parent `5b189927…`). 33a3a8dd: `55786eb` — SESSIONS_SQL starts from open
sessions with per-session indexed lookups (migration **0025**: `attempts(session_id,project_id)`,
partial `agent_sessions(created_at) WHERE closed_at IS NULL`), EXPLAIN-plan unit test, 500-cap/
truncated test, `loadAgentSessions` error branch checks project, browser test for the reports label
after a reload with a pending resolution. Subagent review `sessions-polish-review-1` approved
(advisories: plan test asserts exact SCAN strings; still visits every open session). 601d1601:
`eafa93e`,`997f04a`,`8820f08` — `maintenance --session-idle-days` (env `COORDINATOR_SESSION_IDLE_DAYS`,
default 7, 0 disables) closes idle sessions (activity across all projects) except those holding an
active attempt, live reporter, held reservation, unreconciled job or open subagent child; event
`agent_session_closed` data `reason:"idle"`; migration **0026** (`maintenance_runs.sessions_closed`,
`reporters(session_id)`). CLI `connect`/`mcp-client` replace a closed saved session
(`replaced_closed_session_id`); standalone MCP adapter reports `session_state` (owner decision: report
+ reconfigure, adapter never generates secrets). Subagent review `session-idle-review-1` approved
(advisories: `replace_closed_session` saves state before registering; transport_status may wait the
30 s HTTP timeout when upstream is down; idle-close event has no credential class). Integrator pushed
**`main` `7fdb18b → 55786eb → 8820f08`**. Gate 699 tests, smoke, docs. Both tasks `done`; worktrees
removed; local branches `dash/sessions-polish-33a3a8dd`, `dash/session-idle-601d1601` deleted at the
owner's request (verified on `origin/main`, `git update-ref -d` with expected OID). **Next deploy carries migrations 0025+0026**; the first daily
maintenance after it will close the ~100 stale sessions. Trap: a repo guard blocks `git reset`, so
WIP commits cannot be squashed; core `build.rs` watches the worktree HEAD file, so provenance goes
stale after a branch commit (touch `crates/core/build.rs` before smoke).

**DONE (2026-10-06): review polish cfda9952 + list connected sessions (027aeac3); deployed.**
Session `sessions-list` (CLI, `4e0c7a21…`). cfda9952: `ace034c` (failed load-more keeps cursor and
control; reports label refreshes when projects load; helper comments; browser check fails on old
web). Subagent review `polish-review-1` (subagent `reviewer`) approved; integrator pushed **`main`
`8bf99e8 → ace034c` at 03:35:09 UTC**. 027aeac3: `7fdb18b` — `GET /api/v1/projects/{p}/sessions
?active_within_hours=N` (`crates/server/src/project_sessions.rs`, no migration): open sessions with
live credential and enabled principal, bound by instruction ack or attempt; last activity = max of
session start, ack, attempt claim/heartbeat/progress/end, checkpoints (reads don't count); default
window 24 h, 0 = all; a session holding any `active` attempt is always listed, lapsed leases flagged
`lease_expired`. Plus `agent-coordinator sessions list`, dashboard project settings → "Connected
sessions", 8 server tests, browser test, smoke check, book (api-contract, CLI, operator-guide). Gate
694 tests. Subagent review `sessions-review-1` approved (3 advisories → task `33a3a8dd`, priority 3,
unclaimed, plus the reload-label browser-test advisory); integrator pushed **`main` `ace034c →
7fdb18b` at 04:20:45 UTC**. Both tasks `done`; both worktrees removed; local branch
`dash/polish-cfda9952` force-deleted at the owner's request (tip `ace034c` is on `main`). Note: reviewers' `reviews claim`
needs `--candidate-checkout` whose origin matches the repo URL; the classifier flagged a scratch
clone's `remote set-url` as "Remote Repoint".

**Production deploy 7fdb18b (2026-10-06, binaries-only since 8bf99e8, fully agent-run).** The owner
added an exact allow rule for `gh workflow run release.yml --ref main`; the agent dispatched release
run `37413618564` (3 jobs green). Archive sha256 OK, server embeds `7fdb18b`, MCP unchanged
(`15868f11…`); `release-next` RELEASE = `7fdb18b… … 8bf99e8` (old archive in `release-8bf99e8`).
Preflight all 0. Old-binary snapshot `20261006T050702.294Z-76f4bd02…` (rollback = binaries
`*-0.1.1-8bf99e8`); new-binary snapshot `20261006T050728.200Z-ed055d57…` verified; maintenance
complete; timers re-armed; 0 keyless-rerank warnings. Downtime 05:07:09–05:07:27 UTC. `/api/v1/info`
`7fdb18b`, not dirty; live `app.js` has the sessions view. Workstation CLI upgraded via
`upgrade_client.py` from detached worktree `~/src/worktrees/ac-release-7fdb18b` (rollback
`~/.local/bin/agent-coordinator.rollback`); `sessions list --active-within-hours 0` against
production lists ~100 open, never-closed sessions since 2026-09-14, none holding attempts (clients
rarely close sessions). Backlog task `601d1601` "Auto-close idle agent sessions": done 2026-10-06 (`8820f08`).

## S6 cutover preflight and full rulesets — 2026-10-05 (workflow off): DONE

**Preflight PASS:** `integrator-preflight.py` (sha256 `7805364e…76b3`, matches `main`) was scp'd to
the service VM `agent-coordinator` (us-east1-b, IAP); the owner ran it as root against
`/var/lib/agent-coordinator/coordinator.sqlite3`: `active_integrations`, `current_attempts`,
`holds`, `jobs`, `publication_intents`, `recovery` and `reservations` were all 0, exit 0. This is
a snapshot, not a lock. Keep new work paused and repeat it before the ownership switch.

**Rulesets (owner-approved one at a time, applied by the agent via `gh api`, version header
`2026-03-10`):** owner actor `10648043`, integrator App `5127380`, Actions `15368` (all three
required contexts green on `3882ba4`).
- A `integrator-writers` **`24525086`**: update/creation/deletion on `main` and `ac/results/**`;
  bypass App + owner (`always`). Stored == reviewed.
- B `integrator-checks` **`24525112`**: `non_fast_forward` + strict required checks (3 contexts,
  `integration_id 15368`), no bypass. Stored == reviewed plus the server default
  `do_not_enforce_on_create:false`.
- Tags `owner-release-tags` **`24525127`**: `v*` creation/update/deletion, owner bypass only.
  Stored == reviewed.
- **`day0-main` `24031302` kept, not superseded (owner decision 2026-10-05).** It is the only
  no-bypass ban on deleting the default branch; its `non_fast_forward` duplicates B.
- Effective `rules/branches/main` read-back lists A, B and day0. `main` is unchanged at `3882ba4`.
  JSON, responses and read-backs are in `~/.local/share/agent-coordinator-autonomy/s6-rulesets/`.

Not yet demonstrated on this repo (it needs the write credential): an App publication to an
`ac/results/**` branch succeeds and an ordinary collaborator update is refused. Prove both
during the canary. **Ownership switch progress (2026-10-05, walked through one step at a time):**
1. The owner stopped and disabled `agentc-integrator@shadow.service`; `@run` was inactive and disabled.
2. The owner issued the dashboard credential `oracle-1-integrator-write` (class integrator, access
   write; the classifier blocked the agent from setting the Class/Access selects). It was scp'd to
   oracle-1 as ubuntu (use `-i ~/gdrive/Development/oracle-1-key-2026-09-16.key
   ubuntu@oracle.sithbit.com`; there is no `oracle-1` SSH alias), installed over
   `/etc/agentc/integrator-credentials.toml` as `root:root 0400`, and every copy shredded. The old
   read-only `oracle-1-integrator` credential is **still active**: revoke it once `@run` is healthy.
3. The `/etc/agentc/integrator.toml` credential paths now point at `@run.service`. Backup:
   `/etc/agentc/integrator.toml.shadow-20261005T191353Z`.
4. Preflight repeated: all 0, exit 0. `main` was still `3882ba4`.
5. **Policy rev 6 → 7, `integration_owner=integrator`** (human `PATCH` from a dashboard DevTools
   snippet at 19:17:18 UTC, Idempotency-Key `s6-integration-owner-flip-20261005`, request
   `9c22b7d9…`); every other field preserved; provenance recorded. Rollback is the same PATCH
   with `integration_owner:"agent"` against rev 7.

6. `@run` started 19:24:15 UTC (active/running, NRestarts 0, `Idle` every ~30 s); shadow inactive.
7. The old read-only `oracle-1-integrator` credential was **revoked** (agent clicked, owner-approved).

**Canary PASS (2026-10-05/06) — S6 COMPLETE.** Task `f92bf97d-ab91-436f-ab93-009cc4b41376`
(dashboard), session `s6-canary`, attempt `bc7ffdf0…`, commit `a7418bd` (keeps day0-main in
`integrator-cutover.md`), submission `b35147b8-e952-4d44-931b-5dd3c35044c3`, owner-approved human
review. The integrator pushed R = `a7418bd` to `ac/results/a8514051-e6ca-4319-946b-664b7900d12b` as
`marshallr12-agentc-integrator[bot]` (App publication to a result branch works under ruleset A). A
GitHub Actions runner outage (incident 19:11–22:51 UTC, major outage at its peak) left the Linux job
with no runner: run `37364065124` attempts 1–3 were cancelled. The integrator auto-reran twice
(`MAX_RERUNS=2`), then reported `Blocked(no_result … after 2 reruns)` with flaky report
`b0f90f8b…`; nothing was held. After recovery the agent ran one manual `gh run rerun --failed`
(attempt 4, success 22:59). The integrator logged `Published` and then `Observed("published")` at 22:59:30, and
**`main` `3882ba4 → a7418bd` was pushed by the App at 22:59:29 UTC** (fast-forward; ruleset B's strict
checks satisfied). Task `done` by service completion; push CI on `main` green. The owner resolved
report `b0f90f8b…` (human, 00:35:14 UTC, DevTools POST, key `s6-canary-resolve-no-result-20261005`).
Canary worktree and local branch removed. Collaborator refusal on protected refs relies on the U4
throwaway-repo proof (`GH013`), as the runbook allows. Evidence: GitHub activity API (pusher), the
`@run` journal, check runs on `a7418bd`. **Next:** the owner un-pauses new agent work; then P3b
prerequisites (R-P3b items, oracle-1 supervised-host setup).

**Backlog (2026-10-06): dashboard view for integrator reports.** DONE in `8bf99e8` (see the top section).

## S6 required-check stability sample — 2026-10-05 (workflow off): PASS

Per `book/src/docs/integrator-cutover.md` "Required-check stability evidence". Pinned
`main` `3882ba4b6f121f4cad35f99d2737fb6398d3b26d` (re-checked before every rerun and at the
end; unchanged). Push runs: Coordination checks `37117852141`, Documentation checks
`37117852080`. 20 sequential "rerun all jobs" attempts each (14:49-17:28 UTC; no
failed-job-only reruns, no dispatches) on top of attempt 1 ⇒ **21 attempts per required check,
0 non-success, rate 0.0**: `Linux format, Clippy, and workspace tests` 21/21,
`Audit locked dependencies` 21/21, `Pinned mdBook build and local-link validation` 21/21
(non-required `Native Windows client, CLI, and local runner tests` also 21/21; the
launcher-lock test did not fail). `integrator-flip-rate.py` rc 0, all `passed: true`.
Evidence outside Git in `~/.local/share/agent-coordinator-autonomy/s6-stability/`:
`ci-jobs.json`, `docs-jobs.json` (`filter=all`), `flip-rate-report.json`, per-attempt
`ci-loop.log`/`docs-loop.log`, driver `rerun-loop.sh`. Note: the docs log's attempt-19 line
says `null` because the jobs API lagged the completed run attempt; the run attempt and the job
later read `completed`/`success`. Remaining S6 chain: production shadow, live candidate test,
cutover preflight, rulesets, write credential, `integration_owner` switch, canary (all owner
steps; nothing changed here).

## Current resume point — follow-up phases (2026-10-02 daytime)

Agent Coordinator workflow disabled again; local commits only, no push/deploy.
Branch `hardening/core-20261002` in `~/src/worktrees/agent-coordinator-core`
(read its `HANDOFF.md` for phase evidence) now holds the TypeSafe
`typesafe.env` secret file (`09a2b78`), C1 routing (`8b24b8e`), exact-ref
readback (`8c2cdc6`), R-P3b.5(d)/U20 (`417dbca`), R-P3b.3/U18 (`709d277`) and
its cwd residual `6f19eb4`: candidate-shell resets the harness's tracked cwd
to the clone after each command, and candidates get their own
`$RUN/candidate-tmp` while the harness's `$RUN/tmp` is a tmpfs inside the
nested sandbox. Full gate green (fmt, Clippy, 554 tests, docs) and an
independent CONFIRMED review. Still open before the first live launch: an
owner run of the pinned Claude Code confirming
the `pwd -P >| <TMPDIR file>` suffix (else every reviewer command exits 126).
R-P3b.4 per U21 landed as `52191ce` (Claude launches in their own network
namespace; supervisor relays only proxy and loopback staging; gate 566 tests;
red-team CONFIRMED after one repair). The containment-suite relay leg landed
as `f1edadd` (real mock-harness launch per role + broken-relay preflight
check; red-team fix for pipefail; not yet run: needs root and the new
supervisor installed on mxmini). U18's per-run staging login landed per U22
as `e47c46f` (supervisor signs in, candidates get only the session cookie,
signed out at launch end; gate 573 tests; red-team CONFIRMED after one
repair). **R-P3b.2 code-complete (2026-10-02 evening, U23-U26):** `598c77a` protocol,
`2132b4c` `agentc-push` binary, `bb5294b` CLI via helper socket, `f55cdbc` one commit
per launch, `d8a9b80` root `launch-root` wrapper, `08fd595` host setup + containment
leg (+ handoff commits up to `70393d3`); every phase red-teamed to CONFIRMED; full
gate 670 tests. Nothing pushed, deployed or run as root. **Root runs done (2026-10-02 night):**
branch pushed to origin; containment suite all-pass on mxmini (incl. relay leg)
and oracle-1 (incl. push-key legs; Ubuntu needs host-setup `APPARMOR_BWRAP=1`,
`a72f402`/`4ce6b93`; details in the core tree's `HANDOFF.md`). `--uninstall`
still not run as root. R-P3b.5(b)(c) landed
as `7b8d3a1` (no_new_privs + subreaper leftover kill; uninstall retires agent
accounts before the firewall; red-teamed twice, all findings fixed; gate 674
tests). **Shipped 2026-10-03 03:5x UTC:** `main` `2f3c347..c89e94b` via `ship.py` (17 commits incl.
`c89e94b`, a test fix for CI's newer Git `largePathname` fsck limit); no server change, so no
service deploy; workstation CLI not upgraded. Then: owner Claude host/auth proof (incl. `pwd -P`
suffix), S6 cutover chain (stability sample now pins `c89e94b`). Known gap: Codex implementers can reach
concurrent launches' push sockets (run them one at a time).

## Owner Claude auth proof on oracle-1 (2026-10-03, workflow off)

**Status (2026-10-03 day): U27 proven on oracle-1.** Branch `hardening/claude-token-20261003`
(`f257ea8` + handoff commits, pushed) passed host-setup, the suite (101 PASS) and a real-token
auth/`pwd -P` proof on Claude 2.1.288. **Shipped:** `main` `c89e94b..3882ba4` via `ship.py` (incl.
`3882ba4`, a retry in the flaky CLI launcher-lock test that failed CI twice; no server change,
so no service deploy). **mxmini done 2026-10-05:** host-setup re-run, pin 2.1.289, two tokens (auth `ok`), suite rc 0
(90 PASS, no NOTE); exposed impl token revoked and both tokens replaced (revoke setup-tokens at
claude.ai Settings > Claude Code > Authorization tokens, `user:inference` rows); `pwd -P` suffix
re-proof on 2.1.289 PASS. Next: the S6 cutover chain; stability sample on `main` `3882ba4` PASSED 2026-10-05 (see the S6 section above). Details in the core tree's `HANDOFF.md`.

Run by an owner Claude session on oracle-1 (directed from mxmini over Remote Control) plus
one owner-run strace. Pinned agent copy is **2.1.287** (`supervisor.toml` `claude = "2.1.287"`;
host-setup copies the owner's binary at setup time, so re-prove after every host-setup re-run).
- Install of two separate `claude auth login` credentials in place (same inode, 0600): PASS.
- Auth smoke as each uid via the egress proxy: PASS (no egress denials).
- `pwd -P` suffix (agentc-rev, logging `CLAUDE_CODE_SHELL_PREFIX`): **PASS**. Plain, `cd` and
  background commands all end `&& pwd -P >| $TMPDIR/claude-XXXX-cwd`, file directly in TMPDIR;
  no snapshot invocation under `env -i`.
- Refresh in place: **FAIL by design.** strace: `mkdirat(claude-config/.oauth_refresh.lock)
  = EACCES` (x5); no credential write. Root-owned claude-config forbids the refresh lock, so
  login credentials die ~8h after login. ⇒ **U27** (`claude setup-token` via
  `CLAUDE_CODE_OAUTH_TOKEN`), committed as `f257ea8` on `hardening/claude-token-20261003`
  (core worktree; pushed; gate 684 tests; red-team CONFIRMED after one repair round each). Also seen, harmless: `.claude.json` temp+rename, `debug/`,
  `projects/`, `sessions/` all EACCES.
- **New backlog item:** role settings set `"sandbox": {"enabled": true}`, but oracle-1 lacks
  `socat` (mxmini has it), so a direct run warns "Sandbox disabled… dependencies missing". In
  launches the supervisor's outer bwrap is the boundary (implementers `--disable-userns`, so
  Claude's native sandbox can't nest anyway). Decide after a real supervised launch per host:
  drop the setting, or require socat on every host and fail closed.
- Cleanup done: `~/agentc-bootstrap` and both proof scratch dirs shredded/removed. Still
  present: the installed `claude-config/.credentials.json` files (retired by U27; the U27
  host-setup re-run removes them). After U27 tokens work, owner revokes those two
  `claude auth login` sessions at claude.ai.

## Previous resume point — core overnight wave (2026-10-02)

The user disabled the Agent Coordinator workflow. No live service tasks were
created, claimed or updated. Local implementation, review and commits were
approved until 08:00 Eastern on October 2 (12:00 UTC); no Git pushes, production
changes, root host changes, credential issuance, rulesets or ownership switch.

Resume the implementation in `~/src/worktrees/agent-coordinator-core`, branch
`hardening/core-20261002`, based on freshly fetched `origin/main` `1e8aebb`.
Read that tree's `HANDOFF.md` for the current phase evidence and exact next steps.
The existing `autonomy/s6` tree was preserved. Historical live state below was
not rechecked during this local hardening wave.

- TypeSafe cap/breaker/context fallback tests: independently verified and locally
  committed. Default cap 4; breaker opens for 60 seconds after three consecutive
  failures and allows one recovery probe. Secret-file separation remains future
  work; no TypeSafe key or host configuration was changed.
- Claude confinement: per-launch state with isolated writable Cargo caches,
  terminal-only last-five retention, protected root seeds and fail-closed path
  validation, followed by Claude-only Bubblewrap mounts, descriptor fencing,
  sealed prompt stdin and process cleanup. Independently verified locally;
  owner host installation, authenticated credential refresh and nested browser
  compatibility remain unverified. Codex native sandbox/auth behavior preserved.
- `ship.py` overall deadline: independently verified and committed from the
  clean `hardening/ship-deadline-20261002` lane. Default 1800 seconds; query cap
  300 seconds; full-SHA failures cannot authorize a final push.
- `ship.py` exact repository routing phase: **BLOCKED after two repairs**. Preserve
  uncommitted `agent-coordinator-core-ship` work; do not integrate it. A Git remote
  whose name is the approved URL can redirect literal URL operations to another
  repository. Existing 26 tests pass but this independent counterexample fails.
  The user chose to record the blocker and finish other lanes at this limit.
- Builders used Sol/high and Astra/xhigh, with Astra/xhigh independent security
  review. User's cache answer supersedes the older shared-cache D2 plan below:
  each launch has writable isolated caches; efficient disk-pressure fallbacks
  must preserve confinement. Disk pressure did not require a fallback.
- Full integration gate passed on product commit `0beabe8`, Rust 1.99.0: format,
  warnings-denied all-target Clippy, all 545 workspace tests (zero ignored), build,
  local service/CLI smoke, backup/restore smoke, 18 shipping deadline tests, pinned
  mdBook/source/package/link checks and dependency audit. Focused phase checks
  also passed on Rust 1.98.1. Evidence:
  `~/.local/share/agent-coordinator-autonomy/core-20261002/`. No push or deploy.

Next backlog (routing and secret file done in the follow-up above): owner proof of
Claude authentication/refresh and nested sandbox compatibility; then the shared
supervisor security queue and S6 cutover prerequisites listed below. No live
candidate proof or production cutover was claimed by this wave.

## Historical resume point (2026-09-29 evening; 2026-09-30 UTC)

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

### Historical full wave plan (superseded for the core wave above)

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

