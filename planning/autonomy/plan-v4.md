# Plan v4 — Full agent autonomy for Agent Coordinator projects

Status: **consolidated after discussion rounds 1–4** (`discussion-transcript.md`), including the round-4
pre-mortem. Supersedes v1–v3. Changes since v3 are marked **(v4)**.
Participants: Lead, Dana (devil's advocate), Sol (systems & protocol), Hari (harness & ops).
Evidence: `review/{service-data,codebase,history}.md`.

**Convention:** every design item is tagged **[R-Pn]** (required for correctness in phase *n*) or
**[F]** (follow-up, built only when a measured failure justifies it: the "measured-failure rule").
Every component names what it deletes.

## 0. Goal, threat model, success measure

**Goal.** On a project whose human-owned policy grants agents review, recovery, integration
authorization and rule editing, every task that exists reaches a **terminal state** — `done`
(reviewed, integrated into the target branch, validated on the integrated result) or
`canceled`/`superseded` with a recorded agent rationale — **with no human attention**.

**Threat model** (Dana/Sol): agents are *confidently wrong* or *accidentally weakening*, not
adversarial. Against adversarial or prompt-injected agents the design only bounds blast radius
(separate uids, read-only reviewer credentials, rulesets, no secrets in CI, secret scanning).

**Success measure.**
- A task is *autonomous* iff every mutation on it came from a `class=supervised` credential, the
  supervisor, or the integrator. Interactive-session credentials and dashboard actions are human
  attention. Supervised sessions cannot prompt (`--permission-prompts none`,
  `approval_policy=never`), so chat attention is covered by construction.
- **HRI/task** = human-attention mutations + parked-task notifications per terminal task. Target 0 on
  the costed pilot and across 2 weeks of dogfooding, excluding reserved decisions.
- **(v4) Stalls count**: "queue non-empty with no progress for more than N hours" counts as HRI, so a
  silent stall can never score 0 (Hari pre-mortem).
- **Canaries**: direct human pushes/week; revise ping-pong; claim churn; spend vs budget; review
  latency; **audit-sample disagreement rate** (M5); **(v4) value canary**: human-originated tasks done
  per week and their median time to done (Dana pre-mortem); **(v4) escaped defects**: reverts per week;
  **(v4) flake rate** per required check.

**Reserved (explicit, labelled, never silent):** bootstrap (projects, policy, host enrolment,
harness logins, billing); authoring what to build; constitution changes; coordinator releases that
change guard code or need non-expand migrations (U9); irreversible product decisions; privilege-bearing
CI changes; unfenceable external consoles (DNS/Cloudflare dashboards).

## 1. Diagnosis (see v1 §1 and the review reports)

D1 human-only backward transitions (26 reopens, 6 unblocks) · D2 whole-revision staleness (16 of 26
reopens) · D3 workstation-bound state (9 reconciliations; 38f3ffe4: 9 attempts on 3 hosts) · D4
human-only configuration · D5 no liveness (≥20 "continue"; 32 h agent-silent gaps; P0 waits 40–144 h)
· D6 harness authority ≠ project policy (push/submit approvals, ~700 escalations, classifier blocks) ·
D7 check placement and unreproducible environments (36% job failures) · D8 instruction surface
(~10–11k words before first claim) · D9 bypass via direct pushes (9bb9a28) · D10 self-hosting
contract churn · D11 human-only acceptance criteria.

Reframes: most human clicks were authority, but not all (triage bins a/b/c); LLMs should not do
deterministic safety-critical plumbing; the incremental pattern works (3590591f) — evolve, don't
rewrite.

### Gate triage
| Gate today | Bin | Disposition |
|---|---|---|
| reopen (stale/conflict/failing/legacy) | a | `revise` + scoped pinning (P1); stale class gone with repo rules (P4) |
| unblock | a | agent unblock under `recovery_mode=agent` (P1) |
| publication reconciliation | a | deleted by pinned-R roll-forward (P4) |
| reservation resolve | a/c | check reservations deleted with Actions receipts (P4); exclusive resources keep a human gate until fenced [F] |
| roster edit | b/c | repo roster read from target (P4); protected check ids are constitution |
| resource creation | b | host-advertised capabilities from enrolled hosts; the repo only declares requirements |
| task edit/cancel/archive | a/c | agent cancel/replace with rationale (P1); AC amendments by contributors need explicit reviewer approval; auto-archive [F] |
| human-only acceptance criteria | b/c | Playwright + staging login (P2); `requires_human_acceptance` non-blocking (U10) |
| production deploy | c | reserved (U9); fence gate [F] |
| Windows required check | c | U1 |
| harness approvals / classifier | a | deterministic supervised profile; integrator does all target pushes |
| "continue", idle agents | a | supervisor |
| product questions in chat | a/c | decisions with timeout-to-recommendation for reversible questions (M1) |

## 2. Design

### 2.0 (v4) Attention budget — the headline fix from the pre-mortem
All three round-4 pre-mortems failed the same way: v3 turned noise into work and work into
notifications, the user was flooded, bypassed a guard (ruleset B, or muting notifications), and HRI still
read 0. The fix is one item with five parts:
1. **Admission control for agent-originated work** [R-P3b]. Tasks created by agents start in state
   `proposed` with `origin=agent`; `next` never offers a proposed task. A **human-owned admission
   rule** (service-side, part of the constitution) admits: `revert`, `fix-target`, de-flake,
   fixes for labelled refusals, and up to N agent-originated tasks per week (budget). Everything else
   waits in the digest for one-pass human triage.
2. **One daily digest** [R-P3b]: proposed tasks, M1 decisions pending or taken on timeout, post-hoc
   rule notices, audit-sample disagreements, release packets, credential warnings, spend.
3. **Paging only on SLO breach** [R-P3b]: a **daily end-to-end synthetic canary per vendor and host**
   (claim → launch → edit → push-candidate → review → integrator → Actions → done, on a dedicated
   canary project/repo, low effort, cents per day) plus throughput/queue-age SLOs. Page when the
   canary fails or an SLO breaks; everything else goes to the digest. The canary also records the
   same-SHA flip rate of each required check.
4. **Stalls count as HRI** (§0).
5. **Flaky checks never reach an author or the user** (§2.4 flake attribution).

Also: `next` avoids handing out tasks whose paths overlap files the user shipped in the last 24 h
(file lists only) [R-P3b]; reserved release decisions come with an auto-built **release packet**
(diff summary by area, guard-file diff, staging soak evidence) so the click carries information [F].

### 2.1 Components and deletions
| Component | Deletes |
|---|---|
| Supervisor (per host, ~300–500 lines) | "continue" prompts; lease expiry on turn end; hand-spawned reviewers; relays; worktree-cleanup prose ×3 (M4) |
| Integrator (deterministic, non-LLM) | LLM `integrations prepare/publish/reconcile/finish`; publication intents; reconciliation; journal attestations; global hold |
| Actions receipts (bound) | workstation producers on the required path; check reservations |
| Repo rules + roster | agent `PATCH policy`; `workflow_policies`; whole-revision staleness |
| `next` + per-role contract ≤1.5k words | ~10k words of startup prose (MCP frozen, not rewritten) |

### 2.2 Service protocol (P1 unless noted)
1. **`revise`** [R-P1]: single CAS on current submission + subject generation; closed reason enum with
   evidence and allowed actors — `conflict{T,C}`, `check_failed{receipt on R}` (integrator role;
   in P1 any agent with evidence), `candidate_missing`, `requirements_changed{task_def_rev}`
   (**refused for contributors to the task**), `author_withdraw` (author only). Refused while a
   publication intent is unresolved (existing guard near `workflow.rs:1133`). Rate limit; ping-pong
   ⇒ **serialize first** (make the later task depend on the earlier), park only on a persistent cycle.
2. **Scoped pinning** [R-P1]: pin candidate + task-definition revision + monotone required-review set.
   Satisfaction order `human ≥ either ≥ agent`; an active activity no longer required may finish and
   its approval counts [R-P1]. Roster at push time (P1: current `workflow_policies`; P4: the file in
   T0, which equals push time by construction). Rules/lease/flags live. Acks keyed on (contract
   version, rules revision). Decisions re-pend only on their own scope.
3. **P1 migration** [R-P1]: startup transaction runs the monotone reconciliation once (adds missing
   review activities under current policy) so the predicate change never grandfathers a tightened
   `review_mode`.
4. **Agent unblock and cancel/replace** [R-P1] with rationale under `recovery_mode=agent` /
   `agent_rule_editing` (cancel allowed with prior submissions; wrong-kind tasks are canceled and
   replaced with a link).
5. **AC amendments** [R-P1]: a contributor's amendment rides in the submission as
   `ac_amendment{old,new,rationale}`; the reviewer's structured output carries a separate
   `amendment_decision`. Keeps the intent of `coordination.rs:1093`. **(v4)** A rejected amendment
   makes the whole submission `changes_requested`.
5a. **(v4) Review unit and stacking** [R-P1] (Sol round 4):
   - **Invariant R-1**: the reviewed diff is `merge-base(T_review, C)..C`; the decision records
     `(C, reviewed_base)`. Under I-FF the landing diff is a subset of it. The merge *interaction* in R
     is covered by checks only — stated explicitly in the contract.
   - **Contributors come from the landing range**: every commit in `merge-base(T,C)..C` maps to the
     attempt that checkpointed it; the reviewer must not be a contributor to any of them; commits of
     unknown provenance count as the submitter's. Closes self-review by stacking.
   - A candidate whose range contains commits from a submission that is neither approved nor
     integrated is refused (`stacked_on_unapproved`).
   - Resubmission after `revise` is reviewed over the full range (range-diff is presentation only).
6. **Labelled human gates** [R-P1]: distinct codes + `details.required_actor="human"` for every
   remaining human-only refusal; surfaced in `preconditions`/`next`; human queue + push notification.
   (Dana's PoC: 62 lines added / 17 deleted covered pinning + gates + labels; all 155 server tests
   pass.)
7. **(v4) Lease extension by measured service downtime → [F]**, because `lease_seconds` stays 3600
   and the supervisor releases immediately on crash (only host loss recovers slowly, ≤1 h). If built:
   one startup transaction before serving, extend only leases active at shutdown by `min(gap, cap)`,
   record an event, never after a restore; a backward clock remains an incident.
8. **`next`** [R-P3]: single next action for the caller's role and capabilities with call template;
   reviews have a reserved slot per host.
9. **Decisions with default (M1)** [R-P3]: supervised agents record decisions (existing subsystem)
   with options + recommendation + reversibility; reversible and unanswered after T hours ⇒ proceed on
   the recommendation and notify; irreversible ⇒ wait (reserved). **(v4)** T = 24 h, batched in the
   digest.
10. **Additive protocol** [R-P4 cutovers]: previous client minor version accepted; the supervisor pins
    the coordination CLI to production's advertised client commit.
11. [F] `attach_candidate_ref`; `needs_attention` replacing `blocked`; `mode=recovery` and
    review-kind cleanup; auto-archive; claim-churn breaker; budget-aware ordering; launch tokens.

### 2.3 Supervisor (P3a shadow ∥ P2; P3b live)
- Root-owned service; children run as **two uids per host**: `agentc-impl` (static `class=supervised`
  implementer credential) and `agentc-rev` (**read-only** credential; the supervisor posts the
  verdict). Supervisor uid and credential not shared with children. Supervisor binary/config,
  firewall and pinned binaries are not writable by agent uids [R-P2]. Windows: Scheduled Task under
  the agent user (MINIAIR opportunistic only).
- **Harness profile** [R-P2]: Claude `-p --permission-mode dontAsk --permission-prompts none
  --setting-sources user --settings <role> --strict-mcp-config` + allowlist (prefer `--bare` with API
  key); Codex `exec --ignore-user-config --ignore-rules -s workspace-write` +
  `network_access=true` + `--add-dir <git-common-dir>` + `approval_policy=never` +
  `project_doc_max_bytes=0` (verify). **Candidate-controlled `.claude/`, `CLAUDE.md`, `AGENTS.md`,
  `.codex/` never auto-load**; repo instructions and rules are delimited data. Raw `git push` denied;
  `agent-coordinator push-candidate` with **secret scan** [R-P2]. Root-owned egress firewall allowlist
  (the only network restriction for Codex). **Settings preflight** refuses to launch on invalid role
  files (Claude `-p` silently ignores invalid settings) [R-P2]. **Pinned harness + CLI versions** in
  a root-owned prefix, autoupdate off, version recorded per launch [R-P2]. Exact draft command lines:
  transcript, Hari round 2 (⚠ items listed in Hari round 3 block P2).
- **Progress-gated renewal** [R-P3]: renew only while alive, within budget, a tool event within
  ≈10–15 min, and last checkpoint younger than ≈60 min; service-enforced `max_attempt_duration` with
  continuation claims for long work; launch identity persisted before spawn (`--session-id`, boot id,
  pid, start time); never respawn for an attempt whose launch may be alive; release on exit, crash,
  suspend, shutdown and host-update drain (**(v4) drain = release at a checkpoint**, never let leases
  expire).
- **Worktree ownership (M4)** [R-P3]: the supervisor creates the worktree, `$RUN` (outside it:
  prompt, credential, schema, target dir), and garbage-collects after the terminal state; disk
  high-water check; shared `sccache`; `TMPDIR` under `$RUN`.
- **Reviewers** [R-P3]: fresh config dir and session per launch; read-only worktree at the candidate
  SHA; structured output `{decision, findings[], criteria_evidence[], amendment_decision}`; the
  supervisor rejects decisions without per-criterion evidence (M5); **(v4)** `review_independence =
  distinct_launch` (**default**: separate reviewer uid, fresh context, contributors from the landing
  range) | `distinct_host` | `distinct_vendor` (used where both vendors are configured; the audit
  disagreement rate decides whether to require it), recorded on the decision. **Audit sample**
  [R-P3b]: re-review ~10% of approvals with the other vendor at low effort; findings go to the
  digest, not automatic tasks.
- **Health (M3)** [R-P3]: `claude auth status` / `codex login status` before claims; daily expiry
  check with notification N days ahead; 429 ⇒ mark vendor `exhausted_until`, route to the other vendor.
- **Cost** [R-P3]: the model never waits; long-poll under Cloudflare's ~100 s; short single-task
  sessions; per-role caps; kill switch; shadow mode logs would-launch + cost estimates.
- **UI verification (M2)** [R-P2]: Playwright + Chromium for agent uids; **(v4)** generic optional
  per-project `verification_env` (URL, test credential, Playwright on/off); for this project a staging
  coordinator with a staging-only test login in the reviewer's `$RUN`; screenshots/DOM assertions as
  artifacts.
- **(v4) Project environment setup** [R-P3]: a project-declared, host-approved `setup` command
  (e.g. `cargo fetch`, `uv sync`) and cache paths; the **per-project egress allowlist is owned by
  the host owner**, not the repo. Rust specifics (`CARGO_TARGET_DIR`, `sccache`, crates.io) are this
  project's configuration, not supervisor code.
- **Notifications** [R-P3b]: governed by the attention budget (§2.0): pages only for canary/SLO
  breaches and incidents; everything else in the daily digest.
- **(v4) Host updater** [R-P3b] (root-owned pull updater per host; systemd timer / Scheduled Task):
  our binaries (supervisor, integrator, CLI) only from releases built by `release.yml` from
  owner-created `v*` tags, verified by checksum and Actions build attestation, installed side by side,
  drain → switch → canary → roll back on failure; harness binaries and Playwright staged beside the
  pinned version and promoted only if the **settings preflight, the canary and the P2 containment
  suite** all pass (candidate instructions not loaded, raw push denied, writes outside `$WT`/`$RUN`
  denied, egress outside the allowlist fails) — weekly and immediately after an "upgrade
  required"-class failure; toolchains from `rust-toolchain.toml` (reviewed); unattended OS security
  updates in quiet hours. The only recurring human touchpoint is credential renewal (notified N days
  ahead).
- [F] periodic WIP snapshots (`.gitignore`-respecting temp index, size cap, secret scan); launch
  tokens; claim-churn breaker; Windows-regression auto-tasks.

### 2.4 Integrator (P4-min before the pilot)
- Runs on the e2-micro VM as its own uid + systemd unit (`MemoryMax≈256M`, `Restart=always`, cgroup
  kill on restart); GitHub App key readable only by that uid; the deploy-gate key is **not** on this
  uid. Service exposes `integrator_last_seen`; notify if stale with a non-empty queue.
- **Required for correctness (Sol's five)** [R-P4]: (1) `integration_results(submission, T0, C, R,
  tree)` with **UNIQUE(submission, T0)**, recorded before checks; later integrators reuse R;
  (2) the integrator is the only producer of R on integrator-mode projects (LLM `integrations *`
  refused with `integration_owned_by_integrator`); (3) any hold is bound to the submission, not the
  attempt; (4) I-FF: target tips form a fast-forward chain (ruleset `non_fast_forward` from day 0) +
  a monitor that freezes the queue on `target_rewritten`; (5) legacy intents imported as pinned
  results at cutover.
- **Roll-forward reconciliation** [R-P4]: after a fresh observation of tip X — `R` contained ⇒
  published (**service verifies ancestry itself with one read-only compare API call**); `X == T0` ⇒
  push R with `--force-with-lease=T0` if checks and approvals still hold; otherwise not published ⇒
  target moved: re-integrate onto the new tip; conflict or failing check on R: `revise` to the author.
  **Never infer success from a push exit code** (Sol PoC: `--force-with-lease` exits 0 when the
  remote already equals R even with a stale lease) — new DURABLE-RECORD invariant.
- **"Revise loses to a landed push"** [R-P4]: a revise during `integrating` is a request; if R later
  appears in the target, record done and turn the revision into a follow-up task. **(v4)** If the
  losing revise was `author_withdraw`, the follow-up is a high-priority `revert` (§2.4a).
- **Result branches** `ac/results/<id>` (branches trigger Actions; receipts bind the SHA)
  [R-P4], protected by ruleset A so only the App updates/deletes them.
- **Privilege gate before pushing R** [R-P4]: any workflow file in R whose blob differs from the
  target's and requests write permissions, `secrets.*`, `pull_request_target`, or new triggers ⇒ (c)
  decision. Unchanged workflow files cost nothing.
- **Checks** [R-P4]: GitHub Actions check-runs on R accepted only if `app.id` = GitHub Actions,
  `head_sha == R`, and the workflow definition blob equals the target's (or the protected roster's).
  **Exactly one deciding run** per (R, definition blob): the latest completed run; a `revise` must
  cite it. Cache written only from `main`. Actions workflows stay secret-free (documented invariant).
  Measured: "Coordination checks" 6 min, docs 1 min.
- **(v4) Reproducible flake attribution** [R-P4] (Sol pre-mortem): `check_failed` requires the failure
  to **reproduce on R** (2 of 2 or 2 of 3) **and** the check to pass on T0; anything else is
  `flaky{check, R}` ⇒ automatic rerun, a de-flake task (always admitted), never routed to the author or
  the user. A failure that also occurs on T0 alone ⇒ `fix-target` notification ([F] auto task).
  Test-level retries with flake *reporting* (nextest `--retries`; retry-and-report for the `.mjs`
  browser test). **P4 exit gate: each required check's same-SHA flip rate < 2%** over ~20 reruns of
  `main` in staging, or that check does not become required.
- **No-op** (`R == T0`) ⇒ done without "published" — **(v4) except** when a recorded revert touches
  C's range: refuse with `candidate_reverted_in_history` [R-P4]; re-land candidates must be new commits
  (cherry-pick or revert-of-revert). The ancestry audit cannot catch this, because R stays an ancestor.
- **(v4) `unreviewed_landing`** [R-P4]: the monitor labels every tip move not made by the integrator;
  landings whose range contains commits with **agent trailers** (`Co-Authored-By: Claude…`, Codex
  equivalent, `Claude-Session:`) or unknown provenance get a non-blocking post-hoc agent review whose
  findings go to the digest or become revert/fix tasks. The user's own unassisted commits stay
  unreviewed (U2). Git author identity is not the test (interactive LLMs commit as the user).
- **(v4) Compare-API credential** [R-P4]: a fine-grained read-only single-repo token (`contents:read`)
  held by the service and listed in the constitution. Projects without one fall back to integrator
  attestation plus the continuous ancestry audit (then [R] for those projects).
- **(v4) Placement**: for this project the e2-micro VM; the **generic default is the supervisor host**
  (a third uid), since not every project has an always-on VM.
- [F] **batched integration** — test the head N approved candidates together, bisect on failure;
  result keyed by `(members, T0)`, receipts on `R_batch`, per-member `R` after bisection, per-member
  "revise loses to push". **This is the first [F] item expected to trigger** (Sol's O(n²) recheck
  story). Until then small approved changes may jump the recompute queue (**ordering only**; no change
  to review or checks). Also [F]: push slot (only when >1 integrator runs); continuous ancestry audit
  (except projects without the compare credential); R′ re-keying when R is lost; warm standby;
  local container runner fast path.

### 2.4a (v4) Reverting integrated changes (M6) [R-P4]
- Task kind `revert{target:(submission, R), reason, evidence}`, created by a **human one-click**
  (dashboard or digest; no review; counted as an escaped-defect canary, not HRI) or by an **agent with
  evidence** (a check passing on R's first parent and failing on the tip after bisect; an audit-sample
  rejection; an `author_withdraw` that lost to a landed push). Reverts are always admitted.
- **Candidate computed by the integrator**, not an LLM: `git revert -m 1 R` on the current tip (the
  commit range in the FF case). For a clean mechanical revert, review judges the *decision and its
  evidence*, not the inverse diff; the integrator attests "tree == mechanical revert"; checks run as usual.
- **Conflict or failing checks** (later changes depend on X): a capped **cascade** (revert X plus later
  integrations touching X's files, newest first, at most 3, only when `main` is broken); otherwise
  convert to an ordinary implementation task ("undo X's behaviour, keep Y") with full review.
- X stays `done` with a `reverted_by` link; a defect-reason revert creates a **proposed** re-land task
  seeded with X's candidate and the revert evidence as acceptance criteria (subject to admission).

### 2.5 Constitution, rules and roster
- **Outside the repo, human-owned** [R-P4, day-0 subset now]: ruleset A (`update`, `deletion`,
  `creation` for branches incl. `ac/results/**`; bypass = integrator App + owner), ruleset B
  (`non_fast_forward`, `required_status_checks` pinned to the Actions `integration_id`; **no
  bypass**), a **tag ruleset** restricting `v*` to the owner; service switches (autonomy switches,
  review/independence floor, recovery floor, deployment mode); **protected check ids** (agents may
  add, never remove); the constitution path list; CODEOWNERS. `workflow_dispatch` deploys human-only.
- **Day-0 ruleset** (now, before P4): only `non_fast_forward` + `deletion` on `main`. Full A/B at the
  P4 cutover, because today's LLM publisher pushes `main` with agent credentials and its R never gets
  an Actions status (Hari's timing trap). Branch/tag creation limits are a **precondition of P4**.
- **Above the floor, agent-editable** [R-P4]: `.agent-coordinator/rules.md` and
  `.agent-coordinator/roster.toml` read from the **target**, changed only via review → integrate, with
  **elevated review** (one `distinct_vendor` reviewer where available + non-blocking post-hoc
  notification in the digest with a one-click revert). **(v4)** Protected check ids are the v0 guard
  against weakening; the **cooling delay** for weakening edits (check ids removed/non-required, edits
  to `@binding` lines including moving/removing a tag) is **[F]** — the evidence shows no agent ever
  weakened a check. No self-benefit: a rules/roster revision never applies to a submission
  whose contributors include its author or approver. Rules reach agents as delimited data, never as
  system prompt.

### 2.6 Human day-to-day work (U2)
No `quick` lane. Humans fast-forward `main` only to SHAs with green required checks (ruleset B, no
bypass) via a ~20-line `ship` script (push `ac/human/<name>` → `gh run watch` → push SHA to `main`).
Pre-merge review on every agent path (median review 3.9 min). Direct pushes are a canary.

### 2.7 Coordinator deployment (U9)
Production pinned to a release; staging dogfoods `main`. Expand-only migrations, backup before
migrate, smoke test against a restored backup. Deploys are reserved decisions until P6 data exists.
[F] fenced deploy gate (forced-command SSH key; monotonic generation; per-generation journal; key not on
the integrator uid).

### 2.8 Instructions and docs
Per-role contract ≤1.5k words (moved into P3). MCP frozen for interactive/foreign harnesses. Stale
docs fixed as touched; `book.toml` `additional-js` external-link hook (user's global rule). [F] book
restructure into Agent / Operator / Design history.

## 3. Phases

Order agreed by all four participants: **P0 → P1 → P2 ∥ P3a → P4-min → P3b pilot (go/no-go) → P6
dogfood**.

| Phase | Content | Deletes | Exit criterion |
|---|---|---|---|
| **P0 (day 0, human + agent, ~15 min)** | Put U1–U11 to the user; ruleset `non_fast_forward` + `deletion` on `main` (via `gh api`, verified by a rejected force push); user runs the 10-minute throwaway-repo ruleset test (U4); cancel sentinel 5655e94d with rationale; settle dea68719 | — | Rules live; decisions recorded |
| **P1 Unblock (1–2 days, ~450–650 LOC + stacking rules)** | §2.2 items 1–6 incl. 5a (landing-range contributors, `stacked_on_unapproved`), P1 migration; guidance prose "operator reopen" ×6 + 4 book pages | whole-revision staleness | One regression test per fix; staging replay of B1/B2 (lease edit, `review_mode` change, conflict, legacy null ref) with no human |
| **P2 Containment & staging** | `agentc-impl`/`agentc-rev` uids, root-owned firewall, pinned binaries, harness profiles, settings preflight, secret-scanning `push-candidate`, Playwright + `verification_env`, staging coordinator (mxmini, loopback, default config) + throwaway project, `ship` script; ~10 probe prompts (<$1, user-approved) for the ⚠ items | full-access unattended defaults | **Containment suite** passes under each exact profile (candidate instructions not loaded, raw push denied, out-of-tree writes denied, egress outside allowlist fails); `cargo test` passes under each profile (no LLM) |
| **P3a Shadow (∥ P2)** | read-only host principal, `next` v1, would-launch log with cost estimates | — | 1 day of shadow logs |
| **P4-min Integrator** | §2.4 required items (Sol's five, roll-forward, revise-loses-to-push, result-branch protection, privilege gate, bound receipts, one deciding run, reproducible flake attribution, `candidate_reverted_in_history`, `unreviewed_landing`, compare-API token) + §2.4a revert; repo roster/rules read from target; constitution; branch/tag creation limits **first**, then full rulesets A/B + tag ruleset; cutover preflight (zero unresolved intents/holds/reservations/jobs/recovery) + legacy import; shadow day first | LLM integration commands, intents, reconciliation, journal attestations, global hold, agent `PATCH policy`, `workflow_policies` (after first roster commit) | Staging soak (no LLM): conflicts, target moves, check failures, crashes, reverts resolve with HRI=0; integration p50 < 10 min; **each required check's flip rate < 2%** |
| **P3b Pilot (go/no-go)** | live supervisor on oracle-1 (+ mxmini): progress renewal, crash release, worktree ownership, project `setup`, reviewers + audit sample, health, per-role contract ≤1.5k words, M1 decisions, **attention budget** (admission, digest, canary + SLO paging, stall-as-HRI, path-overlap avoidance), host updater | "continue" prompts; hand-spawned reviewers; ~10k words of startup prose; worktree-cleanup prose | **5 real tasks**: HRI=0, recorded cost/task, canary green per vendor/host, audit disagreement noted; user go/no-go |
| **P6 Dogfood** | 2 weeks autonomous on this project; [F] items only when measured (batching expected first) | as justified | HRI/task = 0 excluding reserved; value canary not worse than baseline; direct-push canary flat; digest ≤ 1/day |

## 4. User decisions (recommendation first)
- **U1** Windows: non-blocking Actions observer (**recommended**) vs first-class required producer.
- **U2** Human changes: FF pushes of green SHAs via `ship` under rulesets; pre-merge review for all agent
  paths; post-hoc review only for landings with agent trailers or unknown provenance (**recommended**).
- **U3** Billing: API billing for the unattended pool (**recommended**; also enables Claude `--bare`),
  ceiling set after the pilot (estimate $5–30/task; one stuck task cost ≈$50).
- **U4** Org transfer: **no** (recommended), unless the 10-minute ruleset test on a throwaway repo
  fails.
- **U5 (v4)** Review independence default: `distinct_launch` with a separate reviewer uid and
  landing-range contributors (**recommended**); `distinct_vendor` where both vendors are configured;
  the audit sample decides whether to require it.
- **U6** Hosts: oracle-1 primary supervisor, mxmini secondary with quiet hours, MINIAIR opportunistic,
  integrator on the e2-micro (**recommended**, pending oracle-1 uptime/specs).
- **U7** GitHub dependency: Actions receipts as the required default (**recommended**; native
  producer receipt type kept as an escape hatch). Conscious departure from day-one vendor neutrality.
- **U8** Rule edits: autonomous above the constitution with elevated review and post-hoc
  notification; cooling delay [F] (**recommended**; it is the stated goal).
- **U9** Coordinator deploys: reserved decisions (with release packet [F]) until P6 data exists
  (**recommended**).
- **U10** `requires_human_acceptance`: does **not** block done; separate non-blocking follow-up
  (**recommended**).
- **U11** Probe spend: approve ~10 trivial probe prompts (<$1) in P2 and the 5-task pilot budget.
- **U12 (v4)** Admission rule for agent-originated work: always admit reverts/fix-target/de-flake/
  refusal fixes, plus **N = 5 agent-originated tasks per week** (**recommended** starting value);
  everything else triaged in the daily digest.
- **U13 (v4)** Notification channel for the digest and pages (ntfy, email, or both).

## 5. (v4) Generic projects (not just this one)
- Runtime design is not over-built for a small project; the cost is **bootstrap**, which must be
  scripted, consistent with "runnable with empty config": `agent-coordinator bootstrap github`
  (creates rulesets A/B + tag ruleset via the API; roster defaults to every job of every
  push-triggered workflow; protected check ids default to that set) and `agent-coordinator host init`
  (uids, firewall, pinned harnesses, integrator as a third uid on the same host). [R-P6 for generic
  onboarding; not needed for this project's pilot]
- Coordinator-specific items are configuration, not design: Rust caches/egress, the staging
  coordinator as `verification_env`, integrator on an always-on VM, deploy/fence gate and U9 are
  **self-hosting only**.
- Non-GitHub remotes: the protocol (pinned R, roll-forward, I-FF) is remote-agnostic; receipts fall
  back to the native-producer type and ancestry to integrator attestation plus continuous audit.

## 6. (v4) Executing this plan
**Hybrid** (Hari round 4; Dana and Sol agree):
- **Direct interactive sessions** for P0, P1, P2 and the P4 cutover, because P1 fixes the very
  workflow defects that would stall it inside the coordinator, and P4 replaces the integration path it
  would have to use. Discipline anyway: phase branch in its own worktree with its own
  `CARGO_TARGET_DIR`; repo gate green locally (fmt, clippy `-D warnings`, test, docs check); push the
  branch; Actions green; then fast-forward `main` (the `ship` pattern before rulesets enforce it).
  Production deploys remain reserved decisions.
- **Switch to dogfooding once P1 is deployed to production**: P3a/P3b items and all follow-ups become
  coordinator tasks, worked by interactive sessions first and by the supervisor from P3b.
- **Start execution in a new session** from `HANDOFF-autonomy.md`, not in the planning session.

**First three steps for the next session:**
1. **P0 with the user** (~15 min): U1–U13 via AskUserQuestion (recommendation first); day-0 ruleset;
   the U4 ruleset test; cancel 5655e94d; settle dea68719.
2. **P1** on branch `autonomy/p1` in a fresh worktree: start from Dana's measured PoC shape (scoped
   pinning + monotone migration + satisfaction order), then `revise` with reason enum + unresolved-intent
   guard + rate limit, landing-range contributors + `stacked_on_unapproved`, agent unblock and
   cancel/replace, `ac_amendment`, labelled gates. One regression test per fix, gate after each
   sub-phase, checkpoint commits.
3. **Staging coordinator on mxmini** (loopback, default config) with a throwaway project and repo;
   deploy the P1 build there and replay B1/B2 with the CLI as a scripted agent; then ship P1 to `main`
   and put the production deploy to the user as a reserved decision.
