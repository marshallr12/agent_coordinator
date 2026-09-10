# Implementation handoff — 2026-09-10

Backlog item 5 is implemented and reviewed locally. Verified online SQLite/artifact
backups, self-contained retention, fresh-directory restore, and authority
invalidation are wired into the server. Restore pauses coordination until a human
records all captured hold inspections, fences the old installation, and reconciles
the post-snapshot gap. Holds and unknown work stay preserved. The Access dashboard
provides the checklist and replacement credentials for the same agent principal.

All 140 workspace tests, warnings-denied Clippy, formatting, workspace build,
JavaScript checks, the full existing smoke exercise, and the new restore exercise
passed. The final small restore fixture recovered in 6.9 seconds; it is not a
production-size benchmark or an actual off-server transfer. Browser verification
covered the checklist, multiline evidence, task links, credential replacement,
retained resource capacity, and a 375 CSS-pixel layout with no browser errors.
The disposable service and tab were stopped and closed.

Main-agent review corrected manifest/database artifact coverage, atomic
no-overwrite directory publication, standalone verification parent mutation,
repository overlap checks, backup schema read-only behavior, deadline coverage,
old receipt-key reuse, and invalidated integration authorization renewal.
Independent review found no remaining restore-authority blocker. History cursors
now use a restore epoch; old cursors must be restarted after restore or upgrade.
Instruction version 6 explains restore recovery and requires fresh acknowledgment.

See [implementation status](docs/implementation-status.md),
[backup and restore operations](docs/backup-restore-guide.md), and
[storage contract](docs/backup-contract.md). Engine limits are 8 GiB database,
1,000,000 artifact entries, 64 MiB manifest, 16 MiB per blob, and a cooperative
45-minute deadline. Full snapshots require their own storage; off-server transport
and production installation remain operator-selected deployment inputs.

Continue sequentially with item 6: Linux packaging/acceptance, clock rollback,
storage retention, and the 20-project / 50-session / 100,000-task load target.
Complete remaining items through 6.2 without operator intervention unless missing
information is essential. MCP is 6.1 and mdBook is 6.2. The operator will commence
native Windows workstation acceptance as final item 7; CI does not replace it.
Each concurrent worktree must retain its own Cargo target directory.

Item 4 passed Linux, native Windows, and audit CI at `1fcf9e4` in
[run 34441512277](https://github.com/marshallr12/agent_coordinator/actions/runs/34441512277).
Item 5 CI evidence will be recorded after the integrated commit is pushed.
