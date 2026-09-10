//! Immutable submissions, independent review, and serialized integration.
use crate::{auth::Auth, error::AppError, mutation::Mutation, response, state::AppState};
use axum::{
    Json, Router,
    extract::{Path, State, rejection::JsonRejection},
    http::HeaderMap,
    routing::{get, post},
};
use coordinator_core::{
    ActivityClaimInput, ActivityReleaseInput, FinalizeIntegrationInput, INSTRUCTION_VERSION,
    IntegrationAuthorizationInput, IntegrationResultInput, PublicationIntentInput,
    PublicationReconciliationInput, ReopenSubmissionInput, RequiredCheck, ReviewInput,
    SubmissionInput, WorkflowPolicyInput, timestamp,
};
use serde_json::{Value, json};
use sqlx::{Row, SqliteConnection};
use std::collections::BTreeSet;
use uuid::Uuid;

type Reply = Result<Json<Value>, AppError>;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/api/v1/projects/{project}/workflow-policy",
            get(get_workflow_policy).put(put_workflow_policy),
        )
        .route(
            "/api/v1/projects/{project}/attempts/{attempt}/submissions",
            post(submit),
        )
        .route(
            "/api/v1/projects/{project}/tasks/{task}/workflow",
            get(task_workflow),
        )
        .route(
            "/api/v1/projects/{project}/tasks/{task}/workflow/reopen",
            post(reopen),
        )
        .route(
            "/api/v1/projects/{project}/workflow-activities/{activity}",
            get(activity_detail),
        )
        .route(
            "/api/v1/projects/{project}/workflow-activities/{activity}/claim",
            post(claim_activity),
        )
        .route(
            "/api/v1/projects/{project}/workflow-activities/{activity}/release",
            post(release_activity),
        )
        .route(
            "/api/v1/projects/{project}/workflow-activities/{activity}/review",
            post(review),
        )
        .route(
            "/api/v1/projects/{project}/workflow-activities/{activity}/authorization",
            post(authorize_integration),
        )
        .route(
            "/api/v1/projects/{project}/workflow-activities/{activity}/publication-intent",
            post(publication_intent),
        )
        .route(
            "/api/v1/projects/{project}/workflow-activities/{activity}/integration-result",
            post(integration_result),
        )
        .route(
            "/api/v1/projects/{project}/workflow-activities/{activity}/publication-reconciliation",
            post(reconcile_publication),
        )
        .route(
            "/api/v1/projects/{project}/workflow-activities/{activity}/finalize",
            post(finalize),
        )
}

fn payload<T>(value: Result<Json<T>, JsonRejection>) -> Result<T, AppError> {
    value.map(|Json(v)| v).map_err(|_| {
        AppError::bad_request("The JSON body does not match this operation's request schema.")
    })
}

fn bounded(value: &str, name: &str, max: usize, required: bool) -> Result<(), AppError> {
    if value.len() > max || value.contains('\0') || (required && value.trim().is_empty()) {
        return Err(AppError::bad_request(&format!(
            "{name} must {}contain at most {max} bytes and no NUL characters.",
            if required { "be nonempty and " } else { "" }
        )));
    }
    Ok(())
}

fn revision(value: &str, name: &str) -> Result<(), AppError> {
    if !matches!(value.len(), 40 | 64) || !value.bytes().all(|v| v.is_ascii_hexdigit()) {
        return Err(AppError::bad_request(&format!(
            "{name} must be a full 40–64 character hexadecimal revision identity."
        )));
    }
    Ok(())
}

fn session(actor: &crate::auth::Actor) -> Result<&str, AppError> {
    actor.session_id.as_deref().ok_or_else(|| {
        AppError::forbidden("A current authenticated session is required for workflow work.")
    })
}

fn human(actor: &crate::auth::Actor) -> Result<(), AppError> {
    if actor.kind != "human" {
        return Err(AppError::forbidden(
            "An authenticated human operator must perform this action.",
        ));
    }
    Ok(())
}

async fn project_exists(c: &mut SqliteConnection, project: &str) -> Result<(), AppError> {
    if sqlx::query_scalar::<_, i64>("SELECT count(*) FROM projects WHERE id=?")
        .bind(project)
        .fetch_one(&mut *c)
        .await?
        == 0
    {
        return Err(AppError::not_found());
    }
    Ok(())
}

fn validate_checks(checks: &[RequiredCheck]) -> Result<(), AppError> {
    if checks.is_empty() || checks.len() > 100 {
        return Err(AppError::bad_request(
            "Configure an explicit roster of 1–100 required checks.",
        ));
    }
    let mut identities = BTreeSet::new();
    for check in checks {
        bounded(&check.identity, "check identity", 255, true)?;
        bounded(&check.version, "check version", 255, true)?;
        bounded(&check.environment, "check environment", 255, true)?;
        if !identities.insert((
            check.identity.clone(),
            check.version.clone(),
            check.environment.clone(),
        )) {
            return Err(AppError::bad_request(
                "Each required check identity/version/environment tuple must be unique.",
            ));
        }
    }
    Ok(())
}

async fn workflow_policy_value(c: &mut SqliteConnection, project: &str) -> Result<Value, AppError> {
    let row = sqlx::query("SELECT * FROM workflow_policies WHERE project_id=?")
        .bind(project)
        .fetch_optional(&mut *c)
        .await?
        .ok_or_else(|| {
            AppError::conflict(
                "workflow_policy_required",
                "A human must configure the canonical repository key and explicit required-check roster.",
            )
        })?;
    Ok(json!({
        "project_id":project,
        "revision":row.get::<i64,_>("revision"),
        "canonical_repository_key":row.get::<String,_>("canonical_repository_key"),
        "required_checks":serde_json::from_str::<Value>(&row.get::<String,_>("required_checks_json"))?,
        "updated_at":timestamp(row.get("updated_at"))
    }))
}

async fn get_workflow_policy(
    State(state): State<AppState>,
    _auth: Auth,
    Path(project): Path<String>,
) -> Reply {
    let mut c = state.pool.acquire().await?;
    project_exists(&mut c, &project).await?;
    Ok(response(workflow_policy_value(&mut c, &project).await?))
}

async fn put_workflow_policy(
    State(state): State<AppState>,
    auth: Auth,
    Path(project): Path<String>,
    headers: HeaderMap,
    body: Result<Json<WorkflowPolicyInput>, JsonRejection>,
) -> Reply {
    let input = payload(body)?;
    bounded(
        &input.canonical_repository_key,
        "canonical_repository_key",
        255,
        true,
    )?;
    validate_checks(&input.required_checks)?;
    let mut mutation = Mutation::begin(
        &state,
        &auth,
        &headers,
        &format!("PUT /api/v1/projects/{project}/workflow-policy"),
        &input,
    )
    .await?;
    human(&mutation.actor)?;
    project_exists(&mut mutation.tx, &project).await?;
    if let Some(value) = mutation.replay {
        return Ok(response(value));
    }
    if sqlx::query_scalar::<_, i64>(
        "SELECT count(*) FROM integration_holds h JOIN workflow_activities a ON a.id=h.activity_id \
         WHERE a.project_id=? AND h.state='held'",
    )
    .bind(&project)
    .fetch_one(&mut *mutation.tx)
    .await?
        > 0
    {
        return Err(AppError::conflict(
            "policy_hold_conflict",
            "Finish or reconcile the held integration before changing workflow policy.",
        ));
    }
    let current = sqlx::query(
        "SELECT revision,canonical_repository_key FROM workflow_policies WHERE project_id=?",
    )
    .bind(&project)
    .fetch_optional(&mut *mutation.tx)
    .await?;
    let current_revision = current.as_ref().map_or(0, |r| r.get("revision"));
    if current_revision != input.expected_revision {
        return Err(AppError::conflict(
            "revision_conflict",
            "Read the current workflow policy before replacing it.",
        ));
    }
    if current.as_ref().is_some_and(|row| {
        row.get::<String, _>("canonical_repository_key") != input.canonical_repository_key
    }) {
        let used: i64 = sqlx::query_scalar("SELECT count(*) FROM submissions WHERE project_id=?")
            .bind(&project)
            .fetch_one(&mut *mutation.tx)
            .await?;
        if used > 0 {
            return Err(AppError::conflict(
                "canonical_binding_frozen",
                "The canonical repository binding cannot change after workflow evidence exists.",
            ));
        }
    }
    let repository_url: String =
        sqlx::query_scalar("SELECT repository_url FROM projects WHERE id=?")
            .bind(&project)
            .fetch_one(&mut *mutation.tx)
            .await?;
    if let Some(other)=sqlx::query_scalar::<_,String>("SELECT wp.canonical_repository_key FROM workflow_policies wp JOIN projects p ON p.id=wp.project_id WHERE p.repository_url=? AND p.id!=? LIMIT 1")
        .bind(&repository_url).bind(&project).fetch_optional(&mut *mutation.tx).await?
        && other!=input.canonical_repository_key {
        return Err(AppError::conflict("canonical_repository_conflict","Projects with the exact same repository URL must use the same canonical repository key."));
    }
    let next = current_revision + 1;
    let encoded = serde_json::to_string(&input.required_checks)?;
    sqlx::query(
        "INSERT INTO workflow_policies(project_id,revision,canonical_repository_key,required_checks_json,updated_by,updated_at) \
         VALUES(?,?,?,?,?,?) ON CONFLICT(project_id) DO UPDATE SET revision=excluded.revision, \
         canonical_repository_key=excluded.canonical_repository_key,required_checks_json=excluded.required_checks_json, \
         updated_by=excluded.updated_by,updated_at=excluded.updated_at",
    )
    .bind(&project)
    .bind(next)
    .bind(&input.canonical_repository_key)
    .bind(&encoded)
    .bind(&mutation.actor.id)
    .bind(mutation.now)
    .execute(&mut *mutation.tx)
    .await?;
    sqlx::query("INSERT INTO workflow_policy_revisions(project_id,revision,canonical_repository_key,required_checks_json,actor_id,created_at) VALUES(?,?,?,?,?,?)")
        .bind(&project).bind(next).bind(&input.canonical_repository_key).bind(&encoded)
        .bind(&mutation.actor.id).bind(mutation.now).execute(&mut *mutation.tx).await?;
    let value = workflow_policy_value(&mut mutation.tx, &project).await?;
    Ok(response(
        mutation
            .finish(value, Some(&project), "workflow_policy.updated", &project)
            .await?,
    ))
}

