-- A human account may register passkeys (WebAuthn credentials) and sign in
-- with one instead of its password. Only the public key is stored. The
-- challenge table holds the one-time random value of each ceremony; a row is
-- deleted when its ceremony finishes and expires five minutes after creation.
CREATE TABLE passkeys (
    id TEXT PRIMARY KEY NOT NULL,
    principal_id TEXT NOT NULL REFERENCES principals(id),
    credential_id TEXT NOT NULL UNIQUE,
    public_key TEXT NOT NULL,
    sign_count INTEGER NOT NULL DEFAULT 0,
    name TEXT NOT NULL CHECK(length(name) BETWEEN 1 AND 100),
    created_at INTEGER NOT NULL,
    last_used_at INTEGER
);
CREATE INDEX passkeys_principal ON passkeys(principal_id,created_at);

CREATE TABLE passkey_challenges (
    id TEXT PRIMARY KEY NOT NULL,
    purpose TEXT NOT NULL CHECK(purpose IN ('register','sign_in')),
    principal_id TEXT REFERENCES principals(id),
    challenge TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL
);
CREATE INDEX passkey_challenges_expiry ON passkey_challenges(expires_at);
