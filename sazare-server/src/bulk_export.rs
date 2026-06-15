//! Asynchronous Bulk Data export (FHIR Bulk Data Access IG, "Flat FHIR").
//!
//! Kick-off:  `GET /$export` with `Prefer: respond-async`
//!            -> `202 Accepted` + `Content-Location: <status-url>`
//! Status:    `GET <status-url>` -> `202` while running (with `X-Progress`),
//!            or `200` with a manifest `{transactionTime, request, output[...]}`
//!            once complete. `DELETE <status-url>` cancels the job.
//! Files:     `GET <output-url>` -> NDJSON for that resource type.
//!
//! Without `Prefer: respond-async` the endpoint falls back to the legacy
//! synchronous NDJSON response so existing callers keep working.

use std::collections::HashMap;
use std::sync::Arc;

use axum::{
    extract::{Extension, Path, Query, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::Mutex;

use crate::auth::{check_scope, AuthType, AuthUser};
use crate::AppState;

/// Authorize a Bulk Data operation against the caller's SMART scopes.
///
/// Bulk export/import touch the *entire* dataset (or all patients), so they must
/// not be reachable by a patient-scoped token, and a JWT caller needs a
/// system-level `*.{action}` scope. Auth-disabled deployments (no `AuthUser`)
/// and coarse server credentials (API key / Basic, which carry no scopes in this
/// server's model) are allowed through, matching how CRUD scope checks behave.
// The `Err` is a ready-to-return axum `Response`; callers just `return resp`.
// Boxing it (what `result_large_err` wants) would only add an allocation on the
// auth-failure path and ripple through every caller for no real benefit.
#[allow(clippy::result_large_err)]
pub(crate) fn authorize_bulk(
    auth: &Option<Extension<AuthUser>>,
    action: &str,
) -> Result<(), Response> {
    let Some(Extension(user)) = auth.as_ref() else {
        return Ok(());
    };
    if user.auth_type != AuthType::Jwt {
        return Ok(());
    }
    if user.is_patient_scoped() {
        return Err(bulk_forbidden(
            "Patient-scoped tokens may not perform Bulk Data operations",
        ));
    }
    if !check_scope(&user.scopes, "*", action) {
        return Err(bulk_forbidden(&format!(
            "Bulk Data {action} requires a system-level *.{action} scope"
        )));
    }
    Ok(())
}

fn bulk_forbidden(msg: &str) -> Response {
    (
        StatusCode::FORBIDDEN,
        Json(op_outcome("forbidden", msg.to_string())),
    )
        .into_response()
}

/// Query parameters accepted by `$export`. Field names mirror the FHIR
/// parameter names (`_type`, `_since`, `_outputFormat`) for serde matching.
#[derive(Deserialize, Default)]
#[allow(non_snake_case)]
pub struct ExportParams {
    /// Comma-separated resource types (e.g. `Patient,Observation`).
    pub _type: Option<String>,
    /// Only resources changed at/after this instant (`meta.lastUpdated`).
    pub _since: Option<String>,
    /// Output format; only NDJSON variants are supported.
    pub _outputFormat: Option<String>,
    /// `_typeFilter` is part of the Bulk Data IG but not implemented here; it is
    /// captured only so we can reject it rather than silently ignore it (which
    /// would over-disclose data the client asked to filter out).
    pub _typeFilter: Option<String>,
}

#[derive(Clone)]
enum JobStatus {
    InProgress,
    Complete,
    Failed(String),
}

struct ExportJob {
    status: JobStatus,
    transaction_time: String,
    request_url: String,
    /// (resource type, NDJSON) for each non-empty type.
    files: Vec<(String, String)>,
}

/// In-memory registry of bulk-export jobs.
#[derive(Default)]
pub struct ExportJobs {
    jobs: Mutex<HashMap<String, ExportJob>>,
}

impl ExportJobs {
    pub fn new() -> Self {
        Self::default()
    }

    async fn start(&self, id: String, transaction_time: String, request_url: String) {
        self.jobs.lock().await.insert(
            id,
            ExportJob {
                status: JobStatus::InProgress,
                transaction_time,
                request_url,
                files: Vec::new(),
            },
        );
    }

    async fn complete(&self, id: &str, files: Vec<(String, String)>) {
        if let Some(job) = self.jobs.lock().await.get_mut(id) {
            job.files = files;
            job.status = JobStatus::Complete;
        }
    }

    async fn fail(&self, id: &str, err: String) {
        if let Some(job) = self.jobs.lock().await.get_mut(id) {
            job.status = JobStatus::Failed(err);
        }
    }

    async fn remove(&self, id: &str) -> bool {
        self.jobs.lock().await.remove(id).is_some()
    }
}

fn base_url(headers: &HeaderMap) -> String {
    let host = headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("localhost:8080");
    let scheme = headers
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("http");
    format!("{scheme}://{host}")
}

fn op_outcome(code: &str, diag: String) -> Value {
    json!({
        "resourceType": "OperationOutcome",
        "issue": [{"severity": "error", "code": code, "diagnostics": diag}]
    })
}

/// Which resources a `$export` covers.
pub enum ExportScope {
    /// All resources (`[base]/$export`).
    System,
    /// Every resource in the Patient compartment (`[base]/Patient/$export`).
    AllPatients,
    /// Only the given patients' compartments (`[base]/Group/{id}/$export`).
    Patients(Vec<String>),
}

/// Build per-type NDJSON for the requested scope, applying `_type` and `_since`.
pub fn build_export_files(
    state: &AppState,
    scope: &ExportScope,
    type_filter: &Option<Vec<String>>,
    since: &Option<String>,
) -> Result<Vec<(String, String)>, String> {
    let since_dt = since
        .as_deref()
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok());

    let rows = state
        .store
        .list_all(None)
        .map_err(|e| format!("Export failed: {e}"))?;

    // Preserve first-seen type order for stable manifests.
    let mut order: Vec<String> = Vec::new();
    let mut by_type: HashMap<String, String> = HashMap::new();

    for (rtype, _id, data) in rows {
        if let Some(types) = type_filter
            && !types.iter().any(|t| t == &rtype)
        {
            continue;
        }
        // Scope filtering: restrict to the Patient compartment as needed.
        match scope {
            ExportScope::System => {}
            ExportScope::AllPatients => {
                if !state.compartment_def.is_in_compartment(&rtype) {
                    continue;
                }
            }
            ExportScope::Patients(ids) => {
                if !state.compartment_def.is_in_compartment(&rtype) {
                    continue;
                }
                let resource: Value = match serde_json::from_slice(&data) {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                let belongs = ids.iter().any(|pid| {
                    state
                        .compartment_def
                        .resource_belongs_to_patient(&rtype, &resource, pid)
                });
                if !belongs {
                    continue;
                }
            }
        }
        let Ok(text) = std::str::from_utf8(&data) else {
            continue;
        };
        if let Some(since_dt) = since_dt {
            // Drop resources not modified since the cutoff.
            let last_updated = serde_json::from_slice::<Value>(&data)
                .ok()
                .and_then(|v| {
                    v.get("meta")
                        .and_then(|m| m.get("lastUpdated"))
                        .and_then(|l| l.as_str())
                        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                });
            match last_updated {
                Some(lu) if lu >= since_dt => {}
                _ => continue,
            }
        }
        if !by_type.contains_key(&rtype) {
            order.push(rtype.clone());
        }
        let buf = by_type.entry(rtype).or_default();
        buf.push_str(text.trim_end());
        buf.push('\n');
    }

    Ok(order
        .into_iter()
        .map(|t| {
            let ndjson = by_type.remove(&t).unwrap_or_default();
            (t, ndjson)
        })
        .collect())
}

fn parse_type_filter(params: &ExportParams) -> Option<Vec<String>> {
    params._type.as_ref().map(|t| {
        t.split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    })
}

/// Shared `$export` logic for every level. Async when `Prefer: respond-async`,
/// otherwise a legacy single concatenated NDJSON body. `request_path` is the
/// operation path recorded in the manifest's `request` field.
async fn run_export(
    state: Arc<AppState>,
    headers: HeaderMap,
    scope: ExportScope,
    request_path: &str,
    params: ExportParams,
) -> Response {
    // Reject _typeFilter rather than silently ignoring it (over-disclosure).
    if params._typeFilter.is_some() {
        return (
            StatusCode::BAD_REQUEST,
            Json(op_outcome(
                "not-supported",
                "_typeFilter is not supported by this server".into(),
            )),
        )
            .into_response();
    }

    // Validate _outputFormat (NDJSON only).
    if let Some(fmt) = &params._outputFormat {
        let ok = matches!(
            fmt.as_str(),
            "application/fhir+ndjson" | "application/ndjson" | "ndjson"
        );
        if !ok {
            return (
                StatusCode::BAD_REQUEST,
                Json(op_outcome(
                    "not-supported",
                    format!("Unsupported _outputFormat '{fmt}'. Use application/fhir+ndjson"),
                )),
            )
                .into_response();
        }
    }

    let type_filter = parse_type_filter(&params);

    let want_async = headers
        .get("prefer")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_lowercase().contains("respond-async"))
        .unwrap_or(false);

    // Legacy synchronous path: concatenate everything into one NDJSON body.
    if !want_async {
        return match build_export_files(&state, &scope, &type_filter, &params._since) {
            Ok(files) => {
                let body: String = files.into_iter().map(|(_, n)| n).collect();
                (
                    StatusCode::OK,
                    [(header::CONTENT_TYPE, "application/fhir+ndjson")],
                    body,
                )
                    .into_response()
            }
            Err(e) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(op_outcome("exception", e)),
            )
                .into_response(),
        };
    }

    // Async path: register a job, build in the background, return 202 + Content-Location.
    let base = base_url(&headers);
    let job_id = uuid::Uuid::new_v4().to_string();
    let transaction_time = chrono::Utc::now().to_rfc3339();
    let request_url = format!("{base}{request_path}");
    state
        .export_jobs
        .start(job_id.clone(), transaction_time, request_url)
        .await;

    let state2 = state.clone();
    let job_id2 = job_id.clone();
    let since = params._since.clone();
    tokio::spawn(async move {
        match build_export_files(&state2, &scope, &type_filter, &since) {
            Ok(files) => state2.export_jobs.complete(&job_id2, files).await,
            Err(e) => state2.export_jobs.fail(&job_id2, e).await,
        }
    });

    let status_url = format!("{base}/$export-status/{job_id}");
    (
        StatusCode::ACCEPTED,
        [(header::CONTENT_LOCATION, status_url)],
    )
        .into_response()
}