async fn submit(
    State(state): State<AppState>,
    auth: Auth,
    Path((project, attempt)): Path<(String, String)>,
    headers: HeaderMap,
    body: Result<Json<SubmissionInput>, JsonRejection>,
) -> Reply {
    let input = payload(body)?;
    bounded(&input.summary, "summary", 8192, true)?;
    bounded(&input.handoff, "handoff", 16384, false)?;
    if !["code", "general"].contains(&input.kind.as_str()) {
        return Err(AppError::bad_request("kind must be code or general."));
    }
    let mut mutation = Mutation::begin(
        &state,
        &auth,
        &headers,
        &format!("POST /api/v1/projects/{project}/attempts/{attempt}/submissions"),
        &input,
    )
    .await?;
    if let Some(value) = mutation.replay {
        return Ok(response(value));
    }
    let subject = owned_subject(&mut mutation, &project, &attempt, input.generation).await?;
    crate::objectives::ensure_required_children_done(&mut mutation.tx, &project, &subject.task_id)
        .await?;
    if subject.kind != input.kind {
        return Err(AppError::bad_request(
            "Submission kind must match the subject task kind.",
        ));
    }
    if subject.task_revision != input.task_revision
        || subject.project_policy_revision != input.project_policy_revision
    {
        return Err(AppError::conflict(
            "pinned_revision_changed",
            "The submission must use the task and policy revisions pinned by its claim.",
        ));
    }
    let current = sqlx::query("SELECT t.revision,p.policy_revision,p.repository_url,p.target_branch FROM tasks t JOIN projects p ON p.id=t.project_id WHERE t.project_id=? AND t.id=?")
        .bind(&project).bind(&subject.task_id).fetch_one(&mut *mutation.tx).await?;
    if current.get::<i64, _>("revision") != input.task_revision
        || current.get::<i64, _>("policy_revision") != input.project_policy_revision
    {
        return Err(AppError::conflict(
            "policy_changed",
            "Task or project policy changed after this attempt was claimed. Release and claim a current revision.",
        ));
    }
    validate_acceptance(&input, &subject.acceptance_json)?;
    crate::jobs::ensure_attempt_quiescent(&mut mutation.tx, &project, &subject.task_id).await?;
    ensure_workflow_quiescent(&mut mutation.tx, &project, &subject.task_id).await?;

    let mut canonical: Option<String> = None;
    let repository = current.get::<String, _>("repository_url");
    let target = current.get::<String, _>("target_branch");
    if input.kind == "code" {
        let policy = workflow_policy_value(&mut mutation.tx, &project).await?;
        if policy["revision"] != input.workflow_policy_revision {
            return Err(AppError::conflict(
                "workflow_policy_changed",
                "Read the current required-check roster and claim a current revision.",
            ));
        }
        if input.repository.as_deref() != Some(repository.as_str()) {
            return Err(AppError::conflict(
                "repository_mismatch",
                "The submission repository must exactly match the configured project repository.",
            ));
        }
        for (value, name) in [
            (input.base_revision.as_deref(), "base_revision"),
            (input.candidate_revision.as_deref(), "candidate_revision"),
            (input.candidate_tree.as_deref(), "candidate_tree"),
        ] {
            revision(
                value.ok_or_else(|| AppError::bad_request(&format!("{name} is required.")))?,
                name,
            )?;
        }
        let checkout =
            sqlx::query("SELECT base_revision FROM checkouts WHERE project_id=? AND attempt_id=?")
                .bind(&project)
                .bind(&attempt)
                .fetch_optional(&mut *mutation.tx)
                .await?
                .ok_or_else(|| {
                    AppError::conflict(
                        "checkout_required",
                        "Register the isolated implementation checkout before submitting code.",
                    )
                })?;
        if checkout.get::<String, _>("base_revision") != input.base_revision.as_deref().unwrap() {
            return Err(AppError::conflict(
                "base_revision_mismatch",
                "The submission base must match the registered implementation checkout.",
            ));
        }
        canonical = policy["canonical_repository_key"]
            .as_str()
            .map(str::to_owned);
    } else if input.workflow_policy_revision != 0
        || input.repository.is_some()
        || input.base_revision.is_some()
        || input.candidate_revision.is_some()
        || input.candidate_tree.is_some()
    {
        return Err(AppError::bad_request(
            "General submissions use workflow_policy_revision 0 and omit Git fields.",
        ));
    }

    let previous = sqlx::query("SELECT current_submission_id,phase FROM workflow_subjects WHERE project_id=? AND task_id=?")
        .bind(&project).bind(&subject.task_id).fetch_optional(&mut *mutation.tx).await?;
    if previous
        .as_ref()
        .is_some_and(|r| r.get::<String, _>("phase") != "revision_needed")
    {
        return Err(AppError::conflict(
            "submission_current",
            "This task already has a current submission in workflow.",
        ));
    }
    if let Some(row) = previous.as_ref() {
        sqlx::query("UPDATE submissions SET superseded_at=? WHERE id=? AND superseded_at IS NULL")
            .bind(mutation.now)
            .bind(row.get::<String, _>("current_submission_id"))
            .execute(&mut *mutation.tx)
            .await?;
    }
    let submission = Uuid::new_v4().to_string();
    let owner_session = session(&mutation.actor)?.to_owned();
    record_contributor(
        &mut mutation.tx,
        &subject.task_id,
        &mutation.actor.id,
        &owner_session,
        mutation.now,
    )
    .await?;
    sqlx::query("INSERT INTO submissions(id,project_id,task_id,attempt_id,kind,task_revision,project_policy_revision,workflow_policy_revision,summary,acceptance_evidence_json,handoff,canonical_repository_key,repository_url,target_branch,base_revision,candidate_revision,candidate_tree,created_by,contributor_session_id,created_at) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)")
        .bind(&submission).bind(&project).bind(&subject.task_id).bind(&attempt).bind(&input.kind)
        .bind(input.task_revision).bind(input.project_policy_revision).bind(input.workflow_policy_revision)
        .bind(&input.summary).bind(serde_json::to_string(&input.acceptance_evidence)?).bind(&input.handoff)
        .bind(&canonical).bind(if input.kind=="code" {Some(repository.as_str())} else {None})
        .bind(if input.kind=="code" {Some(target.as_str())} else {None})
        .bind(&input.base_revision).bind(&input.candidate_revision).bind(&input.candidate_tree)
        .bind(&mutation.actor.id).bind(&owner_session).bind(mutation.now).execute(&mut *mutation.tx).await?;
    let lessons = crate::knowledge::insert_submission_lessons(
        &mut mutation.tx,
        &mutation.actor,
        mutation.now,
        &project,
        &subject.task_id,
        &submission,
        &input.lessons,
    )
    .await?;
    for lesson in lessons {
        sqlx::query("INSERT INTO submission_knowledge(project_id,submission_id,knowledge_id,knowledge_revision) VALUES(?,?,?,1)")
            .bind(&project).bind(&submission).bind(lesson["id"].as_str().ok_or_else(|| AppError::bad_request("Lesson creation omitted its identity."))?)
            .execute(&mut *mutation.tx).await?;
    }
    crate::artifacts::validate_submission_artifacts(
        &mut mutation.tx,
        &project,
        &input.artifact_ids,
        mutation.now,
    )
    .await?;
    crate::artifacts::link_submission_artifacts(
        &mut mutation.tx,
        &project,
        &submission,
        &input.artifact_ids,
        mutation.now,
    )
    .await?;
    sqlx::query("UPDATE attempts SET state='submitted',ended_at=?,outcome=? WHERE id=?")
        .bind(mutation.now)
        .bind(&input.summary)
        .bind(&attempt)
        .execute(&mut *mutation.tx)
        .await?;
    sqlx::query("UPDATE tasks SET current_attempt_id=NULL,blocked_reason=? WHERE id=?")
        .bind("An immutable submission is in the completion workflow.")
        .bind(&subject.task_id)
        .execute(&mut *mutation.tx)
        .await?;

    let need_agent = matches!(subject.review_mode.as_str(), "agent" | "both");
    let need_human = matches!(subject.review_mode.as_str(), "human" | "both");
    let phase = if need_agent || need_human {
        "review"
    } else if input.kind == "code" {
        "integration"
    } else {
        "done"
    };
    sqlx::query("INSERT INTO workflow_subjects(project_id,task_id,current_submission_id,phase,updated_at) VALUES(?,?,?,?,?) ON CONFLICT(task_id) DO UPDATE SET current_submission_id=excluded.current_submission_id,phase=excluded.phase,updated_at=excluded.updated_at")
        .bind(&project).bind(&subject.task_id).bind(&submission).bind(phase).bind(mutation.now)
        .execute(&mut *mutation.tx).await?;
    if need_agent {
        create_activity(
            &mut mutation.tx,
            &project,
            &subject,
            &submission,
            "agent_review",
            1,
            mutation.now,
        )
        .await?;
    }
    if need_human {
        create_activity(
            &mut mutation.tx,
            &project,
            &subject,
            &submission,
            "human_review",
            1,
            mutation.now,
        )
        .await?;
    }
    if input.kind == "code" {
        let integration_activity = create_activity(
            &mut mutation.tx,
            &project,
            &subject,
            &submission,
            "integration",
            1,
            mutation.now,
        )
        .await?;
        if need_agent || need_human {
            sqlx::query("UPDATE tasks SET blocked_reason='Required reviews are pending.' WHERE id=(SELECT activity_task_id FROM workflow_activities WHERE id=?)").bind(integration_activity).execute(&mut *mutation.tx).await?;
        }
    } else if phase == "done" {
        sqlx::query("UPDATE tasks SET lifecycle='done',blocked_reason=NULL WHERE id=?")
            .bind(&subject.task_id)
            .execute(&mut *mutation.tx)
            .await?;
        ready_dependents(&mut mutation.tx, &subject.task_id, mutation.now).await?;
    }
    let value =
        workflow_snapshot(&mut mutation.tx, &project, &subject.task_id, mutation.now).await?;
    Ok(response(
        mutation
            .finish(value, Some(&project), "submission.created", &submission)
            .await?,
    ))
}

async fn submission_value(c: &mut SqliteConnection, id: &str) -> Result<Value, AppError> {
    let row = sqlx::query("SELECT * FROM submissions WHERE id=?")
        .bind(id)
        .fetch_optional(&mut *c)
        .await?
        .ok_or_else(AppError::not_found)?;
    let lesson_ids = sqlx::query_scalar::<_, String>(
        "SELECT knowledge_id FROM submission_knowledge WHERE submission_id=? ORDER BY knowledge_id",
    )
    .bind(id)
    .fetch_all(&mut *c)
    .await?;
    let lessons = sqlx::query("SELECT k.knowledge_id,k.revision,k.title,k.body,k.status,k.provenance_json FROM submission_knowledge sk JOIN knowledge_revisions k ON k.knowledge_id=sk.knowledge_id AND k.revision=sk.knowledge_revision WHERE sk.submission_id=? ORDER BY k.knowledge_id")
        .bind(id).fetch_all(&mut *c).await?;
    let lessons = lessons.iter().map(|r| Ok(json!({"id":r.get::<String,_>("knowledge_id"),"revision":r.get::<i64,_>("revision"),"title":r.get::<String,_>("title"),"body":r.get::<String,_>("body"),"status":r.get::<String,_>("status"),"provenance":serde_json::from_str::<Value>(&r.get::<String,_>("provenance_json"))?}))).collect::<Result<Vec<_>,serde_json::Error>>()?;
    let artifact_ids = sqlx::query_scalar::<_, String>(
        "SELECT artifact_id FROM submission_artifacts WHERE submission_id=? ORDER BY artifact_id",
    )
    .bind(id)
    .fetch_all(&mut *c)
    .await?;
    Ok(json!({
        "id":row.get::<String,_>("id"),"task_id":row.get::<String,_>("task_id"),
        "attempt_id":row.get::<String,_>("attempt_id"),"kind":row.get::<String,_>("kind"),
        "task_revision":row.get::<i64,_>("task_revision"),
        "project_policy_revision":row.get::<i64,_>("project_policy_revision"),
        "workflow_policy_revision":row.get::<i64,_>("workflow_policy_revision"),
        "summary":row.get::<String,_>("summary"),
        "acceptance_evidence":serde_json::from_str::<Value>(&row.get::<String,_>("acceptance_evidence_json"))?,
        "handoff":row.get::<String,_>("handoff"),
        "lesson_ids":lesson_ids,"lessons":lessons,"artifact_ids":artifact_ids,
        "canonical_repository_key":row.get::<Option<String>,_>("canonical_repository_key"),
        "repository":row.get::<Option<String>,_>("repository_url"),
        "target_branch":row.get::<Option<String>,_>("target_branch"),
        "base_revision":row.get::<Option<String>,_>("base_revision"),
        "candidate_revision":row.get::<Option<String>,_>("candidate_revision"),
        "candidate_tree":row.get::<Option<String>,_>("candidate_tree"),
        "created_by":row.get::<String,_>("created_by"),
        "created_at":timestamp(row.get("created_at")),
        "superseded_at":row.get::<Option<i64>,_>("superseded_at").map(timestamp)
    }))
}

async fn activity_value(
    c: &mut SqliteConnection,
    project: &str,
    id: &str,
    now: i64,
) -> Result<Value, AppError> {
    let row = sqlx::query("SELECT wa.*,t.current_attempt_id,a.owner_id,a.session_id,a.generation,a.state AS attempt_state,a.expires_at,a.created_at AS attempt_created_at, \
        COALESCE(owner.disabled_at IS NULL AND owner.id IS NOT NULL AND CASE WHEN a.credential_id IS NULL THEN bs.id IS NOT NULL AND bs.revoked_at IS NULL AND bs.expires_at>? ELSE cr.id IS NOT NULL AND cr.revoked_at IS NULL AND (cr.expires_at IS NULL OR cr.expires_at>?) AND ag.id IS NOT NULL AND ag.closed_at IS NULL END,0) AS owner_authorized \
        FROM workflow_activities wa JOIN tasks t ON t.id=wa.activity_task_id LEFT JOIN attempts a ON a.id=t.current_attempt_id \
        LEFT JOIN principals owner ON owner.id=a.owner_id LEFT JOIN credentials cr ON cr.id=a.credential_id \
        LEFT JOIN agent_sessions ag ON ag.id=a.session_id AND ag.credential_id=a.credential_id LEFT JOIN browser_sessions bs ON bs.id=a.session_id \
        WHERE wa.project_id=? AND wa.id=?")
        .bind(now).bind(now).bind(project).bind(id).fetch_optional(&mut *c).await?.ok_or_else(AppError::not_found)?;
    let attempt = row.get::<Option<String>,_>("current_attempt_id").map(|attempt_id| json!({
        "id":attempt_id,"owner_id":row.get::<String,_>("owner_id"),"session_id":row.get::<String,_>("session_id"),
        "generation":row.get::<i64,_>("generation"),"state":row.get::<String,_>("attempt_state"),
        "expires_at":timestamp(row.get("expires_at")),"valid_by_time":row.get::<i64,_>("expires_at")>now,
        "owner_authorized":row.get::<bool,_>("owner_authorized")
    }));
    let review_row = sqlx::query("SELECT decision,summary,reviewer_id,reviewer_session_id,created_at FROM review_decisions WHERE activity_id=?")
        .bind(id).fetch_optional(&mut *c).await?;
    let review = if let Some(r) = review_row {
        let findings=sqlx::query("SELECT id,severity,remedy,evidence,created_at FROM review_findings WHERE activity_id=? ORDER BY created_at,id LIMIT 101")
            .bind(id).fetch_all(&mut *c).await?;
        let truncated = findings.len() > 100;
        let findings=findings.into_iter().take(100).map(|f|json!({"id":f.get::<String,_>("id"),"severity":f.get::<String,_>("severity"),"remedy":f.get::<String,_>("remedy"),"evidence":f.get::<String,_>("evidence"),"created_at":timestamp(f.get("created_at"))})).collect::<Vec<_>>();
        Some(
            json!({"decision":r.get::<String,_>("decision"),"summary":r.get::<String,_>("summary"),"reviewer_id":r.get::<String,_>("reviewer_id"),"reviewer_session_id":r.get::<String,_>("reviewer_session_id"),"created_at":timestamp(r.get("created_at")),"findings":findings,"findings_truncated":truncated}),
        )
    } else {
        None
    };
    let clock_ready: bool =
        sqlx::query_scalar("SELECT status='ready' FROM clock_state WHERE singleton=1")
            .fetch_one(&mut *c)
            .await?;
    let authorization = sqlx::query("SELECT actor_id,summary,created_at,invalidated_at,authorization_revision FROM integration_authorizations WHERE activity_id=?")
        .bind(id).fetch_optional(&mut *c).await?.map(|r| {
            let record_valid = r.get::<Option<i64>, _>("invalidated_at").is_none();
            json!({"actor_id":r.get::<String,_>("actor_id"),"summary":r.get::<String,_>("summary"),"created_at":timestamp(r.get("created_at")),"invalidated_at":r.get::<Option<i64>,_>("invalidated_at").map(timestamp),"valid":record_valid && clock_ready,"validity_reason":if !clock_ready { Some("clock_reconciliation_required") } else if !record_valid { Some("authorization_invalidated") } else { None },"revision":r.get::<i64,_>("authorization_revision")})
        });
    let intent = sqlx::query("SELECT observed_target_revision,observed_target_tree,result_revision,result_tree,created_by,created_at FROM publication_intents WHERE activity_id=?")
        .bind(id).fetch_optional(&mut *c).await?.map(|r|json!({"observed_target_revision":r.get::<String,_>("observed_target_revision"),"observed_target_tree":r.get::<String,_>("observed_target_tree"),"result_revision":r.get::<String,_>("result_revision"),"result_tree":r.get::<String,_>("result_tree"),"created_by":r.get::<String,_>("created_by"),"created_at":timestamp(r.get("created_at"))}));
    let result = sqlx::query("SELECT publication_state,observed_target_revision,result_revision,result_tree,check_job_ids_json,summary,created_at FROM integration_results WHERE activity_id=?")
        .bind(id).fetch_optional(&mut *c).await?.map(|r|json!({"publication_state":r.get::<String,_>("publication_state"),"observed_target_revision":r.get::<String,_>("observed_target_revision"),"result_revision":r.get::<String,_>("result_revision"),"result_tree":r.get::<String,_>("result_tree"),"check_job_ids":serde_json::from_str::<Value>(&r.get::<String,_>("check_job_ids_json")).unwrap_or(json!([])),"summary":r.get::<String,_>("summary"),"created_at":timestamp(r.get("created_at"))}));
    let hold = sqlx::query("SELECT id,canonical_repository_key,target_branch,state,acquired_at,released_at,release_reason FROM integration_holds WHERE activity_id=?")
        .bind(id).fetch_optional(&mut *c).await?.map(|r|json!({"id":r.get::<String,_>("id"),"canonical_repository_key":r.get::<String,_>("canonical_repository_key"),"target_branch":r.get::<String,_>("target_branch"),"state":r.get::<String,_>("state"),"acquired_at":timestamp(r.get("acquired_at")),"released_at":r.get::<Option<i64>,_>("released_at").map(timestamp),"release_reason":r.get::<Option<String>,_>("release_reason")}));
    let reconciliation=sqlx::query("SELECT disposition,observed_target_revision,observed_target_tree,evidence,actor_id,created_at FROM publication_reconciliations WHERE activity_id=?")
        .bind(id).fetch_optional(&mut *c).await?.map(|r|json!({"disposition":r.get::<String,_>("disposition"),"observed_target_revision":r.get::<String,_>("observed_target_revision"),"observed_target_tree":r.get::<String,_>("observed_target_tree"),"evidence":r.get::<String,_>("evidence"),"actor_id":r.get::<String,_>("actor_id"),"created_at":timestamp(r.get("created_at"))}));
    let persisted = row.get::<String, _>("state");
    let derived = if persisted == "active"
        && attempt
            .as_ref()
            .is_some_and(|a| a["valid_by_time"] != true || a["owner_authorized"] != true)
    {
        "recovery_required"
    } else {
        persisted.as_str()
    };
    Ok(
        json!({"id":row.get::<String,_>("id"),"kind":row.get::<String,_>("kind"),"slot":row.get::<i64,_>("slot"),
        "activity_task_id":row.get::<String,_>("activity_task_id"),"subject_task_id":row.get::<String,_>("subject_task_id"),
        "submission_id":row.get::<String,_>("submission_id"),"status":derived,
        "current_attempt":attempt,"review":review,"authorization":authorization,"intent":intent,"result":result,"publication_reconciliation":reconciliation,"hold":hold}),
    )
}

