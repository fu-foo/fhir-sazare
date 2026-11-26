//! fhir-sazare - Lightweight FHIR Server
//!
//! A portable FHIR R4 server with JP-Core support.

pub mod audit;
pub mod auth;
pub mod bulk;
pub mod bulk_export;
pub mod bundle;
pub mod compartment_check;
pub mod apps;
pub mod config;
pub mod dashboard;
pub mod demo;
pub mod handlers;
pub mod index_generation;
pub mod metrics;
pub mod smart;
pub mod subscription;
pub mod tls;
pub mod webhook;
pub mod websocket;

use axum::{
    http::Method,
    routing::{get, post},
    Router,
};
use sazare_core::{
    validation::{ProfileRegistry, TerminologyRegistry},
    CompartmentDef, SearchParamRegistry, SearchQuery,
};
use sazare_store::{AuditLog, SearchExecutor, SearchIndex, SqliteStore};
use serde_json::Value;
use std::sync::Arc;
use tokio::sync::Mutex;
use tower_http::{
    cors::{Any, CorsLayer},
    limit::RequestBodyLimitLayer,
    services::ServeDir,
    trace::TraceLayer,
};

/// Application state
pub struct AppState {
    pub store: SqliteStore,
    pub index: Mutex<SearchIndex>,
    /// Read-only connections for searches (empty: searches use `index`).
    pub index_readers: Readers,
    /// The index generation in use (see `index_generation`).
    pub index_file: std::sync::RwLock<std::path::PathBuf>,
    /// Held (shared) by every write across its index and store steps; taken
    /// exclusively by an index switch, so no write is between the two.
    pub write_gate: tokio::sync::RwLock<()>,
    /// A background index rebuild, if one runs.
    pub rebuild: index_generation::RebuildStatus,
    pub audit: Arc<Mutex<AuditLog>>,
    pub config: config::ServerConfig,
    pub profile_registry: ProfileRegistry,
    pub terminology_registry: TerminologyRegistry,
    pub search_param_registry: SearchParamRegistry,
    pub compartment_def: CompartmentDef,
    pub jwk_cache: tokio::sync::RwLock<auth::JwkCache>,
    /// Discovered static apps (served under /apps/, without auth)
    pub app_names: Vec<String>,
    /// Live WebSocket clients bound to Subscriptions (R4 websocket channel)
    pub ws_registry: Arc<websocket::WsRegistry>,
    /// Lifecycle webhook dispatcher (BundleCreated / TaskCompleted)
    pub webhook: Arc<webhook::WebhookManager>,
    /// In-flight async Bulk Data export jobs
    pub export_jobs: Arc<bulk_export::ExportJobs>,
    /// Seen SMART Backend Services assertion `jti` values (→ assertion `exp`),
    /// for one-time-use replay protection. Pruned lazily on insert.
    pub seen_jti: std::sync::Mutex<std::collections::HashMap<String, u64>>,
}

/// Conditional create result
pub enum ConditionalResult {
    NoMatch,
    Exists(Value),
    MultipleMatches,
    SearchError(String),
}

/// The HTTP client for outbound calls to configured services (JWKS, webhooks):
/// bounded connect and total timeouts, so a slow or silent remote can't hold
/// requests — or locks — indefinitely.
pub(crate) fn http_client() -> &'static reqwest::Client {
    static C: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    C.get_or_init(|| {
        reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(5))
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .unwrap_or_default()
    })
}

/// The swappable pool of read-only index connections: an index switch
/// replaces it; searches in flight keep the pool they started with.
pub struct Readers {
    current: std::sync::RwLock<Arc<sazare_store::IndexReaders>>,
    capacity: usize,
    /// One permit per reader: a search waits here (async) for a free reader
    /// before it takes a blocking thread, so a long search on one reader
    /// doesn't hold up a short one that another free reader could serve, and
    /// blocked searches don't pile up on the blocking thread pool.
    permits: tokio::sync::Semaphore,
}

impl Readers {
    pub fn new(readers: sazare_store::IndexReaders) -> Self {
        let capacity = readers.len().max(2);
        let permits = tokio::sync::Semaphore::new(readers.len().max(1));
        Self { current: std::sync::RwLock::new(Arc::new(readers)), capacity, permits }
    }
    pub fn current(&self) -> Arc<sazare_store::IndexReaders> {
        self.current.read().unwrap_or_else(|e| e.into_inner()).clone()
    }
    /// Install `new`; returns the previous pool (still used by searches in flight).
    pub fn replace(&self, new: sazare_store::IndexReaders) -> Arc<sazare_store::IndexReaders> {
        std::mem::replace(&mut *self.current.write().unwrap_or_else(|e| e.into_inner()), Arc::new(new))
    }
    pub fn capacity(&self) -> usize {
        self.capacity
    }
}

