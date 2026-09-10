# Artifact contract

This release stores bounded reports and logs as authenticated artifacts and also
records external HTTPS links. Source checkpoints continue to travel through Git
remotes. The service never fetches an external artifact URL and never treats a
client filename as a filesystem path.

## Limits and storage

- An uploaded artifact is at most 16 MiB. Request bodies are streamed to disk and
  SHA-256 checked; they are not encoded as JSON or buffered in memory.
- Live and reserved uploads share a 10 GiB service-wide quota. A reservation
  counts until it expires, is deleted, or becomes finalized and later expires.
- The service preserves 256 MiB of free disk space. It checks available space at
  reservation, before upload, and while writing each chunk. Four uploads may run
  concurrently. Waiting for an upload slot is bounded to five seconds and an
  upload must complete within two minutes.
- The store is a directory named after and next to its database, for example
  `coordinator.sqlite3.artifacts`. Each database therefore has an isolated store.
  Blob, staging, and lock names use server-generated UUIDs. On Unix, directories
  are mode 0700 and staged files are mode 0600.
- Upload reservations last one hour. Retention defaults to 90 days and can be set
  from 1 to 3650 days or pinned with no expiry. Expiry is evaluated using the
  service clock. Metadata remains after expiry or deletion.

The service stages a body under its generated key, enforces its exact reserved
size while receiving it, hashes it incrementally, calls `fsync`, and atomically
renames it into the blob store. Only then does it take the SQLite writer lock,
recheck the current credential/session, reservation author, expiry, size, and
digest, and commit finalized metadata, its mutation receipt, and its event in one
transaction. No transaction is held while receiving a body or doing filesystem
work. A crash after rename and before commit leaves a recoverable orphan bound to
the same reservation; retrying the same saved bytes and idempotency key verifies
that blob and finalizes it without replacement or duplication.

Each cleanup pass is bounded to 100 database records, 100 files from one rotating
blob prefix, and 100 staging entries. Before removing a known expired/deleted blob,
cleanup obtains the artifact lock and rechecks its current database state. Unknown
blob orphans must be older than five minutes, then receive the same lock and a
fresh absence check. This prevents cleanup racing upload finalization or a retention
change. Live reservations remain intact, including a durable blob awaiting a
retried database finalization. Run reconciliation after opening the state at
service startup; reservation creation also invokes a bounded pass.

## HTTP API

Every route requires normal human or agent authentication. Every mutation also
requires a persisted `Idempotency-Key`. All JSON success bodies use the normal
`data` envelope.

`POST /api/v1/projects/{project}/artifacts` records an external link:

```json
{
  "display_name": "CI report",
  "media_type": "text/html",
  "external_url": "https://artifacts.example/report/123",
  "task_id": null,
  "job_id": null,
  "size_bytes": null,
  "sha256": null,
  "retention_days": 90,
  "pinned": false
}
```

The URL must be HTTPS, have a host, and contain no embedded credentials. The
service records it without making a network request. Optional task and job IDs
must belong to the same project; when both are supplied, the job must belong to
that task.

`POST /api/v1/projects/{project}/artifacts/uploads` reserves an upload:

```json
{
  "filename": "check-report.txt",
  "media_type": "text/plain",
  "size_bytes": 1234,
  "sha256": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
  "task_id": null,
  "job_id": null,
  "retention_days": 90,
  "pinned": false
}
```

The response contains `artifact` metadata and an authenticated `upload_path`.
Send the exact raw bytes to that path with `PUT`, the same normal authentication,
and a separately persisted idempotency key. `Content-Length`, when supplied, must
match the reservation. The reservation author is the only principal allowed to
upload its content. A retry must reuse its saved bytes, digest, and key.

`GET /api/v1/projects/{project}/artifacts` returns a bounded cursor page plus
authenticated service-wide upload usage, quota, disk capacity/reserve, maximum
artifact size, and default retention diagnostics in `data.storage`. `limit`
defaults to 50 and is capped at 200. `GET
/api/v1/projects/{project}/artifacts/{artifact}` returns one metadata record.

`GET /api/v1/projects/{project}/artifacts/{artifact}/content` streams finalized,
live upload bytes. Downloads always use `Content-Disposition: attachment`, a
generated safe ASCII filename, the recorded content type, `Content-Length`, and
`X-Content-Type-Options: nosniff`. External-link content is not proxied.

`POST /api/v1/projects/{project}/artifacts/{artifact}/retention` accepts:

```json
{"pinned": false, "retention_days": 180}
```

When `pinned` is true, omit `retention_days`. The author or a human operator may
change retention. `POST /api/v1/projects/{project}/artifacts/{artifact}/delete`
accepts a nonempty `reason`; the author or a human operator may create the
deletion tombstone. The database change commits before physical cleanup. If
cleanup is interrupted, retrying the same key or startup reconciliation removes
the inaccessible bytes.

Metadata reports database `state` as `reserved`, `finalized`, or `deleted`, plus
a current `availability` value:

| Availability | Meaning |
| --- | --- |
| `pending` | A live upload reservation has not finalized. |
| `available` | A live external link or service blob is accessible. |
| `expired` | Its reservation or retention deadline passed. |
| `deleted` | An attributed deletion tombstone exists. |
| `unavailable` | Finalized upload metadata exists but its blob is missing or unsafe. |

Task submission integration must call
`validate_submission_artifacts(connection, project, ids, now)` under its writer
transaction before inserting the submission, then call
`link_submission_artifacts(connection, project, submission, ids, now)` in that
same transaction. References are unique, limited to 100, same-project, finalized,
and live. `submission_artifacts` is immutable; later expiry or deletion changes
availability without erasing the historical reference.
