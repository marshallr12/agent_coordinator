-- A human account carries one email address. A new or changed address needs
-- verification: the service mails a single-use link whose token is stored only
-- as a hash. Issuing a newer link (the "resend" action, or a changed address)
-- invalidates every earlier unused link for the account.
ALTER TABLE principals ADD COLUMN email TEXT
    CHECK(email IS NULL OR length(email) BETWEEN 3 AND 254);
ALTER TABLE principals ADD COLUMN email_verified_at INTEGER;

CREATE TABLE email_verifications (
    id TEXT PRIMARY KEY NOT NULL,
    principal_id TEXT NOT NULL REFERENCES principals(id),
    email TEXT NOT NULL,
    token_hash TEXT NOT NULL UNIQUE,
    created_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL,
    invalidated_at INTEGER,
    used_at INTEGER,
    -- Delivery outcome, filled in after the message is handed to the relay.
    -- send_error is a short code, never relay output.
    sent_at INTEGER,
    send_error TEXT
);
CREATE INDEX email_verifications_principal ON email_verifications(principal_id,created_at);

-- Every administrator that exists when this schema arrives is the service
-- owner's account; their address is already known to be reachable, so it is
-- recorded as verified without sending anything.
UPDATE principals
SET email='marshall.rosenstein@gmail.com',
    email_verified_at=CAST(strftime('%s','now') AS INTEGER)*1000,
    revision=revision+1
WHERE kind='human' AND role='admin';
