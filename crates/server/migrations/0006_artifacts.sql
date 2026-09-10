CREATE TABLE artifacts (
    id TEXT PRIMARY KEY NOT NULL,
    project_id TEXT NOT NULL REFERENCES projects(id),
    kind TEXT NOT NULL CHECK(kind IN ('external_link','upload')),
    task_id TEXT,
    job_id TEXT,
    display_name TEXT NOT NULL CHECK(length(display_name) BETWEEN 1 AND 255),
    media_type TEXT NOT NULL CHECK(length(media_type) BETWEEN 1 AND 255),
    size_bytes INTEGER CHECK(size_bytes IS NULL OR size_bytes >= 0),
    sha256 TEXT CHECK(sha256 IS NULL OR (length(sha256)=64 AND sha256=lower(sha256))),
    external_url TEXT,
    storage_key TEXT UNIQUE,
    state TEXT NOT NULL CHECK(state IN ('reserved','finalized','deleted')),
    created_by TEXT NOT NULL REFERENCES principals(id),
    created_at INTEGER NOT NULL,
    reservation_expires_at INTEGER,
    finalized_at INTEGER,
    retention_until INTEGER,
    pinned INTEGER NOT NULL DEFAULT 0 CHECK(pinned IN (0,1)),
    deleted_at INTEGER,
    deleted_by TEXT REFERENCES principals(id),
    deletion_reason TEXT,
    UNIQUE(project_id,id),
    FOREIGN KEY(project_id,task_id) REFERENCES tasks(project_id,id),
    FOREIGN KEY(project_id,job_id) REFERENCES jobs(project_id,id),
    CHECK((kind='external_link' AND external_url IS NOT NULL AND storage_key IS NULL
              AND state IN ('finalized','deleted') AND reservation_expires_at IS NULL)
       OR (kind='upload' AND external_url IS NULL AND storage_key IS NOT NULL
              AND size_bytes IS NOT NULL AND sha256 IS NOT NULL)),
    CHECK((state='reserved' AND finalized_at IS NULL AND deleted_at IS NULL)
       OR (state='finalized' AND finalized_at IS NOT NULL AND deleted_at IS NULL)
       OR (state='deleted' AND deleted_at IS NOT NULL))
);
CREATE INDEX artifacts_project_created ON artifacts(project_id,created_at,id);
CREATE INDEX artifacts_task ON artifacts(task_id);
CREATE INDEX artifacts_job ON artifacts(job_id);
CREATE INDEX artifacts_retention ON artifacts(state,pinned,retention_until);

CREATE TABLE submission_artifacts (
    project_id TEXT NOT NULL,
    submission_id TEXT NOT NULL,
    artifact_id TEXT NOT NULL,
    linked_at INTEGER NOT NULL,
    PRIMARY KEY(submission_id,artifact_id),
    FOREIGN KEY(project_id,submission_id) REFERENCES submissions(project_id,id),
    FOREIGN KEY(project_id,artifact_id) REFERENCES artifacts(project_id,id)
);
CREATE INDEX submission_artifacts_artifact ON submission_artifacts(artifact_id);
