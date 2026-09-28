-- Deterministic integrator, step S1 (planning p4-design §2). Additive: the
-- agent integration path is untouched until a project sets
-- integration_owner='integrator'.
--
-- Widen the credential class enum on credentials and events without rewriting
-- migration history. Startup disables foreign keys on its migration
-- connection, outside this transaction. Preserve rowids (and the events
-- sequence) because history/list cursors use insertion order.
CREATE TABLE credentials_new (
    id TEXT PRIMARY KEY NOT NULL,
    principal_id TEXT NOT NULL REFERENCES principals(id),
    token_hash TEXT NOT NULL UNIQUE,
    created_at INTEGER NOT NULL,
    revoked_at INTEGER,
    expires_at INTEGER,
    name TEXT NOT NULL DEFAULT 'initial' CHECK(length(name) BETWEEN 1 AND 100),
    issued_by TEXT REFERENCES principals(id),
    class TEXT NOT NULL DEFAULT 'interactive'
        CHECK(class IN ('interactive','supervised','integrator')),
    access TEXT NOT NULL DEFAULT 'write' CHECK(access IN ('write','read'))
);
INSERT INTO credentials_new(rowid,id,principal_id,token_hash,created_at,revoked_at,expires_at,name,issued_by,class,access)
    SELECT rowid,id,principal_id,token_hash,created_at,revoked_at,expires_at,name,issued_by,class,access FROM credentials;
DROP TABLE credentials;
ALTER TABLE credentials_new RENAME TO credentials;
CREATE INDEX credentials_principal ON credentials(principal_id);

CREATE TEMP TABLE integrator_events_sequence AS
    SELECT COALESCE((SELECT seq FROM sqlite_sequence WHERE name='events'),0) AS seq;
CREATE TABLE events_new (
    seq INTEGER PRIMARY KEY AUTOINCREMENT,
    project_id TEXT,
    actor_id TEXT NOT NULL REFERENCES principals(id),
    kind TEXT NOT NULL,
    record_id TEXT NOT NULL,
    data_json TEXT NOT NULL CHECK(json_valid(data_json)),
    created_at INTEGER NOT NULL,
    credential_class TEXT CHECK(credential_class IS NULL
        OR credential_class IN ('interactive','supervised','integrator'))
);
INSERT INTO events_new(seq,project_id,actor_id,kind,record_id,data_json,created_at,credential_class)
    SELECT seq,project_id,actor_id,kind,record_id,data_json,created_at,credential_class FROM events;
DROP TABLE events;
ALTER TABLE events_new RENAME TO events;
CREATE INDEX events_project_seq ON events(project_id,seq);
CREATE INDEX project_events ON events(project_id,seq);
CREATE INDEX events_record_history ON events(project_id,record_id,seq);
-- Never reuse a sequence number, even one whose row was deleted.
DELETE FROM sqlite_sequence WHERE name IN ('events','events_new');
INSERT INTO sqlite_sequence(name,seq)
    SELECT 'events', max((SELECT seq FROM integrator_events_sequence),
                         COALESCE((SELECT max(seq) FROM events),0));
DROP TABLE integrator_events_sequence;

-- Per-project cutover switch and the integrator's heartbeat.
ALTER TABLE projects ADD COLUMN integration_owner TEXT NOT NULL DEFAULT 'agent'
    CHECK(integration_owner IN ('agent','integrator'));
ALTER TABLE projects ADD COLUMN integrator_last_seen INTEGER;

-- One pinned integration result per (submission, observed target T0).
CREATE TABLE integrator_results (
    id TEXT PRIMARY KEY NOT NULL,
    project_id TEXT NOT NULL REFERENCES projects(id),
    submission_id TEXT NOT NULL REFERENCES submissions(id),
    t0 TEXT NOT NULL, t0_tree TEXT NOT NULL,
    c TEXT NOT NULL, r TEXT NOT NULL, r_tree TEXT NOT NULL,
    landing_range_json TEXT NOT NULL CHECK(json_valid(landing_range_json)),
    roster_json TEXT NOT NULL CHECK(json_valid(roster_json)),
    created_by TEXT NOT NULL REFERENCES principals(id),
    created_at INTEGER NOT NULL,
    -- Push authority (S2): outstanding from issue until the next observation.
    authority_issued_at INTEGER, authority_expires_at INTEGER,
    -- Other tasks whose commits the landing range carries (5a contributors).
    contributor_tasks_json TEXT CHECK(contributor_tasks_json IS NULL
        OR json_valid(contributor_tasks_json)),
    UNIQUE(submission_id,t0)
);
CREATE INDEX integrator_results_project ON integrator_results(project_id,created_at,id);

