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
//!
//! Neither holds the export in memory: a job writes its files under its
//! directory (`<data_dir>/exports/<job>/`, deleted with the job), the
//! synchronous response is streamed as it is read — a page of resources at a
//! time either way.

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path as FsPath, PathBuf};
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

use crate::auth::{has_system_scope, AuthType, AuthUser};
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
    let perms = if action == "read" { "r" } else { "cu" };
    if !has_system_scope(&user.scopes, perms) {
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
    /// (resource type, NDJSON file) for each non-empty type.
    files: Vec<(String, PathBuf)>,
    /// When the job completed or failed (for expiry).
    finished_at: Option<std::time::Instant>,
}

/// How long a finished export's files stay downloadable.
const JOB_RETENTION: std::time::Duration = std::time::Duration::from_secs(60 * 60);
/// Jobs kept at once (running or finished but not yet expired/deleted): each
/// holds a copy of (part of) the data on disk.
const MAX_JOBS: usize = 8;
/// Resources read from the store at a time.
const PAGE: usize = 1000;

/// The bulk-export jobs, their files under `dir/<job id>/`.
pub struct ExportJobs {
    jobs: Mutex<HashMap<String, ExportJob>>,
    dir: PathBuf,
    /// `dir` is a temporary directory of this value's own (deleted on drop).
    owned: bool,
}

impl ExportJobs {
    /// Jobs in a temporary directory of their own (tests, embedding).
    pub fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("sazare-exports-{}", uuid::Uuid::new_v4()));
        Self { jobs: Default::default(), dir, owned: true }
    }

    /// Jobs under `dir` (`<data_dir>/exports`). Jobs don't outlive the
    /// process, so files a previous run left there are deleted.
    pub fn in_dir(dir: PathBuf) -> Self {
        let _ = std::fs::remove_dir_all(&dir);
        Self { jobs: Default::default(), dir, owned: false }
    }

    fn job_dir(&self, id: &str) -> PathBuf {
        self.dir.join(id)
    }

    /// Register a job. Finished jobs past their retention are dropped first;
    /// `false` when [`MAX_JOBS`] are still held.
    async fn start(&self, id: String, transaction_time: String, request_url: String) -> bool {
        let mut jobs = self.live().await;
        if jobs.len() >= MAX_JOBS {
            return false;
        }
        jobs.insert(
            id,
            ExportJob {
                status: JobStatus::InProgress,
                transaction_time,
                request_url,
                files: Vec::new(),
                finished_at: None,
            },
        );
        true
    }

    async fn complete(&self, id: &str, files: Vec<(String, PathBuf)>) {
        match self.jobs.lock().await.get_mut(id) {
            Some(job) => {
                job.files = files;
                job.status = JobStatus::Complete;
                job.finished_at = Some(std::time::Instant::now());
            }
            // Deleted while it ran.
            None => self.delete_files(id),
        }
    }

    async fn fail(&self, id: &str, err: String) {
        self.delete_files(id);
        if let Some(job) = self.jobs.lock().await.get_mut(id) {
            job.status = JobStatus::Failed(err);
            job.finished_at = Some(std::time::Instant::now());
        }
    }

    /// Lock the job table with finished jobs past their retention dropped
    /// (their files deleted), so an expired job is gone whoever looks next.
    async fn live(&self) -> tokio::sync::MutexGuard<'_, HashMap<String, ExportJob>> {
        let mut jobs = self.jobs.lock().await;
        let expired: Vec<String> = jobs
            .iter()
            .filter(|(_, j)| j.finished_at.is_some_and(|t| t.elapsed() >= JOB_RETENTION))
            .map(|(id, _)| id.clone())
            .collect();
        for id in expired {
            jobs.remove(&id);
            self.delete_files(&id);
        }
        jobs
    }

    async fn remove(&self, id: &str) -> bool {
        let removed = self.jobs.lock().await.remove(id).is_some();
        if removed {
            // A running job's files go when it finishes (`complete`).
            self.delete_files(id);
        }
        removed
    }

    /// Delete a job's files (a download still reading one keeps it open
    /// until done, where the system allows; else the next start removes it).
    fn delete_files(&self, id: &str) {
        let _ = std::fs::remove_dir_all(self.job_dir(id));
    }
}

