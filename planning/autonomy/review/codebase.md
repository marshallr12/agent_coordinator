# Agent Coordinator: autonomy-gap review of the codebase

Scope: branch `autonomy-plan` (== `main`, HEAD `d72a8cb`). Read-only review. File references are
`path:line` relative to the repository root. The live evidence comes from saved
service snapshots in `scratchpad/svc/` (orientation, policy history, 6,153 events). I did not contact
the service.

**Bottom line.** Each of the four "autonomy" settings works only on the happy path. Each one sits
next to a human-only operation that the normal failure modes run into. The worst three are these:

1. **Human-only `reopen` is the only way from a stale or unmergeable candidate back to revision.**
   Several things make a candidate stale or unmergeable: any project-policy revision (including
   an agent rule edit), any change to the required-check roster, a merge conflict at integration
   time, and a failing integration check.
2. **The required-check roster and the physical resources the checks reserve can only be created
   by a human.** Every code integration needs both.
3. **Agent publication reconciliation needs the original workstation's local journal file.**
   Anything else (a moved target, a lost sandbox, a new claimant) goes back to a human while a
   global repository hold blocks every other integration and every policy edit.

The live event log backs this up. On the project with every autonomy setting enabled, the human
performed:

- 26 `submission.reopened`
- 9 `integration.publication_reconciled`
- 6 `task.unblocked`
- 4 `reservation.resolved`
- 3 `workflow_policy.updated`
- 8 activity claims and 4 review decisions

Agents performed **zero** publication reconciliations. Agents recorded 48 publication intents but
completed only 39 integrations out of 89 submissions.

---

## 1. Architecture overview

| Crate | LOC (main files) | Role |
|---|---|---|
| `crates/server` | workflow.rs 3,111; coordination.rs 1,975; jobs.rs 1,619; artifacts.rs 1,524; backup.rs 1,428; knowledge.rs 1,380; operator_access.rs 1,120; auth.rs 999; mcp.rs 533 + mcp/catalog.rs 727; discovery.rs (instructions) | Axum + SQLite (sqlx). Owns every state machine. `Mutation::begin` gives each write an idempotency receipt, a transaction and an event. |
| `crates/cli` | main.rs 4,249; worktree.rs 791; state.rs 979; artifact_transfer.rs 989; session_adoption.rs 523 | Native `agent-coordinator` client. It has a durable local mutation journal and runs Git, jobs and artifacts locally. |
| `crates/local` | git_workflow.rs 2,258; lib.rs 1,374 | Git: candidate checkpoint refs, a deterministic merge (`merge-tree` plus a fixed-identity `commit-tree`), and a CAS push with `--force-with-lease` behind a durable intent journal. |
| `crates/mcp-transport` | main.rs 406; journal.rs 352 | stdio MCP adapter (`agent-coordinator-mcp`). It adds a durable journal, `coordinator_transport_status` and `coordinator_transport_retry`. |
| `crates/core`, `crates/client` | ~1.1k | Shared DTOs and the HTTP client. |

### Surface counts

- **HTTP:** about 119 distinct `/api/v1/...` and `/mcp` path literals and about 114 method-handler
  bindings. The workflow module alone has 14 routes (`workflow.rs:23-81`).
- **MCP tools:** 62 server tools (`mcp/catalog.rs`), 1,142 words of tool descriptions, plus 2
  adapter-local tools.
- **CLI:** 31 top-level commands and **86 leaf commands**. For example, `integrations` has 10
  subcommands: list, status, claim, renew, release, prepare, publish, reconcile, reconcile-agent,
  finish.
- **Error codes:** about **162 distinct conflict or precondition codes** (grep of
  `conflict(`/`add(`/`"code":`), plus generic ones. **Every `forbidden` shares the code
  `operation_not_permitted`** (`error.rs:32-33`), so an agent cannot tell a human-only gate from a
  role gate by its code.

