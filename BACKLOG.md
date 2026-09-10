# Implementation backlog

Execution instruction: complete the remaining items sequentially. The main agent
reviews each item's code and validation evidence before starting the next item.
Continue without operator review; ask only for information or access needed to
proceed. The numbered release backlog through item 7 is complete. Item 7 used an
actual native Windows workstation coordinated with a separate Linux workstation;
CI did not replace that physical exercise.

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

Completed item 6.1: authenticated stateless MCP with 56 typed tools sharing the
REST session, policy, ownership, and receipt checks. The native client launcher
securely shares its saved harness identity with a trusted foreground MCP client.
All 182 workspace tests, formatting, Clippy, build, and both smoke exercises pass.
Main-agent review is complete; see [MCP guidance](docs/mcp-guide.md) for compatibility
and local-operation limits. Linux/native Windows CI and the dependency audit passed
in [CI run 34468585302](https://github.com/marshallr12/agent_coordinator/actions/runs/34468585302).

Completed item 6.2: all guides and README content are consolidated into a 35-chapter
mdBook with pinned builds, source coverage and link checks, concise compatibility
entry points, and live includes of root agent/handoff/backlog/lesson records.
Main-agent review, the rendered link/fragment checks, browser navigation/search
and phone layout, package bounds/checksums/offline links, and a book build from
an extracted package passed, including local-file navigation/search with networking
disabled. After the repository became public, the pinned documentation workflow
passed on main in
[documentation run 34487173043](https://github.com/marshallr12/agent_coordinator/actions/runs/34487173043),
and the complete Linux/native-Windows coordination workflow and dependency audit
passed in
[coordination run 34487175859](https://github.com/marshallr12/agent_coordinator/actions/runs/34487175859).
See [book maintenance](docs/documentation.md).

Completed item 7: the accepted Windows x86-64 CLI ran on the native `MINIAIR`
workstation against a disposable service on Linux workstation `mxmini` over HTTPS.
The exercise covered public help and origin-bound authentication, separate
principals and sessions, two-project isolation, Windows claim/checkpoint/submission,
same-principal review rejection, independent Linux review, an exactly-one-owner
cross-workstation claim race, and Linux recovery of checkpointed work after the
dedicated Windows session was closed. Sanitized evidence is retained in
[completion commit `c017a69`](https://github.com/marshallr12/agent_coordinator/commit/c017a69152aaf52778bc3e48825446b700a18903).
The disposable public service and tunnel were shut down afterward. This completes
the numbered release backlog, but does not claim a production deployment,
permanent endpoint, hardware attestation, or off-server backup protection.

No unrestricted task-status edit, automatic force recovery, or unverified
completion shortcut should be added to make these milestones appear complete.
