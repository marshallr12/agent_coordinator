//! Exact-window autonomy metrics: `GET …/telemetry/autonomy/window`.
//!
//! The rolling-days read in [`crate::telemetry`] answers "how did the last N
//! days go" and counts only integrator reports and parked subjects as
//! human-required. This read answers for a fixed interval whose two bounds the
//! caller names, so an audit can reproduce a figure whenever it asks. Every
//! windowed figure is computed from records stamped inside `[start_ms,
//! end_ms]` (both bounds included) and from records stamped no later than
//! `end_ms`; nothing windowed reads the clock. The one exception is the
//! `current_state` section, which says so.
//!
//! The reply keeps its sources apart instead of folding them into one number:
//! raw audit events by credential class, the integrator's human-required
//! reports and parks, historical stall episodes, reserved (canary) exclusions,
//! throughput and latency, and push coverage. `hri` states whether every
//! source it needs was available and withholds its total when one was not.
use crate::{
    auth::Auth,
    error::AppError,
    response,
    state::AppState,
    telemetry::{parked_now, require_project},
};
use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
};
use coordinator_core::timestamp;
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::{Row, SqliteConnection};
use std::collections::BTreeMap;

type Reply = Result<Json<Value>, AppError>;

const DAY_MS: i64 = 86_400_000;
/// The widest window one read covers.
const MAX_WINDOW_MS: i64 = 92 * DAY_MS;
/// Episodes listed per section; the counts are always complete.
const MAX_ITEMS: usize = 100;
/// Consecutive attempts that ended without a submission before a task is
/// stalled, as the attention digest counts them.
const STALL_ATTEMPTS: usize = crate::attention::STALL_ATTEMPTS as usize;
/// Attempt states that end without a submission.
const FAILED_STATES: [&str; 4] = ["released", "blocked", "expired", "canceled"];
/// The records of canary (budget-exempt) tasks that events may name: the task,
/// its submissions and its attempts.
macro_rules! canary_records {
    () => {
        "(SELECT id FROM tasks WHERE budget_exempt IS NOT NULL \
          UNION SELECT s.id FROM submissions s JOIN tasks t ON t.id=s.task_id WHERE t.budget_exempt IS NOT NULL \
          UNION SELECT a.id FROM attempts a JOIN tasks t ON t.id=a.task_id WHERE t.budget_exempt IS NOT NULL)"
    };
}

/// Events a delegated job reporter records; its credential has no class.
macro_rules! reporter_kinds {
    () => {
        "('job.observed','job.observation_replayed','attempt.reporter_renewed')"
    };
}

#[derive(Deserialize)]
pub(crate) struct WindowQuery {
    start_ms: Option<i64>,
    end_ms: Option<i64>,
}

/// The validated bounds: `0 <= start <= end <= now`, at most 92 days apart.
fn bounds(query: &WindowQuery, now: i64) -> Result<(i64, i64), AppError> {
    let (Some(start), Some(end)) = (query.start_ms, query.end_ms) else {
        return Err(AppError::bad_request(
            "start_ms and end_ms are both required (milliseconds since the epoch).",
        ));
    };
    if start < 0 || end < start {
        return Err(AppError::bad_request(
            "start_ms must not be negative and end_ms must not precede start_ms.",
        ));
    }
    if end - start > MAX_WINDOW_MS {
        return Err(AppError::bad_request(
            "The window may span at most 92 days.",
        ));
    }
    if end > now {
        return Err(AppError::new(
            StatusCode::BAD_REQUEST,
            "window_not_closed",
            "end_ms is in the future; an exact window must already be closed.",
        ));
    }
    Ok((start, end))
}

/// Nearest-rank percentile of the sorted `values`, in seconds.
fn percentile(values: &[i64], pct: usize) -> Option<f64> {
    let rank = (pct * values.len())
        .div_ceil(100)
        .clamp(1, values.len().max(1));
    values.get(rank - 1).map(|ms| *ms as f64 / 1000.0)
}

/// Latency figures of `latencies` (milliseconds), sorted in place.
fn latency(latencies: &mut [i64]) -> Value {
    latencies.sort_unstable();
    json!({
        "count": latencies.len(),
        "median_seconds": percentile(latencies, 50),
        "p90_seconds": percentile(latencies, 90),
        "max_seconds": latencies.last().map(|ms| *ms as f64 / 1000.0),
    })
}

