//! A target that fails a required check must not stall every landing until a
//! human acts (P6). Three service-side parts, all driven by the integrator's
//! attestations:
//!
//! * A stored `fix_target` report with verdict `target_failing` opens one
//!   always-admitted fix task (origin `service`, class `fix_target`, priority
//!   0) in the same transaction, unless an unfinished fix task already covers
//!   the same repository, target branch and check ([`open_for_report`]).
//! * Once the same check passes on the target branch's tip in a run that
//!   started after the report was created, the integrator says so
//!   (`POST …/integrator/reports/{id}/target-passed`) and the service resolves
//!   the report with a note naming the passing run, so it neither blocks
//!   subjects nor waits for a human.
//! * A result whose check fails only because the target fails it too may
//!   proceed when every input path the roster declares for the check is
//!   identical in R and in the target tip X. The integrator records that
//!   exemption (`POST …/integrator/exemptions`); [`verify`] judges it, at
//!   recording and again when push authority is asked, from the service's own
//!   records: the roster's declared inputs, the receipts of the check on R,
//!   and the target report. A forged or stale record never passes.
use crate::{
    auth::Auth,
    error::AppError,
    integrator::require_integrator_project,
    integrator_authority::{FAILING, RosterCheck, failure_records},
    mutation::Mutation,
    response,
    state::AppState,
    workflow::{bounded, payload, revision},
};
use axum::{
    Json, Router,
    extract::{Path, State, rejection::JsonRejection},
    http::HeaderMap,
    routing::post,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{Row, SqliteConnection, sqlite::SqliteRow};

type Reply = Result<Json<Value>, AppError>;

/// Conclusions that pass a required check.
const PASSING: &[&str] = &["success", "skipped", "neutral"];
/// Fix tasks are more urgent than any ordinary work (lower sorts first).
const FIX_PRIORITY: i64 = 0;

/// The routes of the target-failure follow-through.
pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/api/v1/projects/{p}/integrator/reports/{id}/target-passed",
            post(target_passed),
        )
        .route(
            "/api/v1/projects/{p}/integrator/exemptions",
            post(record_exemption),
        )
}

/// A text field of a report's details, or "" when absent.
fn text<'a>(details: &'a Value, key: &str) -> &'a str {
    details[key].as_str().unwrap_or_default()
}

/// True for a `fix_target` report whose verdict is `target_failing`.
fn is_target_failing(kind: &str, details: &Value) -> bool {
    kind == "fix_target" && details["verdict"] == "target_failing"
}

/// The unfinished fix task covering a repository, branch and check, if any.
async fn unfinished_fix_task(
    c: &mut SqliteConnection,
    p: &str,
    (repository, branch, check): (&str, &str, &str),
) -> Result<Option<String>, AppError> {
    Ok(sqlx::query_scalar(
        "SELECT f.task_id FROM integrator_fix_tasks f JOIN tasks t ON t.id=f.task_id \
         WHERE f.project_id=? AND f.repository_url=? AND f.target_branch=? AND f.check_name=? \
         AND t.lifecycle IN ('planned','open') AND t.archived_at IS NULL \
         ORDER BY f.created_at LIMIT 1",
    )
    .bind(p)
    .bind(repository)
    .bind(branch)
    .bind(check)
    .fetch_optional(&mut *c)
    .await?)
}

/// Title, description and acceptance criteria of a fix task for `details`.
fn fix_task_fields(report_id: &str, details: &Value) -> (String, String, Value) {
    let (check, branch) = (text(details, "check_name"), text(details, "target_branch"));
    let run = &details["target_result"]["run_id"];
    let run = run
        .as_i64()
        .map_or_else(|| "unknown".to_owned(), |id| id.to_string());
    let description = format!(
        "The required check \"{check}\" fails on the tip of {branch} in {repo}. Target revision: \
         {x}. Failing run: {run}. Integrator report: {report_id}.\n\nFix the target so that the \
         check passes again on the branch tip, without weakening or removing the check. The \
         service resolves the report once the check passes on the tip in a later run.",
        repo = text(details, "repository_url"),
        x = text(details, "x"),
    );
    let title: String = format!("Fix failing check \"{check}\" on {branch}")
        .chars()
        .take(300)
        .collect();
    let acceptance = json!([format!(
        "The required check \"{check}\" passes on the tip of {branch}"
    )]);
    (title, description, acceptance)
}

