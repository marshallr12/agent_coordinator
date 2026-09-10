# Repository and hook review

Reviewed locally on 2026-09-09 to refine the coordination-service plan.
Recommendations below are design proposals, not new authorization to change
either reference project or to execute its workflows.

## Evidence and limits

| Source | Snapshot and coverage |
| --- | --- |
| SithBit | `development` at `20368b6f`; 3,472 reachable commits. Read root agent guidance, current handoff/plan, relevant backlog and durable-record sections, repository and workstation memory, and selected archive incidents. Examined recent history and history focused on coordination, gates, hooks, and pruning. |
| Submission | `main` at `bbbdf8b`; 73 reachable commits. Read AGENTS.md, `context/RESUME.md`, project context, document-script implementation plan and handoff, relevant operational documentation, and migration/merge history. The checkout contains existing uncommitted work. |
| Harness configuration | Read the visible global and SithBit-local Claude hook registrations, referenced scripts, current/older memory locations, and relevant global configuration Git history. Inspected both repositories' Git hook directories and resolved `core.hooksPath` settings. |

This was a document, configuration, script-source, and local Git review. No
reference-project tests or hooks were executed, and no remote fetch was needed.
Historical test reports are evidence of what was reported at their recorded
revision; this review does not certify either project's current runtime behavior.
The 38,684-line SithBit archive was searched and sampled, not read end to end.

Submission has no root CLAUDE.md, HANDOFF.md, BACKLOG.md, DURABLE-RECORD.md,
or HANDOFF-archive.md in the inspected checkout. Its equivalent records use other
paths. No Submission-specific memory directory was found among the inspected
local Claude project directories. This does not establish that another machine
or harness has no such memory.

The records demonstrate local parallel work and cross-workstation knowledge
drift. They are not evidence that a distributed task-claim protocol already
exists; the proposed service still needs to supply that missing coordination.

## Findings that change the design

### E1. Real work already has phases, reviewers, and integration

SithBit's current plan has parallel groups of work, ordered phases, acceptance
criteria, required capabilities, shared files, and a dependency-driven exception:
the final health-listener tests wait for two other groups to finish because they
build the same crates. Its overnight task ledger distinguishes building,
verifying, gating, landed, parked, blocked, and done. The branch gate is followed
by a separate gate after integration. Submission likewise records phase
checkpoints and a separately authorized integration step.

Evidence: SithBit plan: `/home/marshall/src/solana/sithbit/HANDOFF.md:161`,
overnight ledger: `/home/marshall/src/solana/sithbit/.claude/overnight-tasks.md:1`,
Submission phases: `/home/marshall/src/ids/submission/EmarsModern/docs/document-scripts/IMPLEMENTATION-PLAN.md:94`.
SithBit commit `c2f593a3` introduced the worktree-aware overnight orchestration;
`970258f3` and `55539f0d` are recent integration commits.

**Revision:** support parent tasks, ordered dependencies, execution attempts,
review records, and integration evidence. An implementation can finish before
its parent objective is complete. Keep this a small typed model; a general
user-programmable workflow engine is not needed to represent these examples.

### E2. Finished work is repeatedly rediscovered as open work

SithBit explicitly preserves do-not-requeue records because copying a list of
spawned candidates instead of consumed candidates resurrected completed work.
An archived session found backlog item 26 already implemented three waves
earlier and corrected the record without rebuilding it. Current memory requires
checking Git history, symbols, and acceptance criteria before implementing a
backlog item. The same numeric labels also recur in separate session contexts.

Evidence: archive closure records: `/home/marshall/src/solana/sithbit/HANDOFF-archive.md:36069`,
item 26 correction: `/home/marshall/src/solana/sithbit/HANDOFF-archive.md:36774`,
selection feedback: `/home/marshall/.claude/projects/-home-marshall-src-solana-sithbit/memory/backlog-item-prove-undone.md`,
backlog warning: `/home/marshall/src/solana/sithbit/BACKLOG.md:2201`.

**Revision:** stable task identities, source-scoped import identities, durable
closure/duplicate/supersession links, and a recorded pre-implementation check.
Archive and export operations must never make completed work eligible again.
An agent finding an already-delivered result should reconcile the task with
evidence, not manufacture another implementation attempt's code changes.