/// One completed task in the window.
struct Completion {
    origin: String,
    kind: String,
    canary: bool,
    external: bool,
    created_at: i64,
    at: i64,
    day: String,
    integrator_push: bool,
    already_contained: bool,
    agent_published: bool,
}

/// Tasks whose workflow reached done in the window, plus tasks a human
/// completed externally, each once.
async fn completions(
    c: &mut SqliteConnection,
    p: &str,
    start: i64,
    end: i64,
) -> Result<Vec<Completion>, AppError> {
    let rows = sqlx::query(
        "SELECT t.origin,t.kind,t.created_at,(t.budget_exempt IS NOT NULL) AS canary,0 AS external,w.updated_at AS at,\
         EXISTS(SELECT 1 FROM integrator_results r JOIN integrator_observations o ON o.result_id=r.id \
           WHERE r.submission_id=w.current_submission_id AND o.observed_at<=?3 \
           AND o.disposition IN ('published','published_after_reopen')) AS push,\
         EXISTS(SELECT 1 FROM integrator_results r JOIN integrator_observations o ON o.result_id=r.id \
           WHERE r.submission_id=w.current_submission_id AND o.observed_at<=?3 \
           AND o.disposition='already_contained') AS contained,\
         EXISTS(SELECT 1 FROM integration_results i WHERE i.submission_id=w.current_submission_id \
           AND i.created_at<=?3 AND i.publication_state='published') AS agent \
         FROM workflow_subjects w JOIN tasks t ON t.id=w.task_id \
         WHERE w.project_id=?1 AND w.phase='done' AND w.updated_at BETWEEN ?2 AND ?3 \
         UNION ALL \
         SELECT t.origin,t.kind,t.created_at,(t.budget_exempt IS NOT NULL),1,e.created_at,0,0,0 \
         FROM events e JOIN tasks t ON t.id=e.record_id AND t.project_id=e.project_id \
         WHERE e.project_id=?1 AND e.kind='task.completed_externally' AND e.created_at BETWEEN ?2 AND ?3 \
         AND NOT EXISTS(SELECT 1 FROM workflow_subjects w WHERE w.task_id=t.id AND w.phase='done' \
           AND w.updated_at BETWEEN ?2 AND ?3) \
         ORDER BY 6",
    )
    .bind(p)
    .bind(start)
    .bind(end)
    .fetch_all(&mut *c)
    .await?;
    Ok(rows
        .iter()
        .map(|r| {
            let at: i64 = r.get("at");
            Completion {
                origin: r.get("origin"),
                kind: r.get("kind"),
                canary: r.get::<i64, _>("canary") == 1,
                external: r.get::<i64, _>("external") == 1,
                created_at: r.get("created_at"),
                at,
                day: timestamp(at)[..10].to_owned(),
                integrator_push: r.get::<i64, _>("push") == 1,
                already_contained: r.get::<i64, _>("contained") == 1,
                agent_published: r.get::<i64, _>("agent") == 1,
            }
        })
        .collect())
}

