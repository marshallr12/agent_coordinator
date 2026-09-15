-- Widen the two closed enums without rewriting migration history. Startup
-- disables foreign keys on its migration connection, outside this transaction.
-- Preserve rowids because history/list cursors use insertion order.
CREATE TABLE projects_new (
    id TEXT PRIMARY KEY, name TEXT NOT NULL UNIQUE,
    repository_url TEXT NOT NULL, target_branch TEXT NOT NULL,
    policy_revision INTEGER NOT NULL DEFAULT 1,
    review_mode TEXT NOT NULL DEFAULT 'agent' CHECK(review_mode IN ('none','agent','human','both','either')),
    recovery_mode TEXT NOT NULL DEFAULT 'agent' CHECK(recovery_mode IN ('agent','manual')),
    lease_seconds INTEGER NOT NULL DEFAULT 600 CHECK(lease_seconds BETWEEN 30 AND 3600),
    rules TEXT NOT NULL DEFAULT '',
    agent_rule_editing INTEGER NOT NULL DEFAULT 0 CHECK(agent_rule_editing IN (0,1)),
    automatic_integration INTEGER NOT NULL DEFAULT 1 CHECK(automatic_integration IN (0,1)),
    created_at INTEGER NOT NULL,
    allow_subagent_reviews INTEGER NOT NULL DEFAULT 0 CHECK(allow_subagent_reviews IN (0,1))
);
INSERT INTO projects_new(rowid,id,name,repository_url,target_branch,policy_revision,review_mode,recovery_mode,lease_seconds,rules,agent_rule_editing,automatic_integration,created_at,allow_subagent_reviews)
    SELECT rowid,id,name,repository_url,target_branch,policy_revision,review_mode,recovery_mode,lease_seconds,rules,agent_rule_editing,automatic_integration,created_at,allow_subagent_reviews FROM projects;
DROP TABLE projects;
ALTER TABLE projects_new RENAME TO projects;

CREATE TABLE workflow_activities_new (
    id TEXT PRIMARY KEY NOT NULL, project_id TEXT NOT NULL, subject_task_id TEXT NOT NULL,
    submission_id TEXT NOT NULL, activity_task_id TEXT NOT NULL UNIQUE,
    kind TEXT NOT NULL CHECK(kind IN ('agent_review','human_review','either_review','integration')),
    slot INTEGER NOT NULL DEFAULT 1 CHECK(slot > 0),
    state TEXT NOT NULL CHECK(state IN ('queued','active','completed','canceled','recovery_required')),
    created_at INTEGER NOT NULL, completed_at INTEGER, canceled_at INTEGER,
    UNIQUE(submission_id,kind,slot), UNIQUE(project_id,id),
    FOREIGN KEY(project_id,subject_task_id) REFERENCES tasks(project_id,id),
    FOREIGN KEY(project_id,submission_id) REFERENCES submissions(project_id,id),
    FOREIGN KEY(project_id,activity_task_id) REFERENCES tasks(project_id,id)
);
INSERT INTO workflow_activities_new(rowid,id,project_id,subject_task_id,submission_id,activity_task_id,kind,slot,state,created_at,completed_at,canceled_at)
    SELECT rowid,id,project_id,subject_task_id,submission_id,activity_task_id,kind,slot,state,created_at,completed_at,canceled_at FROM workflow_activities;
-- This trigger belongs to integration_holds but references the rebuilt table.
DROP TRIGGER restore_capture_inserted_integration_hold;
DROP TABLE workflow_activities;
ALTER TABLE workflow_activities_new RENAME TO workflow_activities;
CREATE INDEX workflow_activities_subject ON workflow_activities(subject_task_id,created_at,id);
CREATE INDEX workflow_activities_project_subject_history ON workflow_activities(project_id,subject_task_id,created_at,id);

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

-- Fail and roll back both the schema change and its migration receipt if any
-- relationship was lost. Request connections always enforce foreign keys.
CREATE TEMP TABLE either_review_integrity(violations INTEGER NOT NULL CHECK(violations=0));
INSERT INTO either_review_integrity SELECT count(*) FROM pragma_foreign_key_check;
DROP TABLE either_review_integrity;
