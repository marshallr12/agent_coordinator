-- An administrator can delete a revoked credential from the Issued credentials
-- list. Sessions, attempts and reporters keep referencing the row, so the
-- delete is recorded as a timestamp and the credential is hidden, never removed.
ALTER TABLE credentials ADD COLUMN deleted_at INTEGER;
