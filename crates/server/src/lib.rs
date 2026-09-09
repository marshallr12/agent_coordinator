pub mod auth;
pub mod coordination;
pub mod error;
pub mod jobs;
pub mod mutation;
pub mod state;

use axum::{
    Json, Router,
    body::Body,
    extract::{DefaultBodyLimit, Request, State},
    http::{HeaderValue, Method, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::get,
};
use serde_json::{Value, json};
use state::AppState;

pub fn response(data: Value) -> Json<Value> {
    Json(
        json!({"data":data,"request_id":uuid::Uuid::new_v4().to_string(),"server_time":chrono::Utc::now().to_rfc3339()}),
    )
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .merge(coordination::routes())
        .merge(jobs::routes())
        .merge(auth::routes())
        .route(
            "/healthz",
            get(|| async { response(json!({"status":"ok"})) }),
        )
        .route("/api/v1/info", get(info))
        .route("/api/v1/help/authentication", get(authentication_help))
        .route(
            "/",
            get(|| async {
                asset(
                    "text/html; charset=utf-8",
                    include_str!("../../../web/index.html"),
                )
            }),
        )
        .route(
            "/app.js",
            get(|| async {
                asset(
                    "text/javascript; charset=utf-8",
                    include_str!("../../../web/app.js"),
                )
            }),
        )
        .route(
            "/style.css",
            get(|| async {
                asset(
                    "text/css; charset=utf-8",
                    include_str!("../../../web/style.css"),
                )
            }),
        )
        .fallback(|| async { error::AppError::not_found() })
        .layer(DefaultBodyLimit::max(256 * 1024))
        // Router-wide middleware includes unmatched private paths and runs
        // before route/path/query/body decoding or existence disclosure.
        .layer(middleware::from_fn_with_state(state.clone(), protect))
        .with_state(state)
}

async fn protect(State(state): State<AppState>, request: Request, next: Next) -> Response {
    let (mut parts, body) = request.into_parts();
    let public_read = matches!(parts.method, Method::GET | Method::HEAD)
        && matches!(
            parts.uri.path(),
            "/" | "/app.js"
                | "/style.css"
                | "/healthz"
                | "/api/v1/info"
                | "/api/v1/help/authentication"
        );
    let login = parts.method == Method::POST && parts.uri.path() == "/api/v1/auth/login";
    let mut result = if parts.uri.path().starts_with("/api/v1/reporters/") {
        match jobs::ReporterAuth::authenticate(&parts, &state).await {
            Ok(auth) => {
                parts.extensions.insert(auth);
                next.run(Request::from_parts(parts, body)).await
            }
            Err(error) => error.into_response(),
        }
    } else if !public_read && !login {
        match auth::Auth::authenticate(&parts, &state).await {
            Ok(auth) => {
                parts.extensions.insert(auth);
                next.run(Request::from_parts(parts, body)).await
            }
            Err(error) => error.into_response(),
        }
    } else {
        next.run(Request::from_parts(parts, body)).await
    };
    // Axum's built-in extraction/method errors also use the stable envelope;
    // their raw parser messages can include caller-controlled input.
    if result.status().is_client_error()
        && !result
            .headers()
            .get(header::CONTENT_TYPE)
            .is_some_and(|v| v.as_bytes().starts_with(b"application/json"))
    {
        let error = match result.status() {
            StatusCode::PAYLOAD_TOO_LARGE => error::AppError::new(
                StatusCode::PAYLOAD_TOO_LARGE,
                "payload_too_large",
                "The request body exceeds 256 KiB.",
            ),
            StatusCode::METHOD_NOT_ALLOWED => error::AppError::new(
                StatusCode::METHOD_NOT_ALLOWED,
                "method_not_allowed",
                "This HTTP method is not available for that route.",
            ),
            StatusCode::NOT_FOUND => error::AppError::not_found(),
            _ => error::AppError::bad_request(
                "The request body, path, or query could not be decoded.",
            ),
        };
        result = error.into_response();
    }
    let headers = result.headers_mut();
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    headers.insert("x-frame-options", HeaderValue::from_static("DENY"));
    headers.insert("referrer-policy", HeaderValue::from_static("no-referrer"));
    headers.insert(
        "permissions-policy",
        HeaderValue::from_static("camera=(), microphone=(), geolocation=()"),
    );
    headers.insert("content-security-policy", HeaderValue::from_static("default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; img-src 'self'; base-uri 'none'; frame-ancestors 'none'; form-action 'self'; object-src 'none'"));
    if state.config.secure_cookie() {
        headers.insert(
            "strict-transport-security",
            HeaderValue::from_static("max-age=31536000"),
        );
    }
    result
}

fn asset(content_type: &'static str, body: &'static str) -> Response {
    let mut result = Response::new(Body::from(body));
    result
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    result
}
async fn info() -> Json<Value> {
    response(
        json!({"product":"Agent Coordinator","version":env!("CARGO_PKG_VERSION"),"api_version":"v1","instruction_version":coordinator_core::INSTRUCTION_VERSION,
        "implementation_stage":"job_evidence","authentication_help":"/api/v1/help/authentication",
        "available_features":["local_admin_login","agent_credentials","agent_sessions","projects","project_policy","tasks","task_dependencies","orientation","claims","renewals","checkpoints","release","checkout_registration","recovery_inspection","resources","reservations","jobs","scoped_reporters","events"],
        "unavailable_features":["submission","review","integration","artifacts","knowledge","decisions","markdown_import_export","backup_restore"]}),
    )
}
async fn authentication_help() -> Json<Value> {
    response(
        json!({"title":"Connect to Agent Coordinator","message":"Ask the service administrator to enroll your workstation. Every authenticated person and agent can access all projects; credential administration requires a human administrator.",
        "browser_steps":["The host operator initializes the first administrator with the local init-admin command.","Open this service over HTTPS and sign in with your local username and password.","An administrator can issue and revoke agent credentials from the browser."],
        "agent_steps":["Obtain a workstation credential from a human administrator.","Store the credential outside repositories and bind it to this service's trusted HTTPS origin.","Use Authorization: Bearer <agent-token>. Never put credentials in a URL.","Generate and persist a random session ID and a random 32-byte session proof before registering a session.","Send X-Coordinator-Session and X-Coordinator-Session-Proof with the agent token for subsequent session work."],
        "registration_path":"/api/v1/sessions","sign_in_path":"/api/v1/auth/login","public_registration":false,
        "mutation_requirement":"Persist an Idempotency-Key and the request before each authenticated mutation. Retry uncertain requests with the same key."}),
    )
}
