# Agent Coordinator: where autonomy broke down in the live service data

Source: a read-only snapshot in `scratchpad/svc/` (53 tasks, 6,153 events, 6 knowledge entries, policy history, orientation, export). Snapshot time was about 2026-09-25T16:13Z. Supporting per-task dumps are in `scratchpad/review/dumps/`, and the metrics table is in `scratchpad/review/metrics.txt`.

## 0. Actors, policy timeline, event inventory

**Actors.** Only four principals appear in the data. The report calls them:

| Label | Principal | Identified as | Evidence |
|---|---|---|---|
| **HUMAN** | fc0babb9 | Operator | Created the project, made every policy change, and made all 140 non-agent events |
| **A-c92e** | c92e5d03 | Windows workstation "MINIAIR" (`C:/src/...`) | 3,302 events, 56 sessions (`instructions.acknowledged`) |
| **A-dd3f** | dd3fcaa5 | Linux ARM64 host "oracle-1" (`/home/ubuntu/...`) | 1,903 events, 29 sessions |
| **A-2508** | 250802da | Linux host (`~/...`) | 808 events, 22 sessions |

Subagents are not separate principals. They are registered "subagent identities" under the parent's credential, for example `navigation-review`, `either-deployment-reviewer`, `worktree-cleanup-implementer` and `continuation-evaluator`.

**Policy history** (`policy_history.json`; every change was made by HUMAN):

| Rev | Time | Change | Provenance text |
|---|---|---|---|
| 1 | 09-14 20:31 | review=agent, recovery=agent, auto-integration=true, lease 600 s | – |
| 2 | 09-14 20:34 | review=**human** | "Initial testing" |
| 3 | 09-15 02:57 | review=agent | "I want agents to continue work on the project without my intervention." |
| 4 | 09-15 14:50 | review=**either**, allow_subagent_reviews=true | – |
| 5 | 09-15 20:27 | lease 600 → **3600 s** | "Enable longer running tasks" |
| 6 | 09-23 15:33 | agent_rule_editing=true | "Enhance the ability of agents to work autonomously without human intervention." |

The required-check roster (workflow policy) had three revisions:

- **Rev 1** required 4 checks, including `native-windows-tests` on `windows-x86_64-msvc`.
- **Rev 2** (09-23 15:35, HUMAN) removed `native-windows-tests`.
- **Rev 3** (09-23 18:08, HUMAN) set the environment for all three remaining checks to `any`.

**Event kinds and frequencies** (all 6,153 events): job.observed 3,081; attempt.renewed 1,084; job.registered 360; checkout.registered 172; attempt.checkpointed 169; reservation.created 154; reservation.released 150; workflow_activity.claimed 146; attempt.claimed 143; instructions.acknowledged 107; submission.created 89; review.decided 75; task.created 53; integration.publication_intended 48; attempt.released 47; task.archived 45; integration.result_recorded 39; integration.completed 39; artifact.upload_reserved 28; artifact.upload_finalized 27; **submission.reopened 26**; workflow_activity.released 16; task.edited 13; **integration.publication_reconciled 9**; task.unblocked 6; knowledge.created 6; recovery.resolved 6; policy.updated 5; reservation.resolved 4; workflow_policy.updated 3; project.created 1; artifact.deleted 1; task.canceled 1.

About 67% of all events (job.observed plus attempt.renewed) are liveness telemetry, not work.

**HUMAN's 140 events by kind:**

| Kind | Count | Notes |
|---|---|---|
| task.archived | 45 | Bulk housekeeping on 09-25 03:25–03:27 |
| submission.reopened | 26 | **Every reopen in the project was done by the human** |
| task.created | 18 | |
| task.edited | 10 | |
| integration.publication_reconciled | 9 | **All 9** |
| workflow_activity.claimed | 8 | |
| task.unblocked | 6 | **All 6** |
| policy.updated | 5 | |
| review.decided | 4 | |
| reservation.resolved | 4 | **All 4** |
| workflow_policy.updated | 3 | |
| task.canceled | 1 | |
| project.created | 1 | |

Setting aside the 45 archives, task creation and policy setup, there were **49 "unblock-the-machine" operations**: reopens, reconciliations, unblocks, reservation resolutions, and review claims and decisions.

**Human interventions by phase:**

| Phase | Interventions |
|---|---|
| Through 09-15 | 15 reopens, 8 review claims, 4 review decisions, 1 reconciliation, 1 reservation resolve, 1 unblock |
| 09-16 → 09-23 15:33 (policy rev 5: either review, subagents, agent recovery, auto-integration) | 8 reopens, 8 reconciliations, 3 reservation resolves, 3 unblocks, 7 edits |
| After policy rev 6 (full autonomy flags, 09-23 15:33 → snapshot) | 3 reopens, 2 unblocks, 2 roster edits, 1 cancel, 1 sentinel edit, plus the 45 archives |

After rev 6 there were zero reconciliations. That coincides with the integration of 3590591f on 09-23 22:19 (see §3).

---

## 1. Knowledge entries and their histories

There are six entries. **Each has exactly one revision** (revision=1), none was superseded, and every entry shows feedback `useful=0 / not_useful=0`. So there was no revision history to mine, and **no agent ever rated or re-used an entry** through the feedback mechanism. Five entries are `checkpoint` kind and one is a `lesson`. All were created by agents, none by HUMAN.

1. **f1dcc43c "Integration queue blocked by stale project policy on 2026-09-15"**
   - Checkpoint by A-c92e, 09-15 08:22, status observed.
   - Three human-approved subjects were sitting in waiting_integration: 4ca4d064, ebd86130 and 627d9d02. Each pinned policy rev 2 while the project was on rev 3, because HUMAN had flipped review_mode from human to agent.
   - "The documented workflow/reopen endpoint requires a human actor; operator must reopen these stale candidates…"
   - It also recorded that `allow_subagent_reviews=false` prevented the parent's subagent from reviewing, and advised: "Configure intended reviewer policy before resubmission to avoid immediately staling new submissions."
   - No external GitHub evidence counted as a coordinator receipt.
   - This is the first explicit record of the **policy-change → human-reopen trap**.

