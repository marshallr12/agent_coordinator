# Service clock safety contract

Service time is part of every credential, session, lease, reporter, decision,
and publication-authority check. The coordinator records a durable high-water
mark and combines it with an in-process monotonic clock. Authoritative reads and
writers sample that clock through a short SQLite writer transaction before
checking time-dependent authority.

The protected service time never moves behind a time already observed by the
coordinator. During one process lifetime it also advances by monotonic elapsed
time when the wall clock stalls or moves backward. A backward adjustment of at
most 5 seconds is clamped to protected time without opening an incident. A wall
clock more than 5 seconds behind expected protected time creates a durable clock
incident and changes `clock_state.status` to `clock_reconciliation`.

A mutation that first detects rollback under its writer lock commits only the
clock incident and authority expiry, then returns
`409 clock_reconciliation_required` without its requested coordination effect.
Authentication can also detect and persist an incident before the handler runs.
Evidence reads may still succeed; newly expired reporter credentials fail normal
authentication, and ordinary mutations encounter the reconciliation pause.
Detection expires every active attempt and reporter deadline while preserving
task pointers, generations, resource reservations, and integration holds for
recovery inspection. Existing integration authorization is shown with
`valid: false` and `validity_reason: clock_reconciliation_required` while the
incident is active.

## Reconciliation pause

Authenticated evidence remains readable during clock reconciliation. The
coordinator rejects operations that grant, extend, or depend on current work
authority, including sign-in, session creation, instruction acknowledgment,
claims, renewal, submission, decision answers, account changes, credentials,
and new publication authority.

The pause keeps bounded operations needed to make work safe or inspectable:

- browser sign-out and revocation, and agent-session closure;
- reservation release and explicit resolution;
- workflow activity release, integration-result recording, and publication
  reconciliation;
- restore reconciliation and clock reconciliation.

Attempt checkpoint and release routes remain on the pause allowlist so a request
already proven under a live attempt cannot be rejected solely by the global
pause. Rollback detection expires all active attempts, however, so a later
request still fails the normal ownership check and cannot use this exception to
revive one. Reporter deadlines are capped at detection and later reporter
authentication fails.

The available exceptions do not renew an attempt, release a physical or
integration hold implicitly, create a credential, or mark uncertain external
work complete.
Restore invalidation remains available for an offline staged database. If it is
the first observer of a rollback, it commits the clock incident before beginning
restore invalidation under a fresh writer lock.

## Administrator API

`GET /api/v1/admin/clock` requires a currently enabled administrator and returns:

```json
{
  "clock_state": {
    "status": "clock_reconciliation",
    "last_safe_time": "2027-01-15T08:00:20.000Z",
    "last_safe_time_ms": 1800000020000,
    "incident_id": "incident-id",
    "observed_wall_time": "2027-01-15T08:00:01.000Z",
    "observed_wall_time_ms": 1800000001000,
    "detected_at": "2027-01-15T08:00:20.000Z"
  },
  "incident": {
    "id": "incident-id",
    "observed_wall_time_ms": 1800000001000,
    "high_water_time_ms": 1800000020000,
    "recovered_at": null,
    "recovery_reason": null,
    "recovered_by": null,
    "recovery_kind": null
  },
  "material_rollback_threshold_ms": 5000
}
```

`POST /api/v1/admin/clock/reconcile` accepts an idempotency key and:

```json
{
  "incident_id": "incident-id",
  "reason": "The host clock now agrees with an independent trusted source."
}
```

The incident ID must still be current. The reason contains 1 through 2000 bytes
and may contain newlines and tabs but no other control characters or surrounding
whitespace. The raw wall clock must reach or pass the current protected time;
the 5-second detection tolerance does not weaken this recovery check. Otherwise
the endpoint returns `clock_incident_changed`,
`clock_still_untrusted`, or `clock_not_paused` as appropriate. A successful
administrator reconciliation records the administrator principal in both the
ordinary mutation event and the immutable clock-reconciliation audit record.

If sign-in is blocked or the administrator session expired, correct host time and
run the host-local command as the service account:

```sh
sudo -u agent-coordinator /usr/local/bin/agent-coordinator-server \
  --database /var/lib/agent-coordinator/coordinator.sqlite3 \
  recover-clock --reason 'Host time verified against the configured trusted source.'
```

Use evidence specific to the incident. There is no unauthenticated HTTP recovery
route. A successful command does not recover expired task ownership or clear a
separate restore reconciliation pause. If a restored snapshot also contains a
clock incident, reconcile the clock before recovering an operator password and
completing the restore checklist.

It applies the same trustworthy-time and reason checks. Its audit event has
`initiator_kind: host_operator` and a null `actor_id`; it does not invent or
impersonate a principal. Correcting the wall clock does not itself lift the
pause. An administrator or host operator must record explicit reconciliation.

## Guarantees and deployment limit

The persisted high-water mark prevents a restarted process from returning to a
service time it previously observed, so an authority deadline already observed
as expired cannot become current after restart. The monotonic anchor accounts
for elapsed time only within the current process lifetime.

No local monotonic clock can measure time across a power-off or restart. If the
wall clock rolls backward during unobserved downtime but still starts later than
the last persisted observation, the coordinator cannot detect that adjustment
without an external trusted time source. Hosts must start the service only after
establishing synchronized, trustworthy wall time. Service-manager ordering on a
time-synchronization unit alone is not evidence that synchronization succeeded.
The clock status is protected coordinator time, not an attestation of absolute
time or an NTP health report.
