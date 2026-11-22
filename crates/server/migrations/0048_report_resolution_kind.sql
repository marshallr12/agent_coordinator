-- How an integrator report was resolved: 'automatic' when the service closed
-- it from its own records (a passing target run, a rerun that went through, an
-- integrated or superseded result), 'human' when a person resolved it, NULL
-- while it is open. Rows resolved before this column existed keep their stored
-- requires_human and get a kind only where resolved_by decides it:
--   * a kind only a person ever resolved (anything but flaky and fix_target)
--     is 'human';
--   * a flaky or fix_target report resolved by an agent principal is
--     'automatic', because agents cannot use the human resolve route;
--   * a flaky or fix_target report resolved by a human principal stays NULL:
--     a person's own resolution and an automatic closure that ran inside a
--     human's workflow mutation both record that person.
ALTER TABLE integrator_reports ADD COLUMN resolution_kind TEXT
    CHECK(resolution_kind IS NULL OR resolution_kind IN ('automatic','human'));
UPDATE integrator_reports SET resolution_kind = CASE
    WHEN kind NOT IN ('flaky','fix_target') THEN 'human'
    WHEN (SELECT kind FROM principals WHERE id=integrator_reports.resolved_by)='agent' THEN 'automatic'
    ELSE NULL END
WHERE resolved_at IS NOT NULL;
