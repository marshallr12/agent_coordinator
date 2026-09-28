//! Typed calls to the coordinator's integrator routes (P4 S1/S2). Every call
//! uses the `class=integrator` bearer credential and no session. Refusals
//! (non-2xx replies with an error code) are returned as values, not errors,
//! because most of them steer the loop rather than stop it.
use anyhow::{Context, Result, anyhow};
use coordinator_client::{ApiResponse, CoordinatorClient};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::path::Path;

/// One approved, pins-current subject waiting for integration.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct QueueItem {
    pub subject_task_id: String,
    pub submission_id: String,
    pub title: String,
    pub priority: i64,
    pub candidate_revision: String,
    pub candidate_ref: Option<String>,
    pub reviewed_base: String,
    pub repository_url: String,
    pub target_branch: String,
    #[serde(default)]
    pub results: Vec<ResultRecord>,
}

/// A pinned result: R computed from target tip T0 and candidate C.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct ResultRecord {
    pub id: String,
    pub submission_id: String,
    pub t0: String,
    pub t0_tree: String,
    pub c: String,
    pub r: String,
    pub r_tree: String,
    pub landing_range: Vec<String>,
    pub roster: Value,
    /// Set while push authority is outstanding for this result.
    #[serde(default)]
    pub authority_expires_at: Option<String>,
}

/// The queue plus the service's current required-check roster.
#[derive(Debug, Clone, Deserialize)]
pub struct Queue {
    pub roster: ServiceRoster,
    pub items: Vec<QueueItem>,
}

/// The project's required-check identities as the service stores them.
#[derive(Debug, Clone, Deserialize)]
pub struct ServiceRoster {
    pub revision: i64,
    pub required_checks: Vec<Value>,
}

/// A service refusal: its code and structured details.
#[derive(Debug, Clone, PartialEq)]
pub struct Refusal {
    pub code: String,
    pub details: Value,
}

/// Either the reply's `data` or a refusal.
pub type Reply<T> = std::result::Result<T, Refusal>;

/// A new result to pin (`POST …/integrator/results`).
#[derive(Debug, Clone, Serialize)]
pub struct NewResult {
    pub submission_id: String,
    pub t0: String,
    pub t0_tree: String,
    pub c: String,
    pub r: String,
    pub r_tree: String,
    pub landing_range: Vec<String>,
    pub roster: Value,
}

/// One completed check run on R (`POST …/integrator/receipts`).
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Receipt {
    pub result_id: String,
    pub check_name: String,
    pub run_id: i64,
    pub run_attempt: i64,
    pub app_id: i64,
    pub head_sha: String,
    pub workflow_path: String,
    pub workflow_blob: String,
    pub conclusion: String,
}

/// Attested tip ancestry relative to a result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ancestry {
    Contained,
    EqualT0,
    Moved,
}

impl Ancestry {
    /// The wire value of this ancestry.
    fn as_str(self) -> &'static str {
        match self {
            Self::Contained => "contained",
            Self::EqualT0 => "equal_t0",
            Self::Moved => "moved",
        }
    }
}

/// One attested observation (`POST …/integrator/observations`).
pub struct Observation<'a> {
    pub result_id: &'a str,
    pub tip: &'a str,
    pub ancestry: Ancestry,
    pub evidence: &'a str,
    pub nonce: &'a str,
}

/// One finding to report (`POST …/integrator/reports`); the service keeps
/// the first report per (kind, dedupe_key).
#[derive(Debug, Clone, Serialize)]
pub struct NewReport {
    pub kind: &'static str,
    pub dedupe_key: String,
    pub task_id: Option<String>,
    pub submission_id: Option<String>,
    pub result_id: Option<String>,
    pub details: Value,
}

/// A stored report as the service returns it.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct ReportRecord {
    pub id: String,
    pub kind: String,
    /// Set once a human resolved the report.
    #[serde(default)]
    pub resolved_at: Option<String>,
    /// True only for a resolved `privilege_gate` report a human allowed.
    #[serde(default)]
    pub allowed: bool,
}

/// Integrator API for one coordinator origin.
pub struct Service {
    client: CoordinatorClient,
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

impl Service {
    /// Builds a client from the matching entry of a CLI-format credentials file.
    pub fn from_credential_file(
        path: &Path,
        origin: &str,
        insecure_loopback: bool,
    ) -> Result<Self> {
        let text =
            std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
        let file: CredentialFile =
            toml::from_str(&text).with_context(|| format!("parse {}", path.display()))?;
        let entry = file
            .credentials
            .into_iter()
            .find(|c| origin.is_empty() || c.origin == origin)
            .with_context(|| format!("no matching credential in {}", path.display()))?;
        let client = CoordinatorClient::new(&entry.origin, entry.token, insecure_loopback)
            .map_err(|error| anyhow!("coordinator client: {error}"))?;
        Ok(Self { client })
    }

    /// Reads the queue (also the integrator heartbeat).
    pub async fn queue(&self, project: &str) -> Result<Reply<Queue>> {
        let path = format!("/api/v1/projects/{project}/integrator/queue");
        let response = self.client.get(&path, None).await.map_err(transport)?;
        decode(response)
    }

    /// Pins a result; replays of the same (submission, t0) return the stored row.
    pub async fn record_result(
        &self,
        project: &str,
        result: &NewResult,
    ) -> Result<Reply<ResultRecord>> {
        let body = json!(result);
        self.post(project, "results", &body, &body_key("result", &body, ""))
            .await
    }