impl Default for ExportJobs {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for ExportJobs {
    fn drop(&mut self) {
        if self.owned {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }
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

/// Parse `_since` (a FHIR instant). Query decoding turns an unencoded `+`
/// (`...T10:00:00+09:00`) into a space; an instant never contains one, so the
/// `+` is put back first.
fn parse_since(since: &Option<String>) -> Result<Option<chrono::DateTime<chrono::FixedOffset>>, String> {
    match since.as_deref() {
        None => Ok(None),
        Some(s) => chrono::DateTime::parse_from_rfc3339(&s.replace(' ', "+"))
            .map(Some)
            .map_err(|_| {
                format!(
                    "Invalid _since '{s}': expected a FHIR instant such as 2024-01-01T00:00:00Z or 2024-01-01T09:00:00+09:00"
                )
            }),
    }
}

/// Read the resources of the requested scope (`_type`, `_since` applied) a
/// page at a time, handing each one's JSON to `out(type, json)`, type by type.
/// Stops at the first error `out` returns.
fn write_export(
    state: &AppState,
    scope: &ExportScope,
    type_filter: &Option<Vec<String>>,
    since: &Option<String>,
    out: &mut dyn FnMut(&str, &str) -> Result<(), String>,
) -> Result<(), String> {
    let since_dt = parse_since(since)?;
    let types = state.store.count_by_type().map_err(|e| format!("Export failed: {e}"))?;
    for (rtype, _) in types {
        if let Some(types) = type_filter
            && !types.iter().any(|t| t == &rtype)
        {
            continue;
        }
        if !matches!(scope, ExportScope::System) && !state.compartment_def.is_in_compartment(&rtype) {
            continue;
        }
        let mut after = String::new();
        loop {
            let page = state.store.list_page(&rtype, &after, PAGE).map_err(|e| format!("Export failed: {e}"))?;
            let Some((last, _)) = page.last() else { break };
            after = last.clone();
            for (_id, data) in &page {
                if exported(state, scope, since_dt, &rtype, data) {
                    let Ok(text) = std::str::from_utf8(data) else { continue };
                    out(&rtype, text.trim_end())?;
                }
            }
        }
    }
    Ok(())
}

/// Whether a stored resource of an exported type goes into the export: in
/// the requested patients' compartments, changed since `_since`.
fn exported(
    state: &AppState,
    scope: &ExportScope,
    since_dt: Option<chrono::DateTime<chrono::FixedOffset>>,
    rtype: &str,
    data: &[u8],
) -> bool {
    if matches!(scope, ExportScope::System | ExportScope::AllPatients) && since_dt.is_none() {
        return true;
    }
    let Ok(resource) = serde_json::from_slice::<Value>(data) else { return false };
    if let ExportScope::Patients(ids) = scope
        && !ids.iter().any(|pid| state.compartment_def.resource_belongs_to_patient(rtype, &resource, pid))
    {
        return false;
    }
    if let Some(since_dt) = since_dt {
        // Drop resources not modified since the cutoff.
        let last_updated = resource
            .pointer("/meta/lastUpdated")
            .and_then(|v| v.as_str())
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok());
        if !last_updated.is_some_and(|lu| lu >= since_dt) {
            return false;
        }
    }
    true
}

/// Write a job's export into `dir`, one `<type>.ndjson` per non-empty type.
fn build_export_files(
    state: &AppState,
    scope: &ExportScope,
    type_filter: &Option<Vec<String>>,
    since: &Option<String>,
    dir: &FsPath,
) -> Result<Vec<(String, PathBuf)>, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("Export failed: {}: {e}", dir.display()))?;
    let mut files: Vec<(String, PathBuf, std::io::BufWriter<std::fs::File>)> = Vec::new();
    write_export(state, scope, type_filter, since, &mut |rtype, json| {
        if files.last().is_none_or(|(t, _, _)| t != rtype) {
            // Types come one after another: the previous file is complete.
            let path = dir.join(format!("{rtype}.ndjson"));
            let file = std::fs::File::create(&path).map_err(|e| format!("Export failed: {}: {e}", path.display()))?;
            files.push((rtype.to_string(), path, std::io::BufWriter::new(file)));
        }
        let (_, path, w) = files.last_mut().expect("just pushed");
        w.write_all(json.as_bytes())
            .and_then(|_| w.write_all(b"\n"))
            .map_err(|e| format!("Export failed: {}: {e}", path.display()))
    })?;
    files
        .into_iter()
        .map(|(t, path, mut w)| {
            w.flush().map_err(|e| format!("Export failed: {}: {e}", path.display()))?;
            Ok((t, path))
        })
        .collect()
}