2. **f1f2948e "Six pending agent reviews require an eligible independent reviewer"**
   - Checkpoint by A-c92e, 09-15 08:22.
   - A user-requested review subagent found six subjects (16b23320, ac2f9c1f, 7e0c58b5, a46c7941, 45433a0b, 491e36e0) waiting on agent_review. All were contributed by the same parent principal, so the subagent was ineligible under `allow_subagent_reviews=false`.
   - Its advice: "a human can enable registered non-contributing subagent reviews; since this changes policy, existing submissions then require human reopening". That is exactly what happened at 14:50–15:15, when 8 reopens were made in 25 minutes.

3. **c12fb8d1 "Completed-task worktree and branch cleanup survey on MINIAIR"**
   - Checkpoint by A-c92e, 09-16 03:30, status validated.
   - It surveyed the done tasks and safely removed the worktrees and branches for 08cf9f41.
   - It also recorded that integration of 5027ca0 (91f8f7aa) was **blocked because the claim was rejected as `workflow_policy_changed`**. The submission pinned rev 4, but rev 5 changed only lease_seconds. A human reopen followed at 03:32.

4. **4aae33ac "Branch-cleanup guidance published and its task refs safely retired"**
   - Checkpoint by A-c92e, 09-16 04:35, status validated.
   - Audit trail for 91f8f7aa: review bf8a43ab, integration fadc7a08, four check receipts, and non-forced cleanup with expected-OID leases.
   - A success record with no autonomy problem.

5. **bdd9c04f "Deployment evidence task completed and local cleanup verified"**
   - Checkpoint by A-dd3f, 09-17 12:10.
   - dbbc3260 was done and its worktrees and refs were cleaned. The dependent MINIAIR task 6d81e4f4 became ready.
   - It notes that "remote-host cleanup remains unverified", so cross-host cleanup gaps remain.

6. **bf576b53 "candidate_remote_mismatch from an outdated 0.1.0 CLI"**
   - Lesson by A-dd3f, 09-24 04:13, tags client-compatibility and arm64.
   - The service required candidate_remote/candidate_ref (added in 1f340349, deployed at d928e4b). Old binaries, and even a fresh build of origin/main (8bf489f), omitted them. The result was a misleading `candidate_remote_mismatch`.
   - Both binaries report version 0.1.0.
   - The official Linux package is x86-64, so it could not run on the ARM64 host. The agent rebuilt the CLI from d928e4b and swapped `/usr/local/bin/agent-coordinator`.
   - This led directly to task ad0d9d35.

**Takeaway:** the knowledge store holds good root-cause writing, but it is write-once. Nothing links entries to the tasks they spawned. Entries 1–3 describe the same structural defect: policy or requirement changes strand immutable submissions, and only a human can reopen them. That defect persisted for 10 days without being turned into a fix task. §3 covers it.

---

## 2. Task-by-task timeline (compact)

Column key:

- **SA** = subject work attempts
- **Exp** = expired attempts (subject and activity)
- **Sub** = submissions
- **CR** = changes_requested reviews
- **IA** = integration activities (claims, including canceled ones)
- **→claim** = hours from creation to first claim
- **sub→done** = hours from first submission to integration completed (or final approval for general tasks)
- **H** = human service actions on the task, excluding archive

Suffixes on human actions: reo = submission reopened, rec = publication reconciled, unb = unblocked, rsv = reservation resolved, rev = review claimed/decided, ed = edited, cr = created, cxl = canceled.