### E3. Session prose can be valid historically and misleading now

Submission's AGENTS.md says repository-level Git history is unavailable, although
the current checkout has 73 reachable commits. Its migration handoff describes
work remaining on a feature branch. Later merge commits `d0d3cee` and `fb3ef17`
record integration, and ancestry checks confirm `3b2dbde` and `171d8d8` are
ancestors of the current HEAD. `context/RESUME.md` still displays July 20 as its
update date despite a later edit in `3ccb46b` and later implementation records.

Evidence: history statement: `/home/marshall/src/ids/submission/AGENTS.md:34`,
branch-specific handoff: `/home/marshall/src/ids/submission/EmarsModern/docs/document-scripts/BUILDING-GROUP-MIGRATION-HANDOFF.md:3`,
resume record: `/home/marshall/src/ids/submission/context/RESUME.md:3`.

**Revision:** source revision, observation time, applicable branch/environment,
and supersession status belong on imported claims. Current state is a projection
of explicit events, not whichever prose paragraph appears first. Historical
permission to work on a branch does not become authority for future merges.

### E4. Knowledge is broader than a durable-record file

SithBit uses SAVERS.md for practical discoveries, DURABLE-RECORD.md for standing
rules and decisions, HANDOFF.md for current context, and the archive for history.
Its root CLAUDE.md explicitly directs a fresh session to load those selectively.
Pruning first extracts reusable facts; relocating a narrative must not erase the
rule it taught. The current HANDOFF.md alone is 3,587 lines.

Evidence: handoff organization: `/home/marshall/src/solana/sithbit/HANDOFF.md:3`,
SAVERS.md: `/home/marshall/src/solana/sithbit/SAVERS.md:1`,
durable record: `/home/marshall/src/solana/sithbit/DURABLE-RECORD.md:1`.
Commit `28be6495` moved twelve completed records into the archive and added
extracted facts to SAVERS.md.

**Revision:** distinguish current checkpoints, practical lessons, decisions,
standing policy, rejected approaches, and historical narratives. Return a short
orientation packet and task-specific knowledge, with links to supporting history.
Import must accept SAVERS.md, alternative handoff paths, and memory directories.

### E5. Memory continuity currently depends on fragile local paths

The repository memory README documents a per-machine symlink strategy. On this
machine, the current SithBit workstation memory directory is a real directory
with a different index from the repository memory directory. The older
`-home-marshall-src-solana-rust/memory` symlink resolves to the nonexistent
`/home/marshall/src/solana/rust/.claude/memory`. Two targets named by the repository
MEMORY.md index are also absent from that directory. These observations show
separate or unresolved sources, not that their content is necessarily lost
everywhere.

Evidence: memory bootstrap: `/home/marshall/src/solana/sithbit/.claude/memory/README.md:7`,
repository index: `/home/marshall/src/solana/sithbit/.claude/memory/MEMORY.md`,
workstation index: `/home/marshall/.claude/projects/-home-marshall-src-solana-sithbit/memory/MEMORY.md`.

**Revision:** identify projects independently of absolute paths. Import multiple
memory sources with provenance and unresolved-link reporting; deduplicate without
silently discarding differing versions. Keep machine/harness-specific lessons
scoped, and use explicit sharing for general lessons across projects.

### E6. Agent liveness, job liveness, and test success are different facts

SithBit recorded three lost approximately 25-minute gate runs, killed waiters
whose gates survived, inherited file-lock handles, stale completion markers,
and incorrect results inferred from human log banners. Its detached runner now
uses structured run records, the producer's exit status, and process observations.
An uninterrupted test leg can legitimately produce no log output for about
400 seconds. The local memory's older launch recipe predates this runner.

Evidence: recovery guidance: `/home/marshall/src/solana/sithbit/CLAUDE.md:242`,
runner contract: `/home/marshall/src/solana/sithbit/scripts/gate-detached.sh:1`,
older memory: `/home/marshall/.claude/projects/-home-marshall-src-solana-sithbit/memory/gate-launch-detached.md`.
Commit `713b1ad7` added the detached runner and failure cases.

