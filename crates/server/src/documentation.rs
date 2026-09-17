use axum::{
    body::Body,
    extract::Path,
    http::{HeaderValue, header},
    response::{IntoResponse, Redirect, Response},
};

mod generated {
    include!(concat!(env!("OUT_DIR"), "/documentation.rs"));
}

pub(crate) const CSP: &str = "default-src 'none'; script-src 'self'; style-src 'self' 'unsafe-inline'; font-src 'self'; connect-src 'self'; img-src 'self' data:; base-uri 'none'; frame-ancestors 'none'; form-action 'self'; object-src 'none'";

pub(crate) fn is_path(path: &str) -> bool {
    path == "/documentation" || path.starts_with("/documentation/")
}

pub(crate) async fn redirect() -> Redirect {
    Redirect::permanent("/documentation/")
}
pub(crate) async fn index() -> Response {
    serve("index.html")
}
pub(crate) async fn file(Path(path): Path<String>) -> Response {
    serve(&path)
}

fn serve(path: &str) -> Response {
    match generated::embedded(path) {
        Some((content_type, bytes)) => {
            let mut response = Response::new(Body::from(bytes));
            response
                .headers_mut()
                .insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
            response
        }
        None => crate::error::AppError::not_found().into_response(),
    }
}
