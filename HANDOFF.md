# Implementation handoff — 2026-09-10

Backlog items 6.1 (MCP), 6.2 (mdBook), and 7 (native Windows workstation
acceptance) are implemented, exercised, and reviewed. The numbered release
backlog and hosted-CI validation follow-up are complete. Item 7 used the actual
native Windows workstation and a separate Linux workstation; it did not
substitute native Windows CI or package inspection for the physical exercise.

## Native Windows workstation acceptance

The accepted `agent-coordinator 0.1.0` Windows x86-64 CLI from
[release run 34459987847](https://github.com/marshallr12/agent_coordinator/actions/runs/34459987847)
ran natively on workstation `MINIAIR` against a disposable service on Linux
workstation `mxmini` through a temporary Cloudflare HTTPS tunnel. The downloaded
Windows archive SHA-256 was
`be5e96ec6f8843a63c8be319d9b4f9745f4e0870bebbbc160c2c24f69dc32a1d`,
matching its adjacent checksum; the exercised executable SHA-256 was
`40240c59aeaf012aba6721f378e9dec57419ceb8833aec0187391c3bccb1111d`.

The exercise used distinct Windows and Linux principals and sessions across two
isolated projects. Windows claimed, checkpointed, and submitted project-two work;
a same-principal agent review was rejected, while the independent Linux principal
claimed and approved the exact submission. Project-one work remained isolated.
At a published barrier, both workstations attempted the same project-one task and
revision. Linux obtained generation 2 ownership and Windows received the expected
`claim_conflict`; Linux checkpointed the result and released the task ready. In a
separate recovery case, Windows checkpointed with a dedicated session, that session
was closed, and Linux inspected and released the retained work without reviving the
Windows session.

Sanitized cross-workstation evidence is retained in
[completion commit `c017a69`](https://github.com/marshallr12/agent_coordinator/commit/c017a69152aaf52778bc3e48825446b700a18903).
The encrypted one-time handoff branch was removed after Windows decrypted it. The
disposable public service and tunnel were shut down after acceptance. This proves
the scoped native two-workstation workflow; it is not a production deployment,
permanent public endpoint, hardware attestation, or off-server backup exercise.

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
Local-file navigation and search also passed with networking disabled.

After the repository became public, hosted validation was retried on main commit
`ebf72f7`. The pinned mdBook build and documentation checks passed in
[documentation run 34487173043](https://github.com/marshallr12/agent_coordinator/actions/runs/34487173043).
Linux formatting, warnings-denied Clippy, all workspace tests, the workspace build,
both service/CLI smoke exercises, native Windows client/CLI/local-runner tests,
and the locked dependency audit passed in
[coordination run 34487175859](https://github.com/marshallr12/agent_coordinator/actions/runs/34487175859).
These runs supersede the earlier failed-to-start billing-blocked attempts; no job
steps ran in those attempts, so they remain historical scheduling failures rather
than test results.

Use `mdbook build` or `python3 scripts/check_docs.py` from a source checkout; output
is `target/book/index.html`. See [book maintenance](docs/documentation.md).
The retained [Linux acceptance evidence](docs/linux-capacity-evidence.md) remains
unchanged and identifies its exact accepted package and server. Schema version is
16 and instruction version is 7. No production deployment or actual off-server
transfer is claimed. Host/domain/backup-destination choices remain installation
inputs. Keep separate Cargo targets for concurrent worktrees.
