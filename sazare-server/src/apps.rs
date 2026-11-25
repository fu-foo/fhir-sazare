//! Static app hosting: `/apps/<name>/`.
//!
//! Each subdirectory of the configured apps directory (`apps.dir`) is a static
//! app — typically a single-page app that talks to this server's FHIR API on
//! the same origin, so it needs no CORS setup. Apps are served under `/apps/`
//! only, never at the top level, so they can't collide with FHIR or server
//! routes. Nothing is served unless an apps directory is configured.
//!
//! This hosts files; it does not extend the server. The files are public (no
//! authentication), the data the app fetches goes through the FHIR API and its
//! normal authorization.

use axum::{
    body::Body,
    extract::{Path, State},
    http::{header, StatusCode},
    response::{IntoResponse, Redirect, Response},
    routing::get,
    Router,
};
use serde_json::json;
use std::path::PathBuf;
use std::sync::Arc;

use crate::AppState;

/// URL prefix all apps are served under.
pub const PREFIX: &str = "/apps";

/// App names: `[A-Za-z0-9_-]`, 1..=64 characters (they are URL segments).
fn is_valid_app_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Discover the apps: each (non-symlink) subdirectory with a valid name.
pub fn discover_app_names(config: &crate::config::ServerConfig) -> Vec<String> {
    let Some(base) = config.apps_dir() else {
        return Vec::new();
    };
    let mut names = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&base) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if !is_valid_app_name(&name) {
                continue;
            }
            match std::fs::symlink_metadata(entry.path()) {
                Ok(meta) if meta.is_dir() => names.push(name),
                _ => continue,
            }
        }
    }
    names.sort();
    names
}

/// Whether `path` is an app path that is served without authentication:
/// `/apps/<discovered name>` and everything below it.
pub fn is_app_path(state: &AppState, path: &str) -> bool {
    let Some(rest) = path.strip_prefix(PREFIX).and_then(|r| r.strip_prefix('/')) else {
        return false;
    };
    let name = rest.split('/').next().unwrap_or("");
    state.app_names.iter().any(|n| n == name)
}

/// Resolve a discovered app's directory (canonical, inside the apps dir).
fn resolve_app_dir(state: &AppState, name: &str) -> Option<PathBuf> {
    if !state.app_names.iter().any(|n| n == name) {
        return None;
    }
    let base = state.config.apps_dir()?;
    let app_path = base.join(name);
    let canonical_base = base.canonicalize().ok()?;
    let canonical_app = app_path.canonicalize().ok()?;
    if !canonical_app.starts_with(&canonical_base) {
        return None;
    }
    let metadata = std::fs::symlink_metadata(&app_path).ok()?;
    if metadata.file_type().is_symlink() {
        return None;
    }
    canonical_app.is_dir().then_some(canonical_app)
}

/// Routes under [`PREFIX`]:
///   GET /apps/{name}         → redirect to /apps/{name}/
///   GET /apps/{name}/        → index.html
///   GET /apps/{name}/{*path} → static file (SPA fallback to index.html)
pub fn app_routes() -> Router<Arc<AppState>> {
    Router::new()
        .route(
            "/apps/{name}",
            get(|Path(name): Path<String>| async move {
                if is_valid_app_name(&name) {
                    Redirect::permanent(&format!("{PREFIX}/{name}/")).into_response()
                } else {
                    StatusCode::NOT_FOUND.into_response()
                }
            }),
        )
        .route(
            "/apps/{name}/",
            get(|state: State<Arc<AppState>>, Path(name): Path<String>| async move {
                serve_app_file(state, name, String::new()).await
            }),
        )
        .route(
            "/apps/{name}/{*path}",
            get(|state: State<Arc<AppState>>, Path((name, path)): Path<(String, String)>| async move {
                serve_app_file(state, name, path).await
            }),
        )
}

/// GET /$apps — the installed apps as JSON.
pub async fn list_apps(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let apps: Vec<_> = state
        .app_names
        .iter()
        .map(|name| json!({"name": name, "path": format!("{PREFIX}/{name}/")}))
        .collect();
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json")],
        json!({ "apps": apps }).to_string(),
    )
}

