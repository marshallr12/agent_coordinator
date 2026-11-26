//! Read-only access to the service.
//!
//! `adopt` never changes the service: every function in this module sends a
//! `GET`, and no other module of `adopt` talks to the service. That is why a
//! run needs no session, no idempotency key and no mutation journal, and why
//! running it again is always safe.

use coordinator_client::CoordinatorClient;
use serde_json::{Value, json};

use crate::{Failure, client_failure, require_success};

/// The most project pages one listing follows; far above any real service.
const MAX_PAGES: usize = 500;

/// The fields of a project that `adopt` uses.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Project {
    pub id: String,
    pub name: String,
    pub repository_url: String,
    pub target_branch: String,
}

impl Project {
    /// Reads a project from one item of the service's project list.
    fn from_item(item: &Value) -> Option<Self> {
        let text = |name: &str| item.get(name).and_then(Value::as_str).map(str::to_owned);
        Some(Self {
            id: text("id")?,
            name: text("name").unwrap_or_default(),
            repository_url: text("repository_url").unwrap_or_default(),
            target_branch: text("target_branch").unwrap_or_default(),
        })
    }
}

/// An authenticated, read-only connection to one service origin.
pub struct Service {
    client: CoordinatorClient,
}

impl Service {
    /// Builds the connection; the URL must be HTTPS unless loopback HTTP was permitted.
    pub fn new(service_url: &str, token: String, allow_loopback: bool) -> Result<Self, Failure> {
        let client =
            CoordinatorClient::new(service_url, token, allow_loopback).map_err(client_failure)?;
        Ok(Self { client })
    }

    /// Returns the actor the credential authenticates as (`GET /api/v1/me`).
    pub async fn actor(&self) -> Result<Value, Failure> {
        let response = self.client.get("/api/v1/me", None).await;
        let body = require_success(response.map_err(client_failure)?)?;
        Ok(body.pointer("/data/actor").cloned().unwrap_or(Value::Null))
    }

    /// Returns every unarchived project, following all pages.
    pub async fn projects(&self) -> Result<Vec<Project>, Failure> {
        let mut projects = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..MAX_PAGES {
            let page = self.project_page(cursor.as_deref()).await?;
            let items = page.pointer("/data/items").and_then(Value::as_array);
            projects.extend(items.into_iter().flatten().filter_map(Project::from_item));
            cursor = page
                .pointer("/data/next_cursor")
                .and_then(Value::as_str)
                .map(str::to_owned);
            if cursor.is_none() {
                return Ok(projects);
            }
        }
        Err(Failure::temporary("the project list did not end"))
    }

    /// Fetches one page of `GET /api/v1/projects`.
    async fn project_page(&self, cursor: Option<&str>) -> Result<Value, Failure> {
        let mut query = vec![("limit", "200".to_owned())];
        if let Some(cursor) = cursor {
            query.push(("cursor", cursor.to_owned()));
        }
        let response = self
            .client
            .get_query("/api/v1/projects", &query, None)
            .await;
        require_success(response.map_err(client_failure)?)
    }
}

/// Fetches the anonymous service description (`GET /api/v1/info`), never following redirects.
pub async fn info(service_url: &str, allow_loopback: bool) -> Result<Value, Failure> {
    let client =
        CoordinatorClient::unauthenticated(service_url, allow_loopback).map_err(client_failure)?;
    let response = client.get("/api/v1/info", None).await;
    require_success(response.map_err(client_failure)?)
}

/// Builds the "no usable credential" failure (exit 3) with the service's own setup help.
///
/// The shape matches what every other command prints when authentication is
/// missing, plus where the credential file belongs.
pub async fn credential_failure(
    service_url: &str,
    allow_loopback: bool,
    reason: &str,
    credential_location: &str,
) -> Failure {
    let help = authentication_help(service_url, allow_loopback).await;
    let message = format!(
        "{reason}. Nothing was written. Ask an administrator for an agent credential and \
         store it in {credential_location} (or set AGENT_COORDINATOR_TOKEN and \
         AGENT_COORDINATOR_ORIGIN)."
    );
    Failure {
        exit: 3,
        output: json!({"error": {
            "code": "authentication_required",
            "message": message,
            "details": {"help_path": "/api/v1/help/authentication", "service_help": help},
            "next_actions": [{"action": "show_operator_setup_help"}],
            "retryable": false
        }}),
    }
}

/// Fetches the same-origin authentication help, if the service offers it.
async fn authentication_help(service_url: &str, allow_loopback: bool) -> Option<Value> {
    let client = CoordinatorClient::unauthenticated(service_url, allow_loopback).ok()?;
    let response = client.get("/api/v1/help/authentication", None).await.ok()?;
    response.is_success().then_some(response.body)
}
