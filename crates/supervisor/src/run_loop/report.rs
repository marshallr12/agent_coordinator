//! Reports the cost ledger to the coordinator (autonomy health per project).
//! `<state_dir>/costs.jsonl` stays the durable source; this only copies its
//! rows to `POST …/telemetry/cost-rows` of the row's project. A row's identity
//! is the SHA-256 of its ledger line, so the coordinator stores a row once per
//! (project, host) however often or in whatever batch it arrives: a lost
//! response, a restart and the first run on a host with an old ledger (the
//! backfill) all resend rows without duplicating them.
//!
//! `<state_dir>/costs.reported` lists the identities the coordinator accepted
//! (or refused for good), one per line, so only unsent rows are sent. Losing
//! that file only costs a resend. A batch's Idempotency-Key derives from its
//! row identities, so a retry of the same rows reuses it.
use super::cost;
use crate::config::Config;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::path::PathBuf;

/// Rows per request (the coordinator takes at most 200).
const BATCH: usize = 100;

/// What the coordinator answered to one batch.
#[derive(Debug, Clone, PartialEq)]
pub enum Delivery {
    /// Stored, or already stored: `(accepted, duplicates)`.
    Stored(u64, u64),
    /// Not delivered; the rows stay unsent. `stop` ends the whole pass (the
    /// coordinator or the credential is unusable, not just this project).
    Retry { stop: bool, reason: String },
    /// The coordinator refused the rows as invalid (HTTP 400 or 422).
    Rejected(String),
}

/// What one pass did.
#[derive(Debug, Default, PartialEq)]
pub struct Summary {
    /// Rows the coordinator stored in this pass.
    pub accepted: u64,
    /// Rows it already held.
    pub duplicates: u64,
    /// Rows it refused for good.
    pub rejected: u64,
    /// Rows still unsent: retried by a later pass.
    pub pending: u64,
}

/// `<state_dir>/costs.reported`.
pub fn reported_path(config: &Config) -> PathBuf {
    config.state_dir.join("costs.reported")
}

/// The identity of a ledger line.
fn identity(line: &str) -> String {
    hex::encode(Sha256::digest(line.trim().as_bytes()))
}

/// One unsent ledger row.
struct Unsent {
    id: String,
    project: String,
    row: Value,
}

/// The coordinator's form of a ledger row, with its project; `None` for a
/// line that is not a launch cost (it stays in the ledger, never sent).
fn convert(id: &str, line: &str, default_project: Option<&str>) -> Option<Unsent> {
    let entry: Value = serde_json::from_str(line).ok()?;
    let text = |key: &str| entry[key].as_str().map(str::to_owned);
    let role = text("role").filter(|r| r == "impl" || r == "rev")?;
    let project = text("project").or_else(|| default_project.map(str::to_owned))?;
    let count = |key: &str| entry[key].as_u64().unwrap_or(0);
    let usd = entry["usd"].as_f64().filter(|usd| usd.is_finite());
    let row = json!({
        "row_id": id, "role": role,
        "harness": text("harness").unwrap_or_else(|| "unknown".into()),
        "model": text("model").unwrap_or_else(|| "unknown".into()),
        "input_tokens": count("input_tokens"),
        "cached_input_tokens": count("cached_input_tokens"),
        "output_tokens": count("output_tokens"),
        "usd": usd,
        "usd_estimated": entry["usd_estimated"] == true,
        "canary": entry["canary"] == true,
        "task_id": text("task"), "session_id": text("session"),
        "attempt_id": text("attempt"),
        "occurred_at_ms": entry["at_ms"].as_i64().unwrap_or(0),
    });
    Some(Unsent {
        id: id.to_owned(),
        project,
        row,
    })
}

/// The identities already delivered or refused for good.
fn done(config: &Config) -> HashSet<String> {
    let text = std::fs::read_to_string(reported_path(config)).unwrap_or_default();
    text.lines().map(|l| l.trim().to_owned()).collect()
}