impl AppState {
    /// The index file in use.
    pub fn index_path(&self) -> std::path::PathBuf {
        self.index_file.read().unwrap_or_else(|e| e.into_inner()).clone()
    }
    pub fn set_index_path(&self, path: std::path::PathBuf) {
        *self.index_file.write().unwrap_or_else(|e| e.into_inner()) = path;
    }
}

/// Run a search-side closure on a read-only index connection, off the async
/// workers: SQLite calls block, and a long one (an exact total over millions
/// of matches) must not hold up other requests — nor, with a reader, the
/// writer. Without readers (in-memory index) the writer connection is used.
pub async fn with_index_reader<T, F>(state: &Arc<AppState>, f: F) -> T
where
    T: Send + 'static,
    F: FnOnce(&AppState, &SearchIndex) -> T + Send + 'static,
{
    let permit = state.index_readers.permits.acquire().await.expect("reader permits");
    let st = state.clone();
    let result = tokio::task::spawn_blocking(move || {
        let readers = st.index_readers.current();
        match readers.get() {
            Some(reader) => f(&st, &reader),
            None => {
                let writer = st.index.blocking_lock();
                f(&st, &writer)
            }
        }
    })
    .await
    .expect("search task panicked");
    drop(permit);
    result
}

/// The subset of `ids` (matches of a conditional interaction) the caller may
/// see. A compartment-restricted user must not learn about — let alone get
/// back — another patient's resources through If-None-Exist or a conditional
/// update/delete, so matches outside the compartment don't count.
pub fn visible_matches(
    state: &AppState,
    executor: &SearchExecutor<'_>,
    auth_user: Option<&auth::AuthUser>,
    resource_type: &str,
    ids: Vec<String>,
) -> Result<Vec<String>, String> {
    if !auth_user.is_some_and(|u| u.compartment_restricted(resource_type, 's')) {
        return Ok(ids);
    }
    // In chunks, keeping only ids, so a broad match set isn't held in memory.
    let mut visible = Vec::new();
    for chunk in ids.chunks(500) {
        let resources = executor.load_resources(resource_type, chunk)?;
        visible.extend(
            compartment_check::filter_by_compartment(auth_user, &state.compartment_def, resource_type, resources)
                .iter()
                .filter_map(|r| r.get("id").and_then(|v| v.as_str()).map(str::to_string)),
        );
    }
    // Keep the caller's order (e.g. `_sort`).
    let keep: std::collections::HashSet<&str> = visible.iter().map(String::as_str).collect();
    Ok(ids.iter().filter(|id| keep.contains(id.as_str())).cloned().collect())
}

/// A search by a compartment-restricted user: the page `query` asks for and
/// the total, of the matches they may see. On a v2 index the patient's
/// compartment is one more criterion (`_compartment`, indexed with the same
/// test the access check applies), so the index pages it; a v1 index (only
/// while its replacement is built) reads every match to filter it.
pub fn compartment_search(
    state: &AppState,
    executor: &SearchExecutor<'_>,
    index: &SearchIndex,
    auth_user: Option<&auth::AuthUser>,
    resource_type: &str,
    query: &sazare_core::SearchQuery,
) -> Result<(Vec<String>, usize), String> {
    let Some(patient_id) = auth_user.and_then(|u| u.patient_id.clone()) else {
        return Ok((Vec::new(), 0));
    };
    if !state.compartment_def.is_in_compartment(resource_type) {
        // Not a compartment type: the access check lets every match through.
        return executor.search_with_total(resource_type, query);
    }
    if !index.is_v1() {
        let mut q = query.clone();
        q.parameters.push(sazare_core::SearchParameter {
            name: sazare_store::COMPARTMENT_PARAM.to_string(),
            value: format!("Patient/{patient_id}"),
            modifier: None,
            prefix: None,
            param_type: sazare_core::SearchParamType::Reference,
        });
        return executor.search_with_total(resource_type, &q);
    }
    let mut all = query.clone();
    all.count = None;
    all.offset = None;
    let (ids, _) = executor.search_with_total(resource_type, &all)?;
    let mut ids = visible_matches(state, executor, auth_user, resource_type, ids)?;
    let total = ids.len();
    ids.drain(..query.offset.unwrap_or(0).min(ids.len()));
    if let Some(n) = query.count {
        ids.truncate(n);
    }
    Ok((ids, total))
}

/// Conditional create check
pub async fn conditional_create_check(
    state: &Arc<AppState>,
    auth_user: Option<&auth::AuthUser>,
    resource_type: &str,
    query_string: &str,
) -> ConditionalResult {
    let query = match SearchQuery::parse_with_registry(query_string, Some(resource_type), &state.search_param_registry) {
        Ok(q) => q,
        Err(e) => return ConditionalResult::SearchError(e),
    };

    let index = state.index.lock().await;
    let executor = SearchExecutor::new(&state.store, &index);

    let ids = match executor
        .search(resource_type, &query)
        .and_then(|ids| visible_matches(state, &executor, auth_user, resource_type, ids))
    {
        Ok(ids) => ids,
        Err(e) => return ConditionalResult::SearchError(e),
    };
    match ids.len() {
        0 => ConditionalResult::NoMatch,
        1 => match executor.load_resources(resource_type, &ids) {
            Ok(resources) if !resources.is_empty() => {
                ConditionalResult::Exists(resources.into_iter().next().unwrap())
            }
            _ => ConditionalResult::NoMatch,
        },
        _ => ConditionalResult::MultipleMatches,
    }
}