### States an agent has to reason about

- Workflow activity kinds (4): agent_review, human_review, either_review, integration.
- Activity states (5): queued, active, completed, canceled, recovery_required.
- Subject phases (4): review, integration, revision_needed, done.
- Derived `work_status` (about 11): ready, in_progress, blocked, recovery_required, waiting_review,
  waiting_integration, integrating, validating, done, canceled, planned.
- Attempt states (6) and attempt modes (2: work, recovery).
- Job states (6), reservation states (3), hold states (2).
- Publication states (3) and reconciliation dispositions (3).
- Local integration phases (4) and local publication outcomes (4).
- review_mode (5) and recovery_mode (2).

### Policy storage

- `projects` row: review_mode, recovery_mode, lease_seconds, rules, agent_rule_editing,
  automatic_integration, allow_subagent_reviews.
- Separate `workflow_policies` row: canonical repository key plus the required-check roster.
- Every submission, activity, attempt, decision and acknowledgment is pinned to both revisions.

---

## 2. Each capability, end to end

### 2a. Agent-performed required review (`review_mode` agent/either, `allow_subagent_reviews`)

**Happy path**

1. `submit` (`workflow.rs:423`) supersedes the prior submission and records the submitter as a
   contributor (`:571`). It sets the attempt to submitted, then creates review activities from
   `review_mode` (`:633-690`) and a queued `integration` activity whose task gets
   `blocked_reason='Required reviews are pending.'` (`:692-694`).
2. The reviewer calls `activity_preconditions` (`:833`) and then `claim_activity` (`:1564`).
   - For `agent_review|either_review` by an agent it runs `ensure_independent_reviewer`
     (`:1648-1656`). For `either_review` by a human, `human()` (`:1657`) applies.
   - `ensure_independent_reviewer` (`:1939-1962`): with `allow_subagent_reviews=1` and a
     session bound to a project `subagent_identity`, only contributions by that identity or
     session count. Otherwise any contribution by the **principal** or the session disqualifies
     the reviewer.
3. `review` (`:1964`) re-checks independence (`:2023-2033`) and inserts the decision.
   - `approved` plus all approvals leads to `phase='integration'` and clears the integration
     task's blocked_reason (`:2110-2121`).
   - `changes_requested` leads to `revision_needed` and cancels sibling activities (`:2067-2094`).
     The implementer can then re-claim through `guard_normal_claim` (`:1200`).
4. Subagent identities are self-service. Session registration with
   `subagent:{project_id,name,parent_session_id}` creates or reuses them (`auth.rs:770-870`), and
   helpers are recorded through checkpoint `contributor_session_ids` (`coordination.rs:1655-1675`).

**Gates that stop an agent even with autonomy enabled**

