"""Shared records, bounded context, import/export, and artifact metadata smoke."""
import time


def exercise_shared(temporary, api, cli, project, owner):
    def client(*args, **kwargs):
        return cli(owner, *args, **kwargs)["data"]

    task = client("tasks", "create", body={
        "title": "Anchor shared smoke records",
        "description": "Provides exact task scope for knowledge and a decision.",
        "acceptance_criteria": ["Shared records retain provenance"],
    })
    scope = {
        "task_ids": [task["id"]],
        "components": ["smoke"],
        "environments": ["linux"],
        "versions": ["v1"],
    }
    provenance = {
        "summary": "Disposable shared smoke evidence",
        "source_uri": None,
        "source_task_id": task["id"],
        "source_submission_id": None,
    }
    unique_term = "shared-smoke-correction"
    created = client("knowledge", "create", body={
        "kind": "lesson",
        "title": "Shared smoke lesson",
        "body": "Initial observation before correction.",
        "status": "observed",
        "scope": scope,
        "tags": ["smoke"],
        "applicability": "shared smoke",
        "provenance": provenance,
        "collection": "project",
        "share_across_projects": False,
    })
    assert created["revision"] == 1 and created["status"] == "observed"
    corrected = client("knowledge", "edit", "--id", created["id"], body={
        "expected_revision": 1,
        "title": "Shared smoke lesson",
        "body": f"Validated correction with {unique_term}.",
        "status": "validated",
        "scope": scope,
        "tags": ["smoke", "corrected"],
        "applicability": "shared smoke",
        "provenance": provenance,
        "superseded_by_id": None,
    })
    assert corrected["revision"] == 2 and corrected["status"] == "validated"
    stale = cli(owner, "knowledge", "edit", "--id", created["id"], body={
        "expected_revision": 1,
        "title": "Stale overwrite",
        "body": "Must not replace the correction.",
        "status": "validated",
        "scope": scope,
        "tags": ["smoke"],
        "applicability": "shared smoke",
        "provenance": provenance,
        "superseded_by_id": None,
    }, expected=5)
    assert stale["error"]["code"] == "revision_conflict"

    context = client(
        "context", "--query", unique_term,
        "--task-id", task["id"], "--component", "smoke",
        "--environment", "linux", "--version", "v1",
        "--limit", "20", "--budget", "8192",
    )
    assert context["instructions_complete"]
    assert any(
        item["type"] == "knowledge"
        and item["record"]["id"] == created["id"]
        and unique_term in item["record"]["body"]
        for item in context["items"]
    )

    project_state = api(f"/api/v1/projects/{project}")
    decision = client("decisions", "create", body={
        "question": "May the smoke agent choose the protected release path?",
        "options": ["Proceed", "Stop"],
        "rationale": "The answer is deliberately reserved for a human actor.",
        "required_actor": "human",
        "affected_tasks": [{"task_id": task["id"], "task_revision": task["revision"]}],
        "policy_revision": project_state["policy_revision"],
        "environment": "smoke",
        "conditions": "Operator confirmed",
        "expires_at": int(time.time() * 1000) + 60_000,
    })
    rejected = cli(owner, "decisions", "answer", "--id", decision["id"], body={
        "expected_generation": 1,
        "disposition": "allow",
        "answer": "Proceed",
        "rationale": "An agent must not satisfy a human-required decision.",
        "conditions_confirmed": True,
    }, expected=4)
    assert rejected["error"]["code"] == "operation_not_permitted"

    preview = client("imports", "preview", body={
        "source": {
            "context": "disposable-shared-smoke",
            "git_revision": "a" * 40,
            "observed_at": "2026-09-09T20:00:00-04:00",
            "branch": "main",
            "environment": "smoke",
        },
        "chunks": [{
            "path": "HISTORY.md",
            "markdown": "# Audited\n- [x] Imported closure <!-- coordinator-id: smoke-closure -->\n",
        }],
        "historical_mappings": [],
    })
    assert preview["items"][0]["disposition"] == "closed"
    denied = cli(owner, "imports", "apply", "--id", preview["id"], body={
        "preview_digest": preview["digest"],
        "expected_project_event_revision": preview["project_event_revision"],
    }, expected=4)
    assert denied["error"]["code"] == "operation_not_permitted"
    applied = api(f"/api/v1/projects/{project}/imports/{preview['id']}/apply", {
        "preview_digest": preview["digest"],
        "expected_project_event_revision": preview["project_event_revision"],
    })
    assert applied["closed"] == 1

    artifact = client("artifacts", "link", body={
        "display_name": "shared smoke report",
        "media_type": "text/plain",
        "external_url": "https://unreachable.invalid/shared-smoke-report",
        "task_id": task["id"],
        "job_id": None,
        "size_bytes": 12,
        "sha256": "0" * 64,
        "retention_days": 90,
        "pinned": False,
    })["artifact"]
    assert artifact["kind"] == "external_link"
    assert artifact["state"] == "finalized" and artifact["availability"] == "available"
    assert artifact["content_path"] is None
    listed_artifacts = client("artifacts", "list", "--limit", "20")
    assert any(item["id"] == artifact["id"] for item in listed_artifacts["items"])
    assert "storage" in listed_artifacts

    exported = client("export", "--limit", "200")
    markdown = exported["markdown"]
    assert exported["generated"] and exported["omissions"] == []
    assert created["id"] in markdown
    assert preview["items"][0]["stable_identity"] in markdown
    assert "a" * 40 in markdown
    print("PASS: knowledge correction and bounded context, protected decision answer,")
    print("      human-gated import, generated export provenance, and artifact link metadata.")
