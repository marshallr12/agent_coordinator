-- Who may mint digest acknowledgement links. A link marks the digest read, and
-- the read silences the digest-neglect page, so an ordinary agent credential
-- must not be able to obtain one. By default only humans can; the owner may
-- designate one agent principal per project (the digest timer's credential).
-- The link names its minter, and the read record keeps that principal.
-- designated_by is NULL when the host operator designated it locally.
CREATE TABLE digest_senders (
    project_id TEXT PRIMARY KEY REFERENCES projects(id),
    principal_id TEXT NOT NULL REFERENCES principals(id),
    designated_by TEXT REFERENCES principals(id),
    designated_at INTEGER NOT NULL
);

ALTER TABLE digest_reads ADD COLUMN minted_by TEXT REFERENCES principals(id);
