//! Shadow mode (autonomy plan P3a): poll the coordinator's read-only `next`
//! endpoint with the host's read-access credential and append a JSONL
//! "would launch" record, with a cost estimate, whenever the next action for
//! a (project, role) changes. Nothing is claimed or launched.
use crate::estimate::{self, LaunchEstimate, LaunchOverride, Price};
use anyhow::{Context, Result, bail};
use coordinator_client::CoordinatorClient;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Roles polled on every cycle.
const ROLES: [&str; 2] = ["implementer", "reviewer"];
/// Log slot for failures to list projects.
const LISTING_SLOT: &str = "*/*";

/// Shadow-mode settings; every entry has a default.
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct ShadowConfig {
    /// CLI-format credentials file (`[[credentials]] origin, token`) holding
    /// the read-only host credential; readable only by the supervisor.
    pub credential_file: PathBuf,
    /// Origin to pick from that file; empty means its first entry.
    pub origin: String,
    /// Permit plain HTTP to a loopback origin (the staging coordinator).
    pub allow_insecure_loopback: bool,
    /// Projects to watch; empty means every project the credential lists.
    pub projects: Vec<String>,
    pub poll_seconds: u64,
    /// Append-only would-launch log.
    pub log: PathBuf,
    /// Changes to the default implementer launch profile.
    pub implementer: LaunchOverride,
    /// Changes to the default reviewer launch profile.
    pub reviewer: LaunchOverride,
    /// USD per million tokens by model id, merged over the default table.
    pub prices: BTreeMap<String, Price>,
}

impl Default for ShadowConfig {
    fn default() -> Self {
        Self {
            credential_file: PathBuf::from("/etc/agentc/shadow-credentials.toml"),
            origin: String::new(),
            allow_insecure_loopback: false,
            projects: Vec::new(),
            poll_seconds: 60,
            log: PathBuf::from("/var/lib/agentc/shadow/would-launch.jsonl"),
            implementer: LaunchOverride::default(),
            reviewer: LaunchOverride::default(),
            prices: BTreeMap::new(),
        }
    }
}

impl ShadowConfig {
    /// The effective launch profile for `role`.
    fn launch(&self, role: &str) -> LaunchEstimate {
        match role {
            "reviewer" => self.reviewer.apply(LaunchEstimate::reviewer()),
            _ => self.implementer.apply(LaunchEstimate::implementer()),
        }
    }

    /// Default prices with the configured ones added or replaced.
    fn price_table(&self) -> BTreeMap<String, Price> {
        let mut table = estimate::default_prices();
        table.extend(self.prices.iter().map(|(k, v)| (k.clone(), *v)));
        table
    }
}

#[derive(Deserialize)]
struct CredentialFile {
    #[serde(default)]
    credentials: Vec<CredentialEntry>,
}

#[derive(Deserialize)]
struct CredentialEntry {
    origin: String,
    token: String,
}

/// Polls forever (or once) and logs changes of the next action.
pub async fn run(config: &ShadowConfig, once: bool) -> Result<()> {
    let client = connect(config)?;
    let mut last = HashMap::new();
    loop {
        poll_all(&client, config, &mut last).await;
        if once {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_secs(config.poll_seconds.max(5))).await;
    }
}

/// Builds a client from the configured credential file entry.
fn connect(config: &ShadowConfig) -> Result<CoordinatorClient> {
    let path = &config.credential_file;
    client(path, &config.origin, config.allow_insecure_loopback)
}

/// A client for the entry of the CLI-format credentials file at `path`
/// whose origin is `origin` (normalized), or its first entry when `origin`
/// is empty.
pub(crate) fn client(path: &Path, origin: &str, insecure: bool) -> Result<CoordinatorClient> {
    let text = std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    client_from(&text, path, origin, insecure)
}

/// As [`client`], from the credentials file's `text`, which was read from
/// `shown`. A parse error is reported without its message, which may quote
/// the file.
pub(crate) fn client_from(
    text: &str,
    shown: &Path,
    origin: &str,
    insecure: bool,
) -> Result<CoordinatorClient> {
    let Ok(file) = toml::from_str::<CredentialFile>(text) else {
        bail!("{} is not a valid credentials file", shown.display());
    };
    let normal = |o: &str| coordinator_client::normalize_origin(o, insecure).ok();
    let entry = file
        .credentials
        .into_iter()
        .find(|c| origin.is_empty() || normal(&c.origin) == normal(origin))
        .with_context(|| format!("no matching credential in {}", shown.display()))?;
    CoordinatorClient::new(&entry.origin, entry.token, insecure)
        .map_err(|error| anyhow::anyhow!("coordinator client: {error}"))
}

