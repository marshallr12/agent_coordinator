-- Human-managed, project-scoped authority to change task definitions.
-- A grant names one exact agent principal or an explicit agent role. The only
-- current agent role is `agent`; a role grant therefore deliberately delegates
-- to every currently valid agent in that project and is visible as such.
CREATE TABLE task_definition_grants (
    id TEXT PRIMARY KEY NOT NULL,
    project_id TEXT NOT NULL REFERENCES projects(id),
    target_kind TEXT NOT NULL CHECK(target_kind IN ('principal','role')),
    agent_principal_id TEXT REFERENCES principals(id),
    agent_role TEXT CHECK(agent_role='agent'),
    created_by TEXT NOT NULL REFERENCES principals(id),
    created_at INTEGER NOT NULL,
    revoked_by TEXT REFERENCES principals(id),
    revoked_at INTEGER,
    revision INTEGER NOT NULL DEFAULT 1,
    CHECK((target_kind='principal' AND agent_principal_id IS NOT NULL AND agent_role IS NULL)
       OR (target_kind='role' AND agent_principal_id IS NULL AND agent_role IS NOT NULL)),
    CHECK(created_by != agent_principal_id),
    CHECK((revoked_by IS NULL) = (revoked_at IS NULL))
);
CREATE INDEX task_definition_grants_active
    ON task_definition_grants(project_id, target_kind, agent_principal_id, agent_role)
    WHERE revoked_at IS NULL;
CREATE UNIQUE INDEX task_definition_grants_principal_unique
    ON task_definition_grants(project_id, agent_principal_id)
    WHERE target_kind='principal' AND revoked_at IS NULL;
CREATE UNIQUE INDEX task_definition_grants_role_unique
    ON task_definition_grants(project_id, agent_role)
    WHERE target_kind='role' AND revoked_at IS NULL;

-- Preserve which human delegation authorized an agent-authored definition
-- revision. Human-authored revisions deliberately have no corresponding row.
CREATE TABLE task_definition_revision_grants (
    project_id TEXT NOT NULL,
    task_id TEXT NOT NULL,
    revision INTEGER NOT NULL,
    grant_id TEXT NOT NULL REFERENCES task_definition_grants(id),
    PRIMARY KEY(task_id, revision),
    FOREIGN KEY(task_id, revision)
        REFERENCES task_revisions(task_id, revision)
);
