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

use sazare_server::{build_router, config::ServerConfig, handlers::reindex::perform_reindex, plugins, AppState};

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

    // Minimal flags: `--demo` pre-loads the sample dataset, `--open` opens the
    // dashboard in a browser once the server is listening.
    let args: Vec<String> = std::env::args().collect();
    let want_demo = args.iter().any(|a| a == "--demo");
    let want_open = args.iter().any(|a| a == "--open");

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

    // Load custom profiles from profiles/ directory if it exists (this is how
    // JP Core or any other IG is supplied now — drop the package's profiles in).
    // Validation against US Core remains the only built-in conformance claim.
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

    // Load custom search parameters from searchparameters/ if it exists. Each is
    // a FHIR SearchParameter resource whose `expression` is compiled by the
    // bounded FHIRPath evaluator; ones outside the supported subset are rejected
    // loudly here rather than producing wrong results later. This is how JP Core
    // (or any IG) search params are supplied now — drop them in alongside the
    // matching profiles in profiles/.
    let mut search_param_registry = SearchParamRegistry::new();
    match ProfileLoader::load_resources_from_directory("searchparameters", "SearchParameter") {
        Ok(sps) => {
            for sp in &sps {
                match search_param_registry.register_search_parameter(sp) {
                    Ok(()) => {}
                    Err(e) => tracing::warn!("Skipping custom search parameter: {}", e),
                }
            }
        }
        Err(e) => tracing::warn!("Failed to load custom search parameters: {}", e),
    }

    // Auto-reindex if the search index is empty (fresh deploy, or after an index wipe
    // following a schema change like added common params _id/_profile/_tag/etc.)
    match index.row_count() {
        Ok(0) => {
            let store_has_data = store
                .list_all(None)
                .map(|v| !v.is_empty())
                .unwrap_or(false);
            if store_has_data {
                tracing::info!("Search index is empty; rebuilding from resource store...");
                match perform_reindex(&store, &index, &search_param_registry) {
                    Ok(s) => tracing::info!(
                        "Auto-reindex complete: {} resources, {} entries",
                        s.resources_indexed,
                        s.entries_written
                    ),
                    Err(e) => tracing::error!("Auto-reindex failed: {}", e),
                }
            }
        }
        Ok(n) => tracing::info!("Search index has {} entries", n),
        Err(e) => tracing::warn!("Failed to query search index size: {}", e),
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
        search_param_registry,
        compartment_def: CompartmentDef::patient_compartment(),
        jwk_cache: tokio::sync::RwLock::new(sazare_server::auth::JwkCache::new()),
        plugin_names,
        ws_registry: Arc::new(sazare_server::websocket::WsRegistry::new()),
        webhook: Arc::new(sazare_server::webhook::WebhookManager::new(
            config.webhook.clone(),
        )),
        export_jobs: Arc::new(sazare_server::bulk_export::ExportJobs::new()),
        seen_jti: std::sync::Mutex::new(std::collections::HashMap::new()),
    });

    // `--demo`: load the curated sample dataset so a fresh run has something to
    // explore immediately.
    if want_demo {
        match sazare_server::demo::load_demo_into(&state).await {
            Ok((n, errors)) => {
                tracing::info!("Demo data: {} sample resources loaded", n);
                for e in errors {
                    tracing::warn!("Demo data: {}", e);
                }
            }
            Err(e) => tracing::warn!("Demo data failed to load: {}", e),
        }
    }

    // `SAZARE_SEED_ON_EMPTY=<file>`: on a fresh (empty) store, load an external
    // dataset (Bundle or array) so `docker run` / a bare binary comes up already
    // populated, without baking the data into the binary.
    if let Ok(seed_path) = std::env::var("SAZARE_SEED_ON_EMPTY") {
        match sazare_server::demo::seed_from_file_if_empty(&state, &seed_path).await {
            Ok(Some((n, errors))) => {
                tracing::info!("Seed-on-empty: loaded {} resources from {}", n, seed_path);
                for e in errors {
                    tracing::warn!("Seed-on-empty: {}", e);
                }
            }
            Ok(None) => tracing::info!("Seed-on-empty: store not empty, skipping {}", seed_path),
            Err(e) => tracing::warn!("Seed-on-empty from {} failed: {}", seed_path, e),
        }
    }

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

    // `--open`: launch the dashboard in the user's browser. Use a loopback host
    // (0.0.0.0 isn't a connectable address) and the right scheme.
    if want_open {
        let scheme = if config.server.tls.is_some() { "https" } else { "http" };
        let host = if config.server.host == "0.0.0.0" || config.server.host.is_empty() {
            "127.0.0.1".to_string()
        } else {
            config.server.host.clone()
        };
        open_browser(&format!("{scheme}://{host}:{}", config.server.port));
    }

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
        let app = app.layer(axum::middleware::from_fn(
            sazare_server::tls::propagate_connect_info,
        ));
        axum::serve(
            tls_listener,
            app.into_make_service_with_connect_info::<sazare_server::tls::TlsConnectInfo>(),
        )
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

/// Best-effort: open `url` in the platform's default browser. Failures are
/// non-fatal (the user can always open the URL printed in the log).
fn open_browser(url: &str) {
    #[cfg(target_os = "macos")]
    let cmd = ("open", vec![url]);
    #[cfg(target_os = "windows")]
    let cmd = ("cmd", vec!["/C", "start", url]);
    #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
    let cmd = ("xdg-open", vec![url]);

    match std::process::Command::new(cmd.0).args(&cmd.1).spawn() {
        Ok(_) => tracing::info!("Opening {} in your browser…", url),
        Err(e) => tracing::warn!("Couldn't open a browser ({}). Open {} manually.", e, url),
    }
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
