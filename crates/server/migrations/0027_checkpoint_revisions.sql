-- Service-verifiable recovery evidence (coordinator task 9181e1e3). A
-- checkpoint may record the full commit SHA of the work-in-progress commit its
-- owner pushed to a durable ref. The recorded SHA is authoritative; the ref is
-- only transport. NULL marks a legacy checkpoint, whose recovery keeps the
-- recoverer's local attestations.
ALTER TABLE checkpoints ADD COLUMN revision TEXT
    CHECK(revision IS NULL OR (length(revision)=40 AND revision NOT GLOB '*[^0-9a-f]*'));
