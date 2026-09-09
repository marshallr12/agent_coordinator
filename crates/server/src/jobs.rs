//! Durable producer observations and globally shared resource reservations.
use crate::{
    auth::{Actor, Auth, PROOF_HEADER, SESSION_HEADER, digest},
    coordination::{Attempt, owned},
    error::AppError,
    mutation::Mutation,
    response,
    state::AppState,
};
use axum::{
    Json, Router,
    extract::{FromRequestParts, Path, Query, State, rejection::JsonRejection},
    http::{HeaderMap, header, request::Parts},
    routing::{get, post},
};
use coordinator_core::timestamp;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{Row, SqliteConnection};
use subtle::ConstantTimeEq;
use uuid::Uuid;

type Reply = Result<Json<Value>, AppError>;
const REPORTER_LIFETIME_MS: i64 = 7 * 86_400_000;

fn payload<T>(value: Result<Json<T>, JsonRejection>) -> Result<T, AppError> {
    value.map(|Json(value)| value).map_err(|_| {
        AppError::bad_request("The JSON body does not match this operation's request schema.")
    })
}

fn bounded(value: &str, name: &str, max: usize, required: bool) -> Result<(), AppError> {
    if value.len() > max || (required && value.trim().is_empty()) || value.contains('\0') {
        return Err(AppError::bad_request(&format!(
            "{name} must {}contain at most {max} bytes and no NUL characters.",
            if required { "be nonempty and " } else { "" }
        )));
    }
    Ok(())
}

fn uuid(value: &str, name: &str) -> Result<(), AppError> {
    if Uuid::parse_str(value).is_err() {
        return Err(AppError::bad_request(&format!("{name} must be a UUID.")));
    }
    Ok(())
}

fn resource_key(value: &str) -> Result<(), AppError> {
    bounded(value, "key", 255, true)?;
    if value.trim() != value
        || !value.bytes().all(|byte| {
            byte.is_ascii_lowercase()
                || byte.is_ascii_digit()
                || matches!(byte, b'-' | b'_' | b'.' | b':' | b'/')
        })
    {
        return Err(AppError::bad_request(
            "key must be a canonical lowercase resource name using letters, digits, -, _, ., :, or /.",
        ));
    }
    Ok(())
}

#[derive(Clone)]
pub struct ReporterAuth {
    id: String,
    proof_hash: String,
}

impl ReporterAuth {
    pub fn id(&self) -> &str {
        &self.id
    }

    pub async fn authenticate(parts: &Parts, state: &AppState) -> Result<Self, AppError> {
        if !parts.uri.path().starts_with("/api/v1/reporters/")
            || parts.headers.contains_key(header::COOKIE)
            || parts.headers.contains_key(SESSION_HEADER)
            || parts.headers.contains_key(PROOF_HEADER)
        {
            return Err(AppError::auth_required());
        }
        let mut values = parts.headers.get_all(header::AUTHORIZATION).iter();
        let value = values
            .next()
            .filter(|_| values.next().is_none())
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.strip_prefix("Bearer acr_"))
            .ok_or_else(AppError::auth_required)?;
        let (id, proof) = value.split_once('.').ok_or_else(AppError::auth_required)?;
        uuid(id, "reporter id").map_err(|_| AppError::auth_required())?;
        let decoded = hex::decode(proof).map_err(|_| AppError::auth_required())?;
        if decoded.len() != 32 || !proof.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(AppError::auth_required());
        }
        let auth = Self {
            id: id.to_owned(),
            proof_hash: digest(proof),
        };
        auth.verify(&mut *state.pool.acquire().await?, state.now())
            .await?;
        Ok(auth)
    }

    pub async fn verify(
        &self,
        connection: &mut SqliteConnection,
        now: i64,
    ) -> Result<Actor, AppError> {
        let row = sqlx::query(
            "SELECT r.proof_hash,r.session_id,p.id,p.name,p.kind,p.role,c.id AS credential_id \
             FROM reporters r \
             JOIN credentials c ON c.id=r.credential_id AND c.principal_id=r.principal_id \
             JOIN principals p ON p.id=r.principal_id \
             WHERE r.id=? AND r.expires_at>? AND c.revoked_at IS NULL \
             AND (c.expires_at IS NULL OR c.expires_at>?) AND p.disabled_at IS NULL \
             AND p.kind='agent' AND p.role='agent'",
        )
        .bind(&self.id)
        .bind(now)
        .bind(now)
        .fetch_optional(connection)
        .await?
        .ok_or_else(AppError::auth_required)?;
        let stored: String = row.get("proof_hash");
        if !bool::from(stored.as_bytes().ct_eq(self.proof_hash.as_bytes())) {
            return Err(AppError::auth_required());
        }
        Ok(Actor {
            id: row.get("id"),
            name: row.get("name"),
            kind: row.get("kind"),
            role: row.get("role"),
            credential_id: Some(row.get("credential_id")),
            session_id: Some(row.get("session_id")),
        })
    }
}

impl FromRequestParts<AppState> for ReporterAuth {
    type Rejection = AppError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        if let Some(auth) = parts.extensions.get::<ReporterAuth>() {
            return Ok(auth.clone());
        }
        Self::authenticate(parts, state).await
    }
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/resources", get(resources).post(create_resource))
        .route(
            "/api/v1/projects/{project}/attempts/{attempt}/reservations",
            get(attempt_reservations).post(create_reservation),
        )
        .route("/api/v1/projects/{project}/reservations", get(reservations))
        .route(
            "/api/v1/projects/{project}/reservations/{reservation}/release",
            post(release_reservation),
        )
        .route(
            "/api/v1/projects/{project}/reservations/{reservation}/resolve",
            post(resolve_reservation),
        )
        .route(
            "/api/v1/projects/{project}/attempts/{attempt}/jobs",
            post(register_job),
        )
        .route("/api/v1/projects/{project}/jobs", get(jobs))
        .route("/api/v1/projects/{project}/jobs/{job}", get(job_detail))
        .route("/api/v1/reporters/{reporter}", get(reporter_detail))
        .route("/api/v1/reporters/{reporter}/observations", post(observe))
        .route("/api/v1/reporters/{reporter}/renew", post(reporter_renew))
}

#[derive(Deserialize)]
struct Page {
    cursor: Option<String>,
    limit: Option<i64>,
}