/// One cycle over every project × role; failures are logged, never fatal.
async fn poll_all(
    client: &CoordinatorClient,
    config: &ShadowConfig,
    last: &mut HashMap<String, String>,
) {
    let projects = match project_ids(client, config).await {
        Ok(projects) => projects,
        Err(error) => return log_change(config, last, LISTING_SLOT, error_record(&error)),
    };
    last.remove(LISTING_SLOT);
    for project in &projects {
        for role in ROLES {
            let record = match fetch_next(client, project, role).await {
                Ok(next) => next_record(config, &next),
                Err(error) => error_record(&error),
            };
            log_change(config, last, &format!("{project}/{role}"), record);
        }
    }
}

/// Configured projects, or every project the credential can list.
async fn project_ids(client: &CoordinatorClient, config: &ShadowConfig) -> Result<Vec<String>> {
    if !config.projects.is_empty() {
        return Ok(config.projects.clone());
    }
    let query = [("limit", "200".to_owned())];
    let data = get_data(client, "/api/v1/projects", &query).await?;
    Ok(data["items"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|p| p["id"].as_str().map(str::to_owned))
        .collect())
}

/// `GET next` for one project and role.
pub(crate) async fn fetch_next(
    client: &CoordinatorClient,
    project: &str,
    role: &str,
) -> Result<Value> {
    let path = format!("/api/v1/projects/{project}/next");
    get_data(client, &path, &[("role", role.to_owned())]).await
}

/// GETs `path` and returns the envelope's `data`, or the service error code.
async fn get_data(
    client: &CoordinatorClient,
    path: &str,
    query: &[(&str, String)],
) -> Result<Value> {
    let response = client
        .get_query(path, query, None)
        .await
        .map_err(|error| anyhow::anyhow!("GET {path}: {error}"))?;
    if response.status != 200 {
        let code = response.body["error"]["code"].as_str().unwrap_or("unknown");
        bail!("GET {path}: HTTP {} {code}", response.status);
    }
    Ok(response.body["data"].clone())
}

/// A would-launch record (with estimate) or an idle record, keyed so
/// repeats of the same state are not logged again.
fn next_record(config: &ShadowConfig, next: &Value) -> (String, Value) {
    let queue = json!({"human_queue": next["human_queue"], "skipped": next["skipped"]});
    let Some(action) = next.get("action").filter(|a| !a.is_null()) else {
        let key = format!("idle:{}", next["human_queue"]);
        return (key, json!({"event": "idle", "queue": queue}));
    };
    let target = action_target(action);
    let mut record = launch_record(config, next["role"].as_str().unwrap_or(""), action);
    record["caller_steps"] = next["caller_steps"].clone();
    record["queue"] = queue;
    (format!("launch:{target}"), record)
}

/// `kind:id` of the task or activity an action would work on.
fn action_target(action: &Value) -> String {
    let id = action["activity_id"]
        .as_str()
        .or(action["task_id"].as_str());
    format!(
        "{}:{}",
        action["kind"].as_str().unwrap_or(""),
        id.unwrap_or("")
    )
}

/// The launch that `action` would start, with its cost estimate.
fn launch_record(config: &ShadowConfig, role: &str, action: &Value) -> Value {
    let launch = config.launch(role);
    json!({
        "event": "would_launch",
        "target": action_target(action),
        "action": {"kind": action["kind"], "task_id": action["task_id"],
                   "activity_id": action["activity_id"], "subject_task_id": action["subject_task_id"],
                   "title": action["title"], "priority": action["priority"]},
        "launch": {"harness": launch.harness, "model": launch.model, "effort": launch.effort},
        "estimate": estimate::estimate(&launch, &config.price_table()),
    })
}

/// An error record keyed by its message.
fn error_record(error: &anyhow::Error) -> (String, Value) {
    let message = format!("{error:#}");
    (
        format!("error:{message}"),
        json!({"event": "error", "error": message}),
    )
}

/// Appends `record` when its key differs from the slot's last one. A log
/// write failure is reported on stderr and retried on the next change, so a
/// transient disk problem never stops the poller.
fn log_change(
    config: &ShadowConfig,
    last: &mut HashMap<String, String>,
    slot: &str,
    (key, mut record): (String, Value),
) {
    if last.get(slot) == Some(&key) {
        return;
    }
    let (project, role) = slot.split_once('/').unwrap_or((slot, ""));
    record["at_ms"] = json!(now_ms());
    record["project"] = json!(project);
    record["role"] = json!(role);
    match append(&config.log, &record) {
        Ok(()) => {
            last.insert(slot.to_owned(), key);
        }
        Err(error) => eprintln!("agentc-supervisor shadow: {error:#}"),
    }
}

/// Appends one JSON line, creating the file and its directory if needed.
fn append(path: &Path, record: &Value) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("open {}", path.display()))?;
    writeln!(file, "{record}").with_context(|| format!("append {}", path.display()))
}

/// Milliseconds since the Unix epoch (0 if the clock is before it).
pub(crate) fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis())
}

