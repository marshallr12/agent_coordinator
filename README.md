# Agent Coordinator

A vendor-agnostic service for AI agents on different workstations to coordinate
tasks, report progress, and share handoffs and lessons across multiple projects.

**Status: planning and requirements discovery.** This repository contains the
design documents; the service and clients are not implemented yet.

## Design documents

- [Implementation plan](PLAN.md): requirements, confirmed decisions, open
  questions, proposed data model, milestones, and acceptance scenarios.
- [Coordination contract](docs/coordination-contract.md): task ownership,
  renewable leases, retries, worktrees, external jobs, and recovery.
- [Repository and hook review](docs/repository-review.md): evidence from existing
  agent workflows and the resulting design revisions. Local source links in
  this report refer to the workstation where the review was performed.

## Direction

- Rust with Axum and SQLite is the preferred service stack.
- The web interface should use vanilla JavaScript and modern CSS, with Alpine.js
  where useful.
- One service instance will support multiple projects simultaneously.
- Implementation tasks will use separate Git worktrees, with one integration
  step at a time into each target branch.
- Agents will authenticate, claim tasks atomically, renew ownership, and record
  outcomes and shared knowledge through a vendor-neutral interface.

The plan distinguishes confirmed requirements from proposed defaults. Hosting,
access policy, review requirements, and the boundary between coordination and
remote agent launching are still being defined.

## Repository hygiene

Build output, runtime databases, local credentials, logs, and agent worktrees
are ignored. Commit SQL migrations, sanitized configuration examples, shared
agent guidance, and Cargo.lock when the Rust application is added.