pub async fn workflow_snapshot(
    c: &mut SqliteConnection,
    project: &str,
    task_id: &str,
    now: i64,
) -> Result<Value, AppError> {
    let mapped = sqlx::query_scalar::<_, String>(
        "SELECT subject_task_id FROM workflow_activities WHERE project_id=? AND activity_task_id=?",
    )
    .bind(project)
    .bind(task_id)
    .fetch_optional(&mut *c)
    .await?;
    let task_id = mapped.as_deref().unwrap_or(task_id);
    let subject = sqlx::query("SELECT current_submission_id,phase FROM workflow_subjects WHERE project_id=? AND task_id=?")
        .bind(project).bind(task_id).fetch_optional(&mut *c).await?;
    let Some(subject) = subject else {
        return Ok(json!({"submission":null,"activities":[],"blockers":[],"next_actions":[]}));
    };
    let submission_id: String = subject.get("current_submission_id");
    let phase: String = subject.get("phase");
    let rows = sqlx::query("SELECT id FROM workflow_activities WHERE project_id=? AND submission_id=? ORDER BY kind,slot DESC,id LIMIT 101")
        .bind(project).bind(&submission_id).fetch_all(&mut *c).await?;
    let activities_truncated = rows.len() > 100;
    let mut activities = Vec::new();
    for row in rows.into_iter().take(100) {
        activities.push(activity_value(c, project, &row.get::<String, _>("id"), now).await?);
    }
    let mut blockers = Vec::new();
    let sub = submission_value(c, &submission_id).await?;
    let policy: Option<i64> = sqlx::query_scalar("SELECT policy_revision FROM projects WHERE id=?")
        .bind(project)
        .fetch_optional(&mut *c)
        .await?;
    if policy != sub["project_policy_revision"].as_i64() {
        blockers.push("Project policy changed after submission.".to_string());
    }
    if sub["kind"] == "code" {
        let wp: Option<i64> =
            sqlx::query_scalar("SELECT revision FROM workflow_policies WHERE project_id=?")
                .bind(project)
                .fetch_optional(&mut *c)
                .await?;
        if wp != sub["workflow_policy_revision"].as_i64() {
            blockers.push("Required-check policy changed after submission.".to_string());
        }
    }
    let integration = activities
        .iter()
        .filter(|a| a["kind"] == "integration")
        .max_by_key(|a| a["slot"].as_i64().unwrap_or(0));
    let work_status = match phase.as_str() {
        "review" => "waiting_review",
        "revision_needed" => "ready",
        "done" => "done",
        "integration" => match integration {
            Some(a)
                if a["status"] == "recovery_required"
                    || a.pointer("/result/publication_state") == Some(&json!("uncertain")) =>
            {
                "recovery_required"
            }
            Some(a) if a.pointer("/result/publication_state") == Some(&json!("published")) => {
                "validating"
            }
            Some(a) if a.pointer("/current_attempt/valid_by_time") == Some(&json!(true)) => {
                "integrating"
            }
            _ => "waiting_integration",
        },
        _ => "blocked",
    };
    let next_actions = match phase.as_str() {
        "review" => vec!["Claim and decide each required review activity."],
        "integration" => vec!["Authorize if required, then claim the integration activity."],
        "revision_needed" => vec!["Claim the subject task and prepare a new revision."],
        _ => Vec::new(),
    };
    Ok(
        json!({"submission":sub,"activities":activities,"activities_truncated":activities_truncated,"blockers":blockers,"next_actions":next_actions,"phase":phase,"work_status":work_status}),
    )
}

async fn task_workflow(
    State(state): State<AppState>,
    _auth: Auth,
    Path((project, task)): Path<(String, String)>,
) -> Reply {
    let mut c = state.pool.acquire().await?;
    project_exists(&mut c, &project).await?;
    Ok(response(
        workflow_snapshot(&mut c, &project, &task, state.now()).await?,
    ))
}

async fn reopen(
    State(state): State<AppState>,
    auth: Auth,
    Path((project, task)): Path<(String, String)>,
    headers: HeaderMap,
    body: Result<Json<ReopenSubmissionInput>, JsonRejection>,
) -> Reply {
    let input = payload(body)?;
    bounded(&input.reason, "reason", 8192, true)?;
    let mut m = Mutation::begin(
        &state,
        &auth,
        &headers,
        &format!("POST /api/v1/projects/{project}/tasks/{task}/workflow/reopen"),
        &input,
    )
    .await?;
    human(&m.actor)?;
    let subject=sqlx::query("SELECT current_submission_id,phase FROM workflow_subjects WHERE project_id=? AND task_id=?").bind(&project).bind(&task).fetch_optional(&mut *m.tx).await?.ok_or_else(AppError::not_found)?;
    if let Some(v) = m.replay {
        return Ok(response(v));
    }
    if subject.get::<String, _>("current_submission_id") != input.submission_id {
        return Err(AppError::conflict(
            "submission_changed",
            "Reopen must name the exact current submission.",
        ));
    }
    if subject.get::<String, _>("phase") == "done" {
        return Err(AppError::conflict(
            "workflow_complete",
            "A completed workflow cannot be reopened.",
        ));
    }
    let effects:i64=sqlx::query_scalar("SELECT count(*) FROM workflow_activities wa JOIN publication_intents pi ON pi.activity_id=wa.id LEFT JOIN publication_reconciliations pr ON pr.activity_id=wa.id WHERE wa.submission_id=? AND (pr.activity_id IS NULL OR pr.disposition!='not_published')")
        .bind(&input.submission_id).fetch_one(&mut *m.tx).await?;
    if effects > 0 {
        return Err(AppError::conflict(
            "publication_reconciliation_required",
            "Publication intent or result exists. Reconcile that external effect before reopening revision work.",
        ));
    }
    let stale = !workflow_snapshot(&mut m.tx, &project, &task, m.now).await?["blockers"]
        .as_array()
        .is_some_and(Vec::is_empty);
    let activity_ids=sqlx::query_scalar::<_,String>("SELECT wa.id FROM workflow_activities wa JOIN tasks t ON t.id=wa.activity_task_id WHERE wa.submission_id=? AND t.current_attempt_id IS NOT NULL")
        .bind(&input.submission_id).fetch_all(&mut *m.tx).await?;
    if !stale {
        for activity_id in &activity_ids {
            let value = activity_value(&mut m.tx, &project, activity_id, m.now).await?;
            if value["current_attempt"]["valid_by_time"] == true
                && value["current_attempt"]["owner_authorized"] == true
            {
                return Err(AppError::conflict(
                    "activity_owned",
                    "Release every live current workflow activity before reopening.",
                ));
            }
        }
    }
    crate::jobs::ensure_attempt_quiescent(&mut m.tx, &project, &task).await?;
    ensure_workflow_quiescent(&mut m.tx, &project, &task).await?;
    sqlx::query("UPDATE attempts SET state='canceled',ended_at=?,outcome='Human reopened expired, revoked, or stale workflow authority.' WHERE id IN (SELECT t.current_attempt_id FROM workflow_activities wa JOIN tasks t ON t.id=wa.activity_task_id WHERE wa.submission_id=? AND t.current_attempt_id IS NOT NULL) AND state='active'")
        .bind(m.now).bind(&input.submission_id).execute(&mut *m.tx).await?;
    sqlx::query("UPDATE tasks SET current_attempt_id=NULL WHERE id IN (SELECT activity_task_id FROM workflow_activities WHERE submission_id=?)")
        .bind(&input.submission_id).execute(&mut *m.tx).await?;
    sqlx::query("UPDATE submissions SET superseded_at=? WHERE id=? AND superseded_at IS NULL")
        .bind(m.now)
        .bind(&input.submission_id)
        .execute(&mut *m.tx)
        .await?;
    sqlx::query(
        "UPDATE workflow_subjects SET phase='revision_needed',updated_at=? WHERE task_id=?",
    )
    .bind(m.now)
    .bind(&task)
    .execute(&mut *m.tx)
    .await?;
    sqlx::query("UPDATE workflow_activities SET state='canceled',canceled_at=? WHERE submission_id=? AND state IN ('queued','active')").bind(m.now).bind(&input.submission_id).execute(&mut *m.tx).await?;
    sqlx::query("UPDATE tasks SET lifecycle='canceled',blocked_reason=NULL WHERE id IN (SELECT activity_task_id FROM workflow_activities WHERE submission_id=? AND state='canceled')")
        .bind(&input.submission_id).execute(&mut *m.tx).await?;
    sqlx::query("UPDATE integration_holds SET state='released',released_by=?,released_at=?,release_reason=? WHERE activity_id IN (SELECT id FROM workflow_activities WHERE submission_id=?) AND state='held'")
        .bind(&m.actor.id).bind(m.now).bind(&input.reason).bind(&input.submission_id).execute(&mut *m.tx).await?;
    sqlx::query("UPDATE tasks SET blocked_reason=NULL,ready_since=? WHERE id=?")
        .bind(m.now)
        .bind(&task)
        .execute(&mut *m.tx)
        .await?;
    let value = workflow_snapshot(&mut m.tx, &project, &task, m.now).await?;
    Ok(response(
        m.finish(
            value,
            Some(&project),
            "submission.reopened",
            &input.submission_id,
        )
        .await?,
    ))
}

pub async fn guard_normal_claim(
    c: &mut SqliteConnection,
    project: &str,
    task_id: &str,
) -> Result<(), AppError> {
    if sqlx::query_scalar::<_, i64>(
        "SELECT count(*) FROM workflow_activities WHERE project_id=? AND activity_task_id=?",
    )
    .bind(project)
    .bind(task_id)
    .fetch_one(&mut *c)
    .await?
        > 0
    {
        return Err(AppError::conflict(
            "workflow_claim_required",
            "Claim linked review and integration work through its workflow activity.",
        ));
    }
    if let Some(phase) = sqlx::query_scalar::<_, String>(
        "SELECT phase FROM workflow_subjects WHERE project_id=? AND task_id=?",
    )
    .bind(project)
    .bind(task_id)
    .fetch_optional(&mut *c)
    .await?
        && phase != "revision_needed"
    {
        return Err(AppError::conflict(
            "submission_current",
            "This task has an immutable submission in the completion workflow.",
        ));
    }
    Ok(())
}

pub async fn guard_subject_mutation(
    c: &mut SqliteConnection,
    project: &str,
    task_id: &str,
) -> Result<(), AppError> {
    if sqlx::query_scalar::<_, i64>(
        "SELECT count(*) FROM workflow_activities WHERE project_id=? AND activity_task_id=?",
    )
    .bind(project)
    .bind(task_id)
    .fetch_one(&mut *c)
    .await?
        > 0
    {
        return Err(AppError::conflict(
            "workflow_managed_task",
            "Linked workflow activity tasks cannot be edited directly.",
        ));
    }
    if let Some(phase) = sqlx::query_scalar::<_, String>(
        "SELECT phase FROM workflow_subjects WHERE project_id=? AND task_id=?",
    )
    .bind(project)
    .bind(task_id)
    .fetch_optional(&mut *c)
    .await?
        && phase != "revision_needed"
    {
        return Err(AppError::conflict(
            "submission_current",
            "Reopen or finish the current submission before editing task requirements.",
        ));
    }
    Ok(())
}

