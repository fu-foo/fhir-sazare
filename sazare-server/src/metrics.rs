//! `GET /metrics` — Prometheus text exposition, without extra dependencies.
//!
//! Request counters and a latency histogram are recorded by [`track`] (a
//! middleware keyed by the matched route template, so ids don't explode the
//! label set); resource counts and database sizes are read at scrape time.

use axum::{
    extract::{MatchedPath, Request, State},
    http::{header, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

use crate::AppState;

/// Latency histogram bucket upper bounds, in seconds.
const BUCKETS: [f64; 11] = [0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0];

#[derive(Default)]
struct Histogram {
    counts: [u64; BUCKETS.len()],
    sum: f64,
    count: u64,
}

#[derive(Default)]
struct Registry {
    /// (method, route, status) → count
    requests: BTreeMap<(String, String, u16), u64>,
    /// (method, route) → latency histogram
    latency: BTreeMap<(String, String), Histogram>,
}

fn registry() -> &'static Mutex<Registry> {
    static R: OnceLock<Mutex<Registry>> = OnceLock::new();
    R.get_or_init(|| Mutex::new(Registry::default()))
}

/// Record the server start time (for `sazare_uptime_seconds`).
pub fn mark_start() {
    started();
}

fn started() -> &'static Instant {
    static T: OnceLock<Instant> = OnceLock::new();
    T.get_or_init(Instant::now)
}

/// Middleware: count the request and time it, labelled by the route template
/// (`/{resource_type}/{id}`), not the concrete path.
pub async fn track(request: Request, next: Next) -> Response {
    let method = request.method().as_str().to_string();
    let route = request
        .extensions()
        .get::<MatchedPath>()
        .map(|p| p.as_str().to_string())
        .unwrap_or_else(|| "unmatched".to_string());
    let t = Instant::now();
    let response = next.run(request).await;
    let secs = t.elapsed().as_secs_f64();
    let status = response.status().as_u16();

    let mut r = registry().lock().unwrap_or_else(|e| e.into_inner());
    *r.requests.entry((method.clone(), route.clone(), status)).or_default() += 1;
    let h = r.latency.entry((method, route)).or_default();
    for (i, bound) in BUCKETS.iter().enumerate() {
        if secs <= *bound {
            h.counts[i] += 1;
        }
    }
    h.sum += secs;
    h.count += 1;
    response
}

/// Escape a label value for the text format.
fn esc(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', "\\n")
}

/// `GET /metrics`
pub async fn metrics(State(state): State<Arc<AppState>>) -> Response {
    let mut out = String::new();

    let _ = writeln!(out, "# HELP sazare_build_info Build information.");
    let _ = writeln!(out, "# TYPE sazare_build_info gauge");
    let _ = writeln!(out, "sazare_build_info{{version=\"{}\"}} 1", env!("CARGO_PKG_VERSION"));

    let _ = writeln!(out, "# HELP sazare_uptime_seconds Seconds since the server started.");
    let _ = writeln!(out, "# TYPE sazare_uptime_seconds gauge");
    let _ = writeln!(out, "sazare_uptime_seconds {:.0}", started().elapsed().as_secs_f64());

    {
        let r = registry().lock().unwrap_or_else(|e| e.into_inner());
        let _ = writeln!(out, "# HELP sazare_http_requests_total HTTP requests by method, route template and status.");
        let _ = writeln!(out, "# TYPE sazare_http_requests_total counter");
        for ((m, route, status), n) in &r.requests {
            let _ = writeln!(
                out,
                "sazare_http_requests_total{{method=\"{}\",route=\"{}\",status=\"{}\"}} {}",
                esc(m), esc(route), status, n
            );
        }
        let _ = writeln!(out, "# HELP sazare_http_request_duration_seconds HTTP request latency.");
        let _ = writeln!(out, "# TYPE sazare_http_request_duration_seconds histogram");
        for ((m, route), h) in &r.latency {
            let labels = format!("method=\"{}\",route=\"{}\"", esc(m), esc(route));
            for (i, bound) in BUCKETS.iter().enumerate() {
                let _ = writeln!(
                    out,
                    "sazare_http_request_duration_seconds_bucket{{{labels},le=\"{bound}\"}} {}",
                    h.counts[i]
                );
            }
            let _ = writeln!(out, "sazare_http_request_duration_seconds_bucket{{{labels},le=\"+Inf\"}} {}", h.count);
            let _ = writeln!(out, "sazare_http_request_duration_seconds_sum{{{labels}}} {}", h.sum);
            let _ = writeln!(out, "sazare_http_request_duration_seconds_count{{{labels}}} {}", h.count);
        }
    }

    let _ = writeln!(out, "# HELP sazare_resources Stored resources (current versions) by type.");
    let _ = writeln!(out, "# TYPE sazare_resources gauge");
    if let Ok(counts) = state.store.count_by_type() {
        for (rt, n) in counts {
            let _ = writeln!(out, "sazare_resources{{type=\"{}\"}} {}", esc(&rt), n);
        }
    }

    let _ = writeln!(out, "# HELP sazare_database_size_bytes On-disk size of each database file (with its WAL).");
    let _ = writeln!(out, "# TYPE sazare_database_size_bytes gauge");
    let index = state.config.search_index_db_path();
    for (name, path) in [
        ("resources", state.config.resources_db_path()),
        ("search_index", index.clone()),
        ("full_text", index.with_extension("fts.sqlite")),
        ("audit", state.config.audit_db_path()),
    ] {
        let size = |p: &std::path::Path| std::fs::metadata(p).map(|m| m.len()).unwrap_or(0);
        let wal = std::path::PathBuf::from(format!("{}-wal", path.display()));
        let _ = writeln!(out, "sazare_database_size_bytes{{db=\"{name}\"}} {}", size(&path) + size(&wal));
    }

    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/plain; version=0.0.4; charset=utf-8")],
        out,
    )
        .into_response()
}