| id | kind | title (short) | SA | Exp | Sub | CR | IA | →claim h | sub→done h | H |
|---|---|---|---|---|---|---|---|---|---|---|
| d3fb99b2 | code | Setup field tooltips | 2 | 0 | 1 | 0 | 1 | 3.9 | 0.0 | cr |
| afb073cf | code | Derive repository identity | 4 | 0 | 1 | 0 | 1 | 2.1 | 0.1 | cr |
| 4b68f1c7 | code | Copy-token clipboard fix | 9 | 0 | 4 | 1 | **10** | 2.7 | **111.4** | cr, reo×2, rec×2 |
| 4ca4d064 | code | Portable startup guidance | 4 | 3 | 2 | 0 | 3 | 0 | 100.4 | rev(claim×3, decide), reo, rsv, rec |
| ebd86130 | code | MCP-first bootstrap | 3 | 2 | 2 | 0 | 2 | 0 | 65.1 | rev(claim×3, decide), reo |
| 2f756c67 | code | Durable MCP retry journals | 1 | 0 | 1 | 0 | 1 | 51.9 | 0.1 | – |
| 40458fb6 | code | Help Documentation link | 1 | 0 | 1 | 0 | 1 | 53.7 | 0.0 | cr |
| 627d9d02 | code | Saved blockers / human review UI | 6 | 0 | 4 | 0 | **8** | 0 | **195.7** | unb, rev, reo×3, rec, rsv |
| 80f2625a | code | Credential file download | 2 | 1 | 1 | 0 | 1 | 53.3 | 0.0 | cr |
| 3b1ba4c3 | code | Binding .toml download | 2 | 1 | 1 | 0 | 1 | 59.0 | 0.9 | cr |
| d327fd4c | gen | Deploy blocker guidance fix | 4 | 0 | 2 | 1 | 0 | 0 | never | reo, **cxl** (09-25) |
| 577ed262 | gen | Resources page text (wrong kind) | 2 | 0 | 1 | 0 | 0 | 59.4 | – | cr, ed, unb |
| 2f6d4f3b | code | Task queue pagination | 1 | 0 | 1 | 0 | 4 | 58.4 | 64.1 | cr, rec |
| 59b71105 | code | Task attachments | 5 | 1 | 1 | 0 | 1 | 59.1 | 1.5 (219 h total) | cr |
| c2728e4d | code | Project cards open workspace | 6 | 0 | 2 | 0 | 2 | 0 | 49.9 | reo |
| cb5458cb | gen | Deploy settings/task views | 3 | 0 | 2 | 0 | 0 | 0.1 | **212.8** | reo |
| 7e0c58b5 | code | Continue claiming after each task | 3 | 0 | 2 | 0 | 2 | 0 | 50.2 | reo |
| b30fe06c | code | Binding-rules help text | 3 | 0 | 2 | 1 | 2 | 58.0 | 5.6 | cr, unb |
| a46c7941 | code | Review-first selection | 2 | 0 | 2 | 0 | 2 | 0 | 50.0 | reo |
| 45433a0b | gen | Deploy review-first guidance | 2 | 0 | 2 | 0 | 0 | 0 | 111.1 | reo |
| 16b23320 | code | Project credentials + subagent reviews | 2 | 0 | 2 | 0 | 2 | 0 | 11.9 | reo |
| ac2f9c1f | code | Credential path docs | 2 | 0 | 2 | 0 | 2 | 0 | 48.0 | reo |
| 94017b84 | code | Task create date | 1 | 0 | 1 | 0 | 1 | 55.7 | 0.2 | cr |
| 491e36e0 | gen | Deploy credentials/subagent identity | 2 | 0 | 2 | 0 | 0 | 0 | 48.5 | reo |
| 841ca676 | code | Either agent OR human review | 3 | 1 | 2 | 0 | 2 | 0 | 2.6 | reo, rev(claim, decide) |
| 183b1220 | gen | Deploy either-review | 2 | 0 | 2 | 0 | 0 | 0 | 0.3 | reo |
| 08cf9f41 | code | Worktree cleanup guidance | 2 | 0 | 2 | 0 | 4 | 0.1 | 11.8 | rec, rsv, reo |
| bafdbac6 | code | Copy task ids in queue | 2 | 0 | 2 | 0 | 3 | 45.4 | **123.1** | cr, reo |
| c0d9c313 | code | Copyable id in task detail | 2 | 0 | 2 | 1 | 2 | 25.6 | 3.0 | cr |
| dc1ff9c7 | gen | Deploy cleanup guidance | 3 | 0 | 1 | 0 | 0 | 0 | (105.8 total) | ed×2, unb |
| 91f8f7aa | code | Branch cleanup guidance | 2 | 0 | 2 | 0 | 2 | 0 | 12.6 | reo |
| 5c85ebb2 | code | Delegated task-definition editing | 7 | 1 | 2 | 0 | 3 | 45.6 | 70.7 | rec, reo |
| 8ac23e07 | code | CLI session state (Windows sandbox) | 3 | 0 | 2 | 1 | 2 | 8.9 | 5.2 | cr |
| dbbc3260 | code | Deployment reports as artifacts | 1 | 0 | 1 | 0 | 1 | 0.6 | 0.1 | – |
| 6d81e4f4 | gen | MINIAIR report backfill | 2 | 0 | 2 | 1 | 0 | **148.6** | 0.3 | – |
| 1ef9a91c | code | Consolidate account menu | 2 | 0 | 2 | 0 | 3 | 0.8 | 61.5 | cr, reo |
| 72b1133c | code | Subagents auto-review guidance | 3 | 0 | 1 | 0 | 1 | 1.1 | 0.2 | cr |
| 51d0d4e4 | code | Resources page text (replacement) | 1 | 2 | 1 | 0 | 3 | 1.2 | 51.4 | rec×2, rsv |
| 38f3ffe4 | code | Cancel/delete/archive tasks | **9** | 2 | 2 | 0 | 2 | 1.9 | 8.6 (160 h total) | ed, reo, unb |
| f27bcc16 | gen | DISPOSABLE EVAL subject | 1 | 0 | 1 | 0 | 0 | 0 | 0.0 | – |
| 5655e94d | gen | DISPOSABLE EVAL sentinel (still open) | 3 | 0 | 0 | 0 | 0 | 0.1 | – | ed |
| 456daf61 | code | Separate Done view (P0) | 2 | 0 | 2 | 1 | 2 | **143.8** | 0.5 | cr, ed×2 |
| 3590591f | code | **Agent publication reconciliation** (P0) | 1 | 0 | 1 | 0 | 1 | **112.6** | 1.0 | ed |
| c30653e9 | code | In-app browser redirect loop | 3 | 0 | 3 | 1 | 3 | 99.7 | 5.4 | ed×2, reo |
| b2b20364 | code | Durable remote candidate checkpoints | 1 | 0 | 1 | 0 | 1 | 93.0 | 2.6 | – |
| ac4c4f9b | code | Blocker inspection + state wait (P0) | 3 | 0 | 3 | **2** | 3 | 40.5 | 1.3 | – |
| 0d93af40 | gen | Example Done task | 1 | 0 | 1 | 0 | 0 | 0 | 0.0 | – |
| 15f156e8 | gen | Deploy latest main binaries | 2 | 0 | 1 | 0 | 0 | 0 | 0.1 | – |
| fd3df7bf | code | Items-per-page dropdown | 4 | 0 | 3 | 1 | **7** | 6.1 | 5.3 | cr, reo |
| ad0d9d35 | code | CLI compatibility contract | 2 | 0 | 2 | 1 | 2 | 0.1 | 3.2 | – |
| 83dc5ba8 | gen | Deploy latest validated main | 1 | 0 | 1 | 0 | 0 | 0.4 | 0.0 | – |
| 1bf380ad | code | Release archive allowlist | 1 | 0 | 1 | 0 | 1 | 0 | 0.1 | – |
| dea68719 | gen | Deploy archived pagination (open, waiting_review) | 2 | 0 | 1 | 0 | 0 | 0 | – | unb |

**Aggregates:**

