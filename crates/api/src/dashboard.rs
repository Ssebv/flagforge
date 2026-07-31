//! Serving the compiled dashboard.
//!
//! The Trunk output is embedded in the binary rather than read from disk, so
//! the container still has exactly one artefact and no volume to mount. It
//! also means the API and the UI it serves can never be different versions.

use axum::http::{HeaderMap, HeaderValue, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use rust_embed::RustEmbed;

use crate::error::ApiError;

/// The `dist` directory produced by `trunk build --release`.
///
/// The directory is kept in the repository with only a `.gitkeep`, so a plain
/// `cargo build` works without the WASM toolchain — the binary simply reports
/// that the dashboard is not bundled rather than failing to compile.
#[derive(RustEmbed)]
#[folder = "../web/dist"]
struct Assets;

/// Whether a dashboard was bundled into this binary.
pub fn is_bundled() -> bool {
    Assets::get("index.html").is_some()
}

/// Serves a static asset, falling back to `index.html` for client-side routes.
///
/// `/projects/checkout` is a route the SPA knows and the server does not, so
/// anything unrecognised has to return the shell and let the app resolve it —
/// otherwise a reload on any deep link would 404.
pub async fn serve(uri: Uri, headers: HeaderMap) -> Response {
    // `/api/...` is the server's namespace. Falling back to the SPA there
    // would answer a mistyped endpoint with a page of HTML.
    if uri.path().starts_with("/api/") {
        return ApiError::NotFound("endpoint").into_response();
    }

    let path = uri.path().trim_start_matches('/');

    match Assets::get(path) {
        Some(file) => respond(path, &file.data, &file.metadata.sha256_hash(), &headers),
        None => match Assets::get("index.html") {
            Some(shell) => {
                respond("index.html", &shell.data, &shell.metadata.sha256_hash(), &headers)
            }
            None => not_bundled(),
        },
    }
}

fn respond(path: &str, body: &[u8], hash: &[u8; 32], headers: &HeaderMap) -> Response {
    let etag = format!("\"{}\"", hex(&hash[..8]));

    // A matching ETag makes a reload cost one 304 instead of re-downloading a
    // megabyte and a half of WebAssembly.
    if headers.get(header::IF_NONE_MATCH).and_then(|v| v.to_str().ok()) == Some(etag.as_str()) {
        return StatusCode::NOT_MODIFIED.into_response();
    }

    let mut response = Response::builder()
        .header(header::CONTENT_TYPE, content_type(path))
        .header(header::CACHE_CONTROL, cache_control(path));

    if let Ok(value) = HeaderValue::from_str(&etag) {
        response = response.header(header::ETAG, value);
    }

    response
        .body(body.to_vec().into())
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

/// Trunk fingerprints every asset it emits, so those can be cached forever;
/// `index.html` is the one file whose name is stable and therefore must not be.
fn cache_control(path: &str) -> &'static str {
    if path == "index.html" || path.is_empty() {
        "no-cache"
    } else {
        "public, max-age=31536000, immutable"
    }
}

/// Explicit rather than a MIME-guessing crate: the list is short, and getting
/// `application/wasm` wrong breaks streaming instantiation in a way that is
/// slow to diagnose.
fn content_type(path: &str) -> &'static str {
    match path.rsplit_once('.').map(|(_, ext)| ext) {
        Some("html") | None => "text/html; charset=utf-8",
        Some("js") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("wasm") => "application/wasm",
        Some("json") => "application/json",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("ico") => "image/x-icon",
        Some("woff2") => "font/woff2",
        Some(_) => "application/octet-stream",
    }
}

fn not_bundled() -> Response {
    (
        StatusCode::NOT_FOUND,
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        "The dashboard is not bundled into this binary.\n\
         Build it with `trunk build --release` in crates/web, then rebuild the server.\n\
         The API itself is unaffected and available under /api/v1.\n",
    )
        .into_response()
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes.iter().fold(String::with_capacity(bytes.len() * 2), |mut acc, byte| {
        let _ = write!(acc, "{byte:02x}");
        acc
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wasm_gets_the_type_that_allows_streaming_instantiation() {
        assert_eq!(content_type("app_bg.wasm"), "application/wasm");
    }

    #[test]
    fn fingerprinted_assets_are_immutable_and_the_shell_is_not() {
        assert_eq!(cache_control("index.html"), "no-cache");
        assert!(cache_control("app-af9bc2d.css").contains("immutable"));
    }

    #[test]
    fn unknown_extensions_fall_back_to_a_download_rather_than_html() {
        assert_eq!(content_type("weird.xyz"), "application/octet-stream");
        assert_eq!(content_type("no-extension"), "text/html; charset=utf-8");
    }

    #[tokio::test]
    async fn api_paths_never_fall_through_to_the_single_page_app() {
        let response = serve("/api/v1/does-not-exist".parse().unwrap(), HeaderMap::new()).await;

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_eq!(
            response.headers()[header::CONTENT_TYPE],
            "application/problem+json",
            "a mistyped endpoint must get a problem document, not a page of HTML"
        );
    }
}