| # | Gate | Evidence | Human needed? |
|---|---|---|---|
| R1 | Single-principal setups (one agent credential) cannot review at all unless `allow_subagent_reviews` is on. A "new session with the same agent identity does not make you independent". | `workflow.rs:1950-1953`; `discovery.rs:17` | Yes: issue a second agent credential or enable the flag. |
| R2 | **MCP cannot create a subagent session.** The MCP session ID and proof come from host config headers or process env (`mcp-transport/src/main.rs:80-92`), and registration must match the configured header (`mcp/catalog.rs:567-579`). A Claude Code subagent inherits the same MCP server, so it has the same session, so it is not independent. Only `agent-coordinator connect --subagent NAME --parent-session ID` can do it. | as cited | Needs native CLI. |
| R3 | Code-review claims should go through the native CLI, which fetches and verifies the candidate ref before claiming ("For code submissions, use the native CLI"). | `book/src/docs/agent-startup.md:182-186` | Needs native CLI. |
| R4 | `candidate_checkpoint_missing`: legacy submissions without `candidate_ref` "need operator reopening". | `workflow.rs:944-953, 1627-1635` | Yes. |
| R5 | Any policy change after submission gives `operator_reopen_required`, and `ensure_current` rejects the claim. See section 2d. | `workflow.rs:846-857, 1430-1447` | Yes: reopen is human-only (`:1117`). |
| R6 | `scoped_decisions_pending`: decisions become pending again on **any** policy-revision change, affected-task revision change, expiry, or a non-`allow` disposition. Human-required decisions then need a human again. | `knowledge.rs:1287`, `:1137-1142` | Often yes. |
| R7 | `instructions_required`: acknowledgment is per (session, policy revision, `coordination-v8`). Agents can redo it, but only if they re-read orientation after every policy bump. | `workflow.rs:907-918`; `coordination.rs:1424-1468` | No, but it is friction. |
| R8 | `review_mode=both` or `human` creates a `human_review` activity that only a human can claim (`human_reviewer_required`). | `workflow.rs:924-929, 1657` | Yes, by design. |
| R9 | A reviewer that holds an agent_review and whose lease expires can be recovered only after expiry (default `lease_seconds` up to 3600). The live project uses 3600, so a crash stalls the review for up to 1 hour. | `coordination.rs:273-276` (30-3600) | No, but it costs time. |

### 2b. Agent recovery of expired work (`recovery_mode=agent`)

**Normal tasks**

- A `claim` with `mode=recovery` (`coordination.rs:1470`) is refused for agents only when
  `recovery_mode=manual` (`:1534-1538`). It expires the prior attempt (`:1579-1581`).
- `recovery_resolution` (`:1797-1862`) requires `saved_work_checked && running_jobs_checked` and
  `ensure_attempt_quiescent`. It then flips the attempt to `mode='work'`.

**Workflow activities**

- `claim_activity` takes over expired attempts unless `recovery_mode=manual` (`workflow.rs:1700-1714`).

**Gates**

| # | Gate | Evidence | Human needed? |
|---|---|---|---|
| V1 | **Held reservations or nonterminal jobs** (`attempt_evidence_unresolved`) block recovery, release, submit, review and finalize. An `unknown` or `running` job can be closed only by its scoped reporter token (usually on the original workstation, `jobs reconnect`) or by a **human** `resolve_reservation`. | `jobs.rs:1536-1565`; `jobs.rs:717` ("A human operator must resolve uncertain physical resources"); live: 4 human `reservation.resolved` | Yes, when the original workstation is gone. |
| V2 | A recovery attempt cannot be released unblocked (`recovery_unresolved`). The server text tells agents "Release as blocked if inspection is incomplete", but `unblock` is `admin_or_operator` only, so a blocked task needs a human. | `coordination.rs:1745-1752, 1811, 1134`; live: 6 human `task.unblocked` | Yes. |
| V3 | Workflow activities cannot be released with `blocked=true` at all (`workflow.rs:1774-1778`). There is no agent path to say "this candidate is bad". | as cited | Yes, through reopen. |
| V4 | **An integration activity with a prior publication intent is a trap for the recovering agent.** `claim_activity` lets a new agent claim it and does not check for an existing intent. Then: `release` fails with `publication_result_required` (`:1797-1802`); `integration_result` fails with `publication_attempt_mismatch` because the intent is pinned to the old attempt (`:2425-2435`); and `agent_reconcile_publication` needs the old attempt's local journal (section 2c). The new owner holds a lease it can use for nothing and can only wait for it to expire. | as cited | Yes. |
| V5 | The recovery wait equals the full lease (up to 3600 s). There is no "owner crashed" fast path unless the session or credential is revoked (`owner_authorized`). | `coordination.rs:378-394` | No, but it is slow. |
| V6 | Clock incidents and restores need a host operator. | `operator_access.rs:1078-1120`; `coordination.rs:1348` | Yes. |
| V7 | `human_recovery_required` only when manual. Correct, but the claim path returns the generic `forbidden` (`coordination.rs:1535`, `workflow.rs:1709`) instead of a code. | as cited | No (a diagnosability issue). |

