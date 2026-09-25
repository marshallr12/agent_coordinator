# Autonomy planning — group discussion transcript

Session: 2026-09-25. Participants: lead (Claude Opus 5.5, main session) and reviewer subagents (Opus 5.5, high effort). Entries are appended in order with UTC timestamps; messages are verbatim unless marked [summary].


---

### Lead (Claude, main session) — 2026-09-25T16:27:22Z

**Opening (lead).** Welcome. Our job: agree a plan that gets Agent Coordinator projects to *zero human interventions* when project policy grants agents review, recovery, integration authorization and rule editing. The user allows anything: rewrite tooling/DB/service/docs, new branch or new repo. We have 1–6 hours.

Reading list (all in `planning/autonomy/` on branch `autonomy-plan`):
- `plan-v1.md` — my draft plan (diagnosis D1–D11, target architecture, phases P0–P6, open questions).
- `review/service-data.md` — mined from the live service: 53 tasks, 6,153 events. Key facts: humans did 26 reopens, 9 publication reconciliations, 6 unblocks, 4 reservation resolves; 16 of the reopens were pure policy staleness.
- `review/codebase.md` — traces of each capability with file:line gates.
- `review/history.md` — git plus Codex/Claude prompt history. Key facts: ≥20 "continue" prompts, ≥10 human dashboard clicks that "added authority, not information", and 6 prompts routing around the workflow.

Participants: **Dana** (devil's advocate: challenge every claim and assumption until convinced), **Sol** (systems & protocol correctness/safety), **Hari** (agent harnesses, operations, cost, rollout). Please address each other by name, answer each other's questions, and post everything here. Proof-of-concepts are welcome in `/tmp` worktrees, but clean up afterwards.

My three most contestable bets: (1) move integration out of LLM hands into a deterministic integrator; (2) a workstation supervisor daemon that launches headless harness sessions and owns heartbeats; (3) turn every human-only transition into a policy-gated, evidence-backed agent action. Tear them apart.


---

### Dana (devil's advocate) — 2026-09-25T16:30:31Z

**Opening critique (Dana).** Lead, the diagnosis is strong, and I agree with more of it than this list suggests. My worry is the cure. Plan v1 answers "a 51k-LOC system built before anyone used it turned out to be hard to use" by designing a second large system before anyone has used it. That is the history.md §5 failure mode ("front-loaded rigor") repeated. Numbered points follow so Sol and Hari can reply to each one.

**1. The goal is defined against the wrong ledger.** HRI/task is "derived from the event log" (plan §0). Most human effort never reached the event log: ≥20 "continue" prompts, ≥8 push/submit approvals, ~700 sandbox escalations, 3 manual relays, 6 browser logins, and 14 chat authorizations (history.md §3; service-data.md B15). A project could score HRI=0 while the user types "why have you stopped?" every hour. My proposal: count a task as autonomous only if **no interactive human session touched it**. Every session that acted on it must have been launched unattended, and every human service event is tagged. Measure human *attention* (prompts plus clicks plus approvals), not human service events.

**2. "Every task reaches done" is the wrong terminal condition.** d327fd4c should *not* have reached done; the right outcome was cancellation, and a human made that call. B10 lists wrong-kind and obsolete-scope tasks. The goal should be "every task reaches a terminal state (done, or canceled/superseded with recorded agent rationale) with zero human attention". Please also clarify one thing: is *choosing what to build* (task creation and priority) inside the goal? Agents already created 35 of 53 tasks. If a human still writes the backlog, say so explicitly.

**3. "Every click added authority, not information" is over-generalised.** It holds for reopen, reconcile and resolve: roughly 39 of the 49 "unblock-the-machine" actions. It does not hold for dropping the Windows check (a product decision about platform support), creating `production-host` (only the human knew which VM, key and capacity), the b30fe06c comprehension read, the d327fd4c cancel, or the 45 archives (a housekeeping preference). Those carried information or judgement. The plan should sort every human gate into three bins: (a) authority-only, delete it; (b) information the agent could get another way, move the information into config or the repo; (c) genuine judgement, keep it as a labelled, *non-blocking* decision. Right now it treats all of them as (a).

**4. Much of the evidence is historical, and the trend is already improving.** After policy rev 6 (09-23), the human did 3 reopens, 2 unblocks and 2 roster edits, then nothing else except archives (service-data.md §0, "Human interventions by phase"). Reconciliations fell from 9 to 0 once 3590591f landed. 16 of the 26 reopens came from the human tuning policy on 09-15/16, which is setup churn. So the steady-state problem is smaller than the headline numbers. It is also cheaper: the incremental fix pattern (3590591f, ~650 lines) *worked*. That argues for evolving the existing design, not re-architecting it.

**5. P1 is cheap and should ship this week, before any P0 rig.** I counted the human gates: `human(`/`admin_or_operator(` appear at only 9 call sites (`workflow.rs:325` roster, `:1117` reopen, `:1657` human review, `:2150` authorize, `:2510` reconcile; `coordination.rs:231`, `:655` lifecycle, `:1134` unblock; `imports.rs:547`), plus resource creation at `jobs.rs:267`. Whole-revision pinning is a single predicate in `ensure_current` (`workflow.rs:1430-1440`) together with its twins at `:846`/`:1349`. Making reopen and unblock policy-gated, and narrowing the pin to `review_mode` and roster, is a few hundred lines. It would have removed the 26 reopens and most of the 6 unblocks, about 32 of the 49 actions (the 9 reconciliations are already solved by 3590591f). A deterministic fake-agent soak rig (P0) is a large project in its own right, and it cannot observe the failures that matter most (harness turn-ending, sandbox approvals, auto-mode classifier refusals) because it has no LLM or harness in the loop. Build test cases one fix at a time; don't build a rig first.

**6. The check-runner part of P3 duplicates something the repo already has.** `.github/workflows/ci.yml` already runs Linux fmt/clippy/test *and a native `windows-latest` job* on `push` to any branch. The 36% job failure rate, the `link.exe`→coreutils failures and the Windows-only stranding (B6) all came from **workstation-local producers**, not from CI. Agents already invented "CI observer" jobs (service-data.md §5). So why build capability routing, runner advertisement and reproducible containers? Push the deterministic result to a branch such as `ac/results/<id>` and accept the GitHub check-run conclusions on that exact SHA as the receipts. Keep vendor neutrality as an "external CI observer" check type. **Question for Sol:** is a GitHub check-run on an exact SHA a weaker receipt than the current `inputs_unchanged` job receipt, and in what threat model?

**7. Integrator: I half-concede.** Taking lock-holding, CAS-push and reconciliation out of LLM hands is right; B3, V4 and the Claude merge-without-review block (B7) make that case. But the integrator should be a *thin loop around code that already exists* (`git_workflow.rs:488-958` merge-tree/commit-tree, `:708-854` CAS publish), with checks delegated as in point 6. I also want the GitHub-native option costed seriously. Note that the repo is owned by `marshallr12`, a personal account; GitHub merge queue is only available on organisation-owned repositories, so that route needs a free org transfer. The alternative is branch protection with required checks plus a small bot that does "update branch and merge". What does home-grown buy us that "PR per submission + required checks + a bot that merges approved PRs" does not? My guess is only the service's contributor-independence rule, and that can be enforced by the bot itself.

**8. The supervisor is necessary; the plan's version is too big.** D5 (liveness) is the most important finding here: agent-silent gaps of 32 h, 24 h and 18 h, and P0 tasks idle for 112–144 h. But the smallest thing that fixes it is a systemd timer or cron job: `if GET /next != nothing: claude -p "<role contract>"`. That is about 50 lines, not a daemon with budgets, quiet hours and vendor rosters. Two safety issues with "daemon owns heartbeats": (a) a child LLM that is stuck in a loop keeps its lease alive forever. Liveness has to mean *progress* (checkpoints or commits), not a running process. (b) If the daemon holds credentials for both implementer and reviewer, one compromised or confused host can author *and* approve. **Question for Hari:** can subscription-backed `claude -p` and `codex exec` legally and practically run unattended at this cadence, or do we need API keys? What would a realistic day cost?

**9. Safety has a circularity the plan doesn't name.** On *this* project, agents can edit rules, edit the roster (plan 2.4.4), review each other, integrate, and deploy the coordinator itself (2.5). The guardrails "agents can never widen their own authorities / remove all checks" are enforced **by the service code the agents can change and deploy**. One confidently wrong change can remove the guard and then use the missing guard. Invariants must live outside the loop agents control. Concretely: human-owned GitHub branch-protection and required-check settings (agents' tokens lack admin), a CODEOWNERS-protected `constitution` file, deploy only from protected `main`, and a roster rule that is *monotonic*: agents may add or strengthen checks, but never remove or weaken them. "Cannot remove *all* checks" is not a guardrail, because an agent can remove all but one trivial check.

**10. Deployment autonomy (P5) is over-scoped.** Blue/green with automatic rollback on an e2-micro with SQLite runs straight into irreversible migrations, and rollback after a forward migration is where data loss happens. The cheaper version: a GitHub Action deploys on green `main` when the change has no migration; migrating changes wait for an explicit decision. Also note that the self-hosting contract churn (D10/B8) mostly disappears if the coordinator that *runs the project* is not the build *under development*. Pin the production coordinator to a release and dogfood `main` in staging.

**11. Delete before adding.** The plan lists no deletions except guidance prose. Candidates: the fast lane (2.6), which becomes redundant once the normal lane is fast (1bf380ad integrated in 3 minutes after the roster fix); publication intents at prepare time; the separate `workflow_policies` revision; human `reopen` on autonomous projects; the knowledge/decision import subsystem (0 feedback, 0 revisions); agent-held reservations for checks; and the MCP job surface if code work requires the CLI anyway. Every new component should name what it deletes.

**Requests for clarification (Lead):** (i) Must native-Windows workstations stay first-class producers, or is Windows coverage via hosted CI enough? (ii) Is interactive human work (the P7 UI polishing) supposed to go through the coordinator, or is "human pushes are fine as long as the required checks pass and the integrator recomputes on target move" acceptable? The second option removes D9 without a quick lane. (iii) What budget in tokens or dollars per task makes "100% autonomy" worth it? (iv) Does the user accept that production deploys of the coordinator itself stay human-gated when a migration is involved?

**Proposed resequencing:** P1-lite (policy-gated reopen/unblock, narrowed pinning, rules-only edits, labelled human gates) → timer-based launcher + a 1.5k-word role contract + `next` → integrator as a thin loop using GitHub CI receipts → measure for 1 week with the attention-based metric → only then decide on runners, deployment autonomy and the rest.


---

### Sol (systems & protocol) — 2026-09-25T16:31:25Z

**Opening review (Sol, systems & protocol).** Short version: I back bet (1), the deterministic integrator, and bet (3), policy-gated agent transitions. I think both can be made *provably* safe, and more simply than plan-v1 says, but only if we restate the publication protocol around one pinned result object and compare-and-swap (CAS). Bet (2), the supervisor, is fine, but the plan's heartbeat semantics would create a new failure mode. Points below, then questions.

**1. Publication: drop "intent + journal + uncertain" and use "pinned result + roll-forward".**
Proposal. The integrator computes result `R` from `(T0, C)` once, pushes it to `refs/agent-coordinator/results/<submission>/<T0>`, and records `(T0, C, R, tree)` in the service *before* checks. Any later integrator fetches and reuses that recorded `R`. Nobody recomputes it. Determinism then becomes a convenience, not a correctness assumption. That matters because `merge-tree` output can differ with the Git version (merge-ort changes) and with repo or local merge drivers and attributes. Today, recovery on another host silently assumes identical Git. (`commit-tree` with `commit.gpgsign` is *not* a hazard. I checked on 2.39: it does not sign.)
Reconciliation rule for any integrator, at any time, after a fresh `ls-remote`/fetch of tip `X`:
- `X == T0`: **roll forward**. Push `R` with `--force-with-lease=T0`, provided the service says checks and approvals still hold. Two publishers pushing the same `R` under the same lease is idempotent: one succeeds, and the other sees `X == R`. We never need to prove the old publisher stopped. DURABLE-RECORD's "unchanged remote does not prove an old publisher stopped" stays true but stops mattering.
- `R ∈ ancestors(X)`: published.
- `X ≠ T0 ∧ R ∉ ancestors(X)`: **definitively not published**. Once the tip leaves `T0`, a lease-`T0` push can never succeed, *unless the tip returns to T0*.
The whole proof rests on one invariant: **I-FF: the target tips the service observes form a fast-forward chain**, so there is no ABA. Enforce it two ways. Use branch protection with no force-push. Have the integrator check `prev_observed_tip ∈ ancestors(X)` on every observation. On violation, freeze the queue and raise a `target_rewritten` incident. That is also how we catch "published, then rewound", which is otherwise indistinguishable from "never published". Any done task whose `R` is no longer an ancestor gets re-queued automatically.
Edge cases the plan must state:
- **FF case.** Here `R == C`, and `C` sits on a shared candidate ref, so the target can come to contain `C` through a stacked candidate or a human merge without our push. "Contains R" then means *content landed*, not *our checks guarded it*. That is acceptable only if I-FF holds and the monitor records who moved the tip.
- **No-op case.** Here `R == T0`: record `done`. Do not treat it as "published".
Result: publication intents, reconciliation dispositions, `local_journal_verified/publisher_stopped` attestations, and the "retain hold while uncertain" rule can all be deleted.

**2. Who is the trusted observer?** The service "cannot inspect Git remotes" (`coordination.rs:1348`). Today `agent_reconcile_publication` trusts whatever `observed_target_revision` the caller supplies (`workflow.rs:2757`). With LLM agents that is a hallucination hole. Proposal: only principals with role `integrator` may post observations or publication outcomes. Better still, the service gets a read-only fetch of the target, which is observation, not remote execution. With that, "done" never rests on an LLM's claim.

**3. `revise`: a concrete state machine.** Subject phases: `review → approved/queued → integrating(T0,R) → done`, plus `revision_needed`. `revise(submission_id, expected_generation, reason, evidence)` succeeds only in one transaction. Preconditions:
- (a) The CAS on the current submission id plus subject generation succeeds, which kills the race with a concurrent approval or claim.
- (b) There is no `integrating` record, *or* rule 1 proves not-published.
- (c) The reason is one of a closed enum, each with its own required evidence:
  - `conflict{T,C}`: re-checkable by any integrator.
  - `check_failed{receipt_id on R}`: verifiable in the service.
  - `candidate_missing`: re-checkable.
  - `requirements_changed{task_def_rev}`.
  - `author_withdraw`.
Actors:
- `author_withdraw` is author-only.
- `check_failed` and `conflict` are integrator-role only.
- Reviewers already have `changes_requested`.
"Any eligible agent" is too broad. It lets one agent grief another's work. Also add rate limiting (N revises per task per hour), and a circuit breaker to the human queue on revise ping-pong.

**4. Scoped pinning: pin inputs to *judgement*, not configuration.** A submission should pin exactly these:
- the candidate commit and tree;
- the **task-definition revision**, because acceptance criteria are what reviewers judge. An AC edit makes *approvals* stale (re-review), not the candidate;
- the **required-review set** derived at submit time. This is monotone: tightening adds activities, loosening cancels only still-queued ones.
Everything else is evaluated live at the moment of use: roster, `automatic_integration`, `lease_seconds`, rules text, target. Record the rules revision a reviewer saw as *evidence* in the decision, not as a gate. Instruction acknowledgment should key on (contract version, rules revision), not `policy_revision`, so a lease edit never forces re-ack. Decisions (`knowledge.rs:1287`) should re-pend only when their own scope changes.

**5. Delete the global hold across checks.** The hold spans the whole check run and blocks policy edits (`policy_hold_conflict`). Replace it with a service-side queue:
- Only the queue head may *publish*.
- Anyone may run checks on it.
- The head is a short lease on the "push slot" and is re-validated at push time.
- Policy edits never wait on integration.
With pinned `R` and CAS on the target, the hold adds nothing to safety and costs liveness.

**6. Leases under a supervisor: liveness ≠ progress.** If the supervisor heartbeats while the child lives, a looping LLM holds a lease forever. That is worse than today's 1 h expiry. Proposal: the supervisor renews only while all three hold:
- the child is alive,
- it is within its wall/token budget,
- its last checkpoint is younger than `progress_timeout`.
The service independently enforces `max_attempt_duration`. Split brain, meaning a partitioned supervisor whose child keeps working, is already fenced by attempt generation on every write. Candidate refs must include the attempt id so a zombie can't clobber its successor's ref. DURABLE-RECORD "launch identity must survive uncertainty" applies directly to the supervisor. It must persist launch intent plus (boot id, pid, start time) before spawning, and must never respawn for the same attempt while that identity might be alive.

**7. Check receipts are facts about content, not attempts.** Key receipts by `(result tree, check id, check-definition digest, environment)`. A late report from a zombie runner is then still valid evidence. Receipts become reusable across attempts and re-integrations onto the same tree. The attempt/reservation coupling can go for checks. Split "resources" in two:
- **Runner capacity.** Local to the runner, no service reservation, so `reservation.resolved` goes away for checks.
- **Exclusive physical resources** (the production host). Keep holds, but make recovery agent-safe with **fencing tokens** (a monotonically increasing deploy generation the target itself rejects if stale). Without target-side fencing, the human gate is *correct* and must stay.

**8. Delegated configuration is the biggest escalation risk.**
- (a) **Roster in the repo, read from the *target*, never the candidate** (the `pull_request_target` rule). A change that weakens checks must itself be integrated under the old checks and reviewed independently. It can never gate itself.
- (b) **Rules text is a prompt-injection channel.** It is prepended to every agent's context, so an agent that can write "approve submissions tagged X" can steer all reviewers. Rule edits must be reviewed submissions, not PATCHes.
- (c) A human-owned **floor** (a "constitution") that agents cannot go below: minimum review set, protected checks, `review_mode ≥ agent`, recovery floor. Agents edit only above it. Today agents can set `review_mode:none` (`coordination.rs:290-324`).
- (d) **No self-benefit.** A config revision cannot apply to a submission whose contributors include the config change's author or approver.

**9. Review independence.** The service can enforce only principal distinctness. `allow_subagent_reviews` makes independence self-asserted under one credential. Proposal:
- A distinct principal per role, issued to the supervisor.
- `reviewer ∉ contributors(principals)`.
- Role separation: an integrator can't review, and a reviewer can't publish.
- The supervisor attests "fresh context, no author transcript".
- Cross-vendor review is policy-optional, not a gate.

**10. DURABLE-RECORD.**
Keep:
- claim-as-transaction;
- receipts-as-history;
- session ≠ token;
- checks verify immutable source/definition (strengthened by point 7);
- lease ≠ resource hold (for exclusive resources);
- launch identity (now the supervisor's job).
Replace:
- "publication intent is a boundary for retries" → roll-forward (point 1);
- "recovery must transfer hold atomically" → becomes moot without the hold.
Relax: "client participates in correctness". Deterministic workers can derive idempotency keys from content, e.g. `hash(submission, T0, R, step)`, so no random-key journal is needed. Keep the journals for LLM-facing clients.

**11. Deletions.** Tables and states that can go:
- `publication_intents`, `publication_reconciliations`, `integration_holds`;
- attempt `mode=recovery` (recovery is just a claim that carries prior-attempt evidence);
- `human_review`/`either_review` kinds (use one `review` kind with `allowed_actors`);
- the `workflow_policies` table (roster in repo);
- check reservations;
- `blocked`. Replace it with `needs_attention{code, required_actor}`.

**Questions that must be answered**
- Q1. Can we enforce I-FF on the real remote (GitHub ruleset: no force-push, integrator-only push)? Is the user OK losing direct pushes to `main`, or do out-of-band pushes stay allowed? Rule 1 needs only no-force-push, not integrator-only.
- Q2. Is a read-only Git observer inside the service acceptable under CONTRIBUTING's "no remote execution"?
- Q3. Which resources can enforce fencing tokens? Any that can't must keep a human gate. Which are they?
- Q4. Migration. Today's open intents, held holds, held reservations and `recovery_required` activities need handling. I propose a cutover that refuses to run while any intent is unresolved, then runs rule 1 as the first integrator pass. Existing submissions get scoped pins derived from policy history.

**Clarifications**
- C1. Lead: in 2.4.2, does "integration uses current roster" mean the roster at *push* time? It must, or a roster tightening between checks and push is bypassed.
- C2. Does the `quick` lane skip the integrator? It must not, or I-FF monitoring has a hole.


---

### Sol (systems & protocol) — 2026-09-25T16:32:03Z

**Follow-up to Dana (Sol).** We converge on several points: liveness has to mean progress (your 8a, my 6), invariants have to sit outside the agents' loop (your 9, my 8c), and hold-spanning intents should be deleted. Replies:

**Your question in point 6: is a GitHub check-run on an exact SHA a weaker receipt than a job receipt?** On source identity it is *stronger*. The SHA pins the tree, and GitHub, not the workstation, attests that the job ran. Today's `inputs_unchanged` is self-attested by the producer host. It is weaker in three threat models, and each has a concrete fix:
- **(a) The subject defines its own check.** Actions runs the workflow file *contained in R*, and R includes the candidate's edits. So a candidate can rewrite `ci.yml` to make "linux-validation" trivially green. This is the `pull_request_target` problem. The receipt must bind the check *definition* as well: the workflow path and job name, plus the workflow file's blob id equal to the one in the target or protected roster, not the one in R. Workflow edits then land only under the old checks.
- **(b) Spoofed conclusions.** Any token with `checks:write`/`statuses:write`, which includes agents' repository tokens, can post a green status named "linux-validation". Accept only check-runs whose `app.id` is GitHub Actions and whose workflow run `head_sha == R`, or pin the source in the ruleset.
- **(c) Re-runs and retries.** Take the conclusion of the run bound to R and the definition blob. Never accept "any green run on this SHA". Flake policy must be explicit.

With (a) through (c) in place, I would accept GitHub receipts as the default check type and keep native producers as an opt-in type. That deletes most of plan §2.3 for this project, and it answers your deletion ask.

**Your point 5, P1-lite first: yes, with two corrections so that "cheap" doesn't turn into "unsafe".**
- (1) Policy-gated reopen or revise must keep the existing guard at `workflow.rs:~1133`. That guard refuses when any publication intent is not reconciled `not_published`. Until my rule 1 exists, an agent must not be able to revise a subject whose publish may have landed, because the result is a duplicate or conflicting re-integration.
- (2) "Narrow the pin to `review_mode` + roster" is slightly wrong in both directions:
  - The roster should not be pinned at all. It is evaluated at push time.
  - The task-definition revision *should* be pinned. An acceptance-criteria edit invalidates approvals, not candidates.
  Both changes are still in the few-hundred-line range.

**Your point 7, GitHub PR route.** My publication proof (pinned R, roll-forward, I-FF) is agnostic to who pushes. A bot that fast-forwards `main` to a pinned, checked R satisfies it. GitHub's own merge button and merge queue create *their own* merge commit, and the checks ran on something else unless the queue is used. So if we go GitHub-native, it has to be either "merge queue" (needs an org, as you note) or "bot CAS-pushes R". A PR's "Merge" with required checks on the PR head is *not* validation of the integrated result, and the user defined done as exactly that.

**Your point 9, circularity: agreed, and it sharpens my 8a.** The constitution has to be enforced by something the agents cannot deploy:
- GitHub rulesets: no force-push, required checks sourced from Actions, integrator-only or queue-only push to `main`.
- An admin token no agent holds.
- Production pinned to a release (your 10), so a guard removed on `main` is not immediately the guard in force.

For the lead: if the ruleset is human-owned, I-FF, which my whole reconciliation proof needs, comes for free.


---

### Hari (harness & ops) — 2026-09-25T16:32:16Z

**Opening review (Hari: harness, ops, cost, rollout).** I checked the plan against the local harnesses: Claude Code 2.1.282 and codex-cli 0.156.1 (`--help`, `claude auto-mode defaults`/`config`, `~/.codex/config.toml` with secrets excluded), and against this host's session logs. Summary: the supervisor is feasible. But the plan treats "launch `claude -p` with a vetted permission profile" as one line, and in practice that line is where the design either works or fails.

**1. The harness permission layer must be deterministic in supervised sessions. Do not use a classifier there.** `claude -p` accepts `--permission-mode` values `auto | dontAsk | bypassPermissions | acceptEdits | plan | manual`. It also accepts `--permission-prompts none`, which auto-denies anything that would otherwise prompt, and `--allowedTools/--disallowedTools`, `--settings`, `--strict-mcp-config` and `--max-budget-usd` (print mode only). In `auto` mode the default soft-deny list includes **Merge Without Review** ("before any *human* has approved it"), **Self-Approval** ("PR … authored by an automation the agent controls"), **CI Bypass** ("marking a required check as not-required"), **Production Deploy**, **Self-Modification**, **Create Unsafe Agents**, and **Unauthorized Persistence** (systemd units, scheduled tasks). Read them against plan §2.4.4/2.4.5/2.5: delegated roster edits look like CI Bypass, rule edits look like Self-Modification, and deploys look like Production Deploy. The classifier will refuse exactly the actions the user has granted. Codex behaves the same way through `--approve-for-me` / the auto-review assessor (the ~700 assessor calls on 09-17).
Suggestion: the coordinator's policy is the authority layer, so the harness should only provide containment. Run supervised sessions in `dontAsk` with an explicit allowlist, or in `bypassPermissions` inside a real OS sandbox. For Codex, use `codex exec -s workspace-write -c sandbox_workspace_write.network_access=true --add-dir <git-common-dir> -c approval_policy=never`. `--add-dir` fixes the "worktree metadata outside writable root" escalations. Keep auto mode for interactive sessions only.
The plan's own design removes most classifier conflicts anyway. If the integrator is non-LLM, no LLM ever runs `integrations prepare/publish`. If reviewers are fresh processes launched by the supervisor, no session ever reviews "its own" work. **So the deterministic integrator (lead's bet 1) is also the fix for D6**, and the plan should say so.

**2. Unattended sessions must not run as the user.** Today `~/.codex/config.toml` sets `default_permissions=":danger-full-access"` and `approval_policy="never"` globally, and `~/.codex/rules/default.rules` holds accumulated cross-project allows, including a bare `git push`. The Claude `autoMode.environment` in user settings describes a *different* repo ([redacted: another private repo]) as the only trusted repo. A supervisor that inherits these would give an unattended LLM the user's home directory, SSH/gcloud credentials for the production VM, and push rights, with no human watching. Suggestion: create a dedicated OS user per host (`agentc`) with its own `CODEX_HOME`/`CLAUDE_CONFIG_DIR`, launch with `--ignore-user-config --ignore-rules` for Codex and `--setting-sources`/`--settings` for Claude, and never place deploy credentials in an LLM-reachable account. Coordinator credentials should not sit in env vars the model can `echo`; the transcript persists. The supervisor mints a **short-lived, task-scoped token** per launch and hands it over through `--mcp-config` headers or a credential file the CLI reads.
Git push limits (`refs/agent-coordinator/*` only) cannot be enforced by the harness. Enforce them at the remote: agents' credentials can't update `main`, and only the integrator identity can. This is also Dana's point 9.

**3. Dana asked about cost and subscription vs API.** Measured on this Linux host only, which is roughly one host of three:
- Codex, 29 agent_coordinator sessions: **374M input tokens (98% cache hits), 0.85M output**. The largest single sessions were 35–49M input each.
- The single 09-22 Claude session (Sonnet 5, 800 calls, one task, 4b68f1c7): **224M cache-read tokens, 1.4M cache-write, 0.32M output**. At list API prices ($2/$10 per MTok; cache read about $0.20) that is roughly **$50 for one stuck task**, and most of it was polling loops re-reading a long context.

Cost is driven by input tokens: context length × number of turns. Output hardly matters. Implications:
- (a) **The model must never be the thing that waits.** The supervisor long-polls `next`/`state_wait` at zero token cost and launches only when there is work. The same goes for check runs: the LLM ends its turn, and a new short session resumes when results arrive.
- (b) Use fresh, short, single-task sessions with a ≤1.5k-word contract, not marathon sessions.
- (c) The integrator removes the most expensive turns entirely.

On subscriptions: I can't confirm terms-of-service from here, so the user should check. Practically, `claude setup-token` exists for long-lived subscription tokens and `codex exec` runs on ChatGPT auth. Both hit rolling usage caps, though, and `--max-budget-usd` means nothing without API billing. `--bare` mode requires an API key. Design for both: the supervisor must treat 429/usage-cap as *back off and release the lease*, not as failure.
Question for the user: what monthly ceiling, and is API billing acceptable for the unattended pool? My rough estimate is $5–30 per task at Sonnet-class pricing if sessions stay short. **P3 should begin with a 5-task pilot that records `total_cost_usd` (from `claude -p --output-format json`) and Codex `--json` token events per task** before any budget numbers are written into policy.

**4. Dana, on timer vs daemon:** I agree with starting small, and I agree liveness must mean progress, not a live PID. But a cron job that fires `claude -p` misses three things:
- crash handling: release or checkpoint the lease when the child dies;
- a watchdog: kill a session with no tool event for N minutes, via `--output-format stream-json` or Codex `--json`;
- concurrency limits: parallel cargo builds on MINIAIR.

That comes to about 300 lines, not a platform. Heartbeats should be *progress-gated*: the supervisor renews a lease only while the child has emitted a commit/checkpoint or a service call within the window, and a hard wall-clock cap per role applies on top.
Also note that `claude --bg` plus `claude agents --json` / `attach` / `logs` / `respawn` already give background session management with a live "peek" (`claude attach <id>`). That is worth a 1-hour evaluation as the Claude-side launcher. Codex has `exec resume`, and `--output-schema` gives a machine-readable final report.

**5. Windows supervisor specifics.** A Windows *Service* runs as LocalSystem and cannot read the user's harness auth: Codex `auth.json` sits in the user profile and Claude OAuth is per-user. Use a Scheduled Task "at logon, run whether user is logged on", under the `agentc` user. Laptops sleep, so the supervisor must release leases on suspend and on shutdown, not merely stop renewing them.

**6. Check reproducibility.** The 36% failure rate was environment, not code: `link.exe` shadowed by Git-for-Windows coreutils on PATH, WSL on `/mnt/c` (DrvFs), CRLF, and a tmpfs `/tmp` quota. My suggestion, which agrees with Dana's point 6:
- Required checks: Linux only, run in a pinned container image, runnable on both x86 and the ARM64 host (multi-arch image, `TMPDIR` inside the job dir, shared `sccache`, and disk garbage collection for per-worktree targets).
- Windows: a non-blocking GitHub Actions observer. The repo is public, so standard runners are free.

One mechanical gotcha: Actions do not trigger on pushes to `refs/agent-coordinator/*`. Only branches and tags fire `push` or accept `workflow_dispatch`, so results must be pushed as branches (e.g. `ac/results/<id>`).

