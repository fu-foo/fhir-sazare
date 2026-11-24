//! fhir-sazare - Lightweight FHIR Server entry point

use sazare_core::{
    profile_loader::ProfileLoader,
    validation::{DefaultProfileMode, ProfileRegistry, TerminologyRegistry},
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

    // `--config <path>` / `--config=<path>` selects the config file; otherwise
    // `./config.yaml` is used when present.
    let explicit_config = args.iter().enumerate().find_map(|(i, a)| {
        a.strip_prefix("--config=")
            .map(str::to_string)
            .or_else(|| (a == "--config").then(|| args.get(i + 1).cloned()).flatten())
    });
    if args.iter().any(|a| a == "--config") && explicit_config.is_none() {
        eprintln!("--config requires a path");
        std::process::exit(2);
    }

    // Load configuration
    let config = match explicit_config.as_deref() {
        // An explicitly requested file must load — silently running on
        // defaults (port, data_dir, auth off) would be a surprise.
        Some(path) => ServerConfig::load(Some(path)).unwrap_or_else(|e| {
            eprintln!("Failed to load config {path}: {e}");
            std::process::exit(1);
        }),
        None => ServerConfig::load(
            std::path::Path::new("config.yaml")
                .exists()
                .then_some("config.yaml"),
        )
        .unwrap_or_else(|e| {
            tracing::warn!("Failed to load config, using defaults: {}", e);
            ServerConfig::default()
        }),
    };

    // `--backup <dir>`: snapshot the databases and exit (works while another
    // instance is serving the same data_dir).
    if let Some(i) = args.iter().position(|a| a == "--backup") {
        let Some(dir) = args.get(i + 1) else {
            eprintln!("--backup requires a target directory");
            std::process::exit(2);
        };
        std::process::exit(run_backup(&config, std::path::Path::new(dir)));
    }

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

    // Default profiles: validate resources that declare no meta.profile
    // against a configured profile (e.g. JP Core's JP_Patient). A profile that
    // isn't loaded is a configuration error — fail loudly rather than silently
    // skipping the validation the operator asked for.
    let mode = match config.validation.default_profile_mode {
        sazare_server::config::DefaultProfileModeSetting::Enforce => DefaultProfileMode::Enforce,
        sazare_server::config::DefaultProfileModeSetting::Warn => DefaultProfileMode::Warn,
    };
    for (resource_type, url) in &config.validation.default_profiles {
        if let Err(e) = profile_registry.set_default_profile(resource_type, url, mode) {
            tracing::error!("Invalid validation.default_profiles: {}", e);
            std::process::exit(1);
        }
        tracing::info!("Default profile for {}: {} ({:?})", resource_type, url, mode);
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

    // Load IG terminology from terminology/ if it exists (CodeSystems and
    // ValueSets, e.g. JP Core's package plus jpfhir-terminology). Only fully
    // resolvable ValueSets are registered; bindings to the rest stay unchecked.
    let mut terminology_registry = TerminologyRegistry::new();
    match ProfileLoader::load_terminology_directory("terminology", &mut terminology_registry) {
        Ok(Some(s)) => tracing::info!(
            "Terminology: {} code systems, {} value sets loaded ({} value sets not fully resolvable, left unchecked)",
            s.code_systems,
            s.value_sets,
            s.value_sets_skipped
        ),
        Ok(None) => {}
        Err(e) => tracing::warn!("Failed to load terminology: {}", e),
    }

    // Auto-reindex if the search index is empty (fresh deploy, or after an index wipe
    // following a schema change like added common params _id/_profile/_tag/etc.)
    // Also rebuild once when the full-text table is empty but the index
    // isn't — an index built before `_text`/`_content` existed.
    let text_missing = index.text_row_count().map(|n| n == 0).unwrap_or(false);
    match index.row_count() {
        Ok(n) if n == 0 || text_missing => {
            // An existence check, not a full load of every resource.
            let store_has_data = store.has_any().unwrap_or(false);
            if store_has_data {
                tracing::info!(
                    "Search index {}; rebuilding from resource store...",
                    if n == 0 { "is empty" } else { "has no full text yet (_text/_content)" }
                );
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
        terminology_registry,
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

/// Back up every database under `data_dir` into `dir` (created if missing;
/// existing backup files are never overwritten). Returns the process exit code.
fn run_backup(config: &ServerConfig, dir: &std::path::Path) -> i32 {
    if let Err(e) = std::fs::create_dir_all(dir) {
        eprintln!("Cannot create backup directory {}: {e}", dir.display());
        return 1;
    }
    let dbs = [
        config.resources_db_path(),
        config.search_index_db_path(),
        config.audit_db_path(),
    ];
    for src in &dbs {
        let Some(name) = src.file_name() else { continue };
        let dest = dir.join(name);
        if !src.exists() {
            eprintln!("skip {} (not found)", src.display());
            continue;
        }
        if dest.exists() {
            eprintln!("Refusing to overwrite existing {}", dest.display());
            return 1;
        }
        match sazare_store::backup_database(src, &dest) {
            Ok(()) => println!("{} -> {}", src.display(), dest.display()),
            Err(e) => {
                eprintln!("Backup of {} failed: {e}", src.display());
                return 1;
            }
        }
    }
    0
}