pub async fn guard_release_or_recovery(
    c: &mut SqliteConnection,
    project: &str,
    task_id: &str,
) -> Result<(), AppError> {
    if sqlx::query_scalar::<_, i64>(
        "SELECT count(*) FROM workflow_activities WHERE project_id=? AND activity_task_id=?",
    )
    .bind(project)
    .bind(task_id)
    .fetch_one(&mut *c)
    .await?
        > 0
    {
        return Err(AppError::conflict(
            "workflow_release_required",
            "Release this owned activity through its workflow endpoint.",
        ));
    }
    if let Some(phase) = sqlx::query_scalar::<_, String>(
        "SELECT phase FROM workflow_subjects WHERE project_id=? AND task_id=?",
    )
    .bind(project)
    .bind(task_id)
    .fetch_optional(&mut *c)
    .await?
        && phase != "revision_needed"
    {
        return Err(AppError::conflict(
            "workflow_authority_required",
            "This task is controlled by its current workflow activities.",
        ));
    }
    Ok(())
}

pub async fn record_contributor(
    c: &mut SqliteConnection,
    task_id: &str,
    actor_id: &str,
    session_id: &str,
    now: i64,
) -> Result<(), AppError> {
    sqlx::query("INSERT OR IGNORE INTO task_contributors(task_id,principal_id,session_id,first_contributed_at) VALUES(?,?,?,?)")
        .bind(task_id).bind(actor_id).bind(session_id).bind(now).execute(&mut *c).await?;
    Ok(())
}

pub async fn guard_activity_work(
    c: &mut SqliteConnection,
    project: &str,
    activity_task_id: &str,
    now: i64,
) -> Result<(), AppError> {
    crate::knowledge::ensure_decisions_resolved(c, project, activity_task_id, now).await?;
    let row=sqlx::query("SELECT wa.subject_task_id,wa.id,wa.state,wa.submission_id,ws.current_submission_id,s.project_policy_revision,s.workflow_policy_revision,p.policy_revision,wp.revision AS current_workflow_revision,t.current_attempt_id,a.expires_at FROM workflow_activities wa JOIN workflow_subjects ws ON ws.task_id=wa.subject_task_id JOIN submissions s ON s.id=wa.submission_id JOIN projects p ON p.id=wa.project_id LEFT JOIN workflow_policies wp ON wp.project_id=wa.project_id JOIN tasks t ON t.id=wa.activity_task_id LEFT JOIN attempts a ON a.id=t.current_attempt_id WHERE wa.project_id=? AND wa.activity_task_id=?")
        .bind(project).bind(activity_task_id).fetch_optional(&mut *c).await?;
    let Some(row) = row else { return Ok(()) };
    crate::knowledge::ensure_decisions_resolved(
        c,
        project,
        &row.get::<String, _>("subject_task_id"),
        now,
    )
    .await?;
    if row.get::<String, _>("state") != "active"
        || row.get::<String, _>("submission_id") != row.get::<String, _>("current_submission_id")
        || row.get::<i64, _>("project_policy_revision") != row.get::<i64, _>("policy_revision")
        || (row.get::<i64, _>("workflow_policy_revision") > 0
            && row.get::<Option<i64>, _>("current_workflow_revision")
                != Some(row.get::<i64, _>("workflow_policy_revision")))
        || row.get::<Option<String>, _>("current_attempt_id").is_none()
        || row
            .get::<Option<i64>, _>("expires_at")
            .is_none_or(|v| v <= now)
    {
        return Err(AppError::conflict(
            "workflow_authority_lost",
            "The activity candidate, policy, or lease is no longer current.",
        ));
    }
    if sqlx::query_scalar::<_, i64>("SELECT count(*) FROM integration_results WHERE activity_id=?")
        .bind(row.get::<String, _>("id"))
        .fetch_one(&mut *c)
        .await?
        > 0
    {
        return Err(AppError::conflict(
            "integration_result_recorded",
            "This activity already has an immutable integration result.",
        ));
    }
    Ok(())
}

struct ActivityContext {
    id: String,
    kind: String,
    subject_task: String,
    submission: String,
    activity_task: String,
    state: String,
    phase: String,
    superseded_at: Option<i64>,
    project_policy_revision: i64,
    workflow_policy_revision: i64,
    current_submission: String,
    project_current_policy: i64,
    workflow_current_policy: Option<i64>,
    automatic_integration: bool,
    recovery_mode: String,
    lease_seconds: i64,
    canonical_repository_key: Option<String>,
    target_branch: Option<String>,
}

async fn activity_context(
    c: &mut SqliteConnection,
    project: &str,
    id: &str,
) -> Result<ActivityContext, AppError> {
    let r=sqlx::query("SELECT wa.id,wa.kind,wa.subject_task_id,wa.submission_id,wa.activity_task_id,wa.state,ws.current_submission_id,ws.phase,s.superseded_at,s.task_revision,s.project_policy_revision,s.workflow_policy_revision,s.canonical_repository_key,s.target_branch,p.policy_revision,p.review_mode,p.automatic_integration,p.recovery_mode,p.lease_seconds,wp.revision AS current_workflow_revision FROM workflow_activities wa JOIN workflow_subjects ws ON ws.task_id=wa.subject_task_id JOIN submissions s ON s.id=wa.submission_id JOIN projects p ON p.id=wa.project_id LEFT JOIN workflow_policies wp ON wp.project_id=wa.project_id WHERE wa.project_id=? AND wa.id=?")
        .bind(project).bind(id).fetch_optional(&mut *c).await?.ok_or_else(AppError::not_found)?;
    Ok(ActivityContext {
        id: r.get("id"),
        kind: r.get("kind"),
        subject_task: r.get("subject_task_id"),
        submission: r.get("submission_id"),
        activity_task: r.get("activity_task_id"),
        state: r.get("state"),
        phase: r.get("phase"),
        superseded_at: r.get("superseded_at"),
        project_policy_revision: r.get("project_policy_revision"),
        workflow_policy_revision: r.get("workflow_policy_revision"),
        current_submission: r.get("current_submission_id"),
        project_current_policy: r.get("policy_revision"),
        workflow_current_policy: r.get("current_workflow_revision"),
        automatic_integration: r.get("automatic_integration"),
        recovery_mode: r.get("recovery_mode"),
        lease_seconds: r.get("lease_seconds"),
        canonical_repository_key: r.get("canonical_repository_key"),
        target_branch: r.get("target_branch"),
    })
}

fn ensure_current(a: &ActivityContext) -> Result<(), AppError> {
    if a.submission != a.current_submission
        || a.superseded_at.is_some()
        || a.phase == "revision_needed"
        || a.project_policy_revision != a.project_current_policy
        || (a.workflow_policy_revision > 0
            && a.workflow_current_policy != Some(a.workflow_policy_revision))
    {
        return Err(AppError::conflict(
            "workflow_policy_changed",
            "The candidate or its pinned policy is no longer current.",
        ));
    }
    if matches!(
        a.state.as_str(),
        "completed" | "canceled" | "recovery_required"
    ) {
        return Err(AppError::conflict(
            "activity_not_eligible",
            "This workflow activity cannot grant new work authority.",
        ));
    }
    Ok(())
}

async fn activity_detail(
    State(state): State<AppState>,
    auth: Auth,
    Path((project, id)): Path<(String, String)>,
) -> Reply {
    let mut c = state.pool.begin().await?;
    let now = state.now();
    let mut value = activity_value(&mut c, &project, &id, now).await?;
    let ctx = activity_context(&mut c, &project, &id).await?;
    let current = value["current_attempt"].clone();
    let owned = current["owner_id"] == auth.actor.id
        && current["session_id"].as_str() == auth.actor.session_id.as_deref()
        && current["valid_by_time"] == true
        && current["owner_authorized"] == true;
    let policy_current = ctx.submission == ctx.current_submission
        && ctx.project_policy_revision == ctx.project_current_policy
        && (ctx.workflow_policy_revision == 0
            || ctx.workflow_current_policy == Some(ctx.workflow_policy_revision));
    let decisions_ready =
        crate::knowledge::pending_decision_ids(&mut c, &project, Some(&ctx.subject_task), now, 1)
            .await?
            .is_empty()
            && crate::knowledge::pending_decision_ids(
                &mut c,
                &project,
                Some(&ctx.activity_task),
                now,
                1,
            )
            .await?
            .is_empty();
    let valid = owned && policy_current && decisions_ready && ctx.state == "active";
    let remaining = if valid {
        current["expires_at"]
            .as_str()
            .and_then(|v| chrono::DateTime::parse_from_rfc3339(v).ok())
            .map_or(0, |v| (v.timestamp_millis() - now).max(0))
    } else {
        0
    };
    value["current_authority"] = json!({"valid":valid,"owned":owned,"policy_current":policy_current,"decisions_ready":decisions_ready,"lease_remaining_ms":remaining,"attempt_id":current["id"],"generation":current["generation"]});
    let (publication_allowed, check_job_ids) = if valid && ctx.kind == "integration" {
        publication_readiness(&mut c, &project, &ctx, current["id"].as_str().unwrap_or("")).await?
    } else {
        (false, Vec::new())
    };
    value["publication_allowed"] = json!(publication_allowed);
    value["qualifying_check_job_ids"] = json!(check_job_ids);
    Ok(response(value))
}

async fn publication_readiness(
    c: &mut SqliteConnection,
    project: &str,
    ctx: &ActivityContext,
    attempt_id: &str,
) -> Result<(bool, Vec<String>), AppError> {
    if sqlx::query_scalar::<_, i64>("SELECT count(*) FROM integration_results WHERE activity_id=?")
        .bind(&ctx.id)
        .fetch_one(&mut *c)
        .await?
        > 0
    {
        return Ok((false, Vec::new()));
    }
    if !approvals_satisfied(c, &ctx.submission).await?
        || sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM integration_holds WHERE activity_id=? AND state='held'",
        )
        .bind(&ctx.id)
        .fetch_one(&mut *c)
        .await?
            == 0
    {
        return Ok((false, Vec::new()));
    }
    if !ctx.automatic_integration && sqlx::query_scalar::<_,i64>("SELECT count(*) FROM integration_authorizations WHERE activity_id=? AND submission_id=? AND project_policy_revision=? AND workflow_policy_revision=? AND invalidated_at IS NULL")
        .bind(&ctx.id).bind(&ctx.submission).bind(ctx.project_policy_revision).bind(ctx.workflow_policy_revision).fetch_one(&mut *c).await?==0{return Ok((false,Vec::new()));}
    let intent=sqlx::query("SELECT result_revision,result_tree FROM publication_intents WHERE activity_id=? AND attempt_id=?").bind(&ctx.id).bind(attempt_id).fetch_optional(&mut *c).await?;
    let Some(intent) = intent else {
        return Ok((false, Vec::new()));
    };
    let roster_json:String=sqlx::query_scalar("SELECT required_checks_json FROM workflow_policy_revisions WHERE project_id=? AND revision=?").bind(project).bind(ctx.workflow_policy_revision).fetch_one(&mut *c).await?;
    let roster: Vec<RequiredCheck> = serde_json::from_str(&roster_json)?;
    let result_revision: String = intent.get("result_revision");
    let result_tree: String = intent.get("result_tree");
    let mut ids = Vec::new();
    for check in roster {
        let job=sqlx::query_scalar::<_,String>("SELECT id FROM jobs WHERE project_id=? AND task_id=? AND attempt_id=? AND state='succeeded' AND exit_code=0 AND inputs_unchanged=1 AND reconciled_at IS NULL AND source_revision=? AND source_tree=? AND check_identity=? AND check_version=? AND check_environment=? ORDER BY created_at DESC,id DESC LIMIT 1")
            .bind(project).bind(&ctx.activity_task).bind(attempt_id).bind(&result_revision).bind(&result_tree)
            .bind(&check.identity).bind(&check.version).bind(&check.environment).fetch_optional(&mut *c).await?;
        let Some(job) = job else {
            return Ok((false, Vec::new()));
        };
        ids.push(job);
    }
    Ok((!ids.is_empty(), ids))
}

async fn approvals_satisfied(c: &mut SqliteConnection, submission: &str) -> Result<bool, AppError> {
    Ok(sqlx::query_scalar::<_,i64>("SELECT count(*) FROM workflow_activities wa LEFT JOIN review_decisions rd ON rd.activity_id=wa.id AND rd.decision='approved' WHERE wa.submission_id=? AND wa.kind IN ('agent_review','human_review') AND rd.activity_id IS NULL")
        .bind(submission).fetch_one(&mut *c).await?==0)
}