/// Inserts the fix task and the row that ties it to its target and report.
async fn insert_fix_task(
    m: &mut Mutation,
    p: &str,
    report_id: &str,
    details: &Value,
) -> Result<String, AppError> {
    let id = uuid::Uuid::new_v4().to_string();
    let (title, description, acceptance) = fix_task_fields(report_id, details);
    sqlx::query("INSERT INTO tasks(id,project_id,title,description,acceptance_json,kind,priority,lifecycle,created_at,ready_since) VALUES(?,?,?,?,?,'code',?,'open',?,?)")
        .bind(&id).bind(p).bind(&title).bind(&description).bind(acceptance.to_string())
        .bind(FIX_PRIORITY).bind(m.now).bind(m.now).execute(&mut *m.tx).await?;
    crate::admission::record_service(m, &id, Some("fix_target")).await?;
    crate::coordination::save_task_revision(m, p, &id).await?;
    sqlx::query("INSERT INTO integrator_fix_tasks(task_id,project_id,report_id,repository_url,target_branch,check_name,created_at) VALUES(?,?,?,?,?,?,?)")
        .bind(&id).bind(p).bind(report_id).bind(text(details, "repository_url"))
        .bind(text(details, "target_branch")).bind(text(details, "check_name")).bind(m.now)
        .execute(&mut *m.tx).await?;
    sqlx::query("INSERT INTO events(project_id,actor_id,kind,record_id,data_json,created_at,credential_class) VALUES(?,?,'integrator.fix_task_opened',?,?,?,'integrator')")
        .bind(p).bind(&m.actor.id).bind(&id)
        .bind(json!({"report_id": report_id}).to_string()).bind(m.now)
        .execute(&mut *m.tx).await?;
    Ok(id)
}

/// Opens the fix task for a report this call just stored, when it is a
/// `target_failing` report naming its repository, branch and check and no
/// unfinished fix task covers them. Returns the new task's id. A report that
/// was already stored (a deduplicated replay) never reaches this function.
pub(crate) async fn open_for_report(
    m: &mut Mutation,
    p: &str,
    report: &SqliteRow,
) -> Result<Option<String>, AppError> {
    let details: Value = serde_json::from_str(report.get("details_json"))?;
    if !is_target_failing(report.get("kind"), &details) {
        return Ok(None);
    }
    let target = (
        text(&details, "repository_url"),
        text(&details, "target_branch"),
        text(&details, "check_name"),
    );
    if [target.0, target.1, target.2].contains(&"") {
        return Ok(None);
    }
    if unfinished_fix_task(&mut m.tx, p, target).await?.is_some() {
        return Ok(None);
    }
    let id: String = report.get("id");
    insert_fix_task(m, p, &id, &details).await.map(Some)
}

/// The open fix task of a target and check, for the task view and the
/// digest: `{task_id, title}` or null.
pub(crate) async fn fix_task_value(
    c: &mut SqliteConnection,
    p: &str,
    (repository, branch, check): (&str, &str, &str),
) -> Result<Value, AppError> {
    let Some(id) = unfinished_fix_task(c, p, (repository, branch, check)).await? else {
        return Ok(Value::Null);
    };
    let title: Option<String> = sqlx::query_scalar("SELECT title FROM tasks WHERE id=?")
        .bind(&id)
        .fetch_optional(&mut *c)
        .await?;
    Ok(json!({"task_id": id, "title": title}))
}

/// The passing run the integrator saw on the target tip.
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct TargetPassed {
    /// The target tip the check passed on.
    x: String,
    run_id: i64,
    run_attempt: i64,
    conclusion: String,
    /// RFC 3339 time the passing run started.
    started_at: String,
}

impl TargetPassed {
    /// Checks every bound and returns the start time in milliseconds.
    fn validate(&self) -> Result<i64, AppError> {
        revision(&self.x, "x")?;
        if self.run_id <= 0 || self.run_attempt <= 0 {
            return Err(AppError::bad_request(
                "run_id and run_attempt must be positive.",
            ));
        }
        if !PASSING.contains(&self.conclusion.as_str()) {
            return Err(AppError::bad_request(
                "conclusion must be success, skipped or neutral.",
            ));
        }
        bounded(&self.started_at, "started_at", 64, true)?;
        let started = chrono::DateTime::parse_from_rfc3339(&self.started_at)
            .map_err(|_| AppError::bad_request("started_at must be an RFC 3339 timestamp."))?;
        Ok(started.timestamp_millis())
    }
}