/// Serve `file_path` of app `name` (empty → index.html), with SPA fallback.
async fn serve_app_file(State(state): State<Arc<AppState>>, name: String, file_path: String) -> Response {
    let Some(app_dir) = resolve_app_dir(&state, &name) else {
        return (StatusCode::NOT_FOUND, "App not found").into_response();
    };
    if file_path.is_empty() {
        let index = app_dir.join("index.html");
        if index.is_file() {
            return serve_file(&index, true).await;
        }
        return (StatusCode::NOT_FOUND, "index.html not found").into_response();
    }

    // Reject obviously malicious paths
    if file_path.contains("..") || file_path.contains('\0') || file_path.contains('\\') {
        return (StatusCode::BAD_REQUEST, "Invalid path").into_response();
    }

    let target = app_dir.join(&file_path);

    // Canonicalize and verify the target stays inside the app dir
    if let Ok(canonical) = target.canonicalize() {
        if !canonical.starts_with(&app_dir) {
            return (StatusCode::BAD_REQUEST, "Invalid path").into_response();
        }

        // Reject symlinks
        if let Ok(meta) = std::fs::symlink_metadata(&canonical)
            && meta.file_type().is_symlink()
        {
            return (StatusCode::FORBIDDEN, "Forbidden").into_response();
        }

        if canonical.is_file() {
            let is_index = canonical
                .file_name()
                .map(|n| n == "index.html")
                .unwrap_or(false);
            return serve_file(&canonical, is_index).await;
        }

        // If it's a directory, try index.html inside it
        if canonical.is_dir() {
            let index = canonical.join("index.html");
            if index.is_file() {
                return serve_file(&index, true).await;
            }
        }
    }

    // SPA fallback: file not found → serve the app's root index.html
    let index = app_dir.join("index.html");
    if index.is_file() {
        return serve_file(&index, true).await;
    }

    (StatusCode::NOT_FOUND, "Not found").into_response()
}

/// Serve a single file with appropriate MIME type and Cache-Control.
async fn serve_file(path: &std::path::Path, is_index: bool) -> Response {
    let content = match tokio::fs::read(path).await {
        Ok(c) => c,
        Err(_) => return (StatusCode::INTERNAL_SERVER_ERROR, "Read error").into_response(),
    };

    let mime = mime_from_extension(path);
    let cache = if is_index {
        "no-cache"
    } else {
        "public, max-age=604800"
    };

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, mime)
        .header(header::CACHE_CONTROL, cache)
        .body(Body::from(content))
        .unwrap_or_else(|_| {
            (StatusCode::INTERNAL_SERVER_ERROR, "Response build error").into_response()
        })
}

/// Determine MIME type from file extension.
fn mime_from_extension(path: &std::path::Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()) {
        Some("html" | "htm") => "text/html; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("js" | "mjs") => "application/javascript; charset=utf-8",
        Some("json") => "application/json; charset=utf-8",
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("svg") => "image/svg+xml",
        Some("ico") => "image/x-icon",
        Some("webp") => "image/webp",
        Some("woff") => "font/woff",
        Some("woff2") => "font/woff2",
        Some("ttf") => "font/ttf",
        Some("otf") => "font/otf",
        Some("wasm") => "application/wasm",
        Some("map") => "application/json",
        Some("txt") => "text/plain; charset=utf-8",
        Some("xml") => "application/xml; charset=utf-8",
        Some("pdf") => "application/pdf",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_app_names() {
        for ok in ["musubi", "my-app", "app123", "SMART-App", "my_app", "a", "Patient", "metadata"] {
            assert!(is_valid_app_name(ok), "{ok}");
        }
        for bad in ["", "..", "../etc", "my.app", "my/app", "my app"] {
            assert!(!is_valid_app_name(bad), "{bad}");
        }
        assert!(!is_valid_app_name(&"a".repeat(65)));
    }

    #[test]
    fn test_mime_types() {
        assert_eq!(
            mime_from_extension(std::path::Path::new("app.js")),
            "application/javascript; charset=utf-8"
        );
        assert_eq!(
            mime_from_extension(std::path::Path::new("style.css")),
            "text/css; charset=utf-8"
        );
        assert_eq!(
            mime_from_extension(std::path::Path::new("index.html")),
            "text/html; charset=utf-8"
        );
        assert_eq!(
            mime_from_extension(std::path::Path::new("data.json")),
            "application/json; charset=utf-8"
        );
        assert_eq!(
            mime_from_extension(std::path::Path::new("image.png")),
            "image/png"
        );
        assert_eq!(
            mime_from_extension(std::path::Path::new("font.woff2")),
            "font/woff2"
        );
        assert_eq!(
            mime_from_extension(std::path::Path::new("app.wasm")),
            "application/wasm"
        );
        assert_eq!(
            mime_from_extension(std::path::Path::new("unknown")),
            "application/octet-stream"
        );
    }
}
