# Plan v2 — Full agent autonomy for Agent Coordinator projects

Status: **revised after discussion rounds 1–2** (see `discussion-transcript.md`). Supersedes
`plan-v1.md`. Participants: Lead, Dana (devil's advocate), Sol (systems & protocol), Hari (harness &
ops). Evidence: `review/{service-data,codebase,history}.md`.

## 0. Goal and success measure

**Goal.** On a project whose human-owned policy grants agents review, recovery, integration
authorization and rule editing, every task that exists in the service reaches a **terminal state**
— `done` (reviewed, integrated into the target branch, validated on the integrated result) or
`canceled`/`superseded` with a recorded agent rationale — **with no human attention**.

**Success measure (attention-based, computable from the event log).**
- A task is *autonomous* iff **every mutation on it came from a supervisor-minted launch credential
  or the integrator principal** (Hari). Interactive-session credentials and dashboard actions are
  tagged human attention. Inside supervised sessions, approvals and "continue" prompts cannot occur
  (`--permission-prompts none`, `approval_policy=never`), so chat attention is covered by
  construction.
- **HRI/task** = human-attention mutations + parked-task notifications per terminal task. Target: 0
  on a 5-task pilot, then 0 across 2 weeks of dogfooding, excluding **reserved decisions** (below).
- **Canaries**: direct human pushes/week (Dana), revise ping-pong count, spend vs budget, review
  latency distribution.

**Out of scope / reserved (explicit, labelled, never silent blockers):**
- Bootstrap: creating projects and policy, enrolling hosts, harness logins, billing.
- Authoring *what* to build (task creation and priority) — humans or agents may do it. This is input,
  not intervention.
- **Constitution changes** (the human-owned floor, §2.5).
- **Releases of the coordinator service itself that change guard code** (Dana, round 2: in practice
  most server releases). Counted as reserved decisions, not as autonomy failures.
- Genuine judgement items (bin (c) below) raised as non-blocking decisions with push notification.

## 1. Diagnosis (unchanged in substance; see plan-v1 §1)

D1 human-only backward transitions · D2 whole-revision staleness · D3 workstation-bound state ·
D4 human-only configuration · D5 no liveness · D6 harness authority ≠ project policy · D7 check
placement and unreproducible environments · D8 instruction surface and diagnosability · D9 bypass
via direct pushes · D10 self-hosting contract churn · D11 human-only acceptance criteria.

**Key reframes from the discussion:**
- Human clicks were mostly *authority*, but not all (Dana): gates are triaged into three bins —
  **(a)** authority-only → agent path; **(b)** information → move into repo/config;
  **(c)** judgement → labelled, non-blocking decision with push notification.
- LLMs should not perform deterministic, safety-critical plumbing. The deterministic integrator also
  removes the harness-classifier conflicts (D6), because no LLM ever merges or approves (Hari).
- The incremental pattern works (3590591f removed human reconciliations). **Evolve, don't rewrite;
  every new component names what it deletes** (Dana).

### Gate triage (initial table)

