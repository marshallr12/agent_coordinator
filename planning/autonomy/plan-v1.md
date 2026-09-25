# Plan v1 — Full agent autonomy for Agent Coordinator projects

Author: lead (Claude Opus 5.5), 2026-09-25. Status: **draft for group review**.
Evidence base: `planning/autonomy/review/{service-data,codebase,history}.md`.

## 0. Goal, and what "100% autonomy" means

**Goal.** On a project whose policy grants agents the four authorities — required review
(`review_mode` agent/either + `allow_subagent_reviews`), expired-work recovery
(`recovery_mode=agent`), integration authorization (`automatic_integration`), and binding-rule
changes (`agent_rule_editing`) — every task created in the service reaches **done** (reviewed,
integrated into the target branch, validated on the integrated result) with **zero human actions**,
under every *ordinary* failure mode.

**Proposed operational definition (to be challenged):**

- *In scope (must be zero-human):* claim, implement, submit, review, revise, conflict resolution,
  check failures, crashed/idle agents, expired leases, lost workstations, policy/rule edits,
  target-branch movement, publication uncertainty, resource contention, continuation between tasks,
  cross-workstation handoff, deployment of the coordinator itself when the project's policy allows.
- *Out of scope (irreducible human bootstrap):* creating the project and its policy, issuing the
  first credentials, provisioning hosts/harness logins/billing, answering decisions that the human
  has explicitly reserved (`required_actor=human`), and choosing to *withhold* an authority.
- *Metric:* **human-required interventions per completed task (HRI/task)** on autonomous projects,
  derived from the event log. Target: 0 over a fault-injected soak of ≥50 tasks, and 0 over two
  consecutive weeks of real dogfooding. Every remaining human gate must be machine-labelled and
  visible in a "human queue", so a non-zero value is always explainable.

## 1. Diagnosis (condensed from the three reviews)

The four policy switches were set on day one (09-09) and have been on since 09-15/23. **The policy
was never the blocker.** What stopped agents:

| # | Root cause | Evidence (this project, all switches on) |
|---|---|---|
| D1 | **Human-only backward transitions.** `workflow/reopen` (`workflow.rs:1117`) is the only path from stale/conflicting/failing candidate to `revision_needed`; `unblock` is operator-only. | 26 human reopens, 6 human unblocks; fd3df7bf: 4 agent integration claims rediscovered one conflict over 3 h, waiting for a human click. |
| D2 | **Over-broad staleness.** Submissions pin the whole `project_policy_revision` + `workflow_policy_revision`; any edit (even `lease_seconds`) invalidates all in-flight work, and agent rule edits therefore undo themselves. | 16 of 26 reopens were pure policy staleness (2 were lease-length only). |
| D3 | **Workstation-bound state.** Publication intents recorded before checks; agent reconciliation needs a local journal file; candidates/worktrees/jobs local to one host. | 9 human reconciliations (0 agent before 09-23); 38f3ffe4 took 9 attempts on 3 hosts; 31.5 h stall on an already-published integration. |
| D4 | **Human-only configuration.** Required-check roster, resources, task definitions after contribution, lifecycle, deployment authority. | 3 roster edits, 4 resource blocks, 45 human archives, every prod deploy authorized in chat. |
| D5 | **No liveness.** Agents exist only while a human keeps a harness turn alive. Leases expire at turn end; nothing wakes an agent; nothing picks up expired work. | ≥20 "continue/why have you stopped" prompts; agent-silent gaps of 32 h / 24 h / 18 h; P0 tasks waited 40–144 h. |
| D6 | **Harness authority ≠ project policy.** Push/submission approvals, sandbox escalations (~700 on 09-17), Claude auto-mode blocking `integrations prepare` as merge-without-review and subagent review as self-approval. | B7 in service report; 5 hand-typed "spawn a reviewer" prompts. |
| D7 | **Check placement without routing; unreproducible environments.** | 131/360 jobs failed (36%); Windows-only required check stranded integrations until a human edited the roster. |
| D8 | **Instruction surface & diagnosability.** ~10–11k words before first claim, contradictions (MCP "no CLI needed" vs code needs CLI; "never stop" vs "stop without journal"; "never change policy" vs rule editing), one generic `operation_not_permitted`. | ~41 of 256 commits are guidance/clarification; startup guide 179→591 lines in 10 days. |
| D9 | **Bypass.** Direct pushes to `main` break required checks and move targets; workflow too heavy for small changes, so the user routes around it. | 9bb9a28 broke `documentation` for every candidate; 6 "do not create a task" prompts. |
| D10 | **Self-hosting & contract churn.** Schema changes strand in-flight submissions; deployment needs a human. | 2 legacy-ref reopens; `candidate_remote_mismatch` from same-version old CLIs. |
| D11 | **Human-only acceptance criteria** accepted into autonomous projects. | b30fe06c, 4b68f1c7, c30653e9. |