### 2c. Automatic integration authorization (`automatic_integration=true`) and the integration pipeline

**Happy path**

1. `claim_activity` (integration) requires `approvals_satisfied`, skips authorization when
   automatic (`workflow.rs:1664-1682`), and inserts a **global hold** on (canonical_repository_key,
   target_branch). The unique index gives `integration_target_held` (`:1720-1740`).
2. CLI `integrations prepare` (`cli/main.rs:~1500-1623`):
   - Local `prepare_integration` (`git_workflow.rs:488-570`) requires an isolated linked
     worktree at the exact remote target.
   - It builds a deterministic result (fast-forward, or `merge-tree --write-tree` +
     `commit-tree` with a fixed identity and date, `:884-958`).
   - It then POSTs `publication-intent` (`workflow.rs:2207`), which requires a registered checkout
     and the held hold.
3. Run every roster check as `jobs run --activity` on the exact result revision and tree.
   Reservations must exist on human-created resources.
4. `integrations publish` → `publish_prepared` (`git_workflow.rs:708-854`): observe, fresh
   authority callback, re-observe, journal `push_intent`, `push --force-with-lease`, observe again.
5. `integrations finish` → `integration-result` (`workflow.rs:2378`) with `validate_check_jobs`
   (`:2299-2376`), then `finalize` (`:2838`), which re-validates checks and a fresh observation,
   then marks done and releases the hold.

**Gates**

| # | Gate | Evidence | Human needed? |
|---|---|---|---|
| I1 | **The required-check roster is human-only.** `PUT workflow-policy` calls `human()` (`workflow.rs:325`). It must hold 1-100 entries (`:135-140`), and every code submit reads it (`:474-481` → `workflow_policy_required`, `:279`). There is no MCP write tool. Changing a check's `version` bumps the workflow revision, which makes every in-flight code candidate stale (I7). | as cited; live: 3 human roster updates | Yes. |
| I2 | **Resources are human-only** (`jobs.rs:267-270`). Jobs require a `reservation_id`, so every required check needs a human-created resource ("Ask the operator to define missing resource identities", `agent-startup.md:479`). | as cited | Yes, once per resource. |
| I3 | **Merge conflict.** `create_integration_result` bails with "candidate conflicts with the expected target" (`git_workflow.rs:907-909`). The agent can only `release`, which cancels the activity and creates an identical replacement (`workflow.rs:1830-1862`). No agent-callable transition moves the subject to `revision_needed`. Only `reopen` does, and it is human-only (`:1117`). | as cited; the orientation hint at `workflow.rs:998` says "a stale/conflicting immutable candidate requires operator reopen"; live submission text says "conflicted with current main during guarded integration and was reopened by a human" | Yes. |
| I4 | **A failing check on the integrated result** (the merge is clean but tests fail against the new main) is the same dead end as I3. Live text: "reopened after its integration Linux check exposed stale default-deny fixtures". | same | Yes. |
| I5 | **Target moved after the intent** (any push to `main` outside the coordinator, e.g. the user committing directly). `publish_prepared` returns `TargetMoved` (`git_workflow.rs:760-765, 823-828`). Release is refused (`workflow.rs:1797`). A `not_published` result sets `recovery_required` "requires human reconciliation" (`:2455-2470`). Agent reconciliation accepts only an exact base or result observation (`:2757-2768`), and the CLI refuses outright: "the target moved; retain the hold and use human reconciliation" (`cli/main.rs:1778-1785`). | as cited | Yes. |
| I6 | **Agent publication reconciliation needs everything below at once** (`workflow.rs:2615-2790`): `recovery_mode=agent`; unchanged policies; disposition `published`/`not_published` only; `local_journal_verified && publisher_stopped` attestations; an observation less than 120 s old (`:2697-2703`); intent attempt and generation match; owner not live; no registered/running/unknown jobs; no held reservations; the hold still held. The CLI implements it by loading the intent from `coordinator_home()/integrations/<sha(origin,project,activity,attempt)>.json` (`cli/main.rs:2602-2616`). That file exists only on the workstation that prepared. An ephemeral sandbox, a different host or an MCP-only agent can never satisfy it. | as cited; live: 9 human reconciliations, 0 agent ones | Yes, almost always. |
| I7 | **Policy staleness mid-integration.** A project or workflow policy bump after submission gives `workflow_authority_lost` on every owned operation (`workflow.rs:1320-1374`) and `workflow_policy_changed` on agent reconciliation (`:2670-2677`, "only a human can reconcile"). | as cited | Yes. |
| I8 | **The global hold is a singleton across projects.** It is held through all check runs and survives expiry. While held (including indefinitely during an uncertain publish), every other integration gets `integration_target_held`, and every **policy edit** (agent or human) gets `policy_hold_conflict` (`coordination.rs:306-320`; `workflow.rs:338-351`). | as cited | It amplifies I5 and I6. |
| I9 | `automatic_integration=false` needs `authorize_integration`, which is human-only (`workflow.rs:2150`). Agents cannot flip this flag (`coordination.rs:290-301`). | as cited | By design. |
| I10 | **Native CLI is required.** `agent_startup.local_operations.requires_native_cli` lists `submissions code`, `jobs run`, `integrations prepare/publish/reconcile/finish` (`discovery.rs:~40`). The MCP catalog has no job or reservation writes, so an MCP-only agent can never produce check receipts and can never finalize code. | as cited | Capability blocker. |