/// Throughput, latency and push coverage of the counted (non-canary)
/// completions, and the count of the canary ones left out.
fn value_and_push(done: &[Completion], width_ms: i64) -> (Value, Value, usize) {
    let counted: Vec<&Completion> = done.iter().filter(|d| !d.canary).collect();
    let excluded = done.len() - counted.len();
    let mut by_origin: BTreeMap<&str, i64> = [("human", 0), ("agent", 0), ("service", 0)]
        .into_iter()
        .collect();
    let mut by_day: BTreeMap<&str, i64> = BTreeMap::new();
    let (mut all, mut human) = (Vec::new(), Vec::new());
    let mut external = 0_i64;
    for d in &counted {
        *by_origin.entry(d.origin.as_str()).or_default() += 1;
        *by_day.entry(d.day.as_str()).or_default() += 1;
        external += i64::from(d.external);
        let ms = (d.at - d.created_at).max(0);
        all.push(ms);
        if d.origin == "human" {
            human.push(ms);
        }
    }
    let human_count = human.len();
    let days = width_ms.max(1) as f64 / DAY_MS as f64;
    let value = json!({
        "source": "workflow_subjects.updated_at where phase=done, plus task.completed_externally events for tasks not already counted",
        "completed": {
            "count": counted.len(),
            "by_origin": by_origin,
            "completed_externally": external,
            "by_day": by_day.iter().map(|(day, n)| json!({"day": day, "count": n})).collect::<Vec<_>>(),
        },
        "human_origin": {
            "count": human_count,
            "per_day": human_count as f64 / days,
            "window_days": days,
        },
        "latency": {
            "basis": "task created_at to completion; nearest-rank percentiles",
            "all": latency(&mut all),
            "human_origin": latency(&mut human),
        },
    });

    let code: Vec<&&Completion> = counted
        .iter()
        .filter(|d| d.kind == "code" && !d.external)
        .collect();
    let (mut pushed, mut agent, mut contained, mut none) = (0_i64, 0_i64, 0_i64, 0_i64);
    for d in &code {
        if d.integrator_push {
            pushed += 1;
        } else if d.agent_published {
            agent += 1;
        } else if d.already_contained {
            contained += 1;
        } else {
            none += 1;
        }
    }
    let denominator = code.len() as i64;
    let ratio = |n: i64| (denominator > 0).then(|| n as f64 / denominator as f64);
    let push = json!({
        "status": if none == 0 { "complete" } else { "partial" },
        "denominator": {
            "definition": "non-canary code tasks whose workflow reached done in the window, excluding tasks a human completed externally",
            "completed_code_tasks": denominator,
            "not_applicable_general_tasks": counted.iter().filter(|d| d.kind != "code" && !d.external).count(),
            "completed_externally": external,
        },
        "integrator_direct_push": {"count": pushed, "ratio": ratio(pushed),
            "evidence": "integrator_observations with disposition published or published_after_reopen"},
        "agent_integration_published": {"count": agent, "ratio": ratio(agent),
            "evidence": "integration_results with publication_state published, reported by an integration activity"},
        "already_contained_only": {"count": contained, "ratio": ratio(contained),
            "evidence": "integrator observation that the target already held the result; no push by the integrator"},
        "no_publication_evidence": {"count": none, "ratio": ratio(none),
            "evidence": "no integrator observation or published integration result is recorded for the completed submission"},
        "timestamps": {
            "push_time": "unavailable",
            "recorded_time": "the service records when the integrator or agent reported the observation, not when the push happened",
            "commit_times": "git author and commit timestamps are neither stored nor used as push times",
        },
    });
    (value, push, excluded)
}

/// Events in the window by credential class and kind.
struct EventRow {
    kind: String,
    class: String,
    canary: bool,
    events: i64,
    records: i64,
}

async fn events(
    c: &mut SqliteConnection,
    p: &str,
    start: i64,
    end: i64,
) -> Result<Vec<EventRow>, AppError> {
    let sql = concat!(
        "SELECT e.kind AS kind,\
         CASE WHEN e.credential_class IS NOT NULL THEN e.credential_class \
              WHEN pr.kind='human' THEN 'human_session' \
              WHEN e.kind IN ",
        reporter_kinds!(),
        " THEN 'job_reporter' \
              ELSE 'unclassified' END AS class,\
         (e.record_id IN ",
        canary_records!(),
        ") AS canary,count(*) AS n,count(DISTINCT e.record_id) AS records \
         FROM events e JOIN principals pr ON pr.id=e.actor_id \
         WHERE e.project_id=?1 AND e.created_at BETWEEN ?2 AND ?3 \
         GROUP BY e.kind,class,canary ORDER BY e.kind,class,canary"
    );
    let rows = sqlx::query(sql)
        .bind(p)
        .bind(start)
        .bind(end)
        .fetch_all(&mut *c)
        .await?;
    Ok(rows
        .iter()
        .map(|r| EventRow {
            kind: r.get("kind"),
            class: r.get("class"),
            canary: r.get::<i64, _>("canary") == 1,
            events: r.get("n"),
            records: r.get("records"),
        })
        .collect())
}

/// One stall episode: a run of attempts that all ended without a submission.
struct Stall {
    task_id: String,
    title: String,
    canary: bool,
    started_at: i64,
    failed_attempts: i64,
    last_failure_at: i64,
    next_attempt_at: Option<i64>,
    resolved_at: Option<i64>,
}

