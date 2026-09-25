# Agent Coordinator: development history and what it says about autonomy

Scope: git history (256 commits, 2026-09-09 → 2026-09-25), HANDOFF.md, DURABLE-RECORD.md,
`book/src/docs/implementation-history.md`, 52 Codex rollout files that mention agent_coordinator
(about 30 top-level sessions and 22 child/subagent/assessor sessions), `~/.codex/history.jsonl`,
and one prior Claude Code session (`9782905e…`, 2026-09-22). The Claude `memory/` directory is empty.

Caveats about the sources:
- Only this Linux workstation's (`mxmini`) harness logs are available. Commits made on the Windows
  workstation `MINIAIR` (much of 2026-09-14…16, the 30 commits authored as "Codex" on 2026-09-17,
  and the Windows side of the 2026-09-20/22 integrations) have no prompt history here.
- Ten commits are authored as "Agent Coordinator integration" and dated `2000-01-01`. The service's
  guarded publisher makes these merge commits with a fixed timestamp (body: `target <sha> / candidate
  <sha>`), so `git log --reverse` order is misleading. Use `--date-order` or the candidate dates.
- No credentials appear in this report. Token and password handling is described only in general terms.

---

## 1. Timeline of phases and pivots

| Phase | Dates | Commits | What happened |
|---|---|---|---|
| **P0 Planning by interview** | 09-09 16:36–19:03 | 2728a13…8c79dcb (6) | A long Codex prompt plus `/goal`: "Plan a vendor-agnostic agent coordination service. Ask me questions until the goal and implementation … is very well defined." About 20 multiple-choice decisions were recorded (see §2). The user added multi-project support and a private GitHub repo. |
| **P1 Big-bang implementation with parallel subagents** | 09-09 19:10 → 09-10 11:17 | 4faca37…677253a (~120) | "start the implementation" / "use subagents to delegate work in parallel … can use smaller ai models". Codex v2 multi-agent threads with nicknames (Hypatia=service_auth, Tesla=native_cli, Volta=web_dashboard, Gibbs/Popper/Mill=jobs, Leibniz/Meitner=completion, Boyle/Heisenberg=knowledge/artifacts). Backlog items 1–6 (foundation, jobs/worktrees, reviewed completion, knowledge/artifacts, operator, backup, clock/retention/packaging/capacity) plus 6.1 MCP (56 tools) and 6.2 mdBook. Rust went from 6.5k to 42.9k lines in about 16 hours. |
| **P2 CI billing and acceptance** | 09-10 | b074cf7, f53dae9, a9d7375 (PR #1) | GitHub Actions was blocked by billing, so the user said "Make the repository public" and "Retry item 6.2". Item 7 was a physical Windows (MINIAIR) + Linux (mxmini) acceptance run through a disposable Cloudflare tunnel with an RSA-encrypted credential handoff. |
| **P3 Production deployment and service-led startup** | 09-10 → 09-14 | aa52094, 019ffb6, 20e5c9f, dac3125…a8b6c84 | GCP e2-micro VM, Caddy, Cloudflare, `agents.sithbit.com`. **Pivot:** the local BACKLOG.md/HANDOFF.md workflow was retired. `/api/v1/info` now publishes `agent_startup`, AGENTS.md/CLAUDE.md became a 6–7-line bootstrap, and the live service became the only queue. MCP-first startup was added, with the CLI as fallback. |
| **P4 Dogfooding: agents work the queue** | 09-14 → 09-17 | 7de3fca, 2014f10, 6f8e2b1, 2e491b1, 741c6e4…6df1217, 7a3b43b, a9c879d… | Agents take tasks from the live queue. Friction shows up at once: premature stops, sandbox escalations, push/submission approvals, reviews that need a separately-credentialed subagent, lease expiry during idle turns. A series of guidance commits followed ("continue claiming", "prioritize reviews", "continue automatically after subagent reviews"). |
| **P5 Integration jams across workstations** | 09-19 → 09-22 | ce64361, f5c20b3, 9bb9a28, 88a81f7, f67474b | Unblocks, reservation holds, integration target locks, a required `native-windows-tests` check that Linux cannot satisfy, and an unpublished candidate commit that existed only in a Linux worktree. **Pivot:** Linux-first required checks (ce64361). The Claude Code session was used as a Linux↔Windows peer over the cross-session bridge. |
| **P6 Closing autonomy gaps in the protocol** | 09-23 → 09-24 | 1ba7d7a, 34a3061, 3b206f5, 1f34034, 138050e, d5d66b3, a2c4b1d | Evidence-gated *agent* publication reconciliation, blocker precondition inspection and state wait, mandatory durable candidate refs (`refs/agent-coordinator/candidates/<id>`), CLI protocol compatibility negotiation, and task attachments. |
| **P7 Direct UI polishing outside the workflow** | 09-23 → 09-25 | 2c681e9, 5330a56, 0c671cc, a89fbc3, 0d62114, d52d1d9, e96b11a, 3bff21a, d72a8cb | The user repeatedly tells Codex to **bypass** the coordinator ("DO NOT create a new agent coordination task"), iterates in the in-app browser, then says "commit, push, deploy to production". Releases v0.1.0/v0.1.1 went out. |

Commit density: 64 on 09-09, 70 on 09-10, 19 on 09-14, 39 on 09-17, then 1–15 per day. Once
agents had to go through the coordinator's own workflow (P4 onward), throughput fell by roughly
5–10×.

---

## 2. User intent evolution (quotes)

**Original vision (09-09):**
> "Create a service that enables autonomous agents to query for tasks to be performed … Use of the
> service should be defined in the AGENTS.md / CLAUDE.md file with sufficient prose for even a
> small-parameter agent to be able to connect … Agents should also be able to communicate lessons
> learned … so that over time, agents become more educated, talented, and efficient."
>
> "Since you are an agent creating this service, you will know better than I the best workflow…"

**The autonomy policy the user chose on day one** (Codex `request_user_input` answers). Every setting
the current "100% autonomy" goal depends on was already chosen here, per project:
- "Agents create and claim tasks; each project configures whether completion requires review."
- "Another agent may recover [expired work] after checking saved work and still-running jobs; each
  project can require manual recovery instead."
- Review: "Each project chooses independent agent review, human review, or both; default to
  independent agent review."
- "Yes; each project can allow automatic integration or require human authorization."
- "Each project may also authorize agents to change binding project rules without human approval."
  The user picked this over the recommended option.
- Complete means "After required review, integration into the target branch, and validation of the
  integrated result."
- "Give every authenticated person and agent access to every project."
- Scale: 20 projects / 50 sessions / 100k tasks. Linux + native Windows clients. Public HTTPS.

**Push for hands-off execution (09-10 02:10):**
> "complete all backlog items sequentially without operator review or intervention unless you need
> me to answer questions before proceeding. You (the main agent) should take on the task of
> reviewing the wor[k] completed for each backlog item…"

**Dogfooding prompts become terse (09-17 → 09-24):** "work on tasks" (×3), "work on agent
coordinator tasks" (×3), "review agent coordinator tasks".

**Frustration and learning about the harness (09-17, 09-20):**
> "why have you stopped?"
> "Is there some mechanism that the agent running on windows could have made this request to you directly?"
> "why was the explicit step missed, and how can we prevent that mistake from happening again?"
> "How do I set the codex cli to default to full access permissions when I start a new session?"

**Explicit autonomy goal handed to an agent pair (Claude, 09-22):**
> `/goal keep waiting for the windows session to get back to you, then work it out between yourselves
> how to get this task to the "Done" stage without my continued intervention`
> "for integrations prepare / publish stage, I grant you permission in advance to 'Merge without review'"
> "I give you permission to spawn a subagent to perform the review and bypass the harness guard"

**Retreat from the workflow for small work (09-20 → 09-25):**
> "update the documentation now; do not create a new task in the coordinator service for the update - just do it directly"
> "For the work below, ignore the AGENTS.md workflow instructions. DO NOT create a new agent coordination task…" (09-24, 09-25)
> "Perform the following work without creating a new task in the Agent Coordinator service or using the Agent Coordinator tool." (09-24)
> "Perform this work outside of the agent coordinator service workflow. DO NOT create a new task for this" (09-25)

In two weeks the arc went from "agents know best, make it autonomous" to "fully autonomous
completion policy on paper" to "I relay messages, click dashboard buttons, and approve pushes" to
"for quick things, skip the coordinator entirely".

---

## 3. Manual interventions and friction (with frequencies)

Counts are from about 30 top-level Codex sessions and one Claude session on this workstation.
Windows-side sessions are not visible, so these are lower bounds.

### 3.1 Re-prompting to keep going: at least 20
- Planning `/goal`: "continue" ×6 between question batches (09-09). The goal loop auto-continued
  but ended its turn after each question batch.
- Backlog phase: "Resume the next backlog item", "Continue with the next backlog item", "continue
  with the next item", "continue" (09-09/10). This was fixed by the 02:10 "complete all backlog
  items sequentially" instruction.
- Dogfooding: "work on tasks" / "work on agent coordinator tasks" (×7 new sessions), "why have you
  stopped?", "continue", "work on another task", "when the result comes back, resume task
  processing", "continue the recovery, then work on the task(s)", "done; continue".
- Agent self-report on 09-17: *"I stopped prematurely because I treated the feature's breadth as a
  reason to hand it off. That was my mistake."* Commit 7de3fca "Tell agents to continue claiming
  eligible work in the same session" followed directly.

### 3.2 Approvals of external effects: at least 8
- 09-17: *"The publication request was denied by the environment because pushing to the remote needs
  explicit user authorization"* → user: "approve push". Then *"sending repository-derived evidence …
  to the separate coordinator service … requires a separate explicit authorization"* → "approve
  submission". Later "push changes", "submit".
- 09-22 (Claude): "Merge without review" pre-grant, and permission to "bypass the harness guard" for
  a review subagent.
- About 700 sandbox escalation requests (`require_escalated`) went to Codex's auto-review assessor
  across the 09-17 TUI sessions (for example 143 in `01a0af45`, 122 in `01a0afca`, 113 in
  `01a0af63-a8f4`). All were `allow`, one was rated high-risk. Each one costs latency and tokens. On
  09-20 the user switched Codex to full access, and escalations then fell to about 0.

### 3.3 Spawning reviewers by hand: 5 explicit prompts
"create subagents to perform the reviews" (09-17), "spawn a subagent to perform a review, wait for the
results, then continue" (×2, 09-17), "assign a subagent to perform the review" (09-20), "assign a
subagent … if that will progress the task" (09-24). Mitigations were 6f8e2b1 (opt-in subagent review
identities with separate credentials), 2e491b1 (one shared agent/human review slot), 2014f10
(reviews before new work) and 7a3b43b (continue automatically after subagent reviews). Even so, 09-20
and 09-24 still needed a prompt.