Two structural observations drive the plan:

1. **Every human click added authority, not information.** Agents did the diagnosis and filled the
   dialog; the human pressed save. Safety came from *evidence*, not from the human.
2. **LLM agents are being asked to perform deterministic, safety-critical plumbing** (holding a
   global integration lock across a check run, CAS-pushing, reconciling journals) inside harnesses
   that end turns, sandbox pushes and treat merges as dangerous. The plumbing should be done by a
   deterministic non-LLM component; LLM agents should only produce and judge changes.

## 2. Target architecture ("autonomy kernel")

Keep the service (Rust/Axum/SQLite), its receipts/idempotency, immutable submissions, durable
candidate refs, deterministic merge, CAS push, exact check receipts, and contributor-based
independence. Restructure around five components:

```
            ┌──────────────── Coordinator service (authority, state, policy) ────────────────┐
            │  tasks · work items · reviews · merge queue · leases · policy · human queue    │
            └──────▲───────────────▲──────────────────▲───────────────────▲─────────────────┘
                   │ next/claim    │ review           │ queue jobs         │ check jobs
        ┌──────────┴───┐   ┌───────┴──────┐   ┌───────┴────────┐   ┌──────┴───────────┐
        │ Implementer  │   │ Reviewer     │   │ Integrator     │   │ Check runner(s)  │
        │ LLM session  │   │ LLM session  │   │ (deterministic │   │ (deterministic,  │
        │ (harness)    │   │ (harness,    │   │  worker, no    │   │  capability-     │
        │              │   │  other ident)│   │  LLM)          │   │  tagged hosts)   │
        └──────▲───────┘   └──────▲───────┘   └───────▲────────┘   └──────▲───────────┘
               └─────── Workstation supervisor daemon (launch, heartbeat, restart) ───┘
```

### 2.1 Supervisor daemon (`agent-coordinator supervise`) — fixes D5, D6, parts of D3/D7
- Long-running per-workstation process (systemd user unit / Windows service), configured with a
  capability profile (OS, toolchains, resources it can satisfy) and a **roster of credentials by
  role** (implementer, reviewer; optionally one per vendor).
- Long-polls the service (`state_wait`) for eligible work matching its capabilities and budget, then
  **launches a headless harness session** (`claude -p …`, `codex exec …`) with a prepared worktree,
  a role-specific short prompt, and a vetted harness permission profile (network allowlist for the
  service and Git remote, writable worktree, push allowed only to `refs/agent-coordinator/*`).
- **Owns lease heartbeats** while the child process lives (leases no longer depend on LLM turns);
  on child exit/crash it checkpoints (pushes WIP ref) and releases or marks for recovery immediately
  → recovery latency drops from ≤1 h to seconds.
- Enforces concurrency, token/cost budgets and quiet hours from policy.
- Independence: reviewers are **separate processes with a separate credential and a fresh context**,
  so review is independent in substance (satisfies harness self-approval heuristics) and the service
  can require `reviewer_principal != author_principal`.
- The service still never executes anything (CONTRIBUTING: no remote execution): supervisors *pull*.

### 2.2 Integrator (merge queue) — fixes D1 (conflicts/check failures), D3, D6, D9
- A deterministic worker role (runs inside the supervisor on any host with Git + remote write
  access; any number may exist; the service serializes by lease). No LLM.