/// Stall episodes reconstructed from attempt history as of `end`, and the
/// number of attempts whose end time is missing (a gap in the evidence).
///
/// An episode starts when the third consecutive attempt ends without a
/// submission, grows with each further such attempt, and ends when an attempt
/// submits. Attempts that ended after `end` count as still running.
async fn stalls(
    c: &mut SqliteConnection,
    p: &str,
    end: i64,
) -> Result<(Vec<Stall>, i64), AppError> {
    let rows = sqlx::query(
        "SELECT a.task_id,t.title,(t.budget_exempt IS NOT NULL) AS canary,a.state,a.created_at,a.ended_at \
         FROM attempts a JOIN tasks t ON t.id=a.task_id \
         WHERE a.project_id=?1 AND a.created_at<=?2 ORDER BY a.task_id,a.generation",
    )
    .bind(p)
    .bind(end)
    .fetch_all(&mut *c)
    .await?;
    let (mut found, mut gaps) = (Vec::<Stall>::new(), 0_i64);
    let mut task = String::new();
    let (mut run, mut open): (usize, Option<usize>) = (0, None);
    for r in &rows {
        let id: String = r.get("task_id");
        if id != task {
            (task, run, open) = (id.clone(), 0, None);
        }
        let state: String = r.get("state");
        let (created, ended): (i64, Option<i64>) = (r.get("created_at"), r.get("ended_at"));
        if let Some(i) = open
            && found[i].next_attempt_at.is_none()
        {
            found[i].next_attempt_at = Some(created);
        }
        let failed = FAILED_STATES.contains(&state.as_str());
        match ended {
            Some(at) if at <= end && failed => {
                run += 1;
                if run == STALL_ATTEMPTS {
                    open = Some(found.len());
                    found.push(Stall {
                        task_id: id,
                        title: r.get("title"),
                        canary: r.get::<i64, _>("canary") == 1,
                        started_at: at,
                        failed_attempts: run as i64,
                        last_failure_at: at,
                        next_attempt_at: None,
                        resolved_at: None,
                    });
                } else if let Some(i) = open {
                    found[i].failed_attempts = run as i64;
                    found[i].last_failure_at = at;
                    found[i].next_attempt_at = None;
                }
            }
            Some(at) if at <= end && state == "submitted" => {
                if let Some(i) = open.take() {
                    found[i].resolved_at = Some(at);
                }
                run = 0;
            }
            None if failed => {
                gaps += 1;
                run = 0;
            }
            _ => {}
        }
    }
    Ok((found, gaps))
}

fn stall_value(s: &Stall) -> Value {
    json!({
        "task_id": s.task_id, "title": s.title,
        "started_at": timestamp(s.started_at),
        "failed_attempts": s.failed_attempts,
        "last_failure_at": timestamp(s.last_failure_at),
        "next_attempt_at": s.next_attempt_at.map(timestamp),
        "resolved_at": s.resolved_at.map(timestamp),
    })
}

/// Parks the integrator's revises recorded in the window.
async fn parks(
    c: &mut SqliteConnection,
    p: &str,
    start: i64,
    end: i64,
) -> Result<Vec<(Value, bool)>, AppError> {
    let rows = sqlx::query(
        "SELECT v.submission_id,v.task_id,t.title,v.reason_code,v.revised_at,(t.budget_exempt IS NOT NULL) AS canary \
         FROM integrator_revises v JOIN tasks t ON t.id=v.task_id \
         WHERE t.project_id=?1 AND v.park_reason IS NOT NULL AND v.revised_at BETWEEN ?2 AND ?3 \
         ORDER BY v.revised_at,v.submission_id",
    )
    .bind(p)
    .bind(start)
    .bind(end)
    .fetch_all(&mut *c)
    .await?;
    Ok(rows
        .iter()
        .map(|r| {
            (
                json!({"submission_id": r.get::<String, _>("submission_id"),
                       "task_id": r.get::<String, _>("task_id"),
                       "title": r.get::<String, _>("title"),
                       "reason_code": r.get::<String, _>("reason_code"),
                       "parked_at": timestamp(r.get("revised_at"))}),
                r.get::<i64, _>("canary") == 1,
            )
        })
        .collect())
}

