ALTER TABLE projects ADD COLUMN allow_subagent_reviews INTEGER NOT NULL DEFAULT 0 CHECK(allow_subagent_reviews IN (0,1));

CREATE TABLE subagent_identities (
    id TEXT PRIMARY KEY NOT NULL,
    project_id TEXT NOT NULL REFERENCES projects(id),
    principal_id TEXT NOT NULL REFERENCES principals(id),
    name TEXT NOT NULL CHECK(length(name) BETWEEN 1 AND 120),
    parent_identity_id TEXT REFERENCES subagent_identities(id),
    created_by_session_id TEXT NOT NULL REFERENCES agent_sessions(id),
    created_at INTEGER NOT NULL,
    UNIQUE(project_id,principal_id,name)
);
ALTER TABLE agent_sessions ADD COLUMN subagent_identity_id TEXT REFERENCES subagent_identities(id);
ALTER TABLE agent_sessions ADD COLUMN parent_session_id TEXT REFERENCES agent_sessions(id);
CREATE INDEX sessions_subagent_identity ON agent_sessions(subagent_identity_id);