### 3.4 Human-only dashboard operations done through a browser: at least 10
| Date | Operation | Why it was human-only |
|---|---|---|
| 09-19/20 | Unblock task `dc1ff9c7` (2 sessions) | *"This task's unblock endpoint is human-only, and the dashboard requires a human sign-in."* |
| 09-20 | Reconcile publication (`target_moved`) | *"the saved publication intent requires a human reconciliation before the service will release the hold."* The agent filled the form and the user clicked save. |
| 09-22 | Reopen stale submission, then reconcile publication ×2 | *"reopening a stale submission is gated to the authenticated human operator dashboard"*. The user did it on a Chromebook ("it asks for 'observed remote commit' 'observed remote tree' and evidence"). |
| 09-24 | Reopen for revision (missing durable ref) | "guide me through reopening…" The agent filled the dialog in Brave and the user clicked. |
| 09-24 | Reopen stale submission after an integration merge conflict | Same pattern. |
| 09-25 | Create `production-host` resource, then unblock the deploy task | *"The coordinator has no registered resource for the production VM … Please create a production-host resource … provide its exact key and capacity."* Resource creation is human-only. |
| 09-20 | Required-check roster change (removing the Windows check) | *"The roster is human-managed, so it must be changed through an authorized human/operator account."* |

Pattern: the agent does all the diagnosis, fills the dialog, and leaves "the final save button for
you". The human click adds no information; it only supplies authority.