/// Build the application router with all routes and middleware
pub fn build_router(state: Arc<AppState>) -> Router {
    metrics::mark_start();
    let max_request_bytes = state.config.server.max_request_mb.saturating_mul(1024 * 1024);
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods([
            Method::GET,
            Method::POST,
            Method::PUT,
            Method::PATCH,
            Method::DELETE,
            Method::OPTIONS,
        ])
        .allow_headers(Any);

    // Static apps under /apps/<name>/
    let app_router = apps::app_routes();

    // Optional UI: served from SAZARE_UI_DIR if set and exists
    let ui_dir = std::env::var("SAZARE_UI_DIR").ok().filter(|p| std::path::Path::new(p).is_dir());
    let mut router = app_router;
    if let Some(dir) = ui_dir.as_ref() {
        tracing::info!("UI: serving static files from {} at /ui", dir);
        router = router.nest_service("/ui", ServeDir::new(dir));
    }

    router
        .merge(Router::new()
        // Health check
        .route("/health", get(handlers::metadata::health_check))
        // Prometheus metrics (operators only when auth is enabled)
        .route("/metrics", get(metrics::metrics))
        // Dashboard
        .route("/", get(dashboard::dashboard_page).post(bundle::process_bundle))
        .route("/$status", get(dashboard::status_api))
        // Dashboard browse (Basic auth only when auth is enabled — operators)
        .route("/$browse/{resource_type}", get(dashboard::browse_list))
        .route("/$browse/{resource_type}/{id}", get(dashboard::browse_read))
        // One-click sample data for first-run exploration. Writes data, so it is
        // NOT auth-exempt — when auth is enabled it requires a credential like
        // any other write; in the default (auth-off) local mode it just works.
        .route("/$demo", post(demo::load_demo))
        // Static app listing
        .route("/$apps", get(apps::list_apps))
        // Bulk operations — async Bulk Data IG export (with sync fallback) + import
        .route("/$export", get(bulk_export::export))
        .route("/Patient/$export", get(bulk_export::patient_export))
        .route("/Group/{id}/$export", get(bulk_export::group_export))
        .route(
            "/$export-status/{job_id}",
            get(bulk_export::export_status).delete(bulk_export::export_delete),
        )
        .route("/$export-file/{job_id}/{resource_type}", get(bulk_export::export_file))
        .route("/$import", post(bulk::import))
        // Admin: rebuild search index
        .route("/$reindex", post(handlers::reindex::reindex))
        // Metadata
        .route("/metadata", get(handlers::metadata::capability_statement))
        // SMART on FHIR configuration
        .route("/.well-known/smart-configuration", get(handlers::metadata::smart_configuration))
        .route("/token", post(smart::token))
        .route("/ws", get(websocket::ws_handler))
        // Operations (must be before /{resource_type}/{id} to avoid matching as {id})
        .route("/{resource_type}/$validate", post(handlers::validate::validate))
        .route("/{resource_type}/{id}/$everything", get(handlers::everything::patient_everything))
        // FHIR search-via-POST (alternative to GET search; body is form-encoded params)
        .route("/{resource_type}/_search", post(handlers::search::search_post))
        // CRUD + Search + Conditional
        .route(
            "/{resource_type}",
            get(handlers::search::search)
                .post(handlers::crud::create)
                .put(handlers::conditional::conditional_update)
                .delete(handlers::conditional::conditional_delete),
        )
        .route(
            "/{resource_type}/{id}",
            get(handlers::crud::read)
                .put(handlers::crud::update)
                .patch(handlers::crud::patch_resource)
                .delete(handlers::crud::delete_resource),
        )
        // History
        .route("/{resource_type}/{id}/_history", get(handlers::history::history))
        .route("/{resource_type}/{id}/_history/{vid}", get(handlers::history::vread))
        )
        // Middleware
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            auth::auth_middleware,
        ))
        // Outside auth, so refused requests are counted too.
        .layer(axum::middleware::from_fn(metrics::track))
        // One configurable cap for every request body. axum's extractors also
        // enforce their own 2MB default, which silently overrode this layer
        // (Synthea bundles and $import NDJSON routinely exceed 2MB), so that
        // default is disabled in favour of the explicit limit.
        .layer(RequestBodyLimitLayer::new(max_request_bytes))
        .layer(axum::extract::DefaultBodyLimit::disable())
        .layer(cors)
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}
