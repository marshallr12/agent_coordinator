ALTER TABLE principals
ADD COLUMN revision INTEGER NOT NULL DEFAULT 1 CHECK(revision > 0);

ALTER TABLE credentials
ADD COLUMN name TEXT NOT NULL DEFAULT 'initial'
CHECK(length(name) BETWEEN 1 AND 100);

ALTER TABLE credentials
ADD COLUMN issued_by TEXT REFERENCES principals(id);

ALTER TABLE browser_sessions
ADD COLUMN created_at INTEGER;

CREATE INDEX principals_human_access
ON principals(kind, disabled_at, role, name);

CREATE INDEX browser_sessions_principal_created
ON browser_sessions(principal_id, created_at, id);
