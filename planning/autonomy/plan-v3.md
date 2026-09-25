# Plan v3 — Full agent autonomy for Agent Coordinator projects

Status: **consolidated after discussion rounds 1–3** (`discussion-transcript.md`). Supersedes v1/v2.
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
- **Canaries**: direct human pushes/week; revise ping-pong; claim churn; spend vs budget; review
  latency; **audit-sample disagreement rate** (M5).

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
   `amendment_decision`. Keeps the intent of `coordination.rs:1093`.
6. **Labelled human gates** [R-P1]: distinct codes + `details.required_actor="human"` for every
   remaining human-only refusal; surfaced in `preconditions`/`next`; human queue + push notification.
   (Dana's PoC: 62 lines added / 17 deleted covered pinning + gates + labels; all 155 server tests
   pass.)
7. **Lease extension by measured service downtime** [R-P3]: in one startup transaction before
   serving, extend only leases active at shutdown by `min(gap, cap)`, record an event, never after a
   restore; a backward clock remains an incident.
8. **`next`** [R-P3]: single next action for the caller's role and capabilities with call template;
   reviews have a reserved slot per host.
9. **Decisions with default (M1)** [R-P3]: supervised agents record decisions (existing subsystem)
   with options + recommendation + reversibility; reversible and unanswered after T hours ⇒ proceed on
   the recommendation and notify; irreversible ⇒ wait (reserved).
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
  suspend, shutdown; "service unreachable" pauses renewal expectations (lease extension, §2.2.7).
- **Worktree ownership (M4)** [R-P3]: the supervisor creates the worktree, `$RUN` (outside it:
  prompt, credential, schema, target dir), and garbage-collects after the terminal state; disk
  high-water check; shared `sccache`; `TMPDIR` under `$RUN`.
- **Reviewers** [R-P3]: fresh config dir and session per launch; read-only worktree at the candidate
  SHA; structured output `{decision, findings[], criteria_evidence[], amendment_decision}`; the
  supervisor rejects decisions without per-criterion evidence (M5); `review_independence =
  distinct_vendor` (default when available) | `distinct_host` | `distinct_launch`, recorded on the
  decision. **Audit sample** [R-P3b]: re-review ~10% of approvals with the other vendor at low
  effort; disagreement rate is a canary.
- **Health (M3)** [R-P3]: `claude auth status` / `codex login status` before claims; daily expiry
  check with notification N days ahead; 429 ⇒ mark vendor `exhausted_until`, route to the other vendor.
- **Cost** [R-P3]: the model never waits; long-poll under Cloudflare's ~100 s; short single-task
  sessions; per-role caps; kill switch; shadow mode logs would-launch + cost estimates.
- **UI verification (M2)** [R-P2]: Playwright + Chromium for agent uids; staging coordinator with a
  staging-only test login in the reviewer's `$RUN`; screenshots/DOM assertions as artifacts.
- **Notifications** [R-P3]: push (ntfy/email) for parked tasks, reserved decisions, incidents,
  integrator silence, credential expiry.
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
  appears in the target, record done and turn the revision into a follow-up task.
- **Result branches** `ac/results/<id>` (branches trigger Actions; receipts bind the SHA)
  [R-P4], protected by ruleset A so only the App updates/deletes them.
- **Privilege gate before pushing R** [R-P4]: any workflow file in R whose blob differs from the
  target's and requests write permissions, `secrets.*`, `pull_request_target`, or new triggers ⇒ (c)
  decision. Unchanged workflow files cost nothing.
- **Checks** [R-P4]: GitHub Actions check-runs on R accepted only if `app.id` = GitHub Actions,
  `head_sha == R`, and the workflow definition blob equals the target's (or the protected roster's);
  explicit flake policy (bounded retry; a failure that also occurs on T0 alone is not attributed to the
  candidate ⇒ notify, [F] auto `fix-target` task). Cache written only from `main`. Actions
  workflows stay secret-free (documented invariant). Measured: "Coordination checks" 6 min, docs 1 min.
- **No-op** (`R == T0`) ⇒ done without "published".
- [F] push slot (needed only when >1 integrator runs); continuous ancestry audit; R′ re-keying when
  R is lost; warm standby on oracle-1; local container runner fast path.

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
  **elevated review** (one `distinct_vendor` reviewer + non-blocking post-hoc notification with a
  one-click revert task). **Weakening** (check ids removed or made non-required; edits to lines tagged
  `@binding`, including removing or moving a tag) takes effect after a **cooling delay**;
  strengthening is immediate. No self-benefit: a rules/roster revision never applies to a submission
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

