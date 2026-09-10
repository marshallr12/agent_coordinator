-- Restore is an authority boundary, not merely a file replacement.  These
-- singleton epochs survive restarts and are rotated only by an explicit restore.
CREATE TABLE service_state (
    singleton INTEGER PRIMARY KEY NOT NULL CHECK(singleton=1),
    authority_epoch TEXT NOT NULL CHECK(length(authority_epoch) BETWEEN 1 AND 128),
    cursor_epoch TEXT NOT NULL CHECK(length(cursor_epoch) BETWEEN 1 AND 128),
    coordination_state TEXT NOT NULL CHECK(coordination_state IN ('ready','restore_reconciliation')),
    restore_id TEXT,
    restored_at INTEGER,
    CHECK(coordination_state='ready'
       OR (restore_id IS NOT NULL AND restored_at IS NOT NULL))
);
INSERT INTO service_state(singleton,authority_epoch,cursor_epoch,coordination_state)
VALUES(1,'initial','initial','ready');

ALTER TABLE mutation_receipts
ADD COLUMN authority_epoch TEXT NOT NULL DEFAULT 'initial';

ALTER TABLE integration_authorizations
ADD COLUMN invalidated_at INTEGER;
ALTER TABLE integration_authorizations
ADD COLUMN authorization_revision INTEGER NOT NULL DEFAULT 1 CHECK(authorization_revision > 0);

CREATE TABLE integration_authorization_history (
    activity_id TEXT NOT NULL REFERENCES workflow_activities(id),
    authorization_revision INTEGER NOT NULL,
    submission_id TEXT NOT NULL REFERENCES submissions(id),
    project_policy_revision INTEGER NOT NULL,
    workflow_policy_revision INTEGER NOT NULL,
    actor_id TEXT NOT NULL REFERENCES principals(id),
    summary TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    invalidated_at INTEGER NOT NULL,
    PRIMARY KEY(activity_id,authorization_revision)
);

CREATE TABLE restore_runs (
    id TEXT PRIMARY KEY NOT NULL,
    snapshot_id TEXT NOT NULL CHECK(length(snapshot_id) BETWEEN 1 AND 255),
    reason TEXT NOT NULL CHECK(length(reason) BETWEEN 1 AND 2000),
    previous_authority_epoch TEXT NOT NULL,
    authority_epoch TEXT NOT NULL,
    previous_cursor_epoch TEXT NOT NULL,
    cursor_epoch TEXT NOT NULL,
    restored_at INTEGER NOT NULL,
    old_installation_fenced_at INTEGER,
    old_installation_fence_evidence TEXT,
    post_snapshot_gap_reconciled_at INTEGER,
    post_snapshot_gap_evidence TEXT,
    completed_at INTEGER,
    completion_reason TEXT
);

-- The inventory is captured before the staged database is published.  A later
-- operational reconciliation cannot erase the fact that a restored hold needed
-- inspection.
CREATE TABLE restore_requirements (
    restore_id TEXT NOT NULL REFERENCES restore_runs(id),
    kind TEXT NOT NULL CHECK(kind IN ('resource_hold','integration_hold')),
    target_id TEXT NOT NULL,
    project_id TEXT NOT NULL REFERENCES projects(id),
    state_at_restore TEXT NOT NULL,
    detail_json TEXT NOT NULL CHECK(json_valid(detail_json)),
    inspected_at INTEGER,
    inspected_by TEXT REFERENCES principals(id),
    disposition TEXT CHECK(disposition IN ('held','released','unknown')),
    evidence TEXT,
    PRIMARY KEY(restore_id,kind,target_id),
    CHECK((inspected_at IS NULL AND inspected_by IS NULL AND disposition IS NULL AND evidence IS NULL)
       OR (inspected_at IS NOT NULL AND inspected_by IS NOT NULL AND disposition IS NOT NULL AND evidence IS NOT NULL))
);
CREATE INDEX restore_requirements_page
ON restore_requirements(restore_id,kind,target_id);

-- Publication reconciliation is intentionally available during the pause and
-- may replace an uncertain integration activity. Capture its newly held target
-- in the same transaction so restore completion cannot overlook it.
CREATE TRIGGER restore_capture_inserted_integration_hold
AFTER INSERT ON integration_holds
WHEN NEW.state='held'
 AND (SELECT coordination_state FROM service_state WHERE singleton=1)='restore_reconciliation'
BEGIN
    INSERT INTO restore_requirements(
        restore_id,kind,target_id,project_id,state_at_restore,detail_json
    )
    SELECT ss.restore_id,'integration_hold',NEW.id,a.project_id,'held',
           json_object(
               'hold_id',NEW.id,
               'activity_id',NEW.activity_id,
               'subject_task_id',a.subject_task_id,
               'activity_task_id',a.activity_task_id,
               'canonical_repository_key',NEW.canonical_repository_key,
               'target_branch',NEW.target_branch
           )
    FROM service_state ss
    JOIN workflow_activities a ON a.id=NEW.activity_id
    WHERE ss.singleton=1;
END;

CREATE TABLE restore_reconciliation_events (
    seq INTEGER PRIMARY KEY AUTOINCREMENT,
    restore_id TEXT NOT NULL REFERENCES restore_runs(id),
    actor_id TEXT NOT NULL REFERENCES principals(id),
    kind TEXT NOT NULL CHECK(kind IN ('hold_inspected','old_installation_fenced','post_snapshot_gap_reconciled','restore_completed')),
    target_kind TEXT,
    target_id TEXT,
    disposition TEXT,
    evidence TEXT NOT NULL CHECK(length(evidence) BETWEEN 1 AND 32768),
    created_at INTEGER NOT NULL
);
CREATE INDEX restore_reconciliation_events_run
ON restore_reconciliation_events(restore_id,seq);