- **39 code tasks done.** Median lifetime was 56 h. The median from first submission to done was 5.3 h, with a maximum of 195.7 h (627d9d02).
- **Attempts:** 289 in total. The 143 subject attempts ended 89 submitted, 40 released, 7 blocked and 7 expired. The 146 activity attempts ended 114 submitted, 16 released, 9 canceled and 7 expired.
- **Integration activities:** 95 in total, **56 canceled** against 39 completed. That is 1.4 wasted integration claims for every successful one.
- **Jobs:** 360 in total, with **131 failed** (36%) and 3 not_started. For registered required-check jobs:

| Check | Succeeded | Failed |
|---|---|---|
| native-windows-tests | 33 | 27 |
| linux-validation | 46 | 35 |

- **Submissions:** 89, of which 26 were reopened by HUMAN and 13 received changes_requested.

---

## 3. Autonomy breakdown catalog

### B1. Policy or roster change makes every in-flight submission stale, and only a human can reopen it

- **Frequency:** 16 of the 26 human reopens were purely policy-revision staleness:

| When (09-15/16) | Policy change | Reopens | Tasks |
|---|---|---|---|
| 03:10–03:12 | rev 2→3 | 3 | cb5458cb, d327fd4c, c2728e4d |
| 08:26–08:27 | rev 2→3, after human approval | 3 | 4ca4d064, ebd86130, 627d9d02 |
| 14:54–15:15 | rev 3→4 | 8 in 21 min | 16b23320, 183b1220, 45433a0b, 491e36e0, 7e0c58b5, 841ca676, a46c7941, ac2f9c1f |
| 20:30 and 09-16 03:32 | rev 4→5, **lease duration only** | 2 | 08cf9f41, 91f8f7aa |

- **Evidence:** knowledge entries f1dcc43c and c12fb8d1. Attempt af901371 on 7e0c58b5: "Submission refused because project/task policy changed during validation." Orientation step 7 in the snapshot still says "If requirements changed, **ask an operator** to inspect and reopen the obsolete candidate."
- **Cost:** 7e0c58b5, a46c7941, ac2f9c1f and 45433a0b sat about 48–111 h between their first and final submissions with no source changes. Agents then invented a "revalidate unchanged candidate" workaround: they resubmitted the current main commit that contained the original change as an ancestor.
- **Root cause:** submissions pin `project_policy_revision` and `workflow_policy_revision`. Any change, including irrelevant ones like `lease_seconds`, invalidates them, and `workflow/reopen` requires a human actor. The four human-approved submissions from 09-15 01:47–01:58 were thrown away by the human's own policy change an hour later.

### B2. A stale or conflicting candidate cannot be rebased by agents without a human reopen

- **Frequency:** 5 reopens came from merge conflicts: 627d9d02 (twice), 4b68f1c7, bafdbac6 and fd3df7bf. Several other cases had an integration claimed and abandoned first. Two more reopens needed a fix inside the candidate: 4b68f1c7 (README link) and 5c85ebb2 (fixtures broke in the integrated result).
- **Worst case, fd3df7bf.** The approved candidate 69c9f0db conflicted in `scripts/test_task_detail_copy_controls.mjs`. **Four separate integration claims** (06:01 A-dd3f, 08:04 A-dd3f, 08:30 A-2508, 09:01 A-2508) each rediscovered the same conflict, logged "requires an operator to reopen", and released. That burned about 3 h until HUMAN reopened at 09:08. The rebased fix then went through review and integration in 16 minutes.
- **627d9d02:** 8 integration activities over 196 h, including two conflict cycles on the same file.
- **Hotspot:** one shared browser fixture file, `scripts/test_task_detail_copy_controls.mjs`, is the named conflict in 4b68f1c7, 627d9d02 (×2), fd3df7bf and 38f3ffe4.
- **Root cause:** immutable-candidate semantics, plus a human-only reopen, plus no agent "supersede/rebase my own stale submission" path. Parallel UI tasks all edit one test harness.
- **Note:** ac4c4f9b was created specifically because agents learned about the reopen gate only by failing. Its precondition-inspection endpoint now surfaces "requires operator reopen", but it still does not remove the human.

### B3. Integration lease expired after publication intent, and only a human could reconcile

- **Frequency:** 9 reconciliations (6 not_published, 2 target_moved, 1 published) and 4 human reservation.resolved, all between 09-15 and 09-23.
- **4ca4d064:** the integration was published to main at 09-17 19:24, but the A-dd3f activity lease expired before the result was recorded. It sat **31.5 h** until HUMAN reconciled it as "published" on 09-19 04:06 and released reservation 740a5778.
- **08cf9f41:** the 10-minute lease expired after four successful checks and before publishing. That needed a human reconciliation, a reservation resolve, a policy change to 3600 s, and then a reopen, because the lease change itself staled the submission (B1). It is **four human actions caused by one lease timeout**.
- **51d0d4e4:** the activity expired and sat 13.8 h. The agent reported "Native integrations reconcile repeatedly failed with 'read integration intent'". It then needed a second human target_moved reconciliation.
- **4b68f1c7:** A-2508 recorded two publication intents on Linux and then could not run the Windows-only check. HUMAN reconciled both as not_published, with the note "Releasing this attempt so the Windows workstation can claim fresh."
- **Root cause:** after `publication_intended` an agent cannot release or abandon. Before 09-23 only humans could reconcile.
- **The fix worked:** A-dd3f created **3590591f "Allow evidence-gated agent reconciliation of interrupted publications"** (P0) on 09-19 at 04:09, three minutes after doing the 4ca4d064 reconciliation. It was not claimed until 09-23 20:45, **112.6 h later**, and was integrated at 22:19. The data shows **zero human reconciliations after that**. Its own acceptance criteria still keep "target moved, ambiguity, missing journal → human-gated".

### B4. Human-only resource creation and canonical-resource gaps

Agents can list and reserve resources but not create them. This caused four blocked attempts:

