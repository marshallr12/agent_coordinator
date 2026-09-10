# Agent Coordinator

A vendor-agnostic service for AI agents on different workstations to coordinate
tasks, report progress, and share handoffs and lessons across multiple projects.

**Status: coordination and Linux operations implemented and accepted; MCP and mdBook are next.**
The Rust service, embedded web dashboard, and native CLI now support authentication,
multiple projects, task admission, atomic ownership, renewable leases, checkpoints,
inspected recovery, worktree preparation, local job reporting, and shared resource
reservations, immutable submissions, independent review, serialized integration,
completion after exact integrated checks, revisioned lessons, scoped decisions,
bounded evidence uploads, authoritative Markdown imports/exports, operator account
management, project policy editing, objective grouping, complete task history,
verified online backups, restore reconciliation, clock rollback handling, and
bounded storage maintenance. [Current implementation and limits](docs/implementation-status.md)
distinguish working behavior from the complete release design.

## Try it locally

Install Rust 1.94 or newer with a native C build toolchain, then build the locked
workspace. Native integration uses Git 2.39 or newer (tested locally with 2.39.5).
SQLite is bundled; the dashboard has no separate build or CDN dependency.

```sh
cargo build --workspace --locked
target/debug/agent-coordinator-server \
  --public-origin http://127.0.0.1:8080 --allow-insecure-loopback \
  init-admin --username admin
target/debug/agent-coordinator-server \
  --public-origin http://127.0.0.1:8080 --allow-insecure-loopback serve
```