/// A body that streams the chunks a blocking producer sends.
fn streamed(rx: tokio::sync::mpsc::Receiver<Result<Vec<u8>, std::io::Error>>) -> axum::body::Body {
    axum::body::Body::from_stream(futures_util::stream::unfold(rx, |mut rx| async move {
        rx.recv().await.map(|chunk| (chunk, rx))
    }))
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

    // Reject a malformed _since up front: silently ignoring it would export
    // everything, which looks like success.
    if let Err(e) = parse_since(&params._since) {
        return (StatusCode::BAD_REQUEST, Json(op_outcome("invalid", e))).into_response();
    }

    let type_filter = parse_type_filter(&params);

    let want_async = headers
        .get("prefer")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_lowercase().contains("respond-async"))
        .unwrap_or(false);

    // Legacy synchronous path: one NDJSON body, streamed as the store is read
    // (off the async workers). A failure part way can only cut the body short.
    if !want_async {
        let (tx, rx) = tokio::sync::mpsc::channel(4);
        let since = params._since.clone();
        tokio::task::spawn_blocking(move || {
            let mut chunk = Vec::with_capacity(1 << 16);
            let sent = write_export(&state, &scope, &type_filter, &since, &mut |_, json| {
                chunk.extend_from_slice(json.as_bytes());
                chunk.push(b'\n');
                if chunk.len() >= 1 << 16 {
                    let full = std::mem::replace(&mut chunk, Vec::with_capacity(1 << 16));
                    tx.blocking_send(Ok(full)).map_err(|_| "the client went away".to_string())?;
                }
                Ok(())
            });
            let last = match sent {
                Ok(()) => Ok(chunk),
                Err(e) => {
                    tracing::warn!("synchronous $export stopped: {e}");
                    Err(std::io::Error::other(e))
                }
            };
            let _ = tx.blocking_send(last);
        });
        return (StatusCode::OK, [(header::CONTENT_TYPE, "application/fhir+ndjson")], streamed(rx)).into_response();
    }

    // Async path: register a job, build in the background, return 202 + Content-Location.
    let base = crate::handlers::base_url_from_headers(&headers);
    let job_id = uuid::Uuid::new_v4().to_string();
    let transaction_time = chrono::Utc::now().to_rfc3339();
    let request_url = format!("{base}{request_path}");
    if !state
        .export_jobs
        .start(job_id.clone(), transaction_time, request_url)
        .await
    {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            [(header::RETRY_AFTER, "60")],
            Json(op_outcome(
                "throttled",
                format!("{MAX_JOBS} export jobs are held; delete finished ones (DELETE the status URL) or retry later"),
            )),
        )
            .into_response();
    }

    let state2 = state.clone();
    let job_id2 = job_id.clone();
    let since = params._since.clone();
    // Building reads the whole store synchronously: keep it off the async
    // worker threads.
    tokio::spawn(async move {
        let state3 = state2.clone();
        let dir = state2.export_jobs.job_dir(&job_id2);
        let built = tokio::task::spawn_blocking(move || build_export_files(&state3, &scope, &type_filter, &since, &dir))
            .await
            .unwrap_or_else(|e| Err(format!("export task failed: {e}")));
        match built {
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
    let jobs = state.export_jobs.live().await;
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
            let base = crate::handlers::base_url_from_headers(&headers);
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
    let path = {
        let jobs = state.export_jobs.live().await;
        jobs.get(&job_id).and_then(|j| j.files.iter().find(|(t, _)| t == &rtype)).map(|(_, p)| p.clone())
    };
    // Opened before the job could expire and its files go.
    let file = match path {
        Some(p) => tokio::fs::File::open(p).await.ok(),
        None => None,
    };
    match file {
        Some(file) => {
            let body = axum::body::Body::from_stream(futures_util::stream::unfold(file, |mut file| async move {
                use tokio::io::AsyncReadExt;
                let mut buf = vec![0u8; 1 << 16];
                match file.read(&mut buf).await {
                    Ok(0) => None,
                    Ok(n) => {
                        buf.truncate(n);
                        Some((Ok::<_, std::io::Error>(buf), file))
                    }
                    Err(e) => Some((Err(e), file)),
                }
            }));
            (StatusCode::OK, [(header::CONTENT_TYPE, "application/fhir+ndjson")], body).into_response()
        }
        None => (
            StatusCode::NOT_FOUND,
            Json(op_outcome("not-found", "Unknown export file".into())),
        )
            .into_response(),
    }
}

#[cfg(test)]
mod job_file_tests {
    use super::*;

    #[tokio::test]
    async fn a_job_s_files_go_with_it() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("exports");
        // Left by a previous run: deleted on start.
        std::fs::create_dir_all(dir.join("old-job")).unwrap();
        let jobs = ExportJobs::in_dir(dir.clone());
        assert!(!dir.join("old-job").exists());

        assert!(jobs.start("j1".into(), String::new(), String::new()).await);
        std::fs::create_dir_all(jobs.job_dir("j1")).unwrap();
        let file = jobs.job_dir("j1").join("Patient.ndjson");
        std::fs::write(&file, "{}\n").unwrap();
        jobs.complete("j1", vec![("Patient".into(), file)]).await;
        assert!(jobs.remove("j1").await);
        assert!(!jobs.job_dir("j1").exists(), "DELETE removes the files");

        // A job deleted while it ran: its files go when it finishes.
        assert!(jobs.start("j2".into(), String::new(), String::new()).await);
        assert!(jobs.remove("j2").await);
        std::fs::create_dir_all(jobs.job_dir("j2")).unwrap();
        jobs.complete("j2", vec![]).await;
        assert!(!jobs.job_dir("j2").exists());
    }

    #[test]
    fn a_temporary_job_directory_is_removed_on_drop() {
        let jobs = ExportJobs::new();
        std::fs::create_dir_all(jobs.job_dir("j")).unwrap();
        let dir = jobs.dir.clone();
        drop(jobs);
        assert!(!dir.exists());
    }
}