| Task | When | Blocker |
|---|---|---|
| 627d9d02 | 09-15 01:03 | "A human administrator must configure a named validation resource". HUMAN created miniair-validation and unblocked at 01:11. |
| c2728e4d | 09-17 03:22 | No Linux runner resource. "Asked operator to define one", and the operator supplied oracle-1. |
| dea68719 | 09-25 04:10–04:57 | "No canonical production-host resource… operator must create". HUMAN unblocked at 05:08. |
| dc1ff9c7 | 09-19 20:07 | Blocked because miniair-validation (capacity 1) was held by another task. HUMAN unblocked at 09-20 00:58. |

A-dd3f's 72b1133c and 4ca4d064 also stalled on "oracle-1 capacity remains held by recovery-required attempt 9d07f647" (59b71105's expired attempt).

**Root cause:** resource CRUD is human-only, and capacity-1 resources become global chokepoints when a holder expires.

### B5. Cross-workstation state: unpublished candidates and workstation-local worktrees

- **2f6d4f3b** and **1ef9a91c** (A-2508 on 09-17): the approved candidates bcf97676 and 77a344ec were never pushed. A-dd3f (09-19 04:13) and A-c92e (09-20 03:26) could not integrate them: "candidate … is absent from this verified checkout and no recorded matching candidate branch is available from origin." 1ef9a91c needed a human reopen, and 2f6d4f3b needed a target_moved reconciliation.
- **59b71105:** the expired attempt's commit 29b43248 existed only in `/tmp` on A-dd3f. A-2508 released it with "Another workstation must recover the exact saved source." It sat idle until 09-24, and the eventual implementation was new.
- **38f3ffe4 (worst):** 9 subject attempts across 3 workstations.
  - The A-2508 commit 44a0171 was lost (expired 09-17, stuck 32 h).
  - The A-c92e Windows candidate 1ddac715 was never pushed.
  - A-dd3f rebuilt it (6d03f1fd).
  - HUMAN reopened it because of a legacy checkpoint (B8).
  - A-c92e expired again on 09-24.
  - A-dd3f's recovery was blocked: "exact latest checkout is registered on workstation MINIAIR… cannot be inspected from this Linux workstation".
  - HUMAN unblocked it, and A-2508 finished at 08:56.
- **Fix task:** b2b20364 "Require durable remote candidate checkpoints before code submission" (created 09-20 by A-2508, idle 93 h, done 09-24).
- **Remaining gap:** recovery of uncommitted or unpushed work is still workstation-bound.

### B6. Platform-split required checks (Windows vs Linux) and toolchain fragility

- Roster rev 1 required `native-windows-tests` on windows-x86_64-msvc and three Linux checks. So every integration needed both a Windows and a Linux producer.
- Failed native-Windows attempts include:
  - 2f6d4f3b: "Required roster entry native-windows-tests… unavailable on this Linux workstation", and publication_allowed=false.
  - 4b68f1c7: two workstations coordinating "over an external Claude-to-Claude messaging channel".
  - MSVC-linker failures: `link.exe` resolved to coreutils in 16b23320, ebd86130, 08cf9f41, bafdbac6 (×4), 38f3ffe4 (×5) and 1ef9a91c.
- WSL path and CRLF failures hit 08cf9f41 (4 retries) and 16b23320 (4 retries). Missing Python hit 38f3ffe4. A Windows-incompatible `smoke.py` (ACL-rejected temp `AGENT_COORDINATOR_HOME`) also hit 38f3ffe4. /tmp tmpfs quota exhaustion hit c2728e4d and 72b1133c.
- **131 of 360 jobs failed (36%).** native-windows-tests failed 27 times against 33 successes.
- **Human resolution:** HUMAN removed the Windows check (09-23 15:35) and set environment=`any` (18:08). After that, integrations use 3 checks and complete in minutes (1bf380ad in 3 min, ad0d9d35 in 19 min).
- **Root cause:** the required-check roster encodes a heterogeneous host requirement, with no scheduling or routing to a capable host. Workstation environments are not reproducible.

### B7. Agent-harness permission gates, not coordinator policy

These show autonomy breaking at the agent runtime:

- **5c85ebb2 (A-2508, 09-17 and 09-20):** "External Git push is awaiting explicit user authorization". Separately: "External coordinator submission requires explicit user authorization". The same blockers were recorded in 3 checkpoints on 09-20 15:29–15:42.
- **51d0d4e4 (A-2508):** "Publishing and candidate submission require user authorization for this new task."
- **841ca676 (A-c92e):** "Automatic approval review rejected production deployment and detailed report upload."
- **4b68f1c7 (A-c92e, 09-22 03:23):** "`integrations prepare` and the plain check-tagged `jobs run` were blocked by this harness's own auto-mode safety classifier (Merge-Without-Review guard), independent of coordinator policy." Native Windows tests were then run outside the coordinator, so they gave no receipts.
- **5c85ebb2:** "Official MCP-client loopback tests are environment-blocked by sandbox socket permission."
- **80f2625a:** "Lease expired during conversation pause". The user-facing session stopped mid-task, and the lease lapsed.

**Root cause:** the coordinator policy grants authority that the host harness does not recognize. Push, network submission and "merge without review" guards all require a human click.

### B8. Service/client contract changes strand in-flight work

- **b2b20364** made candidate_remote/candidate_ref mandatory. The upgraded service (about 09-24 02:00) left legacy submissions with a null ref: c30653e9's 9873d7fd (00:39) and 38f3ffe4's e95ec886 (00:19). Both were **reopened by HUMAN** at 02:57 and 03:08.
- Old 0.1.0 CLIs then failed with `candidate_remote_mismatch` on fd3df7bf (03:12) and 59b71105 (03:48). That cost two lost attempts and a CLI rebuild (knowledge bf576b53).
- b2b20364 itself could not submit until production was upgraded: "live coordinator rejected the submission… request-schema mismatch". The fix needed "**User authorized upgrading the configured production coordinator**".
- Follow-up **ad0d9d35** "Advertise CLI compatibility requirements" (done 09-24 08:30).
- **Root cause:** a self-hosting chicken-and-egg problem. The service under development is also the coordinator, so schema changes need a deployment, the deployment needs human authorization, and the version string does not identify the build.

