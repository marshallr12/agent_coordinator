# Implementation handoff — 2026-09-10

Backlog item 6.1 is implemented and reviewed on top of accepted Linux item 6.
The authenticated stateless `/mcp` endpoint exposes 56 closed, typed tools through
the same guarded REST handlers. Modern discovery and legacy initialization use
the pinned official Rust MCP SDK. Each call rechecks current credentials and
coordination authority; connections, ping, and receipt replay never renew a lease.

The native `mcp-client` launcher validates and shares the exact protected CLI
session with one trusted foreground client through its child environment. It
requires an absolute executable path and preserves native CLI continuity for Git,
local jobs, and binary transfers. It does not supervise background client trees.
No service-side execution or OAuth discovery is introduced. See the
[MCP connection guide](docs/mcp-guide.md) for configuration and limits.

Implementation candidate `cd9a633` passed all 182 workspace tests, formatting,
warnings-denied Clippy, workspace build, and both service/CLI smoke exercises.
Official SDK tests use real TCP for modern and legacy connections. Wire tests
cover competing claims, revocation after outer admission, stale generations,
exact shared REST receipts, policy/review restrictions, and clock pause. The live
launcher smoke records a checkpoint on the CLI-owned attempt and proves that
reconnect does not renew it. Native Windows CI and dependency audit are pending.

Main-agent review covered transport credential stripping, disabled SDK payload
logging, Host/Origin guards, fixed route/schema dispatch, current authorization,
atomic receipt reuse, protected environment injection, and the assertions behind
protocol compatibility. Raw REST results are capped before their duplicated MCP
structured/text representation; this is not a 1 MiB encoded-message guarantee.

Continue with item 6.2, mdBook documentation consolidation. Complete it without
operator intervention unless essential information is missing. Retain the
[Linux acceptance evidence](docs/linux-capacity-evidence.md), including exact
packaged identities and the measured limits. Existing schema-12 snapshots remain
compatible through private migration to schema 16; instruction version is 7.
The operator will commence actual Windows workstation acceptance as item 7;
native Windows CI and packages do not replace that exercise. No production
deployment or actual off-server transfer is claimed.

Keep a separate Cargo target directory in every concurrent worktree. See
[implementation status](docs/implementation-status.md) and [backlog](BACKLOG.md).