    /// Posts one check-run receipt for a result.
    pub async fn record_receipt(&self, project: &str, receipt: &Receipt) -> Result<Reply<Value>> {
        let body = json!(receipt);
        self.post(project, "receipts", &body, &body_key("receipt", &body, ""))
            .await
    }

    /// Asks for push authority; `data.granted` may be false (back to review).
    pub async fn push_authority(&self, project: &str, result_id: &str) -> Result<Reply<Value>> {
        let key = format!("authority-{}", uuid::Uuid::new_v4());
        self.post(
            project,
            "push-authority",
            &json!({"result_id": result_id}),
            &key,
        )
        .await
    }

    /// Attests the observed target tip relative to a result. `nonce` names
    /// the authority grant being ended (empty when none was issued), so an
    /// identical observation after a later grant is not replayed from the
    /// earlier one's idempotency record.
    pub async fn observe(
        &self,
        project: &str,
        observation: &Observation<'_>,
    ) -> Result<Reply<Value>> {
        let body = json!({"result_id": observation.result_id, "tip": observation.tip,
            "ancestry": observation.ancestry.as_str(), "evidence": observation.evidence});
        let key = body_key("observe", &body, observation.nonce);
        self.post(project, "observations", &body, &key).await
    }

    /// Sends the subject back to its implementer (`conflict` or `check_failed`;
    /// the latter cites the result whose receipts reproduce the failure).
    pub async fn revise(
        &self,
        project: &str,
        submission: &str,
        reason: &str,
        evidence: &str,
        result_id: Option<&str>,
    ) -> Result<Reply<Value>> {
        let body = json!({"submission_id": submission, "reason_code": reason,
            "evidence": evidence, "result_id": result_id});
        let key = format!("revise-{}", uuid::Uuid::new_v4());
        self.post(project, "revise", &body, &key).await
    }

    /// Reports a finding and returns the stored row, including any human
    /// resolution. Deduplication is the service's (kind, dedupe_key) rule,
    /// so every call uses a fresh idempotency key: a replayed key would
    /// return the reply stored before a later resolution.
    pub async fn report(&self, project: &str, report: &NewReport) -> Result<Reply<ReportRecord>> {
        let key = format!("report-{}", uuid::Uuid::new_v4());
        self.post(project, "reports", &json!(report), &key).await
    }

    /// POSTs to `…/integrator/<route>` with an idempotency key.
    async fn post<T: DeserializeOwned>(
        &self,
        project: &str,
        route: &str,
        body: &Value,
        key: &str,
    ) -> Result<Reply<T>> {
        let path = format!("/api/v1/projects/{project}/integrator/{route}");
        let response = self
            .client
            .mutate(&path, body, key, None)
            .await
            .map_err(transport)?;
        decode(response)
    }
}

/// A retry-safe idempotency key: the route prefix and a digest of the body
/// (and a nonce), so a changed body never collides with an earlier request.
fn body_key(prefix: &str, body: &Value, nonce: &str) -> String {
    let digest = Sha256::digest(format!("{body}\n{nonce}").as_bytes());
    format!("{prefix}-{}", &hex::encode(digest)[..48])
}

/// Wraps a transport error without its (possibly credential-bearing) source.
fn transport(error: coordinator_client::ClientError) -> anyhow::Error {
    anyhow!("coordinator request failed: {error}")
}

/// Splits a response into data, a coded refusal, or an error.
fn decode<T: DeserializeOwned>(response: ApiResponse) -> Result<Reply<T>> {
    if response.is_success() {
        let data = response.body.get("data").cloned().unwrap_or(Value::Null);
        return Ok(Ok(
            serde_json::from_value(data).context("decode service reply")?
        ));
    }
    let error = &response.body["error"];
    match error["code"].as_str() {
        Some(code) if response.status == 409 || response.status == 403 => Ok(Err(Refusal {
            code: code.to_string(),
            details: error["details"].clone(),
        })),
        code => Err(anyhow!(
            "service error {} {}",
            response.status,
            code.unwrap_or("unknown")
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn response(status: u16, body: Value) -> ApiResponse {
        ApiResponse { status, body }
    }

    #[test]
    fn conflicts_become_refusals_and_other_errors_fail() {
        let conflict = json!({"error": {"code": "result_conflict", "details": {"x": 1}}});
        let reply: Reply<Value> = decode(response(409, conflict)).unwrap();
        assert_eq!(reply.unwrap_err().code, "result_conflict");
        let missing = json!({"error": {"code": "record_not_found"}});
        assert!(decode::<Value>(response(404, missing)).is_err());
    }

    #[test]
    fn keys_follow_the_body_and_nonce() {
        let a = body_key("receipt", &json!({"check_name": "a"}), "");
        let b = body_key("receipt", &json!({"check_name": "b"}), "");
        assert_ne!(a, b);
        assert_ne!(a, body_key("receipt", &json!({"check_name": "a"}), "n"));
        assert!(a.len() <= 128);
    }

    #[test]
    fn success_returns_data() {
        let reply: Reply<Value> =
            decode(response(200, json!({"data": {"granted": true}}))).unwrap();
        assert_eq!(reply.unwrap()["granted"], true);
    }
}
