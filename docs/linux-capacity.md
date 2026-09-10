# Linux capacity exercise

The release target is 20 projects, 50 live agent sessions, and 100,000 historical
tasks. Run the disposable exercise against a freshly built executable:

```sh
cargo build --release --workspace --locked
python3 scripts/load_acceptance.py \
  --server target/release/agent-coordinator-server \
  --duration 1800 --rate 50 --history 100000 \
  --report capacity-report.json
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

The report records per-operation and combined p95/p99, achieved request rate,
errors, peak resident memory, scheduling delay, claim conflicts, and final
ownership/foreign-key checks. Every metadata operation must have p95 below
500 milliseconds, with no unexpected error and at least 98% of the target rate.
The final check requires exactly 50 current, unexpired ownership generations.

Both service and backup are restricted to the same two available CPU cores and
4 GiB of virtual address space **per process**. Resident memory is sampled
separately. This is not a 4 GiB aggregate host memory limit; the report names the
actual constraint and host. A production host must also budget memory for its
proxy, OS, and concurrent backups. The actual deployment hardware is undecided.

Transport for this capacity workload is authenticated loopback HTTP. The separate
[Linux installation exercise](linux-installation.md) verifies systemd and HTTPS.
The report includes executable SHA-256, source revision/dirty status, and database
schema version. Keep it with the exact build's other release evidence.

Short runs (`--duration 60`, for example) are development checks and have
`full_acceptance: false`. They do not substitute for the 30-minute release run.
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
