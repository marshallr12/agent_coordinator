//! Authenticated, stateless MCP Streamable HTTP transport.
//!
//! The MCP handler can only dispatch requests prepared by the closed catalog.
//! Every prepared request passes through the ordinary REST authentication and
//! authorization middleware, so an MCP connection or prior tool result never
//! renews coordination authority.

mod catalog;

use axum::{
    Router,
    body::Body,
    extract::{Request, State},
    http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, header, request::Parts},
    middleware::{self, Next},
    response::{IntoResponse, Response},
};
use http_body_util::BodyExt;
use rmcp::{
    ErrorData as McpError, RoleServer, ServerHandler,
    model::{
        CallToolRequestParams, CallToolResponse, CallToolResult, Implementation, ListToolsResult,
        PaginatedRequestParams, ServerCapabilities, ServerInfo, Tool,
    },
    service::RequestContext,
    transport::streamable_http_server::{
        StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
    },
};
use serde_json::{Value, json};
use tower::ServiceExt;

use crate::{
    auth::{Auth, PROOF_HEADER, SESSION_HEADER},
    error::AppError,
    state::AppState,
};

const MAX_REST_RESULT_BYTES: usize = 1024 * 1024;
const AUTH_HELP: &str = "/api/v1/help/authentication";
const MCP_INSTRUCTIONS: &str = "1. Obtain an agent credential from a human administrator. Run `agent-coordinator --session NAME connect`, then start a trusted MCP client with `agent-coordinator --session NAME mcp-client -- /absolute/path/to/trusted-client [ARGS...]`; configure that client to map the protected AGENT_COORDINATOR_MCP_TOKEN, AGENT_COORDINATOR_MCP_SESSION_ID, and AGENT_COORDINATOR_MCP_SESSION_PROOF environment values to Authorization: Bearer, X-Coordinator-Session, and X-Coordinator-Session-Proof. Never put credentials in tool arguments, command arguments, URLs, logs, or repositories. 2. List tools, then inspect the configured session with coordinator_session_get or register it once with coordinator_session_register. 3. Read coordinator_orientation before choosing work. 4. Acknowledge the exact instruction and project-policy revisions with coordinator_instructions_ack. 5. Claim eligible work atomically with coordinator_claim. 6. Use the same launcher environment with the native CLI and a separate worktree to inspect and change actual source; MCP never runs Git or local processes. 7. Checkpoint evidence and renew before expiry; MCP ping, initialize, metadata calls, and receipt replay do not renew ownership. 8. Submit immutable evidence and lessons with coordinator_submit. 9. Required independent review must use a different eligible principal, followed by serialized integration and integrated checks against exact source identities. Persist every mutation's idempotency key and exact body before sending it, and reuse both after an uncertain result.";

/// Coordinator credentials deliberately do not implement `Debug` or
/// `Serialize`. The guard removes their headers before rmcp receives the HTTP
/// request and carries only this opaque extension to the handler.
#[derive(Clone)]
struct CoordinatorHeaders(HeaderMap);

#[derive(Clone)]
struct CoordinatorMcp {
    rest: Router,
}

pub fn routes(state: AppState) -> Router {
    let public_origin = url::Url::parse(&state.config.public_origin)
        .expect("AppState only contains a validated public origin");
    let public_authority = canonical_authority(&public_origin);
    let config = StreamableHttpServerConfig::default()
        .with_legacy_session_mode(false)
        .with_json_response(true)
        .with_stateless_protocol_metadata_required(false)
        .with_allowed_hosts([public_authority])
        .with_allowed_origins([state.config.public_origin.clone()])
        .with_max_request_body_bytes(state.config.json_body_limit_bytes);
    let rest = crate::rest_router(state.clone());
    let service: StreamableHttpService<CoordinatorMcp, LocalSessionManager> =
        StreamableHttpService::new(
            move || Ok(CoordinatorMcp { rest: rest.clone() }),
            Default::default(),
            config,
        );

    Router::new()
        .route_service("/mcp", service)
        // The guard is outside the SDK service. Host/origin and authentication
        // failures are sanitized before rmcp can log caller-controlled values.
        .layer(middleware::from_fn_with_state(state, guard))
}

impl ServerHandler for CoordinatorMcp {
    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        if request.input_responses.is_some() || request.request_state.is_some() {
            return Ok(tool_error(
                "invalid_request",
                "These coordination tools do not accept elicitation responses or request state.",
                json!({}),
            )
            .into());
        }
        let Some(parts) = context.extensions.get::<Parts>() else {
            return Ok(tool_error(
                "internal_error",
                "The operation could not be completed.",
                json!({}),
            )
            .into());
        };
        let Some(headers) = parts.extensions.get::<CoordinatorHeaders>() else {
            return Ok(tool_error(
                "authentication_required",
                "Configure this workstation's agent credential, session ID, and session proof, then reconnect.",
                json!({"help_path":AUTH_HELP}),
            )
            .into());
        };
        let args = request.arguments.unwrap_or_default();
        let inner = match catalog::prepare(&request.name, args, &headers.0) {
            Ok(inner) => inner,
            Err(error) => return Ok(app_error_result(error).await.into()),
        };

