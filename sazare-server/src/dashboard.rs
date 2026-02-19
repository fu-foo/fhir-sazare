//! Web dashboard for server status monitoring
//!
//! GET /           — HTML dashboard (when Accept is not application/json)
//! GET /$status    — JSON API for dashboard polling

use crate::AppState;

use axum::{
    extract::State,
    http::{header, StatusCode},
    response::IntoResponse,
    Json,
};
use serde_json::json;
use std::sync::Arc;

/// GET / — serve the HTML dashboard page
pub async fn dashboard_page() -> impl IntoResponse {
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        DASHBOARD_HTML,
    )
}

/// GET /$status — JSON status for dashboard polling
pub async fn status_api(
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    // Resource counts
    let counts = state.store.count_by_type().unwrap_or_default();
    let total: i64 = counts.iter().map(|(_, c)| c).sum();
    let resource_counts: Vec<_> = counts
        .into_iter()
        .map(|(rt, count)| json!({"type": rt, "count": count}))
        .collect();

    // Recent audit log entries
    let audit = state.audit.lock().await;
    let recent = audit.recent_entries(20).unwrap_or_default();
    drop(audit);

    let log_entries: Vec<_> = recent
        .into_iter()
        .map(|(ts, op, rt, id, result)| {
            json!({
                "timestamp": ts,
                "operation": op,
                "resourceType": rt,
                "resourceId": id,
                "result": result
            })
        })
        .collect();

    Json(json!({
        "serverName": "sazare",
        "version": env!("CARGO_PKG_VERSION"),
        "fhirVersion": "4.0.1",
        "totalResources": total,
        "resourceCounts": resource_counts,
        "recentActivity": log_entries
    }))
}