### 3.5 Logging into browsers: at least 6
"open the internal browser and wait for me to log in", "open the in app browser to the login page;
then I will log in", "I did click the button to log in", *"credentials did not autofill, so it remains
at sign-in"*, *"No external browser is currently connected to Codex"* (×3: 09-20 twice, 09-24).

### 3.6 Credentials and identity
- HANDOFF: *"The operator saved the `codex-miniair` token in protected Windows configuration after
  manually copying it."* The Copy token clipboard failure then became its own task (`4b68f1c7`), which
  took the 09-22 two-agent marathon to finish (28a5d01 → 88a81f7).
- Item 7 (09-10): *"The supplied public key is malformed/truncated"*; *"Lost one-time Windows credential
  must be revoked and reissued"*.
- Features that followed: repository binding download (18e4333), credential file download
  (5c24655/99d8940), project credentials (6f8e2b1), `session adopt-mcp` (046f683), legacy identity
  adoption (211ee44), a separate evaluator identity (57fb43d).

### 3.7 Expired leases and recovery: at least 4 visible
- 09-17: *"Active implementation attempt expired while awaiting the status request, so the service
  correctly marks it `recovery_required`."* The lease ran out while the harness was idle between user
  turns.
- 09-17: *"The saved commit and checkout are unavailable on this workstation, and it was never
  published … releasing the task for a runner that retains the source."*
