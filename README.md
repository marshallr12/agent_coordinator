# Agent Coordinator

A vendor-agnostic service for AI agents on different workstations to coordinate
tasks, report progress, and share handoffs and lessons across multiple projects.

**Status: executable foundation implemented; full release still in development.**
The Rust service, embedded web dashboard, and native CLI now support authentication,
multiple projects, task admission, atomic ownership, renewable leases, checkpoints,
and inspected recovery. [Current implementation and limits](docs/implementation-status.md)
distinguish working behavior from the complete release design.

## Try the foundation locally

Install Rust 1.94 or newer with a native C build toolchain, then build the locked
workspace. SQLite is bundled; the dashboard has no separate build or CDN dependency.

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
cargo audit --file Cargo.lock
```

The smoke exercise uses disposable local accounts and data, and tests two actual
CLI processes against the server. Install `cargo-audit` separately to run the last
command. Linux and native Windows client checks are defined in GitHub Actions.

## Design documents

- [Implementation plan](PLAN.md): requirements, confirmed decisions, open
  installation inputs, data model, milestones, and acceptance scenarios.
- [Release scope](docs/release-scope.md): engineering defaults, deferred features,
  and implementation-readiness evidence.
- [Coordination contract](docs/coordination-contract.md): task ownership,
  renewable leases, retries, worktrees, external jobs, and recovery.
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
  Windows. MCP and a TUI are deferred.
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

The release target is 20 projects, 50 simultaneous agent sessions, and 100,000
historical tasks; those load targets are not yet verified. Planned backups run
hourly, retaining 24 hourly and 30 daily copies, with documented off-server copying
and a one-hour restore target. Backup/restore is not in the foundation.

The actual server is undecided. The test baseline is Ubuntu 24.04 LTS on x86_64
with 2 CPU cores and 4 GB RAM. Remaining host/domain/backup-destination choices
are installation inputs; no blocking product questions remain.

## Repository hygiene

Build output, runtime databases, local credentials, logs, and agent worktrees
are ignored. Commit SQL migrations, sanitized configuration examples, shared
agent guidance, and Cargo.lock. Never put tokens in a repository binding or URL.
