//! Small, policy-enforcing HTTP client for Agent Coordinator.
//!
//! The client deliberately accepts only a service origin plus API paths. It
//! never follows redirects, so credentials cannot be forwarded to a different
//! origin. HTTP is available only for explicit loopback development.

use std::fmt;
use std::net::IpAddr;
use std::time::Duration;

use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderValue};
use reqwest::{Method, StatusCode, Url};
use serde::{Deserialize, Serialize};
use serde_json::Value;

const SESSION_HEADER: &str = "x-coordinator-session";
const SESSION_PROOF_HEADER: &str = "x-coordinator-session-proof";
const IDEMPOTENCY_HEADER: &str = "idempotency-key";

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum HttpMethod {
    Get,
    Post,
    Patch,
}

impl HttpMethod {
    fn reqwest(&self) -> Method {
        match self {
            Self::Get => Method::GET,
            Self::Post => Method::POST,
            Self::Patch => Method::PATCH,
        }
    }

    pub fn is_mutation(&self) -> bool {
        matches!(self, Self::Post | Self::Patch)
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct SessionAuth {
    pub id: String,
    pub proof: String,
}

impl fmt::Debug for SessionAuth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SessionAuth")
            .field("id", &self.id)
            .field("proof", &"[REDACTED]")
            .finish()
    }
}

#[derive(Debug)]
pub enum ClientError {
    InvalidOrigin(String),
    InvalidPath(String),
    InvalidHeader(&'static str),
    Transport(reqwest::Error),
    InvalidResponse(String),
}

impl fmt::Display for ClientError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidOrigin(message) => write!(f, "invalid service origin: {message}"),
            Self::InvalidPath(message) => write!(f, "invalid API path: {message}"),
            Self::InvalidHeader(name) => write!(f, "invalid value for {name}"),
            Self::Transport(error) if error.is_timeout() => write!(f, "service request timed out"),
            Self::Transport(_) => write!(f, "service request failed"),
            Self::InvalidResponse(message) => write!(f, "invalid service response: {message}"),
        }
    }
}

impl std::error::Error for ClientError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Transport(error) => Some(error),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ApiResponse {
    pub status: u16,
    pub body: Value,
}

impl ApiResponse {
    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }

    pub fn status_code(&self) -> Option<StatusCode> {
        StatusCode::from_u16(self.status).ok()
    }
}

#[derive(Clone)]
pub struct CoordinatorClient {
    origin: Url,
    token: Option<String>,
    http: reqwest::Client,
}

impl CoordinatorClient {
    pub fn new(
        service_origin: &str,
        token: impl Into<String>,
        allow_insecure_loopback: bool,
    ) -> Result<Self, ClientError> {
        let origin = validate_origin(service_origin, allow_insecure_loopback)?;
        let token = token.into();
        if token.is_empty() {
            return Err(ClientError::InvalidHeader("authorization"));
        }
        Ok(Self {
            origin,
            token: Some(token),
            http: http_client()?,
        })
    }

    pub fn unauthenticated(
        service_origin: &str,
        allow_insecure_loopback: bool,
    ) -> Result<Self, ClientError> {
        Ok(Self {
            origin: validate_origin(service_origin, allow_insecure_loopback)?,
            token: None,
            http: http_client()?,
        })
    }

    pub fn origin(&self) -> &str {
        self.origin.as_str().trim_end_matches('/')
    }

    pub async fn get(
        &self,
        path: &str,
        session: Option<&SessionAuth>,
    ) -> Result<ApiResponse, ClientError> {
        self.request(HttpMethod::Get, path, None, None, session, true)
            .await
    }

    pub async fn get_query(
        &self,
        path: &str,
        query: &[(&str, String)],
        session: Option<&SessionAuth>,
    ) -> Result<ApiResponse, ClientError> {
        self.request_inner(HttpMethod::Get, path, query, None, None, session, true)
            .await
    }

    pub async fn mutate(
        &self,
        path: &str,
        body: &Value,
        idempotency_key: &str,
        session: Option<&SessionAuth>,
    ) -> Result<ApiResponse, ClientError> {
        self.request(
            HttpMethod::Post,
            path,
            Some(body),
            Some(idempotency_key),
            session,
            true,
        )
        .await
    }

    /// Sends a session-creation mutation. The proof is sent, but the session ID
    /// header is intentionally omitted because the session does not exist yet.
    pub async fn create_session(
        &self,
        body: &Value,
        idempotency_key: &str,
        session: &SessionAuth,
    ) -> Result<ApiResponse, ClientError> {
        self.request(
            HttpMethod::Post,
            "/api/v1/sessions",
            Some(body),
            Some(idempotency_key),
            Some(session),
            false,
        )
        .await
    }