-- GitHub Actions check-run receipts observed by the integrator for a result.
CREATE TABLE integrator_receipts (
    result_id TEXT NOT NULL REFERENCES integrator_results(id),
    check_name TEXT NOT NULL,
    run_id INTEGER NOT NULL, run_attempt INTEGER NOT NULL CHECK(run_attempt > 0),
    head_sha TEXT NOT NULL, app_id INTEGER NOT NULL,
    workflow_path TEXT NOT NULL, workflow_blob TEXT NOT NULL,
    conclusion TEXT NOT NULL CHECK(conclusion IN ('success','failure','cancelled',
        'timed_out','neutral','skipped','action_required','stale','startup_failure')),
    observed_at INTEGER NOT NULL,
    PRIMARY KEY(result_id,check_name,run_id,run_attempt)
);

-- Tip observations the integrator attests after (or instead of) a push.
CREATE TABLE integrator_observations (
    id TEXT PRIMARY KEY NOT NULL,
    result_id TEXT NOT NULL REFERENCES integrator_results(id),
    tip TEXT NOT NULL,
    ancestry TEXT NOT NULL CHECK(ancestry IN ('contained','equal_t0','moved')),
    disposition TEXT NOT NULL CHECK(disposition IN ('published','already_contained',
        'published_after_reopen','not_published','target_moved')),
    evidence TEXT NOT NULL,
    observed_by TEXT NOT NULL REFERENCES principals(id),
    observed_at INTEGER NOT NULL
);
CREATE INDEX integrator_observations_result ON integrator_observations(result_id,observed_at);

-- An agent revise that arrived while push authority was outstanding. It
-- applies if the push does not land, and becomes a follow-up task if it does.
CREATE TABLE integrator_revise_requests (
    submission_id TEXT PRIMARY KEY NOT NULL REFERENCES submissions(id),
    result_id TEXT NOT NULL REFERENCES integrator_results(id),
    requested_by TEXT NOT NULL REFERENCES principals(id),
    reason TEXT NOT NULL, reason_code TEXT NOT NULL, evidence TEXT,
    requested_at INTEGER NOT NULL, resolved_at INTEGER,
    resolution TEXT CHECK(resolution IS NULL OR resolution IN ('applied','follow_up')),
    follow_up_task_id TEXT REFERENCES tasks(id)
);

-- Findings the integrator reports (step S4) for the digest and, when
-- requires_human is set, the human queue. First write per
-- (project, kind, dedupe_key) wins; only a human resolves one. A resolved
-- privilege_gate report with decision 'allow' permits pushing its result.
CREATE TABLE integrator_reports (
    id TEXT PRIMARY KEY NOT NULL,
    project_id TEXT NOT NULL REFERENCES projects(id),
    kind TEXT NOT NULL CHECK(kind IN ('privilege_gate','flaky','fix_target',
        'unreviewed_landing','target_rewritten','ruleset_missing')),
    task_id TEXT REFERENCES tasks(id),
    submission_id TEXT REFERENCES submissions(id),
    result_id TEXT REFERENCES integrator_results(id),
    dedupe_key TEXT NOT NULL,
    details_json TEXT NOT NULL CHECK(json_valid(details_json)),
    requires_human INTEGER NOT NULL CHECK(requires_human IN (0,1)),
    created_by TEXT NOT NULL REFERENCES principals(id),
    created_at INTEGER NOT NULL,
    resolved_at INTEGER,
    resolved_by TEXT REFERENCES principals(id),
    resolution_note TEXT,
    decision TEXT CHECK(decision IS NULL OR decision IN ('allow','deny')),
    UNIQUE(project_id,kind,dedupe_key)
);
CREATE INDEX integrator_reports_open ON integrator_reports(project_id,resolved_at,created_at);

-- An approval voided because its reviewer contributed to the landing range.
ALTER TABLE review_decisions ADD COLUMN invalidated_at INTEGER;
ALTER TABLE review_decisions ADD COLUMN invalidated_reason TEXT;

-- Fail and roll back both the schema change and its migration receipt if any
-- relationship was lost. Request connections always enforce foreign keys.
CREATE TEMP TABLE integrator_integrity(violations INTEGER NOT NULL CHECK(violations=0));
INSERT INTO integrator_integrity SELECT count(*) FROM pragma_foreign_key_check;
DROP TABLE integrator_integrity;