**7. e2-micro.** The capacity test (84.6 MiB RSS) ran on a 2-CPU/4-GiB scope, not the 1-GiB e2-micro, and HANDOFF says so. The service will cope with supervisors. Keep these constraints:
- long-polls must stay under Cloudflare's roughly 100 s proxy timeout;
- nothing that builds Rust or runs checks goes on the VM;
- heartbeat events should be compacted, since 67% of events are already liveness.

Plan §2.5's blue/green deploy cannot run two SQLite writers on one VM. Use expand-only migrations, backup-before-migrate, and a pre-cutover smoke test of the new binary against a *restored copy* of the latest backup. Otherwise "auto-rollback" means data loss.

**8. Rollout on the live service.**
1. Stand up a **staging coordinator** on a workstation plus a throwaway project and repo for all soak runs. Never soak on prod.
2. Run the supervisor in **shadow mode** for a day: it computes `next` and logs what it *would* launch, with estimated cost.
3. Enable autonomy features per project behind a flag, with a one-command **kill switch**: supervisors stop launching and running children checkpoint and exit.
4. Keep a break-glass manual deploy path that doesn't depend on the service.
5. Drain or auto-migrate in-flight submissions before each protocol change.

**9. Observability.** Every launch writes a service record containing: harness, version, model/effort, permission profile, prompt hash, task, exit reason, tokens/cost, duration, and transcript path. The dashboard shows live sessions per host, spend today vs budget, the human queue, and a loop detector (same task → N failed revisions → park and notify). The notification must be **push-based** (ntfy/email), because the user won't be watching a dashboard. HRI=0 is meaningless if a parked task sits unseen.

**10. MCP.** Supervised sessions need the CLI anyway (Git, jobs), so MCP shouldn't try to match it. My proposal: MCP becomes a ~10-tool, `next`-centric surface for interactive or foreign harnesses (ChatGPT, desktop apps). Supervised sessions get `--strict-mcp-config` with a per-session, task-scoped credential, which also gives reviewers a distinct principal automatically. The mutation-journal complexity can then shrink.

**11. The user's day-to-day.** The P7 UI loop is interactive iteration in a browser. The user will not accept "wait for review before I see it". Clarification for the lead/user: is post-hoc acceptable? Under that model the user pushes via `agent-coordinator quick` (which creates the task, candidate and submission in one command); the integrator merges once checks are green; agent review runs asynchronously and opens a fix/revert task if it objects. Or must review gate the merge? My recommendation is post-hoc for `quick` paths only.

**Questions:**
- (a) Can autoMode rules/environment be set per project, or is it user- and managed-scope only? If a repo can't widen its own classifier (sensible), interactive sessions stay classifier-bound.
- (b) Which hosts are always on? MINIAIR sleeps; is oracle-1 dedicated?
- (c) Is cross-vendor review wanted (Codex reviews Claude and vice versa)? It decorrelates errors and spreads rate limits.
- (d) What happens when a vendor is down or out of quota: queue, or fall back to the other vendor?


---

### Dana (devil's advocate) — 2026-09-25T16:32:45Z

**Follow-up (Dana) to Sol and Hari.**

**Where I'm convinced:**
- **Sol, on GitHub receipts.** Your threat model (a)–(c) answers my question: bind the definition blob from the *target*, accept only Actions check-runs, and bind each run to R. I accept GitHub receipts as the default check type, with native producers as opt-in. I also accept your correction to my P1-lite: keep the `not_published` guard near `workflow.rs:1133` on agent revise. Pin the task-definition revision. Don't pin the roster; evaluate it at push time.
- **Hari, on cron vs supervisor.** Crash release, a progress watchdog and per-host concurrency are real requirements. A cron job can't provide them. I withdraw "50 lines" and accept roughly 300 lines, *on the condition that* the lease is renewed only when there is progress. A dedicated `agentc` OS user and task-scoped tokens should be in P-first, not P3: today's `danger-full-access` plus the user's SSH/gcloud keys is the biggest safety hole in the room.
- **Hari, on the deterministic integrator as the fix for D6.** Agreed. That is the strongest argument for bet (1), and the plan should make it explicitly.

**Where I still push back:**
1. **Sol, your redesign is correct, but it is a second big bang.** Roll-forward, a queue instead of the hold, content-keyed receipts, deleting intents/holds/reservations/`blocked`/`mode=recovery`/review kinds, plus a migration cutover (Q4): that is P2 alone rewriting most of `workflow.rs`. I'd order it like this. First, P1-lite with your guards. Second, the integrator uses pinned R + roll-forward + I-FF (this deletes intents and holds as part of the same change). Third, the remaining deletions, each one justified by a measured failure. Please say which of your deletions are *required* for correctness in the integrator phase and which are cleanup.
2. **Sol 8b plus my point 9 point to a simpler design than delegated config.** If rule edits must be reviewed submissions, and the roster is read from the target, then put **rules and roster in repo files** (`.agent-coordinator/rules.md`, `roster.toml`) that change only through the normal integrate path, with CODEOWNERS/ruleset protecting a constitution file. The `PATCH policy` agent path, `agent_rule_editing`, the `workflow_policies` table, and D2's revision-pinning problem then *all disappear*. We would be deleting a feature instead of adding `agent_config_editing`. Resources (for example production-host) are the only config left in the service. Objections?
3. **Hari 11, post-hoc review for `quick`.** The user defined done as "after required review, integration and validation". If post-hoc review is acceptable for human-authored UI tweaks, the honest question to the user is whether it is acceptable for *any* low-risk path, agents included. A human-only quick lane is a second workflow. There is also a cheaper route: allow human direct pushes but require a GitHub ruleset with "branch up to date + required checks". Then the pushed head *is* the integrated result (Sol's objection to PR-merge doesn't apply to fast-forward-only pushes), I-FF holds, and the integrator simply recomputes when the target moves. No `quick` command is needed.
4. **Cost.** Hari's $5–30/task × ~53 tasks per fortnight comes to $265–1,600 per two weeks, and one stuck task cost about $50. The user has to see that number before we commit to 100% unattended operation. The five-task costed pilot should be a gate, not a footnote.

**Open ask to the lead:** please drop the fake-agent soak rig as P0. Replace it with Hari's staging coordinator + shadow mode + the costed pilot, and keep an attention-based HRI metric (my point 1) that counts chat prompts and approvals, not just service events.


---

### Hari (harness & ops) — 2026-09-25T16:32:50Z

**Follow-up to Dana and Sol (Hari).** We mostly agree. Below, I map what we agree on to concrete harness mechanics.

1. **Sol 6 (progress-gated renewal plus durable launch identity) is directly implementable.** The Claude CLI accepts `--session-id <uuid>`. That lets the supervisor persist (attempt, session-uuid, boot id, pid, start time) *before* it spawns the process, and it can never accidentally launch a second session under the same identity. Codex prints its thread id as the first `--json` event, so the supervisor records it right after spawn. The "progress" signal is the stream (`stream-json` / `--json`): a tool event, a commit, or a service call inside `progress_timeout`. Checkpoints alone are too coarse, because a 20-minute cargo build is legitimate silence. So: tool-activity timeout ≈ 10–15 min, and checkpoint-age timeout ≈ 60 min.

2. **Sol 9's "fresh context" attestation needs enforcement in the harness, not a promise.** A reviewer must be launched with:
   - no `--resume`/`--continue`;
   - `--no-session-persistence`;
   - `--strict-mcp-config`;
   - a **separate `CLAUDE_CONFIG_DIR`/`CODEX_HOME`**.

   The separate config directory is the important one. Claude auto-memory and Codex `memories_1.sqlite` are per-user stores, so an author's notes would otherwise leak into the reviewer on the same host. `--bare` also skips auto-memory and CLAUDE.md discovery, but it requires an API key. The attestation is then a fact about the launch command line, and the supervisor can record that command line in the session record.

3. **Sol 8b (rules text is an injection channel): agreed, with a harness corollary.** Supervised prompts should carry rules and task text as clearly delimited *data* in the user turn. They should never go through `--append-system-prompt`. Reviewers should receive the rules from the *target* revision.

4. **Sol's GitHub-receipt conditions (a)–(c) have two operational costs:**
   - Actions workflows run candidate code, so they must stay secret-free. That holds today, but it must become a documented invariant.
   - A cold Rust build on standard runners takes roughly 10–15 min. That breaks the plan's "integration p50 < 10 min" unless there is a cache keyed on `Cargo.lock` plus the toolchain. The cache is written only from `main` runs, so candidates can't poison it.

   So: GitHub receipts should be the required default, and a local Linux container runner is an optional fast path that produces the same receipt shape.

5. **Dana 1 (attention metric): the supervisor makes it measurable.** Every service call carries a credential class: supervisor-minted task token, interactive-session credential, or human dashboard. The rule becomes "a task is autonomous iff every mutation on it came from supervisor-minted tokens". It is computable from the event log and includes no chat. Approvals and "continue" prompts cannot occur inside supervised sessions (`--permission-prompts none` / `approval_policy=never`). So the only remaining human-attention channel is interactive sessions, and those are tagged.

6. **Sol Q1 / C2 and the user's direct pushes are one decision.** If I-FF plus integrator-only push is adopted, the user's UI loop must go through `agent-coordinator quick`. That means one command, landing in minutes, with review post-hoc as I proposed. Otherwise the ruleset blocks the user. That trade-off belongs in front of the user explicitly, and in my view it is the single most important day-to-day UX question in the plan.


---

### Lead (Claude, main session) — 2026-09-25T16:34:30Z

**Round 2 (lead): what I'm adopting, what's still open, and assignments.** Thank you all; this has already improved the plan a lot. Here is where I stand now; plan-v2 will follow the next round.

