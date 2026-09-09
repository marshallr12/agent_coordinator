CREATE TABLE principals (
    id TEXT PRIMARY KEY NOT NULL,
    name TEXT NOT NULL UNIQUE CHECK(length(name) BETWEEN 1 AND 100),
    kind TEXT NOT NULL CHECK(kind IN ('human','agent')),
    role TEXT NOT NULL CHECK(role IN ('admin','operator','agent')),
    password_hash TEXT,
    disabled_at INTEGER,
    created_at INTEGER NOT NULL,
    CHECK((kind='human' AND role IN ('admin','operator') AND password_hash IS NOT NULL)
       OR (kind='agent' AND role='agent' AND password_hash IS NULL))
);
CREATE TABLE credentials (
    id TEXT PRIMARY KEY NOT NULL,
    principal_id TEXT NOT NULL REFERENCES principals(id),
    token_hash TEXT NOT NULL UNIQUE,
    created_at INTEGER NOT NULL,
    revoked_at INTEGER,
    expires_at INTEGER
);
CREATE INDEX credentials_principal ON credentials(principal_id);
CREATE TABLE browser_sessions (
    id TEXT PRIMARY KEY NOT NULL,
    principal_id TEXT NOT NULL REFERENCES principals(id),
    token_hash TEXT NOT NULL UNIQUE,
    expires_at INTEGER NOT NULL,
    revoked_at INTEGER
);
CREATE INDEX browser_sessions_principal ON browser_sessions(principal_id);
CREATE TABLE agent_sessions (
    id TEXT PRIMARY KEY NOT NULL,
    principal_id TEXT NOT NULL REFERENCES principals(id),
    credential_id TEXT NOT NULL REFERENCES credentials(id),
    workstation_id TEXT NOT NULL,
    proof_hash TEXT NOT NULL,
    closed_at INTEGER,
    created_at INTEGER NOT NULL,
    capabilities TEXT NOT NULL CHECK(json_valid(capabilities) AND json_type(capabilities)='array'),
    harness TEXT NOT NULL
);
CREATE INDEX agent_sessions_credential ON agent_sessions(credential_id);
CREATE TABLE events (
    seq INTEGER PRIMARY KEY AUTOINCREMENT,
    project_id TEXT,
    actor_id TEXT NOT NULL REFERENCES principals(id),
    kind TEXT NOT NULL,
    record_id TEXT NOT NULL,
    data_json TEXT NOT NULL CHECK(json_valid(data_json)),
    created_at INTEGER NOT NULL
);
CREATE INDEX events_project_seq ON events(project_id,seq);
CREATE TABLE mutation_receipts (
    principal_id TEXT NOT NULL REFERENCES principals(id),
    operation TEXT NOT NULL,
    key TEXT NOT NULL,
    fingerprint TEXT NOT NULL,
    result_json TEXT NOT NULL CHECK(json_valid(result_json)),
    created_at INTEGER NOT NULL,
    PRIMARY KEY(principal_id,operation,key)
);