### B9. Human-in-the-loop acceptance criteria and human-verifiable UX

Several human-written criteria required human action:

- **b30fe06c:** "Human reads the explanatory text and understands…"
  - A-c92e returned changes_requested because the evidence lacked a human reading.
  - A-2508 blocked on it (09-17 14:40).
  - HUMAN unblocked it at 18:02. The only evidence is a human confirmation made in chat.
- **4b68f1c7:** "verify by pasting into an independent application".
  - Rejected once (09-17 15:37) because the harness exposed only Brave.
  - Passed only after "a **user manual Ctrl+V** into a separate Chrome window".
- **c30653e9:** "fresh Codex in-app browser session". A-2508's own reviewer returned changes_requested because the in-app browser could not be exercised headlessly.
- **51d0d4e4** AC5 ("A human reviewer can view the page and easily understand…") was approved by an agent reviewer. That is inconsistent with b30fe06c, where the same kind of criterion blocked.

### B10. Wrong task shape: wrong kind, pinned obsolete scope, or user-expanded scope

- **577ed262** was a "general" task asking for a code change. The agent blocked because the task kind has no code integration path. A human edit and unblock were needed, plus a replacement code task, 51d0d4e4.
- **d327fd4c** pinned candidate d511a92 for deployment.
  - By 09-24 production served its descendant 1f34034.
  - An agent submission falsely claimed "remains deployed", and the independent agent reviewer caught it with changes_requested.
  - The agent then blocked for "a project/task owner" to decide scope, and HUMAN **canceled** it on 09-25.
  - Although agent_rule_editing was on, the agent did not rescope or cancel it itself.
- **c2728e4d:** "agent attempt to revise criteria was refused by service" (09-15, rev 2 had agent_rule_editing=false).
- **dc1ff9c7:** "User expanded cleanup scope", and HUMAN edited the task twice.
- **15f156e8** is a counterexample. After rev 6 the agent edited its own task scope (task.edited by A-dd3f at 20:09) following the user's clarification. That is the only clear use of agent_rule_editing.

### B11. Recovery of expired work was slow

There were **14 expired attempts**:

| Holder | Count | Stuck after expiry |
|---|---|---|
| HUMAN review claims (10-min lease) | 4 | 2–39 min |
| Agent subject work | 7 | 43 min to **32 h** (38f3ffe4) |
| Agent integration activity | 3 | 7 min to **31.5 h** (4ca4d064) |

- Recovery itself was done by agents, all 6 recovery.resolved: A-c92e 3, A-dd3f 2, A-2508 1. **recovery_mode=agent worked as a mechanism.**
- The latency comes from **no agent being alive**. The largest gaps with zero agent events were 32.3 h (09-17 19:28 → 09-19 03:45), 23.7 h (09-21 → 09-22) and 18.2 h (09-24 09:55 → 09-25 04:10). There were no agent events at all on 09-18, and only 4 on 09-21.
- 72b1133c recorded the fact: "the service cannot wake an agent after its host turn ends."
- **Root cause:** pull-only coordination with session-scoped agents and no scheduler or supervisor.

### B12. Queue selection latency and priority inversion

- P0 tasks sat unclaimed:
  - 456daf61 (Done view): 143.8 h.
  - 3590591f (agent reconciliation): 112.6 h.
  - ac4c4f9b (blocker inspection): 40.5 h.
- b2b20364, which prevents B5, waited 93 h.
- Meanwhile agents repeatedly claimed and released tasks for "bounded startup evaluation". afb073cf had 3 such attempts, 4b68f1c7 had 2 and d3fb99b2 had 1, all released with no work.
- The disposable sentinel 5655e94d is still `ready` in the queue. It was claimed 3 times and edited by HUMAN.
- **Root cause:** a mix of session gaps (B11) and agents working on user-directed scope in chat rather than the queue order.

### B13. Review independence is nominal, and reviews are fast

- **60 of 71 agent review decisions** were by the **same principal** as the submitter, through registered subagent identities: dd3f 28, c92e 19, 2508 13. Only 11 crossed principals.
- The median review latency was 3.9 min. **44 of 75** decisions came within 5 min, for example 40458fb6 in 1 min and 0d93af40 in 1 min.
- The rejections were real, though: 12 agent changes_requested caught substantive issues, including a false deployment claim (d327fd4c), a missing human paste (4b68f1c7), and a job-wait token bug (ac4c4f9b).
- **Risk:** reviewer quality depends on the parent's discipline. There is no enforced separation of context.

### B14. Work done outside the coordinator

- The local git log shows main commits after the last coordinator integration (a331634 at 09-24 09:35). These are a89fbc3, 0c671cc, 0d62114, d52d1d9, e96b11a, 3bff21a and d72a8cb, with no matching tasks. dea68719 says "Committed and pushed e96b11a", so it was a direct push.
- On 09-21, 9bb9a28 ("Remove redundant CLAUDE.md…") was pushed directly. It broke the README link and failed the `documentation` required check "for every integration on this project" (A-2508, 4b68f1c7). It also broke release packaging (15f156e8, b2b20364: "package_release.py still expects removed CLAUDE.md").
- Direct pushes also produced target_moved conflicts: 2f6d4f3b (ce64361) and 51d0d4e4.
- **Root cause:** there is no enforcement that main only changes through coordinator integration. Human or interactive sessions bypass it and invalidate in-flight integrations.

### B15. In-chat human decisions not captured in the service

Checkpoints cite user decisions made outside the service:

- "User selected standalone MCP adapter" (2f756c67)
- "User clarified…" (c2728e4d ×2, 15f156e8)
- "User explicitly authorized deployment" (cb5458cb, 45433a0b, 491e36e0, 183b1220, 841ca676)
- "User split historical backfill" (dbbc3260)
- "User authorized upgrading production" (b2b20364)
- "user-authorized latest-version deployment" (fd3df7bf, 83dc5ba8)

