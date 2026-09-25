# Final plan — Full agent autonomy for Agent Coordinator projects

Status: **FINAL** — plan-v4 plus the round-5 sign-off reservations (all three reviewers: AGREE WITH
RESERVATIONS; every reservation applied, marked **(final)**). Supersedes v1–v4. Changes since v3 are
marked **(v4)**; round-6/7 cold-read rulings **(r7)**; round-8 verification fixes **(r8)** (see
§2.2-P1). Verbatim discussion: `discussion-transcript.md`.
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
  silent stall can never score 0 (Hari pre-mortem). **(r7) Defaults: stall N = 6 h outside quiet hours;
  digest-unread N = 3 days; both configurable in code-defaulted settings.**
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
1. **Admission control for agent-originated work** **(final) [R-P6, in place before dogfood starts]**
   (not needed for the 5-task pilot). Tasks created by agents start in state
   `proposed` with `origin=agent`; `next` never offers a proposed task. A **human-owned admission
   rule** (service-side, part of the constitution) admits: `revert`, `fix-target`, de-flake,
   fixes for labelled refusals, and up to N agent-originated tasks per week (budget). Everything else
   waits in the digest for one-pass human triage.
2. **One daily digest** [R-P3b]: proposed tasks, M1 decisions pending or taken on timeout, post-hoc
   rule notices, audit-sample disagreements, release packets, credential warnings, spend.
3. **Paging only on SLO breach** [R-P3b]: **(final)** one end-to-end canary for the pilot, then a
   **daily end-to-end synthetic canary per vendor and host** [R-P6]
   (claim → launch → edit → push-candidate → review → integrator → Actions → done, on a dedicated
   canary project/repo, low effort, cents per day) plus throughput/queue-age SLOs. Page when the
   canary fails or an SLO breaks; everything else goes to the digest. The canary also records the
   same-SHA flip rate of each required check.
