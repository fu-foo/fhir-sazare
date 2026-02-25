//! fhir-sazare - Lightweight FHIR Server
//!
//! A portable FHIR R4 server with JP-Core support.

pub mod audit;
pub mod auth;
pub mod bulk;
pub mod bundle;
pub mod compartment_check;
pub mod config;
pub mod dashboard;
pub mod handlers;
pub mod plugins;
pub mod subscription;
pub mod tls;
#[allow(dead_code)]
pub mod webhook;

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
    /// Discovered plugin names (for auth bypass and routing)
    pub plugin_names: Vec<String>,
}

/// Conditional create result
pub enum ConditionalResult {
    NoMatch,
    Exists(Value),
    MultipleMatches,
    SearchError(String),
}

/// Conditional create check
pub async fn conditional_create_check(
    state: &Arc<AppState>,
    resource_type: &str,
    query_string: &str,
) -> ConditionalResult {
    let query = match SearchQuery::parse(query_string) {
        Ok(q) => q,
        Err(e) => return ConditionalResult::SearchError(e),
    };

    let index = state.index.lock().await;
    let executor = SearchExecutor::new(&state.store, &index);

    match executor.search(resource_type, &query) {
        Ok(ids) if ids.is_empty() => ConditionalResult::NoMatch,
        Ok(ids) if ids.len() == 1 => {
            match executor.load_resources(resource_type, &ids) {
                Ok(resources) if !resources.is_empty() => {
                    ConditionalResult::Exists(resources.into_iter().next().unwrap())
                }
                _ => ConditionalResult::NoMatch,
            }
        }
        Ok(_) => ConditionalResult::MultipleMatches,
        Err(e) => ConditionalResult::SearchError(e),
    }
}

/// Build the application router with all routes and middleware
pub fn build_router(state: Arc<AppState>) -> Router {
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

    // Build plugin routes (explicit per-plugin paths, e.g. /sample-patient-register/)
    let plugin_router = plugins::plugin_routes(&state);

    plugin_router
        .merge(Router::new()
        // Health check
        .route("/health", get(handlers::metadata::health_check))
        // Dashboard
        .route("/", get(dashboard::dashboard_page).post(bundle::process_bundle))
        .route("/$status", get(dashboard::status_api))
        // Dashboard browse (auth-free)
        .route("/$browse/{resource_type}", get(dashboard::browse_list))
        .route("/$browse/{resource_type}/{id}", get(dashboard::browse_read))
        // Plugin listing
        .route("/$plugins", get(plugins::list_plugins))
        // Bulk operations
        .route("/$export", get(bulk::export))
        .route("/$import", post(bulk::import))
        // Metadata
        .route("/metadata", get(handlers::metadata::capability_statement))
        // SMART on FHIR configuration
        .route("/.well-known/smart-configuration", get(handlers::metadata::smart_configuration))
        // Operations (must be before /{resource_type}/{id} to avoid matching as {id})
        .route("/{resource_type}/$validate", post(handlers::validate::validate))
        .route("/{resource_type}/{id}/$everything", get(handlers::everything::patient_everything))
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
        .layer(RequestBodyLimitLayer::new(16 * 1024 * 1024)) // 16MB
        .layer(cors)
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}
