//! Plugin static file serving
//!
//! Serves static files from plugin subdirectories under the configured plugin dir.
//! Each subdirectory is a plugin (SPA) with its own index.html and static assets.

use axum::{
    body::Body,
    extract::{Path, State},
    http::{header, StatusCode},
    response::{IntoResponse, Redirect, Response},
};
use serde_json::json;
use std::path::PathBuf;
use std::sync::Arc;

use crate::AppState;

/// Validate plugin name: only [a-zA-Z0-9_-], 1..=64 characters.
fn is_valid_plugin_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Resolve a plugin's base directory with security checks.
/// Returns None if plugin dir is not configured, name is invalid,
/// the directory doesn't exist, or it's a symlink.
fn resolve_plugin_dir(state: &AppState, name: &str) -> Option<PathBuf> {
    if !is_valid_plugin_name(name) {
        return None;
    }
    let base = state.config.plugin_dir()?;
    let plugin_path = base.join(name);

    // Canonicalize both and verify plugin stays inside base
    let canonical_base = base.canonicalize().ok()?;
    let canonical_plugin = plugin_path.canonicalize().ok()?;
    if !canonical_plugin.starts_with(&canonical_base) {
        return None;
    }

    // Reject symlinks at the plugin directory level
    let metadata = std::fs::symlink_metadata(&plugin_path).ok()?;
    if metadata.file_type().is_symlink() {
        return None;
    }

    canonical_plugin.is_dir().then_some(canonical_plugin)
}

/// GET /plugins — List installed plugins as JSON
pub async fn list_plugins(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let Some(plugin_base) = state.config.plugin_dir() else {
        return (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "application/json")],
            json!({ "plugins": [] }).to_string(),
        );
    };

    let mut plugins = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&plugin_base) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if !is_valid_plugin_name(&name) {
                continue;
            }
            // Skip symlinks
            if let Ok(meta) = std::fs::symlink_metadata(entry.path())
                && meta.file_type().is_symlink()
            {
                continue;
            }
            if let Ok(meta) = entry.metadata() {
                if !meta.is_dir() {
                    continue;
                }
            } else {
                continue;
            }
            plugins.push(json!({
                "name": name,
                "path": format!("/plugins/{}/", name),
            }));
        }
    }

    plugins.sort_by(|a, b| {
        a["name"]
            .as_str()
            .unwrap_or("")
            .cmp(b["name"].as_str().unwrap_or(""))
    });

    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json")],
        json!({ "plugins": plugins }).to_string(),
    )
}

/// GET /plugins/{name} — Redirect to trailing slash
pub async fn plugin_redirect(Path(name): Path<String>) -> impl IntoResponse {
    Redirect::permanent(&format!("/plugins/{name}/"))
}

/// GET /plugins/{name}/ — Serve plugin's index.html
pub async fn serve_plugin_index(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Response {
    let Some(plugin_dir) = resolve_plugin_dir(&state, &name) else {
        return (StatusCode::NOT_FOUND, "Plugin not found").into_response();
    };

    let index = plugin_dir.join("index.html");
    if index.is_file() {
        return serve_file(&index, true).await;
    }

    (StatusCode::NOT_FOUND, "index.html not found").into_response()
}

/// GET /plugins/{name}/*path — Serve static file or SPA fallback
pub async fn serve_plugin_file(
    State(state): State<Arc<AppState>>,
    Path((name, file_path)): Path<(String, String)>,
) -> Response {
    let Some(plugin_dir) = resolve_plugin_dir(&state, &name) else {
        return (StatusCode::NOT_FOUND, "Plugin not found").into_response();
    };

    // Reject obviously malicious paths
    if file_path.contains("..") || file_path.contains('\0') || file_path.contains('\\') {
        return (StatusCode::BAD_REQUEST, "Invalid path").into_response();
    }

    let target = plugin_dir.join(&file_path);

    // Canonicalize and verify the target stays inside the plugin dir
    if let Ok(canonical) = target.canonicalize() {
        if !canonical.starts_with(&plugin_dir) {
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

    // SPA fallback: file not found → serve plugin's root index.html
    let index = plugin_dir.join("index.html");
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
    fn test_valid_plugin_names() {
        assert!(is_valid_plugin_name("musubi"));
        assert!(is_valid_plugin_name("my-plugin"));
        assert!(is_valid_plugin_name("plugin123"));
        assert!(is_valid_plugin_name("SMART-App"));
        assert!(is_valid_plugin_name("my_plugin"));
        assert!(is_valid_plugin_name("a"));
    }

    #[test]
    fn test_invalid_plugin_names() {
        assert!(!is_valid_plugin_name(""));
        assert!(!is_valid_plugin_name(".."));
        assert!(!is_valid_plugin_name("../etc"));
        assert!(!is_valid_plugin_name("my.plugin"));
        assert!(!is_valid_plugin_name("my/plugin"));
        assert!(!is_valid_plugin_name("my plugin"));
        assert!(!is_valid_plugin_name(&"a".repeat(65)));
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