**Revision:** record external jobs independently of agent sessions. Preserve
job identity across observation failures and distinguish running, finished,
failed, interrupted, and unknown. Agent heartbeats, runner heartbeats, progress,
and lease validity must be separately visible. Never restart or reclaim merely
because a waiter disconnected or logs are quiet.

### E7. Shared resources extend beyond file names

Standing rule 16 documents interference through build dependencies despite
disjoint edits. The archive records a shared-tree stash temporarily reverting
another builder's twelve in-flight edits; that incident recovered without loss.
The overnight ledger records separate worktree builds exhausting disk space and
requiring serialized checks. Worktrees isolate editable files but do not
automatically isolate disk, fixture services, build outputs, or shared branches.

Evidence: dependency interference: `/home/marshall/src/solana/sithbit/DURABLE-RECORD.md:104`,
shared-tree incident: `/home/marshall/src/solana/sithbit/HANDOFF-archive.md:20946`,
disk exhaustion: `/home/marshall/src/solana/sithbit/.claude/overnight-tasks.md:42`,
resource policy: `/home/marshall/src/solana/sithbit/scripts/overnight.sh:90`.

**Revision:** record checkout identity, expected edits, and named resources with
scope: workstation, checkout, or shared project/environment. Prefer isolated
implementation worktrees and serialized integration. Resource admission must
not serialize independent computers just because both run a command named
"gate". A disconnected process may still occupy a resource after its task lease
expires; resource recovery needs its own evidence.

### E8. Eligibility depends on environment and decisions

SithBit's QRESYNC item is blocked on a published dependency release, with a local
fork explicitly rejected. Other work was skipped because emulators were down.
Preflight requirements cover capabilities such as working toolchains, available
disk, and access to an environment. Submission's live SQL checks depend on VPN
access; its mutation checks additionally depend on designated inputs and explicit
authorization. A credential existing on a workstation is insufficient evidence
that it is usable for the requested operation.

Evidence: upstream blocker: `/home/marshall/src/solana/sithbit/BACKLOG.md:385`,
preflight contract: `/home/marshall/src/solana/sithbit/CLAUDE.md:253`,
Submission live checks: `/home/marshall/src/ids/submission/EmarsModern/README.md:409`,
mutation scope: `/home/marshall/src/ids/submission/EmarsModern/README.md:672`.
Upstream release facts here describe the local record; no current external
release claim is made by this review.

**Revision:** typed blockers with reopening conditions; capability requirements
and expiring observations; decisions and authorization records bound to scope.
Claiming a task does not authorize database mutations, publishing, or deployment.
Preserve granted authority without repeatedly asking, and require a new decision
only when the actual action falls outside its recorded scope.

### E9. Repeated mistakes need correction and escalation, not more prose

SithBit records accepted designs and rejections with reopening conditions. It
also limits repeated disputes by defect class, because renaming a phase or
rescoping work previously reset the retry count and wasted further attempts.
Current handoff records distinguish a candidate new rule from an adopted rule.

Evidence: dispute rule: `/home/marshall/src/solana/sithbit/DURABLE-RECORD.md:248`,
rejections: `/home/marshall/src/solana/sithbit/DURABLE-RECORD.md:948`,
candidate rule: `/home/marshall/src/solana/sithbit/HANDOFF.md:150`.

**Revision:** stable finding IDs/classes, linked attempts, explicit decision
records, and configurable retry/escalation limits. Lessons can be observed,
validated, superseded, or rejected. Promotion into standing policy is a distinct
authorized action. Retrieval can show whether a lesson helped or was corrected;
this is shared operational knowledge, not a claim of model training.

## Hook inventory and implications

These are registrations observed in the inspected configuration, not proof of
coverage in every harness invocation. Both repositories' default Git hook
directories contain samples only; neither resolves a configured `core.hooksPath`.