| Gate today | Evidence | Bin | Disposition in v2 |
|---|---|---|---|
| `workflow/reopen` (stale/conflict/failing/legacy) | 26 human reopens | a | Agent `revise` with a reason enum (P1); stale-policy class deleted by scoped pinning (P1) and repo rules (P4) |
| `unblock` | 6 | a | Agent unblock under `recovery_mode=agent`; `blocked` → `needs_attention{code, required_actor}` later |
| Publication reconciliation | 9 (0 since 3590591f) | a | Deleted by pinned-R roll-forward (P2) |
| Reservation resolve (stuck jobs/holds) | 4 | a / c | Check reservations deleted with GitHub receipts; exclusive resources keep a human gate unless target-side fencing exists (Sol) |
| Required-check roster edit | 3 | b / c | Roster file in the repo read from the target; protected check ids are constitution (c) |
| Resource creation (production-host, runners) | 3–4 blocks | b | Resources declared in repo/host config; runner capacity is local to the runner (no service resource) |
| Task definition edit / cancel / archive | 45 archives, d327fd4c cancel | a / c | Agent edit/cancel with rationale under `agent_rule_editing` (cancel = terminal with rationale); archive is auto after N days |
| Integration authorization | 0 | a | Already automatic |
| Human-only acceptance criteria | b30fe06c, 4b68f1c7, c30653e9 | b / c | AC lint at creation; rewrite as agent-verifiable, or mark a `requires_human_acceptance` step (c) |
| Production deploy authorization | every deploy (chat) | c | Pinned release; agent deploy only with expand-only migrations + smoke on a restored backup; otherwise a decision |
| Windows-only required check | B6 | c | Product decision U1 |
| Harness approvals (push, submit, classifier) | ≥8 + ~700 escalations | a | Deterministic supervised harness profile; integrator does all target pushes |
| "Continue" prompts / idle agents | ≥20; 32 h gaps | a | Supervisor |

## 2. Target design

### 2.1 Components (and what each deletes)

| Component | Replaces / deletes |
|---|---|
| **Supervisor** (per host, ~300 lines) | Human "continue" prompts; lease expiry on turn end; hand-spawned reviewers; cross-host relays; most of the startup prose about continuation |
| **Integrator** (deterministic, non-LLM) | LLM-driven `integrations prepare/publish/reconcile/finish`; publication intents (→ pinned results); agent/human publication reconciliation; journal attestations; the long global hold |
| **GitHub Actions receipts** (bound checks) | Workstation check producers as the required path; check reservations; runner-capability routing (deferred indefinitely) |
| **Repo-held rules and roster** | Agent `PATCH policy` path; `workflow_policies` table; whole-policy-revision staleness |
| **`next` endpoint + per-role contract** | ~10k words of startup prose; duplicated cleanup/continuation/review text; most MCP tools for supervised sessions |

### 2.2 Supervisor (per host)
- Runs as root-owned service launching children as a dedicated **`agentc` uid** (Windows: Scheduled
  Task "run whether user is logged on" under the agent user). The supervisor binary, its config, the
  egress firewall, and the host credential are **not writable/readable by `agentc`** (Dana r2 B4).
- **Host principal** (enrolled once by the human, never visible to a model) can: `next`, claim,
  progress-gated renew/release, and **mint launch tokens**.
- **Launch tokens** (`POST /api/v1/launches`): opaque, hash-stored, principal `launch:<uuid>`, scoped
  to one role + one activity + generation, `expires_at = min(role cap, lease)`, revoked on
  `POST /launches/<id>/finish {exit_reason, usage}`. Same-uid children can read them, so the design
  bounds blast radius rather than hiding the token (Hari).
- **Launch identity** persisted before spawn (`--session-id`, boot id, pid, start time); never respawn
  for an attempt whose launch might be alive (DURABLE-RECORD launch-identity rule).
- **Progress-gated renewal**: renew only while the child is alive **and** within budget **and** has
  emitted a tool event within ≈10–15 min (stream-json / `--json`) **and** its last checkpoint is
  younger than ≈60 min. Service enforces `max_attempt_duration`. On exit/crash/suspend/shutdown:
  checkpoint (push WIP to `refs/agent-coordinator/candidates/<attempt>`) and release immediately.
- **Harness profile (deterministic containment, no classifier)**: Claude `-p --permission-mode dontAsk
  --permission-prompts none --setting-sources user --settings <role file> --strict-mcp-config` with an
  allowlist; Codex `exec --ignore-user-config --ignore-rules -s workspace-write` + network +
  `--add-dir <git-common-dir>` + `approval_policy=never`. Per-role `CLAUDE_CONFIG_DIR`/`CODEX_HOME`.
  **Candidate-controlled `.claude/`, `CLAUDE.md`, `AGENTS.md`, `.codex/` are never auto-loaded**; repo
  instructions are passed as delimited *data* (Hari r2). Raw `git push` denied; pushes go through
  `agent-coordinator push-candidate` (candidate refs only). Exact draft command lines: transcript,
  Hari round 2.