/// Records `ids` as delivered (mode 0600, created on first use).
fn mark_done(config: &Config, ids: &[&str]) {
    use std::io::Write;
    let mut options = std::fs::OpenOptions::new();
    options.append(true).create(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let text: String = ids.iter().map(|id| format!("{id}\n")).collect();
    let written = options
        .open(reported_path(config))
        .and_then(|mut file| file.write_all(text.as_bytes()));
    if let Err(error) = written {
        eprintln!("agentc-supervisor run: record reported costs: {error}");
    }
}

/// The ledger rows not yet delivered, in ledger order.
fn unsent(config: &Config, default_project: Option<&str>) -> Vec<Unsent> {
    let text = std::fs::read_to_string(cost::ledger(config)).unwrap_or_default();
    let done = done(config);
    let rows = text.lines().filter(|l| !l.trim().is_empty());
    let rows = rows.map(|l| (identity(l), l));
    let fresh = rows.filter(|(id, _)| !done.contains(id));
    fresh
        .filter_map(|(id, line)| convert(&id, line, default_project))
        .collect()
}

/// The Idempotency-Key of a batch of rows `ids`: the same rows reuse it.
fn batch_key(project: &str, ids: &[&str]) -> String {
    let digest = Sha256::digest(format!("{project}\n{}", ids.join(",")));
    format!("cost-{}", &hex::encode(digest)[..40])
}

/// Delivers every unsent ledger row with `send(project, host, rows, key)`,
/// one request per project and batch of [`BATCH`] rows. A batch the
/// coordinator refuses as invalid is resent row by row so one bad row never
/// holds the others back; a single row refused is skipped for good (it stays
/// in the ledger). `default_project` names the project of rows that carry
/// none. Delivered rows are recorded in [`reported_path`] as they go.
pub fn flush(
    config: &Config,
    host: &str,
    default_project: Option<&str>,
    send: &mut dyn FnMut(&str, &Value, &str) -> Delivery,
) -> Summary {
    let rows = unsent(config, default_project);
    let mut summary = Summary::default();
    let mut projects: Vec<&str> = rows.iter().map(|r| r.project.as_str()).collect();
    let mut seen = HashSet::new();
    projects.retain(|p| seen.insert(*p));
    let mut stopped = false;
    for project in projects {
        let mine: Vec<&Unsent> = rows.iter().filter(|r| r.project == project).collect();
        let mut failed = false;
        for batch in mine.chunks(BATCH) {
            if stopped || failed {
                summary.pending += batch.len() as u64;
                continue;
            }
            match deliver(config, host, project, batch, send, &mut summary) {
                Flow::Go => {}
                Flow::SkipProject => failed = true,
                Flow::Stop => stopped = true,
            }
        }
    }
    summary
}

/// Whether a pass goes on after a batch.
enum Flow {
    Go,
    SkipProject,
    Stop,
}

/// Sends one batch, bisecting into single rows when it is refused as invalid.
fn deliver(
    config: &Config,
    host: &str,
    project: &str,
    batch: &[&Unsent],
    send: &mut dyn FnMut(&str, &Value, &str) -> Delivery,
    summary: &mut Summary,
) -> Flow {
    let ids: Vec<&str> = batch.iter().map(|r| r.id.as_str()).collect();
    let body = json!({"host": host, "rows": batch.iter().map(|r| &r.row).collect::<Vec<_>>()});
    match send(project, &body, &batch_key(project, &ids)) {
        Delivery::Stored(accepted, duplicates) => {
            mark_done(config, &ids);
            summary.accepted += accepted;
            summary.duplicates += duplicates;
            Flow::Go
        }
        Delivery::Rejected(reason) if batch.len() > 1 => {
            eprintln!("agentc-supervisor run: report costs: {reason}; resending row by row");
            let mut flow = Flow::Go;
            for (n, row) in batch.iter().enumerate() {
                match deliver(config, host, project, &[row], send, summary) {
                    Flow::Go => {}
                    other => {
                        summary.pending += (batch.len() - n - 1) as u64;
                        flow = other;
                        break;
                    }
                }
            }
            flow
        }
        Delivery::Rejected(reason) => {
            eprintln!(
                "agentc-supervisor run: report costs: row {} refused: {reason}",
                ids[0]
            );
            mark_done(config, &ids);
            summary.rejected += 1;
            Flow::Go
        }
        Delivery::Retry { stop, reason } => {
            eprintln!("agentc-supervisor run: report costs: {reason}");
            summary.pending += batch.len() as u64;
            if stop { Flow::Stop } else { Flow::SkipProject }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    /// A config whose state directory is `dir`.
    fn config(dir: &std::path::Path) -> Config {
        Config {
            state_dir: dir.to_path_buf(),
            ..Config::default()
        }
    }

    /// Appends ledger lines for `n` launches of `project`.
    fn ledger(config: &Config, project: &str, from: i64, n: i64) {
        let lines: Vec<String> = (from..from + n)
            .map(|i| {
                json!({"at_ms": i, "role": "impl", "project": project, "task": format!("t{i}"),
                    "session": "s", "attempt": "a", "harness": "claude", "model": "m",
                    "input_tokens": 1, "cached_input_tokens": 2, "output_tokens": 3,
                    "usd": 0.5})
                .to_string()
            })
            .collect();
        let mut text = std::fs::read_to_string(cost::ledger(config)).unwrap_or_default();
        text.push_str(&lines.join("\n"));
        text.push('\n');
        std::fs::write(cost::ledger(config), text).unwrap();
    }

    /// Everything a fake coordinator was sent: `(project, key, row ids)`.
    type Calls = RefCell<Vec<(String, String, Vec<String>)>>;

    fn row_ids(body: &Value) -> Vec<String> {
        let rows = body["rows"].as_array().unwrap();
        rows.iter()
            .map(|r| r["row_id"].as_str().unwrap().to_owned())
            .collect()
    }

    #[test]
    fn rows_are_sent_once_in_batches_and_recorded() {
        let dir = tempfile::tempdir().unwrap();
        let config = config(dir.path());
        ledger(&config, "p1", 0, 150);
        ledger(&config, "p2", 1000, 2);
        let calls = Calls::default();
        let mut send = |p: &str, body: &Value, key: &str| {
            calls
                .borrow_mut()
                .push((p.into(), key.into(), row_ids(body)));
            assert_eq!(body["host"], "h");
            Delivery::Stored(body["rows"].as_array().unwrap().len() as u64, 0)
        };
        let summary = flush(&config, "h", None, &mut send);
        assert_eq!(
            summary,
            Summary {
                accepted: 152,
                ..Summary::default()
            }
        );
        let sizes: Vec<_> = calls
            .borrow()
            .iter()
            .map(|c| (c.0.clone(), c.2.len()))
            .collect();
        assert_eq!(
            sizes,
            [("p1".into(), 100), ("p1".into(), 50), ("p2".into(), 2)]
        );
        // A second pass finds nothing unsent.
        calls.borrow_mut().clear();
        let mut send = |_: &str, _: &Value, _: &str| -> Delivery { panic!("nothing is unsent") };
        assert_eq!(flush(&config, "h", None, &mut send), Summary::default());
        assert!(calls.borrow().is_empty());
        // A new row alone is sent.
        ledger(&config, "p1", 5000, 1);
        let mut send = |p: &str, body: &Value, key: &str| {
            calls
                .borrow_mut()
                .push((p.into(), key.into(), row_ids(body)));
            Delivery::Stored(1, 0)
        };
        flush(&config, "h", None, &mut send);
        assert_eq!(calls.borrow().len(), 1);
    }

    #[test]
    fn a_failed_pass_leaves_the_rows_for_a_retry_with_the_same_key() {
        let dir = tempfile::tempdir().unwrap();
        let config = config(dir.path());
        ledger(&config, "p1", 0, 3);
        let keys = RefCell::new(Vec::new());
        let mut down = |_: &str, _: &Value, key: &str| {
            keys.borrow_mut().push(key.to_owned());
            Delivery::Retry {
                stop: true,
                reason: "down".into(),
            }
        };
        let summary = flush(&config, "h", None, &mut down);
        assert_eq!(
            summary,
            Summary {
                pending: 3,
                ..Summary::default()
            }
        );
        assert!(!reported_path(&config).exists());
        // The same rows retry under the same key.
        let mut down_again = |_: &str, _: &Value, key: &str| {
            keys.borrow_mut().push(key.to_owned());
            Delivery::Retry {
                stop: true,
                reason: "down".into(),
            }
        };
        flush(&config, "h", None, &mut down_again);
        flush(&config, "h", None, &mut down_again);
        let keys = keys.borrow();
        assert_eq!(keys[0], keys[1]);
        assert_eq!(keys[1], keys[2]);
        assert!(keys[0].starts_with("cost-") && keys[0].len() <= 128);
        // Once the coordinator is back every row goes and is recorded.
        let mut up = |_: &str, body: &Value, _: &str| {
            Delivery::Stored(body["rows"].as_array().unwrap().len() as u64, 0)
        };
        assert_eq!(flush(&config, "h", None, &mut up).accepted, 3);
        assert_eq!(done(&config).len(), 3);
    }

    #[test]
    fn a_project_that_fails_does_not_hold_back_the_others() {
        let dir = tempfile::tempdir().unwrap();
        let config = config(dir.path());
        ledger(&config, "gone", 0, 2);
        ledger(&config, "p2", 100, 2);
        let mut send = |p: &str, body: &Value, _: &str| {
            if p == "gone" {
                Delivery::Retry {
                    stop: false,
                    reason: "404".into(),
                }
            } else {
                Delivery::Stored(body["rows"].as_array().unwrap().len() as u64, 0)
            }
        };
        let summary = flush(&config, "h", None, &mut send);
        assert_eq!((summary.accepted, summary.pending), (2, 2));
    }

    #[test]
    fn a_refused_batch_is_resent_row_by_row_and_only_the_bad_row_is_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let config = config(dir.path());
        ledger(&config, "p1", 0, 3);
        let bad = identity(
            std::fs::read_to_string(cost::ledger(&config))
                .unwrap()
                .lines()
                .nth(1)
                .unwrap(),
        );
        let mut send = |_: &str, body: &Value, _: &str| {
            if row_ids(body).contains(&bad) {
                Delivery::Rejected("invalid".into())
            } else {
                Delivery::Stored(body["rows"].as_array().unwrap().len() as u64, 0)
            }
        };
        let summary = flush(&config, "h", None, &mut send);
        assert_eq!(
            summary,
            Summary {
                accepted: 2,
                rejected: 1,
                ..Summary::default()
            }
        );
        // The refused row is not resent; the ledger still holds it.
        let mut never = |_: &str, _: &Value, _: &str| -> Delivery { panic!("nothing is unsent") };
        assert_eq!(flush(&config, "h", None, &mut never), Summary::default());
        assert_eq!(
            std::fs::read_to_string(cost::ledger(&config))
                .unwrap()
                .lines()
                .count(),
            3
        );
    }

    #[test]
    fn ledger_rows_convert_to_the_coordinators_form() {
        let line = json!({"at_ms": 42, "role": "rev", "project": "p", "task": "t",
            "session": "s", "attempt": "a", "harness": "codex", "model": "m",
            "input_tokens": 1, "cached_input_tokens": 2, "output_tokens": 3,
            "usd": null, "usd_estimated": true, "canary": true})
        .to_string();
        let row = convert("id1", &line, None).unwrap();
        assert_eq!(row.project, "p");
        assert_eq!(
            row.row,
            json!({"row_id": "id1", "role": "rev", "harness": "codex", "model": "m",
                "input_tokens": 1, "cached_input_tokens": 2, "output_tokens": 3,
                "usd": null, "usd_estimated": true, "canary": true, "task_id": "t",
                "session_id": "s", "attempt_id": "a", "occurred_at_ms": 42})
        );
        // Lines that are not launch costs, or name no project, are not sent.
        assert!(convert("id", "not json", Some("p")).is_none());
        assert!(convert("id", r#"{"role":"other","project":"p"}"#, None).is_none());
        assert!(convert("id", r#"{"role":"impl"}"#, None).is_none());
        let defaulted = convert("id", r#"{"role":"impl","at_ms":1}"#, Some("main")).unwrap();
        assert_eq!(defaulted.project, "main");
    }
}