        // This fresh router invocation intentionally performs ordinary REST
        // authentication and all operation-specific authority checks again.
        let response = match self.rest.clone().oneshot(inner).await {
            Ok(response) => response,
            Err(never) => match never {},
        };
        Ok(response_result(response).await.into())
    }

    async fn list_tools(
        &self,
        request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        if request.is_some_and(|request| request.cursor.is_some()) {
            return Err(McpError::invalid_params(
                "The fixed tool catalog is not paginated; omit cursor.",
                None,
            ));
        }
        Ok(ListToolsResult {
            tools: catalog::tools(),
            ..Default::default()
        })
    }

    fn get_tool(&self, name: &str) -> Option<Tool> {
        catalog::tools().into_iter().find(|tool| tool.name == name)
    }

    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(
                Implementation::new("agent-coordinator", env!("CARGO_PKG_VERSION"))
                    .with_title("Agent Coordinator")
                    .with_description(
                        "Authenticated coordination tools with durable ownership and replay safety.",
                    ),
            )
            .with_instructions(MCP_INSTRUCTIONS)
    }
}

async fn guard(State(state): State<AppState>, request: Request, next: Next) -> Response {
    let (mut parts, body) = request.into_parts();
    if let Err(error) = validate_public_request(&parts, &state.config.public_origin) {
        return secure_response(error.into_response(), &state);
    }
    if parts.uri.query().is_some() {
        return secure_response(
            AppError::bad_request("The MCP endpoint does not accept query parameters.")
                .into_response(),
            &state,
        );
    }
    if parts.headers.contains_key(header::COOKIE) {
        return authentication_response(AppError::auth_required(), &state);
    }

    let coordinator_headers = coordinator_headers(&parts.headers);
    let synthetic = synthetic_auth_parts(&coordinator_headers.0);
    if let Err(error) = Auth::authenticate(&synthetic, &state).await {
        return authentication_response(error, &state);
    }

    // Authenticate before method handling so unauthenticated probes do not
    // learn transport behavior. Streamable HTTP is POST-only and stateless.
    if parts.method != Method::POST {
        let mut response = AppError::new(
            StatusCode::METHOD_NOT_ALLOWED,
            "method_not_allowed",
            "The MCP endpoint accepts authenticated POST requests only.",
        )
        .into_response();
        response
            .headers_mut()
            .insert(header::ALLOW, HeaderValue::from_static("POST"));
        return secure_response(response, &state);
    }

    parts.headers = sanitized_sdk_headers(&parts.headers, &state.config.public_origin);
    parts.extensions.insert(coordinator_headers);
    let response = next.run(Request::from_parts(parts, body)).await;
    secure_response(response, &state)
}

fn coordinator_headers(headers: &HeaderMap) -> CoordinatorHeaders {
    let mut selected = HeaderMap::new();
    for key in [header::AUTHORIZATION.as_str(), SESSION_HEADER, PROOF_HEADER] {
        for value in headers.get_all(key) {
            let mut value = value.clone();
            value.set_sensitive(true);
            selected.append(HeaderName::from_static(key), value);
        }
    }
    CoordinatorHeaders(selected)
}

fn synthetic_auth_parts(headers: &HeaderMap) -> Parts {
    let request = Request::builder()
        .method(Method::GET)
        .uri("/api/v1/me")
        .body(Body::empty())
        .expect("static authentication request is valid");
    let (mut parts, _) = request.into_parts();
    for value in headers.get_all(header::AUTHORIZATION) {
        parts.headers.append(header::AUTHORIZATION, value.clone());
    }
    parts
}

fn sanitized_sdk_headers(headers: &HeaderMap, public_origin: &str) -> HeaderMap {
    let origin = url::Url::parse(public_origin).expect("validated public origin");
    let mut selected = HeaderMap::new();
    selected.insert(
        header::HOST,
        HeaderValue::from_str(&canonical_authority(&origin))
            .expect("validated origin authority is an HTTP header"),
    );
    if headers.contains_key(header::ORIGIN) {
        selected.insert(
            header::ORIGIN,
            HeaderValue::from_str(public_origin)
                .expect("validated public origin is an HTTP header"),
        );
    }
    for (name, value) in headers {
        let lower = name.as_str();
        if matches!(lower, "accept" | "content-type") || lower.starts_with("mcp-") {
            selected.append(name.clone(), value.clone());
        }
    }
    selected
}

