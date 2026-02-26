//! Web dashboard for server status monitoring
//!
//! GET /           — HTML dashboard (when Accept is not application/json)
//! GET /$status    — JSON API for dashboard polling

use crate::AppState;

use axum::{
    extract::{Path, Query, State},
    http::{header, StatusCode},
    response::IntoResponse,
    Json,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;

#[derive(Deserialize, Default)]
pub struct BrowseParams {
    #[serde(default)]
    pub _count: Option<usize>,
    #[serde(default)]
    pub _offset: Option<usize>,
}

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

/// GET /$browse/{resource_type} — list resources for dashboard
pub async fn browse_list(
    State(state): State<Arc<AppState>>,
    Path(resource_type): Path<String>,
    Query(params): Query<BrowseParams>,
) -> impl IntoResponse {
    let count = params._count.unwrap_or(20);
    let offset = params._offset.unwrap_or(0);

    let (raw_entries, total) = match state.store.list_by_last_updated(&resource_type, count, offset) {
        Ok(r) => r,
        Err(e) => return Json(json!({"error": e.to_string()})).into_response(),
    };

    let entries: Vec<Value> = raw_entries
        .into_iter()
        .filter_map(|(_id, data)| serde_json::from_slice::<Value>(&data).ok())
        .map(|r| {
            json!({
                "id": r.get("id").and_then(|v| v.as_str()).unwrap_or(""),
                "lastUpdated": r.get("meta").and_then(|m| m.get("lastUpdated")).and_then(|v| v.as_str()).unwrap_or(""),
                "resource": r
            })
        })
        .collect();

    Json(json!({
        "total": total,
        "offset": offset,
        "count": count,
        "entries": entries
    })).into_response()
}

/// GET /$browse/{resource_type}/{id} — single resource for dashboard
pub async fn browse_read(
    State(state): State<Arc<AppState>>,
    Path((resource_type, id)): Path<(String, String)>,
) -> impl IntoResponse {
    match state.store.get(&resource_type, &id) {
        Ok(Some(data)) => {
            match serde_json::from_slice::<Value>(&data) {
                Ok(resource) => Json(resource).into_response(),
                Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))).into_response(),
            }
        }
        Ok(None) => (StatusCode::NOT_FOUND, Json(json!({"error": "Not found"}))).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))).into_response(),
    }
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
  .stat.clickable { cursor: pointer; transition: background 0.15s, transform 0.15s; }
  .stat.clickable:hover { background: #e8f4fd; transform: translateY(-1px); }
  .stat .num { font-size: 28px; font-weight: 700; color: #2c3e50; }
  .stat .label { font-size: 12px; color: #95a5a6; margin-top: 2px; }
  .hidden { display: none; }
  .back-btn { background: none; border: 1px solid #ddd; border-radius: 4px; padding: 4px 12px;
              cursor: pointer; font-size: 13px; color: #555; margin-right: 12px; }
  .back-btn:hover { background: #f0f0f0; }
  .panel-header { display: flex; align-items: center; margin-bottom: 12px; }
  .panel-header h2 { margin-bottom: 0; }
  .resource-table { width: 100%; border-collapse: collapse; font-size: 13px; }
  .resource-table th { text-align: left; padding: 8px 12px; border-bottom: 2px solid #eee;
                       color: #95a5a6; font-weight: 600; }
  .resource-table td { padding: 6px 12px; border-bottom: 1px solid #f0f0f0; }
  .resource-table tr.clickable-row { cursor: pointer; }
  .resource-table tr.clickable-row:hover { background: #f0f7ff; }
  .json-view { background: #1e1e1e; color: #d4d4d4; padding: 16px; border-radius: 6px;
               font-family: "SF Mono", Menlo, Consolas, monospace; font-size: 13px;
               overflow-x: auto; max-height: 600px; overflow-y: auto; white-space: pre; line-height: 1.5; }
  .json-view .jk { color: #9cdcfe; }
  .json-view .js { color: #ce9178; }
  .json-view .jn { color: #b5cea8; }
  .json-view .jb { color: #569cd6; }
  .json-view .jl { color: #569cd6; }
  .json-view .jp { color: #d4d4d4; }
  .pagination { display: flex; justify-content: space-between; align-items: center;
                margin-top: 12px; font-size: 13px; color: #95a5a6; }
  .pagination button { background: #fff; border: 1px solid #ddd; border-radius: 4px;
                       padding: 4px 14px; cursor: pointer; font-size: 13px; }
  .pagination button:hover { background: #f0f0f0; }
  .pagination button:disabled { opacity: 0.4; cursor: default; }
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

  <div class="card hidden" id="resource-list">
    <div class="panel-header">
      <button class="back-btn" onclick="hideResourceList()">&larr; Back</button>
      <h2 id="resource-list-title">Resources</h2>
    </div>
    <table class="resource-table">
      <thead>
        <tr><th>ID</th><th>Last Updated</th><th>Summary</th></tr>
      </thead>
      <tbody id="resource-list-body">
      </tbody>
    </table>
    <div class="pagination" id="resource-list-pagination"></div>
  </div>

  <div class="card hidden" id="resource-detail">
    <div class="panel-header">
      <button class="back-btn" onclick="hideResourceDetail()">&larr; Back</button>
      <h2 id="resource-detail-title">Resource</h2>
    </div>
    <pre class="json-view" id="resource-detail-json"></pre>
  </div>

  <div class="card" id="activity-card">
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
      statsHtml += '<div class="stat clickable" onclick="showResourceList(\'' + rc.type + '\')">' +
                   '<div class="num">' + rc.count + '</div><div class="label">' + rc.type + '</div></div>';
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

let currentType = '';
let currentOffset = 0;
const PAGE_SIZE = 20;

async function showResourceList(type, offset) {
  currentType = type;
  currentOffset = offset || 0;
  document.getElementById('resource-list-title').textContent = type;
  document.getElementById('resource-list').classList.remove('hidden');
  document.getElementById('resource-detail').classList.add('hidden');

  const body = document.getElementById('resource-list-body');
  body.innerHTML = '<tr><td colspan="3" style="color:#bbb">Loading...</td></tr>';

  try {
    const res = await fetch('/$browse/' + type + '?_count=' + PAGE_SIZE + '&_offset=' + currentOffset);
    const data = await res.json();
    const entries = data.entries || [];
    const total = data.total || 0;

    if (entries.length === 0) {
      body.innerHTML = '<tr><td colspan="3" style="color:#bbb">No resources</td></tr>';
    } else {
      body.innerHTML = entries.map(e => {
        const id = e.id || '-';
        const updated = e.lastUpdated ? new Date(e.lastUpdated).toLocaleString() : '-';
        const summary = getSummary(e.resource);
        return '<tr class="clickable-row" onclick="showResource(\'' + type + '\',\'' + id + '\')">' +
               '<td><code>' + id + '</code></td><td>' + updated + '</td><td>' + summary + '</td></tr>';
      }).join('');
    }

    // Pagination
    const pag = document.getElementById('resource-list-pagination');
    const showing = Math.min(currentOffset + PAGE_SIZE, total);
    pag.innerHTML =
      '<button ' + (currentOffset === 0 ? 'disabled' : 'onclick="showResourceList(\'' + type + '\',' + (currentOffset - PAGE_SIZE) + ')"') + '>&larr; Prev</button>' +
      '<span>' + (currentOffset + 1) + '–' + showing + ' of ' + total + '</span>' +
      '<button ' + (currentOffset + PAGE_SIZE >= total ? 'disabled' : 'onclick="showResourceList(\'' + type + '\',' + (currentOffset + PAGE_SIZE) + ')"') + '>Next &rarr;</button>';
  } catch (err) {
    body.innerHTML = '<tr><td colspan="3" style="color:#c00">Error: ' + err.message + '</td></tr>';
  }
}

function getSummary(r) {
  if (r.name && r.name[0]) {
    const n = r.name[0];
    return [].concat(n.given || []).join(' ') + ' ' + (n.family || '');
  }
  if (r.code && r.code.text) return r.code.text;
  if (r.code && r.code.coding && r.code.coding[0]) return r.code.coding[0].display || r.code.coding[0].code || '';
  if (r.status) return 'status: ' + r.status;
  return '';
}

function highlightJson(obj, indent) {
  indent = indent || 0;
  var pad = '  '.repeat(indent);
  var pad1 = '  '.repeat(indent + 1);
  if (obj === null) return '<span class="jl">null</span>';
  if (typeof obj === 'boolean') return '<span class="jb">' + obj + '</span>';
  if (typeof obj === 'number') return '<span class="jn">' + obj + '</span>';
  if (typeof obj === 'string') {
    var escaped = obj.replace(/&/g,'&amp;').replace(/</g,'&lt;').replace(/>/g,'&gt;').replace(/"/g,'&quot;');
    return '<span class="js">"' + escaped + '"</span>';
  }
  if (Array.isArray(obj)) {
    if (obj.length === 0) return '<span class="jp">[]</span>';
    var items = obj.map(function(v) { return pad1 + highlightJson(v, indent + 1); });
    return '<span class="jp">[</span>\n' + items.join('<span class="jp">,</span>\n') + '\n' + pad + '<span class="jp">]</span>';
  }
  var keys = Object.keys(obj);
  if (keys.length === 0) return '<span class="jp">{}</span>';
  var entries = keys.map(function(k) {
    var ek = k.replace(/&/g,'&amp;').replace(/</g,'&lt;').replace(/>/g,'&gt;').replace(/"/g,'&quot;');
    return pad1 + '<span class="jk">"' + ek + '"</span><span class="jp">: </span>' + highlightJson(obj[k], indent + 1);
  });
  return '<span class="jp">{</span>\n' + entries.join('<span class="jp">,</span>\n') + '\n' + pad + '<span class="jp">}</span>';
}

async function showResource(type, id) {
  document.getElementById('resource-detail-title').textContent = type + '/' + id;
  document.getElementById('resource-detail').classList.remove('hidden');
  document.getElementById('resource-list').classList.add('hidden');
  const pre = document.getElementById('resource-detail-json');
  pre.textContent = 'Loading...';

  try {
    const res = await fetch('/$browse/' + type + '/' + id);
    const data = await res.json();
    pre.innerHTML = highlightJson(data, 0);
  } catch (err) {
    pre.textContent = 'Error: ' + err.message;
  }
}

function hideResourceList() {
  document.getElementById('resource-list').classList.add('hidden');
}

function hideResourceDetail() {
  document.getElementById('resource-detail').classList.add('hidden');
  document.getElementById('resource-list').classList.remove('hidden');
}
</script>
</body>
</html>
"##;
