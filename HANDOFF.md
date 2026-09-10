# Implementation handoff — 2026-09-10

Backlog items 6.1 (MCP) and 6.2 (mdBook) are implemented and reviewed.
Only item 7 remains: the operator will commence native Windows workstation
acceptance against the Linux service/workstation. Do not substitute native
Windows CI or package checks for that physical workstation exercise.

## MCP completion

The authenticated stateless `/mcp` endpoint exposes 56 closed, typed tools through
the same guarded REST handlers. Connections, discovery, ping, and receipt replay
never renew ownership. The native `mcp-client` launcher securely shares one saved
harness session with a trusted foreground MCP client and its native CLI children.
Git, local producers, and binary transfer remain native operations. The launcher
requires an absolute executable path and does not supervise background clients.
No service-side execution or OAuth discovery is introduced.

Candidate `cd9a633` passed all 182 local workspace tests, formatting,
warnings-denied Clippy, build, and both smoke exercises. Linux workspace/smoke,
native Windows client/CLI/local-runner tests, and the dependency audit passed in
[CI run 34468585302](https://github.com/marshallr12/agent_coordinator/actions/runs/34468585302).
The integrated `677253a` also passed
[CI run 34468821269](https://github.com/marshallr12/agent_coordinator/actions/runs/34468821269).
Official SDK tests exercise modern and legacy protocols over real TCP; wire tests
cover revocation, races, policies, shared REST receipts, and generation/lease guards.
See the [MCP guide](docs/mcp-guide.md) for configuration and compatibility limits.

## Documentation completion

Canonical guides, contracts, the plan, and README content now live in `book/src`.
The original docs/README/PLAN entry paths remain short compatibility links.
Root AGENTS.md, HANDOFF.md, BACKLOG.md, and DURABLE-RECORD.md remain authoritative;
their book chapters include them at build time. Edit those root files directly.

The pinned mdBook 0.5.4 build covers all 35 chapters. Main review verified retained
acceptance evidence, proposal/current-status distinctions, source references,
rendered internal links and fragments, search, live includes, and desktop/390-pixel
phone layout without page overflow or JavaScript errors. Release packages retain
bounded Markdown sources and book configuration, without generated HTML.

Package checks passed for layout, checksums, permissions, offline links, explicit
member/size limits, and building the book from an extracted archive. The final
local structural package used stripped copies of the current local debug binaries;
it is documentation validation, not new release-binary or capacity acceptance.
Documentation CI is configured; its first run is pending.

Use `mdbook build` or `python3 scripts/check_docs.py` from a source checkout; output
is `target/book/index.html`. See [book maintenance](docs/documentation.md).
The retained [Linux acceptance evidence](docs/linux-capacity-evidence.md) remains
unchanged and identifies its exact accepted package and server. Schema version is
16 and instruction version is 7. No production deployment or actual off-server
transfer is claimed. Host/domain/backup-destination choices remain installation
inputs. Keep separate Cargo targets for concurrent worktrees.