### 2d. Agents changing binding project rules (`agent_rule_editing`)

**Implementation** (`update_policy`, `coordination.rs:264-330`)

- Agents may PATCH when `agent_rule_editing` is already true. They may not change
  `agent_rule_editing`, `automatic_integration` or `allow_subagent_reviews` (`:290-301`).
- **They may change `review_mode`, `recovery_mode`, `lease_seconds` and `rules`.** That includes
  setting `review_mode:"none"`, which removes all review, or `recovery_mode:"manual"`, which locks
  agents out.
- The docs say agents change "rules only" (`book/src/docs/knowledge-contract.md:57`). The code is
  broader. That is a security gap and a docs gap at the same time.
- Every successful PATCH does `policy_revision+1` (`:323`).

**Why this capability undoes itself**

- Every open submission is pinned to `project_policy_revision`. After the bump:
  - `activity_preconditions` reports `operator_reopen_required` (`workflow.rs:846-857`).
  - `ensure_current` rejects all claims (`:1430-1440`).
  - `guard_activity_work` gives `workflow_authority_lost` on owned review and integration work
    (`:1349-1364`).
  - Task preconditions add `operator_reopen_required` (`coordination.rs:549-553`).
- In-progress implementation attempts fail `submit` with `policy_changed` (`workflow.rs:452-461`),
  so they must release and re-claim.
- All scoped decisions become pending again (`knowledge.rs:1287`).
- All sessions must re-acknowledge.
- **Only a human `reopen` recovers the submissions** (`workflow.rs:1117`). Live evidence: a
  submission "resubmitted … after human reopening solely because project policy advanced from
  revision 4 to 5 (lease duration)". The provenance of policy revision 6 is "Enhance the ability of
  agents to work autonomously".
- The edit is also refused whenever any integration hold is held (`policy_hold_conflict`).
- The agent guidance says "Do not change policy … to make a review eligible" (`discovery.rs:17`,
  `agent-startup.md:179`) and never explains when an agent should use rule editing.

---

## 3. Other autonomy blockers

