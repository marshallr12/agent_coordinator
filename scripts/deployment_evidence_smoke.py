"""Publish synthetic deployment evidence and retrieve it from another workstation."""
import hashlib
import json
import uuid


def exercise_deployment_evidence(temporary, api, cli, project, owner, task, job):
    source = temporary / "deployment-report.json"
    report = {
        "schema_version": 1, "synthetic": True,
        "project_id": project, "task_id": task, "producer_job_id": job,
        "source_revision": "a" * 40, "package_sha256": "b" * 64,
        "started_at": "2026-09-17T00:00:00Z", "finished_at": "2026-09-17T00:01:00Z",
        "health": {"status": "verified", "scope": "disposable fixture"},
        "backup": {"status": "not_exercised"},
        "rollback": {"status": "not_exercised"},
        "timers": {"status": "not_exercised"},
    }
    original = json.dumps(report, sort_keys=True).encode()
    source.write_bytes(original)
    digest = hashlib.sha256(original).hexdigest()
    metadata = {"filename": source.name, "media_type": "application/json",
                "size_bytes": len(original), "sha256": digest,
                "task_id": task, "job_id": job, "retention_days": 90, "pinned": False}
    publication = str(uuid.uuid4())
    command = ("artifacts", "publish", "--id", publication, "--file", str(source))
    artifact = cli(owner, *command, body=metadata)["data"]["artifact"]
    identity = artifact["id"]
    assert artifact["state"] == "finalized" and artifact["availability"] == "available"
    assert artifact["sha256"] == digest and artifact["job_id"] == job
    # Remove only the saved reservation result: simulate interruption after the
    # server committed it but before the client recorded its artifact identity.
    # The retry must replay the original POST key, not reserve another artifact.
    journals = list((temporary / f"agent-{owner}").rglob(f"{publication}/publication.json"))
    assert len(journals) == 1
    journal = journals[0]
    saved = json.loads(journal.read_text())
    saved["artifact_id"] = None
    journal.write_text(json.dumps(saved))
    source.write_bytes(b"modified source after publication")
    retried = cli(owner, *command, body=metadata)["data"]["artifact"]
    assert retried["id"] == identity and retried["sha256"] == digest
    records = api(f"/api/v1/projects/{project}/artifacts")["items"]
    assert sum(item["job_id"] == job for item in records) == 1
    changed = dict(metadata, filename="different.json")
    cli(owner, *command, body=changed, expected=2)

    reader = 1 - owner
    history = cli(reader, "tasks", "history", "--id", task,
                  "--kind", "artifacts", "--limit", "50")["data"]
    assert identity in json.dumps(history) and job in json.dumps(history)
    current = cli(reader, "artifacts", "show", "--id", identity)["data"]["artifact"]
    assert current["task_id"] == task and current["job_id"] == job
    destination = temporary / "independent-workstation-report.json"
    cli(reader, "artifacts", "download", "--id", identity, "--output", str(destination))
    assert destination.read_bytes() == original
    assert hashlib.sha256(destination.read_bytes()).hexdigest() == digest
    assert json.loads(destination.read_text())["package_sha256"] == report["package_sha256"]
    other = api("/api/v1/projects", {"name": "Evidence isolation", "repository_url":
                "https://example.com/isolation.git", "target_branch": "main"})["id"]
    cli(reader, "connect", project_id=other)
    wrong = cli(reader, "artifacts", "show", "--id", identity, project_id=other, expected=6)
    assert "error" in wrong
    cli(owner, "artifacts", "delete", "--id", identity, body={"reason": "Exercise unavailable evidence"})
    cli(reader, "artifacts", "download", "--id", identity,
        "--output", str(temporary / "must-not-exist.json"), expected=2)
    cli(owner, *command, body=metadata, expected=2)
    assert not (temporary / "must-not-exist.json").exists()
    print("PASS: deployment publication replay without duplicate reservation, retained exact bytes,")
    print("      task/job discovery, independent download/checksum, project isolation and unavailable evidence.")