/// Loads an open `target_failing` report of project `p`, or refuses.
async fn open_target_failing(
    c: &mut SqliteConnection,
    p: &str,
    id: &str,
) -> Result<(SqliteRow, Value), AppError> {
    let row = sqlx::query("SELECT * FROM integrator_reports WHERE id=? AND project_id=?")
        .bind(id)
        .bind(p)
        .fetch_optional(&mut *c)
        .await?
        .ok_or_else(AppError::not_found)?;
    let details: Value = serde_json::from_str(row.get("details_json"))?;
    if !is_target_failing(row.get("kind"), &details) {
        return Err(AppError::conflict(
            "report_not_resolvable",
            "Only a fix_target report with verdict target_failing resolves itself.",
        ));
    }
    if row.get::<Option<i64>, _>("resolved_at").is_some() {
        return Err(AppError::conflict(
            "report_already_resolved",
            "This report was already resolved.",
        ));
    }
    Ok((row, details))
}

/// `POST …/integrator/reports/{id}/target-passed`: the same check passed on
/// the target tip in a run that started after the report was created, so the
/// service resolves the report itself, with a note naming the passing run.
async fn target_passed(
    State(s): State<AppState>,
    auth: Auth,
    Path((p, id)): Path<(String, String)>,
    headers: HeaderMap,
    body: Result<Json<TargetPassed>, JsonRejection>,
) -> Reply {
    let input = payload(body)?;
    let started = input.validate()?;
    let op = format!("POST /api/v1/projects/{p}/integrator/reports/{id}/target-passed");
    let mut m = Mutation::begin(&s, &auth, &headers, &op, &input).await?;
    if let Some(v) = m.replay.take() {
        return Ok(response(v));
    }
    require_integrator_project(&mut m.tx, &p).await?;
    let (row, details) = open_target_failing(&mut m.tx, &p, &id).await?;
    if started <= row.get::<i64, _>("created_at") {
        return Err(AppError::conflict(
            "pass_not_fresh",
            "The passing run started before this report was created.",
        ));
    }
    let note = format!(
        "Resolved automatically: check {} passed on target {} (run {} attempt {}, started {}) \
         after this report was created.",
        text(&details, "check_name"),
        input.x,
        input.run_id,
        input.run_attempt,
        input.started_at
    );
    sqlx::query(
        "UPDATE integrator_reports SET resolved_at=?,resolved_by=?,resolution_note=? WHERE id=?",
    )
    .bind(m.now)
    .bind(&m.actor.id)
    .bind(&note)
    .bind(&id)
    .execute(&mut *m.tx)
    .await?;
    let value = json!({"id": id, "resolved": true, "resolution_note": note});
    Ok(response(
        m.finish(value, Some(&p), "integrator.report_auto_resolved", &id)
            .await?,
    ))
}

/// One declared input path with its blob id in R and in X (`None` when the
/// path is absent there).
#[derive(Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct InputBlobs {
    path: String,
    r_blob: Option<String>,
    x_blob: Option<String>,
}

/// The integrator's record that a failing check on R is exempt.
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ExemptionInput {
    result_id: String,
    identity: String,
    check_name: String,
    r: String,
    x: String,
    report_id: String,
    inputs: Vec<InputBlobs>,
}

impl ExemptionInput {
    /// Checks identities and bounds before any state is read.
    fn validate(&self) -> Result<(), AppError> {
        bounded(&self.result_id, "result_id", 100, true)?;
        bounded(&self.identity, "identity", 200, true)?;
        bounded(&self.check_name, "check_name", 200, true)?;
        bounded(&self.report_id, "report_id", 100, true)?;
        revision(&self.r, "r")?;
        revision(&self.x, "x")?;
        if self.inputs.is_empty() || self.inputs.len() > coordinator_core::MAX_CHECK_INPUTS {
            return Err(AppError::bad_request(
                "inputs must list the check's declared paths.",
            ));
        }
        for input in &self.inputs {
            for blob in [&input.r_blob, &input.x_blob].into_iter().flatten() {
                revision(blob, "blob")?;
            }
        }
        Ok(())
    }
}