/// `GET /$export` — system-level export.
pub async fn export(
    State(state): State<Arc<AppState>>,
    auth: Option<Extension<AuthUser>>,
    headers: HeaderMap,
    Query(params): Query<ExportParams>,
) -> Response {
    if let Err(resp) = authorize_bulk(&auth, "read") {
        return resp;
    }
    run_export(state, headers, ExportScope::System, "/$export", params).await
}

/// `GET /Patient/$export` — every Patient compartment.
pub async fn patient_export(
    State(state): State<Arc<AppState>>,
    auth: Option<Extension<AuthUser>>,
    headers: HeaderMap,
    Query(params): Query<ExportParams>,
) -> Response {
    if let Err(resp) = authorize_bulk(&auth, "read") {
        return resp;
    }
    run_export(
        state,
        headers,
        ExportScope::AllPatients,
        "/Patient/$export",
        params,
    )
    .await
}

/// `GET /Group/{id}/$export` — the compartments of the Group's member patients.
pub async fn group_export(
    State(state): State<Arc<AppState>>,
    auth: Option<Extension<AuthUser>>,
    Path(group_id): Path<String>,
    headers: HeaderMap,
    Query(params): Query<ExportParams>,
) -> Response {
    if let Err(resp) = authorize_bulk(&auth, "read") {
        return resp;
    }
    // Load the Group and collect its member Patient ids.
    let group = match state.store.get("Group", &group_id) {
        Ok(Some(data)) => match serde_json::from_slice::<Value>(&data) {
            Ok(v) => v,
            Err(e) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(op_outcome("exception", format!("Bad Group: {e}"))),
                )
                    .into_response();
            }
        },
        _ => {
            return (
                StatusCode::NOT_FOUND,
                Json(op_outcome("not-found", format!("Group/{group_id} not found"))),
            )
                .into_response();
        }
    };

    let patient_ids: Vec<String> = group
        .get("member")
        .and_then(|m| m.as_array())
        .map(|members| {
            members
                .iter()
                .filter_map(|m| {
                    m.get("entity")
                        .and_then(|e| e.get("reference"))
                        .and_then(|r| r.as_str())
                        .and_then(|r| r.strip_prefix("Patient/"))
                        .map(|id| id.to_string())
                })
                .collect()
        })
        .unwrap_or_default();

    let path = format!("/Group/{group_id}/$export");
    run_export(state, headers, ExportScope::Patients(patient_ids), &path, params).await
}