- Pipeline per approved submission: fetch target → deterministic merge (existing
  `merge-tree`/`commit-tree`) → push result to `refs/agent-coordinator/results/<id>` → enqueue
  required checks (current roster) on capable runners → on all-green, CAS-push target from the
  observed base to the result → record. Target moved ⇒ recompute automatically. Publication
  uncertainty is resolved by the integrator by observing the remote (deterministic result SHA ⇒
  "published iff target contains result"), so **publication reconciliation disappears as a human or
  LLM concern**.
- Outcomes route automatically: conflict or failing check on the integrated result ⇒ subject goes to
  `revision_needed` with the conflict/check evidence attached and a new implementation work item is
  queued (prefer original author identity, any eligible agent after a timeout).
- Batching/speculative queue (optional later): test N candidates together, bisect on failure.
- Target branch protection: only the integrator identity may push the target branch (GitHub ruleset
  or equivalent); humans use a fast-lane task (2.6). Removes D9's direct-push breakage.

### 2.3 Check runners with capability routing — fixes D7
- Roster entries declare `requires: {os, arch, tools…}`; runners advertise capabilities; the service
  routes each check job to a capable runner; results are exact receipts (existing format).
- Checks run in declared reproducible environments (containerised on Linux; pinned toolchain
  manifest on Windows) to kill "link.exe resolved to coreutils"-class failures.
- Flaky-check policy: bounded automatic retry; a check that fails on the candidate and passes on the
  target alone is attributed to the candidate.

### 2.4 Protocol changes in the service — fixes D1, D2, D4, D8, D10
1. **`revise` transition (agent-callable).** Any eligible agent (author, reviewer, integrator) may
   move a non-published submission to `revision_needed` with structured evidence when
   `recovery_mode=agent`. Replaces human reopen for stale, conflicting, failing, legacy candidates.
   Human `reopen` remains for manual projects.
2. **Scoped pinning, never whole-revision.** Review phase pins only `review_mode`; integration uses
   the **current** roster/policy at integration time. Policy/rule edits never invalidate in-flight
   work; if `review_mode` tightens, missing review activities are added automatically.
3. **Agent unblock** (with reason, recorded) when policy permits; `blocked` releases become
   "needs-attention" items that agents may pick up.
4. **Delegated configuration** under a new policy switch (`agent_config_editing`, or folded into
   `agent_rule_editing`): resources, required-check roster, task definitions/lifecycle
   (edit, cancel, archive). Guardrails: agents can never widen their own authorities, disable
   review, or remove *all* checks; every change is an audited revision with provenance.
5. **Restrict `agent_rule_editing` to the `rules` text** (fixes the `review_mode:none` gap) and
   make rules edits non-invalidating.
6. **Machine-labelled human gates.** Every remaining human-only refusal gets a distinct code and
   `details.required_actor="human"`; `preconditions`/`next` surface them; a **Human queue** view lists
   them with age. Autonomy metric (HRI/task) computed from events and shown on the dashboard.
7. **`GET /next` (and MCP `coordinator_next`)**: server-computed single next action for the caller's
   role and capabilities (claim X, review Y, revise Z, wait until T, nothing eligible), with the exact
   call template. The agent contract becomes "loop on `next`".
8. **Additive, versioned protocol.** Server accepts older clients for one minor version; migrations
   auto-upgrade in-flight records (e.g. backfill candidate refs by fetching); client identity is the
   source commit, not the semver string.
9. **Acceptance-criteria lint.** At task creation/edit on autonomous projects, criteria requiring a
   human (read, paste, look at) are flagged; the task must either be rewritten into agent-verifiable
   criteria (headless browser screenshots, DOM assertions, clipboard integration tests) or be
   explicitly marked `requires_human_acceptance`, which routes only that step to the human queue.

### 2.5 Deployment as an autonomous, policy-controlled operation — fixes D10, B15
- New policy switch `deployment_mode: human|agent`, and a `deploy` check type bound to a
  `production-host` resource/capability.
- The integrator can trigger a deploy job after integration on projects with continuous deployment;
  deploys are blue/green with health checks and **automatic rollback**; the running build identity is
  recorded in the service; decisions that are genuinely human are raised as service decisions, not
  chat.

