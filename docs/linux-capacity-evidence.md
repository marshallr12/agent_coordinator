# Linux release acceptance evidence

Backlog item 6 passed main-agent review and all acceptance gates for candidate
`a16d879`. [Standard CI](https://github.com/marshallr12/agent_coordinator/actions/runs/34459987850)
passed all 160 workspace tests, formatting, warnings-denied Clippy, build, both
service/CLI smoke exercises, native Windows client/local-runner tests, and the
locked dependency audit. [Release CI](https://github.com/marshallr12/agent_coordinator/actions/runs/34459987847)
passed native Linux/Windows package reproducibility and Ubuntu 24.04 installation
with trusted HTTPS, restart/reconnect, both timers, and a verified first backup.

## Packaged-server capacity

The accepted packaged server completed 90,000 requests in 30 minutes at 50
requests/second across 20 projects and 50 sessions with 100,000 historical
tasks. Overall p95 was 21.0 ms and p99 was 21.7 ms; every operation met the
500 ms p95 target. There were no unexpected errors, and exact ownership checks
passed. The shared two-CPU/4-GiB/no-swap scope peaked at 664.1 MiB; server RSS
peaked at 84.6 MiB. The captured snapshot restored to a verified, authority-
invalidated reconciliation pause in 6.511 seconds.

The tested source was clean PR merge revision `ef10939b6aa3c5b755e650807e99f241307d2ea9`. The capacity
job downloaded the accepted Linux archive and verified its checksums before
testing. Its observed and expected server SHA-256 both were:

```text
0f1f8288987a40cb84ab5396cb30c0918b17336dcbf5d083170e0fc86058d9dc
```

| Operation | Requests | p95 (ms) | p99 (ms) |
| --- | ---: | ---: | ---: |
| task list | 14,400 | 4.796 | 6.948 |
| task detail | 14,400 | 2.742 | 3.833 |
| orientation | 14,400 | 2.551 | 3.493 |
| renew | 18,000 | 3.630 | 6.335 |
| context | 14,400 | 21.688 | 30.511 |
| history | 14,400 | 12.718 | 18.670 |

The mix was 72,000 reads and 18,000 renewals; each of the 50 sessions renewed
successfully 360 times. A burst of 100 claims produced exactly one winner and
99 expected conflicts. Queue depth peaked at 26; maximum
client scheduling delay was 10.057 ms and is included
in request latency.

The 16 MiB upload took 2.709 seconds.
The 171,532,860-byte online snapshot took
6.565 seconds. Their intervals overlapped
by 2.707 seconds while metadata traffic continued.

## Scope and limits

History consists of synthetic canceled tasks with definitions and audit events,
distributed across the projects while the service was stopped. The workload is
authenticated loopback HTTP; the separate installation gate verifies HTTPS.
The restore measurement covers private-directory restore, integrity/foreign-key
checks, the exact historical count, invalidated authority, and reconciliation
pause. It excludes human inspection, old-installation fencing, reconnection,
and service resumption. The concurrent snapshot may precede upload finalization;
live artifact ownership and digest are checked separately.

The small end-to-end recovery smoke separately passed in 7.3 seconds. An actual
prior schema-12 executable's database upgraded to schema 16 with work retained;
its snapshot verified unchanged and restored with old access rejected. No
production installation or actual off-server transfer is claimed. The real
Windows workstation exercise remains [backlog item 7](../BACKLOG.md).

## Retained machine-readable report

The complete sanitized report is preserved below. Its original file SHA-256 is
`378210fb920bf55549a3a9d179d7038e5b62a9cd543297ac301ef028e5899534`.

<details>
<summary>Full capacity report</summary>

```json
{
  "schema_version": 2,
  "passed": true,
  "full_acceptance": true,
  "full_acceptance_eligible": true,
  "baseline_required": true,
  "duration_requested_seconds": 1800,
  "requests_per_second": 50,
  "projects": 20,
  "agent_sessions": 50,
  "historical_tasks": 100000,
  "historical_fixture": "Synthetic canceled tasks, definition revisions, and audit events; seeded with service stopped.",
  "host": {
    "os": "Linux-6.17.0-1022-azure-x86_64-with-glibc2.39",
    "architecture": "x86_64",
    "service_cpu_cores": 2,
    "service_address_space_limit_bytes": 4294967296,
    "memory_limit_kind": "RLIMIT_AS per service/backup/restore process; cgroup v2 is required for aggregate baseline acceptance",
    "aggregate_cgroup": {
      "version": 2,
      "cpu_stat": {
        "usage_usec": 701457173,
        "user_usec": 501619525,
        "system_usec": 199837648,
        "nr_periods": 18553,
        "nr_throttled": 0,
        "throttled_usec": 0
      },
      "available": true,
      "memory_max_bytes": 4294967296,
      "memory_swap_max_bytes": 0,
      "memory_current_bytes": 303566848,
      "memory_peak_bytes": 696315904,
      "cpu_quota_micros": 200000,
      "cpu_period_micros": 100000
    }
  },
  "transport": "Authenticated loopback HTTP; HTTPS installation is exercised separately.",
  "source_commit": "ef10939b6aa3c5b755e650807e99f241307d2ea9",
  "source_dirty": false,
  "executable_sha256": "0f1f8288987a40cb84ab5396cb30c0918b17336dcbf5d083170e0fc86058d9dc",
  "expected_server_sha256": "0f1f8288987a40cb84ab5396cb30c0918b17336dcbf5d083170e0fc86058d9dc",
  "executable_matches_expected": true,
  "github": {
    "github_repository": "marshallr12/agent_coordinator",
    "github_run_id": "34459987847",
    "github_run_attempt": "1",
    "github_sha": "ef10939b6aa3c5b755e650807e99f241307d2ea9",
    "github_ref": "refs/pull/1/merge",
    "github_workflow": "Release package acceptance",
    "github_job": "linux-load"
  },
  "baseline": {
    "verified": true,
    "checks": {
      "ubuntu_24_04": true,
      "x86_64": true,
      "source_clean": true,
      "two_cpus_available": true,
      "cgroup_v2": true,
      "aggregate_memory_max_at_most_4_gib": true,
      "aggregate_swap_disabled": true,
      "aggregate_cpu_quota_at_most_two_cores": true,
      "aggregate_memory_peak_available": true,
      "github_head_matches_source": true
    },
    "os_id": "ubuntu",
    "os_version_id": "24.04",
    "architecture": "x86_64",
    "cgroup": {
      "version": 2,
      "cpu_stat": {
        "usage_usec": 104766,
        "user_usec": 75830,
        "system_usec": 28935,
        "nr_periods": 1,
        "nr_throttled": 0,
        "throttled_usec": 0
      },
      "available": true,
      "memory_max_bytes": 4294967296,
      "memory_swap_max_bytes": 0,
      "memory_current_bytes": 15167488,
      "memory_peak_bytes": 17379328,
      "cpu_quota_micros": 200000,
      "cpu_period_micros": 100000,
      "cpu_quota_cores": 2.0
    }
  },
  "full_workload": true,
  "database_schema_version": 16,
  "claim_burst": {
    "requests": 100,
    "sessions": 50,
    "winners": 1,
    "expected_conflicts": 99
  },
  "concurrent_operations": {
    "upload": {
      "passed": true,
      "bytes": 16777216,
      "seconds": 2.709,
      "started_offset_seconds": 30.0,
      "finished_offset_seconds": 32.709
    },
    "backup": {
      "passed": true,
      "seconds": 6.565,
      "database_bytes": 171532288,
      "snapshot_bytes": 171532860,
      "started_offset_seconds": 30.002,
      "finished_offset_seconds": 36.567
    },
    "overlap_seconds": 2.707
  },
  "total_including_background_seconds": 1799.989,
  "elapsed_seconds": 1800,
  "completed_requests": 90000,
  "achieved_requests_per_second": 50.0,
  "load_model": "open_loop_fixed_schedule",
  "latency_includes_client_schedule_delay": true,
  "metadata_p95_ms": 20.974,
  "metadata_p99_ms": 21.695,
  "operations": {
    "task_list": {
      "count": 14400,
      "p95_ms": 4.796,
      "p99_ms": 6.948
    },
    "task_detail": {
      "count": 14400,
      "p95_ms": 2.742,
      "p99_ms": 3.833
    },
    "orientation": {
      "count": 14400,
      "p95_ms": 2.551,
      "p99_ms": 3.493
    },
    "renew": {
      "count": 18000,
      "p95_ms": 3.63,
      "p99_ms": 6.335
    },
    "context": {
      "count": 14400,
      "p95_ms": 21.688,
      "p99_ms": 30.511
    },
    "history": {
      "count": 14400,
      "p95_ms": 12.718,
      "p99_ms": 18.67
    }
  },
  "workload_mix": {
    "reads": 72000,
    "writes": 18000,
    "scheduled_reads_per_second": 40.0,
    "scheduled_writes_per_second": 10.0,
    "sessions_with_successful_renewals": 50,
    "successful_renewals_per_participating_session_min": 360,
    "successful_renewals_per_participating_session_max": 360
  },
  "unexpected_errors": {},
  "maxima": {
    "rss_bytes": 88707072,
    "inflight": 26,
    "schedule_lag_ms": 10.056553000012514
  },
  "observed_projects": 20,
  "observed_agent_sessions": 50,
  "ownership_invariants_passed": true,
  "artifact_ownership_invariant_passed": true,
  "restore_stage": {
    "measurement": "restore_stage",
    "passed": true,
    "full_service_recovery": false,
    "seconds": 6.511,
    "target_seconds": 3600,
    "within_target": true,
    "integrity_ok": true,
    "foreign_keys_ok": true,
    "history_count_ok": true,
    "coordination_paused": true,
    "credentials_revoked": true,
    "browser_sessions_revoked": true,
    "agent_sessions_closed": true,
    "human_accounts_disabled": true,
    "no_active_attempts": true,
    "reporters_expired": true,
    "integration_authorizations_invalidated": true,
    "old_receipts_invalidated": true,
    "coordination_state": "restore_reconciliation",
    "database_bytes": 171540480,
    "artifact_count": 0,
    "artifact_bytes": 0,
    "historical_tasks": 100000,
    "credential_count": 1,
    "browser_session_count": 2,
    "agent_session_count": 50,
    "human_account_count": 1,
    "attempt_count": 51
  },
  "recent_diagnostics": [
    {
      "elapsed_seconds": 1680.0,
      "completed_requests": 84000,
      "pending_requests": 1,
      "service_rss_bytes": 88055808,
      "service_cpu_seconds": 552.5,
      "aggregate_cpu_stat": {
        "usage_usec": 651360639,
        "user_usec": 466324261,
        "system_usec": 185036377,
        "nr_periods": 17286,
        "nr_throttled": 0,
        "throttled_usec": 0
      },
      "upload_started": true,
      "upload_finished": true,
      "backup_started": true,
      "backup_finished": true
    },
    {
      "elapsed_seconds": 1681.0,
      "completed_requests": 84050,
      "pending_requests": 1,
      "service_rss_bytes": 88055808,
      "service_cpu_seconds": 552.83,
      "aggregate_cpu_stat": {
        "usage_usec": 651729030,
        "user_usec": 466596891,
        "system_usec": 185132139,
        "nr_periods": 17296,
        "nr_throttled": 0,
        "throttled_usec": 0
      },
      "upload_started": true,
      "upload_finished": true,
      "backup_started": true,
      "backup_finished": true
    },
    {
      "elapsed_seconds": 1682.0,
      "completed_requests": 84100,
      "pending_requests": 1,
      "service_rss_bytes": 88055808,
      "service_cpu_seconds": 553.16,
      "aggregate_cpu_stat": {
        "usage_usec": 652109612,
        "user_usec": 466855308,
        "system_usec": 185254303,
        "nr_periods": 17306,
        "nr_throttled": 0,
        "throttled_usec": 0
      },
      "upload_started": true,
      "upload_finished": true,
      "backup_started": true,
      "backup_finished": true
    },
    {
      "elapsed_seconds": 1683.0,
      "completed_requests": 84150,
      "pending_requests": 1,
      "service_rss_bytes": 88055808,
      "service_cpu_seconds": 553.48,
      "aggregate_cpu_stat": {
        "usage_usec": 652470947,
        "user_usec": 467111443,
        "system_usec": 185359504,
        "nr_periods": 17316,
        "nr_throttled": 0,
        "throttled_usec": 0
      },
      "upload_started": true,
      "upload_finished": true,
      "backup_started": true,
      "backup_finished": true
    },
    {
      "elapsed_seconds": 1684.0,
      "completed_requests": 84200,
      "pending_requests": 1,
      "service_rss_bytes": 88055808,
      "service_cpu_seconds": 553.81,
      "aggregate_cpu_stat": {
        "usage_usec": 652830380,
        "user_usec": 467364252,
        "system_usec": 185466128,
        "nr_periods": 17326,
        "nr_throttled": 0,
        "throttled_usec": 0
      },
      "upload_started": true,
      "upload_finished": true,
      "backup_started": true,
      "backup_finished": true
    },
    {
      "elapsed_seconds": 1685.0,
      "completed_requests": 84250,
      "pending_requests": 1,
      "service_rss_bytes": 88055808,
      "service_cpu_seconds": 554.12,
      "aggregate_cpu_stat": {
        "usage_usec": 653191479,
        "user_usec": 467633299,
        "system_usec": 185558180,
        "nr_periods": 17336,
        "nr_throttled": 0,
        "throttled_usec": 0
      },
      "upload_started": true,
      "upload_finished": true,
      "backup_started": true,
      "backup_finished": true
    },
    {
      "elapsed_seconds": 1686.0,
      "completed_requests": 84300,
      "pending_requests": 1,
      "service_rss_bytes": 88055808,
      "service_cpu_seconds": 554.45,
      "aggregate_cpu_stat": {
        "usage_usec": 653563891,
        "user_usec": 467898001,
        "system_usec": 185665890,
        "nr_periods": 17346,
        "nr_throttled": 0,
        "throttled_usec": 0
      },
      "upload_started": true,
      "upload_finished": true,
      "backup_started": true,
      "backup_finished": true
    },
    {
      "elapsed_seconds": 1687.0,
      "completed_requests": 84350,
      "pending_requests": 1,
      "service_rss_bytes": 88055808,
      "service_cpu_seconds": 554.77,
      "aggregate_cpu_stat": {
        "usage_usec": 653922223,
        "user_usec": 468150191,
        "system_usec": 185772032,
        "nr_periods": 17356,
        "nr_throttled": 0,
        "throttled_usec": 0
      },
      "upload_started": true,
      "upload_finished": true,
      "backup_started": true,
      "backup_finished": true
    },
    {
      "elapsed_seconds": 1688.0,
      "completed_requests": 84400,
      "pending_requests": 1,
      "service_rss_bytes": 88055808,
      "service_cpu_seconds": 555.1,
      "aggregate_cpu_stat": {
        "usage_usec": 654279761,
        "user_usec": 468422409,
        "system_usec": 185857351,
        "nr_periods": 17366,
        "nr_throttled": 0,
        "throttled_usec": 0
      },
      "upload_started": true,
      "upload_finished": true,
      "backup_started": true,
      "backup_finished": true
    },
    {
      "elapsed_seconds": 1689.0,
      "completed_requests": 84450,
      "pending_requests": 1,
      "service_rss_bytes": 88055808,
      "service_cpu_seconds": 555.41,
      "aggregate_cpu_stat": {
        "usage_usec": 654636342,
        "user_usec": 468683150,
        "system_usec": 185953191,
        "nr_periods": 17376,
        "nr_throttled": 0,
        "throttled_usec": 0
      },
      "upload_started": true,
      "upload_finished": true,
      "backup_started": true,
      "backup_finished": true
    },
    {
      "elapsed_seconds": 1690.0,
      "completed_requests": 84500,
      "pending_requests": 1,
      "service_rss_bytes": 88055808,
      "service_cpu_seconds": 555.73,
      "aggregate_cpu_stat": {
        "usage_usec": 654995686,
        "user_usec": 468929342,
        "system_usec": 186066344,
        "nr_periods": 17386,
        "nr_throttled": 0,
        "throttled_usec": 0
      },
      "upload_started": true,
      "upload_finished": true,
      "backup_started": true,
      "backup_finished": true
    },
    {
      "elapsed_seconds": 1691.0,
      "completed_requests": 84550,
      "pending_requests": 1,
      "service_rss_bytes": 88055808,
      "service_cpu_seconds": 556.06,
      "aggregate_cpu_stat": {
        "usage_usec": 655356126,
        "user_usec": 469186149,
        "system_usec": 186169977,
        "nr_periods": 17396,
        "nr_throttled": 0,
        "throttled_usec": 0
      },
      "upload_started": true,
      "upload_finished": true,
      "backup_started": true,
      "backup_finished": true
    },
    {
      "elapsed_seconds": 1692.0,
      "completed_requests": 84600,
      "pending_requests": 1,
      "service_rss_bytes": 88055808,
      "service_cpu_seconds": 556.37,
      "aggregate_cpu_stat": {
        "usage_usec": 655709493,
        "user_usec": 469438204,
        "system_usec": 186271288,
        "nr_periods": 17406,
        "nr_throttled": 0,
        "throttled_usec": 0
      },
      "upload_started": true,
      "upload_finished": true,
      "backup_started": true,
      "backup_finished": true
    },
    {
      "elapsed_seconds": 1693.0,
      "completed_requests": 84650,
      "pending_requests": 1,
      "service_rss_bytes": 88055808,
      "service_cpu_seconds": 556.7,
      "aggregate_cpu_stat": {
        "usage_usec": 656066931,
        "user_usec": 469693837,
        "system_usec": 186373093,
        "nr_periods": 17416,
        "nr_throttled": 0,
        "throttled_usec": 0
      },
      "upload_started": true,
      "upload_finished": true,
      "backup_started": true,
      "backup_finished": true
    },
    {
      "elapsed_seconds": 1694.0,
      "completed_requests": 84700,
      "pending_requests": 1,
      "service_rss_bytes": 88055808,
      "service_cpu_seconds": 557.0,
      "aggregate_cpu_stat": {
        "usage_usec": 656421666,
        "user_usec": 469941827,
        "system_usec": 186479839,
        "nr_periods": 17426,
        "nr_throttled": 0,
        "throttled_usec": 0
      },
      "upload_started": true,
      "upload_finished": true,
      "backup_started": true,
      "backup_finished": true
    },
    {
      "elapsed_seconds": 1695.0,
      "completed_requests": 84750,
      "pending_requests": 1,
      "service_rss_bytes": 88055808,
      "service_cpu_seconds": 557.33,
      "aggregate_cpu_stat": {
        "usage_usec": 656778791,
        "user_usec": 470206389,
        "system_usec": 186572401,
        "nr_periods": 17436,
        "nr_throttled": 0,
        "throttled_usec": 0
      },
      "upload_started": true,
      "upload_finished": true,
      "backup_started": true,
      "backup_finished": true
    },
    {
      "elapsed_seconds": 1696.0,
      "completed_requests": 84800,
      "pending_requests": 1,
      "service_rss_bytes": 88055808,
      "service_cpu_seconds": 557.65,
      "aggregate_cpu_stat": {
        "usage_usec": 657137216,
        "user_usec": 470465173,
        "system_usec": 186672042,
        "nr_periods": 17446,
        "nr_throttled": 0,
        "throttled_usec": 0
      },
      "upload_started": true,
      "upload_finished": true,
      "backup_started": true,
      "backup_finished": true
    },
    {
      "elapsed_seconds": 1697.0,
      "completed_requests": 84850,
      "pending_requests": 1,
      "service_rss_bytes": 88055808,
      "service_cpu_seconds": 557.96,
      "aggregate_cpu_stat": {
        "usage_usec": 657492574,
        "user_usec": 470714402,
        "system_usec": 186778171,
        "nr_periods": 17456,
        "nr_throttled": 0,
        "throttled_usec": 0
      },
      "upload_started": true,
      "upload_finished": true,
      "backup_started": true,
      "backup_finished": true
    },
    {
      "elapsed_seconds": 1698.0,
      "completed_requests": 84900,
      "pending_requests": 1,
      "service_rss_bytes": 88055808,
      "service_cpu_seconds": 558.29,
      "aggregate_cpu_stat": {
        "usage_usec": 657852746,
        "user_usec": 470956130,
        "system_usec": 186896615,
        "nr_periods": 17466,
        "nr_throttled": 0,
        "throttled_usec": 0
      },
      "upload_started": true,
      "upload_finished": true,
      "backup_started": true,
      "backup_finished": true
    },
    {
      "elapsed_seconds": 1699.0,
      "completed_requests": 84950,
      "pending_requests": 1,
      "service_rss_bytes": 88055808,
      "service_cpu_seconds": 558.61,
      "aggregate_cpu_stat": {
        "usage_usec": 658214775,
        "user_usec": 471220108,
        "system_usec": 186994667,
        "nr_periods": 17476,
        "nr_throttled": 0,
        "throttled_usec": 0
      },
      "upload_started": true,
      "upload_finished": true,
      "backup_started": true,
      "backup_finished": true
    },
    {
      "elapsed_seconds": 1700.0,
      "completed_requests": 85000,
      "pending_requests": 1,
      "service_rss_bytes": 88055808,
      "service_cpu_seconds": 558.93,
      "aggregate_cpu_stat": {
        "usage_usec": 658575940,
        "user_usec": 471480685,
        "system_usec": 187095255,
        "nr_periods": 17486,
        "nr_throttled": 0,
        "throttled_usec": 0
      },
      "upload_started": true,
      "upload_finished": true,
      "backup_started": true,
      "backup_finished": true
    },
    {
      "elapsed_seconds": 1701.0,
      "completed_requests": 85050,
      "pending_requests": 1,
      "service_rss_bytes": 88055808,
      "service_cpu_seconds": 559.25,
      "aggregate_cpu_stat": {
        "usage_usec": 658943093,
        "user_usec": 471744911,
        "system_usec": 187198182,
        "nr_periods": 17496,
        "nr_throttled": 0,
        "throttled_usec": 0
      },
      "upload_started": true,
      "upload_finished": true,
      "backup_started": true,
      "backup_finished": true
    },
    {
      "elapsed_seconds": 1702.0,
      "completed_requests": 85100,
      "pending_requests": 1,
      "service_rss_bytes": 88055808,
      "service_cpu_seconds": 559.58,
      "aggregate_cpu_stat": {
        "usage_usec": 659312515,
        "user_usec": 472022528,
        "system_usec": 187289986,
        "nr_periods": 17506,
        "nr_throttled": 0,
        "throttled_usec": 0
      },
      "upload_started": true,
      "upload_finished": true,
      "backup_started": true,
      "backup_finished": true
    },
    {
      "elapsed_seconds": 1703.0,
      "completed_requests": 85150,
      "pending_requests": 1,
      "service_rss_bytes": 88055808,
      "service_cpu_seconds": 559.9,
      "aggregate_cpu_stat": {
        "usage_usec": 659684904,
        "user_usec": 472290816,
        "system_usec": 187394088,
        "nr_periods": 17516,
        "nr_throttled": 0,
        "throttled_usec": 0
      },
      "upload_started": true,
      "upload_finished": true,
      "backup_started": true,
      "backup_finished": true
    },
    {
      "elapsed_seconds": 1704.0,
      "completed_requests": 85200,
      "pending_requests": 1,
      "service_rss_bytes": 88055808,
      "service_cpu_seconds": 560.22,
      "aggregate_cpu_stat": {
        "usage_usec": 660047744,
        "user_usec": 472559782,
        "system_usec": 187487962,
        "nr_periods": 17526,
        "nr_throttled": 0,
        "throttled_usec": 0
      },
      "upload_started": true,
      "upload_finished": true,
      "backup_started": true,
      "backup_finished": true
    },
    {
      "elapsed_seconds": 1705.0,
      "completed_requests": 85250,
      "pending_requests": 1,
      "service_rss_bytes": 88055808,
      "service_cpu_seconds": 560.54,
      "aggregate_cpu_stat": {
        "usage_usec": 660411048,
        "user_usec": 472833033,
        "system_usec": 187578015,
        "nr_periods": 17536,
        "nr_throttled": 0,
        "throttled_usec": 0
      },
      "upload_started": true,
      "upload_finished": true,
      "backup_started": true,
      "backup_finished": true
    },
    {
      "elapsed_seconds": 1706.0,
      "completed_requests": 85300,
      "pending_requests": 1,
      "service_rss_bytes": 88055808,
      "service_cpu_seconds": 560.86,
      "aggregate_cpu_stat": {
        "usage_usec": 660773129,
        "user_usec": 473083500,
        "system_usec": 187689629,
        "nr_periods": 17546,
        "nr_throttled": 0,
        "throttled_usec": 0
      },
      "upload_started": true,
      "upload_finished": true,
      "backup_started": true,
      "backup_finished": true
    },
    {
      "elapsed_seconds": 1707.0,
      "completed_requests": 85350,
      "pending_requests": 1,
      "service_rss_bytes": 88055808,
      "service_cpu_seconds": 561.19,
      "aggregate_cpu_stat": {
        "usage_usec": 661134525,
        "user_usec": 473350977,
        "system_usec": 187783547,
        "nr_periods": 17556,
        "nr_throttled": 0,
        "throttled_usec": 0
      },
      "upload_started": true,
      "upload_finished": true,
      "backup_started": true,
      "backup_finished": true
    },
    {
      "elapsed_seconds": 1708.0,
      "completed_requests": 85400,
      "pending_requests": 1,
      "service_rss_bytes": 88055808,
      "service_cpu_seconds": 561.51,
      "aggregate_cpu_stat": {
        "usage_usec": 661502198,
        "user_usec": 473627019,
        "system_usec": 187875178,
        "nr_periods": 17566,
        "nr_throttled": 0,
        "throttled_usec": 0
      },
      "upload_started": true,
      "upload_finished": true,
      "backup_started": true,
      "backup_finished": true
    },
    {
      "elapsed_seconds": 1709.0,
      "completed_requests": 85450,
      "pending_requests": 1,
      "service_rss_bytes": 88055808,
      "service_cpu_seconds": 561.85,
      "aggregate_cpu_stat": {
        "usage_usec": 661887438,
        "user_usec": 473902517,
        "system_usec": 187984921,
        "nr_periods": 17576,
        "nr_throttled": 0,
        "throttled_usec": 0
      },
      "upload_started": true,
      "upload_finished": true,
      "backup_started": true,
      "backup_finished": true
    },
    {
      "elapsed_seconds": 1710.0,
      "completed_requests": 85500,
      "pending_requests": 1,
      "service_rss_bytes": 88055808,
      "service_cpu_seconds": 562.22,
      "aggregate_cpu_stat": {
        "usage_usec": 662294712,
        "user_usec": 474184907,
        "system_usec": 188109804,
        "nr_periods": 17586,
        "nr_throttled": 0,
        "throttled_usec": 0
      },
      "upload_started": true,
      "upload_finished": true,
      "backup_started": true,
      "backup_finished": true
    },
    {
      "elapsed_seconds": 1711.0,
      "completed_requests": 85550,
      "pending_requests": 1,
      "service_rss_bytes": 88055808,
      "service_cpu_seconds": 562.54,
      "aggregate_cpu_stat": {
        "usage_usec": 662662208,
        "user_usec": 474457376,
        "system_usec": 188204832,
        "nr_periods": 17596,
        "nr_throttled": 0,
        "throttled_usec": 0
      },
      "upload_started": true,
      "upload_finished": true,
      "backup_started": true,
      "backup_finished": true
    },
    {
      "elapsed_seconds": 1712.0,
      "completed_requests": 85600,
      "pending_requests": 1,
      "service_rss_bytes": 88055808,
      "service_cpu_seconds": 562.85,
      "aggregate_cpu_stat": {
        "usage_usec": 663018410,
        "user_usec": 474726518,
        "system_usec": 188291891,
        "nr_periods": 17606,
        "nr_throttled": 0,
        "throttled_usec": 0
      },
      "upload_started": true,
      "upload_finished": true,
      "backup_started": true,
      "backup_finished": true
    },
    {
      "elapsed_seconds": 1713.0,
      "completed_requests": 85650,
      "pending_requests": 1,
      "service_rss_bytes": 88055808,
      "service_cpu_seconds": 563.17,
      "aggregate_cpu_stat": {
        "usage_usec": 663379594,
        "user_usec": 474990060,
        "system_usec": 188389534,
        "nr_periods": 17616,
        "nr_throttled": 0,
        "throttled_usec": 0
      },
      "upload_started": true,
      "upload_finished": true,
      "backup_started": true,
      "backup_finished": true
    },
    {
      "elapsed_seconds": 1714.0,
      "completed_requests": 85700,
      "pending_requests": 1,
      "service_rss_bytes": 88055808,
      "service_cpu_seconds": 563.5,
      "aggregate_cpu_stat": {
        "usage_usec": 663741147,
        "user_usec": 475255532,
        "system_usec": 188485615,
        "nr_periods": 17626,
        "nr_throttled": 0,
        "throttled_usec": 0
      },
      "upload_started": true,
      "upload_finished": true,
      "backup_started": true,
      "backup_finished": true
    },
    {
      "elapsed_seconds": 1715.0,
      "completed_requests": 85750,
      "pending_requests": 1,
      "service_rss_bytes": 88055808,
      "service_cpu_seconds": 563.82,
      "aggregate_cpu_stat": {
        "usage_usec": 664107134,
        "user_usec": 475533814,
        "system_usec": 188573320,
        "nr_periods": 17636,
        "nr_throttled": 0,
        "throttled_usec": 0
      },
      "upload_started": true,
      "upload_finished": true,
      "backup_started": true,
      "backup_finished": true
    },
    {
      "elapsed_seconds": 1716.0,
      "completed_requests": 85800,
      "pending_requests": 1,
      "service_rss_bytes": 88055808,
      "service_cpu_seconds": 564.14,
      "aggregate_cpu_stat": {
        "usage_usec": 664467345,
        "user_usec": 475788833,
        "system_usec": 188678512,
        "nr_periods": 17646,
        "nr_throttled": 0,
        "throttled_usec": 0
      },
      "upload_started": true,
      "upload_finished": true,
      "backup_started": true,
      "backup_finished": true
    },
    {
      "elapsed_seconds": 1717.0,
      "completed_requests": 85850,
      "pending_requests": 1,
      "service_rss_bytes": 88055808,
      "service_cpu_seconds": 564.46,
      "aggregate_cpu_stat": {
        "usage_usec": 664821328,
        "user_usec": 476052149,
        "system_usec": 188769179,
        "nr_periods": 17656,
        "nr_throttled": 0,
        "throttled_usec": 0
      },
      "upload_started": true,
      "upload_finished": true,
      "backup_started": true,
      "backup_finished": true
    },
    {
      "elapsed_seconds": 1718.0,
      "completed_requests": 85900,
      "pending_requests": 1,
      "service_rss_bytes": 88055808,
      "service_cpu_seconds": 564.77,
      "aggregate_cpu_stat": {
        "usage_usec": 665179986,
        "user_usec": 476323234,
        "system_usec": 188856751,
        "nr_periods": 17666,
        "nr_throttled": 0,
        "throttled_usec": 0
      },
      "upload_started": true,
      "upload_finished": true,
      "backup_started": true,
      "backup_finished": true
    },
    {
      "elapsed_seconds": 1719.0,
      "completed_requests": 85950,
      "pending_requests": 1,
      "service_rss_bytes": 88055808,
      "service_cpu_seconds": 565.1,
      "aggregate_cpu_stat": {
        "usage_usec": 665544834,
        "user_usec": 476579440,
        "system_usec": 188965393,
        "nr_periods": 17676,
        "nr_throttled": 0,
        "throttled_usec": 0
      },
      "upload_started": true,
      "upload_finished": true,
      "backup_started": true,
      "backup_finished": true
    },
    {
      "elapsed_seconds": 1720.0,
      "completed_requests": 86000,
      "pending_requests": 1,
      "service_rss_bytes": 88055808,
      "service_cpu_seconds": 565.42,
      "aggregate_cpu_stat": {
        "usage_usec": 665907204,
        "user_usec": 476837785,
        "system_usec": 189069419,
        "nr_periods": 17686,
        "nr_throttled": 0,
        "throttled_usec": 0
      },
      "upload_started": true,
      "upload_finished": true,
      "backup_started": true,
      "backup_finished": true
    },
    {
      "elapsed_seconds": 1721.0,
      "completed_requests": 86050,
      "pending_requests": 1,
      "service_rss_bytes": 88055808,
      "service_cpu_seconds": 565.75,
      "aggregate_cpu_stat": {
        "usage_usec": 666283620,
        "user_usec": 477105679,
        "system_usec": 189177941,
        "nr_periods": 17696,
        "nr_throttled": 0,
        "throttled_usec": 0
      },
      "upload_started": true,
      "upload_finished": true,
      "backup_started": true,
      "backup_finished": true
    },
    {
      "elapsed_seconds": 1722.0,
      "completed_requests": 86100,
      "pending_requests": 1,
      "service_rss_bytes": 88055808,
      "service_cpu_seconds": 566.07,
      "aggregate_cpu_stat": {
        "usage_usec": 666655546,
        "user_usec": 477381165,
        "system_usec": 189274381,
        "nr_periods": 17706,
        "nr_throttled": 0,
        "throttled_usec": 0
      },
      "upload_started": true,
      "upload_finished": true,
      "backup_started": true,
      "backup_finished": true
    },
    {
      "elapsed_seconds": 1723.0,
      "completed_requests": 86150,
      "pending_requests": 1,
      "service_rss_bytes": 88055808,
      "service_cpu_seconds": 566.41,
      "aggregate_cpu_stat": {
        "usage_usec": 667025574,
        "user_usec": 477645459,
        "system_usec": 189380114,
        "nr_periods": 17716,
        "nr_throttled": 0,
        "throttled_usec": 0
      },
      "upload_started": true,
      "upload_finished": true,
      "backup_started": true,
      "backup_finished": true
    },
    {
      "elapsed_seconds": 1724.0,
      "completed_requests": 86200,
      "pending_requests": 1,
      "service_rss_bytes": 88055808,
      "service_cpu_seconds": 566.72,
      "aggregate_cpu_stat": {
        "usage_usec": 667389784,
        "user_usec": 477904457,
        "system_usec": 189485326,
        "nr_periods": 17726,
        "nr_throttled": 0,
        "throttled_usec": 0
      },
      "upload_started": true,
      "upload_finished": true,
      "backup_started": true,
      "backup_finished": true
    },
    {
      "elapsed_seconds": 1725.0,
      "completed_requests": 86250,
      "pending_requests": 1,
      "service_rss_bytes": 88055808,
      "service_cpu_seconds": 567.05,
      "aggregate_cpu_stat": {
        "usage_usec": 667758403,
        "user_usec": 478182168,
        "system_usec": 189576235,
        "nr_periods": 17736,
        "nr_throttled": 0,
        "throttled_usec": 0
      },
      "upload_started": true,
      "upload_finished": true,
      "backup_started": true,
      "backup_finished": true
    },
    {
      "elapsed_seconds": 1726.0,
      "completed_requests": 86300,
      "pending_requests": 1,
      "service_rss_bytes": 88055808,
      "service_cpu_seconds": 567.38,
      "aggregate_cpu_stat": {
        "usage_usec": 668128636,
        "user_usec": 478451200,
        "system_usec": 189677435,
        "nr_periods": 17746,
        "nr_throttled": 0,
        "throttled_usec": 0
      },
      "upload_started": true,
      "upload_finished": true,
      "backup_started": true,
      "backup_finished": true
    },
    {
      "elapsed_seconds": 1727.0,
      "completed_requests": 86350,
      "pending_requests": 1,
      "service_rss_bytes": 88055808,
      "service_cpu_seconds": 567.72,
      "aggregate_cpu_stat": {
        "usage_usec": 668508279,
        "user_usec": 478729571,
        "system_usec": 189778708,
        "nr_periods": 17756,
        "nr_throttled": 0,
        "throttled_usec": 0
      },
      "upload_started": true,
      "upload_finished": true,
      "backup_started": true,
      "backup_finished": true
    },
    {
      "elapsed_seconds": 1728.0,
      "completed_requests": 86400,
      "pending_requests": 1,
      "service_rss_bytes": 88055808,
      "service_cpu_seconds": 568.05,
      "aggregate_cpu_stat": {
        "usage_usec": 668883246,
        "user_usec": 479018495,
        "system_usec": 189864751,
        "nr_periods": 17766,
        "nr_throttled": 0,
        "throttled_usec": 0
      },
      "upload_started": true,
      "upload_finished": true,
      "backup_started": true,
      "backup_finished": true
    },
    {
      "elapsed_seconds": 1729.0,
      "completed_requests": 86450,
      "pending_requests": 1,
      "service_rss_bytes": 88055808,
      "service_cpu_seconds": 568.37,
      "aggregate_cpu_stat": {
        "usage_usec": 669259133,
        "user_usec": 479296154,
        "system_usec": 189962978,
        "nr_periods": 17776,
        "nr_throttled": 0,
        "throttled_usec": 0
      },
      "upload_started": true,
      "upload_finished": true,
      "backup_started": true,
      "backup_finished": true
    },
    {
      "elapsed_seconds": 1730.0,
      "completed_requests": 86500,
      "pending_requests": 1,
      "service_rss_bytes": 88055808,
      "service_cpu_seconds": 568.7,
      "aggregate_cpu_stat": {
        "usage_usec": 669632419,
        "user_usec": 479561172,
        "system_usec": 190071246,
        "nr_periods": 17786,
        "nr_throttled": 0,
        "throttled_usec": 0
      },
      "upload_started": true,
      "upload_finished": true,
      "backup_started": true,
      "backup_finished": true
    },
    {
      "elapsed_seconds": 1731.0,
      "completed_requests": 86550,
      "pending_requests": 1,
      "service_rss_bytes": 88055808,
      "service_cpu_seconds": 569.05,
      "aggregate_cpu_stat": {
        "usage_usec": 670022128,
        "user_usec": 479843783,
        "system_usec": 190178344,
        "nr_periods": 17796,
        "nr_throttled": 0,
        "throttled_usec": 0
      },
      "upload_started": true,
      "upload_finished": true,
      "backup_started": true,
      "backup_finished": true
    },
    {
      "elapsed_seconds": 1732.0,
      "completed_requests": 86600,
      "pending_requests": 1,
      "service_rss_bytes": 88055808,
      "service_cpu_seconds": 569.36,
      "aggregate_cpu_stat": {
        "usage_usec": 670386306,
        "user_usec": 480105711,
        "system_usec": 190280594,
        "nr_periods": 17806,
        "nr_throttled": 0,
        "throttled_usec": 0
      },
      "upload_started": true,
      "upload_finished": true,
      "backup_started": true,
      "backup_finished": true
    },
    {
      "elapsed_seconds": 1733.0,
      "completed_requests": 86650,
      "pending_requests": 1,
      "service_rss_bytes": 88055808,
      "service_cpu_seconds": 569.74,
      "aggregate_cpu_stat": {
        "usage_usec": 670807741,
        "user_usec": 480388405,
        "system_usec": 190419335,
        "nr_periods": 17816,
        "nr_throttled": 0,
        "throttled_usec": 0
      },
      "upload_started": true,
      "upload_finished": true,
      "backup_started": true,
      "backup_finished": true
    },
    {
      "elapsed_seconds": 1734.0,
      "completed_requests": 86700,
      "pending_requests": 1,
      "service_rss_bytes": 88055808,
      "service_cpu_seconds": 570.08,
      "aggregate_cpu_stat": {
        "usage_usec": 671182872,
        "user_usec": 480671045,
        "system_usec": 190511826,
        "nr_periods": 17826,
        "nr_throttled": 0,
        "throttled_usec": 0
      },
      "upload_started": true,
      "upload_finished": true,
      "backup_started": true,
      "backup_finished": true
    },
    {
      "elapsed_seconds": 1735.0,
      "completed_requests": 86750,
      "pending_requests": 1,
      "service_rss_bytes": 88055808,
      "service_cpu_seconds": 570.41,
      "aggregate_cpu_stat": {
        "usage_usec": 671564107,
        "user_usec": 480950217,
        "system_usec": 190613890,
        "nr_periods": 17836,
        "nr_throttled": 0,
        "throttled_usec": 0
      },
      "upload_started": true,
      "upload_finished": true,
      "backup_started": true,
      "backup_finished": true
    },
    {
      "elapsed_seconds": 1736.0,
      "completed_requests": 86800,
      "pending_requests": 1,
      "service_rss_bytes": 88055808,
      "service_cpu_seconds": 570.75,
      "aggregate_cpu_stat": {
        "usage_usec": 671932381,
        "user_usec": 481227495,
        "system_usec": 190704885,
        "nr_periods": 17846,
        "nr_throttled": 0,
        "throttled_usec": 0
      },
      "upload_started": true,
      "upload_finished": true,
      "backup_started": true,
      "backup_finished": true
    },
    {
      "elapsed_seconds": 1737.0,
      "completed_requests": 86850,
      "pending_requests": 1,
      "service_rss_bytes": 88055808,
      "service_cpu_seconds": 571.07,
      "aggregate_cpu_stat": {
        "usage_usec": 672299617,
        "user_usec": 481484733,
        "system_usec": 190814884,
        "nr_periods": 17856,
        "nr_throttled": 0,
        "throttled_usec": 0
      },
      "upload_started": true,
      "upload_finished": true,
      "backup_started": true,
      "backup_finished": true
    },
    {
      "elapsed_seconds": 1738.0,
      "completed_requests": 86900,
      "pending_requests": 1,
      "service_rss_bytes": 88055808,
      "service_cpu_seconds": 571.39,
      "aggregate_cpu_stat": {
        "usage_usec": 672664644,
        "user_usec": 481744485,
        "system_usec": 190920158,
        "nr_periods": 17866,
        "nr_throttled": 0,
        "throttled_usec": 0
      },
      "upload_started": true,
      "upload_finished": true,
      "backup_started": true,
      "backup_finished": true
    },
    {
      "elapsed_seconds": 1739.0,
      "completed_requests": 86950,
      "pending_requests": 1,
      "service_rss_bytes": 88055808,
      "service_cpu_seconds": 571.73,
      "aggregate_cpu_stat": {
        "usage_usec": 673042583,
        "user_usec": 482018512,
        "system_usec": 191024071,
        "nr_periods": 17876,
        "nr_throttled": 0,
        "throttled_usec": 0
      },
      "upload_started": true,
      "upload_finished": true,
      "backup_started": true,
      "backup_finished": true
    },
    {
      "elapsed_seconds": 1740.0,
      "completed_requests": 87000,
      "pending_requests": 1,
      "service_rss_bytes": 88055808,
      "service_cpu_seconds": 572.05,
      "aggregate_cpu_stat": {
        "usage_usec": 673408619,
        "user_usec": 482285544,
        "system_usec": 191123075,
        "nr_periods": 17886,
        "nr_throttled": 0,
        "throttled_usec": 0
      },
      "upload_started": true,
      "upload_finished": true,
      "backup_started": true,
      "backup_finished": true
    },
    {
      "elapsed_seconds": 1741.0,
      "completed_requests": 87050,
      "pending_requests": 1,
      "service_rss_bytes": 88055808,
      "service_cpu_seconds": 572.37,
      "aggregate_cpu_stat": {
        "usage_usec": 673776637,
        "user_usec": 482562500,
        "system_usec": 191214136,
        "nr_periods": 17896,
        "nr_throttled": 0,
        "throttled_usec": 0
      },
      "upload_started": true,
      "upload_finished": true,
      "backup_started": true,
      "backup_finished": true
    },
    {
      "elapsed_seconds": 1742.0,
      "completed_requests": 87100,
      "pending_requests": 1,
      "service_rss_bytes": 88055808,
      "service_cpu_seconds": 572.7,
      "aggregate_cpu_stat": {
        "usage_usec": 674144743,
        "user_usec": 482844733,
        "system_usec": 191300010,
        "nr_periods": 17906,
        "nr_throttled": 0,
        "throttled_usec": 0
      },
      "upload_started": true,
      "upload_finished": true,
      "backup_started": true,
      "backup_finished": true
    },
    {
      "elapsed_seconds": 1743.0,
      "completed_requests": 87150,
      "pending_requests": 1,
      "service_rss_bytes": 88055808,
      "service_cpu_seconds": 573.03,
      "aggregate_cpu_stat": {
        "usage_usec": 674511303,
        "user_usec": 483114259,
        "system_usec": 191397043,
        "nr_periods": 17916,
        "nr_throttled": 0,
        "throttled_usec": 0
      },
      "upload_started": true,
      "upload_finished": true,
      "backup_started": true,
      "backup_finished": true
    },
    {
      "elapsed_seconds": 1744.0,
      "completed_requests": 87200,
      "pending_requests": 1,
      "service_rss_bytes": 88055808,
      "service_cpu_seconds": 573.35,
      "aggregate_cpu_stat": {
        "usage_usec": 674879847,
        "user_usec": 483396480,
        "system_usec": 191483366,
        "nr_periods": 17926,
        "nr_throttled": 0,
        "throttled_usec": 0
      },
      "upload_started": true,
      "upload_finished": true,
      "backup_started": true,
      "backup_finished": true
    },
    {
      "elapsed_seconds": 1745.0,
      "completed_requests": 87250,
      "pending_requests": 1,
      "service_rss_bytes": 88055808,
      "service_cpu_seconds": 573.67,
      "aggregate_cpu_stat": {
        "usage_usec": 675245390,
        "user_usec": 483677234,
        "system_usec": 191568155,
        "nr_periods": 17936,
        "nr_throttled": 0,
        "throttled_usec": 0
      },
      "upload_started": true,
      "upload_finished": true,
      "backup_started": true,
      "backup_finished": true
    },
    {
      "elapsed_seconds": 1746.0,
      "completed_requests": 87300,
      "pending_requests": 1,
      "service_rss_bytes": 88055808,
      "service_cpu_seconds": 574.0,
      "aggregate_cpu_stat": {
        "usage_usec": 675609991,
        "user_usec": 483954189,
        "system_usec": 191655801,
        "nr_periods": 17946,
        "nr_throttled": 0,
        "throttled_usec": 0
      },
      "upload_started": true,
      "upload_finished": true,
      "backup_started": true,
      "backup_finished": true
    },
    {
      "elapsed_seconds": 1747.0,
      "completed_requests": 87350,
      "pending_requests": 1,
      "service_rss_bytes": 88055808,
      "service_cpu_seconds": 574.31,
      "aggregate_cpu_stat": {
        "usage_usec": 675971878,
        "user_usec": 484233624,
        "system_usec": 191738254,
        "nr_periods": 17956,
        "nr_throttled": 0,
        "throttled_usec": 0
      },
      "upload_started": true,
      "upload_finished": true,
      "backup_started": true,
      "backup_finished": true
    },
    {
      "elapsed_seconds": 1748.0,
      "completed_requests": 87400,
      "pending_requests": 1,
      "service_rss_bytes": 88055808,
      "service_cpu_seconds": 574.64,
      "aggregate_cpu_stat": {
        "usage_usec": 676331428,
        "user_usec": 484498608,
        "system_usec": 191832819,
        "nr_periods": 17966,
        "nr_throttled": 0,
        "throttled_usec": 0
      },
      "upload_started": true,
      "upload_finished": true,
      "backup_started": true,
      "backup_finished": true
    },
    {
      "elapsed_seconds": 1749.0,
      "completed_requests": 87450,
      "pending_requests": 1,
      "service_rss_bytes": 88055808,
      "service_cpu_seconds": 574.96,
      "aggregate_cpu_stat": {
        "usage_usec": 676696417,
        "user_usec": 484757684,
        "system_usec": 191938732,
        "nr_periods": 17976,
        "nr_throttled": 0,
        "throttled_usec": 0
      },
      "upload_started": true,
      "upload_finished": true,
      "backup_started": true,
      "backup_finished": true
    },
    {
      "elapsed_seconds": 1750.0,
      "completed_requests": 87500,
      "pending_requests": 1,
      "service_rss_bytes": 88055808,
      "service_cpu_seconds": 575.3,
      "aggregate_cpu_stat": {
        "usage_usec": 677082962,
        "user_usec": 485036406,
        "system_usec": 192046556,
        "nr_periods": 17986,
        "nr_throttled": 0,
        "throttled_usec": 0
      },
      "upload_started": true,
      "upload_finished": true,
      "backup_started": true,
      "backup_finished": true
    },
    {
      "elapsed_seconds": 1751.0,
      "completed_requests": 87550,
      "pending_requests": 1,
      "service_rss_bytes": 88055808,
      "service_cpu_seconds": 575.62,
      "aggregate_cpu_stat": {
        "usage_usec": 677444615,
        "user_usec": 485287387,
        "system_usec": 192157228,
        "nr_periods": 17996,
        "nr_throttled": 0,
        "throttled_usec": 0
      },
      "upload_started": true,
      "upload_finished": true,
      "backup_started": true,
      "backup_finished": true
    },
    {
      "elapsed_seconds": 1752.0,
      "completed_requests": 87600,
      "pending_requests": 1,
      "service_rss_bytes": 88055808,
      "service_cpu_seconds": 575.95,
      "aggregate_cpu_stat": {
        "usage_usec": 677806211,
        "user_usec": 485550915,
        "system_usec": 192255295,
        "nr_periods": 18006,
        "nr_throttled": 0,
        "throttled_usec": 0
      },
      "upload_started": true,
      "upload_finished": true,
      "backup_started": true,
      "backup_finished": true
    },
    {
      "elapsed_seconds": 1753.0,
      "completed_requests": 87650,
      "pending_requests": 1,
      "service_rss_bytes": 88055808,
      "service_cpu_seconds": 576.26,
      "aggregate_cpu_stat": {
        "usage_usec": 678165497,
        "user_usec": 485812294,
        "system_usec": 192353203,
        "nr_periods": 18016,
        "nr_throttled": 0,
        "throttled_usec": 0
      },
      "upload_started": true,
      "upload_finished": true,
      "backup_started": true,
      "backup_finished": true
    },
    {
      "elapsed_seconds": 1754.0,
      "completed_requests": 87700,
      "pending_requests": 1,
      "service_rss_bytes": 88055808,
      "service_cpu_seconds": 576.59,
      "aggregate_cpu_stat": {
        "usage_usec": 678528656,
        "user_usec": 486074005,
        "system_usec": 192454651,
        "nr_periods": 18026,
        "nr_throttled": 0,
        "throttled_usec": 0
      },
      "upload_started": true,
      "upload_finished": true,
      "backup_started": true,
      "backup_finished": true
    },
    {
      "elapsed_seconds": 1755.0,
      "completed_requests": 87750,
      "pending_requests": 1,
      "service_rss_bytes": 88055808,
      "service_cpu_seconds": 576.91,
      "aggregate_cpu_stat": {
        "usage_usec": 678900275,
        "user_usec": 486352538,
        "system_usec": 192547737,
        "nr_periods": 18036,
        "nr_throttled": 0,
        "throttled_usec": 0
      },
      "upload_started": true,
      "upload_finished": true,
      "backup_started": true,
      "backup_finished": true
    },
    {
      "elapsed_seconds": 1756.0,
      "completed_requests": 87800,
      "pending_requests": 1,
      "service_rss_bytes": 88055808,
      "service_cpu_seconds": 577.23,
      "aggregate_cpu_stat": {
        "usage_usec": 679261080,
        "user_usec": 486608617,
        "system_usec": 192652463,
        "nr_periods": 18046,
        "nr_throttled": 0,
        "throttled_usec": 0
      },
      "upload_started": true,
      "upload_finished": true,
      "backup_started": true,
      "backup_finished": true
    },
    {
      "elapsed_seconds": 1757.0,
      "completed_requests": 87850,
      "pending_requests": 1,
      "service_rss_bytes": 88055808,
      "service_cpu_seconds": 577.55,
      "aggregate_cpu_stat": {
        "usage_usec": 679619807,
        "user_usec": 486855366,
        "system_usec": 192764441,
        "nr_periods": 18056,
        "nr_throttled": 0,
        "throttled_usec": 0
      },
      "upload_started": true,
      "upload_finished": true,
      "backup_started": true,
      "backup_finished": true
    },
    {
      "elapsed_seconds": 1758.0,
      "completed_requests": 87900,
      "pending_requests": 1,
      "service_rss_bytes": 88055808,
      "service_cpu_seconds": 577.87,
      "aggregate_cpu_stat": {
        "usage_usec": 679979952,
        "user_usec": 487117992,
        "system_usec": 192861959,
        "nr_periods": 18066,
        "nr_throttled": 0,
        "throttled_usec": 0
      },
      "upload_started": true,
      "upload_finished": true,
      "backup_started": true,
      "backup_finished": true
    },
    {
      "elapsed_seconds": 1759.0,
      "completed_requests": 87950,
      "pending_requests": 1,
      "service_rss_bytes": 88055808,
      "service_cpu_seconds": 578.2,
      "aggregate_cpu_stat": {
        "usage_usec": 680352222,
        "user_usec": 487390450,
        "system_usec": 192961772,
        "nr_periods": 18076,
        "nr_throttled": 0,
        "throttled_usec": 0
      },
      "upload_started": true,
      "upload_finished": true,
      "backup_started": true,
      "backup_finished": true
    },
    {
      "elapsed_seconds": 1760.0,
      "completed_requests": 88000,
      "pending_requests": 1,
      "service_rss_bytes": 88055808,
      "service_cpu_seconds": 578.53,
      "aggregate_cpu_stat": {
        "usage_usec": 680735813,
        "user_usec": 487659709,
        "system_usec": 193076103,
        "nr_periods": 18086,
        "nr_throttled": 0,
        "throttled_usec": 0
      },
      "upload_started": true,
      "upload_finished": true,
      "backup_started": true,
      "backup_finished": true
    },
    {
      "elapsed_seconds": 1761.0,
      "completed_requests": 88050,
      "pending_requests": 1,
      "service_rss_bytes": 88055808,
      "service_cpu_seconds": 578.86,
      "aggregate_cpu_stat": {
        "usage_usec": 681092773,
        "user_usec": 487923675,
        "system_usec": 193169097,
        "nr_periods": 18096,
        "nr_throttled": 0,
        "throttled_usec": 0
      },
      "upload_started": true,
      "upload_finished": true,
      "backup_started": true,
      "backup_finished": true
    },
    {
      "elapsed_seconds": 1762.0,
      "completed_requests": 88100,
      "pending_requests": 1,
      "service_rss_bytes": 88055808,
      "service_cpu_seconds": 579.17,
      "aggregate_cpu_stat": {
        "usage_usec": 681449384,
        "user_usec": 488177570,
        "system_usec": 193271814,
        "nr_periods": 18106,
        "nr_throttled": 0,
        "throttled_usec": 0
      },
      "upload_started": true,
      "upload_finished": true,
      "backup_started": true,
      "backup_finished": true
    },
    {
      "elapsed_seconds": 1763.0,
      "completed_requests": 88150,
      "pending_requests": 1,
      "service_rss_bytes": 88055808,
      "service_cpu_seconds": 579.49,
      "aggregate_cpu_stat": {
        "usage_usec": 681812731,
        "user_usec": 488444284,
        "system_usec": 193368447,
        "nr_periods": 18116,
        "nr_throttled": 0,
        "throttled_usec": 0
      },
      "upload_started": true,
      "upload_finished": true,
      "backup_started": true,
      "backup_finished": true
    },
    {
      "elapsed_seconds": 1764.0,
      "completed_requests": 88200,
      "pending_requests": 1,
      "service_rss_bytes": 88055808,
      "service_cpu_seconds": 579.81,
      "aggregate_cpu_stat": {
        "usage_usec": 682171908,
        "user_usec": 488701486,
        "system_usec": 193470422,
        "nr_periods": 18126,
        "nr_throttled": 0,
        "throttled_usec": 0
      },
      "upload_started": true,
      "upload_finished": true,
      "backup_started": true,
      "backup_finished": true
    },
    {
      "elapsed_seconds": 1765.0,
      "completed_requests": 88250,
      "pending_requests": 1,
      "service_rss_bytes": 88055808,
      "service_cpu_seconds": 580.13,
      "aggregate_cpu_stat": {
        "usage_usec": 682527395,
        "user_usec": 488960313,
        "system_usec": 193567081,
        "nr_periods": 18136,
        "nr_throttled": 0,
        "throttled_usec": 0
      },
      "upload_started": true,
      "upload_finished": true,
      "backup_started": true,
      "backup_finished": true
    },
    {
      "elapsed_seconds": 1766.0,
      "completed_requests": 88300,
      "pending_requests": 1,
      "service_rss_bytes": 88055808,
      "service_cpu_seconds": 580.45,
      "aggregate_cpu_stat": {
        "usage_usec": 682896235,
        "user_usec": 489231766,
        "system_usec": 193664469,
        "nr_periods": 18146,
        "nr_throttled": 0,
        "throttled_usec": 0
      },
      "upload_started": true,
      "upload_finished": true,
      "backup_started": true,
      "backup_finished": true
    },
    {
      "elapsed_seconds": 1767.0,
      "completed_requests": 88350,
      "pending_requests": 1,
      "service_rss_bytes": 88055808,
      "service_cpu_seconds": 580.78,
      "aggregate_cpu_stat": {
        "usage_usec": 683251428,
        "user_usec": 489474002,
        "system_usec": 193777425,
        "nr_periods": 18156,
        "nr_throttled": 0,
        "throttled_usec": 0
      },
      "upload_started": true,
      "upload_finished": true,
      "backup_started": true,
      "backup_finished": true
    },
    {
      "elapsed_seconds": 1768.0,
      "completed_requests": 88400,
      "pending_requests": 1,
      "service_rss_bytes": 88055808,
      "service_cpu_seconds": 581.1,
      "aggregate_cpu_stat": {
        "usage_usec": 683612057,
        "user_usec": 489741553,
        "system_usec": 193870504,
        "nr_periods": 18166,
        "nr_throttled": 0,
        "throttled_usec": 0
      },
      "upload_started": true,
      "upload_finished": true,
      "backup_started": true,
      "backup_finished": true
    },
    {
      "elapsed_seconds": 1769.0,
      "completed_requests": 88450,
      "pending_requests": 1,
      "service_rss_bytes": 88055808,
      "service_cpu_seconds": 581.41,
      "aggregate_cpu_stat": {
        "usage_usec": 683971888,
        "user_usec": 490007046,
        "system_usec": 193964841,
        "nr_periods": 18176,
        "nr_throttled": 0,
        "throttled_usec": 0
      },
      "upload_started": true,
      "upload_finished": true,
      "backup_started": true,
      "backup_finished": true
    },
    {
      "elapsed_seconds": 1770.0,
      "completed_requests": 88500,
      "pending_requests": 1,
      "service_rss_bytes": 88055808,
      "service_cpu_seconds": 581.74,
      "aggregate_cpu_stat": {
        "usage_usec": 684328284,
        "user_usec": 490252434,
        "system_usec": 194075850,
        "nr_periods": 18186,
        "nr_throttled": 0,
        "throttled_usec": 0
      },
      "upload_started": true,
      "upload_finished": true,
      "backup_started": true,
      "backup_finished": true
    },
    {
      "elapsed_seconds": 1771.0,
      "completed_requests": 88550,
      "pending_requests": 1,
      "service_rss_bytes": 88055808,
      "service_cpu_seconds": 582.05,
      "aggregate_cpu_stat": {
        "usage_usec": 684687102,
        "user_usec": 490519028,
        "system_usec": 194168073,
        "nr_periods": 18196,
        "nr_throttled": 0,
        "throttled_usec": 0
      },
      "upload_started": true,
      "upload_finished": true,
      "backup_started": true,
      "backup_finished": true
    },
    {
      "elapsed_seconds": 1772.0,
      "completed_requests": 88600,
      "pending_requests": 1,
      "service_rss_bytes": 88055808,
      "service_cpu_seconds": 582.37,
      "aggregate_cpu_stat": {
        "usage_usec": 685042653,
        "user_usec": 490774434,
        "system_usec": 194268218,
        "nr_periods": 18206,
        "nr_throttled": 0,
        "throttled_usec": 0
      },
      "upload_started": true,
      "upload_finished": true,
      "backup_started": true,
      "backup_finished": true
    },
    {
      "elapsed_seconds": 1773.0,
      "completed_requests": 88650,
      "pending_requests": 1,
      "service_rss_bytes": 88055808,
      "service_cpu_seconds": 582.68,
      "aggregate_cpu_stat": {
        "usage_usec": 685395335,
        "user_usec": 491025839,
        "system_usec": 194369496,
        "nr_periods": 18216,
        "nr_throttled": 0,
        "throttled_usec": 0
      },
      "upload_started": true,
      "upload_finished": true,
      "backup_started": true,
      "backup_finished": true
    },
    {
      "elapsed_seconds": 1774.0,
      "completed_requests": 88700,
      "pending_requests": 1,
      "service_rss_bytes": 88055808,
      "service_cpu_seconds": 583.01,
      "aggregate_cpu_stat": {
        "usage_usec": 685762303,
        "user_usec": 491296751,
        "system_usec": 194465551,
        "nr_periods": 18226,
        "nr_throttled": 0,
        "throttled_usec": 0
      },
      "upload_started": true,
      "upload_finished": true,
      "backup_started": true,
      "backup_finished": true
    },
    {
      "elapsed_seconds": 1775.0,
      "completed_requests": 88750,
      "pending_requests": 1,
      "service_rss_bytes": 88055808,
      "service_cpu_seconds": 583.33,
      "aggregate_cpu_stat": {
        "usage_usec": 686120576,
        "user_usec": 491546422,
        "system_usec": 194574154,
        "nr_periods": 18236,
        "nr_throttled": 0,
        "throttled_usec": 0
      },
      "upload_started": true,
      "upload_finished": true,
      "backup_started": true,
      "backup_finished": true
    },
    {
      "elapsed_seconds": 1776.0,
      "completed_requests": 88800,
      "pending_requests": 1,
      "service_rss_bytes": 88055808,
      "service_cpu_seconds": 583.65,
      "aggregate_cpu_stat": {
        "usage_usec": 686481197,
        "user_usec": 491820012,
        "system_usec": 194661185,
        "nr_periods": 18246,
        "nr_throttled": 0,
        "throttled_usec": 0
      },
      "upload_started": true,
      "upload_finished": true,
      "backup_started": true,
      "backup_finished": true
    },
    {
      "elapsed_seconds": 1777.0,
      "completed_requests": 88850,
      "pending_requests": 1,
      "service_rss_bytes": 88055808,
      "service_cpu_seconds": 583.97,
      "aggregate_cpu_stat": {
        "usage_usec": 686838941,
        "user_usec": 492083206,
        "system_usec": 194755735,
        "nr_periods": 18256,
        "nr_throttled": 0,
        "throttled_usec": 0
      },
      "upload_started": true,
      "upload_finished": true,
      "backup_started": true,
      "backup_finished": true
    },
    {
      "elapsed_seconds": 1778.0,
      "completed_requests": 88900,
      "pending_requests": 1,
      "service_rss_bytes": 88707072,
      "service_cpu_seconds": 584.32,
      "aggregate_cpu_stat": {
        "usage_usec": 687217356,
        "user_usec": 492359842,
        "system_usec": 194857514,
        "nr_periods": 18266,
        "nr_throttled": 0,
        "throttled_usec": 0
      },
      "upload_started": true,
      "upload_finished": true,
      "backup_started": true,
      "backup_finished": true
    },
    {
      "elapsed_seconds": 1779.0,
      "completed_requests": 88950,
      "pending_requests": 1,
      "service_rss_bytes": 88707072,
      "service_cpu_seconds": 584.64,
      "aggregate_cpu_stat": {
        "usage_usec": 687586202,
        "user_usec": 492626908,
        "system_usec": 194959294,
        "nr_periods": 18276,
        "nr_throttled": 0,
        "throttled_usec": 0
      },
      "upload_started": true,
      "upload_finished": true,
      "backup_started": true,
      "backup_finished": true
    },
    {
      "elapsed_seconds": 1780.0,
      "completed_requests": 89000,
      "pending_requests": 1,
      "service_rss_bytes": 88707072,
      "service_cpu_seconds": 584.96,
      "aggregate_cpu_stat": {
        "usage_usec": 687949629,
        "user_usec": 492881262,
        "system_usec": 195068367,
        "nr_periods": 18286,
        "nr_throttled": 0,
        "throttled_usec": 0
      },
      "upload_started": true,
      "upload_finished": true,
      "backup_started": true,
      "backup_finished": true
    },
    {
      "elapsed_seconds": 1781.0,
      "completed_requests": 89050,
      "pending_requests": 1,
      "service_rss_bytes": 88707072,
      "service_cpu_seconds": 585.27,
      "aggregate_cpu_stat": {
        "usage_usec": 688300817,
        "user_usec": 493151218,
        "system_usec": 195149599,
        "nr_periods": 18296,
        "nr_throttled": 0,
        "throttled_usec": 0
      },
      "upload_started": true,
      "upload_finished": true,
      "backup_started": true,
      "backup_finished": true
    },
    {
      "elapsed_seconds": 1782.0,
      "completed_requests": 89100,
      "pending_requests": 1,
      "service_rss_bytes": 88707072,
      "service_cpu_seconds": 585.59,
      "aggregate_cpu_stat": {
        "usage_usec": 688652723,
        "user_usec": 493400101,
        "system_usec": 195252621,
        "nr_periods": 18306,
        "nr_throttled": 0,
        "throttled_usec": 0
      },
      "upload_started": true,
      "upload_finished": true,
      "backup_started": true,
      "backup_finished": true
    },
    {
      "elapsed_seconds": 1783.0,
      "completed_requests": 89150,
      "pending_requests": 1,
      "service_rss_bytes": 88707072,
      "service_cpu_seconds": 585.92,
      "aggregate_cpu_stat": {
        "usage_usec": 689020265,
        "user_usec": 493671765,
        "system_usec": 195348499,
        "nr_periods": 18316,
        "nr_throttled": 0,
        "throttled_usec": 0
      },
      "upload_started": true,
      "upload_finished": true,
      "backup_started": true,
      "backup_finished": true
    },
    {
      "elapsed_seconds": 1784.0,
      "completed_requests": 89200,
      "pending_requests": 1,
      "service_rss_bytes": 88707072,
      "service_cpu_seconds": 586.25,
      "aggregate_cpu_stat": {
        "usage_usec": 689393714,
        "user_usec": 493935536,
        "system_usec": 195458178,
        "nr_periods": 18326,
        "nr_throttled": 0,
        "throttled_usec": 0
      },
      "upload_started": true,
      "upload_finished": true,
      "backup_started": true,
      "backup_finished": true
    },
    {
      "elapsed_seconds": 1785.0,
      "completed_requests": 89250,
      "pending_requests": 1,
      "service_rss_bytes": 88707072,
      "service_cpu_seconds": 586.57,
      "aggregate_cpu_stat": {
        "usage_usec": 689746039,
        "user_usec": 494186171,
        "system_usec": 195559868,
        "nr_periods": 18336,
        "nr_throttled": 0,
        "throttled_usec": 0
      },
      "upload_started": true,
      "upload_finished": true,
      "backup_started": true,
      "backup_finished": true
    },
    {
      "elapsed_seconds": 1786.0,
      "completed_requests": 89300,
      "pending_requests": 1,
      "service_rss_bytes": 88707072,
      "service_cpu_seconds": 586.88,
      "aggregate_cpu_stat": {
        "usage_usec": 690098751,
        "user_usec": 494441028,
        "system_usec": 195657723,
        "nr_periods": 18346,
        "nr_throttled": 0,
        "throttled_usec": 0
      },
      "upload_started": true,
      "upload_finished": true,
      "backup_started": true,
      "backup_finished": true
    },
    {
      "elapsed_seconds": 1787.0,
      "completed_requests": 89350,
      "pending_requests": 1,
      "service_rss_bytes": 88707072,
      "service_cpu_seconds": 587.2,
      "aggregate_cpu_stat": {
        "usage_usec": 690448467,
        "user_usec": 494690000,
        "system_usec": 195758467,
        "nr_periods": 18356,
        "nr_throttled": 0,
        "throttled_usec": 0
      },
      "upload_started": true,
      "upload_finished": true,
      "backup_started": true,
      "backup_finished": true
    },
    {
      "elapsed_seconds": 1788.0,
      "completed_requests": 89400,
      "pending_requests": 1,
      "service_rss_bytes": 88707072,
      "service_cpu_seconds": 587.51,
      "aggregate_cpu_stat": {
        "usage_usec": 690798027,
        "user_usec": 494931959,
        "system_usec": 195866067,
        "nr_periods": 18366,
        "nr_throttled": 0,
        "throttled_usec": 0
      },
      "upload_started": true,
      "upload_finished": true,
      "backup_started": true,
      "backup_finished": true
    },
    {
      "elapsed_seconds": 1789.0,
      "completed_requests": 89450,
      "pending_requests": 1,
      "service_rss_bytes": 88707072,
      "service_cpu_seconds": 587.82,
      "aggregate_cpu_stat": {
        "usage_usec": 691147692,
        "user_usec": 495178743,
        "system_usec": 195968949,
        "nr_periods": 18376,
        "nr_throttled": 0,
        "throttled_usec": 0
      },
      "upload_started": true,
      "upload_finished": true,
      "backup_started": true,
      "backup_finished": true
    },
    {
      "elapsed_seconds": 1790.0,
      "completed_requests": 89500,
      "pending_requests": 1,
      "service_rss_bytes": 88707072,
      "service_cpu_seconds": 588.14,
      "aggregate_cpu_stat": {
        "usage_usec": 691501059,
        "user_usec": 495428844,
        "system_usec": 196072214,
        "nr_periods": 18386,
        "nr_throttled": 0,
        "throttled_usec": 0
      },
      "upload_started": true,
      "upload_finished": true,
      "backup_started": true,
      "backup_finished": true
    },
    {
      "elapsed_seconds": 1791.0,
      "completed_requests": 89550,
      "pending_requests": 1,
      "service_rss_bytes": 88707072,
      "service_cpu_seconds": 588.46,
      "aggregate_cpu_stat": {
        "usage_usec": 691855035,
        "user_usec": 495688518,
        "system_usec": 196166517,
        "nr_periods": 18396,
        "nr_throttled": 0,
        "throttled_usec": 0
      },
      "upload_started": true,
      "upload_finished": true,
      "backup_started": true,
      "backup_finished": true
    },
    {
      "elapsed_seconds": 1792.0,
      "completed_requests": 89600,
      "pending_requests": 1,
      "service_rss_bytes": 88707072,
      "service_cpu_seconds": 588.77,
      "aggregate_cpu_stat": {
        "usage_usec": 692209695,
        "user_usec": 495945095,
        "system_usec": 196264600,
        "nr_periods": 18406,
        "nr_throttled": 0,
        "throttled_usec": 0
      },
      "upload_started": true,
      "upload_finished": true,
      "backup_started": true,
      "backup_finished": true
    },
    {
      "elapsed_seconds": 1793.0,
      "completed_requests": 89650,
      "pending_requests": 1,
      "service_rss_bytes": 88707072,
      "service_cpu_seconds": 589.09,
      "aggregate_cpu_stat": {
        "usage_usec": 692562764,
        "user_usec": 496205927,
        "system_usec": 196356836,
        "nr_periods": 18416,
        "nr_throttled": 0,
        "throttled_usec": 0
      },
      "upload_started": true,
      "upload_finished": true,
      "backup_started": true,
      "backup_finished": true
    },
    {
      "elapsed_seconds": 1794.0,
      "completed_requests": 89700,
      "pending_requests": 1,
      "service_rss_bytes": 88707072,
      "service_cpu_seconds": 589.41,
      "aggregate_cpu_stat": {
        "usage_usec": 692920681,
        "user_usec": 496450922,
        "system_usec": 196469759,
        "nr_periods": 18426,
        "nr_throttled": 0,
        "throttled_usec": 0
      },
      "upload_started": true,
      "upload_finished": true,
      "backup_started": true,
      "backup_finished": true
    },
    {
      "elapsed_seconds": 1795.0,
      "completed_requests": 89750,
      "pending_requests": 1,
      "service_rss_bytes": 88707072,
      "service_cpu_seconds": 589.73,
      "aggregate_cpu_stat": {
        "usage_usec": 693276265,
        "user_usec": 496711729,
        "system_usec": 196564536,
        "nr_periods": 18436,
        "nr_throttled": 0,
        "throttled_usec": 0
      },
      "upload_started": true,
      "upload_finished": true,
      "backup_started": true,
      "backup_finished": true
    },
    {
      "elapsed_seconds": 1796.0,
      "completed_requests": 89800,
      "pending_requests": 1,
      "service_rss_bytes": 88707072,
      "service_cpu_seconds": 590.04,
      "aggregate_cpu_stat": {
        "usage_usec": 693629938,
        "user_usec": 496951738,
        "system_usec": 196678200,
        "nr_periods": 18446,
        "nr_throttled": 0,
        "throttled_usec": 0
      },
      "upload_started": true,
      "upload_finished": true,
      "backup_started": true,
      "backup_finished": true
    },
    {
      "elapsed_seconds": 1797.0,
      "completed_requests": 89850,
      "pending_requests": 1,
      "service_rss_bytes": 88707072,
      "service_cpu_seconds": 590.38,
      "aggregate_cpu_stat": {
        "usage_usec": 693996949,
        "user_usec": 497215985,
        "system_usec": 196780963,
        "nr_periods": 18456,
        "nr_throttled": 0,
        "throttled_usec": 0
      },
      "upload_started": true,
      "upload_finished": true,
      "backup_started": true,
      "backup_finished": true
    },
    {
      "elapsed_seconds": 1798.0,
      "completed_requests": 89900,
      "pending_requests": 1,
      "service_rss_bytes": 88707072,
      "service_cpu_seconds": 590.69,
      "aggregate_cpu_stat": {
        "usage_usec": 694348019,
        "user_usec": 497485526,
        "system_usec": 196862492,
        "nr_periods": 18466,
        "nr_throttled": 0,
        "throttled_usec": 0
      },
      "upload_started": true,
      "upload_finished": true,
      "backup_started": true,
      "backup_finished": true
    },
    {
      "elapsed_seconds": 1799.0,
      "completed_requests": 89950,
      "pending_requests": 1,
      "service_rss_bytes": 88707072,
      "service_cpu_seconds": 591.01,
      "aggregate_cpu_stat": {
        "usage_usec": 694700765,
        "user_usec": 497726510,
        "system_usec": 196974254,
        "nr_periods": 18476,
        "nr_throttled": 0,
        "throttled_usec": 0
      },
      "upload_started": true,
      "upload_finished": true,
      "backup_started": true,
      "backup_finished": true
    }
  ],
  "background_progress": {
    "upload_started": true,
    "upload_finished": true,
    "backup_started": true,
    "backup_finished": true
  }
}
```

</details>