/// `GET /$export-status/{job_id}` — poll job status / return the manifest.
pub async fn export_status(
    State(state): State<Arc<AppState>>,
    auth: Option<Extension<AuthUser>>,
    headers: HeaderMap,
    Path(job_id): Path<String>,
) -> Response {
    if let Err(resp) = authorize_bulk(&auth, "read") {
        return resp;
    }
    let jobs = state.export_jobs.jobs.lock().await;
    let Some(job) = jobs.get(&job_id) else {
        return (
            StatusCode::NOT_FOUND,
            Json(op_outcome("not-found", "Unknown export job".into())),
        )
            .into_response();
    };

    match &job.status {
        JobStatus::InProgress => (
            StatusCode::ACCEPTED,
            [("X-Progress", "in-progress"), ("Retry-After", "1")],
        )
            .into_response(),
        JobStatus::Failed(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(op_outcome("exception", e.clone())),
        )
            .into_response(),
        JobStatus::Complete => {
            let base = base_url(&headers);
            let output: Vec<Value> = job
                .files
                .iter()
                .map(|(rtype, _)| {
                    json!({
                        "type": rtype,
                        "url": format!("{base}/$export-file/{job_id}/{rtype}"),
                    })
                })
                .collect();
            let manifest = json!({
                // When auth is enabled the file endpoints require the bearer token,
                // so clients must be told to send it on download.
                "requiresAccessToken": state.config.auth.enabled,
                "transactionTime": job.transaction_time,
                "request": job.request_url,
                "output": output,
                "error": [],
            });
            (StatusCode::OK, Json(manifest)).into_response()
        }
    }
}