/// Human-required integrator reports opened in the window: (counted, canary).
async fn human_reports(
    c: &mut SqliteConnection,
    p: &str,
    start: i64,
    end: i64,
) -> Result<(i64, i64), AppError> {
    let row = sqlx::query(
        "SELECT COALESCE(sum(r.task_id IS NULL OR t.budget_exempt IS NULL),0) AS counted,\
         COALESCE(sum(t.budget_exempt IS NOT NULL),0) AS canary \
         FROM integrator_reports r LEFT JOIN tasks t ON t.id=r.task_id \
         WHERE r.project_id=?1 AND r.requires_human=1 AND r.created_at BETWEEN ?2 AND ?3",
    )
    .bind(p)
    .bind(start)
    .bind(end)
    .fetch_one(&mut *c)
    .await?;
    Ok((row.get("counted"), row.get("canary")))
}

/// `GET …/telemetry/autonomy/window?start_ms=&end_ms=`: autonomy metrics for
/// the closed interval `[start_ms, end_ms]`, both bounds included, at most 92
/// days wide and already past (`end_ms` after the service's clock is refused
/// with `window_not_closed`). The windowed sections depend only on the window
/// and the records stamped no later than `end_ms`, so the same query returns
/// the same figures whenever it is asked; `current_state` alone is read now
/// and says so. See the API contract for the sections.
pub(crate) async fn read_window(
    State(s): State<AppState>,
    _auth: Auth,
    Path(p): Path<String>,
    Query(query): Query<WindowQuery>,
) -> Reply {
    let now = s.now();
    let (start, end) = bounds(&query, now)?;
    let mut c = s.pool.acquire().await?;
    require_project(&mut c, &p).await?;

    let done = completions(&mut c, &p, start, end).await?;
    let (value, direct_push, canary_completions) = value_and_push(&done, end - start);

    let rows = events(&mut c, &p, start, end).await?;
    let mut by_class: BTreeMap<&str, i64> = [
        "interactive",
        "human_session",
        "supervised",
        "integrator",
        "job_reporter",
        "unclassified",
    ]
    .into_iter()
    .map(|class| (class, 0))
    .collect();
    let (mut canary_events, mut by_kind) = (0_i64, Vec::new());
    for row in &rows {
        if row.canary {
            canary_events += row.events;
            continue;
        }
        *by_class.entry(row.class.as_str()).or_default() += row.events;
        by_kind.push(json!({"kind": row.kind, "class": row.class,
                            "events": row.events, "records": row.records}));
    }
    let class_count = |class: &str| by_class.get(class).copied().unwrap_or(0);
    let interventions = class_count("interactive") + class_count("human_session");
    let unclassified = class_count("unclassified");
    let events_total: i64 = by_class.values().sum();

    let (reports, canary_reports) = human_reports(&mut c, &p, start, end).await?;
    let parked = parks(&mut c, &p, start, end).await?;
    let (canary_parks, counted_parks): (Vec<_>, Vec<_>) =
        parked.into_iter().partition(|(_, canary)| *canary);
    let (all_stalls, attempt_gaps) = stalls(&mut c, &p, end).await?;
    let started = |s: &&Stall| (start..=end).contains(&s.started_at);
    let counted_stalls: Vec<&Stall> = all_stalls.iter().filter(|s| !s.canary).collect();
    let started_stalls: Vec<&Stall> = counted_stalls.iter().copied().filter(started).collect();
    let carried_in = counted_stalls
        .iter()
        .filter(|s| s.started_at < start && s.resolved_at.is_none_or(|at| at >= start))
        .count();
    let canary_stalls = all_stalls.iter().filter(|s| s.canary).count();

    let covered: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM (SELECT 1 FROM events WHERE project_id=?1 AND created_at<=?2 LIMIT 1)",
    )
    .bind(&p)
    .bind(start)
    .fetch_one(&mut *c)
    .await?;
    let earliest: Option<i64> =
        sqlx::query_scalar("SELECT min(created_at) FROM events WHERE project_id=?1")
            .bind(&p)
            .fetch_one(&mut *c)
            .await?;

    let mut unavailable = Vec::new();
    if covered == 0 {
        unavailable.push(json!({"source": "audit_events",
            "reason": "the window begins before the project's earliest retained audit event, so interventions before that event are unknown"}));
    }
    if unclassified > 0 {
        unavailable.push(json!({"source": "credential_class",
            "reason": "some audit events were written by an agent credential with no recorded class, so they cannot be split into interactive and supervised",
            "events": unclassified}));
    }
    if attempt_gaps > 0 {
        unavailable.push(json!({"source": "attempt_history",
            "reason": "some attempts ended without a recorded end time, so stall episodes may be missing",
            "attempts": attempt_gaps}));
    }
    let components = json!({
        "interactive_and_human_events": interventions,
        "human_required_integrator_reports": reports,
        "parked_subjects": counted_parks.len(),
        "stall_episodes_started": started_stalls.len(),
    });
    let lower_bound =
        interventions + reports + counted_parks.len() as i64 + started_stalls.len() as i64;
    let complete = unavailable.is_empty();
    let hri = json!({
        "status": if complete { "complete" } else { "partial" },
        "count": complete.then_some(lower_bound),
        "lower_bound": lower_bound,
        "outcome": match (complete, lower_bound) {
            (false, _) => "unknown",
            (true, 0) => "none_recorded",
            (true, _) => "recorded",
        },
        "components": components,
        "unavailable_sources": unavailable,
        "note": "count adds raw events and episodes from different sources and is a measure of recorded attention, not of distinct people-hours; it is null whenever a source is unavailable, and zero is never reported as success then",
    });

    let stalled_now = crate::attention::stalled_tasks(&mut c, &p).await?;
    let parked_now = parked_now(&mut c, &p, now).await?;
    let cap = |items: Vec<Value>| -> (usize, bool, Vec<Value>) {
        let (n, truncated) = (items.len(), items.len() > MAX_ITEMS);
        (n, truncated, items.into_iter().take(MAX_ITEMS).collect())
    };
    let (stall_n, stall_cut, stall_items) =
        cap(started_stalls.iter().map(|s| stall_value(s)).collect());
    let (park_n, park_cut, park_items) = cap(counted_parks.into_iter().map(|(v, _)| v).collect());
    let canary_park_n = canary_parks.len();

    Ok(response(json!({
        "project_id": p,
        "schema": "autonomy-window-v1",
        "window": {
            "start_ms": start, "end_ms": end,
            "start": timestamp(start), "end": timestamp(end),
            "bounds": "both inclusive",
            "width_ms": end - start,
        },
        "value": value,
        "direct_push": direct_push,
        "attention_events": {
            "source": "audit events of the project, classed by the credential behind each; canary-task records are in reserved_exclusions",
            "total": events_total,
            "by_class": by_class,
            "interventions": {
                "definition": "events written by an interactive agent credential or a human session; raw events, not distinct interventions",
                "events": interventions,
            },
            "by_kind": by_kind,
            "classification": {
                "status": if unclassified == 0 { "complete" } else { "partial" },
                "unclassified_events": unclassified,
            },
            "audit_log": {
                "earliest_event_at": earliest.map(timestamp),
                "covers_window_start": covered > 0,
            },
        },
        "integrator_signals": {
            "human_required_reports": reports,
            "source": "integrator_reports with requires_human=1 created in the window",
        },
        "episodes": {
            "stalls": {
                "definition": "three or more consecutive attempts of a task that ended without a submission; one episode however many times a digest reports it. An episode starts when the third attempt ends and ends when an attempt submits.",
                "started_in_window": stall_n,
                "items": stall_items,
                "truncated": stall_cut,
                "carried_in": carried_in,
                "carried_in_note": "episodes that started before the window and have no submitting attempt before it; a task canceled meanwhile is still listed",
                "attempts_without_end_time": attempt_gaps,
            },
            "parks": {
                "definition": "subjects an integrator revise parked, by revise time",
                "count": park_n,
                "items": park_items,
                "truncated": park_cut,
            },
        },
        "reserved_exclusions": {
            "label": "canary tasks (budget_exempt) are probes of the pipeline and are left out of every figure above",
            "canary_tasks": {
                "completions": canary_completions,
                "attention_events": canary_events,
                "human_required_reports": canary_reports,
                "parked_subjects": canary_park_n,
                "stall_episodes": canary_stalls,
                "event_attribution": "events whose record is a canary task, or a submission or attempt of one",
            },
        },
        "hri": hri,
        "current_state": {
            "as_of": timestamp(now),
            "depends_on_query_time": true,
            "stalled_tasks": {"count": stalled_now.len(), "items": stalled_now},
            "parked": {"count": parked_now.len(), "items": parked_now},
        },
    })))
}