async fn claim_activity(
    State(state): State<AppState>,
    auth: Auth,
    Path((project, id)): Path<(String, String)>,
    headers: HeaderMap,
    body: Result<Json<ActivityClaimInput>, JsonRejection>,
) -> Reply {
    let input = payload(body)?;
    let mut m = Mutation::begin(
        &state,
        &auth,
        &headers,
        &format!("POST /api/v1/projects/{project}/workflow-activities/{id}/claim"),
        &input,
    )
    .await?;
    let owner_session = session(&m.actor)?.to_owned();
    let ctx = activity_context(&mut m.tx, &project, &id).await?;
    ensure_activity_decisions(&mut m, &project, &ctx).await?;
    if let Some(mut value) = m.replay {
        let projected = activity_value(&mut m.tx, &project, &ctx.id, m.now).await?;
        let current = &projected["current_attempt"];
        let saved = value.pointer("/attempt/id").and_then(Value::as_str);
        let saved_generation = value.pointer("/attempt/generation").and_then(Value::as_i64);
        let valid = current["id"].as_str() == saved
            && current["generation"].as_i64() == saved_generation
            && current["owner_id"] == m.actor.id
            && current["session_id"] == owner_session
            && current["state"] == "active"
            && current["valid_by_time"] == true
            && current["owner_authorized"] == true
            && projected["status"] == "active"
            && ctx.superseded_at.is_none()
            && ctx.phase != "revision_needed"
            && ctx.submission == ctx.current_submission
            && ctx.project_policy_revision == ctx.project_current_policy
            && (ctx.workflow_policy_revision == 0
                || ctx.workflow_current_policy == Some(ctx.workflow_policy_revision));
        let remaining = if valid {
            current["expires_at"]
                .as_str()
                .and_then(|v| chrono::DateTime::parse_from_rfc3339(v).ok())
                .map_or(0, |v| (v.timestamp_millis() - m.now).max(0))
        } else {
            0
        };
        value["current_authority"] = json!({"valid":valid,"lease_remaining_ms":remaining});
        value["replayed"] = json!(true);
        return Ok(response(value));
    }
    ensure_current(&ctx)?;
    if input.expected_submission_id != ctx.submission
        || input.expected_project_policy_revision != ctx.project_policy_revision
        || input.expected_workflow_policy_revision != ctx.workflow_policy_revision
    {
        return Err(AppError::conflict(
            "workflow_revision_conflict",
            "Read the current activity and retry with its exact candidate and policy revisions.",
        ));
    }
    if m.actor.kind == "agent" {
        let acknowledged:i64=sqlx::query_scalar("SELECT count(*) FROM instruction_acknowledgments WHERE session_id=? AND project_id=? AND policy_revision=? AND instruction_version=?")
            .bind(&owner_session).bind(&project).bind(ctx.project_current_policy).bind(INSTRUCTION_VERSION)
            .fetch_one(&mut *m.tx).await?;
        if acknowledged == 0 {
            return Err(AppError::conflict(
                "instructions_required",
                "Read and acknowledge the current coordination instructions before claiming workflow work.",
            ));
        }
    }
    match ctx.kind.as_str() {
        "agent_review" => {
            if m.actor.kind != "agent" {
                return Err(AppError::forbidden(
                    "An agent session must own an independent agent review.",
                ));
            }
            let contributed:i64=sqlx::query_scalar("SELECT count(*) FROM task_contributors WHERE task_id=? AND (principal_id=? OR session_id=?)")
                .bind(&ctx.subject_task).bind(&m.actor.id).bind(&owner_session).fetch_one(&mut *m.tx).await?;
            if contributed > 0 {
                return Err(AppError::conflict(
                    "reviewer_not_independent",
                    "A contributor principal or session cannot review this task's submissions.",
                ));
            }
        }
        "human_review" => human(&m.actor)?,
        "integration" => {
            if !approvals_satisfied(&mut m.tx, &ctx.submission).await? {
                return Err(AppError::conflict(
                    "reviews_pending",
                    "Every required review must approve this exact submission.",
                ));
            }
            if !ctx.automatic_integration {
                let authorized:i64=sqlx::query_scalar("SELECT count(*) FROM integration_authorizations WHERE activity_id=? AND submission_id=? AND project_policy_revision=? AND workflow_policy_revision=? AND invalidated_at IS NULL")
                    .bind(&ctx.id).bind(&ctx.submission).bind(ctx.project_policy_revision).bind(ctx.workflow_policy_revision)
                    .fetch_one(&mut *m.tx).await?;
                if authorized == 0 {
                    return Err(AppError::conflict(
                        "integration_authorization_required",
                        "A human must authorize this exact integration activity and policy.",
                    ));
                }
            }
        }
        _ => {
            return Err(AppError::conflict(
                "activity_not_eligible",
                "Unknown workflow activity kind.",
            ));
        }
    }
    let task=sqlx::query("SELECT generation,current_attempt_id,blocked_reason FROM tasks WHERE project_id=? AND id=?")
        .bind(&project).bind(&ctx.activity_task).fetch_one(&mut *m.tx).await?;
    if task.get::<Option<String>, _>("blocked_reason").is_some() {
        return Err(AppError::conflict(
            "activity_blocked",
            "This workflow activity still has unmet guards.",
        ));
    }
    if let Some(previous) = task.get::<Option<String>, _>("current_attempt_id") {
        let projected = activity_value(&mut m.tx, &project, &ctx.id, m.now).await?;
        if projected["current_attempt"]["state"] == "active"
            && projected["current_attempt"]["valid_by_time"] == true
            && projected["current_attempt"]["owner_authorized"] == true
        {
            return Err(AppError::conflict(
                "claim_conflict",
                "This workflow activity already has a current owner.",
            ));
        }
        if ctx.recovery_mode == "manual" && m.actor.kind != "human" {
            return Err(AppError::forbidden(
                "This project requires a human to inspect expired or revoked workflow activity authority.",
            ));
        }
        crate::jobs::ensure_attempt_quiescent(&mut m.tx, &project, &ctx.activity_task).await?;
        sqlx::query("UPDATE attempts SET state='expired',ended_at=?,outcome='Workflow activity lease expired.' WHERE id=? AND state='active'")
            .bind(m.now).bind(&previous).execute(&mut *m.tx).await?;
    }
    if ctx.kind == "integration" {
        let existing = sqlx::query_scalar::<_, String>(
            "SELECT state FROM integration_holds WHERE activity_id=?",
        )
        .bind(&ctx.id)
        .fetch_optional(&mut *m.tx)
        .await?;
        match existing.as_deref() {
            Some("held") => {}
            Some(_) => {
                return Err(AppError::conflict(
                    "integration_hold_closed",
                    "This integration activity's canonical hold is closed.",
                ));
            }
            None => {
                sqlx::query("INSERT INTO integration_holds(id,activity_id,canonical_repository_key,target_branch,state,acquired_by,acquired_at) VALUES(?,?,?,?,'held',?,?)")
                    .bind(Uuid::new_v4().to_string()).bind(&ctx.id).bind(ctx.canonical_repository_key.as_deref().ok_or_else(||AppError::conflict("workflow_policy_required","Code integration requires a canonical repository key."))?)
                    .bind(ctx.target_branch.as_deref().ok_or_else(||AppError::conflict("target_required","Code integration requires a target branch."))?)
                    .bind(&m.actor.id).bind(m.now).execute(&mut *m.tx).await.map_err(|e|match e {sqlx::Error::Database(ref d) if d.is_unique_violation()=>AppError::conflict("integration_target_held","Another integration holds this canonical repository and target branch."),_=>e.into()})?;
            }
        }
    }
    let attempt = Uuid::new_v4().to_string();
    let generation = task.get::<i64, _>("generation") + 1;
    let expires = m.now + ctx.lease_seconds * 1000;
    sqlx::query("INSERT INTO attempts(id,project_id,task_id,owner_id,session_id,credential_id,generation,state,mode,expires_at,last_heartbeat_at,last_progress_at,created_at,task_revision,policy_revision) VALUES(?,?,?,?,?,?,?,'active','work',?,?,?,?,1,?)")
        .bind(&attempt).bind(&project).bind(&ctx.activity_task).bind(&m.actor.id).bind(&owner_session).bind(&m.actor.credential_id).bind(generation)
        .bind(expires).bind(m.now).bind(m.now).bind(m.now).bind(ctx.project_current_policy).execute(&mut *m.tx).await?;
    sqlx::query("UPDATE tasks SET current_attempt_id=?,generation=? WHERE id=?")
        .bind(&attempt)
        .bind(generation)
        .bind(&ctx.activity_task)
        .execute(&mut *m.tx)
        .await?;
    sqlx::query("UPDATE workflow_activities SET state='active' WHERE id=?")
        .bind(&ctx.id)
        .execute(&mut *m.tx)
        .await?;
    let value = json!({"activity":activity_value(&mut m.tx,&project,&ctx.id,m.now).await?,
        "attempt":{"id":attempt,"task_id":ctx.activity_task,"generation":generation,"expires_at":timestamp(expires),"state":"active","mode":"work"},
        "lease_remaining_ms":ctx.lease_seconds*1000,"renew_after_seconds":(ctx.lease_seconds/3).min(60),"current_authority":{"valid":true}});
    Ok(response(
        m.finish(value, Some(&project), "workflow_activity.claimed", &ctx.id)
            .await?,
    ))
}

async fn release_activity(
    State(state): State<AppState>,
    auth: Auth,
    Path((project, id)): Path<(String, String)>,
    headers: HeaderMap,
    body: Result<Json<ActivityReleaseInput>, JsonRejection>,
) -> Reply {
    let input = payload(body)?;
    bounded(&input.summary, "summary", 8192, true)?;
    if input.blocked {
        return Err(AppError::bad_request(
            "Workflow activities cannot be released with blocked=true; checkpoint the blocker and release for a guarded retry.",
        ));
    }
    let mut m = Mutation::begin(
        &state,
        &auth,
        &headers,
        &format!("POST /api/v1/projects/{project}/workflow-activities/{id}/release"),
        &input,
    )
    .await?;
    if let Some(v) = m.replay {
        return Ok(response(v));
    }
    let raw = activity_context(&mut m.tx, &project, &id).await?;
    let ctx = owned_activity(&mut m, &project, &id, input.generation, &raw.kind).await?;
    crate::jobs::ensure_attempt_quiescent(&mut m.tx, &project, &ctx.activity_task).await?;
    if sqlx::query_scalar::<_, i64>("SELECT count(*) FROM publication_intents WHERE activity_id=?")
        .bind(&ctx.id)
        .fetch_one(&mut *m.tx)
        .await?
        > 0
    {
        return Err(AppError::conflict(
            "publication_result_required",
            "Publication intent exists. Record a known or uncertain result instead of releasing authority.",
        ));
    }
    let attempt_id: String = sqlx::query_scalar("SELECT current_attempt_id FROM tasks WHERE id=?")
        .bind(&ctx.activity_task)
        .fetch_one(&mut *m.tx)
        .await?;
    sqlx::query("UPDATE attempts SET state=?,ended_at=?,outcome=? WHERE id=?")
        .bind(if input.blocked { "blocked" } else { "released" })
        .bind(m.now)
        .bind(&input.summary)
        .bind(&attempt_id)
        .execute(&mut *m.tx)
        .await?;
    sqlx::query(
        "UPDATE tasks SET current_attempt_id=NULL,blocked_reason=?,ready_since=? WHERE id=?",
    )
    .bind(if input.blocked {
        Some(input.summary.as_str())
    } else {
        None
    })
    .bind(m.now)
    .bind(&ctx.activity_task)
    .execute(&mut *m.tx)
    .await?;
    sqlx::query("UPDATE workflow_activities SET state='queued' WHERE id=?")
        .bind(&ctx.id)
        .execute(&mut *m.tx)
        .await?;
    if ctx.kind == "integration" {
        sqlx::query("UPDATE integration_holds SET state='released',released_by=?,released_at=?,release_reason='Integration activity released before publication intent.' WHERE activity_id=? AND state='held'")
            .bind(&m.actor.id).bind(m.now).bind(&ctx.id).execute(&mut *m.tx).await?;
        sqlx::query("UPDATE workflow_activities SET state='canceled',canceled_at=? WHERE id=?")
            .bind(m.now)
            .bind(&ctx.id)
            .execute(&mut *m.tx)
            .await?;
        sqlx::query("UPDATE tasks SET lifecycle='canceled',blocked_reason=NULL WHERE id=?")
            .bind(&ctx.activity_task)
            .execute(&mut *m.tx)
            .await?;
        let slot:i64=sqlx::query_scalar("SELECT COALESCE(max(slot),0)+1 FROM workflow_activities WHERE submission_id=? AND kind='integration'").bind(&ctx.submission).fetch_one(&mut *m.tx).await?;
        let s=sqlx::query("SELECT t.id,t.kind,t.title,t.acceptance_json,t.revision,p.policy_revision,p.review_mode,p.automatic_integration FROM tasks t JOIN projects p ON p.id=t.project_id WHERE t.id=?").bind(&ctx.subject_task).fetch_one(&mut *m.tx).await?;
        let subject = OwnedSubject {
            task_id: s.get("id"),
            kind: s.get("kind"),
            title: s.get("title"),
            acceptance_json: s.get("acceptance_json"),
            task_revision: s.get("revision"),
            project_policy_revision: s.get("policy_revision"),
            review_mode: s.get("review_mode"),
        };
        create_activity(
            &mut m.tx,
            &project,
            &subject,
            &ctx.submission,
            "integration",
            slot,
            m.now,
        )
        .await?;
    }
    let value = activity_value(&mut m.tx, &project, &ctx.id, m.now).await?;
    Ok(response(
        m.finish(value, Some(&project), "workflow_activity.released", &ctx.id)
            .await?,
    ))
}

async fn owned_activity(
    m: &mut Mutation,
    project: &str,
    id: &str,
    generation: i64,
    kind: &str,
) -> Result<ActivityContext, AppError> {
    let ctx = activity_context(&mut m.tx, project, id).await?;
    ensure_current(&ctx)?;
    if ctx.kind != kind {
        return Err(AppError::conflict(
            "activity_kind_mismatch",
            "This operation does not apply to this workflow activity.",
        ));
    }
    let row=sqlx::query("SELECT a.id,a.owner_id,a.session_id,a.credential_id,a.generation,a.state,a.expires_at,t.current_attempt_id FROM tasks t JOIN attempts a ON a.id=t.current_attempt_id WHERE t.id=?")
        .bind(&ctx.activity_task).fetch_optional(&mut *m.tx).await?.ok_or_else(||AppError::conflict("ownership_lost","Claim this workflow activity before changing it."))?;
    if row.get::<String, _>("owner_id") != m.actor.id
        || row.get::<String, _>("session_id") != session(&m.actor)?
        || row.get::<Option<String>, _>("credential_id") != m.actor.credential_id
        || row.get::<i64, _>("generation") != generation
        || row.get::<String, _>("state") != "active"
        || row.get::<i64, _>("expires_at") <= m.now
    {
        return Err(AppError::conflict(
            "ownership_lost",
            "This workflow activity lease is no longer current.",
        ));
    }
    Ok(ctx)
}

