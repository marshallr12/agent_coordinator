-- Digest read tracking. The owner reads the attention digest in the dashboard
-- or by following the signed acknowledgement link in the emailed digest; either
-- records when, so the canary can page when the digest goes unread. The link is
-- an HMAC-SHA256 over its purpose, project and expiry under a key generated
-- here, so it names no session and can do nothing but record a read.
CREATE TABLE digest_reads (
    project_id TEXT PRIMARY KEY REFERENCES projects(id),
    last_read_at INTEGER NOT NULL,
    read_via TEXT NOT NULL CHECK(read_via IN ('dashboard','ack_link'))
);

CREATE TABLE digest_ack_key (
    singleton INTEGER PRIMARY KEY CHECK(singleton=1),
    key BLOB NOT NULL CHECK(length(key)=32)
);
INSERT INTO digest_ack_key(singleton,key) VALUES(1,randomblob(32));