/// A stored exemption.
pub(crate) struct Exemption {
    pub id: String,
    pub identity: String,
    pub check_name: String,
    pub r: String,
    pub x: String,
    pub report_id: String,
    pub inputs: Vec<InputBlobs>,
}

impl Exemption {
    /// Reads a stored `integrator_exemptions` row.
    fn from_row(row: &SqliteRow) -> Result<Self, AppError> {
        Ok(Self {
            id: row.get("id"),
            identity: row.get("identity"),
            check_name: row.get("check_name"),
            r: row.get("r"),
            x: row.get("x"),
            report_id: row.get("report_id"),
            inputs: serde_json::from_str(row.get("inputs_json"))?,
        })
    }
}

/// The input paths the current workflow policy declares for `identity`.
async fn declared_inputs(
    c: &mut SqliteConnection,
    p: &str,
    identity: &str,
) -> Result<Vec<String>, AppError> {
    let raw: Option<String> =
        sqlx::query_scalar("SELECT required_checks_json FROM workflow_policies WHERE project_id=?")
            .bind(p)
            .fetch_optional(&mut *c)
            .await?;
    let checks: Vec<coordinator_core::RequiredCheck> =
        serde_json::from_str(raw.as_deref().unwrap_or("[]"))?;
    let declaring = checks
        .into_iter()
        .find(|check| check.identity == identity && !check.inputs.is_empty());
    Ok(declaring.map(|check| check.inputs).unwrap_or_default())
}

/// Why an exemption does not hold, or `None` when its recorded inputs are
/// exactly the declared ones and identical in R and X.
fn inputs_refusal(declared: &[String], recorded: &[InputBlobs]) -> Option<&'static str> {
    if declared.is_empty() {
        return Some("the roster declares no inputs for this check");
    }
    let mut paths: Vec<&str> = recorded.iter().map(|i| i.path.as_str()).collect();
    paths.sort_unstable();
    let mut wanted: Vec<&str> = declared.iter().map(String::as_str).collect();
    wanted.sort_unstable();
    if paths != wanted {
        return Some("the compared paths are not the declared inputs");
    }
    if recorded.iter().any(|i| i.r_blob != i.x_blob) {
        return Some("a declared input differs between R and the target tip");
    }
    None
}

/// Why the target report does not support an exemption of `check` on `x`,
/// or `None`: it must be an open `target_failing` report of this project for
/// the same check and tip, whose attested X result is a failure.
async fn report_refusal(
    c: &mut SqliteConnection,
    p: &str,
    (report, check_name, x): (&str, &str, &str),
) -> Result<Option<&'static str>, AppError> {
    let row = sqlx::query(
        "SELECT kind,details_json,resolved_at FROM integrator_reports WHERE id=? AND project_id=?",
    )
    .bind(report)
    .bind(p)
    .fetch_optional(&mut *c)
    .await?;
    let Some(row) = row else {
        return Ok(Some("the target report does not exist"));
    };
    let details: Value = serde_json::from_str(row.get("details_json"))?;
    let failed = details["target_result"]["conclusion"]
        .as_str()
        .is_some_and(|conclusion| FAILING.contains(&conclusion));
    Ok(if !is_target_failing(row.get("kind"), &details) {
        Some("the report is not a target_failing report")
    } else if row.get::<Option<i64>, _>("resolved_at").is_some() {
        Some("the target report was resolved")
    } else if text(&details, "check_name") != check_name || text(&details, "x") != x {
        Some("the target report names another check or revision")
    } else if !failed || details["target_result"]["stale"] == true {
        Some("the target report has no fresh failing result on the target")
    } else {
        None
    })
}

