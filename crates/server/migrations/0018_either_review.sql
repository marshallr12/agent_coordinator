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
DROP TABLE workflow_activities;
ALTER TABLE workflow_activities_new RENAME TO workflow_activities;
CREATE INDEX workflow_activities_subject ON workflow_activities(subject_task_id,created_at,id);
CREATE INDEX workflow_activities_project_subject_history ON workflow_activities(project_id,subject_task_id,created_at,id);

-- Fail and roll back both the schema change and its migration receipt if any
-- relationship was lost. Request connections always enforce foreign keys.
CREATE TEMP TABLE either_review_integrity(violations INTEGER NOT NULL CHECK(violations=0));
INSERT INTO either_review_integrity SELECT count(*) FROM pragma_foreign_key_check;
DROP TABLE either_review_integrity;