async fn ready_dependents(
    c: &mut SqliteConnection,
    task_id: &str,
    now: i64,
) -> Result<(), AppError> {
    sqlx::query("UPDATE tasks SET ready_since=? WHERE id IN (SELECT d.task_id FROM task_dependencies d WHERE d.prerequisite_id=? AND NOT EXISTS(SELECT 1 FROM task_dependencies x JOIN tasks p ON p.id=x.prerequisite_id WHERE x.task_id=d.task_id AND p.lifecycle!='done'))")
        .bind(now).bind(task_id).execute(&mut *c).await?;
    sqlx::query("UPDATE tasks SET ready_since=? WHERE id IN (SELECT oc.objective_task_id FROM objective_children oc WHERE oc.child_task_id=? AND oc.required=1 AND NOT EXISTS(SELECT 1 FROM objective_children pending JOIN tasks child ON child.project_id=pending.project_id AND child.id=pending.child_task_id WHERE pending.objective_task_id=oc.objective_task_id AND pending.required=1 AND child.lifecycle!='done'))")
        .bind(now).bind(task_id).execute(&mut *c).await?;
    Ok(())
}

async fn ensure_workflow_quiescent(
    c: &mut SqliteConnection,
    project: &str,
    subject_task: &str,
) -> Result<(), AppError> {
    let rows = sqlx::query(
        "SELECT activity_task_id FROM workflow_activities WHERE project_id=? AND subject_task_id=?",
    )
    .bind(project)
    .bind(subject_task)
    .fetch_all(&mut *c)
    .await?;
    for row in rows {
        crate::jobs::ensure_attempt_quiescent(
            c,
            project,
            &row.get::<String, _>("activity_task_id"),
        )
        .await?;
    }
    Ok(())
}

async fn review(
    State(state): State<AppState>,
    auth: Auth,
    Path((project, id)): Path<(String, String)>,
    headers: HeaderMap,
    body: Result<Json<ReviewInput>, JsonRejection>,
) -> Reply {
    let input = payload(body)?;
    bounded(&input.summary, "summary", 16384, true)?;
    if !["approved", "changes_requested"].contains(&input.decision.as_str())
        || input.findings.len() > 100
    {
        return Err(AppError::bad_request(
            "Use approved or changes_requested and at most 100 findings.",
        ));
    }
    for f in &input.findings {
        if !["required", "advisory"].contains(&f.severity.as_str()) {
            return Err(AppError::bad_request(
                "Finding severity must be required or advisory.",
            ));
        }
        bounded(&f.remedy, "finding remedy", 8192, true)?;
        bounded(&f.evidence, "finding evidence", 8192, false)?;
    }
    if input.decision == "approved" && input.findings.iter().any(|f| f.severity == "required") {
        return Err(AppError::bad_request(
            "An approval cannot retain an unresolved required finding.",
        ));
    }
    let mut m = Mutation::begin(
        &state,
        &auth,
        &headers,
        &format!("POST /api/v1/projects/{project}/workflow-activities/{id}/review"),
        &input,
    )
    .await?;
    if let Some(v) = m.replay {
        return Ok(response(v));
    }
    let raw = activity_context(&mut m.tx, &project, &id).await?;
    let ctx = owned_activity(&mut m, &project, &id, input.generation, &raw.kind).await?;
    ensure_activity_decisions(&mut m, &project, &ctx).await?;
    if !["agent_review", "human_review"].contains(&ctx.kind.as_str())
        || input.submission_id != ctx.submission
    {
        return Err(AppError::conflict(
            "review_candidate_mismatch",
            "The review must decide its exact immutable submission.",
        ));
    }
    if (ctx.kind == "agent_review" && m.actor.kind != "agent")
        || (ctx.kind == "human_review" && m.actor.kind != "human")
    {
        return Err(AppError::forbidden(
            "The authenticated actor type does not match this review slot.",
        ));
    }
    crate::jobs::ensure_attempt_quiescent(&mut m.tx, &project, &ctx.activity_task).await?;
    let attempt_id: String = sqlx::query_scalar("SELECT current_attempt_id FROM tasks WHERE id=?")
        .bind(&ctx.activity_task)
        .fetch_one(&mut *m.tx)
        .await?;
    sqlx::query("INSERT INTO review_decisions(activity_id,submission_id,attempt_id,reviewer_id,reviewer_session_id,decision,summary,created_at) VALUES(?,?,?,?,?,?,?,?)")
        .bind(&ctx.id).bind(&ctx.submission).bind(&attempt_id).bind(&m.actor.id).bind(session(&m.actor)?).bind(&input.decision).bind(&input.summary).bind(m.now).execute(&mut *m.tx).await?;
    for finding in &input.findings {
        sqlx::query("INSERT INTO review_findings(id,activity_id,severity,remedy,evidence,created_at) VALUES(?,?,?,?,?,?)")
        .bind(Uuid::new_v4().to_string()).bind(&ctx.id).bind(&finding.severity).bind(&finding.remedy).bind(&finding.evidence).bind(m.now).execute(&mut *m.tx).await?;
    }
    sqlx::query("UPDATE attempts SET state='submitted',ended_at=?,outcome=? WHERE id=?")
        .bind(m.now)
        .bind(&input.summary)
        .bind(&attempt_id)
        .execute(&mut *m.tx)
        .await?;
    sqlx::query(
        "UPDATE tasks SET current_attempt_id=NULL,lifecycle='done',blocked_reason=NULL WHERE id=?",
    )
    .bind(&ctx.activity_task)
    .execute(&mut *m.tx)
    .await?;
    sqlx::query("UPDATE workflow_activities SET state='completed',completed_at=? WHERE id=?")
        .bind(m.now)
        .bind(&ctx.id)
        .execute(&mut *m.tx)
        .await?;
    if input.decision == "changes_requested" {
        ensure_workflow_quiescent(&mut m.tx, &project, &ctx.subject_task).await?;
        sqlx::query("UPDATE submissions SET superseded_at=? WHERE id=? AND superseded_at IS NULL")
            .bind(m.now)
            .bind(&ctx.submission)
            .execute(&mut *m.tx)
            .await?;
        sqlx::query(
            "UPDATE workflow_subjects SET phase='revision_needed',updated_at=? WHERE task_id=?",
        )
        .bind(m.now)
        .bind(&ctx.subject_task)
        .execute(&mut *m.tx)
        .await?;
        sqlx::query("UPDATE workflow_activities SET state='canceled',canceled_at=? WHERE submission_id=? AND id!=? AND state='queued'").bind(m.now).bind(&ctx.submission).bind(&ctx.id).execute(&mut *m.tx).await?;
        sqlx::query("UPDATE attempts SET state='canceled',ended_at=?,outcome='A concurrent review requested changes.' WHERE id IN (SELECT t.current_attempt_id FROM workflow_activities wa JOIN tasks t ON t.id=wa.activity_task_id WHERE wa.submission_id=? AND wa.id!=? AND wa.state='active') AND state='active'")
            .bind(m.now).bind(&ctx.submission).bind(&ctx.id).execute(&mut *m.tx).await?;
        sqlx::query("UPDATE tasks SET current_attempt_id=NULL,lifecycle='canceled',blocked_reason=NULL WHERE id IN (SELECT activity_task_id FROM workflow_activities WHERE submission_id=? AND id!=? AND state='active')")
            .bind(&ctx.submission).bind(&ctx.id).execute(&mut *m.tx).await?;
        sqlx::query("UPDATE workflow_activities SET state='canceled',canceled_at=? WHERE submission_id=? AND id!=? AND state='active'")
            .bind(m.now).bind(&ctx.submission).bind(&ctx.id).execute(&mut *m.tx).await?;
        sqlx::query("UPDATE tasks SET lifecycle='canceled',blocked_reason=NULL WHERE id IN (SELECT activity_task_id FROM workflow_activities WHERE submission_id=? AND id!=? AND state='canceled')")
            .bind(&ctx.submission).bind(&ctx.id).execute(&mut *m.tx).await?;
        sqlx::query("UPDATE tasks SET blocked_reason=NULL,ready_since=? WHERE id=?")
            .bind(m.now)
            .bind(&ctx.subject_task)
            .execute(&mut *m.tx)
            .await?;
    } else if approvals_satisfied(&mut m.tx, &ctx.submission).await? {
        let kind: String = sqlx::query_scalar("SELECT kind FROM submissions WHERE id=?")
            .bind(&ctx.submission)
            .fetch_one(&mut *m.tx)
            .await?;
        if kind == "general" {
            crate::objectives::ensure_required_children_done(
                &mut m.tx,
                &project,
                &ctx.subject_task,
            )
            .await?;
            crate::jobs::ensure_attempt_quiescent(&mut m.tx, &project, &ctx.subject_task).await?;
            ensure_workflow_quiescent(&mut m.tx, &project, &ctx.subject_task).await?;
            sqlx::query("UPDATE workflow_subjects SET phase='done',updated_at=? WHERE task_id=?")
                .bind(m.now)
                .bind(&ctx.subject_task)
                .execute(&mut *m.tx)
                .await?;
            sqlx::query("UPDATE tasks SET lifecycle='done',blocked_reason=NULL WHERE id=?")
                .bind(&ctx.subject_task)
                .execute(&mut *m.tx)
                .await?;
            ready_dependents(&mut m.tx, &ctx.subject_task, m.now).await?;
        } else {
            sqlx::query(
                "UPDATE workflow_subjects SET phase='integration',updated_at=? WHERE task_id=?",
            )
            .bind(m.now)
            .bind(&ctx.subject_task)
            .execute(&mut *m.tx)
            .await?;
            sqlx::query("UPDATE tasks SET blocked_reason=NULL,ready_since=? WHERE id=(SELECT activity_task_id FROM workflow_activities WHERE submission_id=? AND kind='integration' AND state='queued')")
                .bind(m.now).bind(&ctx.submission).execute(&mut *m.tx).await?;
        }
    }
    let value = workflow_snapshot(&mut m.tx, &project, &ctx.subject_task, m.now).await?;
    Ok(response(
        m.finish(value, Some(&project), "review.decided", &ctx.id)
            .await?,
    ))
}

async fn authorize_integration(
    State(state): State<AppState>,
    auth: Auth,
    Path((project, id)): Path<(String, String)>,
    headers: HeaderMap,
    body: Result<Json<IntegrationAuthorizationInput>, JsonRejection>,
) -> Reply {
    let input = payload(body)?;
    bounded(&input.summary, "summary", 8192, true)?;
    let mut m = Mutation::begin(
        &state,
        &auth,
        &headers,
        &format!("POST /api/v1/projects/{project}/workflow-activities/{id}/authorization"),
        &input,
    )
    .await?;
    human(&m.actor)?;
    let ctx = activity_context(&mut m.tx, &project, &id).await?;
    if let Some(v) = m.replay {
        return Ok(response(v));
    }
    ensure_current(&ctx)?;
    ensure_activity_decisions(&mut m, &project, &ctx).await?;
    if ctx.kind != "integration"
        || input.submission_id != ctx.submission
        || input.expected_project_policy_revision != ctx.project_policy_revision
        || input.expected_workflow_policy_revision != ctx.workflow_policy_revision
    {
        return Err(AppError::conflict(
            "integration_candidate_mismatch",
            "Authorization must name the exact current integration candidate and policies.",
        ));
    }
    if ctx.automatic_integration {
        return Err(AppError::conflict(
            "authorization_not_required",
            "This project currently permits automatic integration.",
        ));
    }
    if !approvals_satisfied(&mut m.tx, &ctx.submission).await? {
        return Err(AppError::conflict(
            "reviews_pending",
            "Required reviews must approve this submission before integration authorization.",
        ));
    }
    let previous =
        sqlx::query("SELECT invalidated_at FROM integration_authorizations WHERE activity_id=?")
            .bind(&ctx.id)
            .fetch_optional(&mut *m.tx)
            .await?;
    if previous
        .as_ref()
        .is_some_and(|row| row.get::<Option<i64>, _>("invalidated_at").is_none())
    {
        return Err(AppError::conflict(
            "integration_already_authorized",
            "This integration activity already has current human authorization.",
        ));
    }
    if previous.is_some() {
        sqlx::query("UPDATE integration_authorizations SET submission_id=?,project_policy_revision=?,workflow_policy_revision=?,actor_id=?,summary=?,created_at=?,invalidated_at=NULL,authorization_revision=authorization_revision+1 WHERE activity_id=? AND invalidated_at IS NOT NULL")
            .bind(&ctx.submission).bind(ctx.project_policy_revision).bind(ctx.workflow_policy_revision).bind(&m.actor.id).bind(&input.summary).bind(m.now).bind(&ctx.id).execute(&mut *m.tx).await?;
    } else {
        sqlx::query("INSERT INTO integration_authorizations(activity_id,submission_id,project_policy_revision,workflow_policy_revision,actor_id,summary,created_at) VALUES(?,?,?,?,?,?,?)")
            .bind(&ctx.id).bind(&ctx.submission).bind(ctx.project_policy_revision).bind(ctx.workflow_policy_revision).bind(&m.actor.id).bind(&input.summary).bind(m.now).execute(&mut *m.tx).await?;
    }
    let value = activity_value(&mut m.tx, &project, &ctx.id, m.now).await?;
    Ok(response(
        m.finish(value, Some(&project), "integration.authorized", &ctx.id)
            .await?,
    ))
}