- 09-19: a task held a reservation on MINIAIR *"until 20:26:58 UTC"*, and the user could only wait.
- 09-20: the Windows agent could not continue because *"the immutable candidate bcf976… and prior
  prepared result 424c6e… are unavailable from origin, this Windows Git store, and the original Linux
  checkout."* The user relayed this by hand. Root cause, in the agent's words: *"an enforcement/design
  failure, not merely a skipped reminder."* The fix was task `b2b20364`, which landed as 1f34034
  (durable candidate refs).

### 3.8 Integration conflicts and locks: at least 7
- Target lock held by another integration: 09-17 (×3 attempts), 09-20 (`5c85ebb2` and `2f6d4f3b`),
  09-24.
- The same hot file conflicted twice: `scripts/test_task_detail_copy_controls.mjs` (09-22 rebase of
  28a5d01 against the pagination rewrite bcf9767, and 09-24 candidate 69c9f0d vs main e7f55a1). The
  shared browser-test fixture file is a serialization bottleneck.
- A user commit made outside the workflow (9bb9a28, "Remove redundant CLAUDE.md…", 09-21) broke a
  README link, so the required `documentation` check failed for **every** candidate. In Claude's
  words: *"no candidate can pass integration on this project right now."* The fix was 88a81f7.

### 3.9 Relaying between workstations: 3
09-20 the user pasted the Windows agent's message into the Linux session. 09-22 the user pasted
"Here are instructions from the windows workstation". After that, Claude's cross-session bridge
carried about 15 peer messages, and task `ac4c4f9b` was filed: "Add blocker-inspection endpoint and
cross-session message delivery confirmation", which became 34a3061/3b206f5.

### 3.10 Housekeeping: at least 4
"kill any background terminalss that are no longer needed" (9 stale `tail -f` shells), "clean up and
delete any local git worktrees that aren't needed" (44 worktrees; 14 removed, then all), "kill the
localhost web and clean up any temp files", and agent-reported stale `sleep` shells and Cargo target
locks.