impl Page {
    fn limit(&self) -> Result<i64, AppError> {
        let limit = self.limit.unwrap_or(50);
        if !(1..=200).contains(&limit) || self.cursor.as_ref().is_some_and(|v| v.len() > 128) {
            return Err(AppError::bad_request(
                "limit must be between 1 and 200 and cursor must be valid.",
            ));
        }
        Ok(limit)
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ResourceInput {
    key: String,
    capacity: i64,
    #[serde(default)]
    description: String,
}

async fn resources(State(state): State<AppState>, _auth: Auth, Query(page): Query<Page>) -> Reply {
    let limit = page.limit()?;
    let rows = sqlx::query(
        "SELECT x.id,x.key,x.capacity,x.description,x.created_at, \
         COALESCE((SELECT sum(ri.units) FROM reservation_items ri \
           JOIN reservations r ON r.id=ri.reservation_id \
           WHERE ri.resource_id=x.id AND r.state='held'),0) AS held_units FROM resources x \
         WHERE (? IS NULL OR x.id>?) ORDER BY x.id LIMIT ?",
    )
    .bind(&page.cursor)
    .bind(&page.cursor)
    .bind(limit + 1)
    .fetch_all(&state.pool)
    .await?;
    let items = rows
        .iter()
        .take(limit as usize)
        .map(resource_value)
        .collect::<Vec<_>>();
    let next = (rows.len() > limit as usize)
        .then(|| {
            items
                .last()
                .and_then(|item| item["id"].as_str())
                .map(str::to_owned)
        })
        .flatten();
    Ok(response(json!({"items":items,"next_cursor":next})))
}

fn resource_value(row: &sqlx::sqlite::SqliteRow) -> Value {
    json!({
        "id":row.get::<String,_>("id"),
        "key":row.get::<String,_>("key"),
        "capacity":row.get::<i64,_>("capacity"),
        "held_units":row.get::<i64,_>("held_units"),
        "description":row.get::<String,_>("description"),
        "created_at":timestamp(row.get("created_at"))
    })
}

async fn create_resource(
    State(state): State<AppState>,
    auth: Auth,
    headers: HeaderMap,
    body: Result<Json<ResourceInput>, JsonRejection>,
) -> Reply {
    let input = payload(body)?;
    resource_key(&input.key)?;
    bounded(&input.description, "description", 4096, false)?;
    if !(1..=1000).contains(&input.capacity) {
        return Err(AppError::bad_request(
            "capacity must be between 1 and 1000.",
        ));
    }
    let mut mutation =
        Mutation::begin(&state, &auth, &headers, "POST /api/v1/resources", &input).await?;
    if mutation.actor.kind != "human" {
        return Err(AppError::forbidden(
            "A human operator creates globally shared resources.",
        ));
    }
    if let Some(value) = mutation.replay {
        return Ok(response(value));
    }
    if sqlx::query_scalar::<_, i64>("SELECT count(*) FROM resources WHERE key=?")
        .bind(&input.key)
        .fetch_one(&mut *mutation.tx)
        .await?
        > 0
    {
        return Err(AppError::conflict(
            "resource_exists",
            "A resource already uses this canonical key.",
        ));
    }
    let id = Uuid::new_v4().to_string();
    sqlx::query(
        "INSERT INTO resources(id,key,capacity,description,created_by,created_at) VALUES(?,?,?,?,?,?)",
    )
    .bind(&id)
    .bind(&input.key)
    .bind(input.capacity)
    .bind(&input.description)
    .bind(&mutation.actor.id)
    .bind(mutation.now)
    .execute(&mut *mutation.tx)
    .await?;
    let row =
        sqlx::query("SELECT x.id,x.key,x.capacity,x.description,x.created_at,0 AS held_units FROM resources x WHERE id=?")
            .bind(&id)
            .fetch_one(&mut *mutation.tx)
            .await?;
    let value = resource_value(&row);
    Ok(response(
        mutation
            .finish(value, None, "resource.created", &id)
            .await?,
    ))
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ReservationItemInput {
    resource_id: String,
    units: i64,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ReservationInput {
    generation: i64,
    items: Vec<ReservationItemInput>,
}

async fn create_reservation(
    State(state): State<AppState>,
    auth: Auth,
    Path((project, attempt)): Path<(String, String)>,
    headers: HeaderMap,
    body: Result<Json<ReservationInput>, JsonRejection>,
) -> Reply {
    let input = payload(body)?;
    if input.items.is_empty() || input.items.len() > 100 {
        return Err(AppError::bad_request("Provide 1–100 reservation items."));
    }
    let mut seen = std::collections::HashSet::new();
    for item in &input.items {
        uuid(&item.resource_id, "resource_id")?;
        if !(1..=1000).contains(&item.units) || !seen.insert(item.resource_id.as_str()) {
            return Err(AppError::bad_request(
                "Reservation resources must be distinct and request 1–1000 units.",
            ));
        }
    }
    let mut mutation = Mutation::begin(
        &state,
        &auth,
        &headers,
        &format!("POST /api/v1/projects/{project}/attempts/{attempt}/reservations"),
        &input,
    )
    .await?;
    let owned_attempt = owned(&mut mutation, &project, &attempt, input.generation).await?;
    if owned_attempt.mode != "work" {
        return Err(AppError::conflict(
            "recovery_unresolved",
            "Resolve recovery inspection before reserving resources.",
        ));
    }
    if let Some(value) = mutation.replay {
        return Ok(response(value));
    }
    if sqlx::query_scalar::<_, i64>(
        "SELECT count(*) FROM reservations WHERE attempt_id=? AND state='held'",
    )
    .bind(&attempt)
    .fetch_one(&mut *mutation.tx)
    .await?
        > 0
    {
        return Err(AppError::conflict(
            "reservation_exists",
            "This attempt already has a held reservation.",
        ));
    }
    for item in &input.items {
        let resource = sqlx::query("SELECT capacity FROM resources WHERE id=?")
            .bind(&item.resource_id)
            .fetch_optional(&mut *mutation.tx)
            .await?
            .ok_or_else(AppError::not_found)?;
        let used: i64 = sqlx::query_scalar(
            "SELECT COALESCE(sum(ri.units),0) FROM reservation_items ri \
             JOIN reservations r ON r.id=ri.reservation_id \
             WHERE ri.resource_id=? AND r.state='held'",
        )
        .bind(&item.resource_id)
        .fetch_one(&mut *mutation.tx)
        .await?;
        if used + item.units > resource.get::<i64, _>("capacity") {
            return Err(AppError::conflict(
                "resource_unavailable",
                "The complete resource set is not currently available.",
            ));
        }
    }
    let id = Uuid::new_v4().to_string();
    sqlx::query(
        "INSERT INTO reservations(id,project_id,attempt_id,generation,created_by,created_at) \
         VALUES(?,?,?,?,?,?)",
    )
    .bind(&id)
    .bind(&project)
    .bind(&attempt)
    .bind(input.generation)
    .bind(&mutation.actor.id)
    .bind(mutation.now)
    .execute(&mut *mutation.tx)
    .await?;
    for item in &input.items {
        sqlx::query(
            "INSERT INTO reservation_items(reservation_id,resource_id,units) VALUES(?,?,?)",
        )
        .bind(&id)
        .bind(&item.resource_id)
        .bind(item.units)
        .execute(&mut *mutation.tx)
        .await?;
    }
    let value = reservation(&mut mutation.tx, &project, &id, mutation.now).await?;
    Ok(response(
        mutation
            .finish(value, Some(&project), "reservation.created", &id)
            .await?,
    ))
}

async fn attempt_reservations(
    State(state): State<AppState>,
    _auth: Auth,
    Path((project, attempt)): Path<(String, String)>,
) -> Reply {
    let mut connection = state.pool.acquire().await?;
    let rows = sqlx::query(
        "SELECT id FROM reservations WHERE project_id=? AND attempt_id=? ORDER BY created_at,id LIMIT 201",
    )
    .bind(&project)
    .bind(&attempt)
    .fetch_all(&mut *connection)
    .await?;
    if rows.is_empty()
        && sqlx::query_scalar::<_, i64>("SELECT count(*) FROM attempts WHERE project_id=? AND id=?")
            .bind(&project)
            .bind(&attempt)
            .fetch_one(&mut *connection)
            .await?
            == 0
    {
        return Err(AppError::not_found());
    }
    let mut items = Vec::new();
    for row in rows.iter().take(200) {
        items.push(
            reservation(
                &mut connection,
                &project,
                &row.get::<String, _>("id"),
                state.now(),
            )
            .await?,
        );
    }
    Ok(response(json!({"items":items,"truncated":rows.len()>200})))
}

async fn reservations(
    State(state): State<AppState>,
    _auth: Auth,
    Path(project): Path<String>,
    Query(page): Query<Page>,
) -> Reply {
    let limit = page.limit()?;
    if sqlx::query_scalar::<_, i64>("SELECT count(*) FROM projects WHERE id=?")
        .bind(&project)
        .fetch_one(&state.pool)
        .await?
        == 0
    {
        return Err(AppError::not_found());
    }
    let mut connection = state.pool.acquire().await?;
    let rows = sqlx::query(
        "SELECT id FROM reservations WHERE project_id=? AND (? IS NULL OR id>?) ORDER BY id LIMIT ?",
    )
    .bind(&project)
    .bind(&page.cursor)
    .bind(&page.cursor)
    .bind(limit + 1)
    .fetch_all(&mut *connection)
    .await?;
    let mut items = Vec::new();
    for row in rows.iter().take(limit as usize) {
        items.push(
            reservation(
                &mut connection,
                &project,
                &row.get::<String, _>("id"),
                state.now(),
            )
            .await?,
        );
    }
    let next = (rows.len() > limit as usize)
        .then(|| {
            items
                .last()
                .and_then(|item| item["id"].as_str())
                .map(str::to_owned)
        })
        .flatten();
    Ok(response(json!({"items":items,"next_cursor":next})))
}

async fn reservation(
    connection: &mut SqliteConnection,
    project: &str,
    id: &str,
    now: i64,
) -> Result<Value, AppError> {
    let row = sqlx::query(
        "SELECT r.*,a.task_id,a.state AS attempt_state,a.expires_at,a.credential_id,t.current_attempt_id, \
         p.disabled_at,c.revoked_at AS credential_revoked,c.expires_at AS credential_expires, \
         s.closed_at AS session_closed,b.revoked_at AS browser_revoked,b.expires_at AS browser_expires \
         FROM reservations r JOIN attempts a ON a.id=r.attempt_id \
         JOIN tasks t ON t.id=a.task_id LEFT JOIN principals p ON p.id=a.owner_id \
         LEFT JOIN credentials c ON c.id=a.credential_id \
         LEFT JOIN agent_sessions s ON s.id=a.session_id AND s.credential_id=a.credential_id \
         LEFT JOIN browser_sessions b ON b.id=a.session_id \
         WHERE r.project_id=? AND r.id=?",
    )
    .bind(project)
    .bind(id)
    .fetch_optional(&mut *connection)
    .await?
    .ok_or_else(AppError::not_found)?;
    let state: String = row.get("state");
    let parent_live = row.get::<Option<i64>, _>("disabled_at").is_none()
        && if row.get::<Option<String>, _>("credential_id").is_some() {
            row.get::<Option<i64>, _>("credential_revoked").is_none()
                && row
                    .get::<Option<i64>, _>("credential_expires")
                    .is_none_or(|expires| expires > now)
                && row.get::<Option<i64>, _>("session_closed").is_none()
        } else {
            row.get::<Option<i64>, _>("browser_revoked").is_none()
                && row
                    .get::<Option<i64>, _>("browser_expires")
                    .is_some_and(|expires| expires > now)
        };
    let derived = if state != "held" {
        state.as_str()
    } else if row.get::<String, _>("attempt_state") == "active"
        && row.get::<i64, _>("expires_at") > now
        && row
            .get::<Option<String>, _>("current_attempt_id")
            .as_deref()
            == Some(row.get::<String, _>("attempt_id").as_str())
        && parent_live
    {
        "held"
    } else {
        "recovery_required"
    };
    let item_rows = sqlx::query(
        "SELECT ri.resource_id,ri.units,x.key FROM reservation_items ri \
         JOIN resources x ON x.id=ri.resource_id WHERE ri.reservation_id=? ORDER BY x.key",
    )
    .bind(id)
    .fetch_all(&mut *connection)
    .await?;
    let items = item_rows
        .iter()
        .map(|item| {
            json!({"resource_id":item.get::<String,_>("resource_id"),"key":item.get::<String,_>("key"),"units":item.get::<i64,_>("units")})
        })
        .collect::<Vec<_>>();
    Ok(json!({
        "id":row.get::<String,_>("id"),"project_id":project,
        "attempt_id":row.get::<String,_>("attempt_id"),"task_id":row.get::<String,_>("task_id"),
        "generation":row.get::<i64,_>("generation"),"state":derived,"stored_state":state,
        "items":items,"created_at":timestamp(row.get("created_at")),
        "released_at":row.get::<Option<i64>,_>("released_at").map(timestamp),
        "release_reason":row.get::<Option<String>,_>("release_reason"),
        "resolved_at":row.get::<Option<i64>,_>("resolved_at").map(timestamp),
        "resolution_reason":row.get::<Option<String>,_>("resolution_reason"),
        "resolution_evidence":row.get::<Option<String>,_>("resolution_evidence")
    }))
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ReleaseReservationInput {
    generation: i64,
    reason: String,
}

async fn release_reservation(
    State(state): State<AppState>,
    auth: Auth,
    Path((project, reservation_id)): Path<(String, String)>,
    headers: HeaderMap,
    body: Result<Json<ReleaseReservationInput>, JsonRejection>,
) -> Reply {
    let input = payload(body)?;
    bounded(&input.reason, "reason", 8192, true)?;
    let mut mutation = Mutation::begin(
        &state,
        &auth,
        &headers,
        &format!("POST /api/v1/projects/{project}/reservations/{reservation_id}/release"),
        &input,
    )
    .await?;
    let row = sqlx::query(
        "SELECT r.state,r.attempt_id,a.task_id,t.current_attempt_id FROM reservations r \
         JOIN attempts a ON a.id=r.attempt_id JOIN tasks t ON t.id=a.task_id \
         WHERE r.project_id=? AND r.id=?",
    )
    .bind(&project)
    .bind(&reservation_id)
    .fetch_optional(&mut *mutation.tx)
    .await?
    .ok_or_else(AppError::not_found)?;
    let current_id = row
        .get::<Option<String>, _>("current_attempt_id")
        .ok_or_else(|| {
            AppError::conflict(
                "lease_expired",
                "Claim recovery before releasing this reservation.",
            )
        })?;
    let current = owned(&mut mutation, &project, &current_id, input.generation).await?;
    let original_id: String = row.get("attempt_id");
    if current.id != original_id
        && (current.mode != "recovery" || current.task_id != row.get::<String, _>("task_id"))
    {
        return Err(AppError::forbidden(
            "Only the current owner of this task may release its reservation.",
        ));
    }
    if let Some(value) = mutation.replay {
        return Ok(response(value));
    }
    if row.get::<String, _>("state") != "held" {
        return Err(AppError::conflict(
            "reservation_not_held",
            "This reservation is no longer held.",
        ));
    }
    let unfinished: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM jobs WHERE reservation_id=? \
         AND state NOT IN ('succeeded','failed','not_started') AND reconciled_at IS NULL",
    )
    .bind(&reservation_id)
    .fetch_one(&mut *mutation.tx)
    .await?;
    if unfinished > 0 {
        return Err(AppError::conflict(
            "jobs_still_running",
            "Inspect every attached producer and record a terminal result before releasing this resource set.",
        ));
    }
    sqlx::query(
        "UPDATE reservations SET state='released',released_at=?,released_by=?,release_reason=? WHERE id=?",
    )
    .bind(mutation.now)
    .bind(&mutation.actor.id)
    .bind(&input.reason)
    .bind(&reservation_id)
    .execute(&mut *mutation.tx)
    .await?;
    let value = reservation(&mut mutation.tx, &project, &reservation_id, mutation.now).await?;
    Ok(response(
        mutation
            .finish(
                value,
                Some(&project),
                "reservation.released",
                &reservation_id,
            )
            .await?,
    ))
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ResolveReservationInput {
    reason: String,
    evidence: String,
}

async fn resolve_reservation(
    State(state): State<AppState>,
    auth: Auth,
    Path((project, reservation_id)): Path<(String, String)>,
    headers: HeaderMap,
    body: Result<Json<ResolveReservationInput>, JsonRejection>,
) -> Reply {
    let input = payload(body)?;
    bounded(&input.reason, "reason", 8192, true)?;
    bounded(&input.evidence, "evidence", 32768, true)?;
    let mut mutation = Mutation::begin(
        &state,
        &auth,
        &headers,
        &format!("POST /api/v1/projects/{project}/reservations/{reservation_id}/resolve"),
        &input,
    )
    .await?;
    if mutation.actor.kind != "human" {
        return Err(AppError::forbidden(
            "A human operator must resolve uncertain physical resources.",
        ));
    }
    if let Some(value) = mutation.replay {
        return Ok(response(value));
    }
    let stored: Option<String> =
        sqlx::query_scalar("SELECT state FROM reservations WHERE project_id=? AND id=?")
            .bind(&project)
            .bind(&reservation_id)
            .fetch_optional(&mut *mutation.tx)
            .await?;
    match stored.as_deref() {
        None => return Err(AppError::not_found()),
        Some("held") => {}
        _ => {
            return Err(AppError::conflict(
                "reservation_not_held",
                "This reservation is no longer held.",
            ));
        }
    }
    sqlx::query(
        "UPDATE jobs SET reconciled_at=?,reconciled_by=?,reconciliation_reason=?,reconciliation_evidence=? \
         WHERE reservation_id=? AND state NOT IN ('succeeded','failed','not_started') AND reconciled_at IS NULL",
    )
    .bind(mutation.now)
    .bind(&mutation.actor.id)
    .bind(&input.reason)
    .bind(&input.evidence)
    .bind(&reservation_id)
    .execute(&mut *mutation.tx)
    .await?;
    sqlx::query(
        "UPDATE reservations SET state='resolved',resolved_at=?,resolved_by=?,resolution_reason=?,resolution_evidence=? WHERE id=?",
    )
    .bind(mutation.now)
    .bind(&mutation.actor.id)
    .bind(&input.reason)
    .bind(&input.evidence)
    .bind(&reservation_id)
    .execute(&mut *mutation.tx)
    .await?;
    let value = reservation(&mut mutation.tx, &project, &reservation_id, mutation.now).await?;
    Ok(response(
        mutation
            .finish(
                value,
                Some(&project),
                "reservation.resolved",
                &reservation_id,
            )
            .await?,
    ))
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct JobInput {
    generation: i64,
    job_id: String,
    producer_id: String,
    runner_instance_id: String,
    workstation_id: String,
    label: String,
    source_revision: String,
    source_tree: String,
    reservation_id: String,
    reporter_id: String,
    reporter_proof: String,
    renew_for_seconds: i64,
}

async fn register_job(
    State(state): State<AppState>,
    auth: Auth,
    Path((project, attempt)): Path<(String, String)>,
    headers: HeaderMap,
    body: Result<Json<JobInput>, JsonRejection>,
) -> Reply {
    let input = payload(body)?;
    for (name, value) in [
        ("job_id", &input.job_id),
        ("producer_id", &input.producer_id),
        ("runner_instance_id", &input.runner_instance_id),
        ("reservation_id", &input.reservation_id),
        ("reporter_id", &input.reporter_id),
    ] {
        uuid(value, name)?;
    }
    if input.workstation_id.is_empty()
        || input.workstation_id.len() > 128
        || !input
            .workstation_id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"-_.".contains(&c))
    {
        return Err(AppError::bad_request(
            "Use the workstation identity registered with this harness session.",
        ));
    }
    bounded(&input.label, "label", 255, true)?;
    bounded(&input.source_revision, "source_revision", 255, true)?;
    bounded(&input.source_tree, "source_tree", 255, true)?;
    let proof = hex::decode(&input.reporter_proof)
        .map_err(|_| AppError::bad_request("reporter_proof must encode 32 random bytes as hex."))?;
    if proof.len() != 32
        || !input
            .reporter_proof
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
        || !(0..=3600).contains(&input.renew_for_seconds)
    {
        return Err(AppError::bad_request(
            "Use a 32-byte hexadecimal reporter_proof and renew_for_seconds between 0 and 3600.",
        ));
    }
    let mut mutation = Mutation::begin(
        &state,
        &auth,
        &headers,
        &format!("POST /api/v1/projects/{project}/attempts/{attempt}/jobs"),
        &input,
    )
    .await?;
    let owned_attempt = owned(&mut mutation, &project, &attempt, input.generation).await?;
    if owned_attempt.mode != "work" || mutation.actor.kind != "agent" {
        return Err(AppError::forbidden(
            "A live agent work attempt is required to register a producer.",
        ));
    }
    if let Some(value) = mutation.replay {
        return Ok(response(value));
    }
    let checkout =
        sqlx::query("SELECT workstation_id FROM checkouts WHERE project_id=? AND attempt_id=?")
            .bind(&project)
            .bind(&attempt)
            .fetch_optional(&mut *mutation.tx)
            .await?
            .ok_or_else(|| {
                AppError::conflict(
                    "checkout_required",
                    "Register the clean checkout before registering a producer.",
                )
            })?;
    let session_workstation: String =
        sqlx::query_scalar("SELECT workstation_id FROM agent_sessions WHERE id=?")
            .bind(&owned_attempt.session_id)
            .fetch_one(&mut *mutation.tx)
            .await?;
    if checkout.get::<String, _>("workstation_id") != input.workstation_id
        || session_workstation != input.workstation_id
    {
        return Err(AppError::forbidden(
            "The producer and checkout must use this session's registered workstation.",
        ));
    }
    if sqlx::query_scalar::<_, i64>(
        "SELECT count(*) FROM reservations WHERE project_id=? AND id=? AND attempt_id=? \
         AND generation=? AND state='held'",
    )
    .bind(&project)
    .bind(&input.reservation_id)
    .bind(&attempt)
    .bind(input.generation)
    .fetch_one(&mut *mutation.tx)
    .await?
        == 0
    {
        return Err(AppError::conflict(
            "reservation_required",
            "Register the producer against this attempt's held resource set.",
        ));
    }
    if sqlx::query_scalar::<_, i64>("SELECT count(*) FROM jobs WHERE id=? OR producer_id=?")
        .bind(&input.job_id)
        .bind(&input.producer_id)
        .fetch_one(&mut *mutation.tx)
        .await?
        > 0
        || sqlx::query_scalar::<_, i64>("SELECT count(*) FROM reporters WHERE id=?")
            .bind(&input.reporter_id)
            .fetch_one(&mut *mutation.tx)
            .await?
            > 0
    {
        return Err(AppError::conflict(
            "producer_registered",
            "A job, producer, or reporter already uses this identity. Inspect it; do not relaunch.",
        ));
    }
    let credential = owned_attempt
        .credential_id
        .as_deref()
        .ok_or_else(|| AppError::forbidden("Reporter credentials require an agent parent."))?;
    sqlx::query(
        "INSERT INTO jobs(id,producer_id,project_id,task_id,attempt_id,generation,runner_instance_id,workstation_id,label,source_revision,source_tree,reservation_id,created_at) \
         VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?)",
    )
    .bind(&input.job_id)
    .bind(&input.producer_id)
    .bind(&project)
    .bind(&owned_attempt.task_id)
    .bind(&attempt)
    .bind(input.generation)
    .bind(&input.runner_instance_id)
    .bind(&input.workstation_id)
    .bind(&input.label)
    .bind(&input.source_revision)
    .bind(&input.source_tree)
    .bind(&input.reservation_id)
    .bind(mutation.now)
    .execute(&mut *mutation.tx)
    .await?;
    let expires_at = mutation.now + REPORTER_LIFETIME_MS;
    let renew_until = mutation.now + input.renew_for_seconds * 1000;
    sqlx::query(
        "INSERT INTO reporters(id,job_id,principal_id,credential_id,session_id,proof_hash,expires_at,renew_until,created_at) \
         VALUES(?,?,?,?,?,?,?,?,?)",
    )
    .bind(&input.reporter_id)
    .bind(&input.job_id)
    .bind(&mutation.actor.id)
    .bind(credential)
    .bind(&owned_attempt.session_id)
    .bind(digest(&input.reporter_proof))
    .bind(expires_at)
    .bind(renew_until)
    .bind(mutation.now)
    .execute(&mut *mutation.tx)
    .await?;
    let job = job(&mut mutation.tx, &project, &input.job_id, mutation.now).await?;
    let value = json!({
        "job":job,
        "reporter":{"id":input.reporter_id,"expires_at":timestamp(expires_at),"renew_until":timestamp(renew_until)},
        "renew_after_seconds":if input.renew_for_seconds==0 { 0 } else { (input.renew_for_seconds/3).clamp(1,60) }
    });
    Ok(response(
        mutation
            .finish(value, Some(&project), "job.registered", &input.job_id)
            .await?,
    ))
}

async fn jobs(
    State(state): State<AppState>,
    _auth: Auth,
    Path(project): Path<String>,
    Query(page): Query<Page>,
) -> Reply {
    let limit = page.limit()?;
    if sqlx::query_scalar::<_, i64>("SELECT count(*) FROM projects WHERE id=?")
        .bind(&project)
        .fetch_one(&state.pool)
        .await?
        == 0
    {
        return Err(AppError::not_found());
    }
    let mut connection = state.pool.acquire().await?;
    let rows = sqlx::query(
        "SELECT id FROM jobs WHERE project_id=? AND (? IS NULL OR id>?) ORDER BY id LIMIT ?",
    )
    .bind(&project)
    .bind(&page.cursor)
    .bind(&page.cursor)
    .bind(limit + 1)
    .fetch_all(&mut *connection)
    .await?;
    let mut items = Vec::new();
    for row in rows.iter().take(limit as usize) {
        items.push(
            job(
                &mut connection,
                &project,
                &row.get::<String, _>("id"),
                state.now(),
            )
            .await?,
        );
    }
    let next = (rows.len() > limit as usize)
        .then(|| {
            items
                .last()
                .and_then(|item| item["id"].as_str())
                .map(str::to_owned)
        })
        .flatten();
    Ok(response(json!({"items":items,"next_cursor":next})))
}

async fn job_detail(
    State(state): State<AppState>,
    _auth: Auth,
    Path((project, id)): Path<(String, String)>,
) -> Reply {
    let mut connection = state.pool.acquire().await?;
    Ok(response(
        job(&mut connection, &project, &id, state.now()).await?,
    ))
}

async fn job(
    connection: &mut SqliteConnection,
    project: &str,
    id: &str,
    now: i64,
) -> Result<Value, AppError> {
    let row = sqlx::query(
        "SELECT j.*,r.id AS reporter_id,r.expires_at AS reporter_expires_at,r.renew_until \
         FROM jobs j JOIN reporters r ON r.job_id=j.id WHERE j.project_id=? AND j.id=?",
    )
    .bind(project)
    .bind(id)
    .fetch_optional(connection)
    .await?
    .ok_or_else(AppError::not_found)?;
    Ok(job_value(&row, now))
}

fn job_value(row: &sqlx::sqlite::SqliteRow, now: i64) -> Value {
    let last = row.get::<Option<i64>, _>("last_observed_at");
    json!({
        "id":row.get::<String,_>("id"),"producer_id":row.get::<String,_>("producer_id"),
        "project_id":row.get::<String,_>("project_id"),"task_id":row.get::<String,_>("task_id"),
        "attempt_id":row.get::<String,_>("attempt_id"),"generation":row.get::<i64,_>("generation"),
        "runner_instance_id":row.get::<String,_>("runner_instance_id"),"workstation_id":row.get::<String,_>("workstation_id"),
        "label":row.get::<String,_>("label"),"source_revision":row.get::<String,_>("source_revision"),
        "source_tree":row.get::<String,_>("source_tree"),"reservation_id":row.get::<String,_>("reservation_id"),
        "state":row.get::<String,_>("state"),"last_sequence":row.get::<i64,_>("last_sequence"),
        "last_observed_at":last.map(timestamp),
        "observation_freshness":match last { None=>"unobserved",Some(value) if now-value<=90_000=>"fresh",Some(_)=>"stale" },
        "observation_age_ms":last.map(|value|(now-value).max(0)),
        "pid":row.get::<Option<i64>,_>("pid"),"process_started_at":row.get::<Option<String>,_>("process_started_at"),
        "exit_code":row.get::<Option<i64>,_>("exit_code"),
        "inputs_unchanged":row.get::<Option<bool>,_>("inputs_unchanged"),"summary":row.get::<String,_>("summary"),
        "terminal":matches!(row.get::<String,_>("state").as_str(),"succeeded"|"failed"|"not_started"),
        "reconciled_at":row.get::<Option<i64>,_>("reconciled_at").map(timestamp),
        "reconciliation_reason":row.get::<Option<String>,_>("reconciliation_reason"),
        "reconciliation_evidence":row.get::<Option<String>,_>("reconciliation_evidence"),
        "reporter":{"id":row.get::<String,_>("reporter_id"),"expires_at":timestamp(row.get("reporter_expires_at")),"renew_until":timestamp(row.get("renew_until"))},
        "created_at":timestamp(row.get("created_at"))
    })
}

async fn reporter_detail(
    State(state): State<AppState>,
    auth: ReporterAuth,
    Path(id): Path<String>,
) -> Reply {
    require_reporter(&auth, &id)?;
    let now = state.now();
    let mut connection = state.pool.acquire().await?;
    auth.verify(&mut connection, now).await?;
    let row = reporter_job(&mut connection, &id).await?;
    let project: String = row.get("project_id");
    let job_id: String = row.get("id");
    let renewal = reporter_can_renew(&mut connection, &row, now).await?;
    let launch_allowed = reporter_can_launch(&mut connection, &row, now).await?;
    let lease_remaining_ms = if launch_allowed {
        (row.get::<i64, _>("attempt_expires_at") - now).max(0)
    } else {
        0
    };
    Ok(response(json!({
        "job":job(&mut connection,&project,&job_id,now).await?,
        "reporter":{"id":id,"expires_at":timestamp(row.get("reporter_expires_at")),"renew_until":timestamp(row.get("renew_until")),
            "observation_authorized":true,"renewal_authorized":renewal,"launch_allowed":launch_allowed,
            "lease_remaining_ms":lease_remaining_ms}
    })))
}

fn require_reporter(auth: &ReporterAuth, id: &str) -> Result<(), AppError> {
    if auth.id() != id {
        return Err(AppError::auth_required());
    }
    Ok(())
}

async fn reporter_job(
    connection: &mut SqliteConnection,
    reporter: &str,
) -> Result<sqlx::sqlite::SqliteRow, AppError> {
    sqlx::query(
        "SELECT j.*,r.id AS reporter_id,r.expires_at AS reporter_expires_at,r.renew_until, \
         a.expires_at AS attempt_expires_at, \
         r.principal_id,r.credential_id,r.session_id \
         FROM reporters r JOIN jobs j ON j.id=r.job_id JOIN attempts a ON a.id=j.attempt_id WHERE r.id=?",
    )
    .bind(reporter)
    .fetch_optional(connection)
    .await?
    .ok_or_else(AppError::not_found)
}

async fn reporter_can_renew(
    connection: &mut SqliteConnection,
    row: &sqlx::sqlite::SqliteRow,
    now: i64,
) -> Result<bool, AppError> {
    if row.get::<i64, _>("renew_until") <= now {
        return Ok(false);
    }
    Ok(sqlx::query_scalar::<_, i64>(
        "SELECT count(*) FROM attempts a JOIN tasks t ON t.id=a.task_id \
         JOIN reservations r ON r.id=? AND r.attempt_id=a.id AND r.state='held' \
         JOIN agent_sessions s ON s.id=a.session_id AND s.credential_id=a.credential_id \
         WHERE a.id=? AND a.project_id=? AND a.generation=? AND a.state='active' \
         AND a.expires_at>? AND t.current_attempt_id=a.id AND t.generation=a.generation \
         AND a.owner_id=? AND a.credential_id=? AND a.session_id=? AND s.closed_at IS NULL \
         AND a.mode='work' AND ? NOT IN ('succeeded','failed','not_started') AND ? IS NULL",
    )
    .bind(row.get::<String, _>("reservation_id"))
    .bind(row.get::<String, _>("attempt_id"))
    .bind(row.get::<String, _>("project_id"))
    .bind(row.get::<i64, _>("generation"))
    .bind(now)
    .bind(row.get::<String, _>("principal_id"))
    .bind(row.get::<String, _>("credential_id"))
    .bind(row.get::<String, _>("session_id"))
    .bind(row.get::<String, _>("state"))
    .bind(row.get::<Option<i64>, _>("reconciled_at"))
    .fetch_one(connection)
    .await?
        == 1)
}

async fn reporter_can_launch(
    connection: &mut SqliteConnection,
    row: &sqlx::sqlite::SqliteRow,
    now: i64,
) -> Result<bool, AppError> {
    if row.get::<String, _>("state") != "registered" {
        return Ok(false);
    }
    Ok(sqlx::query_scalar::<_, i64>(
        "SELECT count(*) FROM attempts a JOIN tasks t ON t.id=a.task_id \
         JOIN reservations r ON r.id=? AND r.attempt_id=a.id AND r.state='held' \
         JOIN agent_sessions s ON s.id=a.session_id AND s.credential_id=a.credential_id \
         WHERE a.id=? AND a.project_id=? AND a.generation=? AND a.mode='work' \
         AND a.state='active' AND a.expires_at>? AND t.current_attempt_id=a.id \
         AND t.generation=a.generation AND a.owner_id=? AND a.credential_id=? \
         AND a.session_id=? AND s.closed_at IS NULL",
    )
    .bind(row.get::<String, _>("reservation_id"))
    .bind(row.get::<String, _>("attempt_id"))
    .bind(row.get::<String, _>("project_id"))
    .bind(row.get::<i64, _>("generation"))
    .bind(now)
    .bind(row.get::<String, _>("principal_id"))
    .bind(row.get::<String, _>("credential_id"))
    .bind(row.get::<String, _>("session_id"))
    .fetch_one(connection)
    .await?
        == 1)
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ObservationInput {
    sequence: i64,
    producer_id: String,
    state: String,
    pid: Option<i64>,
    process_started_at: Option<String>,
    exit_code: Option<i64>,
    inputs_unchanged: Option<bool>,
    #[serde(default)]
    summary: String,
}

async fn observe(
    State(state): State<AppState>,
    auth: ReporterAuth,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Result<Json<ObservationInput>, JsonRejection>,
) -> Reply {
    require_reporter(&auth, &id)?;
    let input = payload(body)?;
    validate_observation(&input)?;
    let request_hash = digest(&serde_json::to_string(&input)?);
    let mut mutation = Mutation::begin_reporter(
        &state,
        &auth,
        &headers,
        &format!("POST /api/v1/reporters/{id}/observations"),
        &input,
    )
    .await?;
    if let Some(value) = mutation.replay {
        return Ok(response(value));
    }
    let row = reporter_job(&mut mutation.tx, &id).await?;
    if row.get::<String, _>("producer_id") != input.producer_id {
        return Err(AppError::conflict(
            "producer_mismatch",
            "This reporter is scoped to a different producer identity.",
        ));
    }
    let last = row.get::<i64, _>("last_sequence");
    if input.sequence <= last {
        let previous =
            sqlx::query("SELECT * FROM job_observations WHERE reporter_id=? AND sequence=?")
                .bind(&id)
                .bind(input.sequence)
                .fetch_optional(&mut *mutation.tx)
                .await?;
        if let Some(previous) = previous
            && previous.get::<String, _>("request_hash") == request_hash
        {
            let value = json!({"job":job(&mut mutation.tx,&row.get::<String,_>("project_id"),&row.get::<String,_>("id"),mutation.now).await?,"observation":observation_value(&previous),"replayed":true});
            return Ok(response(
                mutation
                    .finish(
                        value,
                        Some(&row.get::<String, _>("project_id")),
                        "job.observation_replayed",
                        &row.get::<String, _>("id"),
                    )
                    .await?,
            ));
        }
        return Err(AppError::conflict(
            "observation_sequence_conflict",
            "This observation sequence is stale or was already used with different data.",
        ));
    }
    if matches!(
        row.get::<String, _>("state").as_str(),
        "succeeded" | "failed" | "not_started"
    ) {
        return Err(AppError::conflict(
            "job_terminal",
            "A terminal producer result cannot be overwritten.",
        ));
    }
    if input.state == "not_started" {
        let launched: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM job_observations WHERE reporter_id=? \
             AND (state='running' OR pid IS NOT NULL OR process_started_at IS NOT NULL)",
        )
        .bind(&id)
        .fetch_one(&mut *mutation.tx)
        .await?;
        if launched > 0 {
            return Err(AppError::conflict(
                "job_already_launched",
                "not_started cannot replace evidence that the producer launched.",
            ));
        }
    }
    if input.state == "registered" && row.get::<i64, _>("last_sequence") > 0 {
        return Err(AppError::conflict(
            "job_state_regression",
            "A producer cannot return to registered after later observations.",
        ));
    }
    sqlx::query(
        "INSERT INTO job_observations(reporter_id,sequence,request_hash,producer_id,state,pid,process_started_at,exit_code,inputs_unchanged,summary,observed_at) \
         VALUES(?,?,?,?,?,?,?,?,?,?,?)",
    )
    .bind(&id)
    .bind(input.sequence)
    .bind(&request_hash)
    .bind(&input.producer_id)
    .bind(&input.state)
    .bind(input.pid)
    .bind(&input.process_started_at)
    .bind(input.exit_code)
    .bind(input.inputs_unchanged)
    .bind(&input.summary)
    .bind(mutation.now)
    .execute(&mut *mutation.tx)
    .await?;
    sqlx::query(
        "UPDATE jobs SET state=?,last_sequence=?,last_observed_at=?,pid=?,process_started_at=?,exit_code=?,inputs_unchanged=?,summary=? WHERE id=?",
    )
    .bind(&input.state)
    .bind(input.sequence)
    .bind(mutation.now)
    .bind(input.pid)
    .bind(&input.process_started_at)
    .bind(input.exit_code)
    .bind(input.inputs_unchanged)
    .bind(&input.summary)
    .bind(row.get::<String, _>("id"))
    .execute(&mut *mutation.tx)
    .await?;
    let observation =
        sqlx::query("SELECT * FROM job_observations WHERE reporter_id=? AND sequence=?")
            .bind(&id)
            .bind(input.sequence)
            .fetch_one(&mut *mutation.tx)
            .await?;
    let project: String = row.get("project_id");
    let job_id: String = row.get("id");
    let value = json!({"job":job(&mut mutation.tx,&project,&job_id,mutation.now).await?,"observation":observation_value(&observation)});
    Ok(response(
        mutation
            .finish(value, Some(&project), "job.observed", &job_id)
            .await?,
    ))
}

fn validate_observation(input: &ObservationInput) -> Result<(), AppError> {
    if input.sequence <= 0
        || ![
            "registered",
            "running",
            "succeeded",
            "failed",
            "unknown",
            "not_started",
        ]
        .contains(&input.state.as_str())
        || input.pid.is_some_and(|pid| pid <= 0)
    {
        return Err(AppError::bad_request(
            "Use a positive sequence, valid producer state, and a positive PID when present.",
        ));
    }
    uuid(&input.producer_id, "producer_id")?;
    bounded(&input.summary, "summary", 8192, false)?;
    if let Some(value) = &input.process_started_at {
        bounded(value, "process_started_at", 128, true)?;
    }
    match input.state.as_str() {
        "succeeded" if input.exit_code != Some(0) || input.inputs_unchanged.is_none() => {
            return Err(AppError::bad_request(
                "A succeeded observation requires exit_code 0 and a final input-stability result.",
            ));
        }
        "failed" if input.exit_code.is_none() || input.inputs_unchanged.is_none() => {
            return Err(AppError::bad_request(
                "A failed observation requires an exit_code and a final input-stability result.",
            ));
        }
        "not_started"
            if input.summary.trim().is_empty()
                || input.pid.is_some()
                || input.process_started_at.is_some()
                || input.exit_code.is_some() =>
        {
            return Err(AppError::bad_request(
                "not_started requires an explicit local failure summary and no process metadata.",
            ));
        }
        "registered" | "running" | "unknown" if input.exit_code.is_some() => {
            return Err(AppError::bad_request(
                "Only succeeded or failed observations include an exit_code.",
            ));
        }
        _ => {}
    }
    Ok(())
}

fn observation_value(row: &sqlx::sqlite::SqliteRow) -> Value {
    json!({
        "sequence":row.get::<i64,_>("sequence"),"producer_id":row.get::<String,_>("producer_id"),
        "state":row.get::<String,_>("state"),"pid":row.get::<Option<i64>,_>("pid"),
        "process_started_at":row.get::<Option<String>,_>("process_started_at"),
        "exit_code":row.get::<Option<i64>,_>("exit_code"),"inputs_unchanged":row.get::<Option<bool>,_>("inputs_unchanged"),
        "summary":row.get::<String,_>("summary"),"observed_at":timestamp(row.get("observed_at"))
    })
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ReporterRenewInput {
    generation: i64,
}

async fn reporter_renew(
    State(state): State<AppState>,
    auth: ReporterAuth,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Result<Json<ReporterRenewInput>, JsonRejection>,
) -> Reply {
    require_reporter(&auth, &id)?;
    let input = payload(body)?;
    let mut mutation = Mutation::begin_reporter(
        &state,
        &auth,
        &headers,
        &format!("POST /api/v1/reporters/{id}/renew"),
        &input,
    )
    .await?;
    let row = reporter_job(&mut mutation.tx, &id).await?;
    if row.get::<i64, _>("generation") != input.generation
        || row.get::<i64, _>("renew_until") <= mutation.now
    {
        return Err(AppError::conflict(
            "reporter_renewal_expired",
            "This reporter cannot renew task ownership. Continue observations without extending the lease.",
        ));
    }
    if !reporter_can_renew(&mut mutation.tx, &row, mutation.now).await? {
        return Err(AppError::conflict(
            "reporter_renewal_expired",
            "This reporter cannot renew task ownership. Continue observations without extending the lease.",
        ));
    }
    let project: String = row.get("project_id");
    let attempt_id: String = row.get("attempt_id");
    let current = owned(&mut mutation, &project, &attempt_id, input.generation).await?;
    if let Some(mut value) = mutation.replay {
        value["attempt"] = attempt_value(&current);
        value["lease_remaining_ms"] = json!((current.expires_at - mutation.now).max(0));
        value["replayed"] = json!(true);
        return Ok(response(value));
    }
    let lease_seconds: i64 = sqlx::query_scalar("SELECT lease_seconds FROM projects WHERE id=?")
        .bind(&project)
        .fetch_one(&mut *mutation.tx)
        .await?;
    let extension = (mutation.now + lease_seconds * 1000).min(row.get("renew_until"));
    let expires_at = current.expires_at.max(extension);
    sqlx::query("UPDATE attempts SET expires_at=?,last_heartbeat_at=? WHERE id=?")
        .bind(expires_at)
        .bind(mutation.now)
        .bind(&attempt_id)
        .execute(&mut *mutation.tx)
        .await?;
    let updated: Attempt = sqlx::query_as("SELECT * FROM attempts WHERE id=?")
        .bind(&attempt_id)
        .fetch_one(&mut *mutation.tx)
        .await?;
    let value = json!({
        "attempt":attempt_value(&updated),"lease_remaining_ms":expires_at-mutation.now,
        "renew_after_seconds":((expires_at-mutation.now)/3000).clamp(1,60)
    });
    Ok(response(
        mutation
            .finish(
                value,
                Some(&project),
                "attempt.reporter_renewed",
                &attempt_id,
            )
            .await?,
    ))
}

fn attempt_value(attempt: &Attempt) -> Value {
    json!({
        "id":attempt.id,"project_id":attempt.project_id,"task_id":attempt.task_id,
        "owner_id":attempt.owner_id,"session_id":attempt.session_id,"generation":attempt.generation,
        "state":attempt.state,"mode":attempt.mode,"expires_at":timestamp(attempt.expires_at),
        "last_heartbeat_at":timestamp(attempt.last_heartbeat_at),"last_progress_at":timestamp(attempt.last_progress_at),
        "created_at":timestamp(attempt.created_at),"ended_at":attempt.ended_at.map(timestamp),"outcome":attempt.outcome
    })
}

pub async fn ensure_attempt_quiescent(
    connection: &mut SqliteConnection,
    project: &str,
    task_id: &str,
) -> Result<(), AppError> {
    let holds: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM reservations r JOIN attempts a ON a.id=r.attempt_id \
         WHERE a.project_id=? AND a.task_id=? AND r.state='held'",
    )
    .bind(project)
    .bind(task_id)
    .fetch_one(&mut *connection)
    .await?;
    let jobs: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM jobs WHERE project_id=? AND task_id=? \
         AND state NOT IN ('succeeded','failed','not_started') AND reconciled_at IS NULL",
    )
    .bind(project)
    .bind(task_id)
    .fetch_one(&mut *connection)
    .await?;
    if holds > 0 || jobs > 0 {
        return Err(AppError::conflict(
            "attempt_evidence_unresolved",
            "Resource holds or producer observations still require inspection and resolution.",
        )
        .with_details(json!({"held_reservations":holds,"nonterminal_jobs":jobs})));
    }
    Ok(())
}

pub async fn task_evidence(
    connection: &mut SqliteConnection,
    project: &str,
    task_id: &str,
    now: i64,
) -> Result<Value, AppError> {
    let job_total: i64 =
        sqlx::query_scalar("SELECT count(*) FROM jobs WHERE project_id=? AND task_id=?")
            .bind(project)
            .bind(task_id)
            .fetch_one(&mut *connection)
            .await?;
    let reservation_total: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM reservations r JOIN attempts a ON a.id=r.attempt_id \
         WHERE a.project_id=? AND a.task_id=?",
    )
    .bind(project)
    .bind(task_id)
    .fetch_one(&mut *connection)
    .await?;
    let job_rows = sqlx::query(
        "SELECT id FROM jobs WHERE project_id=? AND task_id=? \
         ORDER BY (state NOT IN ('succeeded','failed','not_started') AND reconciled_at IS NULL) DESC, \
         created_at DESC,id DESC LIMIT 50",
    )
    .bind(project)
    .bind(task_id)
    .fetch_all(&mut *connection)
    .await?;
    let reservation_rows = sqlx::query(
        "SELECT r.id FROM reservations r JOIN attempts a ON a.id=r.attempt_id \
         WHERE a.project_id=? AND a.task_id=? ORDER BY (r.state='held') DESC,r.created_at DESC,r.id DESC LIMIT 50",
    )
    .bind(project)
    .bind(task_id)
    .fetch_all(&mut *connection)
    .await?;
    let mut jobs = Vec::new();
    for row in job_rows {
        jobs.push(job(connection, project, &row.get::<String, _>("id"), now).await?);
    }
    let mut reservations = Vec::new();
    for row in reservation_rows {
        reservations
            .push(reservation(connection, project, &row.get::<String, _>("id"), now).await?);
    }
    Ok(json!({
        "jobs":jobs,"reservations":reservations,
        "limits":{"jobs":50,"reservations":50},
        "job_total":job_total,"reservation_total":reservation_total,
        "jobs_truncated":job_total>50,"reservations_truncated":reservation_total>50
    }))
}
