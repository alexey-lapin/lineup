//! Serves the built web UI (`web/dist`), embedded at compile time in release builds.

use axum::http::{HeaderValue, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use rust_embed::Embed;

#[derive(Embed)]
#[folder = "../../web/dist"]
struct Assets;

pub async fn static_handler(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    if ["api", "p"]
        .iter()
        .any(|r| path == *r || path.starts_with(&format!("{r}/")))
    {
        return (StatusCode::NOT_FOUND, "not found\n").into_response();
    }
    if !path.is_empty()
        && let Some(res) = asset(path)
    {
        return res;
    }
    // Paths with a file extension that do not exist are real 404s; everything else is an SPA route.
    let last = path.rsplit('/').next().unwrap_or("");
    if last.contains('.') {
        return (StatusCode::NOT_FOUND, "not found\n").into_response();
    }
    asset("index.html").unwrap_or_else(|| (StatusCode::NOT_FOUND, "UI not built\n").into_response())
}

fn asset(path: &str) -> Option<Response> {
    let file = Assets::get(path)?;
    let mime = file.metadata.mimetype();
    let cache = if path.starts_with("assets/") {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    };
    let mut res = file.data.into_owned().into_response();
    let h = res.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(mime).unwrap_or(HeaderValue::from_static("application/octet-stream")),
    );
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static(cache));
    Some(res)
}