**Every production deployment in the data was preceded by explicit user authorization in chat.** There is no service policy for deployment autonomy. decisions.json is empty, so the service's decision mechanism was never used for any of this.

---

## 4. Human-intervention inventory (services actions plus chat)

**A. Review work, 09-15 only**

| Action | Tasks |
|---|---|
| 8 human claims | 4ca4d064 ×3, ebd86130 ×3, 627d9d02, 841ca676 |
| 4 decisions | 627d9d02, 4ca4d064, ebd86130, 841ca676 (the last was a feature test: "If this dialog changes the status correctly…") |

- 4 of the 8 claims expired, because the 10-minute lease was shorter than human review time.
- 3 human-approved submissions were then discarded by the rev 3 change.
- After 09-15, **no human reviews**. All 71 later decisions came from agents.

**B. Submission reopens (26), by cause**

| Cause | Count | Tasks |
|---|---|---|
| Policy staleness | 16 | See B1 |
| Merge conflict | 5 | 627d9d02 ×2, 4b68f1c7, bafdbac6, fd3df7bf |
| Candidate fix needed after review | 2 | 4b68f1c7 README, 5c85ebb2 fixtures |
| Unpublished candidate | 1 | 1ef9a91c |
| Legacy null checkpoint after upgrade | 2 | c30653e9, 38f3ffe4 |

**C. Publication reconciliations (9)**

| Task | Time | Disposition | Cause |
|---|---|---|---|
| 08cf9f41 | 09-15 20:16 | not_published | Lease expiry |
| 4ca4d064 | 09-19 04:06 | published | Lease expired after push |
| 51d0d4e4 | 09-19 19:25 | not_published | |
| 51d0d4e4 | 20:39 | target_moved | |
| 2f6d4f3b | 09-20 03:22 | target_moved | Direct docs commit ce64361 |
| 5c85ebb2 | 09-20 14:14 | not_published | Check failure after intent |
| 4b68f1c7 | 09-22 05:35 | not_published | Cross-workstation check split |
| 4b68f1c7 | 06:03 | not_published | Cross-workstation check split |
| 627d9d02 | 09-23 04:02 | not_published | |

**D. Reservation resolves (4)**

| Task | Time | Reason |
|---|---|---|
| 08cf9f41 | 09-15 20:22 | Stale after expired lease |
| 4ca4d064 | 09-19 04:02 | Stale after expired lease |
| 51d0d4e4 | 09-19 20:32 | Stale after expired lease |
| 627d9d02 | 09-23 04:10 | Stale after expired lease |

**E. Unblocks (6)**

| Task | Time | Reason |
|---|---|---|
| 627d9d02 | 09-15 | Resource creation |
| b30fe06c | 09-17 | Human comprehension |
| 577ed262 | 09-17 | Wrong kind |
| dc1ff9c7 | 09-20 | Resource contention |
| 38f3ffe4 | 09-24 | Cross-workstation recovery |
| dea68719 | 09-25 | Production resource creation |

**F. Policy and roster changes (8)**

- 5 policy revisions.
- 3 roster revisions: initial, drop Windows, environment=any.
- Each policy revision before rev 6 triggered reopens.

**G. Task edits and cancels**

- Edits: dc1ff9c7 ×2, 577ed262, 456daf61 ×2, 3590591f, 38f3ffe4, c30653e9 ×2 (a burst of scope tidying on 09-20 17:06–17:11), 5655e94d.
- 1 cancel: d327fd4c, after an agent explicitly asked for owner scope reconciliation.

**H. Housekeeping**

- 45 task archives on 09-25, done manually one by one about a second apart.
- 18 task creations. Agents created 35 tasks themselves: c92e 23, dd3f 8, 2508 4.

**I. Off-ledger (chat)**

- At least 14 authorizations or clarifications recorded in checkpoints (B15).
- The human's manual paste for 4b68f1c7.
- The human comprehension read for b30fe06c.
- Creating the resources miniair-validation, oracle-1 and the production host.
- Direct pushes to main (B14).
- Harness permission prompts for push and submission (B7).

**Integration authorization** was never needed. `authorization` and `authorization_history` are null or empty on all 95 integration activities, so automatic_integration worked. All 39 publish results were reported by agents: dd3f 22, c92e 13, 2508 4.

---

## 5. Workflow-complexity observations

**Per done code task** (39 tasks; mean / median / max):

| Metric | Mean | Median | Max |
|---|---|---|---|
| Events | 144 | 105 | 395 |
| Attempts | 6.3 | 5 | 20 |
| Subject attempts | 2.9 | 2 | 9 |
| Activity attempts | 3.4 | 2 | 11 |
| Checkpoints | 4.7 | 4 | 21 |
| Jobs | 8.8 | 8 | 21 |
| Failed jobs | 3.2 | 2 | 13 |
| Checkouts/worktrees | 4.1 | 3 | 13 |
| Reservations | 3.6 | 2 | 19 |
| Integration activities | 2.4 | 2 | 10 |
| Distinct event kinds | 18 | 18 | 22 |
| Lease renewals | 25 | 17 | 108 |
| Job observations | 77 | 48 | 233 |

- Total registered checkouts: 169.
- General tasks are much lighter: 24.5 events, 3 attempts and 1 job on average.

**The minimum happy path for a code task** is about 15 state transitions under 5–7 distinct leases or identities:

1. Claim.
2. `worktree prepare` (one worktree per attempt: "CLI pins one worktree per attempt").
3. Commit.
4. List resources, then reserve.
5. `jobs run`, with a JSON input file and an absolute program path.
6. Release the reservation.
7. Push the candidate ref and verify it (since b2b20364).
8. `submissions code`, with one evidence entry per criterion.
9. Spawn or register a subagent reviewer, then `reviews claim` and `reviews decide`.
10. `integrations claim`, which takes the global target hold.
11. A fresh integration worktree, then `integrations prepare`.
12. Reserve, then run 3–4 exact-result check jobs sequentially, renewing the lease throughout.
13. Release.
14. `integrations publish`, then `finish`.
15. Fresh read to confirm done, then non-forced worktree and branch cleanup with expected-OID leases.

