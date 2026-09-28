# HANDOFF — Full agent autonomy for Agent Coordinator (planning complete, execution not started)

Written 2026-09-25 by the planning session (Claude Opus 5.5, lead) at the end of a review and
multi-agent planning discussion. **Read this file first**; it is self-contained enough to start
execution, and links everything else.

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

**Out-of-band service change on `autonomy-plan` (2026-09-28):** this branch now carries an optional TypeSafe context-reranking prototype, recorded in §11 below. It has not been incorporated into `main` or deployed. Before the next product release, decide whether to cherry-pick the commit onto the current product branch or revert it here. The production VM has a nonempty `TYPESAFE_API_KEY` entry in its protected service environment file, but its installed binary has not been changed by this session. Do not assume the model path has been exercised.

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
| U4 | Move repo to a GitHub org | No, unless the ruleset test fails || 2026-09-25: **no**, unless the ruleset test fails |
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

### Next session: resume here (items 1–3 are P2 history; start at item 4)

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
      **S1 done 2026-09-28 (`65e4f58`); S2 done 2026-09-28 (`1e6b695`), see §11. Next session
      starts at S3** (`crates/integrator`: config with in-code defaults, App auth + askpass, mirror
      and per-result worktrees reusing `coordinator_local::git_workflow`, loop steps 1–3 and 5–7
      against a `ChecksSource` trait with a local fake; crate tests against a local bare remote).
      S3's live test needs p4-design §5 user steps 2 (U4 ruleset test) and 3 (GitHub App).
      S2 as specified was:
      push-authority (digest pin, deciding success receipts per roster check with `workflow_blob`
      from T0, 5a contributors, `stacked_on_unapproved`, submission-bound hold via
      `integration_holds.submission_id`), observations (published / not_published / target_moved,
      revise-loses-to-push), refusal `integration_owned_by_integrator` on the LLM integration routes,
      and `next?role=integrator`. New tests go at the end of `crates/server/tests/workflow.rs`
      (helpers `integrator_caller`, `patch_owner`, `result_body`). Smoke needs a clean committed
      build (`COORDINATOR_BUILD_COMMIT=$(git rev-parse HEAD)`).
   e. **Future deploys** (U9, agent-run): preflight → `gh workflow run release.yml --ref main
      -f binaries_only_since=<production commit>` when no migration changed (else omit the input) →
      download + `sha256sum --check` → `gcloud compute scp` to `~/release-<sha>` → `deploy-pre.sh` →
      preflight again → `deploy-swap.sh` → check `/api/v1/info` `build.source_commit` →
      `upgrade_client.py --source-root <clean worktree at that commit>`. Update `REL=` in both host
      scripts and the user's exact-command permission rules for a new `git push origin <sha>:main`.
      The classifier refused a stdin-redirected preflight under the broad
      `gcloud compute ssh agent-coordinator:*` rule; exact-command rules worked.