const DASHBOARD_HTML: &str = r##"<!DOCTYPE html>
<html lang="ja">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<link rel="icon" href="data:,">
<title>sazare FHIR Server</title>
<style>
  * { margin: 0; padding: 0; box-sizing: border-box; }
  body { font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif;
         background: #f5f5f5; color: #333; line-height: 1.6; }
  .header { background: #2c3e50; color: #fff; padding: 20px 32px; }
  .header h1 { font-size: 24px; font-weight: 600; }
  .header .sub { color: #95a5a6; font-size: 14px; margin-top: 4px; }
  .container { max-width: 960px; margin: 24px auto; padding: 0 16px; }
  .card { background: #fff; border-radius: 8px; padding: 20px 24px; margin-bottom: 16px;
          box-shadow: 0 1px 3px rgba(0,0,0,0.1); }
  .card h2 { font-size: 16px; color: #7f8c8d; margin-bottom: 12px; text-transform: uppercase;
             letter-spacing: 0.5px; }
  .status-row { display: flex; align-items: center; gap: 12px; margin-bottom: 8px; }
  .dot { width: 10px; height: 10px; border-radius: 50%; background: #27ae60; }
  .stats { display: grid; grid-template-columns: repeat(auto-fill, minmax(140px, 1fr)); gap: 12px; }
  .stat { background: #f8f9fa; border-radius: 6px; padding: 12px 16px; text-align: center; }
  .stat .num { font-size: 28px; font-weight: 700; color: #2c3e50; }
  .stat .label { font-size: 12px; color: #95a5a6; margin-top: 2px; }
  .log-table { width: 100%; border-collapse: collapse; font-size: 13px; }
  .log-table th { text-align: left; padding: 8px 12px; border-bottom: 2px solid #eee;
                  color: #95a5a6; font-weight: 600; }
  .log-table td { padding: 6px 12px; border-bottom: 1px solid #f0f0f0; }
  .log-table tr:hover { background: #f8f9fa; }
  .badge { display: inline-block; padding: 2px 8px; border-radius: 4px; font-size: 11px;
           font-weight: 600; }
  .badge.success { background: #d4edda; color: #155724; }
  .badge.error { background: #f8d7da; color: #721c24; }
  .endpoints { font-size: 13px; }
  .endpoints code { background: #f0f0f0; padding: 2px 6px; border-radius: 3px; font-size: 12px; }
  .endpoints li { margin-bottom: 6px; list-style: none; }
  .refresh-note { text-align: center; color: #bbb; font-size: 12px; margin-top: 16px; }
</style>
</head>
<body>

<div class="header">
  <h1>sazare</h1>
  <div class="sub">Lightweight FHIR R4 Server</div>
</div>

<div class="container">
  <div class="card">
    <h2>Server Status</h2>
    <div class="status-row">
      <div class="dot"></div>
      <span>Running</span>
      <span style="color:#95a5a6; margin-left: auto;" id="version"></span>
    </div>
  </div>

  <div class="card">
    <h2>Resources</h2>
    <div class="stats" id="stats">
      <div class="stat">
        <div class="num" id="total">-</div>
        <div class="label">Total</div>
      </div>
    </div>
  </div>

  <div class="card">
    <h2>Recent Activity</h2>
    <table class="log-table">
      <thead>
        <tr><th>Time</th><th>Operation</th><th>Resource</th><th>Result</th></tr>
      </thead>
      <tbody id="logs">
        <tr><td colspan="4" style="color:#bbb">Loading...</td></tr>
      </tbody>
    </table>
  </div>

  <div class="card">
    <h2>API Endpoints</h2>
    <ul class="endpoints">
      <li><code>GET /metadata</code> CapabilityStatement</li>
      <li><code>GET /{type}?params</code> Search</li>
      <li><code>POST /{type}</code> Create</li>
      <li><code>GET /{type}/{id}</code> Read</li>
      <li><code>PUT /{type}/{id}</code> Update</li>
      <li><code>DELETE /{type}/{id}</code> Delete</li>
      <li><code>GET /{type}/{id}/_history</code> History</li>
      <li><code>POST /{type}/$validate</code> Validate</li>
      <li><code>POST /</code> Bundle (transaction / batch)</li>
      <li><code>GET /$export</code> Export (NDJSON)</li>
      <li><code>POST /$import</code> Import (NDJSON)</li>
    </ul>
  </div>

  <div class="refresh-note" id="refresh-note">Auto-refreshes every 5 seconds</div>
</div>

<script>
async function refresh() {
  const noteEl = document.getElementById('refresh-note');
  try {
    const res = await fetch('/$status?_=' + Date.now(), { cache: 'no-store' });
    if (!res.ok) {
      noteEl.textContent = 'Fetch error: HTTP ' + res.status + ' (' + new Date().toLocaleTimeString() + ')';
      return;
    }
    const data = await res.json();

    document.getElementById('version').textContent =
      'v' + data.version + ' / FHIR ' + data.fhirVersion;

    // Resource type stats
    const statsEl = document.getElementById('stats');
    let statsHtml = '<div class="stat"><div class="num">' + data.totalResources +
                    '</div><div class="label">Total</div></div>';
    for (const rc of data.resourceCounts) {
      statsHtml += '<div class="stat"><div class="num">' + rc.count +
                   '</div><div class="label">' + rc.type + '</div></div>';
    }
    statsEl.innerHTML = statsHtml;

    // Activity log
    const logsEl = document.getElementById('logs');
    if (data.recentActivity.length === 0) {
      logsEl.innerHTML = '<tr><td colspan="4" style="color:#bbb">No activity yet</td></tr>';
    } else {
      logsEl.innerHTML = data.recentActivity.map(e => {
        const badge = e.result === 'success'
          ? '<span class="badge success">OK</span>'
          : '<span class="badge error">ERR</span>';
        const res = e.resourceType ? (e.resourceType + (e.resourceId ? '/' + e.resourceId : '')) : '';
        const local = new Date(e.timestamp + 'Z').toLocaleString();
        return '<tr><td>' + local + '</td><td>' + e.operation +
               '</td><td>' + res + '</td><td>' + badge + '</td></tr>';
      }).join('');
    }

    noteEl.textContent = 'Last updated: ' + new Date().toLocaleTimeString();
  } catch (err) {
    noteEl.textContent = 'Fetch failed: ' + err.message + ' (' + new Date().toLocaleTimeString() + ')';
  }
}

refresh();
setInterval(refresh, 5000);
</script>
</body>
</html>
"##;