| Observed hook | Behavior visible in source | Service implication |
| --- | --- | --- |
| Global SessionStart: `git-sync-on-start.sh` | Fetches and conditionally fast-forwards; reports dirty, diverged, or offline states; registered for startup/resume/clear/compact | Resume must reconnect to existing attempts before choosing work. Checkout synchronization cannot run blindly during active edits or a check against that checkout. |
| Global SessionStart: `decisions-pending-on-start.sh` | Surfaces the local parked-decision queue | Centralize pending decisions; preserve their answers and show each once per relevant session/revision. |
| Global PreToolUse: cargo pipeline check | Rejects selected output pipelines that mask exit codes | Report structured producer results; text matching is supplementary, not the evidence model. |
| Global PreToolUse: blanket-stage and tree-revert guards | Protect shared, uncommitted work with command-pattern checks | Keep these protections local; use checkout ownership/isolation and explicit changed-file manifests in the protocol. |
| Global PostToolUse: fmt check | Emits limited diagnostics and always returns success | Advisory hook success must never count as a passed verification gate. |
| Global Stop: orphan-waiter cleanup and gate-status warning | Examines session-owned waiters and reports a surviving gate | A turn ending is not proof a task or external job has ended. Cleanup requires verified process ownership. |
| SithBit PreToolUse: deploy-preflight guard | Requires a fresh local preflight stamp for recognized deployment commands | Represent preflight scope and freshness; it is readiness evidence, not user authorization or a global enforcement boundary. |
| SithBit PostToolUse: edited-Rust formatter | Formats one edited Rust path, never blocks | Keep frequent per-edit formatting local; avoid coordinator round trips for every edit. |
| Global `block-unstaged-destroyers.sh` file | Present, but not referenced by the inspected hook registrations | Distinguish installed files from registered/enabled behavior; don't advertise protection based on file presence. |

Sources: global registrations: `/home/marshall/.claude/settings.json`,
SithBit registrations: `/home/marshall/src/solana/sithbit/.claude/settings.json`,
global hook scripts: `/home/marshall/.claude/hooks/git-sync-on-start.sh`,
preflight guard: `/home/marshall/src/solana/sithbit/scripts/deploy-preflight-guard.sh`,
formatter: `/home/marshall/src/solana/sithbit/scripts/fmt-edited-rust.sh`.
Relevant global-config commits include `4dda2cc` (Git synchronization),
`62446a4` (structured gate-status check), and `ea017f4` (parked decisions).

One source-level inconsistency reinforces the need for contextual recovery
instructions: the synchronization hook recommends "Commit/stash" for a dirty,
behind checkout, while the registered tree-revert guard blocks mutating stash
commands. The service should name a recovery action compatible with the caller's
reported policy, or explain the unresolved policy conflict; it should not send an
agent into a retry loop against an action its harness refuses.

## Resulting scope recommendation

Keep Rust/Axum, SQLite, and the proposed lightweight web technology. The changes
are primarily to the domain model, client workflow, import, and operator views.

Recommend a central coordination API, a CLI usable by any agent with shell
access, and a browser interface. Existing harnesses continue launching work;
optional adapters translate their lifecycle events into the same API. No vendor
hook names, model choices, or shell snippets become mandatory server semantics.
The coordinator must not execute imported hooks or turn retrieved prose into
an executable command. Local execution remains governed by the harness/operator.

Recommend supporting both simple tasks and phased objectives in the data model,
including separate review/integration tasks when policy requires them. During
this review the operator **confirmed separate worktrees per implementation task,
with one integration step at a time into the target branch for the first release**.
The operator subsequently selected coordination of existing harnesses through
API, CLI, and optional hooks, with local runners reporting jobs. Remote agent
launch/supervision and a shared-directory execution mode are not required for
the first release.

After this review, the operator also confirmed public HTTPS access, service-
authoritative records with Markdown import/export, and access to every project
for every authenticated person and agent. Those decisions supersede the earlier
proposal for project-specific access grants. People will use local password
accounts and agents will use revocable API tokens. Agents create and claim tasks
autonomously, with required review configured per project. The selected server
installation is native Linux under systemd behind an HTTPS reverse proxy.
Expired work permits agent-driven recovery after inspecting saved work and jobs,
with a per-project manual alternative. Code tasks finish after required review,
target-branch integration, and validation of the integrated result.
