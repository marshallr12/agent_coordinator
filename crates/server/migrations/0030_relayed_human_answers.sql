-- Relayed human decision answers. A human-only project policy switch lets an
-- agent session record the answer a human gave it in the agent's own
-- interface. Each such answer stays attributed to the relaying agent session
-- and keeps the verbatim prompt, the human's response, and the human
-- principal and policy revision that enabled relaying.
ALTER TABLE projects ADD COLUMN allow_relayed_human_answers INTEGER NOT NULL DEFAULT 0
    CHECK(allow_relayed_human_answers IN (0,1));
ALTER TABLE decision_answers ADD COLUMN relayed INTEGER NOT NULL DEFAULT 0
    CHECK(relayed IN (0,1));
ALTER TABLE decision_answers ADD COLUMN relay_prompt TEXT;
ALTER TABLE decision_answers ADD COLUMN relay_response TEXT;
ALTER TABLE decision_answers ADD COLUMN relay_authorized_by TEXT REFERENCES principals(id);
ALTER TABLE decision_answers ADD COLUMN relay_policy_revision INTEGER;
