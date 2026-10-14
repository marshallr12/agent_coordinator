-- Human-delivered tasks. A task marked `human_delivered` is work an owner
-- delivers in person (for example on a host no supervisor can reach). `next`
-- never offers it to a supervisor and a supervisor's claim of it is refused,
-- through every review cycle; an interactive CLI session or the console still
-- claims it, and `complete-external` still finishes it. Existing tasks keep
-- the default: nothing is inferred from a title or origin.
ALTER TABLE tasks ADD COLUMN human_delivered INTEGER NOT NULL DEFAULT 0
    CHECK(human_delivered IN (0,1));