4. **Stalls count as HRI** (§0).
5. **Flaky checks never reach an author or the user** (§2.4 flake attribution).
6. **(final) Digest-neglect paging** (Dana's top risk): the **value canary** and "digest unread for N
   days" page the user, so a user who stops reading the digest cannot leave HRI at 0 while proposed
   work, decisions and audit findings pile up.

Also: `next` avoids handing out tasks whose paths overlap files the user shipped in the last 24 h
(file lists only) [R-P3b]; reserved release decisions come with an auto-built **release packet**
(diff summary by area, guard-file diff, staging soak evidence) so the click carries information [F].

### 2.1 Components and deletions
| Component | Deletes |
|---|---|
| Supervisor (per host; **(r7)** pilot core (P3b) budget ≤1.5k lines incl. tests — if exceeded, stop and re-scope before the pilot; full supervisor with the P6 items ≈2–3k) | "continue" prompts; lease expiry on turn end; hand-spawned reviewers; relays; worktree-cleanup prose ×3 (M4) |
| Integrator (deterministic, non-LLM) | LLM `integrations prepare/publish/reconcile/finish`; publication intents; reconciliation; journal attestations; global hold |
| Actions receipts (bound) | workstation producers on the required path; check reservations |
| Repo rules + roster | agent `PATCH policy`; `workflow_policies`; whole-revision staleness |
| `next` + per-role contract ≤1.5k words | ~10k words of startup prose (MCP frozen, not rewritten) |

### 2.2 Service protocol (P1 unless noted)
1. **`revise`** [R-P1] — **(r7)** implemented by **extending `workflow/reopen`** with a required
   `reason{code, evidence}` for agent callers; the human path is unchanged; CLI verb `revise`. The
   existing exact-submission check under the writer lock plus the unresolved-intent guard is the CAS
   (**(r8)** the exact current submission id; no subject-generation field exists or is needed). Closed reason enum with evidence and allowed actors —
   `conflict{T,C}`, `check_failed{receipt on R}` (integrator role; in P1 any agent with evidence),
   `candidate_missing` (also covers the B8 legacy null ref), `requirements_changed{task_def_digest}`
   (**refused for contributors to the task**), `author_withdraw` (author only). Refused while a
   publication intent is unresolved (existing guard near `workflow.rs:1133`). **(r7) P1: rate limit
   only — 3 agent revises per subject per 24 h, then park in the human queue/digest.**
   Serialize-before-park (make the later task depend on the one whose landing moved T) is **[R-P4]**,
   because only the integrator knows which landing moved the target.
2. **Scoped pinning** [R-P1]: pin candidate + **(r7) a digest of the judged task fields (title,
   description, acceptance criteria, kind)** — not `tasks.revision`, which priority-only edits bump
   (`coordination.rs:1098`) — + monotone required-review set. A change to the digest re-queues
   *approvals* (re-review), not the candidate; a `kind` change goes to cancel/replace.
   Satisfaction order `human ≥ either ≥ agent`; an active activity no longer required may finish and
   its approval counts [R-P1]. **(r7) Roster (P1): the roster revision is captured on the publication
   intent (the last service call before the push); `integration-result` and `finalize` validate
   against that captured revision, never the current one** (validating against "current" would strand
   already-published results at `workflow.rs:2437`/`:2912`); a roster change after the intent applies
   to the next integration. P4: the roster file in T0. Rules/lease/flags live. **(r7) Acks keyed on
   `(INSTRUCTION_VERSION, sha256(rules))`, computed on read (no new column).** Decisions re-pend only
   on their own scope.
3. **Review reconciliation** [R-P1]: **(r7)** one idempotent function `reconcile_required_reviews`
   (adds missing review activities under the current policy; never removes active ones) called
   **inside the policy-update transaction** and **at every startup** (a Rust post-migrate step; if a
   marker row is used it commits in the same transaction), so the predicate change never grandfathers
   a tightened `review_mode`.
3a. **(r7) P1 production-deploy preflight** [R-P1]: before deploying P1, a read-only preflight must show
   zero unresolved publication intents, held holds, held reservations and active integration attempts;
   otherwise wait for them to drain.
3b. **(r7) CLI** [R-P1]: thin subcommands `revise`, `unblock`, `cancel` over the existing HTTP client and
   mutation journal (~30 lines each).
4. **Agent unblock and cancel/replace** [R-P1] with rationale under `recovery_mode=agent` /
   `agent_rule_editing` (cancel allowed with prior submissions; wrong-kind tasks are canceled and
   replaced with a link).
5. **AC amendments** [R-P1]: a contributor's amendment rides in the submission as
   `ac_amendment{old,new,rationale}`; the reviewer's structured output carries a separate
   `amendment_decision`. Keeps the intent of `coordination.rs:1093`. **(v4)** A rejected amendment
   makes the whole submission `changes_requested`.
5a. **(v4) Review unit and stacking** — **(r7/r8) [R-P4], enforced by the integrator** (Casey's cold
   read: `checkpoints` has no SHA column and the service has no Git, so the service cannot compute
   ranges). At **push-authority time** (§2.4b) the integrator — the only trusted Git-capable component
   — supplies the landing range `merge-base(T0,C)..C`; the service **stores it per submission**,
   computes contributors as the union over submissions whose stored ranges contain each commit (a
   commit found in no other submission counts as the submitter's), and refuses push authority if an
   approving reviewer is among them (the submission returns to review for an eligible reviewer) or if
   the range intersects a submission that is neither approved nor integrated
   (`stacked_on_unapproved`). The stacking hole predates P1 (not a regression), and the pilot runs
   after P4-min. *Fallback for projects without an integrator* (Sol): the submitting CLI sends
   `range_commits[]` (`git rev-list T..C`) and the reviewer's CLI re-verifies on claim. Rules:
   - **Invariant R-1**: the reviewed diff is `merge-base(T_review, C)..C`; the decision records
     `(C, reviewed_base)`. Under I-FF the landing diff is a subset of it. The merge *interaction* in R
     is covered by checks only — stated explicitly in the contract.
   - Resubmission after `revise` is reviewed over the full range (range-diff is presentation only).
6. **Labelled human gates** [R-P1]: distinct codes + `details.required_actor="human"` for every
   remaining human-only refusal; surfaced in `preconditions`/`next`; human queue; **(final)** reported
   in the daily digest (not paged).
6a. **(final) Known P1-only human gate** (Sol): a check failure *after* a publication intent was
   recorded (5c85ebb2 class) still needs human reconciliation in P1 if the preparing workstation is gone,
   because intents are recorded at prepare time. Not patched in P1 (P4 deletes intents); listed so
   nobody mistakes it for a regression. **(r7) (b)** A second P1-only gate: **target moved after the
   intent** (tip ∉ {T0, R}), or the preparing workstation's journal is gone — the 3590591f agent
   reconciliation accepts only an exact base or result (`workflow.rs:2757-2768`). Both are removed by
   P4 roll-forward. [F] optional cheap P1 fix (same workstation only): agent `not_published` when
   tip ≠ T0 and the CLI verifies R ∉ ancestors(tip).
6b. **(r7) Credential attributes** [R-P2/P3a] (Hari): credentials gain `class` (supervised |
   interactive) and `access` (read | write), stored on the credential, enforced on every mutation route
   and recorded on events — the success metric, the read-only reviewer credential and the P3a read-only
   host principal depend on them (none exists in `auth.rs` today). Holders per host: `agentc-impl` =
   write implementer credential; `agentc-rev` = **read-only** credential; the supervisor (unreadable by
   children) renews/releases with the implementer credential and posts verdicts with a separate write
   **reviewer** principal (verdict principal ≠ author principal).
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

### 2.2-P1 (r8) Consolidated P1 specification (answers to the second cold read, Robin)
Where this section and earlier §2.2 text differ, **this section wins**.
- **Switch and actors for agent `revise`** (Sol): all agent reasons require `recovery_mode=agent`
  (manual ⇒ 403 `human_reopen_required`). `conflict`, `check_failed`: caller holds the submission's
  active integration activity (in P1, the LLM integrator). `candidate_missing`: any agent (evidence is
  service-verifiable: `candidate_ref IS NULL`; integration claims are refused in that state at
  `workflow.rs:1627`). `requirements_changed`: any agent that is not a contributor, and only when the
  current judged-field digest ≠ the pinned digest. `author_withdraw`: author only.
- **Rate limit / "park"**: the 4th agent `revise` in 24 h returns 403 `revise_limit_reached` with
  `required_actor="human"`; it surfaces in `preconditions`/`next` and the human queue. **No new state.**
- **Task digest (lead ruling: stored, per Sol; Dana preferred compute-on-read)**: `submissions.task_digest`
  = sha256 of the judged fields (title, description, acceptance criteria, kind), computed at submit time,
  added by an expand-only migration. **Attempt pins move to the digest too**: the submit-time
  `policy_changed`/`pinned_revision_changed` checks (`workflow.rs:455–471`) and attempt pins
  (`:1743`, `:3004`) compare only the digest — otherwise B1 still fails at submit (the 7e0c58b5 symptom).
- **Satisfaction by approver class** (Sol): a human approval satisfies `human_review`, `either_review`
  and `agent_review`; an agent approval satisfies `agent_review` and `either_review`; each approval
  counts once; `both` needs one human and one agent decision; under `none` queued reviews are canceled
  and active ones may finish without gating.
- **`reconcile_required_reviews`**: adds activities **only to subjects in phase `review`**; subjects in
  `integration` (with or without an intent) keep the set they were approved under. Called inside
  `update_policy`'s transaction and once at every startup before serving; idempotent; **no marker row**.
- **Decision scope** (Sol/Dana): replace `policy_revision`/`task_revision` at `knowledge.rs:1066` and
  `:1287` with `sha256(rules)` plus the judged-field digest of each affected task (and the decision's
  own expiry/disposition). Nothing richer.
- **Roster**: captured on `publication_intents` (the only other P1 migration); `integration-result`
  and `finalize` validate against it.
- **Labels only** for `imports.rs` and `jobs.rs:267` gates (no authority change). New helpers ≤20
  lines; untouched handler bodies are not refactored in P1.
- **Prose**: `grep -rl reopen book/src` finds 13 files (4 say "operator reopen"); update every statement
  that reopen is human-only or operator-only, plus the guidance strings. The `additional-js` link hook
  is not P1 work.
- **Tests vs staging**: B2 (conflict) and B8 (legacy null ref) are covered in `crates/server/tests`
  (fixtures already fake repos and insert legacy rows by SQL). **B1** (lease edit, `review_mode` change)
  is replayed in staging with the new CLI subcommands. Replay B2 in staging only if the CLI accepts a
  local bare-repo remote without new code (`scripts/completion_smoke.py:21` shows the setup).
- **Preflight** (read-only, run by the user on the VM with `sqlite3 -readonly` against the live DB or a
  fresh backup; column names verified by the lead against migrations `0003`, `0004`, `0018` on 2026-09-25 — re-check if migrations changed), **before the `main` push
  and again before the deploy**; every count must be 0, otherwise wait:
  ```
  SELECT count(*) FROM publication_intents pi
    LEFT JOIN integration_results ir ON ir.activity_id=pi.activity_id
    LEFT JOIN publication_reconciliations pr ON pr.activity_id=pi.activity_id
    WHERE (ir.activity_id IS NULL OR ir.publication_state='uncertain') AND pr.activity_id IS NULL;
  SELECT count(*) FROM integration_holds WHERE state='held';
  SELECT count(*) FROM reservations WHERE state='held';
  SELECT count(*) FROM workflow_activities WHERE kind='integration' AND state IN ('active','recovery_required');
  ```
- **Pre-merge review of P1 itself** (U2 applies to agent-authored guard code): a fresh-context
  subagent review of the final P1 diff inside the interactive session, then **the user's explicit OK**
  (and optional diff review) before the fast-forward of `main`. **Every push** (any branch; the repo is
  public) and the `main` fast-forward need the user's go-ahead at that moment.
- **Progress record**: an "Execution log" section in `HANDOFF-autonomy.md` on `autonomy-plan` (phase,
  step, commit SHA, gate result).

### 2.3 Supervisor (P3a shadow ∥ P2; P3b live)
- Root-owned service; children run as **two uids per host**: `agentc-impl` (static `class=supervised`
  implementer credential) and `agentc-rev` (**read-only** credential; the supervisor posts the
  verdict). Supervisor uid and credential not shared with children. Supervisor binary/config,
  firewall and pinned binaries are not writable by agent uids [R-P2]. Windows: Scheduled Task under
  the agent user (MINIAIR opportunistic only).
- **Harness profile** [R-P2]: Claude `-p --permission-mode dontAsk --permission-prompts none
  --setting-sources user --settings <role> --strict-mcp-config` + allowlist (prefer `--bare` with API
  key); Codex `exec --ignore-user-config --ignore-rules -s workspace-write` +
  `network_access=true` + **(r7)** `-C <per-launch clone>` (no shared git-common-dir; see Git isolation) + `approval_policy=never` +
  `project_doc_max_bytes=0` (verify). **Candidate-controlled `.claude/`, `CLAUDE.md`, `AGENTS.md`,
  `.codex/` never auto-load**; repo instructions and rules are delimited data. Raw `git push` denied;
  `agent-coordinator push-candidate` with **secret scan** [R-P2]. Root-owned egress firewall allowlist
  (the only network restriction for Codex). **Settings preflight** refuses to launch on invalid role
  files (Claude `-p` silently ignores invalid settings) [R-P2]. **Pinned harness + CLI versions** in
  a root-owned prefix, autoupdate off, version recorded per launch [R-P2]. Exact draft command lines:
  transcript, Hari round 2 (⚠ items listed in Hari round 3 block P2). **(r7)** Those lines predate later
  rulings: launch tokens are [F] (two uids with static credentials instead), `--add-dir <git-common-dir>`
  is replaced by per-launch clones, and `--setting-sources user` does **not** stop `CLAUDE.md` discovery —
  only `--bare` (API key) is known to; verify with the U11 probes (a P2 blocker; if U3 = subscription,
  P2 must prove cwd = `$RUN` + clone via `--add-dir`, or the pilot runs Codex-only).
- **Progress-gated renewal** [R-P3]: renew only while alive, within budget, a tool event within
  ≈10–15 min, and last checkpoint younger than ≈60 min; service-enforced `max_attempt_duration` with
  continuation claims for long work; launch identity persisted before spawn (`--session-id`, boot id,
  pid, start time); never respawn for an attempt whose launch may be alive; release on exit, crash,
  suspend, shutdown and host-update drain (**(v4) drain = release at a checkpoint**, never let leases
  expire).
- **(final) Service-verifiable recovery evidence** [R-P3] (Sol, round 3 B5): recovery requires facts
  the service can check — the old launch is finished or its credential expired, the generation was
  bumped, the last checkpoint SHA equals the WIP ref the recoverer fetched, no nonterminal jobs — not
  local attestations (`saved_work_checked`/`running_jobs_checked`). **Invariant: the checkpoint SHA
  recorded in the service is authoritative; refs are only transport**; recoverers and the integrator
  verify ref == recorded SHA.
- **(final) Git isolation between roles** [R-P2] (Hari): no shared `.git` between implementer and
  reviewer; one clone per launch owned by that role's uid (`git clone --reference` a read-only host
  mirror); supervisor Git calls run as the agent uid with hooks and fsmonitor disabled
  (`core.hooksPath=/dev/null`, `core.fsmonitor=false`), never `safe.directory='*'`; the containment
  suite includes a planted-hook case. (Supersedes `--add-dir <git-common-dir>` for shared worktrees.)
- **Worktree ownership (M4)** [R-P3]: the supervisor creates the **per-launch clone** (r7), `$RUN` (outside it:
  prompt, credential, schema, target dir), and garbage-collects after the terminal state; disk
  high-water check; shared `sccache`; `TMPDIR` under `$RUN`.
- **Reviewers** [R-P3]: fresh config dir and session per launch; read-only **per-launch clone owned by `agentc-rev`** at the candidate
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
- **(v4) Host updater** **(final) [R-P6]** (pilot uses hand-pinned harnesses) (root-owned pull updater per host; systemd timer / Scheduled Task):
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
  a monitor that freezes the queue on `target_rewritten`; (5) **(final)** cutover preflight requires
  **zero unresolved publication intents** (drained by the existing agent reconciliation from 3590591f
  while the old path is still active); no legacy import, so no conflict with R′ re-keying being [F].
- **(final) Ruleset watchdog** [R-P4] (Sol's top risk: the proof depends on ruleset B and the single
  producer, both enforced outside our code): every integrator cycle reads back the branch rules
  (`GET /repos/{o}/{r}/rules/branches/main`); if `non_fast_forward` or the required checks are missing,
  freeze the queue and page.
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
  the user. A failure that also occurs on T0 alone ⇒ `fix-target` entry in the digest ([F] auto task).
  Test-level retries with flake *reporting* (nextest `--retries`; retry-and-report for the `.mjs`
  browser test). **P4 exit gate: each required check's same-SHA flip rate < 2%** over ~20 reruns
  (`gh run rerun`) of the required workflows on the current `main` SHA **in the real repository** (r7;
  a throwaway staging repo has different workflows), or that check does not become required.
- **No-op** (`R == T0`) ⇒ done without "published" — **(v4) except** when a recorded revert touches
  C's range: refuse with `candidate_reverted_in_history` [R-P4]; re-land candidates must be new commits
  (cherry-pick or revert-of-revert). The ancestry audit cannot catch this, because R stays an ancestor.
- **(v4) `unreviewed_landing`** [R-P4]: the monitor labels every tip move not made by the integrator;
  landings whose range contains commits with **agent trailers** (`Co-Authored-By: Claude…`, Codex
  equivalent, `Claude-Session:`) or unknown provenance get a non-blocking post-hoc agent review whose
  findings go to the digest or become revert/fix tasks. The user's own unassisted commits stay
  unreviewed (U2). Git author identity is not the test (interactive LLMs commit as the user).
  **(final)** Trailer detection is best-effort under the confidently-wrong threat model, **not a
  security control** (a session can strip trailers; the stronger form is human-key signing).
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

### 2.4b (r7) Minimal integrator↔service API (P4 design input, Sol round 6)
- `POST integration-results {submission, T0, C, R, tree}` — UNIQUE(submission, T0); idempotent replay
  returns the existing R.
- `POST push-authority {result_id}` → `{deciding_runs[], roster_ids, protected_ids, expires_at}` or a
  refusal; issuing takes the submission-bound hold; a `revise` does not revoke an issued authority
  (revise loses to a landed push).
- `POST observations {result_id, tip}` — the service classifies published / not_published /
  roll_forward itself via the compare API; integrator-role principals only.

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
| **P0 (day 0, human + agent, ~15 min)** | Put **U0–U15** to the user (answers recorded in HANDOFF §6 Answer column; entries then deleted from the uncommitted `.claude/decisions-pending.md`); on **U0**, apply the day-0 ruleset with the exact payload in HANDOFF §5 and verify by reading back `GET /repos/{o}/{r}/rules/branches/main` — **never by force-pushing production `main`** (safe with today's publisher: every guarded publish is a fast-forward); the user runs the U4 throwaway-repo test; **the user**, in the dashboard, cancels sentinel 5655e94d and decides dea68719 (deploy per U9, or cancel) — lifecycle is human-only until P1 | — | Rules live; decisions recorded |
| **P1 Unblock (r7: ~550–750 LOC incl. tests, CLI and docs)** | Branch `autonomy/p1` from `main`. §2.2 items 1–6a and **§2.2-P1** (5a moved to P4; 6b credential attributes are P2/P3a): extended `reopen`/`revise` with reason enum + rate limit, digest pin + monotone review set + satisfaction order, roster captured on the intent, `reconcile_required_reviews`, agent unblock/cancel/replace, `ac_amendment`, labelled gates (imports/jobs: label only), ack key `sha256(rules)`, thin CLI `revise`/`unblock`/`cancel`; reopen prose in guidance strings and the 13 book files `grep -rl reopen book/src` finds; deploy preflight | whole-revision staleness | One regression test per fix; staging replay of B1/B2/B8 (lease edit, `review_mode` change, conflict, legacy null ref) via the new CLI subcommands with no human |
| **P2 Containment & staging** | `agentc-impl`/`agentc-rev` uids, **(final)** per-launch clones per role uid (Git isolation), root-owned firewall, pinned binaries, harness profiles, settings preflight, secret-scanning `push-candidate`, Playwright + `verification_env`, staging coordinator (mxmini, loopback, default config) + throwaway project, `ship` script; ~10 probe prompts (<$1, user-approved) for the ⚠ items | full-access unattended defaults | **Containment suite** passes under each exact profile (candidate instructions not loaded, planted Git hooks not executed, raw push denied, out-of-tree writes denied, egress outside allowlist fails); `cargo test` passes under each profile (no LLM) |
| **P3a Shadow (∥ P2)** | read-only host principal (needs §2.2 6b credential attributes), `next` v1, would-launch log with cost estimates | — | 1 day of shadow logs |
| **P4-min Integrator** | §2.4 required items (Sol's five, **(r7) landing-range contributors + `stacked_on_unapproved` (5a), serialize-before-park, integrator↔service API (§2.4b)**, roll-forward, revise-loses-to-push, result-branch protection, privilege gate, bound receipts, one deciding run, reproducible flake attribution, `candidate_reverted_in_history`, `unreviewed_landing`, compare-API token) + §2.4a revert; repo roster/rules read from target; constitution; branch/tag creation limits **first**, then full rulesets A/B + tag ruleset; cutover preflight (zero unresolved intents/holds/reservations/jobs/recovery; no import) + ruleset watchdog; shadow day first | LLM integration commands, intents, reconciliation, journal attestations, global hold, agent `PATCH policy`, `workflow_policies` (after first roster commit) | Staging soak (no LLM): conflicts, target moves, check failures, crashes, reverts resolve with HRI=0; integration p50 < 10 min; **each required check's flip rate < 2%** |
| **P3b Pilot (go/no-go)** | **(final) pilot core only** (Hari): live supervisor (pilot core ≤1.5k lines; `checkpoints.revision` column + CLI support for B5 recovery facts) on oracle-1 (+ mxmini) with hand-pinned harnesses: progress renewal, crash release, service-verifiable recovery, worktree ownership, project `setup`, reviewers + audit sample, health, per-role contract ≤1.5k words, M1 decisions, digest, one end-to-end canary + SLO paging, stall-as-HRI, path-overlap avoidance | "continue" prompts; hand-spawned reviewers; ~10k words of startup prose; worktree-cleanup prose | **5 real tasks**: HRI=0, recorded cost/task, canary green, audit disagreement noted; user go/no-go |
| **P6 Dogfood** | **Before dogfood starts:** admission control (U12), per-vendor/per-host daily canaries, digest-neglect paging, host updater [R-P6]. Then 2 weeks autonomous on this project; [F] items only when measured (batching expected first) | as justified | HRI/task = 0 excluding reserved; value canary not worse than baseline; direct-push canary flat; digest ≤ 1/day |

## 4. User decisions (recommendation first)
- **U0 (r7)** Apply the day-0 ruleset now (`non_fast_forward` + `deletion` on `main`; binds everyone
  including the owner — no force-push to `main`; no bypass). **Recommended: yes.**
- **U1** Windows: non-blocking Actions observer (**recommended**) vs first-class required producer.
- **U2** Human changes: FF pushes of green SHAs via `ship` under rulesets; pre-merge review for all agent
  paths; post-hoc review only for landings with agent trailers or unknown provenance (**recommended**).
- **U3** Billing: API billing for the unattended pool (**recommended**; also enables Claude `--bare`),
  ceiling set after the pilot (estimate $5–30/task; one stuck task cost ≈$50). **(r7)** If subscription
  billing is chosen, P2 must prove another way to keep candidate `CLAUDE.md` from loading, or the pilot
  runs Codex-only.
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
- **U14 (r7)** After P1 is deployed, may the remaining autonomy work run as coordinator tasks
  (dogfooding; **recommended**), or stay outside the workflow as the user originally instructed?
- **U15 (r7)** Commit `planning/autonomy/` on `autonomy-plan` after a redaction pass (**recommended**),
  or keep it local? (The repo is public; `.claude/decisions-pending.md` is never committed.)

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
- **(r7) If the user approves U14**, switch to dogfooding once P1 is deployed to production: P3a/P3b
  items and all follow-ups become coordinator tasks, worked by interactive sessions first and by the
  supervisor from P3b. Until then, AGENTS.md's auto-claim and CONTRIBUTING's "record progress in the
  service" are **overridden** by the user's instruction to work outside the coordinator.
- **(r7/r8) Branch and file hygiene:** `autonomy/p1` branches from `main`; `planning/autonomy/` never
  enters it (stage explicit paths, never `git add -A`); `CARGO_TARGET_DIR` **inside** the worktree, as
  CONTRIBUTING.md requires (git-ignored via `**/target/`); `.claude/decisions-pending.md` is never
  committed.
- **(r7) Gate (exact CI commands; CI pins Rust 1.98.1 and mdBook 0.5.4):** `cargo fmt --all -- --check`;
  `cargo clippy --workspace --all-targets --locked -- -D warnings`; `cargo test --workspace --locked`;
  `cargo build --workspace --locked`; `python3 scripts/smoke.py`; `python3 scripts/backup_smoke.py`;
  for doc changes `python3 scripts/check_docs.py --mdbook "$(command -v mdbook)"`.
- **(r7/r8) Shipping before the `ship` script exists (P2):** with the user's go-ahead, `git push origin
  autonomy/p1`; wait for *Coordination checks* and *Documentation checks* green on that SHA
  (`gh run watch`); fresh-context subagent review + the user's explicit OK; run the preflight (§2.2-P1);
  then `git push origin <sha>:main` (must fast-forward). Production deploy is a U9 decision after the
  preflight runs again.
- **(r8) Staging (never inherit the production token)** — the CLI's origin comes from the repo binding
  (`--repo-config`/`AGENT_COORDINATOR_REPO_CONFIG`, else `.agent-coordinator.toml`), and
  `AGENT_COORDINATOR_ORIGIN` only cross-checks `AGENT_COORDINATOR_TOKEN`, both exported here with the
  production credential (`crates/cli/src/config.rs:126–134`). Use:
  ```
  env -u AGENT_COORDINATOR_TOKEN -u AGENT_COORDINATOR_ORIGIN -u AGENT_COORDINATOR_MCP_TOKEN \
    AGENT_COORDINATOR_HOME=<staging>/home AGENT_COORDINATOR_STATE_DIR=<staging>/state \
    AGENT_COORDINATOR_REPO_CONFIG=<staging>/binding.toml AGENT_COORDINATOR_ALLOW_INSECURE_LOOPBACK=true \
    agent-coordinator …
  ```
  with `binding.toml` containing `service_url = "http://127.0.0.1:<port>"`; the staging credential lives
  where `crates/cli/README.md:40` says for `AGENT_COORDINATOR_HOME` (mode 0600). Staging server:
  `COORDINATOR_DATABASE=<staging>/db COORDINATOR_LISTEN=127.0.0.1:<port> COORDINATOR_ALLOW_INSECURE_LOOPBACK=true`.
  Before the first staging command, `env | grep -c AGENT_COORDINATOR_TOKEN` inside the wrapper must print
  0. Staging Git remote: a local bare repo under `<staging>/remote.git` (as `scripts/completion_smoke.py`
  does); never create GitHub repos. This host (mxmini) runs staging.
- **(r7) LLM spend:** P0–P1 need none beyond the interactive session; U11 first applies in P2.
- **Start execution in a new session** from `HANDOFF-autonomy.md`, not in the planning session.

**First steps for the next session:** follow `HANDOFF-autonomy.md` §5 exactly (it supersedes the
earlier three-step summary; P1 order and rules are in §2.2-P1 here).

## 7. (final) Sign-off and risk register

All three reviewers: **AGREE WITH RESERVATIONS** (round 5); every reservation is applied above (marked
**(final)**). Round 7 rulings: no objections. Round 8 verification: all fixes applied (marked **(r8)**).
Top remaining risks, one per participant:

| # | Risk (owner) | Mitigation in the plan |
|---|---|---|
| K1 | The user stops reading the daily digest; proposed work, decisions and audit findings pile up while HRI reads 0 (Dana) | Digest-neglect and value-canary paging (§2.0.6) |
| K2 | The publication proof relies on ruleset B and the single-producer rule, enforced outside our code; a user who disables B under flake pressure silently voids it (Sol) | Ruleset watchdog freezes the queue and pages (§2.4); flake attribution + <2% flip-rate gate removes the pressure |
| K3 | The autonomy machinery (supervisor, updater, canary, integrator, rulesets, two harnesses on three hosts) becomes the one-person project's new chore (Hari) | Pilot core only (≤1.5k lines, stop-and-rescope); updater, per-host canaries and admission deferred to P6; measured-failure rule for everything [F]; P3b go/no-go includes the user's judgement of operating cost |
| K4 | Scope re-growth (the project's history: 51k LOC before first use) (lead, from Dana throughout) | [R]/[F] tagging; every component names its deletions; phase exit criteria are measured, not feature-complete |
| K5 | Staging commands reaching production (the production token and origin are exported in the executing environment) (Robin, round 8) | §6 staging wrapper unsets both and checks `env | grep -c AGENT_COORDINATOR_TOKEN` = 0 |
