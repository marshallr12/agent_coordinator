CREATE TABLE projects (
    id TEXT PRIMARY KEY, name TEXT NOT NULL UNIQUE,
    repository_url TEXT NOT NULL, target_branch TEXT NOT NULL,
    policy_revision INTEGER NOT NULL DEFAULT 1,
    review_mode TEXT NOT NULL DEFAULT 'agent' CHECK(review_mode IN ('none','agent','human','both')),
    recovery_mode TEXT NOT NULL DEFAULT 'agent' CHECK(recovery_mode IN ('agent','manual')),
    lease_seconds INTEGER NOT NULL DEFAULT 600 CHECK(lease_seconds BETWEEN 30 AND 3600),
    rules TEXT NOT NULL DEFAULT '',
    agent_rule_editing INTEGER NOT NULL DEFAULT 0 CHECK(agent_rule_editing IN (0,1)),
    automatic_integration INTEGER NOT NULL DEFAULT 1 CHECK(automatic_integration IN (0,1)),
    created_at INTEGER NOT NULL
);
CREATE TABLE policy_revisions (
    project_id TEXT NOT NULL REFERENCES projects(id), revision INTEGER NOT NULL,
    data_json TEXT NOT NULL CHECK(json_valid(data_json)), actor_id TEXT NOT NULL REFERENCES principals(id),
    created_at INTEGER NOT NULL, PRIMARY KEY(project_id,revision)
);
CREATE TABLE tasks (
    id TEXT PRIMARY KEY, project_id TEXT NOT NULL REFERENCES projects(id),
    title TEXT NOT NULL, description TEXT NOT NULL,
    acceptance_json TEXT NOT NULL CHECK(json_valid(acceptance_json)),
    kind TEXT NOT NULL CHECK(kind IN ('code','general')),
    priority INTEGER NOT NULL CHECK(priority BETWEEN 0 AND 3),
    lifecycle TEXT NOT NULL CHECK(lifecycle IN ('planned','open','done','canceled','superseded')),
    revision INTEGER NOT NULL DEFAULT 1,
    generation INTEGER NOT NULL DEFAULT 0,
    current_attempt_id TEXT,
    blocked_reason TEXT, created_at INTEGER NOT NULL, ready_since INTEGER NOT NULL,
    UNIQUE(project_id,id),
    FOREIGN KEY(project_id,id,current_attempt_id) REFERENCES attempts(project_id,task_id,id) DEFERRABLE INITIALLY DEFERRED
);
CREATE INDEX task_queue ON tasks(project_id,lifecycle,priority,ready_since,id);
CREATE TABLE task_revisions (
    project_id TEXT NOT NULL, task_id TEXT NOT NULL, revision INTEGER NOT NULL,
    data_json TEXT NOT NULL CHECK(json_valid(data_json)), actor_id TEXT NOT NULL REFERENCES principals(id),
    created_at INTEGER NOT NULL, PRIMARY KEY(task_id,revision),
    FOREIGN KEY(project_id,task_id) REFERENCES tasks(project_id,id)
);
CREATE TABLE task_dependencies (
    project_id TEXT NOT NULL, task_id TEXT NOT NULL, prerequisite_id TEXT NOT NULL,
    PRIMARY KEY(task_id,prerequisite_id), CHECK(task_id != prerequisite_id),
    FOREIGN KEY(project_id,task_id) REFERENCES tasks(project_id,id),
    FOREIGN KEY(project_id,prerequisite_id) REFERENCES tasks(project_id,id)
);
CREATE INDEX dependency_reverse ON task_dependencies(prerequisite_id,task_id);
CREATE TABLE attempts (
    id TEXT PRIMARY KEY, project_id TEXT NOT NULL, task_id TEXT NOT NULL,
    owner_id TEXT NOT NULL REFERENCES principals(id), session_id TEXT NOT NULL,
    credential_id TEXT REFERENCES credentials(id),
    generation INTEGER NOT NULL, state TEXT NOT NULL CHECK(state IN ('active','released','blocked','expired','canceled','submitted')),
    mode TEXT NOT NULL CHECK(mode IN ('work','recovery')),
    expires_at INTEGER NOT NULL, last_heartbeat_at INTEGER NOT NULL,
    last_progress_at INTEGER NOT NULL, created_at INTEGER NOT NULL,
    ended_at INTEGER, outcome TEXT,
    UNIQUE(task_id,generation), UNIQUE(project_id,id), UNIQUE(project_id,task_id,id),
    FOREIGN KEY(project_id,task_id) REFERENCES tasks(project_id,id)
);
CREATE UNIQUE INDEX one_active_attempt ON attempts(task_id) WHERE state='active';
CREATE INDEX attempt_expiry ON attempts(state,expires_at);
CREATE TABLE checkpoints (
    id TEXT PRIMARY KEY, project_id TEXT NOT NULL, attempt_id TEXT NOT NULL,
    summary TEXT NOT NULL, current_action TEXT NOT NULL, next_step TEXT NOT NULL,
    blockers_json TEXT NOT NULL CHECK(json_valid(blockers_json)), created_at INTEGER NOT NULL,
    FOREIGN KEY(project_id,attempt_id) REFERENCES attempts(project_id,id)
);
CREATE TABLE instruction_acknowledgments (
    session_id TEXT NOT NULL REFERENCES agent_sessions(id),
    project_id TEXT NOT NULL REFERENCES projects(id), policy_revision INTEGER NOT NULL,
    instruction_version TEXT NOT NULL, created_at INTEGER NOT NULL,
    PRIMARY KEY(session_id,project_id)
);
CREATE TABLE checkouts (
    attempt_id TEXT PRIMARY KEY, project_id TEXT NOT NULL,
    workstation_id TEXT NOT NULL, identity TEXT NOT NULL,
    path TEXT NOT NULL, branch TEXT NOT NULL, base_revision TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    FOREIGN KEY(project_id,attempt_id) REFERENCES attempts(project_id,id)
);
CREATE INDEX checkout_identity ON checkouts(workstation_id,identity);
CREATE INDEX project_events ON events(project_id,seq);
