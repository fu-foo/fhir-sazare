//! Web dashboard for server status monitoring
//!
//! GET /           — HTML dashboard (when Accept is not application/json)
//! GET /$status    — JSON API for dashboard polling
//!
//! UI text is localized client-side via a small message catalog (default
//! English, auto-detecting Japanese from the browser, switchable). Server API
//! responses (OperationOutcome, etc.) remain English-only.

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
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<link rel="icon" href="data:,">
<title>sazare FHIR Server</title>
<style>
  * { margin: 0; padding: 0; box-sizing: border-box; }
  body { font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif;
         background: #f5f5f5; color: #333; line-height: 1.6; }
  .header { background: #2c3e50; color: #fff; padding: 20px 32px; display: flex; align-items: center; }
  .header h1 { font-size: 24px; font-weight: 600; }
  .header .sub { color: #95a5a6; font-size: 14px; margin-top: 4px; }
  .lang-toggle { margin-left: auto; background: rgba(255,255,255,0.12); color: #fff; border: 1px solid rgba(255,255,255,0.25);
                 border-radius: 4px; padding: 4px 12px; cursor: pointer; font-size: 13px; }
  .lang-toggle:hover { background: rgba(255,255,255,0.22); }
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
  .primary-btn { background: #2980b9; color: #fff; border: none; border-radius: 6px;
                 padding: 10px 20px; font-size: 14px; cursor: pointer; transition: background 0.15s; }
  .primary-btn:hover { background: #2471a3; }
  .primary-btn:disabled { opacity: 0.5; cursor: default; }
  .search-row { display: flex; gap: 8px; flex-wrap: wrap; align-items: center; }
  .search-row input { border: 1px solid #ddd; border-radius: 4px; padding: 8px 10px; font-size: 14px; }
  .search-row input#search-type { width: 180px; }
  .search-row input#search-query { flex: 1; min-width: 200px; }
  .search-btn { background: #2980b9; color: #fff; border: none; border-radius: 4px;
                padding: 8px 18px; font-size: 14px; cursor: pointer; }
  .search-btn:hover { background: #2471a3; }
  .search-hint { color: #95a5a6; font-size: 12px; margin-top: 8px; }
  .search-url { margin-top: 10px; font-size: 12px; color: #555; }
  .search-url code { background: #f0f0f0; padding: 3px 8px; border-radius: 3px;
                     font-family: "SF Mono", Menlo, Consolas, monospace; word-break: break-all; }
  .chip { display: inline-block; background: #eef3f8; color: #2980b9; border-radius: 12px;
          padding: 2px 10px; font-size: 12px; margin: 2px 4px 2px 0; cursor: pointer; }
  .chip:hover { background: #dceaf6; }
</style>
</head>
<body>

<div class="header">
  <div>
    <h1>sazare</h1>
    <div class="sub" data-i18n="subtitle">Lightweight FHIR R4 Server</div>
  </div>
  <button class="lang-toggle" id="lang-toggle" onclick="toggleLang()"></button>
</div>

<div class="container">
  <div class="card">
    <h2 data-i18n="status.title">Server Status</h2>
    <div class="status-row">
      <div class="dot"></div>
      <span data-i18n="status.running">Running</span>
      <span style="color:#95a5a6; margin-left: auto;" id="version"></span>
    </div>
  </div>

  <div class="card hidden" id="welcome">
    <h2 data-i18n="welcome.title">Getting started</h2>
    <p style="margin-bottom:14px; color:#555;" data-i18n="welcome.body">
      This server is empty. Load a small sample dataset to start exploring patients, vitals, and prescriptions right away.
    </p>
    <button id="demo-btn" class="primary-btn" data-i18n="welcome.btn" onclick="loadDemo()">Load sample data</button>
    <span id="demo-status" style="margin-left:12px; color:#95a5a6; font-size:13px;"></span>
  </div>

  <div class="card">
    <h2 data-i18n="resources.title">Resources</h2>
    <div class="stats" id="stats">
      <div class="stat">
        <div class="num" id="total">-</div>
        <div class="label" data-i18n="resources.total">Total</div>
      </div>
    </div>
  </div>

  <div class="card">
    <h2 data-i18n="search.title">Search</h2>
    <div class="search-row">
      <input id="search-type" list="search-types" data-i18n-ph="search.type" placeholder="Resource type">
      <datalist id="search-types"></datalist>
      <input id="search-query" data-i18n-ph="search.query" placeholder="name=Yamada"
             onkeydown="if(event.key==='Enter')runSearch()">
      <button class="search-btn" data-i18n="search.btn" onclick="runSearch()">Search</button>
    </div>
    <div class="search-hint" data-i18n="search.hint">Tip: type a resource type and a parameter, e.g. Patient with name=Yamada. The FHIR URL is shown so you can learn it.</div>
    <div class="search-url hidden" id="search-url"></div>
    <table class="resource-table hidden" id="search-results-table" style="margin-top:10px;">
      <thead><tr><th data-i18n="list.id">ID</th><th data-i18n="list.summary">Summary</th></tr></thead>
      <tbody id="search-results-body"></tbody>
    </table>
  </div>

  <div class="card hidden" id="resource-list">
    <div class="panel-header">
      <button class="back-btn" data-i18n="list.back" onclick="hideResourceList()">&larr; Back</button>
      <h2 id="resource-list-title">Resources</h2>
    </div>
    <table class="resource-table">
      <thead>
        <tr><th data-i18n="list.id">ID</th><th data-i18n="list.updated">Last Updated</th><th data-i18n="list.summary">Summary</th></tr>
      </thead>
      <tbody id="resource-list-body">
      </tbody>
    </table>
    <div class="pagination" id="resource-list-pagination"></div>
  </div>

  <div class="card hidden" id="resource-detail">
    <div class="panel-header">
      <button class="back-btn" data-i18n="detail.back" onclick="hideResourceDetail()">&larr; Back</button>
      <h2 id="resource-detail-title">Resource</h2>
    </div>
    <pre class="json-view" id="resource-detail-json"></pre>
  </div>

  <div class="card" id="activity-card">
    <h2 data-i18n="activity.title">Recent Activity</h2>
    <table class="log-table">
      <thead>
        <tr><th data-i18n="activity.time">Time</th><th data-i18n="activity.op">Operation</th><th data-i18n="activity.resource">Resource</th><th data-i18n="activity.result">Result</th></tr>
      </thead>
      <tbody id="logs">
        <tr><td colspan="4" style="color:#bbb" data-i18n="activity.loading">Loading...</td></tr>
      </tbody>
    </table>
  </div>

  <div class="card">
    <h2 data-i18n="endpoints.title">API Endpoints</h2>
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

  <div class="refresh-note" id="refresh-note"></div>
</div>

<script>
// --- Minimal i18n: a message catalog keyed by string id. Default English,
// auto-detect Japanese from the browser, switchable and remembered. Adding a
// language is just adding one dictionary here. ---
const I18N = {
  en: {
    "subtitle": "Lightweight FHIR R4 Server",
    "status.title": "Server Status", "status.running": "Running",
    "welcome.title": "Getting started",
    "welcome.body": "This server is empty. Load a small sample dataset to start exploring patients, vitals, and prescriptions right away.",
    "welcome.btn": "Load sample data", "welcome.loading": "Loading…",
    "resources.title": "Resources", "resources.total": "Total",
    "search.title": "Search", "search.type": "Resource type", "search.query": "name=Yamada",
    "search.btn": "Search",
    "search.hint": "Tip: pick a resource type and a parameter, e.g. Patient with name=Yamada. The FHIR URL is shown so you can learn it.",
    "search.results": "results", "search.none": "No matches", "search.error": "Search error",
    "activity.title": "Recent Activity", "activity.time": "Time", "activity.op": "Operation",
    "activity.resource": "Resource", "activity.result": "Result",
    "activity.loading": "Loading...", "activity.none": "No activity yet",
    "endpoints.title": "API Endpoints",
    "list.back": "← Back", "list.id": "ID", "list.updated": "Last Updated", "list.summary": "Summary",
    "list.loading": "Loading...", "list.none": "No resources",
    "list.prev": "← Prev", "list.next": "Next →", "list.of": "of",
    "detail.back": "← Back",
    "footer.auto": "Auto-refreshes every 5 seconds", "footer.updated": "Last updated:",
    "footer.fetchError": "Fetch error", "footer.fetchFailed": "Fetch failed",
    "lang.name": "日本語"
  },
  ja: {
    "subtitle": "軽量 FHIR R4 サーバ",
    "status.title": "サーバ状態", "status.running": "稼働中",
    "welcome.title": "はじめての方へ",
    "welcome.body": "サーバは空の状態です。サンプルデータを入れると、患者・検査値・処方などをすぐに眺められます。",
    "welcome.btn": "サンプルデータを入れる", "welcome.loading": "読み込み中…",
    "resources.title": "リソース", "resources.total": "合計",
    "search.title": "検索", "search.type": "リソース型", "search.query": "name=山田",
    "search.btn": "検索",
    "search.hint": "ヒント：リソース型とパラメータを入力（例：Patient に name=山田）。生成された FHIR の URL を見ながら覚えられます。",
    "search.results": "件", "search.none": "該当なし", "search.error": "検索エラー",
    "activity.title": "最近の操作", "activity.time": "時刻", "activity.op": "操作",
    "activity.resource": "リソース", "activity.result": "結果",
    "activity.loading": "読み込み中...", "activity.none": "まだ操作はありません",
    "endpoints.title": "API エンドポイント",
    "list.back": "← 戻る", "list.id": "ID", "list.updated": "更新日時", "list.summary": "概要",
    "list.loading": "読み込み中...", "list.none": "リソースはありません",
    "list.prev": "← 前", "list.next": "次 →", "list.of": "／",
    "detail.back": "← 戻る",
    "footer.auto": "5秒ごとに自動更新", "footer.updated": "最終更新:",
    "footer.fetchError": "取得エラー", "footer.fetchFailed": "取得失敗",
    "lang.name": "English"
  }
};

let lang = localStorage.getItem('lang') ||
           ((navigator.language || 'en').toLowerCase().startsWith('ja') ? 'ja' : 'en');

function t(key) {
  return (I18N[lang] && I18N[lang][key]) || I18N.en[key] || key;
}

function applyI18n() {
  document.documentElement.lang = lang;
  document.querySelectorAll('[data-i18n]').forEach(function(el) {
    el.textContent = t(el.getAttribute('data-i18n'));
  });
  document.querySelectorAll('[data-i18n-ph]').forEach(function(el) {
    el.setAttribute('placeholder', t(el.getAttribute('data-i18n-ph')));
  });
  document.getElementById('lang-toggle').textContent = t('lang.name');
}

function toggleLang() {
  lang = (lang === 'ja') ? 'en' : 'ja';
  localStorage.setItem('lang', lang);
  applyI18n();
  refresh();
  if (!document.getElementById('resource-list').classList.contains('hidden')) {
    showResourceList(currentType, currentOffset);
  }
}

async function refresh() {
  const noteEl = document.getElementById('refresh-note');
  try {
    const res = await fetch('/$status?_=' + Date.now(), { cache: 'no-store' });
    if (!res.ok) {
      noteEl.textContent = t('footer.fetchError') + ': HTTP ' + res.status + ' (' + new Date().toLocaleTimeString() + ')';
      return;
    }
    const data = await res.json();

    document.getElementById('version').textContent =
      'v' + data.version + ' / FHIR ' + data.fhirVersion;

    // First-run welcome: show the sample-data prompt only while empty.
    const welcome = document.getElementById('welcome');
    if (data.totalResources === 0) welcome.classList.remove('hidden');
    else welcome.classList.add('hidden');

    // Resource type stats
    const statsEl = document.getElementById('stats');
    let statsHtml = '<div class="stat"><div class="num">' + data.totalResources +
                    '</div><div class="label">' + t('resources.total') + '</div></div>';
    for (const rc of data.resourceCounts) {
      statsHtml += '<div class="stat clickable" onclick="showResourceList(\'' + rc.type + '\')">' +
                   '<div class="num">' + rc.count + '</div><div class="label">' + rc.type + '</div></div>';
    }
    statsEl.innerHTML = statsHtml;

    // Populate the search type suggestions.
    const dl = document.getElementById('search-types');
    dl.innerHTML = data.resourceCounts.map(rc => '<option value="' + rc.type + '">').join('');

    // Activity log
    const logsEl = document.getElementById('logs');
    if (data.recentActivity.length === 0) {
      logsEl.innerHTML = '<tr><td colspan="4" style="color:#bbb">' + t('activity.none') + '</td></tr>';
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

    noteEl.textContent = t('footer.updated') + ' ' + new Date().toLocaleTimeString();
  } catch (err) {
    noteEl.textContent = t('footer.fetchFailed') + ': ' + err.message + ' (' + new Date().toLocaleTimeString() + ')';
  }
}

async function loadDemo() {
  const btn = document.getElementById('demo-btn');
  const status = document.getElementById('demo-status');
  btn.disabled = true;
  status.textContent = t('welcome.loading');
  try {
    const res = await fetch('/$demo', { method: 'POST' });
    const data = await res.json();
    if (!res.ok) {
      status.textContent = (data.error || ('HTTP ' + res.status));
      btn.disabled = false;
      return;
    }
    status.textContent = (data.message || 'Loaded') + ' ✓';
    await refresh();
  } catch (err) {
    status.textContent = err.message;
    btn.disabled = false;
  }
}

// --- Search builder: build a FHIR query, show the URL, list results. ---
async function runSearch() {
  const type = (document.getElementById('search-type').value || '').trim();
  const query = (document.getElementById('search-query').value || '').trim();
  if (!type) return;
  const url = '/' + type + (query ? ('?' + query) : '');
  const urlEl = document.getElementById('search-url');
  const tableEl = document.getElementById('search-results-table');
  const bodyEl = document.getElementById('search-results-body');

  urlEl.classList.remove('hidden');
  urlEl.innerHTML = 'GET <code>' + url.replace(/</g,'&lt;') + '</code>';
  tableEl.classList.remove('hidden');
  bodyEl.innerHTML = '<tr><td colspan="2" style="color:#bbb">' + t('list.loading') + '</td></tr>';

  try {
    const res = await fetch(url, { headers: { 'Accept': 'application/fhir+json' } });
    const data = await res.json();
    if (!res.ok) {
      const msg = (data.issue && data.issue[0] && data.issue[0].diagnostics) || ('HTTP ' + res.status);
      bodyEl.innerHTML = '<tr><td colspan="2" style="color:#c0392b">' + t('search.error') + ': ' + msg.replace(/</g,'&lt;') + '</td></tr>';
      return;
    }
    const entries = (data.entry || []).filter(e => e.search ? e.search.mode === 'match' : true);
    urlEl.innerHTML += ' &middot; <strong>' + (data.total != null ? data.total : entries.length) + '</strong> ' + t('search.results');
    if (entries.length === 0) {
      bodyEl.innerHTML = '<tr><td colspan="2" style="color:#bbb">' + t('search.none') + '</td></tr>';
      return;
    }
    bodyEl.innerHTML = entries.map(e => {
      const r = e.resource || {};
      const id = r.id || '-';
      return '<tr class="clickable-row" onclick="showResource(\'' + type + '\',\'' + id + '\')">' +
             '<td><code>' + id + '</code></td><td>' + getSummary(r) + '</td></tr>';
    }).join('');
  } catch (err) {
    bodyEl.innerHTML = '<tr><td colspan="2" style="color:#c0392b">' + t('search.error') + ': ' + err.message + '</td></tr>';
  }
}

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
  body.innerHTML = '<tr><td colspan="3" style="color:#bbb">' + t('list.loading') + '</td></tr>';

  try {
    const res = await fetch('/$browse/' + type + '?_count=' + PAGE_SIZE + '&_offset=' + currentOffset);
    const data = await res.json();
    const entries = data.entries || [];
    const total = data.total || 0;

    if (entries.length === 0) {
      body.innerHTML = '<tr><td colspan="3" style="color:#bbb">' + t('list.none') + '</td></tr>';
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
      '<button ' + (currentOffset === 0 ? 'disabled' : 'onclick="showResourceList(\'' + type + '\',' + (currentOffset - PAGE_SIZE) + ')"') + '>' + t('list.prev') + '</button>' +
      '<span>' + (currentOffset + 1) + '–' + showing + ' ' + t('list.of') + ' ' + total + '</span>' +
      '<button ' + (currentOffset + PAGE_SIZE >= total ? 'disabled' : 'onclick="showResourceList(\'' + type + '\',' + (currentOffset + PAGE_SIZE) + ')"') + '>' + t('list.next') + '</button>';
  } catch (err) {
    body.innerHTML = '<tr><td colspan="3" style="color:#c00">Error: ' + err.message + '</td></tr>';
  }
}

function getSummary(r) {
  if (r.name && r.name[0]) {
    const n = r.name[0];
    if (n.text) return n.text;
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
  const pre = document.getElementById('resource-detail-json');
  pre.textContent = t('list.loading');
  document.getElementById('resource-detail').scrollIntoView({ behavior: 'smooth', block: 'start' });

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
}

applyI18n();
refresh();
setInterval(refresh, 5000);
</script>
</body>
</html>
"##;
