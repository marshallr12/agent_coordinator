-- A forgotten password is recovered with a single-use link mailed to the
-- account's verified address. Only the token's hash is stored. A link expires
-- one hour after it is issued, when it is used, or when a newer link or a
-- successful reset replaces it.
CREATE TABLE password_resets (
    id TEXT PRIMARY KEY NOT NULL,
    principal_id TEXT NOT NULL REFERENCES principals(id),
    email TEXT NOT NULL,
    token_hash TEXT NOT NULL UNIQUE,
    created_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL,
    invalidated_at INTEGER,
    used_at INTEGER
);
CREATE INDEX password_resets_principal ON password_resets(principal_id,created_at);