### 2.6 Fast lane for small changes — fixes D9
- Task kind/label `quick` (doc/UI-only paths per policy glob): single agent review, integrator,
  subset of checks declared by policy. Humans and interactive sessions create a quick task via one
  command (`agent-coordinator quick "<title>"` from a dirty worktree ⇒ candidate ref ⇒ submission) so
  no one needs to push to `main` directly.

### 2.7 Instruction surface
- One agent contract (≤1,500 words) per role, served by the service, stating: loop on `next`; what
  each action means; what to do on each labelled refusal. Duplicated prose (cleanup ×3, continuation
  ×3, review-first ×3) deleted. MCP and CLI capability matrix stated truthfully in one table.
- Book reorganised into **Agent / Operator / Design history**; stale "foundation" statements fixed;
  `book.toml` `additional-js` hook for external links (user's global rule).

## 3. Delivery plan (phases; each independently shippable and gated)

| Phase | Content | Exit criterion |
|---|---|---|
| **P0 Autonomy harness & metric** | Event classifier (human vs agent, required vs optional), HRI/task metric + human-queue read model; a **fault-injection soak rig**: local service + scripted fake agents (no LLM) that drive tasks through conflicts, crashes, lease expiry, policy edits, target moves, check failures, host loss. Baseline run shows current human gates. | Rig reproduces D1–D4 failures deterministically in CI. |
| **P1 Remove dead ends in the service** | `revise`; scoped pinning; agent unblock; restricted rule editing; delegated config switch; labelled human gates; `next` endpoint (v1). | Soak rig: zero human gates for conflicts, check failures, policy/rule edits, legacy refs. |
| **P2 Integrator** | Deterministic merge-queue worker; results refs; current-roster checks; automatic routing of conflicts/failures to `revision_needed`; remote-observation reconciliation; target-branch protection. Delete agent-held holds, publication intents, agent reconciliation paths from agent contract. | Soak: zero human reconciliations; integration p50 < 10 min on 3-check roster. |
| **P3 Supervisor + runners** | Daemon, capability profiles, headless harness launchers (Claude Code, Codex), heartbeat ownership, crash release, role credentials, reviewer independence by principal, check routing, reproducible envs. | Soak with real LLM sessions on 2 hosts: ≥20 tasks, zero "continue" prompts, recovery < 2 min. |
| **P4 Contract & docs** | Per-role contract ≤1.5k words; `next` everywhere; book restructure; link hook; delete superseded guidance. | New-harness cold start to first claim in < 3 min with only the contract. |
| **P5 Deployment autonomy + fast lane + AC lint** | `deployment_mode`, deploy checks, rollback; `quick` lane; AC lint. | Coordinator deploys itself after integration with auto-rollback test. |
| **P6 Dogfood & cut-over** | Run this project fully autonomously for 2 weeks; remove dead code paths. | HRI/task = 0 excluding reserved decisions; human queue empty or explained. |

## 4. Risks and open questions (for the group)

1. Is "rewrite integration as a deterministic worker" better than keeping LLM-driven integration and
   patching its human gates? (Cost: large change to a live system; benefit: removes the most fragile
   class entirely.)
2. Should the integrator and check routing delegate to **GitHub** (PR + rulesets + merge queue +
   Actions) instead of a home-grown queue? Original goal was vendor-agnostic; the repo is on GitHub.
3. Safety with no human in the loop: what stops a confidently-wrong agent + same-vendor reviewer from
   shipping a bad change to `main` and production? (Proposed: required checks on the integrated
   result, principal-distinct (ideally cross-vendor) review, auto-rollback, human-visible audit and
   one-click revert; rules the agents can't weaken.)
4. Cost/runaway: supervisors launching LLM sessions continuously — budgets, quiet hours, circuit
   breakers when HRI or failure rates spike.
5. Is the supervisor "remote execution by the service"? (Claim: no — it is a pull-based local
   daemon configured by the workstation owner.)
6. Does "agents may edit roster/resources" defeat the purpose of required checks? Where is the line?
7. Rewrite vs. evolve: new repo/service vs. staged changes in this one.
8. How to handle review-quality risk (44/75 reviews < 5 min; 60/71 same principal).