- **Reviewers**: fresh config dir and session per launch, candidate SHA worktree read-only except
  `$RUN/`, **structured output only** (`{decision, findings[], criteria_evidence[]}`); the supervisor
  posts the decision. Independence setting `review_independence = distinct_vendor (default when
  available) | distinct_host | distinct_launch (fallback)`; the applied level is recorded on the
  decision (Dana r2 C).
- **Cost**: the model never waits. The supervisor long-polls (`state_wait`, under Cloudflare's ~100 s)
  and launches short single-task sessions; 429/usage caps ⇒ back off and release. `--max-budget-usd`
  / token caps per role; kill switch; shadow mode.
- **Notifications**: push (ntfy/email) for parked tasks, reserved decisions, incidents.

### 2.3 Integrator (deterministic worker, separate principal)
- Separate uid and credential (GitHub App) that **no LLM child shares**; the only non-human writer to
  `main` (ruleset A bypass).
- **Pinned result.** For approved submission C on observed target T0: compute R once
  (`merge-tree --write-tree` + fixed-identity `commit-tree`), push to branch `ac/results/<id>` (branches
  trigger Actions; the receipt binds the SHA, so the branch name is only a trigger), and record
  `integration_results(submission, T0, C, R, tree)` with **UNIQUE(submission, T0)** *before* checks.
  Later integrators reuse R; they never recompute it.
- **Roll-forward reconciliation** after every fresh observation of tip X:
  - `X == R` or `R ∈ ancestors(X)` → published (the service verifies ancestry itself with one read-only
    compare API call; integrator attestation only for non-GitHub remotes).
  - `X == T0` → push R with `--force-with-lease=T0` if checks and approvals still hold (idempotent
    across concurrent publishers).
  - otherwise → not published; **target moved ⇒ re-integrate onto the new tip automatically**;
    genuine conflict or failing check on R ⇒ `revise` to the author.
  - **Never infer success from a push exit code**; always observe (Sol PoC: `--force-with-lease`
    exits 0 when the remote already equals R even with a stale lease). New DURABLE-RECORD invariant.
- **I-FF invariant** (target tips form a fast-forward chain): enforced by ruleset B (no force push, no
  bypass) and detected by a monitor that **freezes the queue** on `target_rewritten`. Continuous audit:
  every fetch re-checks that recently done tasks' R (or C in the FF case) remain ancestors; misses are
  re-queued.
- **Short push slot** bound to the submission (not the attempt) replaces the global hold across
  checks; policy edits never wait on integration.
- **Checks** = GitHub Actions check-runs on R, accepted only if `app.id` = GitHub Actions,
  `head_sha == R`, and the workflow definition blob equals the one in the **target** (or the protected
  roster), not R; explicit flake policy (bounded retry; a failure on R that also fails on T0 alone is
  not attributed to the candidate). Build cache written only from `main`. A local container runner is
  an optional fast path with the same receipt shape.
- **No-op** (`R == T0`) records done without "published".

### 2.4 Service protocol changes
1. **`revise`** (single CAS on current submission + subject generation): reasons form a closed enum,
   each with required evidence and allowed actors — `conflict{T,C}` and `check_failed{receipt on R}`
   (integrator only), `candidate_missing`, `requirements_changed{task_def_rev}`, `author_withdraw`
   (author only). Refused while a publication may have landed (until roll-forward proves otherwise).
   Rate limit + ping-pong circuit breaker ⇒ park + notify.
2. **Scoped pinning**: a submission pins candidate commit/tree, **task-definition revision** (an AC
   edit re-opens *approvals*, not the candidate), and the **monotone required-review set** (tightening
   adds activities; loosening cancels only queued ones). The roster is read at **push time**; rules,
   lease and flags are read live. Instruction acks key on (contract version, rules revision).
   Decisions re-pend only when their own scope changes.
3. **Agent unblock / cancel / edit task definition** with rationale when policy permits
   (`recovery_mode=agent`, `agent_rule_editing`); `blocked` becomes `needs_attention{code,
   required_actor}` (cleanup phase).
4. **Labelled human gates**: every remaining human-only refusal returns a distinct code and
   `details.required_actor="human"`; `preconditions` and `next` surface them; a **human queue** view
   plus push notification.
5. **`GET /next`** (and MCP `coordinator_next`): server-computed single next action for the caller's
   role and capabilities, with the exact call template.
6. **Additive protocol**: the server accepts the previous client minor version; migrations
   auto-upgrade in-flight records (e.g. import legacy intents as pinned results); client identity is
   the source commit.
7. **AC lint** on creation/edit for autonomous projects.
8. **Launch records** (`launches` table) and the attention metric; heartbeat events compacted.

### 2.5 Rules, roster and the constitution (resolving the circularity)

The user's goal explicitly includes **agents changing binding project rules**, so rule changes must
not become human decisions. Resolution:

- **Constitution (human-owned floor)**, enforced *outside* anything agents can change or deploy:
  - GitHub ruleset A (`update` + `deletion`; bypass = integrator App + owner) and ruleset B
    (`non_fast_forward` + `required_status_checks` pinned to the Actions `integration_id`; **no
    bypass**).
  - Service-side human-only policy: minimum review set (`review_mode` floor), **protected check ids**,
    **protected paths** (`.github/workflows/**` jobs that implement protected checks, the constitution
    path list, CODEOWNERS), recovery floor, autonomy switches, deployment mode.
  - Production coordinator pinned to a release; promotion of guard-changing releases is a reserved
    decision.
- **Rules and roster above the floor** live in repo files (`.agent-coordinator/rules.md`,
  `.agent-coordinator/roster.toml`), read from the **target** branch, changed only through the normal
  review → integrate path. They are **agent-editable autonomously**, but a change to them (or to any
  non-protected CI file) requires **elevated review**: two independent reviewers, at least one
  `distinct_vendor`, plus a **non-blocking post-hoc notification** to the human with a one-click revert
  task. Rules are passed to agents as delimited *data*, never as system prompt.
- **Protected paths/check ids** cannot be changed by agents at all (bin (c) decision). This is the
  honest cost Dana named; it is confined to the constitution rather than to all rule edits.
- **No self-benefit**: a rules/roster revision never applies to a submission whose contributors
  include that revision's author or approver.

### 2.6 Human day-to-day work (U2)
No `quick` lane. Humans may push `main` directly **only as fast-forwards of a SHA with green required
checks** (ruleset B has no bypass): a ~20-line `ship` script pushes to `ac/human/<name>`, waits for
Actions, then fast-forwards `main`. The integrator recomputes on target move. Direct pushes are a
tracked canary.

### 2.7 Deployment (coordinator itself)
Production is pinned to a release; staging dogfoods `main`. Expand-only migrations,
backup-before-migrate, and a smoke test of the new binary against a restored copy of the latest
backup. Releases without guard changes and with expand-only migrations may be agent-deployed if
`deployment_mode=agent`; others are reserved decisions. Break-glass manual deploy path kept.

### 2.8 Instruction surface
Per-role contract ≤1,500 words served by the service ("loop on `next`; here is what each action and
each labelled refusal means"). Delete duplicated prose. MCP shrinks to ~10 `next`-centric tools for
interactive or foreign harnesses; supervised sessions use the CLI with a launch token. Book split into
Agent / Operator / Design-history; fix stale "foundation" statements; add the `book.toml`
`additional-js` hook for external links.

## 3. Delivery phases (each independently shippable; every phase lists its deletions)

| Phase | Content | Deletes | Exit criterion |
|---|---|---|---|
| **P1 Unblock (1–2 days, ~450–650 LOC)** | Scoped pinning (task-def rev + review set; roster at push time); `revise` with reason enum + Sol's unresolved-intent guard + rate limit; agent unblock/cancel; labelled human gates; ack keyed on rules rev; decisions re-pend by scope; docs "operator reopen" prose | Whole-revision staleness; 6 prose instances | Regression tests per fix; no human reopen needed for staleness/conflict/legacy in staging |
| **P2 Containment & staging** | `agentc` uid + firewall; per-role harness profiles; staging coordinator + throwaway project; rulesets A/B on the repo (after the user's 10-minute ruleset test); `ship` script | Direct unchecked pushes; full-access harness defaults for unattended work | Rulesets verified; harness profiles pass a dry run with no prompts |
| **P3 Supervisor + launch tokens** | `launches` table/endpoints; host principal; supervisor with progress-gated renewal, crash release, watchdog, shadow mode, kill switch, notifications; `next` v1 | Human "continue" prompts; hand-spawned reviewers | Shadow mode for 1 day; **costed 5-task pilot (go/no-go gate)** with HRI=0 |
| **P4 Integrator + GitHub receipts + repo roster/rules** | Pinned results with UNIQUE(submission,T0); roll-forward; I-FF monitor + audit; push slot; Actions receipts bound per Sol (a)–(c); roster/rules files read from target; constitution in service; legacy-intent import at cutover | LLM-driven integration commands; publication intents/reconciliation; journal attestations; global hold; agent PATCH policy; `workflow_policies` | Staging soak: conflicts, target moves, check failures, crashes all resolve with HRI=0 |
| **P5 Contract & docs** | Per-role contract ≤1.5k words; MCP slimmed; book restructure; link hook | ~10k words of startup prose; ~50 MCP tools | Cold start to first claim in <3 min with only the contract |
| **P6 Dogfood + cleanup** | 2 weeks autonomous on this project; measured cleanup (check reservations, `mode=recovery`, review kinds, `blocked`); AC lint; deployment mode | Items justified by measured failures | HRI/task = 0 excluding reserved decisions; direct-push canary flat |

## 4. User decisions (with recommendations)

- **U1 Windows**: first-class required producer, or non-blocking Actions observer? *Recommend observer.*
- **U2 Human changes**: fast-forward pushes of green SHAs under rulesets (no `quick` lane), with
  pre-merge review for all agent paths. *Recommend yes.*
- **U3 Billing**: API billing + monthly ceiling for the unattended pool (subscription caps and ToS
  unverified). *Recommend API billing; set the ceiling after the pilot (estimate $5–30/task).*
- **U4 Org transfer**: *Recommend no*, unless the 10-minute ruleset test on a throwaway repo fails.
- **U5 Cross-vendor review**: *Recommend `distinct_vendor` default when available.*
- **U6 Always-on hosts**: which hosts run supervisors/integrator (oracle-1 always on? MINIAIR sleeps)?
- **U7 GitHub dependency**: accept GitHub Actions receipts as the required default (depends on
  GitHub availability and the repo staying public for free minutes). *Recommend yes; keep the
  native-producer receipt type as an escape hatch.*
- **U8 Rule edits**: autonomous with elevated review + post-hoc notification (above the constitution),
  vs. every rule edit as a human decision. *Recommend autonomous (it is the stated goal).*
- **U9 Coordinator deployment**: `deployment_mode=agent` for non-guard, expand-only releases, or all
  deploys as decisions? *Recommend decisions until P6 data exists.*

## 5. Open questions (carried to round 3)
- Q-A: Replay the B1–B15 catalog (service-data.md §3) against v2: does anything still need a human?
- Q-B: Exclusive resources (production host): which can enforce fencing tokens?
- Q-C: Migration of live in-flight state (intents, holds, reservations, `recovery_required`) at P1 and
  P4 cutovers.
- Q-D: Where does the integrator run (U6), and what happens when it is down?
