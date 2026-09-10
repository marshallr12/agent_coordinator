-- Read-only history pages follow exact project/task relationships and insertion order.
CREATE INDEX attempts_task_history ON attempts(project_id,task_id,created_at,id);
CREATE INDEX checkpoints_attempt_history ON checkpoints(project_id,attempt_id,created_at,id);
CREATE INDEX checkouts_attempt_history ON checkouts(project_id,attempt_id,created_at,attempt_id);
CREATE INDEX task_revisions_history ON task_revisions(project_id,task_id,revision);
CREATE INDEX events_record_history ON events(project_id,record_id,seq);
CREATE INDEX reservations_attempt_history ON reservations(project_id,attempt_id,created_at,id);
CREATE INDEX workflow_activities_project_subject_history
    ON workflow_activities(project_id,subject_task_id,created_at,id);
CREATE INDEX review_findings_activity_history ON review_findings(activity_id,created_at,id);
CREATE INDEX job_observations_reporter_history
    ON job_observations(reporter_id,observed_at,sequence);
CREATE INDEX submission_artifacts_submission_history
    ON submission_artifacts(project_id,submission_id,artifact_id);
CREATE INDEX submission_knowledge_submission_history
    ON submission_knowledge(project_id,submission_id,knowledge_id);