The served instructions are version 8 and about 11 KB. Step 8 of the completion guidance alone is a paragraph of more than 400 words about cleanup. The MCP catalog grew to 61 tools (ac4c4f9b). Agents frequently spent their first attempt only on orientation, for example afb073cf, whose 3 attempts were "bounded startup inspection". They also used release-then-reclaim just to change worktree or base: 8ac23e07 "releasing only to replace an incorrectly based checkout", and 4b68f1c7 A-2508 "Corrected worktree base".

**Mistakes the workflow induced:**

- Worktree prepared from the candidate instead of the target: 627d9d02 (09-20) and 4b68f1c7 (09-22), each costing an integration claim.
- `inputs_unchanged=false` receipts invalidated by an installer writing into the worktree: 4b68f1c7.
- Tmpfs space: c2728e4d and 72b1133c.
- Source snapshot changed before launch: 4ca4d064 and c2728e4d `not_started` jobs.

**Workarounds agents invented:**

- Registering "Verify exact hosted … evidence" jobs, observer jobs that confirm GitHub Actions results and act as producers (c2728e4d, dbbc3260, 1bf380ad "CI observer").
- Running Linux checks through WSL on Windows.
- Resubmitting "unchanged" candidates as the current-main descendant, to satisfy staleness.
- Fixing an unrelated broken README link inside a feature candidate to unblock a project-wide check.
- A no-mount Docker container as an "independent workstation" (6d81e4f4, after a changes_requested).
- Direct binary builds when packaging broke (15f156e8).
- Rebuilding and replacing the system CLI (bf576b53).
- Disposable eval tasks to prove continuation (f27bcc16 and 5655e94d).
- `git merge-tree` preflight to detect conflicts before claiming.
- An external Claude-to-Claude messaging channel for multi-workstation coordination, with no delivery confirmation. That is tracked as external Claude Code issue #86014.

---

## 6. Top 10 insights for full autonomy

1. **Let agents reopen or supersede stale submissions under policy.** Allow it when the only change is the policy revision, a non-semantic setting (lease), or a target-branch conflict. This single defect, human-only reopen, accounts for 26 human actions. It is also the only recurring human action left after the rev 6 changes: 3 reopens on 09-24. At minimum, stop pinning submissions to changes that do not affect review or check semantics. The lease_seconds change alone cost 2 reopens, 1 reconciliation and 1 reservation resolve.
2. **Provide an agent rebase-and-resubmit path for conflicted candidates**, with a mandatory fresh review, instead of "requires operator reopen". fd3df7bf shows 4 agents × 4 claims × 3 h rediscovering one conflict. Also split the `scripts/test_task_detail_copy_controls.mjs` hotspot into per-feature fixtures.
3. **Keep the agent publication reconciliation that 3590591f introduced, and extend it.** Reconciliations went from 9 to 0 after it landed. Extend it to `target_moved` (the 2 remaining human-gated cases), and let agents resolve stale reservations whose producers are verifiably terminal. That would have removed all 4 reservation.resolved events.
4. **Add a supervisor or wake mechanism.** Recovery was agent-driven but waited up to 32 h for any agent to exist. The service cannot wake agents (72b1133c). A scheduled or looping agent runner, or a webhook-driven launcher, is the difference between "agents may recover" and "work is recovered". The same fix covers idle P0s (3590591f waited 112 h) and the review of dea68719, which is still queued.
5. **Make capability routing explicit.** Required checks should route to hosts that advertise the matching capability (os/arch/toolchain), with pre-provisioned, reproducible check runners, for example containerized Linux checks. The 36% job failure rate and the Windows-check bottleneck were only fixed when the human dropped the Windows check. If Windows coverage matters, run it as a hosted CI observer rather than needing a live Windows agent.
6. **Align harness permissions with coordinator policy.** Pushes, coordinator submissions, `integrations prepare` and deployments were blocked by the agent host's approval gates: Codex approval and Claude Code auto-mode "Merge-Without-Review". Ship harness allowlists and settings that trust coordinator-guarded operations when project policy enables them. Otherwise the coordinator grants authority the runtime will not use.
7. **Let agents manage resources and deployment scope.** Resource creation is human-only, which caused 3 blocks. Every production deploy needed chat authorization. Add policy switches, for example `agent_resource_admin` and `deployment_mode=agent`, plus a canonical production-host resource, so deployment tasks can run end to end.
8. **Enforce "main changes only through integration"**, or auto-reconcile direct pushes. Direct commits (9bb9a28, and the 09-24/25 series) broke the docs check, the packaging, and in-flight integrations (target_moved). A branch-protection rule plus an "adopt external commit" workflow would remove this failure source.
9. **Ship service and client contracts compatibly and guarantee durable cross-host state.** Keep b2b20364 (remote candidate refs) and ad0d9d35 (client compatibility contract). Also migrate legacy submissions automatically instead of stranding them, which caused 2 reopens. Periodically push WIP checkpoints so an expired attempt's work is recoverable from any host (the 38f3ffe4, 59b71105 and 1ef9a91c losses).
10. **Author tasks for agent verifiability, and make the knowledge store active.** Avoid acceptance criteria that need a human's eyes or hands (b30fe06c, 4b68f1c7, c30653e9), or route them to an explicit human-verification queue that does not block integration. Fix wrong-kind and obsolete-scope tasks by letting agents re-kind or cancel them under agent_rule_editing; d327fd4c waited for a human cancel. Record chat decisions in the service's decision mechanism, which is currently empty. Turn knowledge "lessons" into linked fix tasks with feedback: 6 entries, 0 feedback, 0 revisions, and the policy-staleness lesson stayed unfixed for 10 days. Finally, consider real principal separation for reviewers: 60 of 71 agent reviews were same-principal subagents, 44 of them decided within 5 minutes.