/// Why `exemption` does not exempt `check` on result `result`, or `None`
/// when it holds. Everything is judged from the service's own records: the
/// current workflow policy's declared inputs, the result, the receipts of the
/// check on R (a reproduced failure: the deciding attempt and one more
/// failed) and the target report.
pub(crate) async fn verify(
    c: &mut SqliteConnection,
    p: &str,
    result: &SqliteRow,
    check: &RosterCheck,
    exemption: &Exemption,
) -> Result<Option<&'static str>, AppError> {
    let (r, t0): (String, String) = (result.get("r"), result.get("t0"));
    if exemption.r != r || exemption.x != t0 {
        return Ok(Some("the exemption names another R or target tip"));
    }
    if exemption.identity != check.identity || exemption.check_name != check.check_name {
        return Ok(Some("the exemption names another check"));
    }
    let declared = declared_inputs(c, p, &check.identity).await?;
    if let Some(reason) = inputs_refusal(&declared, &exemption.inputs) {
        return Ok(Some(reason));
    }
    let id: String = result.get("id");
    let record = failure_records(c, &id, std::slice::from_ref(check)).await?;
    let reproduced = record.first().is_some_and(|f| {
        f.failures >= 2 && f.deciding.as_deref().is_some_and(|d| FAILING.contains(&d))
    });
    if !reproduced {
        return Ok(Some("the check has no reproduced failure on R"));
    }
    report_refusal(c, p, (&exemption.report_id, &check.check_name, &t0)).await
}

/// The stored exemption of `identity` on `result`, if any.
async fn stored(
    c: &mut SqliteConnection,
    result: &str,
    identity: &str,
) -> Result<Option<Exemption>, AppError> {
    let row = sqlx::query("SELECT * FROM integrator_exemptions WHERE result_id=? AND identity=?")
        .bind(result)
        .bind(identity)
        .fetch_optional(&mut *c)
        .await?;
    row.as_ref().map(Exemption::from_row).transpose()
}

/// The verdict of push authority on a failing roster check of `result`:
/// the verified exemption, or the reason there is none (`None` reason when no
/// exemption was recorded at all).
pub(crate) async fn exemption_for(
    c: &mut SqliteConnection,
    p: &str,
    result: &SqliteRow,
    check: &RosterCheck,
) -> Result<Result<Exemption, Option<&'static str>>, AppError> {
    let id: String = result.get("id");
    let Some(exemption) = stored(c, &id, &check.identity).await? else {
        return Ok(Err(None));
    };
    Ok(match verify(c, p, result, check, &exemption).await? {
        None => Ok(exemption),
        reason => Err(reason),
    })
}

impl Exemption {
    /// The exemption as a record for responses and views.
    pub(crate) fn value(&self) -> Value {
        json!({"exemption_id": self.id, "identity": self.identity,
            "check_name": self.check_name, "r": self.r, "x": self.x,
            "report_id": self.report_id, "inputs": self.inputs})
    }
}

/// `POST …/integrator/exemptions`: records that a failing check on R is
/// exempt. The service verifies the facts it can before storing; a replay of
/// the same (result, check) returns the stored record.
async fn record_exemption(
    State(s): State<AppState>,
    auth: Auth,
    Path(p): Path<String>,
    headers: HeaderMap,
    body: Result<Json<ExemptionInput>, JsonRejection>,
) -> Reply {
    let input = payload(body)?;
    input.validate()?;
    let op = format!("POST /api/v1/projects/{p}/integrator/exemptions");
    let mut m = Mutation::begin(&s, &auth, &headers, &op, &input).await?;
    if let Some(v) = m.replay.take() {
        return Ok(response(v));
    }
    require_integrator_project(&mut m.tx, &p).await?;
    let result = sqlx::query("SELECT * FROM integrator_results WHERE id=? AND project_id=?")
        .bind(&input.result_id)
        .bind(&p)
        .fetch_optional(&mut *m.tx)
        .await?
        .ok_or_else(AppError::not_found)?;
    let roster = crate::integrator_authority::parse_roster(&serde_json::from_str(
        result.get("roster_json"),
    )?)?;
    let check = roster
        .into_iter()
        .find(|check| check.identity == input.identity)
        .ok_or_else(AppError::not_found)?;
    let id = uuid::Uuid::new_v4().to_string();
    let proposed = Exemption {
        id: id.clone(),
        identity: input.identity.clone(),
        check_name: input.check_name.clone(),
        r: input.r.clone(),
        x: input.x.clone(),
        report_id: input.report_id.clone(),
        inputs: input.inputs.clone(),
    };
    if let Some(reason) = verify(&mut m.tx, &p, &result, &check, &proposed).await? {
        return Err(AppError::conflict(
            "exemption_refused",
            "The service cannot verify this exemption from its own records.",
        )
        .with_details(json!({"reason": reason})));
    }
    sqlx::query("INSERT INTO integrator_exemptions(id,project_id,result_id,identity,check_name,r,x,report_id,inputs_json,created_by,created_at) VALUES(?,?,?,?,?,?,?,?,?,?,?) ON CONFLICT(result_id,identity) DO NOTHING")
        .bind(&id).bind(&p).bind(&input.result_id).bind(&input.identity).bind(&input.check_name)
        .bind(&input.r).bind(&input.x).bind(&input.report_id)
        .bind(serde_json::to_string(&input.inputs)?).bind(&m.actor.id).bind(m.now)
        .execute(&mut *m.tx).await?;
    let stored = stored(&mut m.tx, &input.result_id, &input.identity)
        .await?
        .ok_or_else(AppError::not_found)?;
    let value = stored.value();
    let record = stored.id;
    Ok(response(
        m.finish(value, Some(&p), "integrator.exemption_recorded", &record)
            .await?,
    ))
}

