# Self-hosting review: cross-workstation work and record retirement

Reviewed on 2026-09-10 against main commit `f8d8a41`. The workspace test suite
passed locally on that commit (182 tests). This chapter answers three questions
for an agent or operator deciding how to adopt the service for this repository's
own development. It is an adoption assessment, not a new contract, and it grants
no authority.

The review assumes a reliable production instance built from the current code,
reachable at a well-known HTTPS URL, with a human administrator, off-server
backups, and a rehearsed restore. Where a claim depends on that assumption it is
labeled. Where a claim rests on retained evidence, the evidence is linked.

## Question 1: Can agents on different workstations complete tasks for one project?

**Yes.** This is demonstrated, not only designed. With a permanent instance the
only structural gap from the acceptance exercise, a disposable service behind a
temporary tunnel, is closed.

| Capability | Where it is established |
| --- | --- |
| Per-workstation origin-bound credentials and per-harness sessions with private proofs | [Foundation contract](foundation-contract.md), [CLI guide](CLI.md#repository-and-credential-configuration) |
| One repository binding file per checkout naming the service and project | [CLI guide](CLI.md#repository-and-credential-configuration) |
| Exactly-one ownership under the writer lock, monotonic generations, renewable leases | [Coordination contract](coordination-contract.md), [Durable record](../DURABLE-RECORD.md) |
| Isolated worktree per attempt; several projects may share one repository through serialized integration holds | [Job and worktree evidence](job-evidence-contract.md), [Completion workflow](completion-contract.md) |
| Independent review by a different principal; compare-and-swap Git publication; recovery of another workstation's expired work | [Completion workflow](completion-contract.md) |
| Physical Windows/Linux exercise: two-project isolation, claim race with one owner and one `claim_conflict`, cross-principal review, recovery after the owning session closed | [Implementation status](implementation-status.md#native-windows-workstation-acceptance-evidence) |

Operational duties remain with the workstations and the operator:

- Source never moves through the service. Every workstation needs push and fetch
  access to the shared Git remote, and candidate commits must be pushed before
  another workstation can review or integrate them.
- Hosts should keep synchronized time. A material clock rollback pauses new
  authority until reconciliation. See [clock safety](clock-safety-contract.md).
- Workstation facts are client attestations. The service does not inspect a
  remote filesystem and is not remote attestation.

## Question 2: Can the service retire HANDOFF.md and BACKLOG.md?

**BACKLOG.md: yes, immediately. HANDOFF.md: yes, after adopting one convention.
Neither file should be deleted until this repository's own instructions and
tooling stop reading it.**

### Record mapping

| What the root files carry today | Service record | Status |
| --- | --- | --- |
| Ordered work items with dependencies | Tasks with priorities and same-project prerequisites; objectives with membership frozen once work starts | Implemented |
| "Complete sequentially, review before the next item" | Project review and integration policy enforced by the service | Implemented |
| Per-item completion evidence and CI links | Immutable submissions, review decisions, check receipts, artifact links pinned to the task revision | Implemented |
| "Where I stopped, next step, blockers" | Checkpoints carry summary, current action, next step, and blockers; release and submission each carry a handoff | Implemented |
| Lessons, limitations, parked questions | Revisioned knowledge records (`lesson`, `fact`, `rejected_approach`, `checkpoint`) and scoped decisions | Implemented |
| Cold-start orientation | The orientation endpoint returns rules, candidate tasks, blockers, relevant knowledge, and next actions | Implemented |
| Migrating the existing files | Import preview and human-gated apply; checked items become closed tasks, unchecked items planned, prose historical mappings | Implemented |
| A readable file for humans and the book | Snapshot-consistent Markdown export with provenance | Implemented |

Details are in the [knowledge contract](knowledge-contract.md), the
[import contract](import-contract.md), and the [objective contract](objective-contract.md).

### The wave-level narrative convention

HANDOFF.md today holds one integrated story spanning several tasks. The service
stores state per task. The substitute is:

1. One knowledge record of kind `checkpoint` per wave, corrected in place so its
   revision history is the wave history.
2. One decision per parked product question, answered by a human in the dashboard.
3. Orientation as the cold-start entry point instead of reading a file.

Service-delivered agent instructions are a versioned constant compiled into the
server, not per-project editable text. Project-specific guidance belongs in the
project rules or in knowledge records.

### Changes required in this repository before deletion

1. `AGENTS.md` tells agents to read both files first and to update them after
   each item. Replace that with: connect, read orientation, record checkpoints,
   submit with a handoff and lessons.
2. The book includes both root files at build time and the documentation checks
   require those wrappers. Replace the includes with a committed export, or
   remove the chapters and the wrapper check together.
3. Harness skills that read `HANDOFF.md` (resume, handoff, prune, wave planning,
   session economics) must read the CLI export or orientation output instead.
4. `DURABLE-RECORD.md` can migrate to shared knowledge records the same way. It
   is outside the question and may stay.

### Residual points

- Agents cannot apply an import. Each migration is a human dashboard action.
- A generated export is deliberately not re-importable. Once the service is
  authoritative, committed Markdown is a mirror and never a place to edit.

## Question 3: Can this project use the service to bootstrap its own development?

**Yes, with one rule: the production instance must run an accepted release
package, never the working tree it coordinates.** The service's own change
process then flows through the service, and every release is exercised by the
project's real records before the next release is cut.

### Why it holds

- The service already supports the shape of this project's work: code tasks with
  a canonical repository key, a required-check roster, independent review, and
  compare-and-swap publication to `main`.
- Upgrade compatibility is exercised. The prior executable upgraded a schema-12
  database to schema 16 while preserving principals, sessions, an active attempt,
  and checkpoints, and old snapshots restored under the new executable. See
  [Linux acceptance evidence](linux-capacity-evidence.md).
- A schema or instruction-version change made by a task is not live until a
  release is installed. An instruction bump then requires every session to
  acknowledge the new version before its next claim, which is the intended
  rollout gate.

### Bootstrap sequence

1. Install the accepted release on the production host with the documented
   systemd, HTTPS, backup, and maintenance units. Verify a backup and rehearse a
   restore before the first task.
2. Create the project, set its canonical repository key to this repository's
   Git remote, and register at least one required check. The check is a locally
   launched job that runs the gate (`cargo fmt --check`, warnings-denied Clippy,
   workspace tests, `scripts/smoke.py`). Hosted CI is not a registered producer;
   it remains a second, independent gate on the pull request.
3. Commit `.agent-coordinator.toml` naming the service URL and project. Issue one
   credential per workstation; each harness chooses a stable session name.
4. Import `BACKLOG.md` and `HANDOFF.md` once through a preview, apply it as a
   human, and record the migration as a knowledge record.
5. Make the repository changes listed in question 2 in a task coordinated by the
   service. From that commit on, the root files are export mirrors or removed.
6. Continue development as ordinary service tasks. Cut releases through the
   existing release workflow. Upgrade the production host only from a package
   that passed release CI, after a fresh verified backup.

### Risks specific to self-hosting

| Risk | Mitigation already available |
| --- | --- |
| A release breaks the service that coordinates its own fix | Restore the previous snapshot into a fresh directory; the restore invalidates old authority and preserves holds. Keep the prior package on the host. |
| A migration corrupts live records | Snapshots from schema 12 onward verify unchanged and migrate only inside a private restore copy. Rehearse the upgrade against a copy first. |
| Work in flight during an upgrade | Instruction acknowledgment blocks new claims until sessions re-read; existing attempts and holds survive restart. Schedule upgrades at a wave boundary. |
| The only reviewer principal is also the author | Independent review rejects contributing principals and sessions. Enroll at least two agent credentials or require human review. |
| Records of the service's own bugs are lost with the service | Off-server backups are an operator input; export the project periodically and commit the export. |

### What this does not establish

No production instance exists at the time of this review, and this repository
is not bound to one. The sequence above is a plan for an operator to execute.
Nothing in this chapter claims a deployment, a URL, a credential, or a completed
migration.
