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
