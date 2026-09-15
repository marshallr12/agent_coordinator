# Implementation handoff — 2026-09-14

## MCP-first bootstrap

Startup now prefers a configured authenticated MCP connection and uses the native
CLI as fallback. Public discovery schema 2 separates coordination from local
workstation capabilities and gives explicit missing-client and ownership-safe
transition procedures. MCP's own instructions no longer require a native launcher.
The native `session adopt-mcp` command validates and adopts an existing quiescent
MCP session using protected environment values; it never claims or renews work.
CLAUDE.md remains a minimal bootstrap. Deployment/test evidence belongs to the
live service task, not a local task queue.

## Service discovery migration

The public info endpoint now embeds the canonical `book/src/docs/agent-startup.md`
guide as `data.agent_startup` (schema version 1). It describes portable credential
configuration and native CLI workflows. Private task/project contents remain
behind authentication. CLAUDE.md is a seven-line discovery bootstrap; AGENTS.md
contains only its pointer. Engineering requirements moved to CONTRIBUTING.md.
BACKLOG.md and its book wrapper were removed; historical milestones are in the
implementation-history chapter and pending acceptance criteria remain in the live
service. Agents need no local backlog or handoff to select and claim work.

Validation and deployment evidence will be recorded with the live migration task.

## Setup usability follow-up

The two setup usability items and the Copy token investigation are now Code
tasks in the live Agent Coordinator project. Read their current state from the
service; this document is historical context, not the work queue.
The repository binding names project
`fe95a6c5-2aad-463f-8446-4366d9a281c7` at `https://agents.sithbit.com`.
The verified Windows CLI from release run 34891009689 is installed under
`%LOCALAPPDATA%/AgentCoordinator/bin`; `scripts/coordinator.ps1` supplies the
binding and requires an explicit, stable harness session name. The operator
saved the `codex-miniair` token in protected Windows configuration after manually
copying it. Native CLI authentication, complete orientation, reading all three
tasks, and reading the required-check roster passed on 2026-09-14 without using
admin browser authentication. No task was claimed during setup verification.
CLAUDE.md and AGENTS.md now direct future working sessions to the live service,
automatically selecting and claiming eligible work according to live priorities.
The Copy token investigation was created through the external browser as
`4b68f1c7-7ed2-42cb-b8cc-3ce0a1dfaefb`; its evidence distinguishes the user's
manual failure from possible automation clipboard isolation.

Initial operator setup exposed unclear project policy and required-check fields.
The live task queue tracks accessible explanatory tooltips, examples, and guidance
on matching producer registrations. A second item removes redundant repository
identity entry by deriving it from the saved URL while preserving existing
identities and shared integration holds. These are pending service task records;
the deployed UI is unchanged. Implement and verify the live acceptance criteria
before claiming tooltip support.

## Google Cloud deployment

The application from deployment commit `019ffb6` is
installed on the `agent-coordinator` e2-micro VM in `sithbit-19b44`,
`us-east1-b` (South Carolina). It has a 30-GB standard persistent boot disk,
deletion protection, 1 GiB swap, and reserved IPv6 `2600:1900:4020:671::`.
The temporary installation IPv4 address was removed. The earlier empty
central-region VM, disk, address, subnet, and backup bucket were removed.
No unrelated project resources were changed.

The service, Caddy, hourly local backups, daily maintenance, and hourly
download-verified Cloud Storage backup transfer are running. The origin is
live at `https://agents.sithbit.com`. Cloudflare proxies its AAAA record and
applies Full (strict) TLS through a hostname-specific configuration rule.
Browser Integrity Check is disabled for this API hostname after it blocked
Python clients with error 1010. The administrator is `admin`;
its initial password is in the operator's protected local deployment directory,
outside this repository, and was never printed. No repository is enrolled or
bound to this service yet.

Installed package identity from [release run 34891009689](https://github.com/marshallr12/agent_coordinator/actions/runs/34891009689):

- Archive SHA-256: `199c254e0c52b99eaf932b7a41a7c68c37f777688c5db33c3e921f75cdce6699`.
- Server SHA-256: `06f10eb72d4a0ac173ed795786048552b31e5c1aee294078afa37714b238e720`.
- CLI SHA-256: `4ae4d9e3e0e51c2035f4de8ac6676412b45f97064f19edd668645dd66f45b716`.

The Linux package build/systemd/HTTPS and native Windows release jobs passed,
as did local Linux package layout/checksum/link checks. Formatting, warnings-
denied Clippy, workspace tests/build, both smoke exercises, native Windows tests,
and dependency audit passed in [coordination run 34891009230](https://github.com/marshallr12/agent_coordinator/actions/runs/34891009230).
The initial deployment check found the newly published RUSTSEC-2026-0285 in
rustls 0.23.44; deployment commit `019ffb6` updates it to 0.23.45. An earlier
release Windows timing-test failure was not reproduced by the patched runs.
The server binary is byte-identical before and after the client TLS update.
That exact server passed the 30-minute release-size load/restore job in
[run 34888396240](https://github.com/marshallr12/agent_coordinator/actions/runs/34888396240).
The second run's redundant load job was cancelled after its Linux and Windows
package jobs passed; the second run is not claimed as an aggregate green run.
This CI workload is not an e2-micro capacity measurement.

Live checks confirmed the rendered browser sign-in page, public HTTPS from
Windows and the VM, administrator login/logout, Secure cookies, CSRF rejection,
unauthenticated project denial, synchronized time, restart health, and IPv6
access to package mirrors and Cloud Storage after public IPv4 removal.
A complete backup was uploaded to the private
`sithbit-19b44-agent-coordinator-backups-east` bucket, downloaded, and verified.
An independent download on the Ubuntu 24.04 WSL workstation matched SHA-256
`552b597b95fd0e7360760235f71073c4f3bf030e3c43fa5ab77b2da0a52e8650` and restored
snapshot `94f708d6-83f6-4f37-a1c3-ca2d8edbb46f` into an absent directory in
0.425 seconds, ending at `restore_reconciliation`. This is an initial,
artifact-free database rehearsal, not a production-size recovery benchmark.

See [Google Cloud installation](docs/deploy-gcp-e2-micro.md) for
resources, backup-transfer behavior, and cost limits. Free-tier eligibility
depends on total billing-account usage; no zero-cost guarantee is claimed.

## Previous implementation acceptance

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
