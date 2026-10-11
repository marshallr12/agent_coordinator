-- Task cancel, archive, restore and delete events now carry the reason, the acting
-- principal and (for cancel) the replacement task. Copy them once into the events
-- recorded before that, but only where the original mutation receipt still holds the
-- result. A receipt matches its event by principal, operation and timestamp (both
-- were written in one transaction). Events without a surviving receipt keep their
-- empty data; nothing is inferred.
UPDATE events
SET data_json = COALESCE((
    SELECT CASE
        WHEN json_type(r.result_json, '$.replacement_task_id') = 'text'
        THEN json_object('reason', json_extract(r.result_json, '$.reason'),
                         'actor_id', events.actor_id,
                         'replacement_task_id', json_extract(r.result_json, '$.replacement_task_id'))
        ELSE json_object('reason', json_extract(r.result_json, '$.reason'),
                         'actor_id', events.actor_id)
    END
    FROM mutation_receipts r
    WHERE r.principal_id = events.actor_id
      AND r.created_at = events.created_at
      AND r.compacted_at IS NULL
      AND json_type(r.result_json) = 'object'
      AND json_type(r.result_json, '$.reason') = 'text'
      AND r.operation = 'POST /api/v1/projects/' || events.project_id || '/tasks/' || events.record_id
                        || '/' || CASE events.kind WHEN 'task.canceled' THEN 'cancel'
                                                   WHEN 'task.archived' THEN 'archive'
                                                   WHEN 'task.restored' THEN 'restore'
                                                   ELSE 'delete' END
    LIMIT 1
), events.data_json)
WHERE events.kind IN ('task.canceled', 'task.archived', 'task.restored', 'task.deleted')
  AND events.data_json = '{}'
  AND events.project_id IS NOT NULL;
