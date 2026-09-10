# Implementation backlog

Execution instruction: complete the remaining items sequentially. The main agent
reviews each item's code and validation evidence before starting the next item.
Continue without operator review; ask only for information or access needed to
proceed. Complete the remaining items through 6.2 autonomously.
The operator will start Windows workstation acceptance as final item 7 after Linux acceptance; CI does
not replace that workstation exercise.

The foundation, job/worktree evidence, and reviewed completion milestones are implemented. Use
[implementation status](docs/implementation-status.md) for precise limits.

Completed: native worktree preparation, stable producer identity, durable local
job reports, global named resource reservations, scoped health reporting,
observation-only reconnect, and recovery inspection. Uncertain producers retain
physical resource holds; resolving them records evidence without rewriting results.

Completed item 2: immutable submissions, exact-source/check receipts, independent
agent/human/both review, canonical repository/target integration holds, guarded
native Git publication, and integrated-result validation before done/dependency
release. Publication recovery preserves original evidence and transfers the hold
to a fresh integration activity for fresh checks.

Completed item 3: revisioned lessons, binding-rule provenance, scoped decisions,
bounded context, artifact links/uploads, and authoritative Markdown import/export.
The main agent reviewed the integrated behavior; all 114 tests, the built smoke
exercise, browser checks, Linux/Windows CI, and dependency audit passed. Audited
SithBit/Submission fixtures preserve historical closure and leave source
repositories unchanged.

Completed item 4: operator accounts and browser sessions, password recovery,
agent credential rotation, full policy editing, revisioned objective grouping,
complete task history pagination, inspected operator recovery controls, and named
native commands. Main-agent review and all 129 local tests, Clippy, build, smoke,
and browser checks passed. Linux/native-Windows CI and the dependency audit
passed; evidence is linked in the handoff.

Completed item 5: verified consistent SQLite/artifact snapshots, 24 hourly and
30 daily retention buckets, documented off-server copying, fresh-directory restore,
old-authority invalidation, and an explicit operator reconciliation checklist.
Main-agent review, all 140 workspace tests, Clippy, build, both smoke exercises,
and browser verification passed. The disposable restore took 6.9 seconds; this
does not establish production-size recovery or off-server protection.

Completed item 6: clock rollback handling, bounded retention, reproducible native
packages, and Linux systemd/HTTPS installation with both timers and verified backup.
All 160 workspace tests and Linux/Windows CI passed. The exact packaged server
completed 90,000 requests over 30 minutes at 50 requests/second with 20 projects,
50 sessions, and 100,000 historical tasks under a shared two-CPU/4-GiB/no-swap limit.
Overall p95 was 21.0 ms, with no unexpected errors and exact ownership preserved.
The snapshot restore stage passed in 6.511 seconds. Main-agent review is complete;
see [retained acceptance evidence](docs/linux-capacity-evidence.md) for scope and limits.

Next work, preserving the original backlog numbering:
- **6.1.** Add a vendor-agnostic MCP server endpoint to the service. Reuse the existing
   authentication, session, policy, ownership, and idempotency checks; expose
   actionable connection guidance and test protocol/client compatibility.
- **6.2.** Consolidate project documentation and README content into an mdBook project
   with a coherent navigation structure, maintained source links, build checks,
   and concise repository entry points that direct readers to the book.
- **7.** Final operator-initiated native Windows workstation acceptance, including
   coordination with the Linux service/workstation. The operator will commence
   this on a Windows machine after items 6 through 6.2; do not substitute CI for this test or
   claim it passed before the real workstation exercise.

No unrestricted task-status edit, automatic force recovery, or unverified
completion shortcut should be added to make these milestones appear complete.
