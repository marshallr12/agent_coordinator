ALTER TABLE attempts ADD COLUMN task_revision INTEGER;
ALTER TABLE attempts ADD COLUMN policy_revision INTEGER;

ALTER TABLE jobs ADD COLUMN check_identity TEXT;
ALTER TABLE jobs ADD COLUMN check_version TEXT;
ALTER TABLE jobs ADD COLUMN check_environment TEXT;

CREATE TABLE workflow_policies (
    project_id TEXT PRIMARY KEY NOT NULL REFERENCES projects(id),
    revision INTEGER NOT NULL CHECK(revision > 0),
    canonical_repository_key TEXT NOT NULL CHECK(length(canonical_repository_key) BETWEEN 1 AND 255),
    required_checks_json TEXT NOT NULL CHECK(json_valid(required_checks_json) AND json_type(required_checks_json)='array'),
    updated_by TEXT NOT NULL REFERENCES principals(id), updated_at INTEGER NOT NULL
);
CREATE TABLE workflow_policy_revisions (
    project_id TEXT NOT NULL REFERENCES projects(id), revision INTEGER NOT NULL CHECK(revision > 0),
    canonical_repository_key TEXT NOT NULL,
    required_checks_json TEXT NOT NULL CHECK(json_valid(required_checks_json) AND json_type(required_checks_json)='array'),
    actor_id TEXT NOT NULL REFERENCES principals(id), created_at INTEGER NOT NULL,
    PRIMARY KEY(project_id,revision)
);

CREATE TABLE submissions (
    id TEXT PRIMARY KEY NOT NULL, project_id TEXT NOT NULL, task_id TEXT NOT NULL,
    attempt_id TEXT NOT NULL UNIQUE,
    kind TEXT NOT NULL CHECK(kind IN ('code','general')),
    task_revision INTEGER NOT NULL, project_policy_revision INTEGER NOT NULL,
    workflow_policy_revision INTEGER NOT NULL,
    summary TEXT NOT NULL,
    acceptance_evidence_json TEXT NOT NULL CHECK(json_valid(acceptance_evidence_json)),
    handoff TEXT NOT NULL, canonical_repository_key TEXT, repository_url TEXT,
    target_branch TEXT, base_revision TEXT, candidate_revision TEXT, candidate_tree TEXT,
    created_by TEXT NOT NULL REFERENCES principals(id), contributor_session_id TEXT NOT NULL,
    created_at INTEGER NOT NULL, superseded_at INTEGER,
    UNIQUE(project_id,id),
    FOREIGN KEY(project_id,task_id) REFERENCES tasks(project_id,id),
    FOREIGN KEY(project_id,attempt_id) REFERENCES attempts(project_id,id),
    CHECK((kind='general' AND workflow_policy_revision=0 AND canonical_repository_key IS NULL
           AND repository_url IS NULL AND target_branch IS NULL AND base_revision IS NULL
           AND candidate_revision IS NULL AND candidate_tree IS NULL)
       OR (kind='code' AND workflow_policy_revision>0 AND canonical_repository_key IS NOT NULL
           AND repository_url IS NOT NULL AND target_branch IS NOT NULL AND base_revision IS NOT NULL
           AND candidate_revision IS NOT NULL AND candidate_tree IS NOT NULL))
);
CREATE UNIQUE INDEX one_current_submission ON submissions(task_id) WHERE superseded_at IS NULL;
CREATE INDEX submissions_task_created ON submissions(task_id,created_at,id);

CREATE TABLE workflow_subjects (
    project_id TEXT NOT NULL, task_id TEXT PRIMARY KEY NOT NULL,
    current_submission_id TEXT NOT NULL UNIQUE,
    phase TEXT NOT NULL CHECK(phase IN ('review','integration','revision_needed','done')),
    updated_at INTEGER NOT NULL,
    FOREIGN KEY(project_id,task_id) REFERENCES tasks(project_id,id),
    FOREIGN KEY(project_id,current_submission_id) REFERENCES submissions(project_id,id)
);
CREATE TABLE task_contributors (
    task_id TEXT NOT NULL REFERENCES tasks(id), principal_id TEXT NOT NULL REFERENCES principals(id),
    session_id TEXT NOT NULL, first_contributed_at INTEGER NOT NULL,
    PRIMARY KEY(task_id,principal_id,session_id)
);