fn validate_public_request(parts: &Parts, public_origin: &str) -> Result<(), AppError> {
    let configured = url::Url::parse(public_origin).map_err(|_| AppError::internal())?;
    let mut hosts = parts.headers.get_all(header::HOST).iter();
    let supplied = match (hosts.next(), hosts.next()) {
        (Some(value), None) => value
            .to_str()
            .ok()
            .and_then(|value| value.parse::<axum::http::uri::Authority>().ok()),
        (None, None) => parts.uri.authority().cloned(),
        _ => None,
    }
    .ok_or_else(|| {
        AppError::bad_request("Supply exactly one valid Host header for the configured service.")
    })?;
    if !authority_matches(&supplied, &configured) {
        return Err(AppError::new(
            StatusCode::FORBIDDEN,
            "origin_not_permitted",
            "The request Host is not permitted for this service.",
        ));
    }

    let mut origins = parts.headers.get_all(header::ORIGIN).iter();
    match (origins.next(), origins.next()) {
        (None, None) => {}
        (Some(value), None) => {
            let supplied = value
                .to_str()
                .ok()
                .and_then(|value| url::Url::parse(value).ok())
                .filter(is_origin_only);
            if !supplied.is_some_and(|origin| same_origin(&origin, &configured)) {
                return Err(AppError::new(
                    StatusCode::FORBIDDEN,
                    "origin_not_permitted",
                    "The request Origin is not permitted for this service.",
                ));
            }
        }
        _ => {
            return Err(AppError::bad_request(
                "Supply at most one valid Origin header.",
            ));
        }
    }

    for name in parts.headers.keys() {
        if name.as_str().starts_with("mcp-") && parts.headers.get_all(name).iter().nth(1).is_some()
        {
            return Err(AppError::bad_request(
                "Each MCP protocol metadata header may appear only once.",
            ));
        }
    }
    Ok(())
}

fn canonical_authority(origin: &url::Url) -> String {
    let host = match origin.host().expect("validated public origin has a host") {
        url::Host::Domain(host) => host.to_owned(),
        url::Host::Ipv4(host) => host.to_string(),
        url::Host::Ipv6(host) => format!("[{host}]"),
    };
    match origin.port() {
        Some(port) => format!("{host}:{port}"),
        None => host,
    }
}

fn authority_matches(supplied: &axum::http::uri::Authority, configured: &url::Url) -> bool {
    let configured_host = match configured.host() {
        Some(url::Host::Domain(host)) => host.to_owned(),
        Some(url::Host::Ipv4(host)) => host.to_string(),
        Some(url::Host::Ipv6(host)) => host.to_string(),
        None => return false,
    };
    supplied
        .host()
        .trim_matches(['[', ']'])
        .eq_ignore_ascii_case(&configured_host)
        && effective_port(supplied.port_u16(), configured.scheme())
            == effective_port(configured.port(), configured.scheme())
}

fn same_origin(left: &url::Url, right: &url::Url) -> bool {
    left.scheme().eq_ignore_ascii_case(right.scheme())
        && left
            .host_str()
            .zip(right.host_str())
            .is_some_and(|(left, right)| left.eq_ignore_ascii_case(right))
        && effective_port(left.port(), left.scheme())
            == effective_port(right.port(), right.scheme())
}

fn effective_port(port: Option<u16>, scheme: &str) -> Option<u16> {
    port.or(match scheme {
        "https" => Some(443),
        "http" => Some(80),
        _ => None,
    })
}

fn is_origin_only(origin: &url::Url) -> bool {
    origin.username().is_empty()
        && origin.password().is_none()
        && origin.path() == "/"
        && origin.query().is_none()
        && origin.fragment().is_none()
        && origin.host_str().is_some()
}

async fn app_error_result(error: AppError) -> CallToolResult {
    response_result(error.into_response()).await
}

async fn response_result(response: Response) -> CallToolResult {
    let success = response.status().is_success();
    let mut body = response.into_body();
    let mut bytes = Vec::new();
    while let Some(frame) = body.frame().await {
        let frame = match frame {
            Ok(frame) => frame,
            Err(_) => {
                return uncertain_output_error(
                    "The service response could not be read. The operation may have committed.",
                );
            }
        };
        if let Ok(data) = frame.into_data() {
            let Some(total) = bytes.len().checked_add(data.len()) else {
                return uncertain_output_error(
                    "The service response exceeded the MCP output limit. The operation may have committed.",
                );
            };
            if total > MAX_REST_RESULT_BYTES {
                return uncertain_output_error(
                    "The service response exceeded the 1 MiB REST-result limit for MCP tools. The operation may have committed.",
                );
            }
            bytes.extend_from_slice(&data);
        }
    }
    let value: Value = match serde_json::from_slice(&bytes) {
        Ok(value) => value,
        Err(_) => {
            return uncertain_output_error(
                "The service returned an invalid MCP tool response. The operation may have committed.",
            );
        }
    };
    if success {
        CallToolResult::structured(value)
    } else {
        CallToolResult::structured_error(value)
    }
}