/// The exemptions recorded on the results of `submission`, newest first, each
/// with the open fix task of its target, for the task view.
pub(crate) async fn submission_exemptions(
    c: &mut SqliteConnection,
    p: &str,
    submission: &str,
) -> Result<Vec<Value>, AppError> {
    let rows = sqlx::query(
        "SELECT e.*,s.repository_url AS repository_url,s.target_branch AS target_branch \
         FROM integrator_exemptions e JOIN integrator_results r ON r.id=e.result_id \
         JOIN submissions s ON s.id=r.submission_id \
         WHERE r.submission_id=? AND e.project_id=? ORDER BY e.created_at DESC,e.id LIMIT 20",
    )
    .bind(submission)
    .bind(p)
    .fetch_all(&mut *c)
    .await?;
    exemption_views(c, p, &rows).await
}

/// The exemptions recorded since `since`, newest first, for the digest.
pub(crate) async fn exemptions_since(
    c: &mut SqliteConnection,
    p: &str,
    since: i64,
    limit: i64,
) -> Result<Vec<Value>, AppError> {
    let rows = sqlx::query(
        "SELECT e.*,s.repository_url AS repository_url,s.target_branch AS target_branch \
         FROM integrator_exemptions e JOIN integrator_results r ON r.id=e.result_id \
         JOIN submissions s ON s.id=r.submission_id \
         WHERE e.project_id=? AND e.created_at>? ORDER BY e.created_at DESC,e.id LIMIT ?",
    )
    .bind(p)
    .bind(since)
    .bind(limit)
    .fetch_all(&mut *c)
    .await?;
    exemption_views(c, p, &rows).await
}

/// Each exemption row as a view: the record, its subject task and the open
/// fix task of the target, so the state is visible without integrator logs.
async fn exemption_views(
    c: &mut SqliteConnection,
    p: &str,
    rows: &[SqliteRow],
) -> Result<Vec<Value>, AppError> {
    let mut views = Vec::with_capacity(rows.len());
    for row in rows {
        let exemption = Exemption::from_row(row)?;
        let target = (
            row.get::<String, _>("repository_url"),
            row.get::<String, _>("target_branch"),
            exemption.check_name.clone(),
        );
        let fix = fix_task_value(c, p, (&target.0, &target.1, &target.2)).await?;
        let subject = subject_task(c, row.get("result_id")).await?;
        let mut view = exemption.value();
        view["result_id"] = row.get::<String, _>("result_id").into();
        view["recorded_at"] = coordinator_core::timestamp(row.get("created_at")).into();
        view["target_branch"] = target.1.into();
        view["subject_task"] = subject;
        view["target_fix_task"] = fix;
        views.push(view);
    }
    Ok(views)
}

/// The id and title of the task a result belongs to.
async fn subject_task(c: &mut SqliteConnection, result: &str) -> Result<Value, AppError> {
    let row = sqlx::query("SELECT t.id,t.title FROM integrator_results r JOIN submissions s ON s.id=r.submission_id JOIN tasks t ON t.id=s.task_id WHERE r.id=?")
        .bind(result).fetch_optional(&mut *c).await?;
    Ok(row.map_or(Value::Null, |row| {
        json!({"task_id": row.get::<String, _>("id"), "title": row.get::<String, _>("title")})
    }))
}