CREATE TABLE workflow_activities (
    id TEXT PRIMARY KEY NOT NULL, project_id TEXT NOT NULL, subject_task_id TEXT NOT NULL,
    submission_id TEXT NOT NULL, activity_task_id TEXT NOT NULL UNIQUE,
    kind TEXT NOT NULL CHECK(kind IN ('agent_review','human_review','integration')),
    slot INTEGER NOT NULL DEFAULT 1 CHECK(slot > 0),
    state TEXT NOT NULL CHECK(state IN ('queued','active','completed','canceled','recovery_required')),
    created_at INTEGER NOT NULL, completed_at INTEGER, canceled_at INTEGER,
    UNIQUE(submission_id,kind,slot), UNIQUE(project_id,id),
    FOREIGN KEY(project_id,subject_task_id) REFERENCES tasks(project_id,id),
    FOREIGN KEY(project_id,submission_id) REFERENCES submissions(project_id,id),
    FOREIGN KEY(project_id,activity_task_id) REFERENCES tasks(project_id,id)
);
CREATE INDEX workflow_activities_subject ON workflow_activities(subject_task_id,created_at,id);
CREATE TABLE review_decisions (
    activity_id TEXT PRIMARY KEY NOT NULL REFERENCES workflow_activities(id),
    submission_id TEXT NOT NULL REFERENCES submissions(id), attempt_id TEXT NOT NULL REFERENCES attempts(id),
    reviewer_id TEXT NOT NULL REFERENCES principals(id), reviewer_session_id TEXT NOT NULL,
    decision TEXT NOT NULL CHECK(decision IN ('approved','changes_requested')),
    summary TEXT NOT NULL, created_at INTEGER NOT NULL
);
CREATE TABLE review_findings (
    id TEXT PRIMARY KEY NOT NULL, activity_id TEXT NOT NULL REFERENCES workflow_activities(id),
    severity TEXT NOT NULL CHECK(severity IN ('required','advisory')),
    remedy TEXT NOT NULL, evidence TEXT NOT NULL, created_at INTEGER NOT NULL
);

CREATE TABLE integration_authorizations (
    activity_id TEXT PRIMARY KEY NOT NULL REFERENCES workflow_activities(id),
    submission_id TEXT NOT NULL REFERENCES submissions(id), project_policy_revision INTEGER NOT NULL,
    workflow_policy_revision INTEGER NOT NULL, actor_id TEXT NOT NULL REFERENCES principals(id),
    summary TEXT NOT NULL, created_at INTEGER NOT NULL
);
CREATE TABLE integration_holds (
    id TEXT PRIMARY KEY NOT NULL, activity_id TEXT NOT NULL UNIQUE REFERENCES workflow_activities(id),
    canonical_repository_key TEXT NOT NULL, target_branch TEXT NOT NULL,
    state TEXT NOT NULL CHECK(state IN ('held','released')),
    acquired_by TEXT NOT NULL REFERENCES principals(id), acquired_at INTEGER NOT NULL,
    released_by TEXT REFERENCES principals(id), released_at INTEGER, release_reason TEXT
);
CREATE UNIQUE INDEX one_integration_target_hold ON integration_holds(canonical_repository_key,target_branch) WHERE state='held';
CREATE TABLE publication_intents (
    activity_id TEXT PRIMARY KEY NOT NULL REFERENCES workflow_activities(id),
    submission_id TEXT NOT NULL REFERENCES submissions(id), attempt_id TEXT NOT NULL REFERENCES attempts(id),
    observed_target_revision TEXT NOT NULL, result_revision TEXT NOT NULL, result_tree TEXT NOT NULL,
    created_by TEXT NOT NULL REFERENCES principals(id), created_at INTEGER NOT NULL
);
CREATE TABLE integration_results (
    activity_id TEXT PRIMARY KEY NOT NULL REFERENCES workflow_activities(id),
    submission_id TEXT NOT NULL REFERENCES submissions(id), attempt_id TEXT NOT NULL REFERENCES attempts(id),
    publication_state TEXT NOT NULL CHECK(publication_state IN ('published','not_published','uncertain')),
    observed_target_revision TEXT NOT NULL, result_revision TEXT NOT NULL, result_tree TEXT NOT NULL,
    check_job_ids_json TEXT NOT NULL CHECK(json_valid(check_job_ids_json)), summary TEXT NOT NULL,
    reported_by TEXT NOT NULL REFERENCES principals(id), created_at INTEGER NOT NULL
);
CREATE TABLE publication_reconciliations (
    activity_id TEXT PRIMARY KEY NOT NULL REFERENCES workflow_activities(id),
    submission_id TEXT NOT NULL REFERENCES submissions(id),
    disposition TEXT NOT NULL CHECK(disposition IN ('published','not_published')),
    observed_target_revision TEXT NOT NULL, observed_target_tree TEXT NOT NULL, evidence TEXT NOT NULL,
    actor_id TEXT NOT NULL REFERENCES principals(id), created_at INTEGER NOT NULL
);