### 3.11 Opting out of the workflow: 6
Listed in §2. This is the strongest single signal. For UI tweaks, the implement → push candidate ref
→ submit → independent review → integration hold → four required checks → guarded publish → deploy
task → production-host reservation → review of deploy chain costs more than the change is worth.

### 3.12 Product decisions: about 25
About 20 planning answers (09-09), the Windows acceptance scheduling, adding MCP/mdBook to the
backlog, the roster/Linux-first decision (09-20), the auto-refresh default (09-24), plus explanatory
questions ("what is a 'durable candidate ref?'", "why is validation required on windows when the
service runs on linux?"). The user trusted the recommendations: nearly every answer was the
"(recommended)" option. The exceptions were public HTTPS, all-projects access, and agents changing
binding rules. In each exception the user chose more openness.

---

## 4. Harness constraints observed

**Codex (TUI and Desktop, CLI 0.153–0.155)**
- *Sandbox:* no DNS for the service host (*"this sandbox cannot resolve its hostname"*). The
  `sccache` wrapper was unavailable. Long first Rust builds were killed and left Cargo target locks.
  The official MCP-client tests could not bind loopback (`Operation not permitted`). Git worktree
  metadata lives outside the writable root, so commits need escalation. `git push` needs explicit
  user approval. All of this disappeared once the user set full access (09-20), which also shows the
  sandbox was the main source of Codex approval friction.
- *Guardian/auto-review assessor:* each escalation spawns a child "assessor" thread. That produced
  about 700 allow verdicts on 09-17, all justified by "user authorized … trusted project guidance".
- *Turn ending:* Codex ends a turn after a summary. Leases keep ticking while the harness waits for
  the human, so ownership expires during idle time (the 09-17 `recovery_required`). The `/goal`
  mechanism kept planning alive but still yielded after each question batch.
- *Subagents:* v2 threads were good for parallel build-out (P1). For reviews, a subagent inherits the
  parent credential, so the service rejects it as not independent (`reviewer_not_independent`) unless
  it registers as a distinct subagent identity. The parent often did not think to spawn one without
  being told.
- *No cross-session wake-up:* *"[Agent Coordinator] does not provide agent-to-agent messaging or wake
  another active/ended Codex session."*
- *Browsers:* only the in-app browser was reliably available. The external browser was frequently
  "not connected". Auth cookies did not carry over, so a human login was needed.
- *Model/UI noise:* every Desktop session injects a large `<recommended_plugins>` block, and in-app
  browser context is attached to every prompt.

**Claude Code (09-22)**
- *Auto-mode safety classifier:* it blocked `[Self-Approval]` when a session tried to register a
  review subagent for its own submission: *"a subagent spawned under my own credential to review my own
  work is self-approval in substance."* The user had to grant permission explicitly. The service's
  separate-identity model and the harness's notion of independence disagree.
- *Cross-session bridge* worked for peer coordination, but peers "cannot grant escalation". There is
  no delivery confirmation.
- `/goal` Stop hook plus background `until …; sleep` polling loops kept the session alive for about
  2.5 h. Many polls hit their `timeout` (exit 124), and the goal evaluator deferred itself while
  background work ran.
- The agent correctly refused "Merge without review" when the real blocker was a *failed* Windows
  check, not review.

**MCP host limitations**
- MCP gives coordination only. Git, local producers (jobs), and binary transfers stay native CLI
  (HANDOFF). The launcher needs an absolute executable path and does not supervise background clients.
- Hosts do not persist MCP mutation keys across interruptions. That led to a standalone MCP adapter
  with a durable mutation journal (a9c879d, 38e4548, bcef934) and `session adopt-mcp` to hand an MCP
  session to the CLI.
- CLI/protocol drift across workstations led to "Advertise and verify CLI compatibility" (138050e,
  about 1k lines) and "Reject clients missing required protocols" (d5d66b3).

**Windows vs Linux**
- Early fixes: guardian log initialization (67aef7d), verbatim-path normalization for Git (0652c15,
  e0abadb), native CLI session state hardening (6194ddc), Windows backup-helper warnings (6f8d829).
- A required `native-windows-tests` check meant only MINIAIR could publish, and hand-offs between hosts
  lost unpublished objects. WSL on Windows could run the Linux checks only after *"sccache/DrvFs
  permission errors on /mnt/c, and a PATH resolution quirk"*. Resolution: Linux-first required checks
  with Windows CI non-blocking (ce64361).
- Other infrastructure: GitHub Actions billing block (repo made public), and Cloudflare Browser
  Integrity Check error 1010 blocking Python clients (disabled for the API host).

---

## 5. Complexity growth

| Snapshot | Date | Rust LOC | Web LOC | Markdown LOC | SQL migrations |
|---|---|---|---|---|---|
| 5f8db25 foundation | 09-09 | 6,521 | 689 | 2,923 | 2 |
| 60ec757 reviewed completion | 09-09 | 20,379 | 973 | 3,897 | 4 |
| 4afaa29 knowledge/artifacts | 09-09 | 28,977 | 1,191 | 4,535 | 8 |
| 65d4192 Linux acceptance | 09-10 | 39,904 | 1,616 | 9,073 | 16 |
| 677253a MCP | 09-10 | 42,869 | 1,616 | 9,301 | 16 |
| 6df1217 delegation | 09-17 | 47,724 | 1,864 | 11,647 | 19 |
| 1f34034 durable refs | 09-24 | 50,159 | 2,006 | 11,906 | 20 |
| HEAD d72a8cb | 09-25 | 51,026 | 2,498 | 12,036 | 21 |

- **Front-loaded rigor:** most of the complexity came in the first 30 hours, before any real agent
  had used the service. That includes clock-rollback fencing, receipt replay and compaction,
  producer/guardian launch identity, publication-intent CAS, restore epochs, capacity evidence, and
  deterministic packaging. DURABLE-RECORD.md is 30 hard-won invariants, almost all about correctness
  under uncertainty. Few are about agent ergonomics.
- **Instruction surface ballooned:** the portable `agent-startup.md` served via `/api/v1/info` went
  from 179 lines (f8afa8f, 09-14) to **591 lines**, through 21 revisions in 10 days. `CLI.md` is 1,201
  lines. The book has 34 docs chapters. The original goal was "sufficient prose for even a
  small-parameter agent". About 41 of 256 commit subjects are guidance, clarification, or explanation
  ("Clarify…", "Explain…", "Guide agents to…").
- **State machine growth:** the workflow now has `work_status` (ready, active, waiting_review,
  waiting_integration, integrating, blocked, recovery_required, done, and archived) crossed with
  activity kinds (implementation/agent_review/human_review/integration), attempts, generations,
  leases, reservations, integration holds, publication intents, reconciliation dispositions, required
  check rosters pinned per submission, and delegation grants. The user needed the agent to explain
  which statuses a roster change forces into reconciliation.
- **Features that patched autonomy gaps** (each adds surface):
  - Continuation and priority guidance: 7de3fca, 2014f10, 7a3b43b.
  - Separate subagent review identities and a shared review slot: 6f8e2b1 (690 lines), 2e491b1 (559).
  - Delegated task-definition editing via human-granted scopes: 741c6e4 (426). An agent had judged
    that a "human-managed delegation domain" was needed.
  - Evidence-gated *agent* publication reconciliation: 1ba7d7a (653). This removes one human click.
  - Precondition inspection and `wait` on state: 34a3061/3b206f5 (781). This replaces the polling
    loops.
  - Durable candidate refs: 1f34034 (886).
  - CLI compatibility negotiation: 138050e/d5d66b3 (about 1,000).
  - Standalone MCP adapter journal: a9c879d (787).
  - Deployment evidence artifacts: 8b08278.
- **Features that mostly added complexity without removing a human step:** the knowledge and
  decision subsystem with Markdown import/export (heavily built, rarely visible in any dogfooding
  transcript), objectives, the capacity and acceptance evidence machinery, task lifecycle and archive
  UI, and theme preferences. Some of these are useful, but none sits on the autonomy critical path.

---

## 6. Top 10 insights for full autonomy

1. **The policy was never the blocker; residual human-only endpoints were.** Agent review,
   auto-integration, agent recovery, and agent rule changes were chosen on 09-09. What actually needed
   a human was a set of hard-coded human-only operations: unblock, reopen stale candidate, reconcile
   publication, create resource, edit the required-check roster, and grant delegation. Each of these
   needs a project-policy switch that permits an agent with evidence to do it, recorded with the same
   provenance. 1ba7d7a shows the template.

2. **The human clicks added authority, not information.** In every browser episode the agent had
   already diagnosed the problem and filled the dialog ("I left the Reopen for revision button
   unclicked for you"). In autonomous projects these should be evidence-gated agent actions, with a
   separate agent co-signing when the action is destructive.

3. **Leases must not expire because a harness turn ended.** Codex and Claude idle between turns while
   the lease clock runs, which produces `recovery_required` and a full recovery ceremony. Options:
   longer leases with heartbeats handled by a local daemon (the guardian already exists), or an
   automatic "self-recovery" fast path when the same session returns with a clean checkpoint.

4. **Independent review must be spawnable without a human prompt, and it must satisfy the harness's
   idea of independence.** Five manual "spawn a reviewer" prompts plus a Claude `[Self-Approval]`
   block show that a subagent under the same credential looks like self-approval to the harness. The
   durable fix is a pool of *distinct reviewer principals* (or a review queue served to other
   sessions or workstations) that the service dispatches, not the author's own subagent.

5. **Anything another workstation needs must be durable by construction.** The missing-candidate
   incident was a design gap, not agent error. 1f34034 fixed candidates. Apply the same rule to
   prepared integration results, job logs, and checkpoints: the service should refuse a state
   transition whose artifacts are not fetchable from the recorded remote.

6. **Required checks define which hosts can finish work.** One Windows-only required check made every
   integration depend on a single machine and on hand-offs between hosts. For autonomy, check
   placement should be declared (capability tags), and the service should route integration to a
   host that can satisfy all checks, or split checks per host without restarting the whole set. Also
   guard against direct pushes that break a global required check (9bb9a28).

7. **Agents need a wake-up and notification channel, not just durable handoffs.** "Is there some
   mechanism that the agent running on windows could have made this request to you directly?" The
   answer was no, so the user relayed messages. 34a3061's state-wait is a start. Add webhooks,
   long-poll subscriptions, or an external runner that starts a harness session when a task needs a
   capability the current host lacks.

8. **The harness sandbox, not the coordinator, drove most approval friction.** About 700 escalations
   and the push/submission approvals disappeared with Codex full access. Autonomous projects need a
   documented harness profile per vendor: network allowlist for the service and Git remote, writable
   worktree roots, per-worktree `CARGO_TARGET_DIR`, and pre-approved `git push` to `refs/agent-coordinator/*`.

9. **The workflow cost is too high for small changes, so the user routes around it.** Six "don't
   create a task" prompts in P7. A "lightweight lane" (for example, doc/UI-only changes with a
   single-host fast path, agent review, and auto-integrate) would keep small work inside the system.
   Otherwise the queue stops reflecting reality and direct pushes break shared checks.

10. **The instruction surface is itself an autonomy risk.** A 591-line startup guide plus a 1,201-line
    CLI guide is far from "enough for a small-parameter agent". Most "stopped too early" and "missed
    the push step" failures were guidance failures that were later encoded as enforcement. Continue
    that direction. Prefer server-side preconditions (`inspect` → next action), a single
    `next-action` endpoint that tells an agent exactly what to do (claim a review, recover, integrate,
    wait), and shrink the prose. Hot shared files (`test_task_detail_copy_controls.mjs`, `web/app.js`)
    also need splitting, because merge conflicts on them repeatedly forced reopen cycles.
