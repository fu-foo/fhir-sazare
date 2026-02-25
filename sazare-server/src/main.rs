//! fhir-sazare - Lightweight FHIR Server entry point

use sazare_core::{
    profile_loader::ProfileLoader,
    validation::{ProfileRegistry, TerminologyRegistry},
    CompartmentDef, SearchParamRegistry,
};
use sazare_store::{AuditLog, SearchIndex, SqliteStore};
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

use sazare_server::{build_router, config::ServerConfig, plugins, AppState};

#[tokio::main]
async fn main() {
    // Initialize logging
    tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer())
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    tracing::info!("Starting fhir-sazare server...");

    // Load configuration
    let config = ServerConfig::load(
        std::path::Path::new("config.yaml")
            .exists()
            .then_some("config.yaml"),
    )
    .unwrap_or_else(|e| {
        tracing::warn!("Failed to load config, using defaults: {}", e);
        ServerConfig::default()
    });

    // Create data directory
    if let Err(e) = std::fs::create_dir_all(&config.storage.data_dir) {
        tracing::error!("Failed to create data directory: {}", e);
        std::process::exit(1);
    }

    // Initialize stores
    let store = SqliteStore::open(config.resources_db_path()).unwrap_or_else(|e| {
        tracing::error!("Failed to open resource store: {}", e);
        std::process::exit(1);
    });

    let index = SearchIndex::open(config.search_index_db_path()).unwrap_or_else(|e| {
        tracing::error!("Failed to open search index: {}", e);
        std::process::exit(1);
    });

    let audit_log = AuditLog::open(config.audit_db_path()).unwrap_or_else(|e| {
        tracing::error!("Failed to open audit log: {}", e);
        std::process::exit(1);
    });

    // Load profiles
    let mut profile_registry = ProfileRegistry::new();
    let us_core_profiles = ProfileLoader::get_embedded_us_core_profiles();
    profile_registry.load_profiles(us_core_profiles);

    // Load custom profiles from profiles/ directory if it exists
    match ProfileLoader::load_from_directory("profiles") {
        Ok(custom_profiles) if !custom_profiles.is_empty() => {
            tracing::info!("Loading {} custom profiles", custom_profiles.len());
            profile_registry.load_profiles(custom_profiles);
        }
        Ok(_) => {}
        Err(e) => {
            tracing::warn!("Failed to load custom profiles: {}", e);
        }
    }

    let bind_addr = format!("{}:{}", config.server.host, config.server.port);

    let plugin_names = plugins::discover_plugin_names(&config);

    let state = Arc::new(AppState {
        store,
        index: Mutex::new(index),
        audit: Arc::new(Mutex::new(audit_log)),
        config: config.clone(),
        profile_registry,
        terminology_registry: TerminologyRegistry::new(),
        search_param_registry: SearchParamRegistry::new(),
        compartment_def: CompartmentDef::patient_compartment(),
        jwk_cache: tokio::sync::RwLock::new(sazare_server::auth::JwkCache::new()),
        plugin_names,
    });

    tracing::info!(
        "Auth: {}",
        if config.auth.enabled {
            "enabled"
        } else {
            "disabled"
        }
    );

    if state.plugin_names.is_empty() {
        tracing::info!("Plugins: disabled (no plugin directory found)");
    } else {
        tracing::info!(
            "Plugins: {} plugin(s) → /{}",
            state.plugin_names.len(),
            state.plugin_names.join("/, /")
        );
    }

    // Build router
    let app = build_router(state);

    // Bind TCP listener
    let listener = match tokio::net::TcpListener::bind(&bind_addr).await {
        Ok(l) => l,
        Err(e) => {
            tracing::error!("Failed to bind to {}: {}", bind_addr, e);
            std::process::exit(1);
        }
    };

    // Start server (HTTPS or HTTP)
    if let Some(ref tls_config) = config.server.tls {
        let acceptor = sazare_server::tls::load_tls_acceptor(
            &tls_config.cert_file,
            &tls_config.key_file,
        )
        .unwrap_or_else(|e| {
            tracing::error!("Failed to load TLS config: {}", e);
            std::process::exit(1);
        });

        tracing::info!("Listening on https://{}", bind_addr);

        let tls_listener = sazare_server::tls::TlsListener::new(listener, acceptor);
        axum::serve(tls_listener, app.into_make_service())
            .with_graceful_shutdown(shutdown_signal())
            .await
            .unwrap_or_else(|e| {
                tracing::error!("Server error: {}", e);
            });
    } else {
        tracing::info!("Listening on http://{}", bind_addr);

        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .with_graceful_shutdown(shutdown_signal())
        .await
        .unwrap_or_else(|e| {
            tracing::error!("Server error: {}", e);
        });
    }

    tracing::info!("Server shut down gracefully");
}

/// Graceful shutdown signal handler
async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("Failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("Failed to install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => tracing::info!("Received Ctrl+C, shutting down..."),
        _ = terminate => tracing::info!("Received SIGTERM, shutting down..."),
    }
}