fn uncertain_output_error(message: &str) -> CallToolResult {
    tool_error(
        "mcp_output_unavailable",
        message,
        json!({
            "next_actions":[
                {"action":"reuse_saved_request","detail":"Retry with the same persisted idempotency key and exact body."},
                {"action":"inspect_current_state","detail":"Inspect through authenticated REST, the native CLI, or bounded paginated history before deciding whether to retry."}
            ]
        }),
    )
}

fn tool_error(code: &str, message: &str, details: Value) -> CallToolResult {
    CallToolResult::structured_error(json!({
        "request_id":uuid::Uuid::new_v4().to_string(),
        "server_time":chrono::Utc::now().to_rfc3339(),
        "error":{
            "code":code,
            "message":message,
            "details":details,
            "next_actions":[],
            "retryable":false
        }
    }))
}

fn authentication_response(error: AppError, state: &AppState) -> Response {
    let mut response = error.into_response();
    response.headers_mut().insert(
        header::WWW_AUTHENTICATE,
        HeaderValue::from_static("Bearer realm=\"agent-coordinator\""),
    );
    response.headers_mut().insert(
        header::LINK,
        HeaderValue::from_static("</api/v1/help/authentication>; rel=\"help\""),
    );
    secure_response(response, state)
}

fn secure_response(mut response: Response, state: &AppState) -> Response {
    let headers = response.headers_mut();
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    headers.insert("x-frame-options", HeaderValue::from_static("DENY"));
    headers.insert("referrer-policy", HeaderValue::from_static("no-referrer"));
    if state.config.secure_cookie() {
        headers.insert(
            "strict-transport-security",
            HeaderValue::from_static("max-age=31536000"),
        );
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_and_origin_matching_normalizes_default_ports() {
        let configured = url::Url::parse("https://Example.COM").unwrap();
        assert!(authority_matches(
            &"example.com:443".parse().unwrap(),
            &configured
        ));
        assert!(!authority_matches(
            &"example.com:444".parse().unwrap(),
            &configured
        ));
        assert!(same_origin(
            &url::Url::parse("https://example.com:443").unwrap(),
            &configured
        ));
        assert!(!same_origin(
            &url::Url::parse("http://example.com").unwrap(),
            &configured
        ));
        let ipv6 = url::Url::parse("https://[::1]:8443").unwrap();
        assert_eq!(canonical_authority(&ipv6), "[::1]:8443");
        assert!(authority_matches(&"[::1]:8443".parse().unwrap(), &ipv6));
        assert!(!authority_matches(&"[::1]".parse().unwrap(), &ipv6));
        let ipv6_default = url::Url::parse("http://[::1]").unwrap();
        assert_eq!(canonical_authority(&ipv6_default), "[::1]");
        assert!(authority_matches(
            &"[::1]:80".parse().unwrap(),
            &ipv6_default
        ));
    }

    #[test]
    fn sdk_headers_never_receive_coordinator_credentials() {
        let mut headers = HeaderMap::new();
        headers.insert(header::HOST, HeaderValue::from_static("example.com"));
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Bearer secret"),
        );
        headers.insert(SESSION_HEADER, HeaderValue::from_static("session-secret"));
        headers.insert(PROOF_HEADER, HeaderValue::from_static("proof-secret"));
        headers.insert(header::COOKIE, HeaderValue::from_static("cookie-secret"));
        headers.insert(
            "mcp-protocol-version",
            HeaderValue::from_static("2025-11-25"),
        );
        headers.insert(
            "x-unknown-secret",
            HeaderValue::from_static("unknown-secret"),
        );

        let sanitized = sanitized_sdk_headers(&headers, "https://example.com");
        assert_eq!(sanitized.get(header::HOST).unwrap(), "example.com");
        assert!(sanitized.contains_key("mcp-protocol-version"));
        for forbidden in [
            header::AUTHORIZATION.as_str(),
            SESSION_HEADER,
            PROOF_HEADER,
            header::COOKIE.as_str(),
            "x-unknown-secret",
        ] {
            assert!(!sanitized.contains_key(forbidden));
        }
    }
}