1. **The durable-journal requirement for MCP.** "Before mutations, verify host/transport-managed
   durable journaling … If unavailable, report missing durable capability and stop before
   mutations" (`discovery.rs:19`). A direct HTTP MCP host, including Claude Code with a remote MCP
   URL, cannot attest to this ("the HTTP endpoint cannot attest to a direct host journal",
   `discovery.rs` `durable_mutations.scope`). So the recommended "configured_authenticated_mcp"
   first preference is unusable unless the stdio adapter is installed.
2. **Task-definition edits need human-created grants.** Even with a grant, an agent that
   contributed to a task cannot edit it: "A human must change a task definition after this agent
   has contributed to it" (`coordination.rs:1067, 1093`). Agents cannot fix a wrong acceptance
   criterion discovered mid-work. `lifecycle_change` (archive, cancel, delete) is human-only
   (`coordination.rs:655, 681`). Live: 45 human archives.
3. **The `deny` decision disposition permanently blocks** affected tasks until reopened
   (`knowledge.rs:1287`, `a.disposition!='allow'`), and `required_actor=human` decisions can be
   answered only by humans (`:1137`).
4. **Session binding.** Ownership requires the same session ID, credential and generation
   (`workflow.rs:1886-1898`; `coordination.rs:1205-1236`). A harness that loses its session state
   must wait for full lease expiry, then do an inspected recovery.
5. **Integration throughput.** One global hold per repository and branch, held for the full
   check-suite duration. Stalls cascade (I8).
6. **Continuation depends on the host.** The service "does not launch or wake agents"
   (`discovery.rs:3`), so every stall above ends the agent's turn. There is no service-side
   scheduler that re-prompts after a human unblocks something.
7. **Diagnosability.** All `forbidden` responses share `operation_not_permitted`
   (`error.rs:32-33`). Human-only branches are not machine-labelled (for example no
   `requires_actor:"human"` detail). The `precondition_hints` merge-conflict hint says
   "requires_local_observation" without saying that the remedy is human.

---

## 4. The agent instruction surface

### Sizes (words)

| Source | Words |
|---|---|
| `book/src/docs/agent-startup.md` (embedded as `data.agent_startup.guide` in `/api/v1/info`) | 4,912 |
| MCP server instructions (`mcp_instructions()`: MCP 287 + inspection 80 + review-selection 319 + attachments 63 + continuation 235 + worktree-cleanup 423) | 1,407 |
| MCP tool descriptions (62 tools) | 1,142 |
| Orientation payload (live) | 3,826 |
| – of which the `instructions` string (`coordination.rs:1348`) | 1,551 |
| – of which `completion_workflow.steps[7]` (cleanup, repeated) | 424 |
| `CLI.md` (referenced for subagents and commands) | 7,204 |
| Whole book | 64,744 across 40 pages |

An MCP agent reads about **10,000-11,000 words** before its first claim, plus 62 tool schemas.
`agent-startup.md` alone has 40 "must/never" imperatives in 591 lines.

### Duplication

- The worktree-cleanup procedure (423 words) appears three times: `discovery.rs:15`, orientation
  step 8, and `agent-startup.md` §"Remove completed task worktrees".
- Review-first selection appears in `discovery.rs:17`, the startup guide §Review, and the
  orientation.
- The continuation loop appears three times as well.

### Contradictions and ambiguities

1. **MCP versus the CLI.**
   - One set of statements says MCP needs no CLI: `"mcp": {"requires_native_cli": false}` and
     "Missing CLI is not a blocker for MCP listing, claiming…" (`discovery.rs:19`).
   - Another says code work does: `local_operations.requires_native_cli` includes `submissions code`
     and every integration step, and "For code submissions, use the native CLI" to claim reviews.
   - Agents conclude MCP is enough, claim code work, and then get stuck at submit or integration.
2. **"Claim automatically, never stop after listing" versus "stop before mutations" when there is
   no durable journal.** The first is `agent-startup.md:141-153`, the second `discovery.rs:19`.