/// `DELETE /$export-status/{job_id}` — cancel/forget a job.
pub async fn export_delete(
    State(state): State<Arc<AppState>>,
    auth: Option<Extension<AuthUser>>,
    Path(job_id): Path<String>,
) -> Response {
    if let Err(resp) = authorize_bulk(&auth, "read") {
        return resp;
    }
    if state.export_jobs.remove(&job_id).await {
        StatusCode::ACCEPTED.into_response()
    } else {
        (
            StatusCode::NOT_FOUND,
            Json(op_outcome("not-found", "Unknown export job".into())),
        )
            .into_response()
    }
}

/// `GET /$export-file/{job_id}/{resource_type}` — download one NDJSON file.
pub async fn export_file(
    State(state): State<Arc<AppState>>,
    auth: Option<Extension<AuthUser>>,
    Path((job_id, rtype)): Path<(String, String)>,
) -> Response {
    if let Err(resp) = authorize_bulk(&auth, "read") {
        return resp;
    }
    let jobs = state.export_jobs.jobs.lock().await;
    let ndjson = jobs
        .get(&job_id)
        .and_then(|j| j.files.iter().find(|(t, _)| t == &rtype))
        .map(|(_, n)| n.clone());
    match ndjson {
        Some(ndjson) => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "application/fhir+ndjson")],
            ndjson,
        )
            .into_response(),
        None => (
            StatusCode::NOT_FOUND,
            Json(op_outcome("not-found", "Unknown export file".into())),
        )
            .into_response(),
    }
}