Enter a password at the hidden prompt, then open <http://127.0.0.1:8080>. Create a
project and issue an agent credential under **Access**. The token is displayed once.
Use [the CLI setup and command guide](docs/CLI.md) to connect each harness with its
own local session name. Connecting returns instructions and does not claim work.
For remote use, follow [the Linux/systemd/HTTPS examples](deploy/README.md).

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
python3 scripts/smoke.py
python3 scripts/backup_smoke.py
cargo audit --file Cargo.lock
```

The smoke exercise uses disposable local accounts and data, and tests two actual
CLI processes against the server. Install `cargo-audit` separately to run the last
command. Linux and native Windows client checks are defined in GitHub Actions.

## Design documents

- [Linux installation](docs/linux-installation.md): verified native packages,
  systemd, HTTPS, upgrades, and removal.
- [Linux capacity](docs/linux-capacity.md): the constrained 20-project, 50-session,
  100,000-task workload and measured restore stage.
- [Linux acceptance evidence](docs/linux-capacity-evidence.md): passing package,
  installation, workload, and recovery checks with the retained measured report.
- [Clock safety](docs/clock-safety-contract.md): durable time, authority expiry,
  and operator reconciliation.
- [Storage maintenance](docs/retention-contract.md): bounded compaction, permanent
  request identities, and daily maintenance.
- [Backup and restore guide](docs/backup-restore-guide.md): hourly snapshots,
  off-server copying, fresh-directory recovery, and the operator checklist.
- [Backup storage contract](docs/backup-contract.md): consistency, verification,
  retention, storage bounds, and publication safeguards.
- [Operator guide](docs/operator-guide.md): account recovery, credential rotation,
  policy editing, objectives, task history, and inspected recovery.
- [Operator access contract](docs/operator-access-contract.md): current account and
  session authorization and safe account-creation retries.
- [Objective contract](docs/objective-contract.md): child membership and completion gates.
- [Task history contract](docs/history-contract.md): bounded evidence pagination.

- [Implementation plan](PLAN.md): requirements, confirmed decisions, open
  installation inputs, data model, milestones, and acceptance scenarios.
- [Release scope](docs/release-scope.md): engineering defaults, deferred features,
  and implementation-readiness evidence.
- [Job/worktree contract](docs/job-evidence-contract.md): implemented job reporting,
  local launch/reconnect rules, scoped reporters, and physical resource holds.
- [Coordination contract](docs/coordination-contract.md): task ownership,
  renewable leases, retries, worktrees, external jobs, and recovery.
- [Completion contract](docs/completion-contract.md): implemented submissions,
  review activities, required checks, publication guards, and recovery.
- [Knowledge and decisions](docs/knowledge-contract.md): revisioned lessons,
  context search, binding-rule provenance, and scoped work blockers.
- [Artifact contract](docs/artifact-contract.md): bounded streaming uploads,
  retention, digest-verified native downloads, and evidence references.
- [Import/export contract](docs/import-contract.md): historical migration,
  immutable previews, conflict checks, and provenance-preserving snapshots.
- [Workflow specification](docs/workflow-spec.md): proposed states, selection,
  submissions, review/integration activities, and exact completion rules.
- [API and CLI contract](docs/api-contract.md): proposed authentication/session
  flow, request conventions, endpoints, errors, and agent-facing commands.
- [Implementation and acceptance](docs/implementation-spec.md): relational
  constraints, transaction boundaries, permissions, packaging, and release checks.
- [Onboarding contract](docs/onboarding-contract.md): operator setup, public
  authentication help, agent orientation, and the proposed repository snippet.
- [Repository and hook review](docs/repository-review.md): evidence from existing
  agent workflows and the resulting design revisions. Local source links in
  this report refer to the workstation where the review was performed.

## Direction

The complete release is being built toward the following agreed behavior.
Use the implementation-status document above to see which parts are available.

- Rust with Axum and SQLite is the preferred service stack.
- The web interface should use vanilla JavaScript and modern CSS, with Alpine.js
  where useful.
- The first release includes an HTTP API, web dashboard, and CLI with readable
  and JSON output. The CLI and local job reporter support Linux and native
  Windows. An MCP endpoint is scheduled as backlog item 6.1; a TUI is deferred.
- One service instance will support multiple projects simultaneously.
- Workstations connect over public HTTPS. Every authenticated person and agent
  has access to every project.
- The server runs as a native Linux service under systemd, behind an HTTPS
  reverse proxy.
- People sign in with local password accounts; agents use revocable API tokens.
- The service owns the current task, handoff, and lesson records, with Markdown
  import and export.
- Implementation tasks will use separate Git worktrees, with one integration
  step at a time into each target branch.
- Agents will authenticate, claim tasks atomically, renew ownership, and record
  outcomes and shared knowledge through a vendor-neutral interface.
- Agents can create and claim tasks; each project configures required review.
- Review can require an independent agent, a human, or both; independent agent
  review is the default.
- Projects can allow automatic integration or require human authorization. They
  can also delegate binding-rule changes to agents without human approval.
- Store bounded logs/reports in the service, source checkpoints in Git remotes,
  and links to other artifacts.
- Another agent can recover expired work after checking saved work and running
  jobs; projects may require manual recovery. Code tasks finish after required
  review, target-branch integration, and validation of the integrated result.
- Existing harnesses launch agents. The service coordinates their work through
  API, CLI, and optional hooks, with local runners reporting job status.

The packaged server passed the 20-project, 50-session, 100,000-historical-task
target at 50 requests/second for 30 minutes under a shared two-CPU/4-GiB/no-swap
limit. See the acceptance evidence above for measurements and scope. Backup examples run
hourly, retaining 24 hourly and 30 daily copies, with documented off-server copying
and a one-hour restore target. The disposable local restore rehearsal passed;
full operator-led recovery and an actual off-server destination remain installation checks.

The actual server is undecided. The test baseline is Ubuntu 24.04 LTS on x86_64
with 2 CPU cores and 4 GB RAM. Remaining host/domain/backup-destination choices
are installation inputs; no blocking product questions remain.

## Repository hygiene

Build output, runtime databases, local credentials, logs, and agent worktrees
are ignored. Commit SQL migrations, sanitized configuration examples, shared
agent guidance, and Cargo.lock. Never put tokens in a repository binding or URL.