async fn publication_intent(
    State(state): State<AppState>,
    auth: Auth,
    Path((project, id)): Path<(String, String)>,
    headers: HeaderMap,
    body: Result<Json<PublicationIntentInput>, JsonRejection>,
) -> Reply {
    let input = payload(body)?;
    revision(&input.observed_target_revision, "observed_target_revision")?;
    revision(&input.observed_target_tree, "observed_target_tree")?;
    revision(&input.result_revision, "result_revision")?;
    revision(&input.result_tree, "result_tree")?;
    let mut m = Mutation::begin(
        &state,
        &auth,
        &headers,
        &format!("POST /api/v1/projects/{project}/workflow-activities/{id}/publication-intent"),
        &input,
    )
    .await?;
    if let Some(v) = m.replay {
        return Ok(response(v));
    }
    let ctx = owned_activity(&mut m, &project, &id, input.generation, "integration").await?;
    ensure_activity_decisions(&mut m, &project, &ctx).await?;
    if input.submission_id != ctx.submission {
        return Err(AppError::conflict(
            "integration_candidate_mismatch",
            "Publication intent must name the current immutable submission.",
        ));
    }
    if !approvals_satisfied(&mut m.tx, &ctx.submission).await? {
        return Err(AppError::conflict(
            "reviews_pending",
            "All required reviews must still approve this submission.",
        ));
    }
    if !ctx.automatic_integration {
        let n:i64=sqlx::query_scalar("SELECT count(*) FROM integration_authorizations WHERE activity_id=? AND submission_id=? AND project_policy_revision=? AND workflow_policy_revision=? AND invalidated_at IS NULL")
            .bind(&ctx.id).bind(&ctx.submission).bind(ctx.project_policy_revision).bind(ctx.workflow_policy_revision).fetch_one(&mut *m.tx).await?;
        if n == 0 {
            return Err(AppError::conflict(
                "integration_authorization_required",
                "Human integration authorization is required.",
            ));
        }
    }
    let attempt_id: String = sqlx::query_scalar("SELECT current_attempt_id FROM tasks WHERE id=?")
        .bind(&ctx.activity_task)
        .fetch_one(&mut *m.tx)
        .await?;
    if sqlx::query_scalar::<_, i64>(
        "SELECT count(*) FROM checkouts WHERE project_id=? AND attempt_id=?",
    )
    .bind(&project)
    .bind(&attempt_id)
    .fetch_one(&mut *m.tx)
    .await?
        == 0
    {
        return Err(AppError::conflict(
            "checkout_required",
            "Register the isolated integration checkout before publication intent.",
        ));
    }
    if sqlx::query_scalar::<_, i64>(
        "SELECT count(*) FROM integration_holds WHERE activity_id=? AND state='held'",
    )
    .bind(&ctx.id)
    .fetch_one(&mut *m.tx)
    .await?
        == 0
    {
        return Err(AppError::conflict(
            "integration_hold_required",
            "The canonical integration hold is no longer held.",
        ));
    }
    sqlx::query("INSERT INTO publication_intents(activity_id,submission_id,attempt_id,observed_target_revision,observed_target_tree,result_revision,result_tree,created_by,created_at) VALUES(?,?,?,?,?,?,?,?,?)")
        .bind(&ctx.id).bind(&ctx.submission).bind(&attempt_id).bind(&input.observed_target_revision).bind(&input.observed_target_tree).bind(&input.result_revision).bind(&input.result_tree).bind(&m.actor.id).bind(m.now).execute(&mut *m.tx).await?;
    let value = activity_value(&mut m.tx, &project, &ctx.id, m.now).await?;
    Ok(response(
        m.finish(
            value,
            Some(&project),
            "integration.publication_intended",
            &ctx.id,
        )
        .await?,
    ))
}

async fn validate_check_jobs(
    c: &mut SqliteConnection,
    project: &str,
    ctx: &ActivityContext,
    attempt_id: &str,
    result_revision: &str,
    result_tree: &str,
    job_ids: &[String],
) -> Result<(), AppError> {
    let roster_row:String=sqlx::query_scalar("SELECT required_checks_json FROM workflow_policy_revisions WHERE project_id=? AND revision=?")
        .bind(project).bind(ctx.workflow_policy_revision).fetch_optional(&mut *c).await?
        .ok_or_else(||AppError::conflict("workflow_policy_missing","The pinned required-check roster is unavailable."))?;
    let roster: Vec<RequiredCheck> = serde_json::from_str(&roster_row)?;
    validate_checks(&roster)?;
    if job_ids.len() != roster.len() || job_ids.len() > 100 {
        return Err(AppError::conflict(
            "required_checks_incomplete",
            "Select exactly one producer receipt for every pinned required check.",
        ));
    }
    let mut unique = BTreeSet::new();
    let required: BTreeSet<_> = roster
        .into_iter()
        .map(|r| (r.identity, r.version, r.environment))
        .collect();
    let mut actual = BTreeSet::new();
    for job in job_ids {
        if !unique.insert(job) {
            return Err(AppError::conflict(
                "duplicate_check_receipt",
                "A producer job can satisfy at most one required roster entry.",
            ));
        }
        let row=sqlx::query("SELECT task_id,attempt_id,state,exit_code,inputs_unchanged,reconciled_at,source_revision,source_tree,check_identity,check_version,check_environment FROM jobs WHERE project_id=? AND id=?")
            .bind(project).bind(job).fetch_optional(&mut *c).await?.ok_or_else(||AppError::conflict("check_receipt_missing","A selected check job does not exist in this project."))?;
        if row.get::<String, _>("task_id") != ctx.activity_task
            || row.get::<String, _>("attempt_id") != attempt_id
            || row.get::<String, _>("state") != "succeeded"
            || row.get::<Option<i64>, _>("exit_code") != Some(0)
            || row.get::<Option<bool>, _>("inputs_unchanged") != Some(true)
            || row.get::<Option<i64>, _>("reconciled_at").is_some()
            || row.get::<String, _>("source_revision") != result_revision
            || row.get::<String, _>("source_tree") != result_tree
        {
            return Err(AppError::conflict(
                "check_receipt_inapplicable",
                "Required checks must be unreconciled successful producers with stable inputs on the exact intended result.",
            ));
        }
        let tuple = (
            row.get::<Option<String>, _>("check_identity"),
            row.get::<Option<String>, _>("check_version"),
            row.get::<Option<String>, _>("check_environment"),
        );
        let tuple = match tuple {
            (Some(a), Some(b), Some(c)) => (a, b, c),
            _ => {
                return Err(AppError::conflict(
                    "check_identity_missing",
                    "Every selected producer must carry immutable check identity, version, and environment.",
                ));
            }
        };
        if !actual.insert(tuple) {
            return Err(AppError::conflict(
                "duplicate_check_identity",
                "Required check identity tuples cannot be reused.",
            ));
        }
    }
    if actual != required {
        return Err(AppError::conflict(
            "required_checks_incomplete",
            "Selected producer receipts do not exactly cover the pinned required-check roster.",
        ));
    }
    Ok(())
}

async fn integration_result(
    State(state): State<AppState>,
    auth: Auth,
    Path((project, id)): Path<(String, String)>,
    headers: HeaderMap,
    body: Result<Json<IntegrationResultInput>, JsonRejection>,
) -> Reply {
    let input = payload(body)?;
    if !["published", "not_published", "uncertain"].contains(&input.publication_state.as_str()) {
        return Err(AppError::bad_request(
            "publication_state must be published, not_published, or uncertain.",
        ));
    }
    bounded(&input.summary, "summary", 16384, true)?;
    revision(&input.observed_target_revision, "observed_target_revision")?;
    revision(&input.result_revision, "result_revision")?;
    revision(&input.result_tree, "result_tree")?;
    let mut m = Mutation::begin(
        &state,
        &auth,
        &headers,
        &format!("POST /api/v1/projects/{project}/workflow-activities/{id}/integration-result"),
        &input,
    )
    .await?;
    if let Some(v) = m.replay {
        return Ok(response(v));
    }
    let ctx = owned_activity(&mut m, &project, &id, input.generation, "integration").await?;
    ensure_activity_decisions(&mut m, &project, &ctx).await?;
    if input.submission_id != ctx.submission {
        return Err(AppError::conflict(
            "integration_candidate_mismatch",
            "Integration result must name the exact current submission.",
        ));
    }
    let intent=sqlx::query("SELECT attempt_id,observed_target_revision,result_revision,result_tree FROM publication_intents WHERE activity_id=?").bind(&ctx.id).fetch_optional(&mut *m.tx).await?.ok_or_else(||AppError::conflict("publication_intent_required","Record immutable publication intent before reporting an integration result."))?;
    if intent.get::<String, _>("observed_target_revision") != input.observed_target_revision
        || intent.get::<String, _>("result_revision") != input.result_revision
        || intent.get::<String, _>("result_tree") != input.result_tree
    {
        return Err(AppError::conflict(
            "publication_intent_mismatch",
            "Integration result identities must exactly match saved publication intent.",
        ));
    }
    let attempt_id: String = intent.get("attempt_id");
    let current_attempt: String =
        sqlx::query_scalar("SELECT current_attempt_id FROM tasks WHERE id=?")
            .bind(&ctx.activity_task)
            .fetch_one(&mut *m.tx)
            .await?;
    if attempt_id != current_attempt {
        return Err(AppError::conflict(
            "publication_attempt_mismatch",
            "The saved publication intent belongs to a different activity attempt.",
        ));
    }
    if input.publication_state == "published" {
        validate_check_jobs(
            &mut m.tx,
            &project,
            &ctx,
            &attempt_id,
            &input.result_revision,
            &input.result_tree,
            &input.check_job_ids,
        )
        .await?;
    } else if !input.check_job_ids.is_empty() {
        return Err(AppError::bad_request(
            "Unknown or known nonpublication results omit completion check receipts.",
        ));
    }
    sqlx::query("INSERT INTO integration_results(activity_id,submission_id,attempt_id,publication_state,observed_target_revision,result_revision,result_tree,check_job_ids_json,summary,reported_by,created_at) VALUES(?,?,?,?,?,?,?,?,?,?,?)")
        .bind(&ctx.id).bind(&ctx.submission).bind(&attempt_id).bind(&input.publication_state).bind(&input.observed_target_revision).bind(&input.result_revision).bind(&input.result_tree)
        .bind(serde_json::to_string(&input.check_job_ids)?).bind(&input.summary).bind(&m.actor.id).bind(m.now).execute(&mut *m.tx).await?;
    if input.publication_state != "published" {
        sqlx::query("UPDATE workflow_activities SET state='recovery_required' WHERE id=?")
            .bind(&ctx.id)
            .execute(&mut *m.tx)
            .await?;
        sqlx::query("UPDATE attempts SET state='blocked',ended_at=?,outcome=? WHERE id=?")
            .bind(m.now)
            .bind(&input.summary)
            .bind(&attempt_id)
            .execute(&mut *m.tx)
            .await?;
        sqlx::query("UPDATE tasks SET current_attempt_id=NULL,blocked_reason=? WHERE id=?")
            .bind("Publication outcome requires human reconciliation.")
            .bind(&ctx.activity_task)
            .execute(&mut *m.tx)
            .await?;
    }
    let value = activity_value(&mut m.tx, &project, &ctx.id, m.now).await?;
    Ok(response(
        m.finish(
            value,
            Some(&project),
            "integration.result_recorded",
            &ctx.id,
        )
        .await?,
    ))
}

async fn reconcile_publication(
    State(state): State<AppState>,
    auth: Auth,
    Path((project, id)): Path<(String, String)>,
    headers: HeaderMap,
    body: Result<Json<PublicationReconciliationInput>, JsonRejection>,
) -> Reply {
    let input = payload(body)?;
    if !["published", "not_published", "target_moved"].contains(&input.disposition.as_str()) {
        return Err(AppError::bad_request(
            "disposition must be published, not_published, or target_moved.",
        ));
    }
    revision(&input.observed_target_revision, "observed_target_revision")?;
    revision(&input.observed_target_tree, "observed_target_tree")?;
    bounded(&input.evidence, "evidence", 16384, true)?;
    let mut m = Mutation::begin(
        &state,
        &auth,
        &headers,
        &format!(
            "POST /api/v1/projects/{project}/workflow-activities/{id}/publication-reconciliation"
        ),
        &input,
    )
    .await?;
    human(&m.actor)?;
    let ctx = activity_context(&mut m.tx, &project, &id).await?;
    if let Some(v) = m.replay {
        return Ok(response(v));
    }
    if input.submission_id != ctx.submission || ctx.submission != ctx.current_submission {
        return Err(AppError::conflict(
            "integration_candidate_mismatch",
            "Reconciliation must name the exact current submission.",
        ));
    }
    let intent=sqlx::query("SELECT observed_target_revision,observed_target_tree,result_revision,result_tree FROM publication_intents WHERE activity_id=?")
        .bind(&ctx.id).fetch_optional(&mut *m.tx).await?.ok_or_else(||AppError::conflict("publication_intent_required","There is no publication intent to reconcile."))?;
    let result_state: Option<String> =
        sqlx::query_scalar("SELECT publication_state FROM integration_results WHERE activity_id=?")
            .bind(&ctx.id)
            .fetch_optional(&mut *m.tx)
            .await?;
    let intended_revision: String = intent.get("result_revision");
    let intended_tree: String = intent.get("result_tree");
    if input.disposition == "not_published" && result_state.as_deref() == Some("published") {
        return Err(AppError::conflict(
            "publication_known",
            "A known published result cannot be reconciled as not published.",
        ));
    }
    if input.disposition == "not_published"
        && (input.observed_target_revision != intent.get::<String, _>("observed_target_revision")
            || input.observed_target_tree != intent.get::<String, _>("observed_target_tree")
            || input.observed_target_revision == intended_revision)
    {
        return Err(AppError::conflict(
            "nonpublication_source_mismatch",
            "Known nonpublication must observe the exact target revision and tree saved before publication.",
        ));
    }
    if input.disposition == "published"
        && (input.observed_target_revision != intended_revision
            || input.observed_target_tree != intended_tree)
    {
        return Err(AppError::conflict(
            "integrated_source_mismatch",
            "Published reconciliation must observe the exact intended result revision and tree.",
        ));
    }
    if input.disposition == "target_moved"
        && (input.observed_target_revision == intended_revision
            || input.observed_target_revision
                == intent.get::<String, _>("observed_target_revision"))
    {
        return Err(AppError::conflict(
            "target_not_moved",
            "Target-moved reconciliation must observe a revision distinct from both the CAS expectation and intended result.",
        ));
    }
    sqlx::query("INSERT INTO publication_reconciliations(activity_id,submission_id,disposition,observed_target_revision,observed_target_tree,evidence,actor_id,created_at) VALUES(?,?,?,?,?,?,?,?)")
        .bind(&ctx.id).bind(&ctx.submission).bind(&input.disposition).bind(&input.observed_target_revision).bind(&input.observed_target_tree).bind(&input.evidence).bind(&m.actor.id).bind(m.now).execute(&mut *m.tx).await?;
    sqlx::query("UPDATE attempts SET state='canceled',ended_at=?,outcome='Publication reconciled by a human; replacement integration required.' WHERE id=(SELECT current_attempt_id FROM tasks WHERE id=?) AND state='active'")
        .bind(m.now).bind(&ctx.activity_task).execute(&mut *m.tx).await?;
    sqlx::query("UPDATE integration_holds SET state='released',released_by=?,released_at=?,release_reason=? WHERE activity_id=? AND state='held'")
        .bind(&m.actor.id).bind(m.now).bind(format!("Human reconciliation: {}",input.disposition)).bind(&ctx.id).execute(&mut *m.tx).await?;
    sqlx::query("UPDATE workflow_activities SET state='canceled',canceled_at=? WHERE id=?")
        .bind(m.now)
        .bind(&ctx.id)
        .execute(&mut *m.tx)
        .await?;
    sqlx::query("UPDATE tasks SET lifecycle='canceled',current_attempt_id=NULL,blocked_reason=NULL WHERE id=?").bind(&ctx.activity_task).execute(&mut *m.tx).await?;
    let slot:i64=sqlx::query_scalar("SELECT COALESCE(max(slot),0)+1 FROM workflow_activities WHERE submission_id=? AND kind='integration'").bind(&ctx.submission).fetch_one(&mut *m.tx).await?;
    let s=sqlx::query("SELECT t.id,t.kind,t.title,t.acceptance_json,t.revision,p.policy_revision,p.review_mode,p.automatic_integration FROM tasks t JOIN projects p ON p.id=t.project_id WHERE t.id=?").bind(&ctx.subject_task).fetch_one(&mut *m.tx).await?;
    let subject = OwnedSubject {
        task_id: s.get("id"),
        kind: s.get("kind"),
        title: s.get("title"),
        acceptance_json: s.get("acceptance_json"),
        task_revision: s.get("revision"),
        project_policy_revision: s.get("policy_revision"),
        review_mode: s.get("review_mode"),
    };
    let replacement = create_activity(
        &mut m.tx,
        &project,
        &subject,
        &ctx.submission,
        "integration",
        slot,
        m.now,
    )
    .await?;
    sqlx::query("INSERT INTO integration_holds(id,activity_id,canonical_repository_key,target_branch,state,acquired_by,acquired_at) VALUES(?,?,?,?,'held',?,?)")
        .bind(Uuid::new_v4().to_string()).bind(&replacement)
        .bind(ctx.canonical_repository_key.as_deref().ok_or_else(||AppError::conflict("workflow_policy_required","Replacement integration requires the pinned canonical repository key."))?)
        .bind(ctx.target_branch.as_deref().ok_or_else(||AppError::conflict("target_required","Replacement integration requires the pinned target branch."))?)
        .bind(&m.actor.id).bind(m.now).execute(&mut *m.tx).await?;
    let value = workflow_snapshot(&mut m.tx, &project, &ctx.subject_task, m.now).await?;
    Ok(response(
        m.finish(
            value,
            Some(&project),
            "integration.publication_reconciled",
            &ctx.id,
        )
        .await?,
    ))
}

