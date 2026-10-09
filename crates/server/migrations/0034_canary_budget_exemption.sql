-- Canary admission. A task created through the `canary` admission class by a
-- principal the service designates as a canary (`--canary-principals`) neither
-- uses nor is held by the weekly agent-task budget: `budget_exempt` marks it,
-- and `budget_admitted_at` stays NULL so the budget count never sees it.
ALTER TABLE tasks ADD COLUMN budget_exempt TEXT
    CHECK(budget_exempt IS NULL OR budget_exempt IN ('canary'));
