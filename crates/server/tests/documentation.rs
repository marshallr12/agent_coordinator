use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use coordinator_server::{
    router,
    state::{AppState, Config},
};
use http_body_util::BodyExt;
use tower::ServiceExt;

#[tokio::test]
async fn embedded_book_is_public_bounded_and_keeps_console_security_policy() {
    let directory = tempfile::tempdir().unwrap();
    let state = AppState::open(Config {
        database_path: directory.path().join("documentation.sqlite3"),
        public_origin: "https://documentation.example.test".into(),
        ..Config::default()
    })
    .await
    .unwrap();
    let app = router(state);
    let get = |path: &str| Request::builder().uri(path).body(Body::empty()).unwrap();
    let response = app.clone().oneshot(get("/documentation")).await.unwrap();
    assert_eq!(response.status(), StatusCode::PERMANENT_REDIRECT);
    assert_eq!(response.headers()["location"], "/documentation/");
    let response = app.clone().oneshot(get("/documentation/")).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()["content-type"],
        "text/html; charset=utf-8"
    );
    let csp = response.headers()["content-security-policy"]
        .to_str()
        .unwrap();
    assert!(csp.contains("script-src 'self';"));
    assert!(!csp.contains("script-src 'self' 'unsafe-inline'"));
    let html = String::from_utf8(
        response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .to_vec(),
    )
    .unwrap();
    assert!(html.contains("Agent Coordinator"));
    assert!(!html.contains("<script>"));
    let inline_path = html
        .split("src=\"")
        .find(|s| s.starts_with("/documentation/_inline/"))
        .unwrap()
        .split('"')
        .next()
        .unwrap();
    let response = app.clone().oneshot(get(inline_path)).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()["content-type"],
        "text/javascript; charset=utf-8"
    );
    assert!(
        !response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .is_empty()
    );
    let response = app
        .clone()
        .oneshot(get("/documentation/docs/operator-guide.html"))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let chapter = String::from_utf8(
        response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .to_vec(),
    )
    .unwrap();
    assert!(chapter.contains("Project settings"));
    for path in [
        "/documentation/missing.html",
        "/documentation/%2e%2e/Cargo.toml",
        "/documentation/%2fetc%2fpasswd",
        "/documentation/book.toml",
    ] {
        assert_eq!(
            app.clone().oneshot(get(path)).await.unwrap().status(),
            StatusCode::NOT_FOUND,
            "{path}"
        );
    }
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("HEAD")
                .uri("/documentation/")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .is_empty()
    );
    assert_eq!(
        app.clone()
            .oneshot(get("/api/v1/projects"))
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    let response = app.clone().oneshot(get("/")).await.unwrap();
    assert!(
        !response.headers()["content-security-policy"]
            .to_str()
            .unwrap()
            .contains("unsafe-inline")
    );
    let console = String::from_utf8(
        response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .to_vec(),
    )
    .unwrap();
    assert!(console.contains("href=\"/documentation/\""));
}