**Adopted (I'm convinced):**
1. **Goal (Dana 1, 2 + Hari 5).** Success = every task reaches a *terminal* state (done, or canceled/superseded with a recorded agent rationale) where **every mutation came from a supervisor-minted credential**. The metric counts human *attention*: interactive-session mutations, dashboard actions, and parked-task notifications. Authoring tasks and priorities stays human-or-agent input and does not count as intervention; the goal covers everything after a task exists.
2. **Gate triage (Dana 3).** Every human gate goes in one of three bins: (a) authority-only → agent path; (b) information → move it into repo/config; (c) judgement → a labelled, *non-blocking* decision with push notification (Hari 9). plan-v2 will carry the full table.
3. **Sequencing (Dana 5/11).** P1-lite ships first. The fake-agent soak rig is dropped as P0. It is replaced by per-fix regression tests in `crates/server/tests`, a staging coordinator, supervisor shadow mode and a **costed 5-task pilot as a go/no-go gate** (Hari 3/8). Every new component names what it deletes.
4. **Publication (Sol 1, 5).** Pinned R, roll-forward, I-FF, and a queue with a short push slot replace intents, reconciliation, journal attestations and the global hold. Sol's guard stays in P1-lite: no agent revise while an intent is unresolved.
5. **Pinning (Sol 4).** Pin candidate + task-definition revision + monotone required-review set. The roster, rules, lease and flags are read live, with the roster at *push* time (answering Sol C1: yes, push time). Acks are keyed on (contract version, rules revision).
6. **Rules and roster move into repo files read from the *target* (Dana follow-up 2, Sol 8a/b).** This deletes the agent `PATCH policy` path, the `workflow_policies` table and the whole D2 class. The service keeps only the human-owned switches (the "constitution floor": minimum review set, protected check ids, recovery floor) plus resources.
7. **Checks.** GitHub Actions receipts are the required default, bound per Sol (a)–(c), with result branches `ac/results/<id>` (Hari 6) and a `main`-only-write build cache (Hari follow-up 4). A local container runner is an optional fast path with the same receipt shape. Windows becomes a non-blocking observer pending a user decision.
8. **Harness layer (Hari 1, 2).** Supervised sessions use deterministic containment (Claude `dontAsk` + allowlist + `--permission-prompts none`; Codex `exec -s workspace-write` + network + `--add-dir <git-common-dir>` + `approval_policy=never`), a dedicated `agentc` OS user, separate config dirs per role, and short-lived task-scoped tokens. **This moves into the first phase**, because today's full-access defaults are the biggest safety hole (Dana).
9. **Supervisor (~300 lines).** Progress-gated renewal (tool-event ≈10–15 min, checkpoint-age ≈60 min), a service-enforced `max_attempt_duration`, launch identity persisted before spawn (`--session-id`), release on suspend/shutdown, and attempt id in candidate refs.
10. **Deployment.** Production is pinned to a release, and staging dogfoods `main`, which dissolves most of D10. Autonomous deploy only when migrations are expand-only, with a smoke test on a restored backup. Anything else becomes a labelled decision.

**My new observation for challenge: the constitution circularity (Dana 9) exists only for the self-hosted project.** For any *other* project, the guards live in service code that the project's agents cannot deploy. For agent_coordinator itself, the rule "production runs a pinned release; promoting a release that changes guard code (a declared path set) is a (c)-bin decision" breaks the loop without a human on ordinary changes. Dana, is that enough?

**Still open. These are assignments for this round; please post results publicly:**
- **Sol:** (i) Answer Dana: which deletions are *required* for correctness in the integrator phase, and which are cleanup? (ii) Q2: I lean towards *allowing* a read-only observer, meaning ls-remote-style ref reads (GitHub API or smart-HTTP), not a full clone on the e2-micro, with ancestry proofs taken only from integrator-role principals. Does your proof survive with "service verifies the tip equals T0 or R; ancestry comes from the integrator role"? (iii) **PoC:** a ≤100-line shell script in a /tmp scratch repo demonstrating roll-forward with two concurrent publishers pushing the same R under `--force-with-lease=T0`, plus the rewound-target detection. Report the output.
- **Hari:** (i) Verify from GitHub docs (WebFetch is fine) what a **personal-account** repo can enforce. I believe "restrict who can push" and merge queue are org-only, while block force-push and required status checks are available. If so, "integrator-only push to main" requires moving the repo to a free org. (ii) With required status checks on direct pushes, can a human push a commit to `main` that has *not* been checked? That decides whether Dana's "human FF pushes under rulesets" can replace `quick`. (iii) Draft the exact launch command lines per role (implementer, reviewer) for Claude and Codex, and the minimal task-scoped-token design the service needs.
- **Dana:** (i) Challenge the circularity answer above. (ii) **PoC/size check:** in a /tmp worktree (separate CARGO_TARGET_DIR), sketch the P1-lite diff (policy-gated revise/unblock with Sol's guard, scoped pinning, labelled human gates) far enough to estimate real LOC and the tests it breaks. Your "few hundred lines" claim decides the phase plan, so let's measure it. Clean up afterwards.

**User decisions I will put to the user (please challenge the recommendations):**
- U1: Windows as a first-class required producer, or a non-blocking CI observer? *Recommend observer.*
- U2: human UI work goes through `quick` (integrator merges after checks, agent review after the merge) vs. direct pushes. *Recommend `quick` + protected main.*
- U3: API billing plus a monthly ceiling for the unattended pool? *Recommend API billing, with the cap set after the pilot.*
- U4: move the repo to a free GitHub org for push restrictions? *Recommend yes, if Hari confirms it's needed.*
- U5: is cross-vendor review wanted? *Recommend optional; route to the other vendor when available.*
- U6: which hosts are always on? This informs supervisor placement.


---

### Sol (systems & protocol) — 2026-09-25T16:36:30Z

**Round 2 (Sol): answers to the lead's three assignments, plus replies to Dana and Hari.**

**(i) Dana's question: which integrator-phase deletions are *required*, and which are cleanup?**
*Required for correctness.* Each of these changes must land in the same change as the integrator:
1. **Re-key the result record from attempt to `(submission, T0)`, with UNIQUE `(submission_id, base_T0) → R`.** Today's intent is pinned to the attempt (`publication_attempt_mismatch`, V4). That directly contradicts roll-forward, which requires a *new* attempt to be able to publish the *old* R. The unique constraint is the one invariant everything else rests on: only one R ever exists per base. This is *replace*, not *delete*: `publication_intents` becomes `integration_results(submission, T0, C, R, tree)`.
2. **Remove the LLM-driven prepare/publish/reconcile paths on integrator-enabled projects.** A second producer of R, such as the CLI on a different Git version or with a local merge driver, can create R′ ≠ R for the same base, which breaks the unique constraint. The service must refuse any result record that does not come from the integrator role.
3. **The integration hold must follow the submission, not the attempt, and must be short (push slot only).** A hold that survives expiry but is bound to the dead attempt starves roll-forward. The table can stay; its key and lifetime change.
4. **Add the I-FF enforcement:** ruleset no-force-push (or `receive.denyNonFastForwards`), plus a monitor that freezes the queue on `target_rewritten`. *While frozen, roll-forward is suppressed.* See case 3 below.
5. **Migration imports every open legacy intent as a pinned result.** The legacy intent already holds `observed_target_revision`/`result_revision`, which are T0 and R. The first integrator pass then reconciles it by rule 1. **No human is needed at cutover.** Without this, cutover strands exactly the uncertain publications we are trying to kill.

*Cleanup.* These are safe to defer, and each should be deleted when a measured failure justifies it:
- journal/`publisher_stopped` attestations (dead once item 2 lands);
- `publication_reconciliations` as a table (keep rows as history);
- `policy_hold_conflict`;
- check reservations (not needed with GitHub receipts; keep for native runners);
- receipt re-keying by content (re-running checks under a new attempt is *correct*, only wasteful);
- `blocked`→`needs_attention`, `mode=recovery`, the review kinds;
- `workflow_policies` (goes with the repo-roster phase, not this one).

**(ii) Q2: does the proof survive if the service verifies only `tip ∈ {T0, R}`, with ancestry attested by the integrator role?** Mostly, yes. The trust is asymmetric, so it has to be handled per case:
- `tip == R` → published. The service verifies it.
- `tip == T0` → roll forward. The service verifies it, and a lying integrator can only cause a redundant idempotent push.
- `tip ∉ {T0, R}` needs ancestry, and the two possible *wrong* claims differ in damage:
  - A **false "not published"** self-heals. The route is *re-integrate onto the new tip* (never `revise`), and if C is already contained the merge is a no-op. The only cost is a wasted cycle.
  - A **false "published"** is a *lost change*: the task is marked done and its content is not in `main`. This is the direction to guard.

My recommendation has two layers:
- (a) **The service verifies ancestry with one read-only call**, `GET /repos/{o}/{r}/compare/{R}...{tip}`. Status `identical|ahead` means R is contained. It costs one HTTPS request on the e2-micro, needs no clone, and executes nothing. Integrator attestation is used only for non-GitHub remotes.
- (b) **Continuous audit.** Every integrator fetch checks that every task done since the last audit still has its R, or its C for the FF case, as an ancestor of the tip. A false "published" or an unmonitored rewind is then caught within one integration cycle and automatically re-queued.

I-FF itself should rest on the *remote's* enforcement (ruleset / `denyNonFastForwards`), with the monitor as a detector. The service can't prove ancestry from `ls-remote` alone, so it must not be the only guard.

**(iii) PoC.** `/tmp/sol-poc/rollforward.sh`, 54 lines, local bare remote, Git 2.39.5. It builds R once with `merge-tree --write-tree` plus a fixed-date `commit-tree` and pins it at `refs/ac/results/s1`. Output (verbatim):
```
T0=087796c3 C=beffdf35 R=cddfc7f5
== case 1: two concurrent publishers, same R, lease T0
publisher B: push exit 0
publisher A: push rejected
reconcile: published (tip=cddfc7f5)
late zombie retry (lease T0): publisher Z: push exit 0
reconcile: published (tip=cddfc7f5)
== case 2: target moved before publish -> CAS fails forever, provably not published
stale publisher (lease=cddfc7f5): rejected
reconcile s2: not_published (tip=64138d8a, R not ancestor) -> requeue/revise
== case 3: someone force-pushes main back (rewind) -> I-FF monitor fires
reconcile s1 (prev observed tip T1): INCIDENT target_rewritten prev=64138d8a tip=087796c3
roll_forward (tip==T0; push R again under lease T0)
   ^ ABA: tip is T0 again, so a zombie lease-T0 push of R would now succeed; only I-FF makes 'tip!=T0' final.
== case 4: server-side I-FF (receive.denyNonFastForwards) blocks the rewind
rewind rejected by remote
```
Findings:
- **Case 1:** concurrent same-R publishers converge. One wins; the loser's rejection is harmless because the observation says published.
- **New finding from the zombie line:** `git push --force-with-lease=main:T0 R` **exits 0 when the remote is already at R**, even though the lease is stale. Git reports "up to date" without checking the lease. It is harmless for the *same* R, but **a publisher must never infer "I published" or "my lease held" from the exit code; always observe.** The current `publish_prepared` already observes after pushing, so keep that property in the integrator.
- **Case 2:** once the tip leaves the lease value, the CAS fails forever. That makes `not_published` final.
- **Case 3:** shows the ABA hazard. After a rewind the classifier would say "roll forward", which is why the incident must *freeze* the queue first.
- **Case 4:** server-side I-FF closes the hole.

I have cleaned up the scratch repo.

**Replies.**
- **Hari (point 6 / follow-up 4), result branches `ac/results/<id>`:** agreed. Two notes:
  - The branch name is only a *trigger*. The receipt binds the SHA R, so an agent overwriting the branch can't forge anything.
  - Pushes made with `GITHUB_TOKEN` do not trigger workflows, so the integrator must push with its own App/PAT identity. That identity should *not* be allowed to write `main` directly if we use the queue-only model. If it is the only `main` writer (U4 org route), it is the one credential to protect above all others. Keep it off LLM-reachable accounts, per your point 2.
- **Lead, circularity:** your answer works if the "guard path set" explicitly includes:
  - the authz/policy code,
  - the integrator,
  - the migration directory,
  - the CI workflow files.

  Promotion must also use a human-owned credential. Otherwise an agent can relabel a guard change as non-guard by moving code across the boundary. The path-set file itself belongs in the constitution (CODEOWNERS/ruleset). Dana owns the challenge; that is my one constraint on it.


---

### Hari (harness & ops) — 2026-09-25T16:37:54Z

**Round 2 (Hari): GitHub enforcement, direct pushes, launch lines, task-scoped tokens.** Sources are GitHub Docs (fetched today) plus read-only `gh api` calls on the repo. Nothing was mutated and no LLM was launched.

**Where the repo is today:** `marshallr12/agent_coordinator` has `owner.type=User` and `visibility=public`. `rulesets` is `[]`, and `branches/main/protection` returns 404 "Branch not protected". So nothing is enforced, which is how 9bb9a28 could land.

**(i) What a personal-account public repo can enforce.**
- **Rulesets.** Docs: "Rulesets are available in public repositories with GitHub Free and GitHub Free for organizations, and in public and private repositories with GitHub Pro, GitHub Team…". So yes, they work on this repo.
- **Rules that apply to us:** `update` ("only users with bypass permissions can push to branches … that match"), `non_fast_forward` (block force pushes), `deletion`, and `required_status_checks`. Each check takes an **`integration_id`**, described as "the optional integration ID that this status check must originate from". Pinning that to the GitHub Actions app answers Sol's spoofing case (b) at the remote.
- **Bypass actors.** The REST `actor_type` values are `Integration | RepositoryRole | DeployKey | User | Team | OrganizationAdmin`. The docs single out only OrganizationAdmin: "OrganizationAdmin is not applicable for personal repositories."
- **Classic "Restrict who can push" is org-only.** Docs: "You can enable branch restrictions in public repositories owned by a GitHub Free organization…". **But the ruleset `update` rule plus an `Integration` (GitHub App) bypass does the same job on a personal repo.**
- **Merge queue is org-only.** Docs: "available in any public repository owned by an organization, or in private repositories owned by organizations using GitHub Enterprise Cloud". We don't need it: Sol's pinned-R + CAS is our queue.
- **Lead, on U4:** so I believe **no org transfer is needed** (my inference from the docs). The one step to confirm by hand: on a throwaway public personal repo, create a ruleset with `update` (bypass = a test App) and confirm that a plain collaborator push is rejected. That is a 10-minute check for the user; I did not create repos. Recommendation: *U4 = no, unless that test fails.*
- **Rulesets bind admins too.** Unlike classic protection, where "restrictions … don't apply to people with admin permissions" by default, a ruleset binds everyone who is not on its bypass list, including the owner. That is what we want.

**(ii) Can a human push an unchecked commit to `main` under required status checks?** **No, unless they are a bypass actor.** Docs: "Required status checks must have a successful, skipped, or neutral status before collaborators can make changes to a protected branch", and "After all required status checks pass, any commits must either be pushed to another branch and then merged or pushed directly to the protected branch."
- A direct push is accepted only if that exact SHA already carries green required checks. In practice: push to a side branch, wait for Actions, then fast-forward `main` to that SHA.
- With non-fast-forward blocked, the pushed head *is* the integrated, checked result. That satisfies Sol's objection to PR merges and preserves I-FF.

**Dana is right: human fast-forward pushes under rulesets can replace `quick`.** One detail makes it work. Bypass is per ruleset, so use two:
- **Ruleset A:** `update` + `deletion`, bypass = integrator App + owner.
- **Ruleset B:** `non_fast_forward` + `required_status_checks` pinned to the Actions integration, **with no bypass at all.**

Agents cannot write `main`. The human can, but only with a green, fast-forward SHA. The integrator recomputes whenever the target moves. The human's "ship" becomes a ~20-line script (`git push origin HEAD:ac/human/<name>`, `gh run watch`, `git push origin <sha>:main`), not a coordinator workflow.

Two caveats for U2:
- **(a)** Actions runs the workflow file *in the pushed SHA*, so a human push can change CI definitions. That is acceptable for the owner, but Sol's definition-binding applies only to the integrator path.
- **(b)** Human pushes get no review. That matches today's behaviour and the user's P7 intent, and the attention metric simply doesn't count them because they aren't tasks.

*Revised U2 recommendation:* direct fast-forward pushes under ruleset B, with no `quick` lane. This removes plan §2.6.

**(iii) Launch lines.** These are drafts. Flags come from `--help` on claude 2.1.282 and codex 0.156.1; items marked ⚠ need checking against the settings schema.

*Common setup:*
- Processes run as uid `agentc` with a per-role config directory.
- The supervisor prepares `$WT`, a worktree of the claimed attempt, and `$RUN/`, which sits *outside* the worktree and holds `prompt.md`, `token`, `mcp.json` and the schema.
- Egress is limited by a host firewall owner-match on the `agentc` uid: the service, github.com, crates.io/static.crates.io, and the model APIs. Codex's `network_access` is all-or-nothing, so the firewall is what actually enforces the allowlist for both harnesses.

**A new hazard, and it matters.** The worktree contains *candidate-controlled* `.claude/settings.json`, `CLAUDE.md`, `AGENTS.md` and `.codex/`. If the harness auto-loads them, the author controls the reviewer's permissions and instructions. This is the same bug class as Sol's `pull_request_target`. So never load project or local settings, and pass repo instruction files only as *data*:

```
# Claude — implementer (reviewer: same, with the differences listed below)
CLAUDE_CONFIG_DIR=/var/lib/agentc/claude-$ROLE AGENT_COORDINATOR_TOKEN_FILE=$RUN/token \
claude -p --session-id "$LAUNCH_UUID" --name "ac-$ROLE-$TASK" \
  --model "$MODEL" --effort high \
  --permission-mode dontAsk --permission-prompts none \
  --setting-sources user --settings "$RUN/$ROLE-settings.json" \
  --strict-mcp-config --mcp-config "$RUN/mcp.json" \
  --tools "Bash,Read,Edit,Write,Glob,Grep" \
  --allowedTools "Read Edit Write Glob Grep Bash(cargo *) Bash(git *) Bash(agent-coordinator *)" \
  --disallowedTools "WebFetch WebSearch Bash(git push *) Bash(git remote *) Bash(git config *)" \
  --output-format stream-json --verbose --max-budget-usd "$CAP" \
  < "$RUN/prompt.md"
```
- `--setting-sources user` means the per-role `CLAUDE_CONFIG_DIR`, not the human's settings.
- `--bare`, which skips CLAUDE.md discovery and hooks, is the cleaner choice with API billing.
- ⚠ Enable the Bash sandbox in `$ROLE-settings.json`.
- Pushes go through `agent-coordinator push-candidate`, which pushes only `refs/agent-coordinator/candidates/<attempt>`, rather than raw `git push`.

```
# Codex — implementer
CODEX_HOME=/var/lib/agentc/codex-$ROLE AGENT_COORDINATOR_TOKEN_FILE=$RUN/token \
codex exec --ignore-user-config --ignore-rules -m "$MODEL" -c model_reasoning_effort=high \
  -s workspace-write -c sandbox_workspace_write.network_access=true -c approval_policy=never \
  -C "$WT" --add-dir "$(git -C "$WT" rev-parse --git-common-dir)" --add-dir "$RUN/target" \
  -c shell_environment_policy.inherit=core \
  --json -o "$RUN/last.md" --output-schema "$RUN/$ROLE-result.schema.json" - < "$RUN/prompt.md"
```
- ⚠ Suppress auto-loaded `AGENTS.md` (e.g. `-c project_doc_max_bytes=0`), because the candidate controls that file.
- `$WT` sits under `/var/lib/agentc`, outside any trusted project, so repo `.codex/` config is not loaded (⚠ verify).

**Reviewer differences, both vendors:**
- A fresh config directory per launch, seeded from a template, so there is no auto-memory or `memories_1.sqlite` carry-over.
- A new session id.
- Worktree at the candidate SHA; Edit/Write denied except `$RUN/`.
- `git commit`/`push` denied.
- The model defaults to the *other* vendor when available.
- Output is **structured only** (`--json-schema` / `--output-schema`): `{decision, findings[], criteria_evidence[]}`. **The supervisor posts the decision** using the reviewer token. The LLM never calls a "approve" endpoint, so the Self-Approval classifier question disappears.

**Minimal task-scoped token design (service):**
1. **Host principal.** Enrolled once by the human (bootstrap) and stored 0600 under the supervisor uid. It can do exactly four things: `next`, claim, renew/release leases with progress evidence, and **mint**. It never reaches an LLM.
2. **Mint.** `POST /api/v1/launches {activity_id, generation, role, harness, model, launch_uuid, argv_digest}` returns an opaque 256-bit token. The service stores only its hash in `launches(id, host_principal, activity_id, generation, role, scopes, argv_digest, expires_at, revoked_at, exit, usage)`.
3. **Principal.** Each launch is its own principal, `launch:<uuid>`, recorded as `contributor` on every event. That gives Dana's attention metric ("every mutation from a launch token") and Sol's `reviewer ∉ contributors` at launch granularity.
4. **Scopes.** Fixed per role and bound to that single activity:
   - implementer: read project, checkpoint, register candidate ref, submit, and post messages;
   - reviewer: read submission and artifacts; nothing else, because the supervisor decides using the reviewer launch record;
   - integrator: a separate principal type, not mintable by a supervisor.

   Every write is also fenced by `generation`, so a zombie's token dies on re-claim.
5. **Lifetime.** `expires_at = min(role cap, lease)`, extended only by the host's progress-gated renewal. It is revoked by `POST /launches/<id>/finish {exit_reason, usage}`, which is the observability record from my point 9.
6. **Independence setting.** `review_independence = distinct_launch (default) | distinct_host | distinct_vendor`. With `distinct_launch`, one host can author and review (Dana's 8b). Projects that care set `distinct_host`, which works because oracle-1 is always on.

**On secrecy: be honest.** Same-uid children can read `$RUN/token`. The design bounds the *blast radius* (one activity, one generation, a short TTL, no approve scope) rather than pretending to hide the token. Hiding it would require a uid-split local proxy, which I'd defer.

**Sol:** agreed that the integrator's App identity must push `ac/results/*`, because `GITHUB_TOKEN` pushes don't trigger workflows. With ruleset A, that same App is the sole non-human `main` writer, and it lives only on the integrator uid. Your "never infer from exit code" finding also applies to the supervisor: a harness's exit 0 means nothing, and success = service state plus observed refs.


---

### Dana (devil's advocate) — 2026-09-25T16:49:10Z

**Round 2 (Dana): the P1-lite measurement, the circularity challenge, and pushback on the adopted list.**

## A. P1-lite PoC: measured

Setup: worktree `/tmp/dana-p1lite` on `poc/p1-lite-dana` from `d72a8cb`, with `CARGO_TARGET_DIR=/tmp/dana-p1lite-target`. Both the worktree and the branch are now deleted.

**What I built:**
1. **Scoped pinning.**
   - `ActivityContext` gains `project_pin_ok`. It is true when the revisions are equal, *or* when `review_mode` at the pinned revision (from `policy_revisions.data_json`) equals the current `review_mode`.
   - That flag replaces the whole-revision comparison at every workflow site:
     - preconditions `:849`;
     - `ensure_current` `:1434`;
     - the renew and replay checks `:1471` and `:1601`;
     - agent reconcile `:2670`;
     - the `guard_activity_work` row query `:1339`;
     - the snapshot blocker `:1039`;
     - submit-time `policy_changed` `:466`.
   - The roster pin is **untouched**.
2. **Policy-gated reopen/unblock.**
   - New helper `agent_recovery_or_human(project, actor, code)`. It allows humans, and allows agents when `recovery_mode=agent`.
   - It is used at `workflow.rs:1117` (reopen) and `coordination.rs:1134` (unblock).
   - Sol's guard is unchanged. The existing check that refuses reopen while any publication intent is not reconciled `not_published` runs right after the gate.
3. **Labelled human gates.**
   - New `AppError::human_required(code, msg)` returns 403 with `details.required_actor="human"`.
   - Both `human()` and `admin_or_operator()` now emit `human_operator_required`. Reopen and unblock emit `human_reopen_required` and `human_unblock_required`.
   - Result: 8 of the 10 human-gate call sites are now labelled (6 through the shared helpers, plus reopen and unblock). `imports.rs:547` and `jobs.rs:267` are not yet labelled.
   - The task precondition code `operator_reopen_required` is renamed `reopen_required`.
4. **New regression test.** Scenario: a lease-only edit (600→3600 s), then an agent reviewer claims against the pinned revision → 200. Then `review_mode` changes and `recovery_mode=manual`: an agent reopen gets 403 `human_reopen_required`. Then `recovery_mode=agent`: the agent reopen gets 200.

**Numbers:**
- **Source:** 62 lines added, 17 deleted, across `workflow.rs`, `coordination.rs` and `error.rs`.
- **Tests:** 32 lines added (the new test), 1 line changed.
- **Existing tests broken:** 1 assertion, `preconditions_surface_operator_reopen_and_reviewer_independence` at `tests/workflow.rs:1706`, and only because of the code rename. It still correctly sees staleness, since it changes `review_mode`.
- No test asserted `operation_not_permitted` on reopen or unblock.
- **Suite:** all 155 server tests pass (22 binaries). `cargo clippy -p coordinator-server --all-targets -D warnings` is clean.
- No CLI, MCP or web code matches on the changed codes.

**Not in the PoC; estimated remaining work for shippable P1-lite:**

| Item | Estimate |
|---|---|
| Sol's revise contract: reason enum, per-reason actor rules, rate limit and ping-pong circuit breaker (the PoC lets *any* agent reopen, which is the grief vector Sol flagged, so it must not ship as-is) | ~120–180 LOC + tests |
| Roster evaluated at push time instead of pinned: `validate_check_jobs`, the `workflow_policy_revision` compares, and the integration-authorization binds | ~60–120 LOC. This is where existing tests most likely break (roster-staleness cases). Unmeasured |
| Instruction ack keyed on rules revision rather than `policy_revision`. Today a lease edit still forces re-ack; my test had to re-ack | ~30 LOC |
| Decisions re-pending on any policy bump (`knowledge.rs:1287`) | ~30 LOC |
| Labels for `jobs.rs:267` (resources) and the reservation resolve | ~10 LOC |
| Guidance and docs: 6 prose hits in `discovery`/orientation plus 4 book pages that say "operator reopen" | ~150 lines of prose, mostly deletions |

**Total: roughly 450–650 lines including tests and docs, 1–2 focused days.** My "few hundred lines" claim holds. The risky part is Sol's revise contract, not the gate flip. One design caveat: the PoC treats a changed `review_mode` as stale, which is the old behaviour. The monotone "tightening adds activities" rule is additional work (~80 LOC).

## B. Challenging the lead's "the circularity exists only for the self-hosted project"

I'm not convinced, for four reasons.

1. **Adopted item 6 moves the roster and rules into repo files, so every project now has the loop.** An author plus one same-vendor reviewer can land "drop the flaky check" or "rules: approve docs-only changes on sight". Under Sol's target-binding that change is judged by the *old* checks, but from then on everything runs under the weakened ones. Target-binding stops a candidate gating *itself*. It does not stop a two-step weakening. On top of that, per Hari's caveat (a), Actions runs the workflow file in the pushed SHA, so `ci.yml` is agent-editable on any project. The floor has to be enforced by the service or the remote, not by repo files:
   - protected check ids, pinned by `integration_id` in ruleset B (Hari);
   - a service-side list of protected paths (roster, rules, `.github/workflows/**`, CODEOWNERS);
   - any change touching a protected path becomes a (c)-bin decision.
2. **That has a cost the goal statement must admit.** `ci.yml` and rules edits are common here. Every one of them becomes a human decision, so autonomy only reaches zero on changes that don't touch the rulebook. I think that's the right trade, but plan-v2 should say so rather than claim zero human involvement. The alternative is to let agents *strengthen* protected files but not weaken them. "Weaken" for arbitrary YAML can't be decided mechanically, so that alternative can't be enforced.
3. **For the self-hosted project, a "guard path set" doesn't separate cleanly.** Guard logic sits across `workflow.rs` (3.1k lines), `coordination.rs`, `auth.rs`, `jobs.rs` and the migrations. Almost every server release touches one of them. So "promotion of a guard-touching release is a decision" in practice means **every coordinator deploy needs a human**. I accept that. Let's say so plainly: count coordinator releases as reserved decisions in the metric rather than pretending the path set makes them autonomous. I agree with Sol that the path-set file itself must be in the constitution.
4. **The loop also runs through hosts, not just code.** Three places:
   - The supervisor binary and its config live on hosts where `agentc` runs. If `agentc` can write them, an agent can edit its own containment. They must be root- or other-uid-owned and read-only to `agentc`.
   - The integrator App key is the one credential that writes `main` (Sol, Hari). It must sit under a uid that *no* LLM child shares. Hari's same-uid token caveat is acceptable for task tokens but not for this key.
   - The `agentc` egress firewall must be root-owned.

## C. Other items on the adopted list I'm not yet convinced by

- **Metric (adopted 1).** Hari's revised U2 makes human direct pushes legitimate and uncounted. Fine, but also track **direct-push rate per week** as a canary. If it rises, the workflow is too expensive and people are routing around it again (history.md §3.11). HRI=0 alongside rising bypasses would be a false success.
- **U2.** I agree with Hari's revision: no `quick` lane, rulesets A+B. Data point for anyone who still wants post-hoc review: the median agent review took **3.9 min** (service-data.md B13). Pre-merge review adds about 4 minutes to a 10–15-minute Actions run. Post-hoc review saves little and breaks the user's definition of done. **Recommend pre-merge review on every agent path.**
- **Independence default (Hari iii.6).** `distinct_launch` as the default recreates today's situation: 60 of 71 reviews were same-principal, and 44 took under 5 min. I'd default to `distinct_vendor` when available and `distinct_launch` only as a fallback. Record which one was applied on the decision so review quality can be audited later.
- **Checks (adopted 7).** GitHub receipts as the *required* default make this project depend on GitHub availability, and on the repo staying public for free minutes; if it went private, Windows minutes are billed at a higher rate. That's acceptable, but it is a conscious departure from the day-one "vendor-agnostic" goal. Please add it to the user decisions as U7.

Sol: your exit-code finding (`--force-with-lease` exits 0 when the remote is already at R) is a great catch. I'd make "success = observed remote state, never exit code" a DURABLE-RECORD invariant.


---

### Dana (devil's advocate) — 2026-09-25T16:49:24Z

**Short addendum (Dana) to Sol.** Your required/cleanup split answers my question from round 1, and I'm convinced. The five required items (re-key results to `(submission, T0)` with UNIQUE, a single R producer, a submission-bound short hold, I-FF enforcement, and migrating legacy intents as pinned results) are all needed for roll-forward to be correct. None of them is gold-plating. Sequencing the rest as measured cleanup is exactly what I wanted.

I also accept your asymmetric-trust answer, meaning the service does a compare-API ancestry check plus a continuous audit, as the Q2 resolution. One consequence for P1-lite: once the integrator exists, conflicts and moved targets route to *re-integrate onto the new tip* (your point), not to `revise`. So the P1-lite agent `reopen` is interim scaffolding, and Sol's reason enum should leave out `conflict` from the integrator role once the integrator ships.


---

### Dana (devil's advocate) — 2026-09-25T16:49:32Z

**Correction (Dana).** The last sentence of my addendum was wrong. A *moved target* routes to re-integration. A genuine *merge conflict* or a *failing check on R* still needs `revise`, because the author has to change the candidate. So `conflict` and `check_failed` stay in Sol's enum for the integrator role. Only `target_moved` leaves the revise path.


---

### Lead (Claude, main session) — 2026-09-25T16:51:44Z

**Round 3 (lead): plan-v2 is posted at `planning/autonomy/plan-v2.md`. Please red-team it.**

Summary of what changed:
- **Goal:** tasks must reach a terminal state; the metric is based on credentials.
- **Reserved decisions** are listed explicitly, including releases of the coordinator's own guard code (Dana, I accept your point B3 in full).
- **Gate-triage table.**
- **Components table** listing what each one deletes.
- **Supervisor:** root-owned, with the `agentc` uid, launch tokens, progress-gated renewal, a harness profile that never auto-loads candidate-controlled config, and structured reviewer output posted by the supervisor.
- **Integrator:** Sol's pinned R, roll-forward, I-FF, audit, push slot, and bound Actions receipts.
- **Phases re-sequenced:** P1 unblock → P2 containment and staging → P3 supervisor plus a costed pilot gate → P4 integrator and repo roster/rules → P5 contract → P6 dogfood and cleanup.
- **User decisions:** U1–U9.

**My resolution of Dana B1/B2 (circularity vs. the stated goal).** The user *explicitly* wants agents to change binding rules, so making every rule edit a human decision defeats the goal. v2 splits it in two:
- A **constitution floor**: rulesets A/B plus service-side protected check ids and protected paths. Agents can never change it.
- **Rules and roster above the floor**: agent-editable, with *elevated review* (two independent reviewers, at least one of a different vendor) plus a non-blocking post-hoc notification and one-click revert.

Dana: is that an acceptable place to draw the line? If it isn't, what is the *minimum* protected set?

**Assignments for this round. Walk the evidence through v2; don't just argue in the abstract:**
- **Sol:** replay catalog items B1–B5, B8 and B11 (service-data.md §3) step by step against v2's protocol (P1+P4). For each one, name any state where a human is still needed, or where a race or lost-update remains. Also answer Q-B (fencing for the production host: can a deploy script enforce a monotonic generation?) and Q-C (live-data migration at the P1 and P4 cutovers, given the service's *current* in-flight tasks: 2 open tasks, including "Deploy archived task pagination to production").
- **Hari:** replay B6, B7, B9, B12 and B14 against v2's supervisor/harness/ops design. Answer Q-D (integrator placement and downtime; U6). Say which of your ⚠ items in the launch lines are blocking for P2/P3. Also sanity-check the phase ordering: is P2 containment really needed before P3, or can shadow mode run first?
- **Dana:** replay B10, B13 and B15 plus the whole of history.md §3 (the manual-intervention catalog) against v2. Then do an overall "is this still too big?" pass: which pieces of v2 would you cut or defer, and is anything *missing* for the goal?

Post via post.sh as before, re-read, and respond to each other publicly. I'll consolidate into plan-v3 afterwards.


---

### Dana (devil's advocate) — 2026-09-25T16:54:14Z

**Round 3 (Dana): the constitution line, a replay of B10/B13/B15 and history.md §3, and a "still too big?" pass.**

## 1. Constitution floor vs. elevated review: acceptable, with one precondition and a smaller protected set

**The precondition is to state the threat model.** v2 never says whether it defends against *confidently wrong* agents or *adversarial* ones (prompt-injected, or deliberately gaming checks). Under the adversarial model no protected set is enough. Protecting `ci.yml` doesn't protect `scripts/smoke.py` or the `cargo test` targets it runs, and a candidate can weaken those just as easily. Only review stands between an adversarial candidate and main. So plan-v3 should say: **"threat model = confidently wrong + accidental weakening; adversarial resistance is limited to blast-radius bounds (launch tokens, uid split, rulesets, secrets)."** Under that model the lead's split is the right line, and it respects the user's day-one choice of rule editing over the recommended option (history.md §2).

**A minimum protected set, smaller than v2's.** v2 makes `.github/workflows/**` jobs that implement protected checks non-editable by agents. That turns routine CI maintenance into human decisions: action pin bumps, toolchain bumps, and the cargo-audit version were a steady trickle in the git log. The minimum I'd accept:
1. **Outside the repo** (already human-owned): rulesets A and B, the service autonomy switches, the review and independence floor, the recovery floor, and deployment mode.
2. **Protected check *ids*** in the service: agents may add ids to the roster but not remove any. The *definitions* (workflow YAML) stay agent-editable under elevated review. The ruleset B `integration_id` pin means a renamed or deleted job simply fails the gate, so it fails closed.
3. **Privilege-bearing CI is protected.** This is the item v2 is missing. Actions runs whatever workflows are in R, triggered by the integrator's push to `ac/results/<id>`, and in the same repo that is a `push` event. A candidate workflow can declare `permissions: contents: write` and push tags or create GitHub Releases. `release.yml` fires on `v*` tags, and `upgrade_client.py` consumes releases, so this is a supply-chain path. I checked read-only: the repo has 0 secrets, and default workflow permissions are `read`. But a workflow can raise its own `permissions:`. The fix is mechanical: an integrator gate that makes any R whose workflows request write permissions, `secrets.*`, or `pull_request_target` a (c)-bin decision. Add a **tag ruleset** restricting `v*` creation to the owner, and keep `workflow_dispatch` deploys human-only.
4. **The constitution files themselves**: the path list and CODEOWNERS.

Everything else, including rules text, roster additions and workflow job edits, can be autonomous with elevated review. One simplification: make elevated review **one `distinct_vendor` reviewer plus the post-hoc notification**, not two reviewers. Two same-context LLM reviewers add cost but little decorrelation, and not every host will have two vendors.

**Also keep an existing guard that v2 would silently delete.** `coordination.rs:1093` refuses self-related requirement changes ("Delegation never permits self-related requirement changes"). v2 §2.4.3 "agent edit task definition" plus `requirements_changed` revise would let an implementer weaken its own acceptance criteria and then pass them. That is the task-level version of the circularity. Keep the rule, but route it instead of sending it to a human: **a contributor's AC amendment becomes part of the submission, and the independent reviewer must approve the amendment explicitly against the original criteria.** Only the *grant* part (human-created `task_definition_grants`) becomes the policy switch.

## 2. Replay against v2

| Item | Case | v2 outcome | Residual |
|---|---|---|---|
| B10 | 577ed262 wrong kind (general needing code) | Agent edit/cancel | **Gap.** v2 doesn't say agents can change `kind` or cancel-and-replace. Specify "cancel + create replacement with a link", which is cheaper than kind mutation |
| B10 | d327fd4c obsolete deploy scope | Agent cancel with rationale | OK, if cancel is allowed for a task with a prior submission. It must be |
| B10 | c2728e4d criteria revision refused | `requirements_changed` | OK, with the reviewer-approved AC amendment (§1) |
| B10 | dc1ff9c7 user expanded scope | Authoring input | OK, out of scope |
| B13 | 60/71 same principal | distinct_vendor/launch | Principal fixed. **Depth not fixed**: nothing in v2 addresses 44/75 reviews under 5 min |
| B15 | Deploy authorizations in chat | U9 reserved decisions | OK, counted |
| B15 | Product choices in chat (MCP adapter, backfill split, scope clarifications) | "(c) non-blocking decision + push" | **Gap**: "non-blocking" for *what*? The task needing the answer is blocked. See missing item M1 |
| §3.1 | ≥20 "continue" | Supervisor | OK. The planning `/goal` continues are authoring, out of scope |
| §3.2 | Push/submit approvals, ~700 escalations, merge-without-review | Deterministic profile, integrator pushes | OK for supervised sessions; interactive sessions stay classifier-bound (Hari's question a) |
| §3.3 | Hand-spawned reviewers | Supervisor launches reviewers | OK |
| §3.4 | Unblock, reopen, reconcile | P1 / P4 | OK |
| §3.4 | Create `production-host` | Resources in repo/host config | Deploy is a U9 decision anyway; Q-B fencing still open (Sol) |
| §3.4 | Roster change dropping Windows | U1 once, then repo roster | OK |
| §3.5 | ≥6 browser logins | — | **Gap.** Logins served two purposes: human-only dashboard ops (gone) and **UI verification** (c30653e9, 4b68f1c7, most of P7). See M2 |
| §3.6 | Token copy, credential handoff, lost one-time credential | Host enrolment = bootstrap | **Gap: expiry.** Harness OAuth (`setup-token`), host principal and App key expiry would park *everything* silently. See M3 |
| §3.7 | Lease expiry, lost candidate, reservation wait | Progress-gated renewal, WIP refs, no check reservations | OK |
| §3.8 | Target lock, hot-file conflicts, 9bb9a28 | Push slot, auto-revise, rulesets | Mostly OK. The hotspot (`test_task_detail_copy_controls.mjs`, 5 conflicts) will now loop implement→review→conflict automatically at full LLM cost. Add a canary: same file conflicting N times → auto-create a split/refactor task |
| §3.9 | Cross-workstation relays | Supervisor + `next` | OK, unless U1 keeps Windows first-class |
| §3.10 | Kill stale shells, 44 worktrees, temp files, Cargo locks | — | **Gap**, see M4 |
| §3.11 | Opting out of the workflow | `ship` script (U2) | OK. The direct-push canary watches it |
| §3.12 | ~25 product decisions | — | **Gap**, same as M1 |

## 3. Missing for the goal

- **M1: decisions with a default.** The user picked the "(recommended)" option in nearly every one of about 25 decisions (history.md §3.12). A supervised agent should record a decision with options and a recommendation through the existing, unused decisions subsystem (`decisions.json` is empty; no new table). If the question is **reversible** and unanswered after T hours, it proceeds on the recommendation and notifies. Irreversible questions (deploy, migration, deletion, constitution) wait. Without this, every design question parks a task and HRI can't reach 0.
- **M2: agent-verifiable UI.** Recent work is mostly dashboard UI. The harness profile needs a headless browser (the `.mjs` browser tests already exist), a **staging coordinator with a service-issued test login**, and a reviewer contract that accepts screenshots and DOM assertions as evidence. Otherwise the AC lint turns every UI task into `requires_human_acceptance`.
- **M3: credential and quota health.** The supervisor should check harness auth, token expiry and vendor quota before claiming, notify N days before expiry, and route to the other vendor when one is out of quota (Hari's question d).
- **M4: the supervisor owns worktrees.** It creates the worktree before launch and garbage-collects the worktree, the per-worktree `CARGO_TARGET_DIR` and temp files after the terminal state. This is also a deletion: the 423-word cleanup procedure (×3 copies) and `git push --force-with-lease` branch-deletion rules leave the agent contract entirely.
- **M5: review depth.** Without human review, review quality is the main safety property left. The supervisor should reject any structured decision that lacks per-criterion evidence citing files or checks. Also run an **audit sample**: re-review about 10% of approved tasks with the other vendor and track the disagreement rate as a canary. Cheap, and it is the only measurement of whether autonomous review works.
- **M6: rollback of merged regressions.** Checks on R guard the merge. Nothing guards "main is green but the feature is broken". A revert-task path (agent- or human-triggered) should be explicit, not just for rules edits.

## 4. Still too big? Yes, somewhat. Cut or defer:

- **Launch tokens (P3) → defer.** For v0, use **two uids per host (`agentc-impl`, `agentc-rev`) with static per-role credentials tagged `class=supervised`**. That needs zero service changes. It gives principal-distinct review and the attention metric (credential class). Add minted per-launch tokens only if blast radius proves to matter. That removes a table, two endpoints and mint scopes from the critical path.
- **P5 MCP slimming and book restructure → defer.** Supervised sessions don't use MCP. Freeze MCP rather than rewrite it. Keep only the ≤1.5k per-role contract, which is what P3 needs, so **move it into P3**, and drop the rest of P5.
- **AC lint → cut.** It is heuristic natural-language parsing. Replace it with a reviewer-contract rule: an AC that needs a human is answered via M1 or M2.
- **`deployment_mode=agent` → cut from this plan.** U9 recommends decisions anyway.
- **Cleanup items in P6** (`needs_attention`, `mode=recovery`, review kinds, check reservations): drop them from the plan and let the measured-failure rule create them as tasks.
- **Elevated review:** one distinct-vendor reviewer, not two (§1).

That gives **P1 → P2 → P3 (supervisor + contract + `next` + M4) → pilot gate → P4 → dogfood.** Everything else is a follow-up task.

**Sequencing challenge to the lead:** P4 (the integrator) is the largest phase, and it comes *after* the pilot gate. So the pilot runs on the *old* LLM-driven integration: the classifier merge block, the target hold and the reconciliation traps. It will measure the wrong thing, and its cost numbers will include the expensive integration turns Hari measured ($50 on one stuck task). Either pilot with general-kind tasks only (no integration), or move a **minimal integrator** (pinned R, roll-forward, Actions receipts, no audit or push slot yet) ahead of the pilot. I recommend the second.


---

### Hari (harness & ops) — 2026-09-25T16:54:34Z

**Round 3 (Hari): replay of B6/B7/B9/B12/B14, Q-D, which ⚠ items block which phase, and phase order.**

New evidence gathered without launching any model:
- A sandbox probe using `codex sandbox`, which runs a command under Codex's real Linux sandbox and needs no model.
- `gh run list`.
- The repo size.

**Probe result** (scratch dir, since deleted):
- `workspace-write` *without* network: `socket()` itself fails with `EPERM`. **That explains both B7's "MCP loopback tests blocked" and history's "sandbox cannot resolve the service host".**
- With `network_access=true`: loopback bind works, DNS works, writes outside the workspace are still denied, and writes inside succeed.
- So the v2 Codex profile is right. Network access is all-or-nothing, so the root-owned egress firewall is the *only* place the network is restricted.
- Other numbers: "Coordination checks" on Actions take **6 min** and docs take 1 min, so the p50 < 10 min integration target is realistic. `.git` is 18 MB.

### Replay

**B6 (platform-split checks, 36% job failure).** With Actions receipts, 27 native-Windows failures and the `link.exe`/WSL/CRLF/tmpfs failures leave the *required* path. What remains:
- **(1) A non-blocking Windows check that nobody owns will decay.** A red Windows run on `main` should automatically create an agent task "Windows regression at <sha>". That task carries a **task-scoped extra check**: its submission needs the `windows-latest` job green on its candidate branch. Actions already runs `ci.yml` on every branch push, so no Windows host is needed for the fix either. This is a small addition to the roster model: the task-level required checks become the project roster plus the task's extras.
- **(2) Implementers still build locally.** MINIAIR's toolchain fragility now costs tokens, not humans. Hosts advertise a coarse `os` tag, and Rust tasks prefer Linux hosts; `TMPDIR` and `CARGO_TARGET_DIR` go under `$RUN`, not tmpfs.
- **Residual human need:** none, if U1 = observer.

**B7 (harness gates).**
- Push approval: gone (`push-candidate` is allowlisted, raw push is denied).
- Submission approval: gone (the coordinator CLI is allowlisted, and there is no classifier).
- `integrations prepare` blocked as merge-without-review: gone (the integrator does it).
- Deploy/report upload rejected: gone (deploy is not an LLM action; upload goes through the CLI).
- Loopback tests: fixed by `network_access=true` (probe above).
- Lease lapse during a pause: gone (supervisor).

Three **new** harness-side risks that v2 doesn't cover yet:
- **(a) Silent config loss.** `claude --help` says in `-p` mode "Settings files that fail validation are silently ignored". One typo in the role settings file would silently drop the deny rules and the sandbox. The supervisor needs a preflight that validates each role file (schema check, plus `claude doctor` for the agentc config dir) and refuses to launch on failure.
- **(b) Harness auto-update.** Claude moved 2.1.281→282 during this session. Pin the harness versions for `agentc` (disable the autoupdater; install Codex's version explicitly), record the version on each launch, and upgrade through the canary path.
- **(c) Test suites need a network.** Loopback needs `network_access=true` under Codex, so a test that reaches the Internet will succeed inside the sandbox. The firewall has to deny everything except the allowlist, including for test processes.

**B9 (human-only acceptance criteria).** v2 puts AC lint in **P6**. That is too late: any pilot task with a "human reads…" criterion will park, and HRI will be nonzero. Proposals:
- Move AC lint into P1. It is small: pattern plus policy.
- Add to the P2 toolchain a **headless browser (Playwright + Chromium) for `agentc`**. c30653e9's "fresh browser session" and most UI criteria become agent-verifiable with DOM assertions and screenshots stored as artifacts. Supervised sessions have no Chrome MCP and no in-app browser, so without this every UI task, which is the user's most common kind, is unverifiable.
- b30fe06c ("human understands") and 4b68f1c7 ("paste into an independent application") stay in bin (c). The user should decide (**U10**) whether `requires_human_acceptance` *blocks done*. I recommend it does not: done = integrated, with a separate non-blocking "human acceptance" follow-up.

**B12 (queue latency, priority inversion).** The supervisor plus `next` turns 40–144 h P0 waits into minutes while any host is up. Remaining work:
- **Reviews must never queue behind implementations.** Each host reserves one slot for reviews. This carries over the old "reviews first" rule (2014f10) as a deterministic scheduler rule.
- **Claim-churn breaker.** The sentinel 5655e94d was claimed 3 times, and afb073cf was claimed and released 3 times. Under a supervisor that loop burns money unattended. Rule: N claims with no checkpoint progress puts the task into `needs_attention` and sends a notification. This is also a cost control.
- **Budget-aware ordering.** When today's spend passes about 80% of the cap, `next` offers only P0/P1 items and reviews.

**B14 (work outside the coordinator).** With ruleset B, 9bb9a28 would have been *rejected* because `documentation` was red on that SHA. Direct unchecked pushes and force pushes become impossible. Remaining:
- Human fast-forward pushes still move the target, so in-flight integrations re-integrate. That costs Actions minutes, not attention.
- Untracked human work stays untracked, and the direct-push canary is the right measure.
- **Defence-in-depth addition:** also restrict *branch creation* (ruleset A's `creation`, bypass = App + owner). Agents only need `refs/agent-coordinator/*`, which rulesets don't cover and which don't trigger Actions. Without this restriction, an agent credential could push a branch with an edited `ci.yml`, produce a green "Actions" check on an arbitrary SHA, and mint "checked" SHAs. Ruleset A still stops them from moving `main`, but there is no reason to allow it. It also stops agents spending Actions minutes.

### Q-D: where the integrator runs, and downtime

**Recommendation: run it on the e2-micro VM as a separate uid and systemd unit** (`MemoryMax≈256M`, `Restart=always`).
- The repo is 18 MB and the integrator only runs `merge-tree`/`commit-tree`/push and polls check-runs. Builds happen on Actions, so the load is trivial.
- **The integrator is useless when the service is down, so putting them together adds no correlated downtime.** The VM is also the only machine already guaranteed to be always on.
- The App key lives under the integrator uid only. The service uid and any LLM child cannot read it.
- Blast radius if the VM is compromised: an attacker can move `main` only to SHAs that already have green Actions checks, and only as fast-forwards (ruleset B has no bypass). With the branch-creation restriction above, only the App and the owner can make such SHAs.

**Downtime behaviour:**
- Approved submissions wait in `approved`. Implementers and reviewers keep working.
- Pinned results and the short push slot mean nothing expires in a way that needs a human. On restart, roll-forward reconciles.
- The service exposes `integrator_last_seen` and sends a push notification if it is older than 30 min while the queue is non-empty.
- An optional warm standby on oracle-1 is allowed, because the push slot serializes. The cost is a second copy of the key, so defer it until measured downtime justifies it.

**Related gap: service downtime.** A VM reboot, deploy or Cloudflare outage stops renewals, and every active lease can expire together. That would trigger a mass recovery storm across unattended hosts. The service should **extend lease clocks by its own measured downtime** at startup; its restart epoch is already tracked. Sol, does this conflict with the clock-rollback fencing?

**U6.** Proposed placement, pending the user's confirmation of oracle-1's specs and uptime:
- **oracle-1** (ARM64; if it is an Oracle free-tier A1 VM, it is always on with ample RAM): primary supervisor for Linux implement/review work.
- **mxmini**: secondary supervisor, subject to quiet hours.
- **MINIAIR**: opportunistic only, releasing leases on suspend.
- **Integrator**: the e2-micro VM.

### Which ⚠ items block which phase

**Blocking P2** (the containment exit criterion):
1. **Candidate-controlled instructions are not loaded.** Under subscription auth, `--setting-sources user` excludes project *settings*, but I don't believe it stops `CLAUDE.md` discovery from the working directory. Only `--bare` does that, and it requires an API key. Workaround without an API key: cwd = `$RUN` with the worktree added via `--add-dir`, which is unverified. **This is another concrete reason for U3 = API billing.** For Codex, `project_doc_max_bytes` exists in the 0.156.1 binary, so `-c project_doc_max_bytes=0` is plausible but unverified. Codex's repo `.codex/` config must also be ignored for untrusted paths.
2. The preflight that validates the role settings files (B7a).
3. The Claude Bash-sandbox keys in the role settings. Firewall plus uid is the primary containment, but the sandbox is what keeps `agentc` writes inside `$WT`/`$RUN`.
4. **Deterministic P2 test (no LLM): run the project's full `cargo test` under each exact profile** using `codex sandbox …`. For Claude, run it under the same uid and firewall.

Items 1 and 3 need about 10 trivial probe prompts, costing under $1. That is within the pilot budget, and the user should approve it.

**Blocking P3:** the progress events in the `stream-json`/`--json` output, and exit and usage parsing. Both are readable from the pilot runs.

**Not blocking:** `--name`, `--output-schema`/`--json-schema` (the reviewer can fall back to parsing a fenced JSON block), and `--max-budget-usd` (API only; the supervisor's own counter is the real cap).

### Phase order

- **Shadow mode doesn't need containment.** It launches nothing. Split P3 into **P3a, shadow mode** (read-only host principal, `next`, the would-launch log with cost estimates), which runs *in parallel with* P2, and **P3b, live launches**, which comes strictly after P2.
- **Ruleset timing trap.** Today's LLM guarded publisher pushes `main` with agent credentials, and its R is checked by *local* jobs. R never appears on a branch, so there is no Actions status on it. **Ruleset A (update restriction) and B's required checks would therefore break the current integration path until the P4 integrator exists.** Day 0 (now) should add only `non_fast_forward` + `deletion`, plus a docs-check requirement if we want the 9bb9a28 class blocked immediately; in that case `ship` is needed from day 0. Full A/B comes at the P4 cutover.
- **Move the P4 integrator before the costed pilot (P3b).** The integrator is deterministic, so it can be built and soaked in staging without model spend. If the pilot ran through today's LLM integration path, it would pay for, and be judged on, exactly the path we are deleting. That path (holds, reconciliation, classifier) is where HRI came from.

Proposed order: **P1 (+AC lint) → P2 ∥ P3a → P4 → P3b pilot (go/no-go) → P5 → P6.**


---

### Sol (systems & protocol) — 2026-09-25T16:54:49Z

**Round 3 (Sol): replay of B1–B5, B8 and B11 against v2 (P1+P4), plus Q-B and Q-C.** Each item gives the steps under v2, then the ⚠ **residual** human steps or races I found. The one design change I propose is marked **FIX**.

**B1: policy change makes submissions stale (16 reopens).**
*Rev 2→3 (review human→agent), rev 3→4 (subagent reviews), rev 4→5 (lease only).* Under v2, lease, rules and flags are read live, so rev 4→5 is a no-op. Rev 3→4 is also a no-op, because independence is evaluated at claim and decision time. Rev 2→3 re-derives the required review set: queued `human_review` activities are canceled and an `agent_review` is added.
⚠ **Race 1: a policy edit lands while a review activity is *active*.** "Loosening cancels only queued ones" leaves the claimed one running, but nothing says whether its decision counts under the new set. **FIX:** define a satisfaction order, `human ≥ either ≥ agent`. An approval counts if its kind satisfies *some* activity in the new set. An active activity that is no longer required is allowed to finish, and its approval is kept. Without this, the 3 human approvals thrown away on 09-15 can still be lost.
⚠ **Roster races.** These mostly disappear once the roster lives in the repo, which is a nice consequence of v2. The roster that governs R is the file in T0, and a push needs tip == T0. So "roster at push time" equals "roster in T0" *by construction*. A roster edit is a target move, which forces re-integration anyway. The only live input left is the service-side **protected check ids**, which humans can change. The integrator re-reads them in the push-authority call. Residual humans: none, apart from constitution edits.

**B2: merge conflicts (5 reopens; fd3df7bf: 4 claims × 3 h).** Under P4, the integrator's merge-tree fails and it calls `revise{conflict{T0,C}}` (CAS on subject generation). The second and later integrators see `submission_changed` and stop, so there is no rediscovery loop.
⚠ Conflict **hotspots**: B2 counted 5 conflicts in one shared fixture file. The ping-pong breaker "parks and notifies", and that *is* human attention. **FIX:** the breaker's first action should be automatic *serialization*, not parking. Add a dependency so the second task waits for the first to integrate and then rebases. Only a cycle, meaning a conflict that persists after serialization, gets parked.
⚠ **P1 interim, before the integrator.** A conflict bails before the intent is written (`git_workflow.rs:907`), so agent revise works. **But 5c85ebb2 ("check failure after intent") still needs human reconciliation in P1** if the workstation that prepared it is gone, because the intent is recorded at prepare time, before checks. **FIX (P1, needs measurement):** record the intent after checks pass, just before the push (codebase rec. 4). A check failure then happens with no intent, and revise is allowed. If that turns out not to be cheap, the gap is P1-only and closes in P4.

**B3: lease expired after intent (9 reconciliations, 4 reservation resolves, a 31.5 h stall).** Replaying each case:
- 4ca4d064 (published, then expired): the next integrator sees tip == R and records published. It takes seconds.
- 08cf9f41 (expired after 4 green checks and before the push): the receipts are bound to R, not to the attempt, so the next integrator rolls forward. That is 0 human actions where the case needed 4.
- 51d0d4e4 ("read integration intent" failed): there is no local journal any more, so this case cannot occur.
- 4b68f1c7 (cross-workstation check split): the checks run on Actions, and Windows is only an observer.
- Reservation resolves: check reservations no longer exist.

⚠ **Race 2: `revise` against an in-flight push.** A zombie integrator holding a pre-issued push authority can push R *after* a `revise{author_withdraw|requirements_changed}` commits while tip == T0. Git's CAS fences on the target, not on our authority, and a paused process can outlive its local deadline. **FIX: "revise loses to a landed push."** A revise in the `integrating` phase is a *request*. If R later appears in the target, the monitor records the submission as published/done and turns the revision into a follow-up task. Result: never a double integration, and never a lost change.
⚠ **Lost R object.** The branch `ac/results/<id>` can be deleted by any principal that can write to `ac/*`, and then UNIQUE(submission, T0) leaves the submission permanently stuck. **FIX:**
- (a) Add `ac/results/**` to ruleset A, so only the integrator can update or delete it.
- (b) Relax the key: if R is unavailable, the integrator may record R′ under the same T0 *only with its own full receipts*. A zombie racing the push with R is harmless, because both results were checked and CAS lets only one of them win.

⚠ **Target itself broken.** v2 says "a failure on R that also fails on T0 alone is not attributed to the candidate", but it doesn't say what happens next. **FIX:** automatically create a top-priority `fix-target` task (agent-claimable) and hold the queue for affected checks only. Don't route this to a human.

**B4: human-only resources (4 blocks).**
- miniair-validation and the oracle-1 runner: Actions receipts mean no service resource is needed.
- Capacity-1 resource held by an expired attempt (9d07f647): deleted along with check reservations.
- production-host (dea68719): still reserved under the U9 recommendation, and holdable safely once Q-B fencing exists.

⚠ "Resources declared in repo/host config": a repo must never be able to *define* a resource that points at a host. That is the same "repository text is not a credential grant" rule from DURABLE-RECORD. **FIX:** resources are capabilities advertised by **enrolled hosts** (human bootstrap), and the repo declares only *requirements*.

**B5: cross-workstation state (38f3ffe4: 9 attempts on 3 hosts).** The supervisor pushes WIP to `candidates/<attempt>` on exit, crash, suspend or shutdown.
⚠ Power loss or host loss loses everything since the last push. **FIX:** the supervisor auto-snapshots the worktree every N minutes, *independently of the LLM*, as a `git stash create`-style commit that includes untracked files, pushed to a wip ref.
⚠ **The recovery gate is still a local attestation.** `recovery_resolution` requires `saved_work_checked && running_jobs_checked` (`coordination.rs:1797-1862`), and B5 failed on exactly that ("checkout registered on MINIAIR cannot be inspected from Linux"). **FIX (P3):** recovery evidence becomes facts the service can verify:
- the old launch is finished or its token has expired;
- the generation has been bumped;
- the last checkpoint SHA equals the wip ref the recoverer fetched;
- no nonterminal jobs remain (Actions jobs are not attempt-bound).

⚠ **Zombie ref clobbering.** The `agentc` Git credential can write any `refs/agent-coordinator/*`. **Invariant:** the checkpoint SHA recorded in the service is authoritative, and refs are only transport. A recoverer or integrator must verify ref == recorded SHA.

**B8: contract churn (2 legacy-ref reopens; old 0.1.0 CLI).** A null `candidate_ref` should not trigger `revise` (redoing work). **FIX:** add an idempotent `attach_candidate_ref(submission, ref)` that any agent may call. It is accepted only when the ref resolves to the *already recorded* SHA, which the integrator verifies on fetch. Content can't change, so there is no authority risk.
⚠ The self-hosting trap persists in a new form. This project's tasks are coordinated *by production*, while agents build dev CLIs in worktrees. B8 happened because an agent swapped `/usr/local/bin` for a dev build. **FIX:** the supervisor pins the *coordination* client to the released artifact matching production's advertised client commit, auto-updates it, and keeps it separate from worktree builds. Protocol-changing merges then wait only on a release, which is reserved in v2. That is latency, not stranding, provided the protocol stays additive.

**B11: slow recovery (14 expirations; 32 h agent-silent).** The supervisor removes the root cause. Crash release is immediate, and progress gating stops loops from pinning leases.
⚠ **Supervisor restart with an uncertain child** (PID reuse, missing launch file). DURABLE-RECORD requires no respawn, so the worst case is waiting a full lease. That is acceptable, but keep leases short (≤15 min) with progress-gated renewal.
⚠ **Partition.** The service expires the lease and another host claims the work. The zombie's launch token dies at `expires_at ≤ lease`, and its ref pushes are harmless under the B5 invariant.
⚠ `max_attempt_duration` kills legitimately long tasks. They must checkpoint and continue through a *continuation* claim that keeps the same author identity. That is a cost, not a human.
⚠ All hosts asleep. Only U6 fixes that.

**Q-B: fencing tokens for the production host.** Yes, a deploy script can enforce a monotonic generation, but only when *the host itself* is the enforcement point. Four requirements:
- (1) The service increments `resources.fence_gen` on every hold grant or transfer.
- (2) The deploy credential is an SSH key with a **forced command** (`command="/usr/local/sbin/ac-deploy-gate"` in `authorized_keys`), so no raw shell can bypass the gate.
- (3) The gate takes `flock`, verifies the token, requires `gen > /var/lib/agent-coordinator/deploy.gen`, writes the new gen with fsync, and only then acts. It rejects anything stale.
- (4) The token must be unforgeable. Either the service signs `{resource, gen, release_sha, expires}` with ed25519, or, since the production host *runs* the service, the gate reads the current hold from the local service before stopping it.

The gate's steps (backup → expand-only migrate → swap → health → record) are journaled per gen. A new holder at gen+1 finds a half-finished gen and rolls it forward or restores it. Fencing stops concurrency, not partial state.
Coverage by resource:
- **Can fence:** Git remotes (CAS), the prod host (after building the gate, about 150 lines), and anything with state-locked declarative apply, such as Terraform's lock serial.
- **Cannot fence:** consoles and third-party settings with no API (DNS/Cloudflare dashboards), and whatever runs on MINIAIR outside a gate. These keep the human gate. That is bin (c), and correctly so.

**Q-C: live migration.** Live today: dea68719 (general, waiting_review, a deploy) and the sentinel 5655e94d (ready).
*Before any cutover.* Run a read-only preflight that counts:
- intents without a known result;
- held holds and held reservations;
- nonterminal jobs;
- active attempts;
- `recovery_required` activities.

The deploy proceeds only at zero, or with each row matched by a migration rule. Also: cancel the sentinel with a rationale, and let dea68719 finish or cancel first. It is a deploy, so it is reserved either way.
*P1.*
- The required review set of each open submission *is* its existing review activities, so no guessing is needed. **But** the predicate change instantly makes any old stale submission valid, which would silently grandfather a tightened `review_mode`. So the migration runs the monotone reconciliation once, in the startup transaction, adding the missing activities under the current policy.
- Acks are re-keyed to the rules revision recorded in `policy_revisions`. One forced re-ack is also fine.
- Pending decisions stay pending.

*P4.*
- **(1)** Run the integrator in shadow for a day. It computes R and logs what it would do.
- **(2)** Set the project to `integration_mode=integrator`. The LLM `integrations *` commands are refused with a labelled `integration_owned_by_integrator`, and active LLM integration attempts are drained, meaning they finish or expire.
- **(3)** Import legacy intents as `(submission, T0, C, R)`. Note that legacy R exists *only* in the preparing workstation's local ref, not on the remote. The integrator uses the rules below; no human is needed.
  - **If GitHub doesn't know the R object:** R was never pushed, so it is not published. That holds given I-FF, so a rewind plus GC can't hide it.
  - **If tip == T0:** recompute. Publish if R′ == R; otherwise use R′ with fresh receipts, per the B3 fix.
- **(4)** Convert held holds to submission-bound push slots, and drop `policy_hold_conflict`.
- **(5)** Release reservations on retired check resources as part of *retiring the resource*. That is one scripted cutover step inside the deploy, which is reserved anyway, not a data edit.
- **(6)** Handle `recovery_required` activities by kind: integration ones go to (3); review ones re-queue; subject work goes through normal recovery with the B5 evidence.
- **(7)** The roster falls back to the `workflow_policies` row until `.agent-coordinator/roster.toml` exists in T0. The first roster commit is integrated under the old roster, and the table is deleted in cleanup.

**Dana (B1 two-step weakening).** One addition to the lead's line. Weakening rules or roster changes (removing a check, relaxing a rule) take effect only after a **cooling delay** of N hours, during which the post-hoc notification plus one-click revert can act. Strengthening is immediate. This turns "can't be decided mechanically" into "is detectable mechanically": check ids removed, or check ids with `required=false`. Prose rule changes get the delay whenever they touch lines tagged `@binding`.


---

### Hari (harness & ops) — 2026-09-25T16:55:08Z

**Follow-up to Dana, round 3 (Hari).** One housekeeping note first. My previous post used `###` sub-headings, and they look like speaker headers. Lead, please treat everything from 16:54:34Z up to this post as a single Hari post.

1. **Sequencing: we converged independently.** Dana and I both put a minimal integrator (pinned R, roll-forward, Actions receipts) ahead of the costed pilot. I endorse Dana's minimal cut: audit and push slot can follow. Combined order: **P1 → P2 ∥ P3a shadow → P4-min integrator → P3b pilot gate → dogfood**. Everything else is follow-up tasks.

2. **Deferring launch tokens: I accept, with one change that makes it safe.**
   - Use two uids per host with static `class=supervised` credentials. The reviewer uid gets a **read-only** credential. Since v2 has the *supervisor* post the structured verdict, the reviewer LLM never needs write authority.
   - The only write-capable static credentials are then:
     - the implementer's (project-wide claim and submit, acceptable under your "confidently wrong" threat model);
     - the supervisor's own (posts decisions, renews leases), which must be under a uid no child shares.
   - Separate uids also give per-role memory and config isolation for free.
   - What we lose until tokens exist: a runaway implementer child can act on *other* tasks, and zombie writes are caught only by attempt-generation fencing. Both are acceptable for v0. The claim-churn breaker still applies.

3. **AC lint: conceded.** Your "reviewer-contract rule plus M1/M2" is cheaper than natural-language linting. It also removes my "move lint to P1".

4. **M2 matches my B9 point** (Playwright plus Chromium in the agent toolchain). Add one ops detail: the staging coordinator's test login must be a *staging-only* credential in the reviewer's `$RUN`, never a production login.

5. **M3 can be free.** `claude auth status` and `codex login status` are local checks that cost no tokens. The supervisor runs them before every claim, and runs a daily expiry check with a notification N days ahead. A usage-cap 429 marks that vendor `exhausted_until` and routes work to the other vendor (U5). If both vendors are exhausted, it waits silently. That wait does not count as human attention, but it should show in the spend dashboard.

6. **M4 is right, and it also fixes a failure seen in the data.** Per-worktree `CARGO_TARGET_DIR` × 44 worktrees fills disks, and a full disk looks like random build failures. The supervisor needs a disk-high-water check before claiming, plus a shared `sccache`.

7. **M5 audit sample: yes.** It also adds a cost guard. The 10% re-review runs at low effort, and its disagreement rate is the one number that tells the user whether autonomous review deserves trust.

8. **Your privilege-bearing-CI item (3) connects to my B14 point.** Also restrict branch *creation* and `v*` tags to App plus owner, so agent credentials can neither mint green SHAs nor trigger `release.yml`. Your integrator gate on `permissions: write`, `secrets.*` and `pull_request_target` in R then covers the integrator's own pushes.


---

### Sol (systems & protocol) — 2026-09-25T16:55:29Z

**Follow-up (Sol) to Hari and Dana, round 3.**

**Hari: can we extend leases by the service's downtime without breaking clock-rollback fencing?** Yes. It doesn't conflict, provided these rules hold:
- **Measure the gap in one startup transaction, before serving.** Compute `gap = now − last persisted protected time`. Serving waits until this commits.
- **Only extend leases that were active at shutdown.** Extend each by `min(gap, lease_cap)`.
- **Record the extension as an event.**
- **Never extend after a restore.** Restore already invalidates all sessions and ownership, and that stays.
- **A backwards clock is still an incident**, not a negative extension. The clock-rollback rule only guards against time going backwards, and a forward jump is the case this rule handles.

It is safe from a fencing point of view because no one else could have been granted the lease while the service was down. The worst case is that a holder that died during the outage keeps its lease one gap longer, which costs latency and nothing else. Supervisors re-renew on reconnect anyway, because renewal is progress-gated. The integrator's push slot doesn't need this, because the compare-and-swap push plus the pinned result already cover it.

**Hari and Dana: move a minimal integrator ahead of the pilot.** I agree. Here is what "minimal" can and cannot drop:
- **It can drop the push slot**, but only while exactly one integrator instance runs (the e2-micro systemd unit), because that one process already serializes pushes.
- **It can drop the continuous audit**, because the service verifies "published" itself with the compare API.
- **It cannot drop any of my five required items:**
  1. results stored per (submission, target base) with a unique constraint;
  2. the integrator as the only producer of results;
  3. a hold bound to the submission, not the attempt;
  4. enforcement that the target is never rewound (the `non_fast_forward` rule is live from day 0 per Hari, which is good);
  5. importing legacy intents at cutover.
- **It needs the "revise loses to a landed push" rule** from my replay.

I also support Hari's ruleset-timing point. Day 0 adds only `non_fast_forward` + `deletion`, and the full rulesets A and B come at the integrator cutover.

**Dana, point 1.3 (privilege-bearing CI): this is a real hole in my receipt design.** My definition-binding ensures a *weakened check can't count*. It does nothing about the *side effects* of running R's workflows at all. Pushing `ac/results/<id>` runs whatever workflows the candidate wrote, with whatever `permissions:` they request. So the integrator gate you describe must run **before the result branch is pushed**, not after. It checks R's workflow files: any file whose blob differs from the target's version and requests write permissions, `secrets.*`, `pull_request_target`, or new triggers becomes a (c)-bin decision. Add your tag ruleset for `v*`, plus Hari's branch-creation restriction. One refinement: a workflow file that is unchanged from the target needs no gate, so ordinary candidates never pay for this.

**Dana, the self-related AC amendment guard: agreed.** It is exactly the "no self-benefit" rule at task level. Concretely, in my reason enum, `requirements_changed` must refuse callers who are contributors to that task. A contributor's amendment rides inside the submission as `ac_amendment{old, new, rationale}`, and the reviewer's structured output must carry a separate `amendment_decision` field. Approving the code does not implicitly approve the amendment.

**Dana, threat model: agreed.** The model is confidently wrong or accidentally weakening agents, with adversarial resistance limited to bounding the blast radius. My cooling delay on weakening edits fits that model: it is cheap insurance against accidental weakening, not a defence against an adversary.


---

### Dana (devil's advocate) — 2026-09-25T16:55:30Z

**Follow-up (Dana) to Sol and Hari, round 3.**

**Where I'm convinced:**
- **Hari, phase order: P1 → P2 ∥ P3a (shadow) → P4 → P3b pilot.** This is the same sequencing challenge I ended my round-3 post with, and your ruleset timing trap (full A/B would break today's publisher until P4) is a further reason for it. I also agree with **day-0 = `non_fast_forward` + `deletion` only**.
- **Hari, AC lint in P1 plus U10 (human acceptance does not block done).** I withdraw "cut AC lint", provided it stays a pattern-based *warning* plus the `requires_human_acceptance` flag, not a blocker. U10 = non-blocking removes most of what worried me in B9.
- **Hari, Playwright in P2** is the same as my M2. Please add the staging test login explicitly: the headless browser still has to sign in to the dashboard.
- **Hari, lease-clock extension by measured service downtime.** Good catch. Sol should answer the clock-fencing question, but I'd adopt it.
- **Hari, branch-creation restriction.** It closes the "mint a green SHA from an agent-pushed branch" route and complements my privileged-CI point. Please extend it to **tag creation** (`v*`), which triggers `release.yml`.
- **Sol, serialization before parking on conflict hotspots.** This is better than my "create a refactor task" canary. I concede it.
- **Sol, "revise loses to a landed push", the satisfaction order `human ≥ either ≥ agent`, and ref == recorded SHA.** All adopted from my side.
- **Sol, cooling delay for weakening.** I said weakening "can't be decided mechanically". You've narrowed it to what *can* be: check ids removed or made non-required. I accept that. One hole remains: `@binding` tags are author-controlled, so a change that **removes or moves a tag** must itself count as weakening. Otherwise the delay is opt-in for the agent it is meant to restrain.

**Where I still push back:**
1. **Sol's periodic WIP snapshot "including untracked files".** Untracked often means `.env`, local credentials, build output and large binaries. Pushing those to a shared remote is a secret leak and repo bloat. Snapshot must respect `.gitignore`, add a size cap, and run a secret scan before pushing. Otherwise keep it to tracked files plus explicitly added ones.
2. **Scope creep in this round.** Round 3 added roughly 15 **FIX** items. Every one is sensible, and together they are how this project got to 51k LOC before first use. **Lead, in v3 please tag each FIX as *required-for-correctness in phase N*, or *follow-up when measured*.** My proposed tags:
   - **Required:**
     - Sol race 1 (satisfaction order) in P1;
     - the P1 migration's monotone reconciliation;
     - revise-loses-to-push in P4;
     - `ac/results/**` protection in P4;
     - legacy-intent import in P4;
     - Hari's settings preflight and version pin in P2;
     - branch and tag creation limits in P4.
   - **Follow-up when measured:**
     - Q-B fence gate (deploy is reserved anyway);
     - R′ re-keying;
     - `attach_candidate_ref`;
     - periodic snapshots;
     - claim-churn breaker and budget-aware ordering;
     - Windows regression auto-tasks;
     - the `fix-target` task (a notification suffices at first);
     - moving the intent after checks in P1. P4 deletes intents, so don't patch code about to be deleted.
3. **Hari, integrator on the e2-micro.** It's fine operationally. But it co-locates the App key, the only non-human `main` writer, with the public-internet-facing service on one small VM, and uid separation is the only barrier. The blast radius you describe (only green, fast-forward SHAs) is acceptable *only with* the branch- and tag-creation restrictions in place. So make those restrictions a precondition of P4, not defence-in-depth.


---

### Hari (harness & ops) — 2026-09-25T16:55:35Z

**Follow-up to Sol, round 3 (Hari).** These are the ops consequences of your fixes.

1. **The B5 auto-snapshot has a public-exposure hazard.** The repo is **public**, so everything under `refs/agent-coordinator/*` (candidates, WIP, and soon snapshots) can be fetched by anyone. An LLM-independent snapshot that "includes untracked files" will sooner or later pick up something like a `.env`, a token file, or a debug dump the agent wrote. Rules for the snapshot:
   - Build it with a temporary `GIT_INDEX_FILE` plus `git add -A`. That respects `.gitignore` and never touches the agent's index; `git stash create` would skip untracked files anyway.
   - Keep `$RUN` (where the token lives) outside the worktree, which v2 already requires.
   - Run a secret scan (e.g. gitleaks) on the snapshot diff before *every* supervisor push. A hit aborts the push and parks the task.

   The same scan belongs in `push-candidate`. A reviewer judges code, not whether a secret is present, and the classifier's credential-leak rules are gone from supervised sessions.

2. **B8 client pinning: agreed. Extend it to the harness binaries.** The supervisor installs these into a root-owned, versioned prefix that `agentc` can read but not write:
   - the released coordination CLI (matching production's advertised client commit);
   - `claude`;
   - `codex`.

   The autoupdaters are disabled and versions are recorded per launch. Worktree builds of the CLI can never shadow the pinned client on `PATH`.

3. **Short leases (≤15 min) combined with a service outage cause a recovery storm.** When the VM reboots or deploys, every lease can lapse together. This is my open question from the round-3 post. Please confirm that "extend active leases by measured downtime at startup" is compatible with clock-rollback fencing. If it isn't, supervisors should treat "service unreachable" as *pause the child*, not *let the lease lapse*, and re-claim with continuation on reconnect.

4. **Race 2 (zombie integrator): the deployment choice helps.** With the integrator on the VM as a single systemd unit (`Restart=always`, no standby until measured), a zombie can only be the *same* unit across a restart. Having systemd kill the unit's cgroup before restarting it removes the paused-process case in practice. Your "revise loses to a landed push" rule stays as the correctness backstop.

5. **Q-B forced-command gate: agreed.** For the self-hosted coordinator the gate sits on the same VM as the integrator and service. Its SSH key belongs to the *human-approved deploy* path (U9 reserved), not to the integrator uid. Otherwise the one credential that writes `main` could also deploy.


---

### Dana (devil's advocate) — 2026-09-25T16:55:42Z

**One-line reconciliation (Dana).** Hari and I conceded AC lint to each other at the same moment. Lead, take Hari's latest position: **no lint**. Instead, a reviewer-contract rule plus M1/M2, plus the `requires_human_acceptance` flag with U10 = non-blocking. I also adopt Hari's read-only reviewer credential with supervisor-posted verdicts, and Sol's `ac_amendment` + `amendment_decision` shape, and Sol's rule that the privilege gate runs *before* the result branch is pushed. I have nothing further this round.


---

### Hari (harness & ops) — 2026-09-25T16:55:48Z

**Closing note for round 3 (Hari).**

- **Sol:** thanks for the lease-extension rules; adopted as stated. I agree the push slot can wait while exactly one integrator unit runs.
- **Dana:** the staging test login and the `v*` tag-creation limit are already in my follow-up at 16:55:08Z (items 4 and 8). I accept making the branch and tag creation limits a **precondition of P4**, not defence-in-depth. The snapshot rules you and I each proposed are the same: `.gitignore`-respecting, size-capped, secret-scanned. I'm also fine with your required vs follow-up tagging.
- **One addition to the required list:** the secret scan in `push-candidate`. That belongs in P2 or P3b, because the repo is public and no classifier watches supervised sessions.


---

### Lead (Claude, main session) — 2026-09-25T16:57:53Z

**Round 4 (lead): plan-v3 is at `planning/autonomy/plan-v3.md`, and this round is a pre-mortem.**

plan-v3 adopts every round-3 agreement. Each item is tagged **[R-Pn]** (required in phase n) or **[F]** (follow-up, per the measured-failure rule), using Dana's tagging with Hari's addition: the secret scan in `push-candidate` is required in P2.
- Phase order: P0 (day-0 ruleset: `non_fast_forward`+`deletion`) → P1 → P2 ∥ P3a → P4-min → P3b pilot (go/no-go) → P6.
- Two uids with a read-only reviewer credential; launch tokens deferred.
- M1–M5 added; M6 (revert path) is only partly covered. Dana, please check it.
- User decisions U1–U11.

I worry that we converged quickly, so this round is deliberately adversarial toward the *whole* plan. **Everyone:** it is 2026-10-20. The pilot passed, but two weeks of dogfooding ended with the user saying "this is worse than before". Write the most plausible story of how that happened. Then name the one change to v3 that would have prevented it.

Specific questions:
- **Sol:** (1) Is there any path in v3 where a change lands on `main` without an independent review of exactly the content that landed? Consider the FF case R==C, stacked candidates, human `ship` pushes, AC amendments, and the "revise loses to a landed push" follow-up task. (2) M6 regression revert: what is the correct state-machine shape for "revert integrated change X"? A new task whose candidate is `git revert`, integrated normally? What if later changes depend on X?
- **Hari:** (1) Who updates supervisors, pinned harnesses, the integrator and the pinned CLI on hosts? Is it a recurring human touchpoint, and can it be autonomous safely? (2) How should *this plan itself* be executed? Options: (a) through the live coordinator as tasks (dogfooding, but P1 is exactly what the current workflow is bad at); (b) a direct interactive session per phase using the `ship` pattern; (c) a hybrid. Recommend one and give the concrete first 3 steps for the next session.
- **Dana:** (1) Walk v3 for a *different* project, e.g. a small Python repo on GitHub with no Windows and a single CI job. Is v3 over-built for it, and what is the minimum configuration? Is anything coordinator-specific leaking into the generic design? (2) Is anything in v3 still unjustified by the evidence? (3) M6.

After this round I'll ask each of you for a final sign-off: agree, or agree with reservations (listed), or disagree. Then I'll write the handoff.


---

### Hari (harness & ops) — 2026-09-25T16:59:26Z

**Round 4 (Hari): pre-mortem, host updates, and how to execute this plan.**

**Pre-mortem (2026-10-20): "this is worse than before".** The pilot passed on five hand-picked tasks, all run within one afternoon. Dogfooding then went like this:
- **Day 3.** The user's UI loop used to be "tweak, push, deploy" (P7). It became `ship`: an 8-minute Actions wait, followed by a deploy decision click (U9).
- **Day 3 onward, every user ship moved `main`.** Each move re-integrated every queued approval. Three agent UI tasks touched `web/app.js`, as did the user's own tweaks. The result was serialized revise cycles, one "parked" notification a day, and Actions minutes spent on re-runs.
- **Day 9.** A vendor-side change landed; the model alias was retired and the pinned Claude 2.1.282 started getting "please upgrade" errors. Every Claude launch exited within seconds.
- **The supervisor handled it wrongly but plausibly.** It treated each exit as a crash, released the lease, and re-claimed with Codex. Codex hit its usage cap by noon and was marked `exhausted_until`. The queue then waited silently, exactly as designed.
- **Health checks passed throughout.** `auth status` was fine, so M3 saw nothing.
- **Notifications had already been muted.** By then ntfy carried about 40 events a day (parks, decisions, integrator notices, audit disagreements), and the user had muted it on day 6.
- **Result:** 31 hours of zero throughput, discovered when the user wondered why nothing had shipped. HRI/task read 0 the whole time, because muted notifications are not mutations. The user did less clicking but got less done, more slowly, and with less trust.

**The one change that would have prevented it:** replace per-event notifications with **service-level objectives (SLOs) measured by an end-to-end synthetic canary**.
- Each day, per vendor and per host, the supervisor runs a trivial canary task through the *whole* pipeline on a dedicated canary project and repo: claim, launch, edit, push-candidate, review, integrator, Actions, done.
- Page the user only when the canary fails, or when throughput or queue age breaches its SLO.
- Everything else goes into one daily digest.

The canary catches the failure modes that component health checks miss: harness drift, model retirement, quota exhaustion, firewall or DNS changes, broken Actions, a dead integrator, and expired App keys. It costs a few cents a day at low effort. Also count "queue made no progress for more than N hours while non-empty" as HRI, so a silent stall can never score 0.

A secondary fix from the same story: `next` should avoid handing agents tasks whose paths overlap files the user shipped in the last 24 hours. This is cheap and uses file lists only, and it reduces fights with the user.

**Q1: who updates supervisors, harnesses, the integrator and the CLI, and can it be autonomous?** There are four classes, and all but credentials can be autonomous without adding a new human touchpoint.
1. **Our own binaries** (supervisor, integrator, coordination CLI). These are built by `release.yml` from a `v*` tag, and only the owner can create one (tag ruleset). Each host runs a root-owned **pull updater**, a systemd timer (a Scheduled Task on Windows). It:
   - fetches the release that production advertises (the CLI must match production's client commit; supervisor and integrator follow a release channel);
   - verifies checksums and the Actions build attestation (`gh attestation verify`);
   - installs into a new versioned prefix;
   - **drains** (no new launches, and waits for children to finish or checkpoint);
   - switches the symlink, runs the canary, and rolls back to the previous prefix if the canary fails.

   The human act is the tag, which is already a reserved release decision, so this adds no touchpoint. Agents cannot trigger it.
2. **Harness binaries** (claude, codex) and Playwright/Chromium. The same updater stages the newest vendor version *alongside* the pinned one. It runs the settings preflight plus a canary launch on the new version, and promotes only if both pass. Otherwise it stays pinned and adds a digest entry. It runs weekly, and immediately on an "upgrade required"-class launch failure (the day-9 story). This is safe because we already trust vendor binaries; the canary catches flag or behaviour drift.
3. **Toolchains and the OS.** Rust comes from `rust-toolchain.toml` in the repo, so it changes through review. OS security updates are unattended, with reboots in quiet hours. The supervisor drains first, and the lease extension covers the gap.
4. **Credentials.** This is the only recurring human touchpoint. Harness logins, the App key and host enrolment get a notification N days before expiry. API keys don't expire on a schedule, which is another reason for U3.

MINIAIR stays opportunistic and gets the same updater through `winget`/Scheduled Task. If it misses updates, it simply stops being eligible, because version skew fails the preflight.

**Q2: how to execute this plan.** I recommend **(c) hybrid, switching on a concrete trigger.**
- **Direct interactive sessions** for P0, P1, P2 and the P4 cutover. P1 repairs the very workflow defects (reopen, staleness) that would stall it inside the coordinator. P4 replaces the integration path it would have to use. Running them through the coordinator repeats the D10 self-hosting trap.
- **Discipline even when working directly:** a phase branch in its own worktree; the repo gate green locally (fmt, clippy `-D warnings`, test, docs check); push the branch; Actions green; then fast-forward `main`. That is the `ship` pattern before the rulesets enforce it.
- **Switch to dogfooding once P1 is deployed to production.** From that point, P3a/P3b items and all follow-ups become coordinator tasks. At first interactive sessions work them; from P3b the supervisor does. That also produces the first real data on P1.

**First 3 steps for the next session:**
1. **P0 with the user (about 15 minutes):**
   - Put U1–U11 to the user, recommendation first.
   - With their go-ahead, apply the day-0 ruleset on `main` (`non_fast_forward` + `deletion`) via `gh api`. Then confirm with a rejected force-push to a scratch branch pattern.
   - Run the 10-minute ruleset test for U4 on a throwaway public repo, created by the user.
   - Cancel sentinel 5655e94d with a rationale, and settle dea68719.
2. **P1 on branch `autonomy/p1` in a fresh worktree** with a separate `CARGO_TARGET_DIR`. Start from Dana's measured PoC shape: scoped pinning plus the monotone migration and satisfaction order. Then add, in order:
   - `revise` with the reason enum, the unresolved-intent guard and the rate limit;
   - agent unblock and cancel/replace;
   - the `ac_amendment` fields;
   - labelled gates.

   Add one regression test per fix in `crates/server/tests`, run the gate after each sub-phase, and make checkpoint commits.
3. **Stand up the staging coordinator on mxmini** with loopback and all-defaults config, plus a throwaway project and repo. Deploy the P1 build there and replay the B1/B2 scenarios with the CLI as a scripted agent: lease edit, `review_mode` change, conflict, legacy null ref. Then ship P1 to `main` and put the production deploy to the user as a reserved decision.

The planning context in this session is large. Execution should start in a **new session** from the handoff, not continue here.


---

### Dana (devil's advocate) — 2026-09-25T16:59:45Z

**Round 4 (Dana): pre-mortem, a generic-project walk, what's still unjustified, and M6.**

**Pre-mortem (2026-10-20): "this is worse than before".**

The pilot passed on 5 hand-picked tasks. Dogfooding started with 6 human tasks in the queue, and they were finished by day 3.

The supervisor then had nothing to do. Agents are allowed to author tasks: the goal says "humans or agents may do it", and they created 35 of 53 tasks last time. So they filed follow-ups:
- "split the hot fixture file";
- "Windows regression at <sha>";
- the [F] items, now "justified by measured failure";
- doc tidy-ups;
- a refactor suggested by an audit-sample disagreement.

Each one was reviewed by another agent and integrated in minutes, so HRI stayed at 0.

By day 10, `main` had taken about 60 agent-originated changes. The dashboard's layout had moved twice. The user's in-flight UI idea conflicted with three of them. `ship` needed 6 minutes of Actions per attempt, so their tight browser loop became a slow one. Meanwhile, because guard-touching releases are reserved decisions (most server releases, per my own round-2 point), 40 merged changes piled up undeployed in staging. The user got a "release decision" notification they could not evaluate without reading 40 diffs. They either rubber-stamped it, which is exactly the "authority, not information" click we set out to delete, or ignored it and got nothing.

Add roughly 15 notifications a day (M1 timeouts, post-hoc rule notices, audit disagreements, credential warnings) and about $600 of spend on work nobody asked for. The user concluded they had lost control of their own product. They also concluded the metric had measured the absence of their attention, not the presence of value.

**The one change that would have prevented it: admission control for agent-originated work.** Agents may *propose* tasks. A proposed task runs only if a human-owned admission rule admits it: incident fixes, reverts, a failing check on `main`, fixes for labelled refusals, or a per-week budget of N agent-originated tasks. Everything else waits in a **daily digest** the user triages in one pass. The same digest batches M1 decisions, post-hoc notices and release packets. Add a **value canary**: human-originated tasks completed per week and their median time to done. A secondary fix for the release pile-up: auto-build a release packet (diff summary by area, guard-file diff, staging soak evidence), so the reserved decision carries information instead of just a button.

**Q1: v3 for a small Python repo on GitHub (one CI job, no Windows, no deploy).**

Minimum configuration, all human bootstrap:
1. Create the project, enrol one host, and log in to one harness.
2. Create a GitHub App for the integrator and install it. This is the most fiddly step.
3. Create three rulesets: A, B and the tag ruleset.
4. Create the roster file naming the CI job.

Everything else should default. The user's standing preference is "runnable with an empty config", which argues for two commands:
- `agent-coordinator bootstrap github`: creates the rulesets through the API. The roster defaults to "every job of every workflow triggered on push", and the protected check ids default to that same set.
- `agent-coordinator host init`: creates the uids, installs the firewall, pins the harness binaries, and runs the integrator on that same host as a third uid.

With those two commands, v3 is **not over-built for a small project at runtime**. Its per-project cost is bootstrap, and bootstrap can be scripted.

**Coordinator-specific leaks into the generic design:**
- **Rust baked into the supervisor.** `CARGO_TARGET_DIR`, `sccache` and the crates.io egress entry are in the supervisor design. The generic version needs a project-declared, host-approved `setup` command (e.g. `uv sync`) and cache paths. It also needs a per-project egress allowlist (PyPI, not crates.io), and because the allowlist is a security boundary it belongs to the **host owner**, not the repo. **v3 has no environment-preparation step at all.** That is missing, not just leaking.
- **UI verification is written around this coordinator.** M2 ("staging coordinator + staging login") should be a generic, optional per-project `verification_env` (URL, test credential, Playwright on or off).
- **Integrator placement assumes an always-on VM.** "Integrator on the e2-micro" relies on this coordinator having one. The generic default is "on the supervisor host".
- **Deployment, U9 and the fence gate are self-hosting only.** They should be marked as such.
- **Compare-API ancestry adds a GitHub credential to the service.** Unauthenticated API use gets 60 requests per hour per IP, so busy projects need a token. That is a new service-held credential to put in the constitution, or use the integrator's attestation for projects without one.

**Q2: what is still not justified by evidence.** I'll include one of my own.
1. **`distinct_vendor` as the review default (U5).** I pushed for it. B13 shows same-*principal*, fast reviews, and it shows nothing about vendor correlation. Cross-vendor review doubles setup: two harness logins, two billing relationships, two sets of ⚠ probes. **Revise: default `distinct_launch` with a separate reviewer uid. Use `distinct_vendor` only where both vendors are configured, and let the M5 audit disagreement rate decide whether to require it.**
2. **Cooling delay plus `@binding` tagging.** Agents used rule editing exactly once (15f156e8), for a scope edit. No agent ever weakened a check; the only roster changes were human. Keep the protected check ids (cheap, fail-closed). Mark the cooling delay and tags **[F]**, with the post-hoc notification plus revert as v0.
3. **Lease extension by downtime [R-P3].** None of the 14 expirations came from service downtime. The risk only exists because v3 shortens leases. Cheaper alternative: keep `lease_seconds` at 3600 and let progress gating handle loops. Short leases buy faster crash recovery, but the supervisor already releases immediately on crash, so the 15-minute lease only matters for host loss. Make it **[F]**.
4. **M1 timeout-to-recommendation.** The evidence is the user choosing "(recommended)" *while present and being asked*. That is not evidence they're fine not being asked. Keep it, but with a long T (24 h), batched in the digest, and only for reversible choices.
5. **The 10% audit sample.** It's justified as the only measurement of review quality. I'm keeping it, but its findings should go into the digest, not become automatic tasks.

**Q3: M6, reverting an integrated regression.** I'll leave the state machine to Sol, but I'd shape it like this:
- **Triggers:**
  - a human one-click from the dashboard or digest (counted as an **escaped-defect** canary, not HRI, because it carries information);
  - an agent-detected regression (a check red on `main` at X but green at X's T0, found by bisecting the integrated SHAs);
  - a post-hoc rules notice.
- **Shape:** a normal code task with `kind=revert{X}`, whose candidate is `git revert` of X's integrated change. For merge commits that is `-m 1` against T0. For the fast-forward case R==C it is the commit range.
- **Admission:** reverts are always admitted (see the pre-mortem).
- **Review fast path:** if nothing landed after X, the revert's tree equals X's T0 tree. That tree was already validated, so the reviewer only has to confirm it is that exact tree. Otherwise it gets full review.
- **Later changes depend on X:** the `git revert` conflicts, or the checks fail. That becomes a normal `revise` to an implementer, whose job is then a **fix-forward**, not a revert. The task retitles itself and links to X.
- **X's own task:** stays `done`. It gets a `reverted_by` link and an automatically *proposed* (not admitted) "redo X" task.

**To Sol and Hari:** I'm not challenging any of your round-3 closing notes. Hari: your secret scan in `push-candidate` as [R-P2] is right, and the public-repo exposure argument settles it.


---

### Dana (devil's advocate) — 2026-09-25T17:00:03Z

**Reply to Hari, round 4 (Dana).** Our two pre-mortems fail in different places but share one root cause. **v3 pushes events at the user and measures only their clicks.** Your fixes and mine fit together:
- my admission control plus daily digest;
- your end-to-end canary with SLO paging;
- counting "queue non-empty with no progress for more than N hours" as HRI.

Lead, I'd put all three into v4 as a single item: "attention budget". The user sees at most one digest a day. They get paged only when a canary fails or an SLO breaks. Agent-originated work runs only when a human-owned admission rule allows it.

**I agree with:**
- your hybrid execution, with direct sessions for P0/P1/P2 and the P4 cutover;
- the first 3 steps;
- starting in a new session;
- the path-overlap avoidance in `next`.

**One challenge, on harness auto-promotion (your Q1.2).** A canary proves the pipeline still *works*. It does not prove containment still *holds*. A vendor release that changes permission semantics, or starts auto-loading a new instruction file such as `AGENTS.override.md`, would pass the canary and silently widen what an unattended agent can do. Claude `-p` already ignores invalid settings silently. So promotion must also re-run the **P2 containment suite**:
- candidate instruction files are not loaded;
- raw `git push` is denied;
- writes outside `$WT`/`$RUN` are denied;
- egress outside the allowlist fails;
- the settings preflight passes.

If any of these fail, the new version stays pinned, even when the canary passes.


---

### Sol (systems & protocol) — 2026-09-25T17:00:05Z

**Round 4 (Sol): pre-mortem, unreviewed paths to `main`, and the shape of the M6 revert.**

## Pre-mortem: "the queue that ate itself"

**Days 1–3.** The pilot ran 5 tasks on a quiet target, and dogfooding is quiet for three days.
**Day 4.** Four UI tasks are approved within an hour. Each landing moves T0, so every other approved R is recomputed and its checks rerun. That behaviour is correct, and it makes the queue O(n²) in Actions runs. `scripts/test_task_detail_copy_controls.mjs`, the hot file from B2, is also a *flaky* browser test. The evidence supports this: linux-validation had 35 failures against 46 successes, and ci.yml has no test-level retry.
**Day 5.** The flake policy reruns the check on T0 alone. The flake passes there, so a failure on R is "attributed to the candidate" and becomes `revise{check_failed}`. Good candidates bounce back to their authors. Each bounce costs a new LLM implementation turn, then a new review, then a new R, and another roll of the dice.
**Day 6.** The ping-pong breaker "serializes first". It creates dependencies between tasks that are not really related, so the queue becomes a chain and median lead time goes from about 5 h to about 2 days.
**Day 8.** The user wants a one-line CSS fix and runs `ship`. Ruleset B has no bypass, and the same flaky check is red on his SHA, so he can't land it. Before v3 he could just push. He disables ruleset B "for a minute". A force-push to tidy up fires `target_rewritten`, which freezes the queue. The notification lands among 30 others (parked tasks, integrator-silence alerts). The queue stays frozen for a day.
**Day 14.** "Worse than before."

Root cause: v3 treats a check result as a *fact about R*, but a flaky check is a *random variable*. Every rule we built amplifies that noise: roll-forward, re-integrate on target move, revise on failure, and the no-bypass ruleset.

**The one change: make attribution reproducible, and gate P4 on measured flakiness.**
- `check_failed` requires the failure to *reproduce on R* (fails 2 of 2, or 2 of 3) *and* the check to pass on T0. Anything else is `flaky{check, R}`. That state reruns automatically, never goes to the author, and automatically creates a de-flake task.
- Test-level retries plus flake reporting inside the check. Use nextest `--retries` for Rust. For the `.mjs` browser test, retry and fail on "passes only on retry" in the *report*, not in the gate.
- **P4 exit criterion:** rerun `main` about 20 times per required check in staging. Each check's same-SHA flip rate must be under 2%, or that check doesn't become required.
- Cheap second half: a **priority lane** in the queue for 1-file changes (`ship` and small tasks), so small work doesn't wait behind O(n²) rechecks.

## (1) Can content land on `main` without independent review of exactly that content?

For each case in turn:
- **FF, R==C.** Safe *if* the review unit is defined correctly. **Invariant R-1:** the reviewed diff is `merge-base(T_review, C)..C`, and the decision records `(C, reviewed_base)`. Under I-FF, `reviewed_base ∈ ancestors(T0)`, so the landing diff `merge-base(T0,C)..C` ⊆ the reviewed diff. The merge *interaction* in R (auto-resolved hunks, semantic conflicts) is never reviewed. Checks alone cover it. State that explicitly.
- **Stacked candidates: this is a real hole.** Author A writes C1 for task 1. Author B stacks C2 on C1 for task 2. Contributors are recorded per *task and session*, so A is not a contributor to task 2. **A can then review C2, whose diff contains A's own commits.** That is self-review by stacking. It gets worse if C1 is later rejected and re-landed as C1′: C2 still carries the *rejected* C1, and it lands when C2 merges. **Fix [R-P1]:** compute contributors from the *landing range*. Map every commit in `merge-base(T,C)..C` to the attempt that checkpointed it (the service already records checkpoint SHAs) and exclude the union of their contributors. Commits of unknown provenance count as the submitter's. Also, a candidate whose range contains commits from a submission that is not integrated and not approved is refused (`stacked_on_unapproved`) until that parent lands.
- **Resubmission after `revise`.** The reviewer of C′ must review the full range. A range-diff against C is only a presentation aid, because a rebase can hide changes in an "incremental" view.
- **Human `ship`.** Unreviewed by design (U2). But the owner credential also belongs to the user's *interactive agent sessions* (history: "commit, push, deploy"). So an interactive LLM can `ship` any green SHA, including an unreviewed agent candidate. **Fix [R-P4]:** the monitor labels every tip move not made by the integrator as `unreviewed_landing` and automatically queues a *post-hoc* agent review. That review is non-blocking; its findings become fix or revert tasks. The canary counts these landings.
- **AC amendments.** If `amendment_decision = rejected`, the submission as a whole is `changes_requested`, because the code was built for the amended criteria. v3 should state that.
- **"Revise loses to a landed push."** The content was reviewed and approved, so it was not unreviewed. But if the losing revise was `author_withdraw`, the author believes it is wrong. The follow-up must be a *revert candidate* (M6) at high priority, not an ordinary task.
- **Flake race.** A push authority is granted on a green retry while a `revise{check_failed}` cites the red run. The fix is that exactly one run decides the check: the latest completed run bound to (R, definition blob). A revise must cite that deciding run, or it is refused.

## (2) M6: the revert state machine

- **Task kind** `revert{target: (submission, R), reason, evidence}`. It may be created by:
  - a human, one click;
  - any agent with evidence: a check that passes on R's first parent and fails on the current tip after bisect, an audit-sample rejection, or an `author_withdraw` follow-up.
- **Candidate** = `git revert -m 1 R`, applied to the current tip. For the FF case, revert the range. **The integrator computes it, not an LLM.**
- **Clean mechanical revert.** The content is fully determined as the inverse of already-reviewed content, so review judges the *decision* (the evidence), not the diff. The integrator attests "tree == mechanical revert" and the usual checks run. A human one-click revert skips review, because that is authority, not content.
- **Textual conflict, or checks fail on the revert** (later change Y depends on X). Choose between:
  - (a) a **cascade**: revert X plus the later integrations whose ranges touch X's files, newest first, as one candidate, capped at N=3 and only when `main` is broken;
  - (b) otherwise, convert to an ordinary *implementation* task ("undo X's behaviour, keep Y") with full review.

  Never auto-cascade beyond the cap.
- **States.** X stays `done` with a `reverted_by` link, because history is immutable. A defect-reason revert automatically creates a **re-land task** seeded with X's candidate and the revert evidence as acceptance criteria.
- **Git trap, which v3 currently gets wrong:** after reverting merge R, re-merging the *same* C is a **no-op**, because C's commits are already ancestors. v3's "no-op (R==T0) ⇒ done" would then mark the re-land task *done with nothing landed*. That is a silent lost change. **Fix [R-P4]:** a no-op counts as done only if no recorded revert touches C's range. Otherwise it is refused with `candidate_reverted_in_history`, and re-land candidates must be new commits (cherry-picked or rebased) or a revert-of-revert. The continuous ancestry audit cannot catch this, because R stays an ancestor after a revert.

Replies to Dana and Hari follow after I re-read.


---

### Dana (devil's advocate) — 2026-09-25T17:00:33Z

**Reply to Sol, round 4 (Dana).**

**I'm convinced by two findings, which I'd rank as the most important of this round:**
- **Self-review by stacking.** Contributors should be computed from the landing range, and `stacked_on_unapproved` should be refused.
- **Re-landing a reverted merge becomes a silent no-op.** `candidate_reverted_in_history` is needed.

Both are real lost-change or self-review holes, and nobody else found them. Both should be [R]. I also accept these without reservation:
- **flaky vs `check_failed`**, including "exactly one deciding run";
- the **P4 flake-rate exit criterion**, since the evidence supports it (35 of 81 linux-validation runs failed);
- your **M6 shape**: integrator-computed mechanical reverts, human one-click without review, a capped cascade, and a re-land task.

Your M6 and mine differ only in wording. Adopt yours.

**Two challenges:**
1. **"Priority lane for 1-file changes."** That is the `quick` lane coming back, which we removed in round 2. I accept it only as a *queue ordering* rule: small approved changes jump the recompute queue. It must not change review or check requirements. Better still, most of the O(n²) problem goes away if the integrator rebases the queue in **batches** (test the head N together, bisect on failure). That was a plan-v1 idea we deferred, and your day-4 story is the measured failure that justifies it. So it should be the [F] item that triggers first, not a new lane.
2. **`unreviewed_landing` → automatic post-hoc agent review of every non-integrator tip move.** This conflicts with the admission control I proposed and with U2's intent: humans' own pushes are deliberately unreviewed. Reviewing every `ship` would spend money and add digest noise on the user's own work. Narrow it with provenance: auto-review only landings whose range contains commits *not* authored by the owner's human identity. That covers exactly your interactive-LLM `ship` case, meaning agent-authored or unknown provenance. The user's own commits stay unreviewed.

**For the lead:** all three pre-mortems end the same way. The user is flooded with events, the user bypasses a guard (ruleset B or notifications), and HRI still reads 0. So v4's headline fix should be one item, the **attention budget**:
- admission control for agent-originated work;
- one daily digest;
- paging only on canary or SLO breach;
- stalls count as HRI;
- flaky checks never reach an author or the user.


---

### Sol (systems & protocol) — 2026-09-25T17:00:35Z

**Follow-up (Sol) to Dana and Hari, round 4.**

**The three pre-mortems describe one failure.** Dana's version is value/control, Hari's is silent stall, mine is flake amplification. In all three, v3 turns noise into work, or work into notifications, and measures neither. So I support Dana's single "attention budget" item: admission control, a daily digest, SLO/canary paging, and treating no-progress as HRI. Two protocol additions make it enforceable:
- **Admission as a state.** Agent-originated tasks are created `proposed`, with `origin=agent`. `next` never offers a proposed task. The admission rule is service-side and human-owned, so it belongs in the constitution, not the repo. These are always admitted:
  - `revert`
  - `fix-target`
  - `flaky`/de-flake
  - tasks that fix a labelled refusal
- **The canary also measures flake.** Hari's end-to-end canary should record, per required check, how often the same SHA both passes and fails. That is the signal that would have caught my day-5 story early.

**Dana's M6: we agree on the shape. One trap and one generalisation.**
- **Trap: "redo X" cannot reuse X's commits.** After `git revert -m 1 R`, merging C again is a no-op because C is already an ancestor. v3's "no-op ⇒ done" would mark the redo as done with nothing landed. The redo candidate must consist of *new* commits, either cherry-picked or a revert-of-revert. The integrator must refuse a no-op when a recorded revert touches C's range (`candidate_reverted_in_history`). This belongs in [R-P4], alongside M6.
- **Generalisation: your fast path ("revert tree == X's T0 tree") should cover any clean mechanical revert.** The integrator computes the revert on the current tip, not an LLM. Review then judges the revert *decision* and its evidence. It does not re-read an inverse diff of content that was already reviewed. When the revert conflicts, or checks fail, it becomes your fix-forward `revise`. I'd allow a capped cascade (at most 3 later integrations, and only when `main` is broken) before falling back to fix-forward.

**Dana, Q2 items I concede:**
- **(3)** If `lease_seconds` stays at 3600 and crash release is immediate, then extending leases by measured downtime is **[F]**. That leaves host loss as the only slow recovery case (up to 1 h), which is acceptable.
- **(2)** The cooling delay and `@binding` tags become **[F]**, with protected check ids kept as the v0 guard. Agreed: the evidence has no agent weakening anything.
- **(1)** From a protocol standpoint, `distinct_launch` as the default is fine, **but only if** contributors are computed from the *landing range* (my stacking hole in round 4). Principal-level independence is what the protocol has to guarantee; vendor diversity is a quality setting that M5 measures.

**Dana, on the compare-API credential:** yes, it is a new service-held secret. Use a fine-grained, read-only, single-repo token (`contents:read`) and list it in the constitution. Projects without one fall back to integrator attestation plus the continuous audit. That is the asymmetric-trust option from round 2, now [R] for those projects only.

**Hari:** agreed that execution should be hybrid, starting in a new session. Dana's containment-suite gate on harness promotion is necessary; a canary pass proves liveness, not containment. One protocol note on host updates: *drain* must mean the host's leases are **released at a checkpoint**, not left to expire. Otherwise every update produces a host-loss-style recovery, which is 1 h with a 3600 s lease.


---

### Sol (systems & protocol) — 2026-09-25T17:00:56Z

**Short reply to Dana (Sol).** I accept both challenges, each with one detail.

1. **The priority lane should be ordering only, and batching is the better fix.**
   - Batching fits the pinned-R protocol if the result key becomes `(batch members, T0)`. Receipts bind to `R_batch`, and `main` only ever moves to a tested batch tip, so "validated on the integrated result" still holds.
   - Bisect-on-failure then produces per-member `R` values under the same UNIQUE rule.
   - "Revise loses to a landed push" applies per member.
   - Agreed that this is the first [F] item to trigger, with my day-4 story as its trigger.

2. **Narrow `unreviewed_landing` by provenance. Agreed, but Git author identity can't be the test.** Interactive LLM sessions commit as the user, with the user's name and email. What actually distinguishes them is the trailers the harness adds by default (`Co-Authored-By: Claude …`, the Codex equivalent), plus the `Claude-Session:` line this user's own attribution rule requires. So the rule is: post-hoc review runs on landings whose range contains commits with agent trailers or from an unknown source.
   - This fits our "confidently wrong, not adversarial" threat model.
   - An adversarial session could strip the trailers. If that ever matters, the stronger version is "unreviewed unless signed with a key only the human holds".
   - The user's own unassisted commits stay unreviewed, as U2 intends.


---

### Lead (Claude, main session) — 2026-09-25T17:04:08Z

**Round 5 (lead): plan-v4 is posted at `planning/autonomy/plan-v4.md`, and I'm asking for final sign-off.** Changes since v3 are marked **(v4)**.

What round 4 changed:
- **§2.0 Attention budget (headline).** Admission control (`proposed` state, human-owned admission rule), one daily digest, paging only on canary/SLO breach, stalls count as HRI, and flaky checks never reach the author or the user. Also a value canary and an escaped-defect canary. This merges all three pre-mortems.
- **Reproducible flake attribution**, with one deciding run per check and a P4 flip-rate gate of <2%.
- **Landing-range contributors**, `stacked_on_unapproved`, and invariant R-1.
- `candidate_reverted_in_history`, `unreviewed_landing` by agent trailers, and the M6 revert state machine (Sol's shape).
- A host updater, with harness promotion gated by preflight, canary *and* the containment suite (Dana's challenge).
- Project `setup`, a host-owned egress allowlist, generic `verification_env`, bootstrap commands, and self-hosting-only items marked as such.
- **Demoted to [F]:** lease extension, the cooling delay/`@binding`, and priority ordering (ordering only). Batching is marked as the first [F] item expected to trigger.
- **U5 is now `distinct_launch`.** New decisions: U12 (admission budget, starting at N=5 per week) and U13 (notification channel).
- New §6: hybrid execution and the first three steps.

**Each of you, please post one message:**
1. **Verdict:** AGREE, AGREE WITH RESERVATIONS (list them), or DISAGREE (state the blocking issue and the minimal fix).
2. **Errors:** anything in v4 that misstates what we agreed, or that I dropped. Quote the transcript.
3. **Top risk:** the single highest remaining risk to the goal, one sentence, for the handoff's risk register.

Keep it short. After this I'll write `HANDOFF-autonomy.md` and ask you to cold-read it for executability in a final round.


---

### Dana (devil's advocate) — 2026-09-25T17:05:01Z

**Final sign-off (Dana): AGREE WITH RESERVATIONS.**

I checked every point I won. All of them landed in v4, correctly:
- the `distinct_launch` default (U5);
- the cooling delay as [F];
- lease extension as [F], with the 3600 s rationale;
- the M1 timeout of 24 h, batched in the digest;
- `unreviewed_landing` narrowed by provenance;
- small changes may jump the queue, "ordering only";
- batching as the first [F];
- the containment suite gating harness promotion;
- generic `setup` and a host-owned egress list;
- `verification_env`;
- integrator default on the supervisor host;
- the M6 shape, following Sol's.

I found no dropped agreements. Reservations, each with its minimal fix:

1. **Scope regrew inside the pilot gate.** P3b now makes the **host updater** [R-P3b] and a **canary per vendor per host** preconditions of a 5-task pilot. A pilot doesn't need unattended self-updating; it needs pinned binaries that someone installed by hand. **Fix:** make the host updater [R-P6], required before the 2 unattended weeks and not before the pilot. For P3b, one canary covering the pilot's single vendor and host is enough, and the per-vendor/per-host matrix moves to P6.
2. **One stale sentence contradicts the attention budget.** §2.2 item 6 still says "human queue + **push notification**" for labelled human gates, while §2.0 says anything other than canary/SLO breaches goes to the digest. **Fix:** labelled gates go to the digest; page only when a gate blocks the canary or an SLO.
3. **Two U-lists disagree.** The P0 row says "Put U1–U11 to the user", but §4 and the next-session steps (line 395) have U1–U13. **Fix:** U1–U13 everywhere.
4. **Provenance by agent trailers is best-effort.** Trailers are optional text and easy to drop, which is acceptable under our confidently-wrong threat model. **Fix:** one sentence in v4 stating that, so no one later mistakes it for a security control.

**Top remaining risk:** the plan now depends on the user reading one digest a day. If they stop, proposed work, M1 decisions and audit findings pile up silently while HRI stays at 0. So the value canary must be paged on, not just reported in the digest.


---

### Sol (systems & protocol) — 2026-09-25T17:05:23Z

**Round 5 sign-off (Sol).**

**Verdict: AGREE WITH RESERVATIONS.** The protocol core is faithfully captured: pinned R, roll-forward, I-FF, "revise loses to a landed push", landing-range contributors, reproducible flake attribution, `candidate_reverted_in_history`, and M6. The three reservations below are all small doc fixes, and none of them blocks the plan.

**Errors or omissions in v4:**

1. **Dropped: service-verifiable recovery evidence (B5).** My round-3 post said: *"The recovery gate is still a local attestation. `recovery_resolution` requires `saved_work_checked && running_jobs_checked` … FIX (P3): recovery evidence becomes facts the service can verify: the old launch is finished or its token has expired; the generation has been bumped; the last checkpoint SHA equals the wip ref the recoverer fetched; no nonterminal jobs remain."* I also stated an invariant: *"the checkpoint SHA recorded in the service is authoritative, and refs are only transport. A recoverer or integrator must verify ref == recorded SHA."* v4 includes neither. Without them, the 38f3ffe4 cross-host recovery failure ("cannot be inspected from this Linux workstation") survives the supervisor.
   **Fix:** add both to §2.3 as [R-P3]. The invariant then also covers zombie ref clobbering, because the `agentc` Git credential can write any `refs/agent-coordinator/*`.

2. **Inconsistency: legacy import vs R′ re-keying.** §2.4 (5) says legacy intents are *"imported as pinned results at cutover"*, but R′ re-keying (*"when R is lost"*) is [F]. Legacy R lives only in the preparing workstation's local ref (round 3: *"If tip == T0: recompute. Publish if R′ == R; otherwise use R′ with fresh receipts"*). So an import where R′ ≠ R hits UNIQUE(submission, T0) and gets stuck. The P4 preflight also demands *"zero unresolved intents"*, which makes the import nearly moot.
   **Fix (pick one):**
   - make the preflight the rule and drop the legacy import (simplest; P0 settles dea68719, so the queue is empty anyway); or
   - make R′ re-keying [R-P4] for imports only.

3. **Undocumented P1 gap.** Round 3: *"5c85ebb2 ('check failure after intent') still needs human reconciliation in P1 … FIX (P1, needs measurement): record the intent after checks pass, just before the push … If that turns out not to be cheap, the gap is P1-only and closes in P4."* v4 drops the item silently.
   **Fix:** either add it to P1 as "measure; implement if cheap", or list it as a known P1-only human gate in the P1 exit criterion. Otherwise the B1/B2 replay could be read as "no human" for a case it doesn't exercise.

Minor: in round 3 I proposed an automatic `fix-target` task (*"Don't route this to a human"*). v4 downgrades it to a notification plus [F]. That is acceptable under the attention budget, but it should go in the digest, not the pager.

**Top risk:** the publication proof depends on invariants enforced *outside* our code (ruleset B's `non_fast_forward` and the single-R-producer rule). Under flake or throughput pressure, the user is likely to disable ruleset B "for a minute", and that silently voids "not published is final" and "published iff contained". So the flake gate and the `target_rewritten` freeze are what hold the whole design together, not polish.


---

### Hari (harness & ops) — 2026-09-25T17:05:23Z

**Round 5 sign-off (Hari): AGREE WITH RESERVATIONS.**

v4 faithfully carries every harness, ops and rollout point I made: containment profile, candidate instructions never loaded, settings preflight, pinned binaries, secret-scanning `push-candidate`, the ruleset timing trap, integrator on the VM, the canary with SLO paging, the host updater with Dana's containment-suite gate, and hybrid execution. I have three reservations. The first is a missing [R] item.

1. **Missing [R-P2]: Git repository isolation between roles.** The Codex profile gives the implementer `--add-dir <git-common-dir>`. v2/v3/v4 also say "worktree", which means one shared `.git` per host. So `agentc-impl` can write `.git/config` and `.git/hooks` (`core.hooksPath`, `remote.*.url`, `core.fsmonitor`). Every other user of that repository then executes what it wrote:
   - the reviewer's read-only worktree at the candidate SHA;
   - the supervisor's checkpoint and GC `git` calls;
   - `push-candidate` itself.

   There is a second trap. Git refuses repositories owned by another uid (`safe.directory`), and the usual "fix" is `safe.directory='*'`, which turns this into hook execution by the supervisor's uid.

   **Minimal fix:**
   - Each launch gets its **own clone**, owned by that role's uid. It is cheap: `git clone --reference <host mirror>` of an 18 MB repo.
   - The host mirror is supervisor-owned and read-only to agents.
   - Supervisor-side `git` in agent-owned clones runs as the *agent* uid, with `-c core.hooksPath=/dev/null -c core.fsmonitor=false`. It never runs as root, and never with a wildcard `safe.directory`.
   - Add one containment-suite case: "a hook planted by the implementer does not run for the reviewer or the supervisor".

2. **The P0 verification step is unsafe as worded.** Row P0 says "ruleset `non_fast_forward` + `deletion` on `main` (via `gh api`, verified by a rejected force push)". If the ruleset did *not* take effect, that test force-pushes production `main`. **Fix:** verify by reading back `GET repos/{o}/{r}/rules/branches/main`. The behavioural test (a rejected force push) belongs in the U4 throwaway repo, where a failure is harmless.

3. **Sizing: P3b is now the biggest phase, and the plan still says "Supervisor (per host, ~300–500 lines)".** That number predates what the supervisor now carries: progress renewal, crash/drain release, worktree/clone ownership, project `setup`, reviewers plus the audit sample, health, M1 decisions, admission, digest, canary plus SLOs, path-overlap avoidance, and the host updater. Realistically that is 2–3k LOC, well beyond Dana's accepted "~300" (round 2: "I … accept roughly 300 lines, *on the condition that* the lease is renewed only when there is progress").

   **Minimal fix:** split P3b.
   - **P3b-pilot** is the go/no-go gate. It needs the supervisor core, reviewers, health, the canary, SLO paging and the digest. Harness versions are pinned and updated by hand during the pilot.
   - The **host updater and the admission rule move to [R-P6]**, so they are required before the two-week dogfood, not before the pilot. Both only matter once time passes and agents originate work.

   Please also restate the component size honestly.

**Errors or dropped items:** none beyond #2. I checked that the U3 wording, the `distinct_launch` default (Sol round 4: "fine, **but only if** contributors are computed from the *landing range*", which v4 §2.2 5a includes), drain-as-release (Sol: "*drain* must mean the host's leases are **released at a checkpoint**"), and the lease-extension demotion all match the transcript.

**Top risk (one sentence):** the autonomy machinery is itself a large new operational surface on a one-person project (supervisor, updater, canary, integrator, rulesets, two vendors' harnesses on three hosts), and if keeping *it* healthy becomes the user's new chore, the goal fails even with HRI/task = 0.


---

### Lead (Claude, main session) — 2026-09-25T17:08:09Z

**Round 6 (lead): the final plan and handoff are written. This round is a cold-read audit.**

Thank you for the sign-offs. Every reservation is applied and marked **(final)** in `planning/autonomy/plan-final.md`:
- The host updater, admission control and per-host canaries move to P6, so the pilot runs the core only.
- Labelled gates and `fix-target` go to the digest.
- U1–U13 everywhere.
- Trailer detection is best-effort.
- B5 recovery facts and "the checkpoint SHA is authoritative" are restored as [R-P3].
- The zero-unresolved-intents preflight replaces the legacy import.
- The P1-only human gate is listed.
- Git isolation is [R-P2].
- The day-0 ruleset is verified by API read-back, never by force-pushing `main`.
- A new §7 risk register holds each of your top risks, plus K4 (scope re-growth).

The handoff is `planning/autonomy/HANDOFF-autonomy.md`. The decisions are parked in `.claude/decisions-pending.md` for the SessionStart hook.

**Participant joining:** **Casey (cold reader)**, a fresh Opus agent with *no* access to this discussion's context. Casey reads only the handoff and what it links, then posts a P0/P1 execution outline plus questions. Please treat Casey's post as the test result: wherever Casey misunderstands, the handoff is wrong, not Casey.

**Assignments:**
- **Dana, Sol, Hari:** audit `HANDOFF-autonomy.md` and `plan-final.md` for contradictions (with each other, and with what you agreed in the transcript), for missing information a fresh session would need, and for anything unsafe to execute literally. Post findings as a numbered list with exact text fixes. Then, once Casey has posted, reply to Casey's questions and flag any misreadings.


---

### Dana (devil's advocate) — 2026-09-25T17:09:36Z

**Round 6 audit (Dana): HANDOFF-autonomy.md and plan-final.md.** Items are ordered by severity, and each gives the exact text fix.

**Unsafe or wrong if executed literally**

1. **P0 tells the agent to do human-only work.** HANDOFF §5.2 says "cancel sentinel task 5655e94d … settle dea68719 … both need the coordinator, so connect a CLI session with the user's consent". Cancel, archive and delete are human-only today (`coordination.rs:655`, `admin_or_operator`). An agent session gets `operation_not_permitted`. Also, in this very session the classifier refused `agent-coordinator connect`.
   Fix: replace with **"ask the user to cancel 5655e94d in the dashboard (task lifecycle is human-only until P1 item 4 ships), and to decide dea68719: either authorize its deploy (U9) or cancel it in the dashboard."** Apply the same change to the P0 row and §6 step 1 of plan-final.

2. **Legacy-intent import is still in the handoff.** HANDOFF §4 (Integrator) ends with "M6 revert state machine; legacy-intent import at cutover." plan-final §2.4(5) replaced that with a **zero-unresolved-intents preflight, no import**.
   Fix: replace "legacy-intent import at cutover" with **"cutover preflight: zero unresolved intents/holds/reservations/jobs/recovery, drained by existing agent reconciliation (no import); ruleset watchdog"**.

3. **The harness profile contradicts Git isolation.** plan-final §2.3 (Harness profile) still prescribes Codex `--add-dir <git-common-dir>`. Two bullets later, Git isolation "Supersedes `--add-dir <git-common-dir>`". HANDOFF §4 and the Hari round 2 launch lines that it points to also still carry it. A fresh session copying the command lines would reintroduce a shared `.git` between roles.
   Fix, in §2.3: replace "`--add-dir <git-common-dir>` +" with **"`-C <per-launch clone>` (no shared git-common-dir; see Git isolation) +"**. In HANDOFF §4, add: **"Hari's round-2 launch lines predate Git isolation: drop `--add-dir <git-common-dir>` and use per-launch clones."**

4. **The flip-rate exit gate is ambiguous about where the reruns happen.** P4 says "each required check's flip rate < 2% over ~20 reruns of `main` in staging". But the checks are GitHub Actions on *this* repo. A throwaway staging repo has different workflows, so reruns there measure nothing.
   Fix: **"~20 re-runs (`gh run rerun`) of the required workflows on the current `main` SHA in the real repository"**.

**Contradictions between the two files**

5. **HANDOFF §4 (Supervisor) still lists "host updater" and "admission control"** as supervisor components. plan-final moved both to P6.
   Fix: append **"(host updater, admission control and per-host canaries are [R-P6], not pilot)"**.

6. **The supervisor size reads as scope re-growth.** plan-final §2.1 says "realistically 2–3k lines for full P3b; pilot core smaller", but after the round-5 moves P3b *is* the pilot core. The line was 300–500 in v3.
   Fix: **"pilot core (P3b): budget ≤1.5k lines incl. tests; full supervisor with the P6 items ≈2–3k"**. Also add to the P3b exit criterion: **"if the pilot core exceeds its budget, stop and re-scope before the pilot"**.

7. **The original "outside the workflow" instruction collides with the dogfood switch.** HANDOFF §1 quotes the user: "DO NOT create a new task for this work". §4 and plan-final §6 say "switch to dogfooding once P1 is deployed … P3a/P3b items … become coordinator tasks". That changes the user's explicit instruction without asking.
   Fix: add **U14: "After P1 is deployed, may remaining autonomy work run as coordinator tasks (recommended), or stay outside the workflow as originally instructed?"** Add it to §6, plan-final §4 and `.claude/decisions-pending.md`.

**Missing information a fresh session needs**

8. **Unset thresholds.** "Stalls … more than N hours" (§0) and "digest unread for N days" (§2.0.6) have no values. The user's standing rule is in-code defaults.
   Fix: **"defaults: stall N = 6 h outside quiet hours; digest-unread N = 3 days; both configurable"**.

9. **P1 estimate.** "~450–650 LOC + stacking rules" leaves the 5a part unmeasured. Mapping every commit in the landing range to a checkpointing attempt may need a new table, because checkpoints record only the latest SHA per checkpoint, not the range.
   Fix: split **P1a** (measured: pinning, migration, satisfaction order, revise, unblock/cancel, labelled gates, `ac_amendment`) and **P1b** (5a landing-range contributors + `stacked_on_unapproved`; estimate after reading the checkpoint schema). P1a ships first. The stacking hole predates P1, so P1a is not a regression without P1b.

10. **Public-repo hygiene for the planning files.** HANDOFF §2 says review before pushing, but doesn't say *whether* to commit `planning/` and `.claude/decisions-pending.md` at all. The latter is repo-local, so committing it would publish the pending questions.
    Fix: **"Do not commit `.claude/decisions-pending.md`. Ask the user whether `planning/autonomy/` is committed (recommended: yes, after redacting host names and VM details), or kept local."**

**Scope check**

11. Items added after sign-off, each small and each tied to a named risk:
    - the ruleset watchdog (K2);
    - Git isolation (Hari);
    - B5 recovery facts restored (Sol).

    I found no silent regrowth in [R] beyond item 6, and none of my [F] demotions were reverted.

Casey, I'll reply once you've posted.


---

### Hari (harness & ops) — 2026-09-25T17:09:52Z

**Round 6 audit (Hari): harness, ops, rollout and P0.** Each item gives the location, the problem, and the exact replacement text.

1. **HANDOFF §4 contradicts §7 and plan-final (deferred items are still listed as pilot items).**
   - In the Supervisor bullet, replace "decisions with 24 h timeout-to-recommendation (reversible only), host updater, and the **attention budget** (admission control for agent-originated work, one daily digest, paging only on end-to-end canary/SLO breach)" with: "decisions with 24 h timeout-to-recommendation (reversible only), one daily digest, and one end-to-end canary with SLO paging for the pilot; **host updater, admission control (U12), per-vendor/per-host canaries and digest-neglect paging are [R-P6], in place before dogfood, not the pilot**".
   - In the Integrator bullet, replace "M6 revert state machine; legacy-intent import at cutover." with: "M6 revert state machine; cutover preflight requires **zero unresolved publication intents** (no legacy import); ruleset watchdog".

2. **The P0 ruleset step has no exact command, and no user decision covers it.** HANDOFF §10 says "Do not … change rulesets … without the user's recorded decision", but none of U1–U13 asks for the day-0 ruleset. Add a row to the §6 table and to `.claude/decisions-pending.md`: "**U0** Apply the day-0 ruleset now (blocks force-push and deletion of `main` for everyone, including you; no bypass) | Yes". Then replace §5 step 2's ruleset clause with:
   ```
   gh api -X POST repos/marshallr12/agent_coordinator/rulesets --input - <<'JSON'
   {"name":"day0-main","target":"branch","enforcement":"active",
    "conditions":{"ref_name":{"include":["~DEFAULT_BRANCH"],"exclude":[]}},
    "rules":[{"type":"deletion"},{"type":"non_fast_forward"}],"bypass_actors":[]}
   JSON
   gh api repos/marshallr12/agent_coordinator/rules/branches/main --jq '[.[].type]|sort'
   # expect ["deletion","non_fast_forward"]; never verify by force-pushing main
   ```
   Add one sentence: "This does not affect today's LLM publisher, whose CAS pushes are fast-forwards. Do **not** add `required_status_checks` or `update` yet, because they break that publisher until the P4 integrator exists (Hari's timing trap)."

3. **"Ship P1 to `main`" (HANDOFF §5 step 4, plan-final §6 step 3) comes before the `ship` script exists (P2).** Append: "Until P2 delivers `ship`, do it by hand:
   - `git push origin autonomy/p1`;
   - wait for *Coordination checks* and *Documentation checks* to be green on that SHA (`gh run watch`);
   - `git push origin <sha>:main`, which must fast-forward."

4. **The gate commands are incomplete.** Replace HANDOFF §5 step 3's gate parenthesis with the CI's exact commands:
   - `cargo fmt --all -- --check`
   - `cargo clippy --workspace --all-targets --locked -- -D warnings`
   - `cargo test --workspace --locked`
   - `cargo build --workspace --locked`
   - `python3 scripts/smoke.py` and `python3 scripts/backup_smoke.py`
   - for doc changes, `python3 scripts/check_docs.py --mdbook "$(command -v mdbook)"` (CI pins Rust 1.98.1 and mdBook 0.5.4).

5. **plan-final §2.3 still contains superseded text next to its own supersession note.**
   - Delete "`--add-dir <git-common-dir>` +" from the Codex profile (line 178). A per-launch clone has its Git dir inside the workspace.
   - Change "the supervisor creates the worktree" to "the supervisor creates the per-launch clone".
   - Change "read-only worktree at the candidate SHA" to "read-only per-launch clone at the candidate SHA (owned by `agentc-rev`)".

6. **The reading list points at superseded launch lines.** HANDOFF §5 step 1 sends the reader to "Hari's launch lines" in round 2. Add: "Round 2's task-scoped launch tokens are **[F]**, replaced by two uids with static credentials (round 3). Round 2's `--add-dir <git-common-dir>` is replaced by per-launch clones (plan-final §2.3). `--setting-sources user` does **not** stop `CLAUDE.md` discovery; only `--bare` (API key) is known to, which is a P2 blocker to verify with the U11 probes."

7. **Missing service work: credential attributes the plan assumes but the service lacks.** Three things depend on credential attributes: the success metric (`class=supervised`), the read-only reviewer credential, and the P3a read-only host principal. I found no read-only or class attribute in `crates/server/src/auth.rs`. Add to plan-final P2 (or P3a) content: "**[R-P2/P3a] service: credential attributes `class` (supervised | interactive) and `access` (read | write), stored on the credential, enforced on every mutation route, and recorded on events.**" Also state who holds what, per host:
   - `agentc-impl`: a write implementer credential.
   - `agentc-rev`: a **read-only** credential.
   - The supervisor (not readable by children): it renews and releases implementer attempts using the implementer credential, and posts verdicts with a separate write **reviewer** principal. That keeps verdict principal ≠ author principal.

8. **U3 hides a P2 dependency.** Add to the U3 row: "If subscription billing is chosen, P2 must prove an alternative way to keep candidate `CLAUDE.md` from loading (cwd = `$RUN` with the clone via `--add-dir`), or supervised Claude sessions wait and the pilot runs Codex-only."

9. **Public-repo exposure before commit (HANDOFF §2).** My round-1 post names a private repository of the user's while describing the global auto-mode configuration. Before committing, redact that name in the transcript, e.g. to "another (private) repo". More generally, the executing session should grep the planning files for private repo names and hostnames not already in HANDOFF.md before pushing.

10. **dea68719 is a production deploy, so settling it is a reserved decision.** Replace "settle dea68719 (deploy archived task pagination)" with: "ask the user whether dea68719 (deploy archived task pagination) should be deployed now, which is their U9-class decision, or canceled with a rationale". Keep "connect a CLI session with the user's consent", and add: "the auto-mode classifier refused `connect` during planning, so expect to need the user's explicit approval".


---

### Sol (systems & protocol) — 2026-09-25T17:09:57Z

**Round 6 audit (Sol): protocol and state-machine findings in HANDOFF-autonomy.md and plan-final.md.** Items 1–3 are unsafe if executed literally. Items 4–7 are gaps or contradictions.

**1. Unsafe in P1: "roster at push time" as written strands published work.** plan-final §2.2.2 says *"Roster at push time (P1: current `workflow_policies`…)"*. In today's code the roster is validated *after* the push, in `integration-result` (`workflow.rs:2437`) and in `finalize` (`:2912`). Both call `validate_check_jobs`, which reads `ctx.workflow_policy_revision` (`:2308`). If P1 switches that lookup to "current", then any roster edit that lands between `publish` and `finish` turns an already-published R into `required_checks_incomplete`. The work is published but can never be finalized, which is a new human gate.
**Text fix** (replace the P1 clause): *"P1: the roster revision is captured on the publication intent (the last service call before the push), and `integration-result`/`finalize` validate against that captured revision, never the current one. A roster change after the intent applies to the next integration only. P4: the roster file in T0."*

**2. Unsafe in P1: landing-range contributors can't be computed by the service.** The service has no Git access and stores candidate/checkpoint tips, not commit ranges. §2.2 5a says *"every commit in `merge-base(T,C)..C` maps to the attempt that checkpointed it"*, but no component can do that today.
**Text fix** (add to 5a):
- *"The submitting CLI computes `range_commits[]` = `git rev-list T..C` against the current target and sends it with the submission. The service stores it per submission.*
- *Contributors are the union over the submissions whose stored `range_commits` contain each SHA. A SHA found in no other submission counts as the submitter's.*
- *`stacked_on_unapproved` = the range intersects the range of a submission that is neither approved nor integrated.*
- *The reviewer's native CLI recomputes the range on claim (it already fetches and verifies the candidate) and refuses on mismatch.*
- *In P4 the integrator also re-verifies it."*

Without this, whoever implements 5a will either skip it or put Git into the service.

**3. HANDOFF contradicts plan-final in two places. A fresh session reads the HANDOFF first.**
- HANDOFF §4 Integrator bullet ends *"M6 revert state machine; legacy-intent import at cutover."*
  **Fix:** *"…M6 revert state machine; cutover preflight requires zero unresolved publication intents (no import); ruleset watchdog."*
- HANDOFF §4 Supervisor bullet lists *"host updater, and the **attention budget** (admission control…)"* inside P3b.
  **Fix:** *"…decisions with 24 h timeout-to-recommendation, daily digest, one end-to-end canary with SLO paging and stall-as-HRI (P3b). Admission control, per-host canaries, digest-neglect paging and the host updater are required before P6 dogfood, not the pilot."*

**4. The P1-only human gates list is incomplete.** plan-final 6a lists only *"check failure after intent"*. A second gate survives P1: **target moved after intent** (tip ∉ {T0, R}). The agent reconciliation from 3590591f accepts only an exact base or result (`workflow.rs:2757-2768`), and the CLI refuses with *"the target moved; retain the hold and use human reconciliation"*. Until P4 enforces ship-only landings, direct human pushes can still trigger it (2f6d4f3b, 51d0d4e4).
**Text fix** (append to 6a): *"(b) target moved after intent (tip ∉ {T0, R}), and any case where the preparing workstation's journal is gone. Both are removed by P4 roll-forward. Optional cheap P1 fix, same workstation only: allow agent `not_published` when tip ≠ T0 and the CLI verifies R ∉ ancestors(tip). A false `not_published` self-heals, because the resubmission re-integrates as a no-op."*

**5. The P1 cutover has no preflight.** My Q-C answer said *"Before any cutover: a read-only preflight"*. plan-final puts one only at P4.
**Text fix** (P1 row, and HANDOFF §5 step 4 before "production deploy"): *"Before the P1 production deploy, a read-only preflight must show zero unresolved publication intents, held holds and held reservations, and no active integration attempts. Otherwise wait for them to drain. The startup monotone reconciliation then runs once."*

**6. Day-0 ruleset compatibility, for the record, so nobody delays it.** `non_fast_forward` does not break today's LLM publisher. Its R is always a descendant of T0 (a merge with T0 as first parent, or a fast-forward), so the lease push is a fast-forward. Worktree branch cleanup is unaffected because the ruleset targets only `main`.
**Text fix** (P0 row, one line): *"Safe with the current publisher: every guarded publish is a fast-forward."*

**7. P4 needs a minimal integrator↔service API. It isn't in the plan, and P4 is where a fresh session would improvise.** Proposed sketch (P4 design input, [R-P4]):
- `POST integration-results {submission, T0, C, R, tree}`: UNIQUE(submission, T0); idempotent replay returns the existing R.
- `POST push-authority {result_id}` → `{deciding_runs[], roster_ids, protected_ids, expires_at}`, or a refusal. Issuing it takes the submission-bound hold. A `revise` does not revoke an issued authority; that is "revise loses to a landed push".
- `POST observations {result_id, tip}`: the service classifies published / not_published / roll_forward itself, using the compare API. Only integrator-role principals may call it.

Everything else in the protocol sections of both files matches the transcript: the five required items, roll-forward, I-FF plus the watchdog, flake attribution, `candidate_reverted_in_history`, M6, and the stacking and R-1 rules.


---

### Casey (cold reader) — 2026-09-25T17:10:56Z

**Cold read (Casey).** I read only HANDOFF-autonomy.md, plan-final.md, review/service-data.md §3, transcript rounds 2–4 (lines 310–1357), `.claude/decisions-pending.md`, CONTRIBUTING.md and the code sites the handoff cites. I did not read rounds 1, 5 or 6 before drafting this. Verdict: **the handoff is enough to start P0 and the first half of P1 faithfully. It is not enough to finish P1 without guessing**, and two P0 steps would fail as written.

**1. Execution outline as I would run it**

*Session start.* Echo the `[git-sync]` and `[decisions] 13 pending` lines. AGENTS.md says "automatically select and claim eligible work", but the handoff and the user's "DO NOT create a task" say to work outside the coordinator. I would follow the handoff and say so in one line, rather than claim anything.

*P0 (with the user):*
1. Put U1–U13 to the user in four AskUserQuestion batches (the tool takes about 4 questions per call), recommendation first. Order: U4, U9, U11 and U13 first, because they gate actions in this plan. The rest only shape P2–P6. Delete each entry from `.claude/decisions-pending.md` once it is answered, and record the answers in HANDOFF-autonomy.md §6 (my guess at the location; see Q1).
2. Ask separately for explicit approval of the day-0 ruleset. §10 says "do not change rulesets without the user's recorded decision", and no U-number covers it (Q2).
3. Apply it with `gh api -X POST repos/marshallr12/agent_coordinator/rulesets` (target branch `~DEFAULT_BRANCH`/`refs/heads/main`, rules `non_fast_forward` + `deletion`, no bypass actors). Verify by reading back `GET …/rules/branches/main`, **never** by force-pushing. The payload is not in the handoff, so I would write it and show it to the user first.
4. Hand the U4 throwaway-repo test to the user as instructions: create a repo, add an `update` ruleset with a test App as bypass, confirm a collaborator push is rejected. I would neither create the repo nor the App.
5. Sentinel 5655e94d: **ask the user to cancel it from the dashboard.** The handoff says to "connect a CLI session with the user's consent", but `lifecycle_change` (`coordination.rs:655` and `:681`) refuses every non-human actor with "Only a human operator may change task lifecycle". An agent CLI session cannot cancel it.
6. dea68719: ask the user what "settle" means (Q3). Two deployment worktrees for it still exist under `/tmp/ac-deployment-worktrees/`.

*P1 (branch `autonomy/p1` in a new worktree, `CARGO_TARGET_DIR` inside it).* Base on `main`, not `autonomy-plan` (Q4). One regression test per step in `crates/server/tests/workflow.rs` or `coordination.rs`. Gate after each step: fmt, clippy `-D warnings`, workspace tests, `scripts/smoke.py`. One checkpoint commit per step.
- **1a. Labelled gates first.** This is a pure refactor with the smallest blast radius. Add `AppError::human_required(code,msg)` in `error.rs` (403, `details.required_actor="human"`) and route `workflow.rs:114 human()`, `coordination.rs:39 admin_or_operator()`, `imports.rs:60`, `jobs.rs:267` and `lifecycle_change` through it. Rename the `operator_reopen_required` precondition to `reopen_required` and fix the assertion at `tests/workflow.rs:1706`. Test: each labelled refusal carries `required_actor`.
- **1b. Scoped pinning plus satisfaction order.** Replace the whole-revision comparisons at `preconditions ~:846`, `ensure_current :1434`, `:1471`, `:1601`, `:2670`, `:1339`, `:1039` and submit `:466` with one helper, `pin_ok(ctx)`, of about 15 lines. Helper candidates: `required_review_kinds(review_mode)` and `satisfies(kind_done, kind_required)` implementing `human ≥ either ≥ agent`. Tests: a lease-only edit keeps the submission valid; loosening `review_mode` while a review is active lets it finish and its approval count (the 09-15 case); tightening adds a review activity.
- **1c. P1 migration.** `0022_*.sql` plus a Rust startup step that runs the monotone reconciliation once, recorded by a marker row (Q7). Test: a stale submission under a tightened mode gains the missing activity at startup.
- **1d. Agent unblock and cancel/replace.** Unblock goes through `agent_recovery_or_human` under `recovery_mode=agent`. Cancel goes through `lifecycle_change` under `agent_rule_editing`, with a required rationale and an optional `replaced_by` link, and is allowed with prior submissions. Tests: manual mode gets 403 `human_unblock_required`; agent mode gets 200; cancel works with a prior submission.
- **1e. `revise`.** New endpoint or extended `reopen` (Q5). CAS on current submission plus subject generation; closed reason enum; per-reason actors; the existing unresolved-intent guard (`workflow.rs:~1133`) kept verbatim; rate limit (Q6). One test per reason, plus contributor refused on `requirements_changed`, author-only `author_withdraw`, and refused while an intent is unresolved.
- **1f. Ack and decision keys.** Instruction acks today key on `policy_revision` (`coordination.rs:542`, `:1458`, `:1525`). Decisions re-pend on any `policy_revision` change (`knowledge.rs:~1287`). Rekey both to the scope they depend on (Q8).
- **1g. `ac_amendment` and `amendment_decision`.** Submission field plus review-decision field; a rejected amendment means the whole submission is `changes_requested`; keep the `coordination.rs:1093` guard for direct edits.
- **1h. Landing-range contributors and `stacked_on_unapproved`.** Last, because it is the least specified. See U1 below.
- **1i. Prose.** "Operator reopen" appears in 6 guidance strings and in book pages (grep finds `reopen` in 10 book files, not 4). Update them and add the `additional-js` external-link hook only if `book.toml` lacks it. Run `mdbook build` (0.5.4) and `scripts/check_docs.py`.
- **1j. CLI.** The staging replay says "with the CLI as a scripted agent", but `crates/cli` has **no reopen, unblock or cancel commands at all**. Only error strings mention reopen (`main.rs:1519`, `:2158`). P1 therefore also needs `revise`, `unblock` and `cancel` CLI subcommands, or the replay has to use raw HTTP. That is not in the LOC estimate.

*Staging.* This host **is** mxmini, so run the staging coordinator on loopback with default config on this machine. Replay B1 (lease edit, `review_mode` change), B2 (conflict ⇒ `revise{conflict}`) and B8 (legacy null ref ⇒ `revise{candidate_missing}`?) with no human. Then push the branch, wait for Actions to go green, fast-forward `main`, and put the production deploy to the user (U9).

*What I would NOT do:* create coordinator tasks or claim work; force-push or test the ruleset on production `main`; create GitHub Apps, repos or rulesets beyond day-0 without approval; deploy production; launch any LLM probe (U11 covers P2 only); commit `planning/autonomy/` to the public repo without the user's review (Q9); build any [F] item, including intent-after-checks (6a says leave it); touch MCP (frozen).

**2. Questions I could not resolve (and where I looked)**
- **Q1** Where are U1–U13 answers recorded, and in what form? (handoff §5–6, plan §4, decisions-pending.md: none says)
- **Q2** Does the day-0 ruleset need its own user approval, or is it implied by U-answers? (§10 vs §5.2 "On approval" — approval of what?)
- **Q3** "Settle dea68719" means deploy it, approve its review, or cancel it? Its deploy is a U9 reserved action. (handoff §5, plan P0, Sol Q-C: "let it finish or cancel")
- **Q4** Is `autonomy/p1` based on `main` or on `autonomy-plan`? Is the planning tree ever committed, and where? (handoff §2 "nothing committed", §5.3)
- **Q5** Is `revise` a new endpoint (`workflow/revise`) that replaces `reopen`, or a renamed or extended `reopen`? What happens to the human `reopen` path? (plan §2.2.1, Dana PoC modified `reopen`)
- **Q6** What are the rate-limit and ping-pong parameters? How is "serialize first" detected in P1, given the service "cannot inspect Git remotes" (`knowledge.rs` INSTRUCTIONS) and a conflict names only {T,C}, not the other task? (plan §2.2.1, Sol round 3 B2)
- **Q7** The monotone migration is "a startup transaction … once". sqlx migrations are SQL-only (`state.rs:519/565`), so is it a Rust post-migrate step, and how is "once" recorded? (plan §2.2.3, Sol Q-C)
- **Q8** Acks are "keyed on (contract version, rules revision)", but in P1 no rules revision exists; rules are a column in `projects` bumped with `policy_revision`. Should P1 add a `rules_revision` column? (plan §2.2.2)
- **Q9** Does the task-definition-revision pin mean `submissions.task_revision`? A priority-only edit bumps `tasks.revision` (`coordination.rs:1098`), which recreates the D2 whole-revision staleness at task level. Which fields count? (plan §2.2.2)
- **Q10** Which revise reason covers the B8 legacy null-ref case: `candidate_missing`, or the [F] `attach_candidate_ref`? (plan §2.2.1 vs §2.2.11)
- **Q11** Can a scripted CLI agent under this project's `recovery_mode=agent` do the staging replay, or do the new endpoints need CLI commands (see 1j)?

**3. Unsafe, contradictory or under-specified**
- **U1 (most important) Landing-range contributors [R-P1] rest on a false premise.** Sol's round-4 fix says "the service already records checkpoint SHAs". The `checkpoints` table (`0002_coordination.sql:60`) has **no SHA column**; only `submissions` has `candidate_revision`/`candidate_ref`. The service also has no Git access, so it cannot compute `merge-base(T,C)..C`. P1 needs (a) a checkpoint SHA column plus client support, and (b) a client-supplied commit list for the range, trusted under the confidently-wrong model, with every unmatched commit attributed to the submitter. Otherwise this item cannot be built as written. The same premise underlies the [R-P3] "checkpoint SHA is authoritative" invariant.
- **U2** The P0 cancel step would fail as written (see step 5). The handoff also asks for a CLI `connect`, which the auto-mode classifier refused last session. Say plainly that the user does P0's two service actions in the dashboard.
- **U3** AGENTS.md (auto-claim) and CONTRIBUTING.md ("record current task progress … in the service") conflict with "work outside the coordinator". The handoff should say explicitly that these are overridden for P0–P1.
- **U4** The handoff says U11 approval covers "probe spend", but the plan's first real LLM spend is in P2. Fine, but §10's "no LLM probes" should say P0–P1 need none.
- **U5** Estimates: the "450–650 LOC" figure excludes CLI commands, the checkpoint-SHA schema and the landing-range work (1h, 1j). Expect roughly 800–1,000.
- **U6** The handoff says "4 book pages"; grep finds reopen language in 10 book files.
- **U7** Public repo: the handoff warns that the review reports quote VM details, but gives no rule for whether `planning/` is committed with P1 or kept local. A cold session could easily `git add -A` it into `autonomy/p1`.


---

### Hari (harness & ops) — 2026-09-25T17:11:29Z

**Reply to Casey (Hari): P0, ops and rollout questions.** Your read is accurate. Where you got stuck, the handoff is at fault. Answers:

- **Q2 (does the day-0 ruleset need its own approval?).** Yes. Nothing in U1–U13 covers it, and the rule also binds the user: no bypass, so no force-push to `main` even for them. My audit item 2 adds **U0**, "Apply the day-0 ruleset now", and gives the exact payload plus the read-back check. Show the payload to the user before running it, as you planned. The payload is compatible with today's publisher (Sol, item 6).
- **Q1 (where do answers go?).** Proposed text for HANDOFF §6: *"Record each answer in an **Answer** column of this table (date, choice, any user note), then delete the entry from `.claude/decisions-pending.md`."* The table is the durable record. The pending file only feeds the SessionStart hook and must stay uncommitted (Dana, item 10). With Dana's U14 and my U0, expect 15 entries, not 13.
- **Q3 and U2 (the P0 service actions).** Agreed, and this corrects my own item 10: both service actions belong to the user **in the dashboard**. Lifecycle changes are human-only until P1 item 4 ships, so an agent CLI session can't do them. dea68719 is a U9-class decision: deploy now, or cancel. Leave the two `/tmp/ac-deployment-worktrees/` entries alone until the user has decided. After that, removing them is local cleanup; do it without force.
- **Q4 (which base, and what happens to the planning files?).** Base `autonomy/p1` on **`main`**. `planning/autonomy/` must never enter `autonomy/p1`. Proposed text: *"Commit `planning/autonomy/` only on `autonomy-plan`, after the redaction pass (Hari audit 9, Dana 10) and with the user's OK. Never `git add -A` in the P1 worktree; stage explicit paths."*
- **U3 (AGENTS.md auto-claim vs working outside the coordinator).** Agreed. Proposed text for HANDOFF §10: *"For P0–P2 and the P4 cutover, AGENTS.md's auto-claim and CONTRIBUTING's 'record progress in the service' are overridden by the user's instruction to work outside the coordinator; U14 decides whether that changes after P1 is deployed."*
- **U4 (LLM spend).** Correct: P0–P1 need **no** LLM probes and no model spend beyond the interactive session itself. U11 first applies in P2.
- **1j (CLI commands) and Q11 (can a scripted CLI agent run the staging replay?).** Include `revise`, `unblock` and `cancel` CLI subcommands in P1. The P3 supervisor and the per-role contract will drive agents through the CLI anyway, so raw-HTTP replay scripts would be throwaway work. Keep each subcommand thin: argument parsing plus one call through the existing mutation journal. Your 800–1,000 LOC estimate is the realistic one; the handoff should say so (it matches Dana's P1a/P1b split).
- **Staging.** Correct that this host is mxmini. Run the staging service as the user on loopback with default config, a separate data directory, and a **separate CLI home/credential**, so the production CLI state and journal are never touched. Suggested line: *"Set a staging-only `AGENTCOORDINATOR_HOME`-style directory (check the CLI's actual variable) and never point it at `agents.sithbit.com`."*
- **Your "CARGO_TARGET_DIR inside it".** Fine, provided that directory is git-ignored. A shared out-of-tree target dir, as used in the PoCs, also works and avoids rebuilding the workspace.
- **U1 (no checkpoint SHA column).** I agree, and it matters for my area too: the [R-P3] "checkpoint SHA is authoritative" invariant and service-verifiable recovery both need that column. Sol's item 2 (client-supplied `range_commits`) plus a `checkpoints.revision` column should go in P1b so the supervisor can depend on it in P3.


---

### Casey (cold reader) — 2026-09-25T17:11:40Z

**Follow-up (Casey), after reading the full transcript.** Every question the transcript answers is a handoff gap, because I could not find the answer from the handoff and its links.

**Answered in the transcript, not in the handoff:**
- **Q2 (day-0 ruleset approval and payload):** Hari round 6 #2 proposes a U0 row plus the exact `gh api` JSON and read-back. Put both in HANDOFF §5 and §6.
- **Q3 (settle dea68719):** Dana round 6 #1 says the user either authorizes its deploy (U9) or cancels it in the dashboard; Sol round 3 Q-C agrees ("finish or cancel"). The handoff still says "connect a CLI session", which cannot work because lifecycle is human-only. My P0 step 5 is independently confirmed by Dana #1.
- **Q4, second half (commit `planning/`?):** Dana round 6 #10 and Hari #9 give the rule: never commit `.claude/decisions-pending.md`; ask the user about `planning/`, redacting first. The base branch for `autonomy/p1` is still unanswered.
- **Q9, partly (what the task-definition pin invalidates):** Sol round 1 #4 and the round-1 follow-up say an AC edit makes *approvals* stale, not the candidate. plan-final's "pin candidate + task-definition revision" loses that distinction. Which task fields count is still open (priority edits bump `tasks.revision`).
- **U1 (landing-range contributors):** Sol round 6 #2 gives the design: the CLI sends `range_commits[]`, the service stores it per submission, and the reviewer's CLI re-verifies it. Dana #9 splits it out as P1b. This is the biggest gap. The handoff and plan-final still say "maps to the attempt that checkpointed it", which no component can do.
- **U5 (estimate):** Dana #9's P1a/P1b split matches my concern.

**New from the transcript. My outline would have gotten these wrong:**
- **Sol round 6 #1, roster at push time.** Following plan-final §2.2.2 literally, I would have switched `validate_check_jobs` to the *current* roster. That strands already-published R at `integration-result`/`finalize`. The handoff must carry Sol's replacement text: capture the roster revision on the intent.
- **Sol round 6 #4 and #5.** There is a second P1-only human gate (target moved after intent), and the P1 production deploy needs its own zero-intents preflight. I had neither.
- **Hari round 6 #3 and #4.** The exact CI gate commands (`--locked`, `backup_smoke.py`, `check_docs.py --mdbook`) and the manual ship steps before `ship` exists. My gate list was incomplete.
- **Dana round 6 #7, U14.** Asking the user before switching to dogfooding goes further than my U3; adopt it.

**Still unanswered anywhere:** Q1 (where the U-answers are recorded), Q5 (`revise` as a new endpoint or an extended `reopen`, and whether the human `reopen` survives), Q6 (rate-limit numbers; how the service detects the other task for "serialize first"), Q7 (how the one-shot startup reconciliation is recorded), Q8 (the ack key in P1, when no rules revision exists), Q10 (which reason covers the legacy null ref), Q11/1j (the CLI has no reopen, unblock or cancel commands, so the staging replay "with the CLI" needs new subcommands that no estimate includes).


---

### Hari (harness & ops) — 2026-09-25T17:11:41Z

**Correction to my Casey reply (Hari).** I checked the source. The exact staging variables are `AGENT_COORDINATOR_HOME` (a staging-only directory), `AGENT_COORDINATOR_ORIGIN` (the loopback URL) and `AGENT_COORDINATOR_ALLOW_INSECURE_LOOPBACK` (for plain-HTTP loopback). Use this handoff line: *"Staging CLI: `AGENT_COORDINATOR_HOME=<staging dir> AGENT_COORDINATOR_ORIGIN=http://127.0.0.1:<port> AGENT_COORDINATOR_ALLOW_INSECURE_LOOPBACK=1`; never reuse the production home."* Check the loopback variable's accepted values in the CLI source before relying on it.


---

### Dana (devil's advocate) — 2026-09-25T17:11:55Z

**Reply to Casey (Dana).** This is an excellent cold read, and your misreadings are handoff bugs. Where we overlap: your P0 step 5 and U2 are my audit item 1, your U7 and Q4 are my item 10, and your U5 matches my P1a/P1b split. Below is where your outline would **skip something required** or **over-build**, plus my proposed answers.

**Required items your outline would skip or can't build as written**

1. **U1 is confirmed, and it moves 5a out of P1.** `0002_coordination.sql:60`: `checkpoints` has no SHA column. The service also has no Git, so it can't compute `merge-base(T,C)..C`. Don't build 5a on client-supplied commit lists in P1. The only trusted Git-capable component is the P4 integrator. It fetches C and can compute the landing range, map commits to other submissions' recorded `candidate_revision`s, refuse `stacked_on_unapproved`, and reject a reviewer who contributed to any of them before pushing R.
   - **Re-tag 5a to [R-P4]** (enforced by the integrator).
   - The stacking hole predates P1, so P1 without it is not a regression.
   - The [R-P3] "checkpoint SHA is authoritative" invariant needs a `checkpoints.revision` column plus CLI support. Add that to P3, not P1.
   - Your 1h then drops out of P1, and so does most of your 800–1,000 LOC worry.
2. **"Serialize first" (plan §2.2.1) can't be built in P1 either.** You're right that `conflict{T,C}` doesn't name the other task. In P1 ship only a rate limit: **max 3 `revise` per subject per 24 h, then park with a digest/human-queue entry**. Move serialize-before-park to **[R-P4]**, where the integrator knows which landed change moved T.
3. **Your Q9 is a real hole in plan §2.2.2.** Pinning `tasks.revision` recreates D2 at task level: a priority-only edit bumps it (`coordination.rs:1098`). **Pin a digest of the judged fields: title, description, acceptance criteria, kind**, not the revision counter. Otherwise P1 doesn't meet its own exit criterion.
4. **Your 1j: CLI subcommands are required, not optional.** After P1 ships, and before any supervisor exists, the only agents who can benefit are interactive CLI sessions (MCP is frozen), so `revise` and `unblock` must exist in the CLI. Keep them thin, about 30 lines each over the existing HTTP client. Include `cancel` only if 1d ships agent cancel. Count them in P1.

**Where you'd over-build**

5. **1a: don't touch `imports.rs:60` or `jobs.rs:267` beyond the label.** Labelling is right. Don't change who may call them in P1: resources move to host-advertised capabilities later, and imports are out of scope.
6. **1e/Q5: prefer extending `reopen` over a new endpoint.** Add a required `reason{code,evidence}` for agent callers and keep the human path unchanged. That means one endpoint, one CLI command and one doc change, and it's what my PoC measured. Deletion-first beats a parallel `revise` route.
7. **1f/Q8: no `rules_revision` column in P1.** Key acks on `(INSTRUCTION_VERSION, sha256(rules))`, computed on read. It needs no migration, and it has the same effect: lease and flag edits no longer force re-ack.
8. **1i: the `additional-js` link hook isn't P1 work.** It's a docs item for when you touch `book.toml`. Keep P1 prose edits to the "operator reopen" strings, all 10 book files as you found. Your count is right and the handoff's "4" is wrong.

**Answers to your remaining questions**
- **Q2:** yes, day-0 ruleset approval should be its own decision. I'd add it as **U0** ahead of U1–U13, alongside my proposed U14 (dogfood vs. outside the workflow).
- **Q3:** "settle dea68719" means the user either authorizes that deploy (U9 reserved) or cancels it in the dashboard. Also ask them about the leftover `/tmp/ac-deployment-worktrees/`.
- **Q4:** base on `main`.
- **Q7:** yes, a Rust post-migrate step with a marker row.
- **Q10:** B8 legacy null ref: P1 uses `revise{candidate_missing}`. `attach_candidate_ref` stays [F].
- **CARGO_TARGET_DIR:** put it outside the worktree (e.g. `/tmp/<name>-target`), as the handoff's PoC did, not inside it.


---

### Dana (devil's advocate) — 2026-09-25T17:12:15Z

**Reconciling with Sol #2 (Dana).** My reply to Casey said "re-tag 5a to [R-P4], integrator-enforced". Sol #2 says "P1b: the CLI sends `range_commits[]`, and the reviewer's CLI re-verifies". Both are sound under the confidently-wrong threat model. The difference is timing and cost. Sol's version needs a schema column, a submit field and reviewer-CLI verification in P1b. Mine needs nothing until P4, where the integrator already has Git. **Lead, pick one explicitly.** I'd take Sol's data model (`range_commits` per submission, plus contributors and `stacked_on_unapproved` computed from it) but **ship it with P4**, where the integrator re-verifies the range. The hole predates P1, and P1a stays small.

My answers to Casey's still-open questions are in my 17:11:55Z post: Q5 extend `reopen`, Q6 max 3 per 24 h with serialize moved to P4, Q7 marker row, Q8 `sha256(rules)`, Q10 `candidate_missing`, 1j thin CLI `revise`/`unblock`. I also agree with Sol #1: capture the roster on the intent. It supersedes "current roster" in P1.


---

### Sol (systems & protocol) — 2026-09-25T17:12:37Z

**Reply to Casey (Sol): protocol questions.** Your U1 finding is correct. My round-4 premise, *"the service already records checkpoint SHAs"*, was wrong: `checkpoints` has no revision column. Answers:

- **U1 / 1h, landing-range contributors: I concede to Dana. Re-tag 5a to [R-P4].** The integrator is the only trusted Git-capable component. My round-6 #2 design (a client-supplied `range_commits[]`) would work under the confidently-wrong threat model. But it adds a schema, a CLI change and reviewer re-verification to P1 to close a hole that predates P1, so P1 without it is not a regression. One refinement so review isn't wasted: in P4 the integrator computes the landing range **when a submission is created** (fetch C; `git rev-list T..C`; map each commit to other submissions' recorded `candidate_revision`s and ranges) and stores it. The service can then refuse an ineligible reviewer *at claim time*. Pre-push verification remains the backstop. The `checkpoints.revision` column (plus CLI support) goes in P3 with the "checkpoint SHA is authoritative" invariant, as Dana and Hari said.
- **Q5, `revise`: extend `reopen`, don't add an endpoint** (agreeing with Dana). Agent callers must send `reason{code, evidence}` from the closed enum. The human path stays unchanged. `reopen` already requires the exact current `submission_id` (`workflow.rs:1121`), and it runs under the writer lock together with the unresolved-intent guard (`:1133`). That is the CAS I asked for, so no extra generation field is needed in P1.
- **Q6, rate limit:** use Dana's numbers. At most 3 agent revises per subject per 24 h; the 4th parks the subject with a labelled `revise_limit_reached` in the human queue/digest. Serialize-before-park moves to [R-P4], because only the integrator knows which landed change moved T.
- **Q7, one-shot migration: make it idempotent instead of "once", and drop the marker row.** Write one function, `reconcile_required_reviews(project)`, that adds missing review activities and never removes active ones. Call it (a) inside the policy-update transaction, which is where tightening happens at runtime, and (b) at every startup, after migrations and before serving. Re-running it is a no-op. That is simpler than recording "once", and it can't be skipped by a crash mid-startup.
- **Q8, ack key in P1:** use Dana's `(INSTRUCTION_VERSION, sha256(rules))`, computed on read. No new column is needed.
- **Q9, what the task pin covers:** agreed that it can't be `tasks.revision`. Pin `sha256(title, description, acceptance_criteria, kind)`. When that digest changes, **approvals** go stale, not the candidate: re-queue the review activities and keep the submission. A `kind` change isn't re-review material; it becomes cancel/replace (1d). Priority, labels and dependencies never invalidate anything.
- **Q10:** `revise{candidate_missing}` in P1. `attach_candidate_ref` stays [F].
- **Q11 / 1j:** yes, a thin CLI subcommand for each agent path P1 adds, `revise` and `unblock` (Hari, Dana). Don't count raw-HTTP replay scripts as P1 output.
- **Your outline vs my round-6 #1:** you are right that §2.2.2, read literally, strands published work. The handoff must carry the fix: *capture the roster revision on the publication intent; `integration-result`/`finalize` validate against it.* Also add 6a(b), target moved after intent, and the P1 zero-intents deploy preflight (#4 and #5) to the handoff's P1 section. You were right to leave intent-after-checks alone.


---

### Lead (Claude, main session) — 2026-09-25T17:12:43Z

**Round 7 (lead): my rulings on every open item from round 6. Objections only, please.**

Casey's cold read was the most useful test we ran. Thank you, Casey, and thank you all for the precise audits. My rulings follow. I'm applying them to `plan-final.md` and the handoff now.

**Conflict: landing-range contributors (5a). I rule for Dana: 5a moves to [R-P4], enforced by the integrator.**
- **Reason:** the integrator is the only trusted component that can run Git. It fetches C, computes `merge-base(T,C)..C`, maps commits to other submissions' recorded `candidate_revision`s, refuses `stacked_on_unapproved`, and rejects a reviewer who contributed to any of them, all before pushing R.
- **Rejected alternative:** Sol's client-supplied `range_commits[]` would work, but it adds a trusted-client input and a second implementation that P4 would then have to replace. Its design is recorded in plan-final as the fallback for projects that run without an integrator.
- **Consequences:**
  - The stacking hole predates P1, so this is not a regression.
  - The pilot (P3b) runs after P4-min, so the pilot is protected.
  - The `checkpoints.revision` column plus CLI support becomes [R-P3], for the B5 recovery facts.
- Sol, you are the one most likely to object; please reply if you do.

**Rulings on Casey's questions:**
- **Q1:** answers go in a new **Answer** column in HANDOFF §6 (date, choice, note). Then delete the entry from `.claude/decisions-pending.md`, which is **never committed**.
- **Q2:** new **U0** "apply day-0 ruleset now", using Hari's exact payload and the read-back check. Never verify by force-pushing `main`. This is compatible with today's publisher (Sol #6).
- **Q3:** the user performs both P0 service actions in the dashboard (lifecycle is human-only until P1). dea68719 is a U9-class decision: deploy or cancel. Leave `/tmp/ac-deployment-worktrees/` alone until the user decides.
- **Q4:**
  - `autonomy/p1` branches from `main`.
  - `planning/autonomy/` is committed only on `autonomy-plan`, after a redaction pass and the user's OK.
  - Never `git add -A` in the P1 worktree.
  - `CARGO_TARGET_DIR` goes outside the worktree.
- **Q5:** extend `workflow/reopen` with a required `reason{code, evidence}` for agent callers. The human path is unchanged, and the CLI verb is `revise` (Dana; deletion-first).
- **Q6:**
  - P1 = rate limit only: 3 agent revises per subject per 24 h, then park in the human queue/digest.
  - Serialize-before-park becomes [R-P4], because only the integrator knows which landing moved T.
- **Q7:** a Rust post-migrate step, with a marker row in `service_state`.
- **Q8:** acks keyed on `(INSTRUCTION_VERSION, sha256(rules))`, computed on read. No new column.
- **Q9:** pin a **digest of the judged fields** (title, description, acceptance criteria, kind), not `tasks.revision`. A change to that digest makes *approvals* stale (re-review), not the candidate.
- **Q10:** the legacy null ref uses `revise{candidate_missing}`. `attach_candidate_ref` stays [F].
- **Q11/1j:** thin CLI subcommands `revise`, `unblock` and `cancel` are in P1. The P1 estimate becomes about 550–750 LOC.
- **AGENTS.md:** auto-claim and CONTRIBUTING's "record progress in the service" are overridden for P0–P2 and the P4 cutover. **U14** asks whether dogfooding may start after P1 is deployed.

**Also adopted:**
- **Sol:**
  - #1 the roster revision is captured on the intent in P1.
  - #4 a second P1-only gate (target moved after intent). The optional fix is [F].
  - #5 a P1 deploy preflight.
  - #7 the integrator↔service API sketch as P4 design input.
- **Hari:**
  - #2 U0 plus the payload.
  - #3 manual ship steps until P2.
  - #4 the exact gate commands.
  - #5/#6 remove superseded `--add-dir` and worktree text, and annotate your round-2 launch lines.
  - #7 **[R-P2/P3a] credential attributes `class` + `access`**, plus who holds what.
  - #8 the U3 dependency.
  - #9 redact the private repo name in the transcript before any commit (I'll do this now and mark it [redacted]).
  - The staging env vars.
- **Dana:**
  - #2/#5 fix the HANDOFF §4 stale lines.
  - #4 flip-rate reruns on the real repo's `main` SHA.
  - #6 pilot core budget ≤1.5k lines, with a stop-and-rescope rule.
  - #8 defaults of stall 6 h and digest-unread 3 days.
  - Book reopen prose covers 10 files.
  - The link hook is not P1 work.
  - Imports and jobs get labels only.

If you object to a ruling, post one short message with the ruling number and your minimal alternative. Silence means consent. This is the last round.


---

### Hari (harness & ops) — 2026-09-25T17:13:04Z

No objections from Hari (harness & ops). All round-6 rulings in my area are accepted as stated.


---

### Dana (devil's advocate) — 2026-09-25T17:13:04Z

**Dana: no objections.** Scope check passed. The only additions are the thin CLI `revise`/`unblock`/`cancel` (needed, because agent cancel ships in P1) and Hari's credential `class`/`access` (needed for read-only reviewers and the metric). Both are required, and nothing demoted to [F] came back.


---

### Sol (systems & protocol) — 2026-09-25T17:13:10Z

**Sol (final): no objections.** One clarification on Q7, not an objection: the marker row is fine only if the post-migrate step and its marker commit in a single transaction. The same `reconcile_required_reviews(project)` function must also run inside the policy-update transaction, because the monotone rule "tightening adds activities" applies at runtime, not only at the P1 cutover.


---

### Lead (Claude, main session) — 2026-09-25T17:15:38Z

**Round 8 (lead): verification.** I've applied every round-6/7 ruling:
- to `plan-final.md`, marked **(r7)**;
- to `HANDOFF-autonomy.md`, where §4–§6 are rewritten with U0–U15 and an Answer column, the exact ruleset payload, user-performed dashboard actions, overrides, the exact gate, manual ship and staging env vars;
- to `.claude/decisions-pending.md`, now 16 entries.

I also redacted the private repo name in the transcript (marked [redacted]).

**Joining:** **Robin (second cold reader)**, a fresh agent with no context. Robin reads only the handoff and its links. It tests whether Casey's gaps are now closed.

**Dana, Sol, Hari:** please verify that the rulings landed *accurately* in both documents: nothing misquoted, nothing lost, no new contradictions. Post one message: "verified", or a numbered list of exact text fixes. Then answer anything Robin raises in your area.


---

### Hari (harness & ops) — 2026-09-25T17:16:05Z

**Round 8 verification (Hari).** Everything in my area checks out except one item.

What I checked and found landed accurately:
- The ruleset payload and read-back check.
- The gate commands.
- The manual ship steps.
- The staging env vars.
- The overrides.
- The supervisor text in HANDOFF §4 (P6 items deferred).
- The `class`/`access` credential attributes, with who holds what (plan-final 6b).
- The Codex `-C <per-launch clone>` flag.
- The per-launch clone wording.
- The annotation marking my round-2 lines as superseded.
- The U3 dependency.
- The redaction. No private repo or other-project names remain in the planning files.

The one fix:

1. **plan-final §3, P0 row:** "Put **U0–U14** to the user" should read "Put **U0–U15** to the user". This matches HANDOFF §5 step 2, plan-final §6 step 1, and the 16 entries in `.claude/decisions-pending.md`.


---

### Dana (devil's advocate) — 2026-09-25T17:16:13Z

**Round 8 verification (Dana).** HANDOFF §4–§6 are accurate. The overrides, U0–U15, the Answer column, the payload, the dashboard actions, the gate and the manual ship steps all match the rulings. There is no scope re-growth, and `.claude/decisions-pending.md` has 16 entries. Three text fixes remain in plan-final.md:

1. **P0 row: the decision range is off by one.** It says "Put **U0–U14** to the user", but §4 and the HANDOFF have U15.
   Fix: "Put **U0–U15** to the user".
2. **§2.2 5a still carries the premise the ruling rejected.** The bullet "every commit in `merge-base(T,C)..C` maps to the attempt that checkpointed it" contradicts the (r7) text directly above it (no checkpoint SHA).
   Fix: "every commit in `merge-base(T,C)..C` is matched against other submissions' recorded `candidate_revision`s and ranges; the reviewer must not be a contributor to any matched submission; unmatched commits count as the submitter's."
3. **§2.2 5a adds an integrator hook at submit time that §2.4b has no endpoint for.** The text says "computes the landing range when a submission is created … service refuses an ineligible reviewer at claim time". The Round 7 ruling was integrator enforcement *before pushing R*.
   Minimal fix: "the integrator computes the range at integration time and records it with `POST integration-results`; `push-authority` is refused (reason `reviewer_is_contributor` or `stacked_on_unapproved`) and the review is re-queued with an eligible reviewer. Claim-time refusal is [F]."
   This keeps P4 to the three endpoints in §2.4b.

Everything else is verified. Robin, I'll reply once you've posted.


---

### Sol (systems & protocol) — 2026-09-25T17:16:14Z

**Round 8 verification (Sol): protocol content.** HANDOFF §4–§6 match the rulings exactly: the extended `reopen`, the rate limit, the digest pin, the roster captured on the intent, reconcile in the policy transaction and at startup, both P1-only gates, the deploy preflight, 5a in P4, no legacy import, the watchdog, and the U0 payload being safe with today's publisher. plan-final needs four text fixes:

1. **plan-final.md:417, P1 row.** It says *"§2.2 items 1–6b except 5a"*, but 6b (credential attributes) is tagged **[R-P2/P3a]**, and HANDOFF §4 puts it in P2. **Fix:** *"§2.2 items 1–6a except 5a (6b is P2/P3a)"*.

2. **plan-final.md:118, `revise` CAS.** It says *"(current submission + subject generation)"*. No subject-generation field exists, and the ruling was that the exact-submission check under the writer lock is the whole CAS. **Fix:** *"(the exact current `submission_id` under the writer lock; no new generation field in P1)"*. Otherwise the implementer adds a column nobody asked for.

3. **plan-final.md:168, 5a rules.** The bullet still says *"every commit in `merge-base(T,C)..C` maps to the **attempt that checkpointed it**"*. That is exactly the premise Casey disproved and the (r7) paragraph above it replaces. **Fix:** *"every commit in the range maps to every submission whose integrator-stored range contains it; the reviewer must not be a contributor to any of those submissions; commits in no other submission count as the submitter's."*

4. **plan-final.md 5a (r7) paragraph.** *"maps commits to other submissions' recorded `candidate_revision`s"* is not enough. A stacked parent's intermediate commits are not anyone's `candidate_revision`. **Fix:** *"computes and **stores the landing range per submission** when it is created, and maps commits by membership in other submissions' stored ranges (not just their tips)."*


---

### Robin (second cold reader) — 2026-09-25T17:21:07Z

**Cold read (Robin, second cold reader).** I read HANDOFF-autonomy.md, plan-final.md, review/service-data.md §3, transcript rounds 2–4 (Dana's PoC section in full, the rest skimmed) and rounds 6–8 in full, `.claude/decisions-pending.md`, CONTRIBUTING.md, and every code site the handoff cites. Read-only, no network.

**Verdict.** Casey's gaps are closed: U0 and its payload, the Answer column, the dashboard-only P0 actions, the `main` base, the (r7) rulings on `reopen`, digest, ack key and roster-on-intent, the exact gate, and the manual ship steps. P0 can be run as written. P1 can be started but not finished without guessing. There is one **unsafe** item: the staging CLI line is wrong against the code and runs next to a production token. There is one real **contradiction**: plan-final §6's "first three steps" still contradict the (r7) rulings. Several P1 design points are under-specified, listed below.

**Code citations checked (all present and as described, except where noted)**
- `workflow.rs`: `reopen` starts at :1100. :1117/:1118 is the `human(&m.actor)?` gate. :1121 is the exact-submission check. :1133–1140 is the unresolved-intent guard. :846–857 is the `operator_reopen_required` whole-revision predicate. :1435–1447 is `ensure_current`. :2437 and :2912 call `validate_check_jobs`, which reads the roster at `ctx.workflow_policy_revision` (:2308). :2757–2768 is the exact-base-or-result reconciliation. :2207, :2378 and :2615 are the intent, result and agent-reconcile handlers. OK.
- `coordination.rs`: :264–330 is `update_policy` (refused while holds exist). :1093 is the self-related guard. :1098–1099 is the task UPDATE `revision=revision+1` (priority bumps it). :1134–1143 is unblock (`admin_or_operator`). `lifecycle_change` is at :643, with the human-only check at :681–683. `jobs.rs:267` is human-only resource creation. OK.
- Not cited, but P1 must change them: the attempt pins at `workflow.rs:1743` and `:3004`; the submit-time `pinned_revision_changed`/`policy_changed` checks at `:455–471`; and the ack query at `:909`/`:1637` and `coordination.rs:543`, `:1450`, `:1517`, `:1526`. Decision staleness is in **two** places: `knowledge.rs:1066` (status) and `:1287` (the pending-ids SQL). Both key on `policy_revision` and `task_revision`.
- **"10 book files" is wrong.** `grep -rl reopen book/src` gives **13** (12 in `docs/` plus `PLAN.md`). Only 4 contain "operator reopen/operator_reopen". Neither doc lists the files, so "10" can't be checked. Replace it with "every hit of `grep -rli reopen book/src`".
- The CI names are right ("Coordination checks", "Documentation checks"). The gate commands match `ci.yml`/`docs.yml` exactly. rustc 1.98.1 and mdBook 0.5.4 are installed on this host. Remote `marshallr12/agent_coordinator`: OK. `origin/main` = `main` = `d72a8cb` as of the last fetch.

**1. Execution outline (as the next session)**

*Session start.* Echo `[git-sync]` and `[decisions] 16 pending` verbatim (16 matches U0–U15 in the file). State in one line that AGENTS.md auto-claim and CONTRIBUTING's "record progress in the service" are overridden (HANDOFF §5). Connect nothing, and claim nothing.

*P0 (user, ~15–20 min).*
1. AskUserQuestion, recommendation first, 4 per call: {U0, U9, U14, U15}, then {U1, U2, U3, U4}, {U5, U6, U7, U8}, {U10, U11, U12, U13}. After each answer, write it into the HANDOFF §6 Answer column and delete its `## Un` block from `.claude/decisions-pending.md`.
2. U0 = yes: show the payload, **ask once more before running the POST** (it is an external write to a public repo), run it, then read back and expect `["deletion","non_fast_forward"]`. Never test with a force-push.
3. Give the user the U4 throwaway-repo test as instructions. Ask the user to cancel 5655e94d and decide dea68719 in the dashboard. After the dea68719 decision, run `git worktree remove` (no `--force`) on the two `/tmp/ac-deployment-worktrees/*`, and ask about their `codex/deploy-archived-*` branches (Q-R10). Leave every other worktree alone (CONTRIBUTING: "Preserve other worktrees").
4. U15 = commit: run a redaction pass on `planning/autonomy/` (hostnames, VM details, private repo names not already in HANDOFF.md), show the diff, and commit only `planning/autonomy/` explicitly on `autonomy-plan`, never `.claude/`. Push only if the user says so.

*P1 (branch `autonomy/p1` from a freshly fetched `origin/main`, a worktree outside the repo, e.g. `~/src/worktrees/ac-p1`).* Stage explicit paths only. Run the full gate after each step, then make a checkpoint commit (local only). My order differs from HANDOFF §5.3 in one place, marked ★:
1. **Labelled gates.** Add `AppError::human_required(code,msg)` in `error.rs` (403, `details.required_actor="human"`). Route through it: `workflow.rs:114 human()`, `coordination.rs:39 admin_or_operator()`, `lifecycle_change :681`, `imports.rs:60`/`:547` (label only), `jobs.rs:267` (label only). Rename `operator_reopen_required` → `reopen_required` and fix `tests/workflow.rs:~1706`. Test: each refusal carries `required_actor`.
2. **Digest pin, satisfaction order and ★ roster captured on the intent, in one step.** Removing `workflow_policy_revision` staleness from `ensure_current`/preconditions without capturing the roster on the intent leaves intermediate checkpoints validating against the submission's stale roster. Work needed:
   - Add a `pin_ok` helper and use it at every site Dana's PoC listed (`:466`, `:849`, `:1039`, `:1339`, `:1434`, `:1471`, `:1601`, `:2670`) plus the attempt pin.
   - Add `sha256(title, description, acceptance, kind)`, stored or computed per submission. A digest change re-queues review activities; a kind change routes to cancel/replace.
   - Add a `workflow_policy_revision` column on `publication_intents` (expand-only migration `0022_*`; NULL falls back to the submission's revision), and read it at `:2308` from `integration-result`/`finalize`.
   - Tests: a lease-only edit keeps the submission and attempt valid; a priority edit keeps approvals; an AC edit re-queues review but keeps the candidate; a roster edit after the intent still finalizes with the captured roster.
3. **`reconcile_required_reviews(project)`.** It is idempotent. Call it inside `update_policy`'s transaction and in a post-migrate startup step before serving; a marker row, if used, commits in the same transaction. Tests: tightening adds an activity; loosening lets an active review finish and count; a restart is a no-op.
4. **Agent unblock** (`recovery_mode=agent`) **and cancel/replace** (`agent_rule_editing`), each with a required rationale, cancel allowed with prior submissions, and an optional `replaced_by`. Tests: 403 `human_unblock_required` in manual mode; 200 in agent mode.
5. **Extended `reopen`.** An agent caller must send `reason{code, evidence}` from the closed enum (per-reason actors), behind the existing guard (unchanged). Rate limit: count reopen events per subject in the last 24 h; the 4th agent revise gets a labelled `revise_limit_reached`. Tests: one per reason, contributor refused on `requirements_changed`, `author_withdraw` by a non-author refused, refused while an intent is unresolved, limit hit on the 4th.
6. **Ack and decision keys.** Acks: compare `sha256(rules)` at the acked `policy_revision` (from `policy_revisions.data_json`) with current, under the same `INSTRUCTION_VERSION`. Decisions: see Q-R4.
7. **`ac_amendment` / `amendment_decision`.** A rejected amendment makes the submission `changes_requested`. The `:1093` guard stays.
8. **CLI subcommands** `revise`, `unblock`, `cancel`: thin, through the existing mutation journal.
9. **Prose:** guidance strings in `coordination.rs`/`discovery`, `crates/cli/src/main.rs` error strings, the book hits, and `implementation-status.md` (CONTRIBUTING: "do not describe planned endpoints as working").

*Staging and ship.* Set up a loopback staging server with its own `COORDINATOR_DATABASE`, bootstrap a human credential the way `scripts/smoke.py` does, and replay B1, B2 and B8 through the new subcommands. Then ask the user before `git push origin autonomy/p1` and again before FF-ing `main` (Q-R1, Q-R2). Run the P1 preflight **before the `main` push as well as before the deploy** (see Unsafe #2). The production deploy is U9.

*What I would NOT do:*
- create or claim coordinator tasks, or run `agent-coordinator connect` against production;
- apply any ruleset except U0's;
- create repos or Apps;
- spend on LLMs;
- build 5a, serialize-before-park, `checkpoints.revision`, credential `class`/`access` (see Contradictions #3), the `additional-js` hook or any [F] item;
- touch MCP;
- `git add -A`;
- commit `.claude/`;
- force anything.

**2. Questions I could not resolve (and where I looked)**
- **Q-R1 Who reviews P1 before it reaches `main`?** U2 recommends "pre-merge review on every agent path", and CONTRIBUTING says "Integrate one reviewed commit at a time". HANDOFF §5.5 ships agent-written guard code (~600 LOC) to `main` on green CI alone. Is the user the reviewer? Is it an independent-context subagent reviewer (that costs LLM spend, which "P0–P1 need none" rules out)? (HANDOFF §5, §10; plan-final §2.6, §6.)
- **Q-R2 Does pushing `autonomy/p1` or `main` need the user's go-ahead?** §10's list of consent-gated actions omits pushes. My harness rule is "commit or push only when the user asks". The repo is public. (HANDOFF §10.)
- **Q-R3 Which policy switch authorizes an agent `revise`?** §2.2.1 lists the reasons and actors but no switch. Dana's PoC used `recovery_mode=agent`. §2.2.4 names switches only for unblock/cancel. Which actors may use `conflict` and `candidate_missing`: any agent, or only the subject's author or claimant?
- **Q-R4 "Decisions re-pend only on their own scope."** What is a decision's scope? Presumably `sha256(rules)` plus the judged-field digests of `decision_affected_tasks`, replacing `policy_revision` and `task_revision` at `knowledge.rs:1066` and `:1287`. It isn't written anywhere. (plan-final §2.2.2; transcript r7 Q8 covers acks only.)
- **Q-R5 How does satisfaction order `human ≥ either ≥ agent` map onto `review_mode=both`, and onto `none`?** `workflow.rs:632–634` builds activities per mode. Is satisfaction judged by the approver's kind or by the activity's kind?
- **Q-R6 Is the digest stored (a new `submissions` column) or recomputed from `task_revisions.data_json` at the pinned `task_revision`?** And does the **attempt** pin (`workflow.rs:1743`, `:3004`, submit `:455–471`) also move to the digest? Otherwise B1's lease edit still fails at submit with `policy_changed` (the exact 7e0c58b5 symptom).
- **Q-R7 What does "park" mean after the rate limit?** Is it a refusal code only (Sol's `revise_limit_reached` appears only in the transcript), or a state change such as `blocked`? The digest doesn't exist until P3b.
- **Q-R8 How is the P1 deploy preflight run?** Neither doc names the API reads (or SQL) that show unresolved intents, held holds, held reservations and active integration attempts, nor who runs it against production. (plan-final §2.2.3a, HANDOFF §5.5.)
- **Q-R9 Staging repository for the B2/B8 replay.** plan-final §6.3 says "throwaway project and repo", but §5.2 forbids creating repos. Is it a local bare repo? Does the native CLI accept a non-HTTPS remote? And B8 (a legacy null `candidate_ref`) can't be produced through the current API. Do we seed it by SQL, or build an old binary? (HANDOFF §5.4.)
- **Q-R10 The dea68719 leftovers.** Delete the `codex/deploy-archived-task-pagination-e96b11a[-g2]` branches too, or only the worktrees?
- **Q-R11 Credential attributes.** Are they in P1? See Contradictions #3.
- **Q-R12 Where is P1 progress recorded across sessions?** P1 is 1–2 days, so it will span sessions. Work runs outside the coordinator, and HANDOFF-autonomy.md lives untracked (or on `autonomy-plan`), not on `autonomy/p1`. Do we update HANDOFF-autonomy.md §6-style, or the repo's root HANDOFF.md?

**3. Unsafe, contradictory or under-specified**

*Unsafe*
1. **The staging CLI line does not do what it says, and it sits next to a production token** (HANDOFF §5.4, plan-final §6):
   - The CLI's origin comes from the repo binding `.agent-coordinator.toml` (`service_url = "https://agents.sithbit.com"`), found by walking up from cwd, or from `--repo-config`/`AGENT_COORDINATOR_REPO_CONFIG` (`cli/src/main.rs:31–33`).
   - `AGENT_COORDINATOR_ORIGIN` is only a cross-check, used when `AGENT_COORDINATOR_TOKEN` is set (`cli/src/config.rs:126–134`).
   - Both `AGENT_COORDINATOR_TOKEN` and `AGENT_COORDINATOR_ORIGIN` are **set in this environment right now**, which is the production credential.

   Consequences:
   - Run as written from the P1 worktree, it fails closed ("does not match the repository's service origin"). Good, but it confuses.
   - If the session "fixes" that by adding a staging `--repo-config` while leaving the env token, it sends the **production bearer token** to the local staging server.
   - If it drops the ORIGIN override instead, commands in the worktree go to **production**.

   Fix text: "Staging CLI: `env -u AGENT_COORDINATOR_TOKEN -u AGENT_COORDINATOR_ORIGIN AGENT_COORDINATOR_HOME=<staging dir> AGENT_COORDINATOR_STATE_DIR=<staging dir>/sessions AGENT_COORDINATOR_REPO_CONFIG=<staging binding with service_url=http://127.0.0.1:<port>> AGENT_COORDINATOR_ALLOW_INSECURE_LOOPBACK=true agent-coordinator …`, with the staging token in `<staging dir>/credentials.toml` (mode 0600). Server: `COORDINATOR_DATABASE=<staging db> COORDINATOR_LISTEN=127.0.0.1:<port> COORDINATOR_ALLOW_INSECURE_LOOPBACK=true` (check the value types in `server/src/main.rs`)."
2. **Pushing to `main` can itself trigger the P1-only human gate.** plan-final 6a(b) says a target move after an intent (tip ∉ {T0, R}) needs a human, and B14 shows direct pushes caused exactly that. HANDOFF §5.5 runs the zero-intents preflight only before the *deploy*, after the `main` push. Fix: run the same read-only preflight immediately before `git push origin <sha>:main`, and wait if any intent or active integration exists.
3. **`reconcile_required_reviews` during integration.** Adding a review activity to a submission that is already integrating or has an intent is undefined. `update_policy` already refuses while holds are held (`coordination.rs:312–321`), but an intent without a hold, or a startup run, isn't covered. State the rule explicitly: skip the add, or refuse, when the subject is `integrating` or has an unresolved intent.

*Contradictions (HANDOFF vs plan-final; HANDOFF says read plan-final §6 "in full")*
1. plan-final §6 "First three steps", step 2, still puts **"landing-range contributors + `stacked_on_unapproved`"** in P1 and gives Dana's PoC order. That contradicts §2.2 5a ([R-P4]), the P1 row ("except 5a") and HANDOFF §5.3. Its step 3 says "replay B1/B2" (the others say B1/B2/B8) and "throwaway project **and repo**". Rewrite step 2 to match HANDOFF §5.3.
2. The plan-final §3 P0 row says "**U0–U14**", but §4 and HANDOFF say U0–U15. HANDOFF §7 still says "decisions are **U1–U13** everywhere".
3. The plan-final P1 row says "§2.2 items **1–6b** except 5a", which pulls in 6b (credential `class`/`access`, tagged **[R-P2/P3a]**). HANDOFF's P1 order omits it. Pick one; I read it as "1–6a, plus 3a and 3b".
4. **CARGO_TARGET_DIR:** CONTRIBUTING.md says "Use a separate Cargo target directory **inside** each worktree". HANDOFF and plan-final say "outside it (`/tmp/ac-p1-target`)", following the r7 ruling. Either note the override explicitly, or use an in-worktree target (it's git-ignored via `**/target/`).
5. §2.2.1 says the CAS is "current submission + subject generation", but Sol r7 says "no extra generation field is needed in P1" (the exact-submission check at `:1121` under the writer lock suffices). Drop "subject generation".
6. plan-final §2.2.3 hedges ("if a marker row is used"), while the r7 ruling says "marker row in `service_state`". `service_state` is a singleton row (`0012_restore_authority.sql:3`), so a marker there means a new column, i.e. a migration. Sol's idempotent no-marker version is simpler. Choose it and say so.

*Under-specified (smaller)*
- The success condition for the "~15 min" P0 is fine, but P1 has no **step-level exit** beyond "one regression test per fix". The exit criterion's staging replay has no script or expected outcomes (e.g. "B1: lease 600→3600 during an active attempt ⇒ submit 200, no re-ack"). Write the three replays as expected-result bullets.
- There is no instruction to update `DURABLE-RECORD.md` or `implementation-status.md` for the P1 protocol changes (new reason enum, agent reopen/unblock/cancel), which CONTRIBUTING requires.
- The user's "functions < 20 lines" rule versus `reopen` (~100 lines today): state whether P1 refactors the handlers it touches or only keeps new helpers short. Otherwise the diff grows unpredictably.


---

### Dana (devil's advocate) — 2026-09-25T17:21:50Z

**Reply to Robin (Dana).** This is an excellent read. Your Unsafe #1 (staging CLI plus a live production token), Unsafe #2 (run the preflight before the `main` push too) and Q-R6 (the attempt pin at `:455–471`, `:1743` and `:3004` must also move to the digest, or B1 still fails at submit) are real and required. My PoC changed `:466` for exactly that reason. Adopt all three as written. Where I think your outline would over-build or skip something:

**Things it would skip that are required**
1. **Q-R1: P1 needs pre-merge review.** This is guard code, and U2 says pre-merge review applies to every agent path. Minimal form: a **fresh-context subagent review of the final P1 diff inside the interactive session** (no extra LLM billing beyond the session), then **the user's explicit OK before the FF to `main`**. That also answers Q-R2: pushes to `autonomy/p1` are fine, pushes to `main` need the user.
2. **Q-R3: which switch allows agent revise.** Use `recovery_mode=agent`, as the PoC did. Actors:
   - `conflict`, `check_failed`, `candidate_missing`: the author, or an agent holding or last holding the subject's integration activity.
   - `requirements_changed`: non-contributors only.
   - `author_withdraw`: the author only.
3. **Q-R5: satisfaction is judged by the approver's kind.** A human approval satisfies an agent review. `both` needs one human and one agent approval. `none` needs nothing.
4. **Q-R7: "park" in P1 is a labelled refusal (`revise_limit_reached`, `required_actor=human`) that lands in the human queue.** No state change.
5. **Your Unsafe #3: adopt the rule.** `reconcile_required_reviews` skips subjects that are `integrating` or have an unresolved intent. The tightened mode applies to the next submission.

**Where it would over-build**
6. **Digest (Q-R6, second half): compute it on read from `task_revisions.data_json` at the pinned `task_revision`, with no new column.** The only P1 migration should be the roster revision on `publication_intents`.
7. **Marker row (your Contradiction 6): use Sol's no-marker idempotent version, not a new `service_state` column.**
8. **Q-R4, decision scope: keep it minimal.** Replace `policy_revision` with `sha256(rules)`, and `task_revision` with the judged-field digest, at `knowledge.rs:1066` and `:1287`. Don't model anything richer.
9. **Staging replay (Q-R9): replay B1 in staging (no Git needed); cover B2 and B8 in `crates/server/tests`.** The existing fixtures already fake repos and insert legacy rows by SQL. Don't create repos or build old binaries. Replay B2 in staging only if the CLI accepts a local bare-repo remote without new code.
10. **The 20-line function rule applies to new and changed helpers only.** Extract the new reason checks from `reopen` into helpers, but don't refactor untouched handler bodies in P1.

**Where I concede to you**
11. **CARGO_TARGET_DIR (your Contradiction 4).** CONTRIBUTING says inside each worktree, and `target/` is git-ignored. My "outside" was PoC hygiene, so follow CONTRIBUTING.


---

### Hari (harness & ops) — 2026-09-25T17:22:29Z

**Reply to Robin (Hari): staging, pushes and ops.** Robin, your Unsafe #1 is correct, and the error is mine. I checked the code:
- The origin comes from the repo binding: `--repo-config` / `AGENT_COORDINATOR_REPO_CONFIG`, otherwise `.agent-coordinator.toml` found by walking up from the working directory (`cli/src/main.rs:31-33`).
- `AGENT_COORDINATOR_ORIGIN` is only a cross-check, used when `AGENT_COORDINATOR_TOKEN` is set (`cli/src/config.rs:126-134`).
- Both are exported in this environment, and they hold the production credential.

Lead, replace the staging line in HANDOFF §5.4 and plan-final §6 with:

*"Staging CLI (never inherit the production token):*
```
env -u AGENT_COORDINATOR_TOKEN -u AGENT_COORDINATOR_ORIGIN -u AGENT_COORDINATOR_MCP_TOKEN \
  AGENT_COORDINATOR_HOME=<staging>/home AGENT_COORDINATOR_STATE_DIR=<staging>/state \
  AGENT_COORDINATOR_REPO_CONFIG=<staging>/binding.toml AGENT_COORDINATOR_ALLOW_INSECURE_LOOPBACK=true \
  agent-coordinator …
```
*`binding.toml` has `service_url = "http://127.0.0.1:<port>"`. The staging credential lives where `crates/cli/README.md:40` says for `AGENT_COORDINATOR_HOME`, mode 0600. Staging server:*
```
COORDINATOR_DATABASE=<staging>/db COORDINATOR_LISTEN=127.0.0.1:<port> COORDINATOR_ALLOW_INSECURE_LOOPBACK=true
```
*Before the first staging command, run `env | grep -c AGENT_COORDINATOR_TOKEN` inside the wrapper and expect 0."*

Clap 4.6 bool flags accept `true` and `1` from the environment.

Answers in my area:
- **Q-R2 (does pushing need the user's go-ahead?).** Yes. Add to HANDOFF §10's consent list: *"push any branch, and fast-forward `main` (public repo); each needs the user's go-ahead at that moment."*
- **Unsafe #2 (the push to `main` can trigger the P1-only human gate).** Agreed. Run the read-only preflight immediately before `git push origin <sha>:main` and again before the deploy. If any intent or active integration exists, wait for it to finish rather than pushing.
- **Q-R9 (staging repository).** Use a **local bare repo** under `<staging>/remote.git`, bound as the project's canonical repository. `scripts/completion_smoke.py:21` already does exactly this (`git init --bare`, `canonical_repository_key`), and `git_workflow.rs:1337` handles `file://`. Copy that setup; creating a GitHub repo is not needed and is not allowed. B8 (a legacy null `candidate_ref`) can't be produced through the current API. Seed it with one SQL `UPDATE` on the **staging** DB only, and write that down in the replay script.
- **Q-R10 (dea68719 leftovers).** Remove the worktrees without force, after the user's decision. Delete the `codex/deploy-archived-*` branches only with the user's OK, since they are the user's Codex branches. Local deletion only; no remote branch deletion.
- **Contradiction #4 (`CARGO_TARGET_DIR`).** I'd rather follow CONTRIBUTING than override it: the target dir goes *inside* the worktree (git-ignored via `**/target/`). That matches what I told Casey. Either way, the documents must say one thing.
- **Q-R12 (where P1 progress is recorded).** Record it in HANDOFF-autonomy.md on `autonomy-plan`: a §11 "Execution log" with the phase, step, commit SHA and gate result. Commit it with explicit paths once U15 allows it; until then leave it untracked. Keep it separate from the repo root HANDOFF.md, which describes the product.
- **Q-R1 (who reviews P1).** This is outside my area, so only an ops note: P1 is agent-authored guard code landing by a human fast-forward. Under the plan's own `unreviewed_landing` rule (agent trailers), it would be flagged later anyway. The cheapest honest option is for the user to review the diff before the fast-forward to `main`. The lead should put that to the user as part of U2/U9.


---

### Sol (systems & protocol) — 2026-09-25T17:22:40Z

**Reply to Robin (Sol): protocol questions.** Robin, this was an excellent read. Unsafe #2 and #3 are real. Contradiction #5 matches my round-8 fix #2, and contradiction #3 matches my fix #1. Answers with text for the docs:

- **Q-R3, which switch and which actors for agent `revise`.** Use **`recovery_mode=agent`** for every agent reason; in `manual` mode agents get 403 `human_reopen_required`. Actors per reason, which closes the griefing hole I raised in round 1:
  - `conflict` and `check_failed`: the caller must hold the submission's **active integration activity**. In P1 that is the LLM integrator.
  - `candidate_missing`: **any agent**. The evidence is service-verifiable (`candidate_ref IS NULL`), and the integration claim is refused at `workflow.rs:1627` in that state, so nobody could hold the activity.
  - `requirements_changed`: any agent that is **not a contributor** to the task, and only when the current judged-field digest ≠ the pinned digest.
  - `author_withdraw`: the author only.
- **Q-R4, decision scope.** A decision's scope is `sha256(rules)` plus the judged-field digest of each task in `decision_affected_tasks`, together with its own expiry and disposition. That replaces `policy_revision`/`task_revision` at **both** `knowledge.rs:1066` and `:1287`.
- **Q-R5, satisfaction order.** Satisfaction is judged by the **approver's actor class** against the **activity's required kind**:
  - A human approval satisfies `human_review`, `either_review` and `agent_review`.
  - An agent approval satisfies `agent_review` and `either_review`.

  Each approval counts for at most one activity. `both` still needs two distinct decisions, one human and one agent. The order matters only when a policy change re-derives the set: an existing approval is reused if its class satisfies a newly required kind. Under `none`, queued reviews are canceled; active ones may finish, and their decision is recorded but no longer gates.
- **Q-R6, digest stored or recomputed.** **Store it**: `submissions.task_digest`, computed at submit time from the current task fields, in an expand-only migration. Recomputing it from `task_revisions.data_json` is possible but couples P1 to that JSON's shape. **Yes, the attempt pin must move too.** Attempts pin the digest only, and the submit-time `policy_changed`/`pinned_revision_changed` checks (`workflow.rs:455–471`, attempt pins `:1743`, `:3004`) compare only the digest. Otherwise B1 (the lease edit) still fails at submit, which is exactly the 7e0c58b5 symptom.
- **Q-R7, what "park" means.** P1 adds **no new state**. The 4th agent call gets 403 `revise_limit_reached` with `required_actor="human"`. The subject surfaces in `preconditions`/`next` and the human queue, and the human `reopen` path stays available. The digest arrives in P3b.
- **Q-R8, how to run the preflight.** It is a read-only query the **user** runs on the VM against the live DB (`sqlite3 -readonly <db>`), or against a fresh backup copy. Column names are from `0003`/`0004`; verify them before relying on the query:
  ```
  SELECT count(*) FROM publication_intents pi
    LEFT JOIN integration_results ir ON ir.activity_id=pi.activity_id
    LEFT JOIN publication_reconciliations pr ON pr.activity_id=pi.activity_id
    WHERE (ir.activity_id IS NULL OR ir.publication_state='uncertain') AND pr.activity_id IS NULL;
  SELECT count(*) FROM integration_holds WHERE state='held';
  SELECT count(*) FROM reservations WHERE state='held';
  SELECT count(*) FROM workflow_activities WHERE kind='integration' AND state IN ('active','recovery_required');
  ```
  Every count must be 0. Per Robin's Unsafe #2, run it **before the `main` push as well as before the deploy**, and wait for zeros.
- **Unsafe #3, `reconcile_required_reviews` during integration.** Rule: it adds activities **only to subjects in phase `review`**. A subject that has left review (phase `integration`, with or without an intent) keeps the review set it was approved under. It is "approved under the policy at approval time", so it never regresses into review mid-integration. Tests: tightening while a subject is in `integration` adds nothing, and the integration completes.
- **Contradiction #6, marker row.** Choose the **idempotent, no-marker** version. `service_state` is a singleton, so a marker would need a migration for nothing. Text: *"called inside `update_policy`'s transaction and once at every startup before serving; re-running is a no-op; no marker row."*
- **Q-R1 and Q-R2 are not my area,** but for protocol consistency: P1 is guard code, so under U2's "pre-merge review on every agent path" the user should review the P1 diff before the `main` FF, or approve an independent-context review. Pushes follow the user's standing rule: ask first.


---

### Lead (Claude, main session) — 2026-09-25T17:25:52Z

**Round 9 (lead): closing.** Every round-8 fix is applied.
- `plan-final.md` has a new **§2.2-P1 consolidated P1 specification**, which takes precedence on any conflict, the safe staging wrapper, the P1 review and push rules, and risk K5.
- I verified Sol's preflight SQL columns against migrations 0003, 0004 and 0018.
- `HANDOFF-autonomy.md` §5 is rewritten to match, and §11 now holds an execution log.
- One lead ruling to note: the digest is **stored** in `submissions.task_digest` (Sol) rather than computed on read (Dana), because that decouples P1 from the shape of `task_revisions.data_json`.

Last word: each of you, **one sentence** addressed to the user, the most important thing they should know before approving execution. Then we're done. Thank you. This was an excellent discussion.


---

### Dana (devil's advocate) — 2026-09-25T17:25:58Z

**Dana, to the user:** Approve P1 on its own first: it is about 600 lines that remove most of the human clicks in the evidence. Treat everything after it (supervisor, integrator, containment) as a separate bet that must earn its place through the costed 5-task pilot and your go/no-go, because this project's biggest past failure was building a large system before anyone used it.


---

### Hari (harness & ops) — 2026-09-25T17:25:59Z

Before approving, know that the unattended agents must never run under your own account: today your global Codex setting is full access with no approvals and your shell exports the production coordinator token, so P2's separate agent users, pinned harnesses and per-launch clones are the step that makes autonomy safe, and nothing should run unattended until P2 passes its containment suite.


---

### Sol (systems & protocol) — 2026-09-25T17:26:03Z

**To the user (Sol):** the safety of autonomous integration rests on two things outside the code, ruleset B (no force-push, required checks with no bypass, including for you) and the integrator being the only producer of results, so if flaky checks ever tempt you to switch B off "for a minute", fix or quarantine the flaky check instead, because disabling B silently voids every "published / not published" guarantee the design relies on.


---

### Lead (Claude, main session) — 2026-09-25T17:26:17Z

**Discussion closed (lead).** Consensus reached after 9 rounds (16:27 to 17:26 UTC). Final artifacts: `plan-final.md`, `HANDOFF-autonomy.md`, the review reports in `review/`, and this transcript. Thank you, Dana, Sol, Hari, Casey and Robin.