3. **"Never change policy"** (`discovery.rs:17`) conflicts with a policy that delegates rule editing
   (`agent_rule_editing:true`), and no guidance says when rule editing is appropriate.
4. **The `blocked` flag.** "Set `blocked: true` only when a real blocker must be resolved"
   (`agent-startup.md:462`), yet the server says "Release as blocked if inspection is incomplete"
   (`coordination.rs:1811`). Neither says that unblocking needs a human.
5. **Where recovery ends.** "Follow the existing recovery or human reconciliation procedure where
   required" (`discovery.rs:17`) does not tell agents which cases are agent-recoverable.
   "If requirements changed, ask an operator to reopen" (orientation step 7) is the only guidance
   for merge conflicts and failed integration checks, and it does not mention them.
6. **Continue versus don't poll.** "Do not give the final response until no eligible work remains"
   (`discovery.rs:3`) conflicts with "Do not … start repeated polling" (`agent-startup.md:245`) when
   the only remaining work is waiting on another agent's review or integration.
7. **Stale "next_actions".** `workflow_snapshot` returns "Authorize if required, then claim the
   integration activity" even when the real blocker is a human reopen (`workflow.rs:1079-1084`).

### Tone

The prose is dense, legalistic and overloaded with fencing clauses. For example, the
branch-deletion guidance mandates `git push --force-with-lease=refs/heads/TASK_BRANCH:EXPECTED_OID`
with 12 conditions. This raises the chance that an agent misreads a gate as terminal and stops,
which is itself a source of human intervention.

---

## 5. Docs versus behavior

| Doc claim | Actual behavior |
|---|---|
| knowledge-contract.md:57: "an agent may change **rules** only when `agent_rule_editing` is already enabled" | An agent may also change `review_mode` (including to `none`), `recovery_mode` and `lease_seconds` (`coordination.rs:290-324`). |
| foundation-contract.md:115-118 (listed under "Implemented contracts"): "The review/integration execution workflows remain unavailable in this foundation" | They are implemented. The page is stale. |
| workflow-spec.md:190-196 (a "Proposed" page, but the only place policy-change consequences are described): "Existing attempts … can checkpoint or relinquish safely … The service records the authorized actor's resolution instead of silently grandfathering" | The implementation needs a **human** reopen for every in-flight submission. No agent-side "resolution" exists. |
| completion-contract.md:132-138 describes manual-mode human reopen | It does not state that reopen is human-only **in agent mode too**, or that it is the only route for merge conflicts and failing checks. |
| agent-startup.md:494-503: "MCP callers may use `coordinator_agent_publication_reconcile` only when they can provide the same fresh exact evidence" | MCP callers have no local intent journal, so in practice they never can. |
| mcp-guide / `requires_native_cli:false` | See 4.1. Code completion is impossible without the CLI. |
| implementation-status.md:107: "explicit human-managed required-check roster" | Accurate, but the agent guide never says that an empty or missing roster blocks all code submits (`workflow_policy_required`). |
| User's global rule: external links open in a new tab through `additional-js` in `book.toml` | `book.toml` has no `additional-js`. The rule is not implemented. |

The book mixes live contracts with preserved release design (7 pages marked "not current API") and
evidence logs (linux-capacity-evidence 5,112 words). Agents following links from the startup guide
can land on superseded design pages.

---

## 6. What to keep and what is accidental complexity

### Keep (sound, differentiating)

- The idempotent mutation receipts plus a durable client journal (the CLI and the MCP adapter).
- Immutable submissions pinned to revisions, and candidate checkpoint refs.
- The deterministic merge commit. It lets any workstation reproduce the same result revision, which
  is the basis for making recovery machine-independent.
- The CAS push with `--force-with-lease` and journaled `push_intent`, and the published,
  not_published and uncertain outcomes.
- Contributor-history independence plus opt-in subagent identities.
- `preconditions` and `state_wait` read endpoints.
- Exact check receipts (identity, version, environment, source, tree).

