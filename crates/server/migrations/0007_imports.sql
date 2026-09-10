CREATE TABLE import_previews (
    id TEXT PRIMARY KEY NOT NULL,
    project_id TEXT NOT NULL REFERENCES projects(id),
    digest TEXT NOT NULL,
    project_event_revision INTEGER NOT NULL,
    source_json TEXT NOT NULL CHECK(json_valid(source_json)),
    items_json TEXT NOT NULL CHECK(json_valid(items_json) AND json_type(items_json)='array'),
    conflicts_json TEXT NOT NULL CHECK(json_valid(conflicts_json) AND json_type(conflicts_json)='array'),
    unresolved_links_json TEXT NOT NULL CHECK(json_valid(unresolved_links_json) AND json_type(unresolved_links_json)='array'),
    created_by TEXT NOT NULL REFERENCES principals(id),
    created_at INTEGER NOT NULL,
    applied_by TEXT REFERENCES principals(id),
    applied_at INTEGER,
    CHECK((applied_by IS NULL) = (applied_at IS NULL))
);
CREATE INDEX import_previews_project_created ON import_previews(project_id,created_at,id);

CREATE TABLE import_records (
    id TEXT PRIMARY KEY NOT NULL,
    project_id TEXT NOT NULL REFERENCES projects(id),
    stable_identity TEXT NOT NULL,
    record_revision INTEGER NOT NULL CHECK(record_revision > 0),
    record_kind TEXT NOT NULL CHECK(record_kind IN ('task','historical')),
    disposition TEXT NOT NULL CHECK(disposition IN ('planned','closed','rejected','superseded')),
    title TEXT NOT NULL,
    source_context TEXT NOT NULL,
    source_path TEXT NOT NULL,
    section_identity TEXT NOT NULL,
    source_git_revision TEXT NOT NULL,
    source_observed_at TEXT NOT NULL,
    source_branch TEXT NOT NULL,
    source_environment TEXT NOT NULL,
    source_digest TEXT NOT NULL,
    evidence TEXT NOT NULL,
    task_id TEXT REFERENCES tasks(id),
    task_revision_at_apply INTEGER,
    knowledge_id TEXT REFERENCES knowledge_records(id),
    knowledge_revision_at_apply INTEGER,
    closure_provenance_json TEXT CHECK(closure_provenance_json IS NULL OR json_valid(closure_provenance_json)),
    preview_id TEXT NOT NULL REFERENCES import_previews(id),
    created_by TEXT NOT NULL REFERENCES principals(id),
    created_at INTEGER NOT NULL,
    updated_by TEXT NOT NULL REFERENCES principals(id),
    updated_at INTEGER NOT NULL,
    UNIQUE(project_id,stable_identity),
    CHECK((record_kind='task' AND task_id IS NOT NULL AND task_revision_at_apply IS NOT NULL
           AND knowledge_id IS NULL AND knowledge_revision_at_apply IS NULL)
       OR (record_kind='historical' AND task_id IS NULL AND task_revision_at_apply IS NULL
           AND knowledge_id IS NOT NULL AND knowledge_revision_at_apply IS NOT NULL))
);
CREATE INDEX import_records_project_identity ON import_records(project_id,stable_identity);
CREATE INDEX import_records_task ON import_records(task_id);
CREATE INDEX import_records_knowledge ON import_records(knowledge_id);

CREATE TABLE import_applications (
    preview_id TEXT PRIMARY KEY NOT NULL REFERENCES import_previews(id),
    project_id TEXT NOT NULL REFERENCES projects(id),
    preview_digest TEXT NOT NULL,
    project_event_revision INTEGER NOT NULL,
    result_json TEXT NOT NULL CHECK(json_valid(result_json)),
    applied_by TEXT NOT NULL REFERENCES principals(id),
    applied_at INTEGER NOT NULL
);