    pub async fn request(
        &self,
        method: HttpMethod,
        path: &str,
        body: Option<&Value>,
        idempotency_key: Option<&str>,
        session: Option<&SessionAuth>,
        include_session_id: bool,
    ) -> Result<ApiResponse, ClientError> {
        self.request_inner(
            method,
            path,
            &[],
            body,
            idempotency_key,
            session,
            include_session_id,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn request_inner(
        &self,
        method: HttpMethod,
        path: &str,
        query: &[(&str, String)],
        body: Option<&Value>,
        idempotency_key: Option<&str>,
        session: Option<&SessionAuth>,
        include_session_id: bool,
    ) -> Result<ApiResponse, ClientError> {
        let mut url = api_url(&self.origin, path)?;
        if !query.is_empty() {
            let mut pairs = url.query_pairs_mut();
            for (name, value) in query {
                pairs.append_pair(name, value);
            }
        }
        if method.is_mutation() && idempotency_key.is_none() {
            return Err(ClientError::InvalidHeader(IDEMPOTENCY_HEADER));
        }

        let mut headers = HeaderMap::new();
        if let Some(token) = &self.token {
            let bearer = HeaderValue::from_str(&format!("Bearer {token}"))
                .map_err(|_| ClientError::InvalidHeader("authorization"))?;
            headers.insert(AUTHORIZATION, bearer);
        }
        if let Some(key) = idempotency_key {
            headers.insert(
                IDEMPOTENCY_HEADER,
                HeaderValue::from_str(key)
                    .map_err(|_| ClientError::InvalidHeader(IDEMPOTENCY_HEADER))?,
            );
        }
        if let Some(session) = session {
            if include_session_id {
                headers.insert(
                    SESSION_HEADER,
                    HeaderValue::from_str(&session.id)
                        .map_err(|_| ClientError::InvalidHeader(SESSION_HEADER))?,
                );
            }
            headers.insert(
                SESSION_PROOF_HEADER,
                HeaderValue::from_str(&session.proof)
                    .map_err(|_| ClientError::InvalidHeader(SESSION_PROOF_HEADER))?,
            );
        }

        let mut request = self.http.request(method.reqwest(), url).headers(headers);
        if let Some(body) = body {
            request = request.header(CONTENT_TYPE, "application/json").json(body);
        }
        let response = request.send().await.map_err(ClientError::Transport)?;
        let status = response.status().as_u16();
        let bytes = response.bytes().await.map_err(ClientError::Transport)?;
        if (300..400).contains(&status) {
            return Ok(ApiResponse {
                status,
                body: serde_json::json!({
                    "error": {
                        "code": "redirect_refused",
                        "message": "the service returned a redirect; update the configured service origin explicitly",
                        "details": {},
                        "next_actions": [],
                        "retryable": false
                    }
                }),
            });
        }
        let body: Value = serde_json::from_slice(&bytes).map_err(|_| {
            ClientError::InvalidResponse(format!(
                "HTTP {status} did not contain a JSON response body"
            ))
        })?;
        validate_envelope(status, &body)?;
        Ok(ApiResponse { status, body })
    }
}

fn http_client() -> Result<reqwest::Client, ClientError> {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(ClientError::Transport)
}

pub fn normalize_origin(
    service_origin: &str,
    allow_insecure_loopback: bool,
) -> Result<String, ClientError> {
    validate_origin(service_origin, allow_insecure_loopback)
        .map(|url| url.as_str().trim_end_matches('/').to_owned())
}

fn validate_origin(input: &str, allow_insecure_loopback: bool) -> Result<Url, ClientError> {
    let mut url = Url::parse(input)
        .map_err(|_| ClientError::InvalidOrigin("expected an absolute URL".into()))?;
    if !url.username().is_empty() || url.password().is_some() {
        return Err(ClientError::InvalidOrigin(
            "credentials are not allowed in the URL".into(),
        ));
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err(ClientError::InvalidOrigin(
            "query strings and fragments are not allowed".into(),
        ));
    }
    if url.path() != "/" && !url.path().is_empty() {
        return Err(ClientError::InvalidOrigin(
            "the origin must not include a path".into(),
        ));
    }
    if url.host_str().is_none() {
        return Err(ClientError::InvalidOrigin("a host is required".into()));
    }

    match url.scheme() {
        "https" => {}
        "http" if allow_insecure_loopback && is_loopback(&url) => {}
        "http" => {
            return Err(ClientError::InvalidOrigin(
                "HTTP is allowed only for loopback development with explicit opt-in".into(),
            ));
        }
        _ => return Err(ClientError::InvalidOrigin("HTTPS is required".into())),
    }
    url.set_path("/");
    Ok(url)
}

fn is_loopback(url: &Url) -> bool {
    let Some(host) = url.host_str() else {
        return false;
    };
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<IpAddr>()
            .map(|address| address.is_loopback())
            .unwrap_or(false)
}

fn api_url(origin: &Url, path: &str) -> Result<Url, ClientError> {
    if !(path == "/healthz" || path == "/api/v1" || path.starts_with("/api/v1/")) {
        return Err(ClientError::InvalidPath(
            "only /healthz and /api/v1 paths are allowed".into(),
        ));
    }
    if path.contains('?') || path.contains('#') || path.starts_with("//") {
        return Err(ClientError::InvalidPath(
            "query strings, fragments, and network paths are not allowed".into(),
        ));
    }
    if path
        .split('/')
        .any(|segment| segment == "." || segment == "..")
    {
        return Err(ClientError::InvalidPath(
            "dot path segments are not allowed".into(),
        ));
    }
    let mut url = origin.clone();
    url.set_path(path);
    Ok(url)
}

fn validate_envelope(status: u16, body: &Value) -> Result<(), ClientError> {
    let object = body.as_object().ok_or_else(|| {
        ClientError::InvalidResponse(format!("HTTP {status} JSON body is not an object"))
    })?;
    let common = object.get("request_id").and_then(Value::as_str).is_some()
        && object.get("server_time").and_then(Value::as_str).is_some();
    let shape = if (200..300).contains(&status) {
        object.contains_key("data")
    } else {
        object
            .get("error")
            .and_then(Value::as_object)
            .is_some_and(|error| {
                error.get("code").and_then(Value::as_str).is_some()
                    && error.get("message").and_then(Value::as_str).is_some()
            })
    };
    if common && shape {
        Ok(())
    } else {
        Err(ClientError::InvalidResponse(format!(
            "HTTP {status} did not match the coordinator response envelope"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    #[test]
    fn origin_policy_requires_https_or_explicit_loopback() {
        assert!(validate_origin("https://coordinator.example", false).is_ok());
        assert!(validate_origin("http://coordinator.example", true).is_err());
        assert!(validate_origin("http://127.0.0.1:8080", false).is_err());
        assert!(validate_origin("http://127.0.0.1:8080", true).is_ok());
        assert!(validate_origin("https://user:secret@example.test", false).is_err());
        assert!(validate_origin("https://example.test/prefix", false).is_err());
    }

    #[test]
    fn paths_cannot_escape_the_api() {
        let origin = validate_origin("https://example.test", false).unwrap();
        assert!(api_url(&origin, "/api/v1/projects").is_ok());
        assert!(api_url(&origin, "//attacker.test/api/v1").is_err());
        assert!(api_url(&origin, "/api/v1/../admin").is_err());
        assert!(api_url(&origin, "/api/v1/projects?token=x").is_err());
    }

    #[test]
    fn only_complete_coordinator_envelopes_are_accepted() {
        assert!(validate_envelope(200, &serde_json::json!({})).is_err());
        assert!(
            validate_envelope(
                200,
                &serde_json::json!({
                    "data": {}, "request_id": "r", "server_time": "2026-09-09T00:00:00Z"
                })
            )
            .is_ok()
        );
        assert!(
            validate_envelope(
                409,
                &serde_json::json!({
                    "error": {"code": "claim_conflict", "message": "busy"},
                    "request_id": "r", "server_time": "2026-09-09T00:00:00Z"
                })
            )
            .is_ok()
        );
    }

    #[tokio::test]
    async fn redirects_are_returned_and_never_followed() {
        let destination = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let destination_address = destination.local_addr().unwrap();
        let reached_destination = Arc::new(AtomicBool::new(false));
        let flag = reached_destination.clone();
        let destination_task = tokio::spawn(async move {
            if tokio::time::timeout(Duration::from_millis(300), destination.accept())
                .await
                .is_ok()
            {
                flag.store(true, Ordering::SeqCst);
            }
        });

        let source = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let source_address = source.local_addr().unwrap();
        let source_task = tokio::spawn(async move {
            let (mut stream, _) = source.accept().await.unwrap();
            let mut input = [0_u8; 2048];
            let _ = stream.read(&mut input).await.unwrap();
            let reply = format!(
                "HTTP/1.1 302 Found\r\nLocation: http://{destination_address}/api/v1/info\r\nContent-Type: application/json\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{{}}"
            );
            stream.write_all(reply.as_bytes()).await.unwrap();
        });

        let client =
            CoordinatorClient::new(&format!("http://{source_address}"), "test-secret", true)
                .unwrap();
        let response = client.get("/api/v1/info", None).await.unwrap();
        assert_eq!(response.status, 302);
        source_task.await.unwrap();
        destination_task.await.unwrap();
        assert!(!reached_destination.load(Ordering::SeqCst));
    }
}