async fn finalize(
    State(state): State<AppState>,
    auth: Auth,
    Path((project, id)): Path<(String, String)>,
    headers: HeaderMap,
    body: Result<Json<FinalizeIntegrationInput>, JsonRejection>,
) -> Reply {
    let input = payload(body)?;
    revision(&input.observed_target_revision, "observed_target_revision")?;
    revision(&input.observed_target_tree, "observed_target_tree")?;
    let mut m = Mutation::begin(
        &state,
        &auth,
        &headers,
        &format!("POST /api/v1/projects/{project}/workflow-activities/{id}/finalize"),
        &input,
    )
    .await?;
    if let Some(v) = m.replay {
        return Ok(response(v));
    }
    let ctx = owned_activity(&mut m, &project, &id, input.generation, "integration").await?;
    ensure_activity_decisions(&mut m, &project, &ctx).await?;
    if input.submission_id != ctx.submission {
        return Err(AppError::conflict(
            "integration_candidate_mismatch",
            "Finalization must name the exact current submission.",
        ));
    }
    if !approvals_satisfied(&mut m.tx, &ctx.submission).await? {
        return Err(AppError::conflict(
            "reviews_pending",
            "All required reviews must still approve this exact submission.",
        ));
    }
    if !ctx.automatic_integration {
        let n:i64=sqlx::query_scalar("SELECT count(*) FROM integration_authorizations WHERE activity_id=? AND submission_id=? AND project_policy_revision=? AND workflow_policy_revision=? AND invalidated_at IS NULL")
            .bind(&ctx.id).bind(&ctx.submission).bind(ctx.project_policy_revision).bind(ctx.workflow_policy_revision).fetch_one(&mut *m.tx).await?;
        if n == 0 {
            return Err(AppError::conflict(
                "integration_authorization_required",
                "This integration activity lacks current human authorization.",
            ));
        }
    }
    let result=sqlx::query("SELECT attempt_id,publication_state,result_revision,result_tree,check_job_ids_json FROM integration_results WHERE activity_id=?")
        .bind(&ctx.id).fetch_optional(&mut *m.tx).await?.ok_or_else(||AppError::conflict("integration_result_required","Record the publication result and exact check receipts before finalization."))?;
    let state: String = result.get("publication_state");
    if state != "published" {
        let disposition: Option<String> = sqlx::query_scalar(
            "SELECT disposition FROM publication_reconciliations WHERE activity_id=?",
        )
        .bind(&ctx.id)
        .fetch_optional(&mut *m.tx)
        .await?;
        if disposition.as_deref() != Some("published") {
            return Err(AppError::conflict(
                "publication_unresolved",
                "Human reconciliation has not established publication.",
            ));
        }
    }
    let result_revision: String = result.get("result_revision");
    let result_tree: String = result.get("result_tree");
    if input.observed_target_revision != result_revision
        || input.observed_target_tree != result_tree
    {
        return Err(AppError::conflict(
            "integrated_source_mismatch",
            "Fresh remote observation must equal the exact intended integrated revision and tree.",
        ));
    }
    let jobs: Vec<String> = serde_json::from_str(&result.get::<String, _>("check_job_ids_json"))?;
    let producer_attempt: String = result.get("attempt_id");
    validate_check_jobs(
        &mut m.tx,
        &project,
        &ctx,
        &producer_attempt,
        &result_revision,
        &result_tree,
        &jobs,
    )
    .await?;
    crate::jobs::ensure_attempt_quiescent(&mut m.tx, &project, &ctx.subject_task).await?;
    crate::jobs::ensure_attempt_quiescent(&mut m.tx, &project, &ctx.activity_task).await?;
    let current_attempt: String =
        sqlx::query_scalar("SELECT current_attempt_id FROM tasks WHERE id=?")
            .bind(&ctx.activity_task)
            .fetch_one(&mut *m.tx)
            .await?;
    sqlx::query("UPDATE attempts SET state='submitted',ended_at=?,outcome='Exact integrated result validated.' WHERE id=?").bind(m.now).bind(&current_attempt).execute(&mut *m.tx).await?;
    sqlx::query("UPDATE tasks SET lifecycle='done',current_attempt_id=NULL,blocked_reason=NULL WHERE id IN (?,?)").bind(&ctx.activity_task).bind(&ctx.subject_task).execute(&mut *m.tx).await?;
    sqlx::query("UPDATE workflow_activities SET state='completed',completed_at=? WHERE id=?")
        .bind(m.now)
        .bind(&ctx.id)
        .execute(&mut *m.tx)
        .await?;
    sqlx::query("UPDATE workflow_subjects SET phase='done',updated_at=? WHERE task_id=?")
        .bind(m.now)
        .bind(&ctx.subject_task)
        .execute(&mut *m.tx)
        .await?;
    sqlx::query("UPDATE integration_holds SET state='released',released_by=?,released_at=?,release_reason='Exact integrated result completed.' WHERE activity_id=? AND state='held'")
        .bind(&m.actor.id).bind(m.now).bind(&ctx.id).execute(&mut *m.tx).await?;
    ready_dependents(&mut m.tx, &ctx.subject_task, m.now).await?;
    let value = workflow_snapshot(&mut m.tx, &project, &ctx.subject_task, m.now).await?;
    Ok(response(
        m.finish(value, Some(&project), "integration.completed", &ctx.id)
            .await?,
    ))
}

struct OwnedSubject {
    task_id: String,
    kind: String,
    title: String,
    acceptance_json: String,
    task_revision: i64,
    project_policy_revision: i64,
    review_mode: String,
}

async fn owned_subject(
    m: &mut Mutation,
    project: &str,
    attempt: &str,
    generation: i64,
) -> Result<OwnedSubject, AppError> {
    let row = sqlx::query(
        "SELECT a.task_id,a.owner_id,a.session_id,a.credential_id,a.generation,a.state,a.expires_at, \
         a.task_revision AS pinned_task_revision,a.policy_revision AS pinned_policy_revision, \
         t.kind,t.title,t.acceptance_json,t.revision,t.current_attempt_id,t.lifecycle, \
         p.policy_revision,p.review_mode,p.automatic_integration \
         FROM attempts a JOIN tasks t ON t.id=a.task_id AND t.project_id=a.project_id \
         JOIN projects p ON p.id=a.project_id WHERE a.project_id=? AND a.id=?",
    )
    .bind(project)
    .bind(attempt)
    .fetch_optional(&mut *m.tx)
    .await?
    .ok_or_else(AppError::not_found)?;
    let valid = row.get::<String, _>("owner_id") == m.actor.id
        && row.get::<String, _>("session_id") == session(&m.actor)?
        && row.get::<Option<String>, _>("credential_id") == m.actor.credential_id
        && row.get::<i64, _>("generation") == generation
        && row.get::<String, _>("state") == "active"
        && row.get::<i64, _>("expires_at") > m.now
        && row
            .get::<Option<String>, _>("current_attempt_id")
            .as_deref()
            == Some(attempt)
        && row.get::<String, _>("lifecycle") == "open";
    if !valid {
        return Err(AppError::conflict(
            "ownership_lost",
            "This attempt no longer has current unexpired authority.",
        ));
    }
    crate::knowledge::ensure_decisions_resolved(
        &mut m.tx,
        project,
        &row.get::<String, _>("task_id"),
        m.now,
    )
    .await?;
    let pinned_task = row.get::<Option<i64>, _>("pinned_task_revision");
    let pinned_policy = row.get::<Option<i64>, _>("pinned_policy_revision");
    if pinned_task.is_none() || pinned_policy.is_none() {
        return Err(AppError::conflict(
            "revision_pins_required",
            "Release this pre-workflow attempt and claim again to pin task and policy revisions.",
        ));
    }
    Ok(OwnedSubject {
        task_id: row.get("task_id"),
        kind: row.get("kind"),
        title: row.get("title"),
        acceptance_json: row.get("acceptance_json"),
        task_revision: pinned_task.unwrap(),
        project_policy_revision: pinned_policy.unwrap(),
        review_mode: row.get("review_mode"),
    })
}

fn validate_acceptance(input: &SubmissionInput, acceptance_json: &str) -> Result<(), AppError> {
    let criteria: Vec<String> = serde_json::from_str(acceptance_json)?;
    if input.acceptance_evidence.len() != criteria.len() {
        return Err(AppError::bad_request(
            "Provide evidence for every current acceptance criterion exactly once.",
        ));
    }
    let mut seen = BTreeSet::new();
    for item in &input.acceptance_evidence {
        bounded(&item.criterion, "acceptance criterion", 2048, true)?;
        bounded(&item.evidence, "acceptance evidence", 8192, true)?;
        if !criteria.contains(&item.criterion) || !seen.insert(item.criterion.clone()) {
            return Err(AppError::bad_request(
                "Acceptance evidence must name each current criterion exactly once.",
            ));
        }
    }
    Ok(())
}

async fn create_activity(
    c: &mut SqliteConnection,
    project: &str,
    subject: &OwnedSubject,
    submission: &str,
    kind: &str,
    slot: i64,
    now: i64,
) -> Result<String, AppError> {
    let activity = Uuid::new_v4().to_string();
    let task = Uuid::new_v4().to_string();
    let title = match kind {
        "agent_review" => format!("Agent review: {}", subject.title),
        "human_review" => format!("Human review: {}", subject.title),
        _ => format!("Integrate: {}", subject.title),
    };
    let acceptance = serde_json::to_string(&vec![match kind {
        "integration" => "Publish and validate the exact integrated result",
        _ => "Record a decision for the immutable submission",
    }])?;
    sqlx::query("INSERT INTO tasks(id,project_id,title,description,acceptance_json,kind,priority,lifecycle,revision,generation,blocked_reason,created_at,ready_since) VALUES(?,?,?,?,?,'general',0,'open',1,0,?,?,?)")
        .bind(&task).bind(project).bind(&title).bind(format!("Internal workflow activity for submission {submission}.")).bind(&acceptance)
        .bind(Option::<String>::None).bind(now).bind(now).execute(&mut *c).await?;
    sqlx::query("INSERT INTO task_revisions(project_id,task_id,revision,data_json,actor_id,created_at) \
        SELECT ?,?,1,json_object('title',?,'description',?,'acceptance_criteria',json(?),'kind','general','priority',0,'depends_on',json('[]'),'planned',false),created_by,? FROM submissions WHERE id=?")
        .bind(project).bind(&task).bind(&title).bind(format!("Internal workflow activity for submission {submission}."))
        .bind(&acceptance).bind(now).bind(submission).execute(&mut *c).await?;
    sqlx::query("INSERT INTO workflow_activities(id,project_id,subject_task_id,submission_id,activity_task_id,kind,slot,state,created_at) VALUES(?,?,?,?,?,?,?,'queued',?)")
        .bind(&activity).bind(project).bind(&subject.task_id).bind(submission).bind(&task).bind(kind).bind(slot).bind(now).execute(&mut *c).await?;
    Ok(activity)
}

async fn ensure_activity_decisions(
    m: &mut Mutation,
    project: &str,
    ctx: &ActivityContext,
) -> Result<(), AppError> {
    crate::knowledge::ensure_decisions_resolved(&mut m.tx, project, &ctx.subject_task, m.now)
        .await?;
    crate::knowledge::ensure_decisions_resolved(&mut m.tx, project, &ctx.activity_task, m.now).await
}