### Accidental or over-built complexity

1. **Pinning to the whole project policy revision.** It invalidates submissions for irrelevant
   edits (lease length, rule text). Pin only the fields that matter to a phase: review_mode for
   review, the roster for integration.
2. **`reopen` as the only backward transition,** and human-only. It serves stale policy, merge
   conflicts, failed checks and missing refs alike.
3. **The two-level journal attestation for agent reconciliation.** It requires a local file keyed by
   the old attempt, even though the Git remote plus the deterministic result revision already give
   the ground truth.
4. **Publication intent recorded at prepare time.** It is recorded before checks run, so the
   publication-uncertain window, and the hold, span the whole check run.
5. **Separate `workflow_policies` table and revision.** It doubles the stale-pin surface.
6. **Instructions repeated across three channels,** with about 10k words before the first claim.
7. **162+ error codes, but generic `forbidden`.**
8. **About 11 derived task statuses and about 30 enumerated states across 12 machines.**

---

## 7. Top 10 recommendations

1. **Add an agent-callable `request_revision` (withdraw) on integration and stale candidates.** When
   there is no publication intent (or it is reconciled `not_published`) and the caller is an
   eligible agent with `recovery_mode=agent`, move the subject to `revision_needed`. Use it for
   merge conflicts, failing integration checks, stale policy and missing refs. This alone removes
   most of the 26 human reopens.
2. **Stop invalidating submissions on unrelated policy edits.** Pin per-phase fields. Auto-carry a
   candidate forward when the relevant fields are unchanged (for example `lease_seconds` or `rules`
   text), or re-derive the required activities automatically under the new policy.
3. **Make agent publication reconciliation workstation-independent.** Use the server-side intent
   (observed base, tree, result, result tree) plus a fresh remote observation. Drop the local
   journal requirement when the observation equals base or result exactly. For a moved target
   whose history contains the result, treat it as published; otherwise treat it as not_published
   and then request revision. Let a new claimant of an intent-bearing activity reconcile directly
   instead of trapping it (V4).
4. **Record publication intent after checks pass, just before the push.** This shortens the
   uncertain window and the hold duration. Consider releasing the hold during check runs and
   re-validating the target at push time.
5. **Let agents manage the check roster and resources** under a delegation setting (like
   `agent_rule_editing`), or allow a roster declared in-repo (for example
   `.agent-coordinator.toml`) that is pinned by commit. Ship defaults so an empty roster is not a
   hard stop.
6. **Restrict agent policy edits to exactly `rules`** (or add per-field delegation). This fixes the
   `review_mode:none` gap and matches the docs.
7. **Give the MCP surface parity or tell the truth.** Either add job, reservation and reporter tools
   and a subagent-session registration path (a per-call session override for subagents), or set
   `requires_native_cli:true` for code tasks and say so in one sentence at the top of the guide.
8. **Machine-readable human gates.** Give every human-only rejection a distinct code (for example
   `human_reopen_required` or `human_reconciliation_required`) and a
   `details.required_actor:"human"`. Surface them in `preconditions` and `next_actions` so agents
   stop cleanly, route to other work, and humans get a queue.
9. **Faster crash recovery.** Decouple the lease from the renew cadence. Allow recovery after N
   missed heartbeats (for example 3 × `renew_after_seconds`) instead of full lease expiry, and let
   a parent session revoke a crashed child's session.
10. **Shrink the agent contract to one state-machine page** (under 1,500 words) plus the tool
    schemas. Deduplicate the cleanup, continuation and review text. Delete or move stale
    "foundation" statements. Split the book into Agent / Operator / Design-history sections.
    Implement the `book.toml` `additional-js` external-link hook.

Also worth doing: an optional service-side "human queue" view that lists every item blocked on a
human action (reopen, unblock, reconcile, resolve, roster, decision). That makes the remaining
intervention visible and measurable against the autonomy goal.
