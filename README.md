# Agent Coordinator

A vendor-agnostic service for AI agents on different workstations to coordinate
tasks, report progress, and share handoffs and lessons across multiple projects.

**Status: planning and requirements discovery.** This repository contains the
design documents; the service and clients are not implemented yet.

## Design documents

- [Implementation plan](PLAN.md): requirements, confirmed decisions, open
  questions, proposed data model, milestones, and acceptance scenarios.
- [Release scope](docs/release-scope.md): remaining product choices, proposed
  engineering defaults, and the checklist for implementation readiness.
- [Coordination contract](docs/coordination-contract.md): task ownership,
  renewable leases, retries, worktrees, external jobs, and recovery.
- [Onboarding contract](docs/onboarding-contract.md): operator setup, public
  authentication help, agent orientation, and the proposed repository snippet.
- [Repository and hook review](docs/repository-review.md): evidence from existing
  agent workflows and the resulting design revisions. Local source links in
  this report refer to the workstation where the review was performed.

## Direction

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
- Another agent can recover expired work after checking saved work and running
  jobs; projects may require manual recovery. Code tasks finish after required
  review, target-branch integration, and validation of the integrated result.
- Existing harnesses launch agents. The service coordinates their work through
  API, CLI, and optional hooks, with local runners reporting job status.

The plan distinguishes confirmed requirements from proposed defaults. Server
configuration, integration permissions, shared-knowledge rules, artifact storage,
and operating targets are still being defined.

## Repository hygiene

Build output, runtime databases, local credentials, logs, and agent worktrees
are ignored. Commit SQL migrations, sanitized configuration examples, shared
agent guidance, and Cargo.lock when the Rust application is added.
