# Implementation handoff — 2026-09-10

Backlog item 6 is implemented and reviewed. Linux operational controls include
durable clock rollback detection and reconciliation, bounded storage maintenance,
verified native packages, and documented systemd/HTTPS installation and recovery.
Instruction version 7 requires fresh acknowledgment before new claims.

Candidate `a16d879` passed all 160 workspace tests, formatting, warnings-denied
Clippy, build, both service/CLI smoke exercises, native Windows client/local-runner
tests, and dependency audit in [standard CI](https://github.com/marshallr12/agent_coordinator/actions/runs/34459987850).
The [release checks](https://github.com/marshallr12/agent_coordinator/actions/runs/34459987847)
verified byte-identical Linux and Windows packages, trusted HTTPS, CLI reconnect
after service restart, the backup and maintenance timers, and the first verified backup.

The accepted packaged server completed 90,000 requests in 30 minutes at 50
requests/second across 20 projects and 50 sessions with 100,000 historical
tasks. Overall p95 was 21.0 ms and p99 was 21.7 ms; every operation met the
500 ms p95 target. There were no unexpected errors, and exact ownership checks
passed. The shared two-CPU/4-GiB/no-swap scope peaked at 664.1 MiB; server RSS
peaked at 84.6 MiB. The captured snapshot restored to a verified, authority-
invalidated reconciliation pause in 6.511 seconds.
See [retained acceptance evidence](docs/linux-capacity-evidence.md).

Main-agent review covered transaction timing, clock sample publication after
commit, permanent receipt identities, bounded maintenance scans, project-scoped
search, historical evidence, snapshot compatibility, native reproducibility,
installation cleanup, and the assertions supporting acceptance claims.
Existing schema-12 snapshots verify unchanged and restore through a privately
migrated schema-16 copy. Restore carries the snapshot's time forward before
invalidating old authority. The actual prior executable's live database upgraded
while retaining recorded work; old snapshot recovery rejected old credentials.

Clock recovery never revives expired task ownership or releases physical holds.
Maintenance retains semantic task/handoff/lesson history and permanent request
identities, so it bounds cleanup work without imposing a total history ceiling.
The capacity restore stage excludes human inspection, old-installation fencing,
reconnection, and resumption. A separate small end-to-end recovery exercise passed
in 7.3 seconds. No production deployment or actual off-server transfer is claimed.

Continue sequentially with item 6.1: a vendor-agnostic MCP endpoint reusing current
authentication, session, policy, ownership, and retry safeguards. Review its code
and validation before item 6.2, the mdBook documentation consolidation. Complete
both without operator intervention unless essential information is missing.
The operator will commence the real Windows workstation acceptance as item 7;
native Windows CI and reproducible packages do not replace that exercise.
Keep a separate Cargo target directory in every concurrent worktree.

See [implementation status](docs/implementation-status.md),
[Linux installation](docs/linux-installation.md), [capacity](docs/linux-capacity.md),
[clock safety](docs/clock-safety-contract.md), [retention](docs/retention-contract.md),
and [backup/restore operations](docs/backup-restore-guide.md).
