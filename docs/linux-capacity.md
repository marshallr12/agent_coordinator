# Linux capacity exercise

The release target is 20 projects, 50 live agent sessions, and 100,000 historical
tasks. Run the disposable exercise against a freshly built executable:

```sh
cargo build --release --workspace --locked
sudo systemd-run --scope --quiet \
  -p MemoryMax=4G -p MemorySwapMax=0 -p CPUQuota=200% \
  runuser -u "$(id -un)" -- \
  /usr/bin/python3 scripts/load_acceptance.py \
    --server target/release/agent-coordinator-server \
    --duration 1800 --rate 50 --history 100000 \
    --require-baseline --report capacity-report.json
```

It creates a private temporary installation and synthetic canceled history while
the service is stopped. Fixture records have task definitions and audit events;
they do not claim that implementation or review occurred. After restarting, the
exercise creates 50 independent authenticated harness sessions across 20 projects
and claims one live task per session. A barrier starts 100 competing claims for
one additional task; exactly one request must win.

Measured traffic consists of 40 reads and 10 ownership renewals per second. Reads
cover task lists, task detail, project orientation, context search, and paginated
events. All 50 active attempts are renewed throughout the run. Requests follow a
fixed schedule; latency includes time waiting in the client queue. The exercise
fails instead of quietly reducing load when 100 requests are pending. It also
streams a 16 MiB artifact and creates a verified online backup during traffic.
Backup waits until the first upload chunk is in flight. The report records both
operation intervals and the runner requires them to overlap. In a full workload,
both operations must complete while metadata traffic is still measured and at
least one metadata request must complete during their overlap.

The report records per-operation and combined p95/p99, achieved request rate,
errors, peak resident memory, scheduling delay, claim conflicts, and final
ownership/foreign-key checks. Every metadata operation must have p95 below
500 milliseconds, with no unexpected error and at least 98% of the target rate.
The report records and checks the exact read/write operation counts and successful
renewal participation for every scheduled session. The final ownership check
compares the exact 50 expected project, task, attempt, generation, principal,
session, and credential tuples. It also requires current task pointers, unexpired
leases, and the finalized artifact's project, creator, size, and digest to match.

`--require-baseline` accepts only a clean source tree on Ubuntu 24.04 x86_64 with
at least two available CPUs and a unified cgroup v2 that limits the whole runner
scope to at most two CPUs and 4 GiB aggregate memory with swap disabled. The
scope contains the load generator, service, backup, restore stage, and their
charged page cache. The report records `cpu.max`, `memory.max`,
`memory.swap.max`, and aggregate `memory.current` and `memory.peak`. Service,
backup, and restore processes retain a 4 GiB address-space limit as a secondary
bound. The service is pinned to two available CPUs; backup and restore use the
same pair.

After measured traffic, the runner stops the live service and restores the
captured 100,000-task snapshot into a fresh temporary directory. This restore
stage must finish in less than one hour and pass SQLite integrity and foreign-key
checks. It verifies historical task count and the restored reconciliation pause,
including invalidated credentials, browser and agent sessions, human passwords,
and active attempts. Its reported time measures file restore plus staged authority
invalidation and validation. It is not a full service-recovery time: the human
inspection checklist, old-installation fencing, client reconnection, and service
resumption are outside this capacity runner. The small end-to-end backup smoke
exercise separately tests completion of that workflow.

Transport for this capacity workload is authenticated loopback HTTP. The separate
[Linux installation exercise](linux-installation.md) verifies systemd and HTTPS.
The report includes executable SHA-256, clean source revision, database schema
version, and GitHub repository, head SHA, ref, workflow, job, run ID, and attempt
when those CI values exist. Keep it with the exact build's other release evidence.
The executable digest and CI build job together identify the tested binary; the
runner never records credentials, request headers, raw errors, or captured server
output. The last 120 one-second diagnostic samples retain service CPU time,
aggregate CPU throttling counters, queue depth, completed counts, and background
operation progress, including on a failed run. These counters help diagnose
contention without recording request contents.

Short runs (`--duration 60`, for example) are development checks and have
`full_acceptance: false`; they may omit `--require-baseline`. The same exact
ownership, workload-accounting, overlap, restore-stage, and sanitization checks
still run, although a backup may finish after short metadata traffic ends. A
30-minute run without every verified baseline condition also has
`full_acceptance: false`. Neither substitutes for the constrained release run.
Local CPU contention from unrelated builds can affect the result and should be
avoided for final measurements. The native Windows workstation exercise remains
[backlog item 7](../BACKLOG.md), separate from all of these checks.

## Search index change

The volume exercise exposed a global full-text ranking query that scanned and
sorted matching tasks from every project before limiting the response. Migration
0015 rebuilds only the derived search index, indexes project identity, and limits
user search words to content fields. Search filters the project in the index and
uses a bounded FTS rank cursor before joining task details. SQLite documents
[rank-based early termination](https://www.sqlite.org/fts5.html#sorting_by_auxiliary_function_results)
for limited result sets. The existing task definitions and provenance stay intact.