Order agreed by all four participants: **P1 → P2 ∥ P3a → P4-min → P3b pilot (go/no-go) → P6 dogfood**.

| Phase | Content | Deletes | Exit criterion |
|---|---|---|---|
| **P0 (day 0, human, minutes)** | Ruleset: `non_fast_forward` + `deletion` on `main`; user answers U1–U10; cancel sentinel 5655e94d; finish or cancel dea68719 | — | Rules live; decisions recorded |
| **P1 Unblock (1–2 days, ~450–650 LOC incl. tests/docs)** | §2.2 items 1–6 + migration; guidance prose "operator reopen" ×6 + 4 book pages | whole-revision staleness | Regression tests per fix; staging: policy edits, conflicts, legacy refs resolve with no human |
| **P2 Containment & staging** | `agentc-impl`/`agentc-rev` uids, firewall, pinned binaries, harness profiles, settings preflight, secret-scanning `push-candidate`, Playwright + staging login, staging coordinator + throwaway project, `ship` script; ~10 probe prompts (<$1, user-approved) for the ⚠ items | full-access unattended defaults | `cargo test` passes under each exact profile (no LLM); probes confirm candidate instructions do not load |
| **P3a Shadow (∥ P2)** | read-only host principal, `next` v1, would-launch log with cost estimates | — | 1 day of shadow logs |
| **P4-min Integrator** | §2.4 required items; privilege gate; Actions receipts; repo roster/rules read from target; constitution; full rulesets A/B + tag ruleset (branch/tag limits first); cutover preflight (zero unresolved intents/holds/reservations/jobs/recovery) + legacy import; shadow day first | LLM integration commands, intents, reconciliation, journal attestations, global hold, agent `PATCH policy`, `workflow_policies` (after first roster commit) | Staging soak (no LLM): conflicts, target moves, check failures, crashes resolve with HRI=0; integration p50 < 10 min |
| **P3b Pilot (go/no-go)** | live supervisor on oracle-1 (+ mxmini): progress renewal, crash release, worktree ownership, reviewers + audit sample, health, notifications, per-role contract, M1 decisions, lease extension | "continue" prompts; hand-spawned reviewers; startup prose | **5 real tasks**: HRI=0, recorded cost/task, audit disagreement noted; user go/no-go |
| **P6 Dogfood** | 2 weeks autonomous on this project; [F] items only when measured | as justified | HRI/task = 0 excluding reserved; canaries flat |

## 4. User decisions (recommendation first)
- **U1** Windows: non-blocking Actions observer (**recommended**) vs first-class required producer.
- **U2** Human changes: FF pushes of green SHAs via `ship` under rulesets, pre-merge review for agents
  (**recommended**).
- **U3** Billing: API billing for the unattended pool (**recommended**; also enables Claude `--bare`),
  ceiling set after the pilot (estimate $5–30/task; one stuck task cost ≈$50).
- **U4** Org transfer: **no** (recommended), unless the 10-minute ruleset test on a throwaway repo
  fails.
- **U5** Review independence default: `distinct_vendor` when available (**recommended**).
- **U6** Hosts: oracle-1 primary supervisor, mxmini secondary with quiet hours, MINIAIR opportunistic,
  integrator on the e2-micro (**recommended**, pending oracle-1 uptime/specs).
- **U7** GitHub dependency: Actions receipts as the required default (**recommended**; native
  producer receipt type kept as an escape hatch). Conscious departure from day-one vendor neutrality.
- **U8** Rule edits: autonomous above the constitution with elevated review, cooling delay on
  weakening, post-hoc notification (**recommended**; it is the stated goal).
- **U9** Coordinator deploys: reserved decisions until P6 data exists (**recommended**).
- **U10** `requires_human_acceptance`: does **not** block done; separate non-blocking follow-up
  (**recommended**).
- **U11** Probe spend: approve ~10 trivial probe prompts (<$1) in P2 and the 5-task pilot budget.

## 5. Open questions for round 4 (pre-mortem)
- Who updates supervisors, harnesses and the integrator on hosts, and is that a recurring human
  touchpoint?
- How is this plan itself executed: through the coordinator (dogfood) or directly?
- Portability to non-GitHub projects.
