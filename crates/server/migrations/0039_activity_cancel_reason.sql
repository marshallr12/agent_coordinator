-- A canceled workflow activity now records why: a machine-readable code, a
-- human-readable detail and the revisions involved (a JSON object). Activities
-- canceled before this migration keep NULL; nothing is inferred.
ALTER TABLE workflow_activities ADD COLUMN cancel_code TEXT;
ALTER TABLE workflow_activities ADD COLUMN cancel_detail TEXT;
ALTER TABLE workflow_activities ADD COLUMN cancel_context_json TEXT CHECK(cancel_context_json IS NULL OR json_valid(cancel_context_json));