/// Summarises a would-launch log: distinct launches and estimated spend per
/// role, errors, the largest human queue seen and the time span covered.
pub fn report(log: &Path) -> Result<Value> {
    let text = std::fs::read_to_string(log).with_context(|| format!("read {}", log.display()))?;
    let records: Vec<Value> = text
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect();
    Ok(summarize(&records))
}

/// Pure aggregation behind `report`.
fn summarize(records: &[Value]) -> Value {
    let errors = records.iter().filter(|r| r["event"] == "error").count();
    let max_queue = records
        .iter()
        .filter_map(|r| r["queue"]["human_queue"].as_u64())
        .max()
        .unwrap_or(0);
    json!({"records": records.len(), "span_hours": span_hours(records),
           "roles": per_role(records), "errors": errors, "max_human_queue": max_queue})
}

/// Distinct would-launch targets and their summed estimate, per role. A
/// target that disappears and reappears (e.g. after an error) counts once,
/// because a live supervisor would have launched it once.
fn per_role(records: &[Value]) -> BTreeMap<String, Value> {
    let mut seen = BTreeSet::new();
    let mut roles: BTreeMap<String, (u64, f64)> = BTreeMap::new();
    for record in records.iter().filter(|r| r["event"] == "would_launch") {
        let role = record["role"].as_str().unwrap_or("?").to_owned();
        let target = (
            record["project"].to_string(),
            role.clone(),
            record["target"].to_string(),
        );
        if seen.insert(target) {
            let entry = roles.entry(role).or_default();
            entry.0 += 1;
            entry.1 += record["estimate"]["usd"].as_f64().unwrap_or(0.0);
        }
    }
    roles
        .into_iter()
        .map(|(role, (n, usd))| {
            (
                role,
                json!({"would_launch": n, "estimated_usd": estimate::cents(usd)}),
            )
        })
        .collect()
}

/// Hours between the first and last record, to one decimal.
fn span_hours(records: &[Value]) -> f64 {
    let times = records.iter().filter_map(|r| r["at_ms"].as_u64());
    match (times.clone().min(), times.max()) {
        (Some(first), Some(last)) => ((last - first) as f64 / 360_000.0).round() / 10.0,
        _ => 0.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `next` response for `role` with `action` and two human-gated tasks.
    fn next(role: &str, action: Value) -> Value {
        json!({"role": role, "action": action, "human_queue": 2, "skipped": {}, "caller_steps": []})
    }

    /// Logs `next` for the implementer slot of project `p`.
    fn log(config: &ShadowConfig, last: &mut HashMap<String, String>, next: &Value) {
        log_change(config, last, "p/implementer", next_record(config, next));
    }

    #[test]
    fn repeated_and_reappearing_targets_count_once() {
        let dir = tempfile::tempdir().unwrap();
        let config = ShadowConfig {
            log: dir.path().join("log.jsonl"),
            ..ShadowConfig::default()
        };
        let mut last = HashMap::new();
        let task = next(
            "implementer",
            json!({"kind": "claim_task", "task_id": "t1"}),
        );
        let idle = next("implementer", Value::Null);
        for state in [&task, &task, &idle, &task] {
            log(&config, &mut last, state);
        }
        let summary = report(&config.log).unwrap();
        assert_eq!(summary["records"], 3);
        assert_eq!(summary["roles"]["implementer"]["would_launch"], 1);
        assert_eq!(summary["roles"]["implementer"]["estimated_usd"], 3.92);
        assert_eq!(summary["max_human_queue"], 2);
    }

    #[test]
    fn reviews_use_the_reviewer_profile() {
        let config = ShadowConfig::default();
        let action = json!({"kind": "claim_review", "activity_id": "a1", "task_id": null});
        let (key, record) = next_record(&config, &next("reviewer", action));
        assert_eq!(key, "launch:claim_review:a1");
        assert_eq!(record["estimate"]["output_tokens"], 20_000);
    }

    #[test]
    fn configured_prices_merge_over_defaults() {
        let config: ShadowConfig =
            toml::from_str("[prices.gpt-x]\ninput = 1.0\ncached_input = 0.1\noutput = 4.0")
                .unwrap();
        let table = config.price_table();
        assert!(table.contains_key("gpt-x") && table.contains_key("claude-opus-5-5"));
    }

    #[test]
    fn empty_config_equals_defaults() {
        assert_eq!(
            toml::from_str::<ShadowConfig>("").unwrap(),
            ShadowConfig::default()
        );
    }
}

#[cfg(test)]
mod credential_tests {
    use super::*;

    #[test]
    fn parse_errors_never_quote_the_file() {
        let error = client_from("token = \"secret-value", Path::new("c.toml"), "", false)
            .err()
            .unwrap();
        assert!(!format!("{error:#}").contains("secret-value"));
    }
}
