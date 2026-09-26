-- Credential attributes (autonomy plan §2.2 6b). Existing credentials stay
-- interactive with write access, which is exactly their behaviour today.
ALTER TABLE credentials ADD COLUMN class TEXT NOT NULL DEFAULT 'interactive'
    CHECK(class IN ('interactive','supervised'));
ALTER TABLE credentials ADD COLUMN access TEXT NOT NULL DEFAULT 'write'
    CHECK(access IN ('write','read'));
-- The credential class behind each audited mutation; NULL for human browser
-- sessions, job reporters and events recorded before this migration.
ALTER TABLE events ADD COLUMN credential_class TEXT
    CHECK(credential_class IS NULL OR credential_class IN ('interactive','supervised'));
