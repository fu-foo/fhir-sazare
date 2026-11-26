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

use sazare_server::{apps, build_router, config::{RebuildMode, ServerConfig}, index_generation, AppState};

mod instance;

const USAGE: &str = "\
sazare-server — a lightweight FHIR R4 server

Usage: sazare-server <command> [options]

Commands:
  start     Start the server in the background and return once it is up
  stop      Stop the background server (waits for in-flight writes)
  status    Show whether the server is running, its URL and pid
  run       Run in the foreground (for Docker, systemd, launchd, brew services)
  logs      Print the background server's log (-f to follow, -n <lines>)
  backup    Write a consistent copy of the databases: backup <dir>
  auth      Credentials for config.yaml:
              auth hash-password [user]   prompt for a password, print password_hash
              auth new-api-key [name]     generate a key, print it and its key_sha256
  version   Print the version
  help      Show this help

Options (start, run):
  --config <file>   Configuration file (default: ./config.yaml when present)
  --demo            Load the sample dataset
  --open            Open the dashboard in a browser

Configuration: --config, else $SAZARE_CONFIG, else ./config.yaml, else the user
configuration file (~/.config/sazare/config.yaml; ~/Library/Application Support/
sazare/config.yaml on macOS; %APPDATA%\\sazare\\config.yaml on Windows). Relative paths
in it are relative to the file. Unknown keys are an error.

One server runs per data directory. stop, status, logs and backup find it through
the same configuration.
";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (command, rest) = match args.split_first() {
        Some((c, rest)) => (c.as_str(), rest.to_vec()),
        None => {
            eprint!("{USAGE}");
            std::process::exit(2);
        }
    };
    let code = match command {
        "run" => {
            init_tracing();
            let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
            runtime.block_on(serve(rest));
            0
        }
        "start" => {
            let config = load_config(&rest);
            let open = rest.iter().any(|a| a == "--open");
            let forwarded: Vec<String> = rest.into_iter().filter(|a| a != "--open").collect();
            for note in &config.notes {
                eprintln!("warning: {note}");
            }
            instance::start(&config, &forwarded, open)
        }
        "stop" => instance::stop(&load_config(&rest)),
        "logs" => instance::logs(&load_config(&rest), &rest),
        "auth" => auth_command(&rest),
        "status" => instance::status(&load_config(&rest)),
        "backup" => {
            let dir = rest.iter().enumerate().find(|(i, a)| {
                !a.starts_with("--") && (*i == 0 || rest[i - 1] != "--config")
            });
            let Some((_, dir)) = dir else {
                eprintln!("Usage: sazare-server backup <dir> [--config <file>]");
                std::process::exit(2);
            };
            run_backup(&load_config(&rest), std::path::Path::new(dir))
        }
        "version" | "--version" | "-V" => {
            println!("sazare-server {}", env!("CARGO_PKG_VERSION"));
            0
        }
        "help" | "--help" | "-h" => {
            print!("{USAGE}");
            0
        }
        other => {
            eprintln!("Unknown command `{other}`.\n");
            if other.starts_with("--") {
                eprintln!("Options go after a command, e.g. `sazare-server start {other}`.\n");
            }
            eprint!("{USAGE}");
            2
        }
    };
    std::process::exit(code);
}

fn init_tracing() {
    use std::io::IsTerminal;
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    // A log file the server writes itself, turning it over: set by `start`,
    // or by a service manager that keeps stdout (launchd, Homebrew). `~/` is
    // the home directory (launchd doesn't expand it).
    let log_file = std::env::var(instance::LOG_FILE_ENV).ok().filter(|p| !p.is_empty()).and_then(|p| {
        let path = match (p.strip_prefix("~/"), std::env::var_os("HOME")) {
            (Some(rest), Some(home)) => std::path::PathBuf::from(home).join(rest),
            _ => std::path::PathBuf::from(p),
        };
        match instance::RotatingLog::open(&path) {
            Ok(log) => Some(log),
            Err(e) => {
                eprintln!("Cannot open the log file {}: {e}; logging to stdout", path.display());
                None
            }
        }
    });
    match log_file {
        Some(log) => tracing_subscriber::registry()
            .with(tracing_subscriber::fmt::layer().with_ansi(false).with_writer(log))
            .with(filter)
            .init(),
        // Colors only on a terminal.
        None => tracing_subscriber::registry()
            .with(tracing_subscriber::fmt::layer().with_ansi(std::io::stdout().is_terminal()))
            .with(filter)
            .init(),
    }
}

/// `auth hash-password [user]` / `auth new-api-key [name]`: print the lines to
/// put in config.yaml. Nothing is written; the file keeps its comments.
fn auth_command(args: &[String]) -> i32 {
    use sazare_server::auth::credentials;
    match args.first().map(String::as_str) {
        Some("hash-password") => {
            let user = args.get(1).cloned().unwrap_or_else(|| "admin".into());
            let password = if std::io::IsTerminal::is_terminal(&std::io::stdin()) {
                let a = rpassword::prompt_password(format!("Password for {user}: ")).unwrap_or_default();
                let b = rpassword::prompt_password("Again: ").unwrap_or_default();
                if a != b {
                    eprintln!("The passwords differ.");
                    return 1;
                }
                a
            } else {
                // Piped: the first line of stdin.
                let mut line = String::new();
                let _ = std::io::stdin().read_line(&mut line);
                line.trim_end_matches(['\r', '\n']).to_string()
            };
            if password.len() < 12 {
                eprintln!("Use at least 12 characters.");
                return 1;
            }
            let hash = match credentials::hash_password(&password) {
                Ok(h) => h,
                Err(e) => {
                    eprintln!("Hashing failed: {e}");
                    return 1;
                }
            };
            // JSON strings are valid YAML scalars: quoting survives any name.
            let q = |s: &str| serde_json::to_string(s).unwrap_or_default();
            println!("  basic_auth:");
            println!("    - username: {}", q(&user));
            println!("      password_hash: {}", q(&hash));
            0
        }
        Some("new-api-key") => {
            let name = args.get(1).cloned().unwrap_or_else(|| "client".into());
            let key = credentials::new_api_key();
            eprintln!("API key for {name} (shown once — give it to the client, it is not stored):");
            eprintln!("  {key}");
            eprintln!();
            let q = |s: &str| serde_json::to_string(s).unwrap_or_default();
            println!("  api_keys:");
            println!("    - name: {}", q(&name));
            println!("      key_sha256: {}", q(&credentials::sha256_hex(&key)));
            0
        }
        _ => {
            eprintln!("Usage: sazare-server auth hash-password [user] | auth new-api-key [name]");
            2
        }
    }
}

/// The configuration to run with (see `ServerConfig::load`): `--config <path>`
/// / `--config=<path>`, `SAZARE_CONFIG`, `./config.yaml`, the user's file.
fn load_config(args: &[String]) -> ServerConfig {
    let explicit_config = args.iter().enumerate().find_map(|(i, a)| {
        a.strip_prefix("--config=")
            .map(str::to_string)
            .or_else(|| (a == "--config").then(|| args.get(i + 1).cloned()).flatten())
    });
    if args.iter().any(|a| a == "--config") && explicit_config.is_none() {
        eprintln!("--config requires a path");
        std::process::exit(2);
    }
    // A configuration that doesn't load stops here: running on defaults
    // instead (port, data_dir, auth off) would be a surprise.
    ServerConfig::load(explicit_config.as_deref()).unwrap_or_else(|e| {
        eprintln!("Invalid configuration: {e}");
        std::process::exit(1);
    })
}

/// `run`: the server in the foreground.
async fn serve(args: Vec<String>) {

    let want_demo = args.iter().any(|a| a == "--demo");
    let want_open = args.iter().any(|a| a == "--open");
    let config = load_config(&args);

    // One server per data directory: a second one on the same databases would
    // break the single-writer assumptions. Taken before anything is opened.
    let paths = instance::Paths::new(&config);
    let _instance_lock = match instance::try_lock(&paths.lock) {
        Ok(Some(lock)) => lock,
        Ok(None) => {
            eprintln!(
                "Another sazare server is already running on {} (see `sazare-server status`).",
                config.storage.data_dir.display()
            );
            std::process::exit(1);
        }
        Err(e) => {
            eprintln!("Cannot lock {}: {e}", paths.lock.display());
            std::process::exit(1);
        }
    };
    // Run info left by a server that crashed names a pid and control port that
    // aren't this server's: drop it now, so `stop` during startup can't act on
    // them (it reports "still starting" until this server publishes its own).
    let _ = std::fs::remove_file(&paths.run);
    tracing::info!("Starting fhir-sazare server...");
    tracing::info!(
        "Config: {}",
        config.source.as_ref().map(|p| p.display().to_string()).unwrap_or_else(|| "none (defaults)".into())
    );
    tracing::info!("Data:   {}", config.storage.data_dir.display());
    tracing::info!("IG:     {} (profiles/, searchparameters/, terminology/)", config.ig_dir().display());
    for note in &config.notes {
        tracing::warn!("{note}");
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

    // The newest complete index generation (unfinished ones are removed: a
    // build that didn't finish restarts below).
    let index_path = index_generation::current(&config.search_index_db_path(), true);
    let index = SearchIndex::open(&index_path).unwrap_or_else(|e| {
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
    match ProfileLoader::load_from_directory(config.ig_dir().join("profiles")) {
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
    match ProfileLoader::load_resources_from_directory(config.ig_dir().join("searchparameters"), "SearchParameter") {
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
    match ProfileLoader::load_terminology_directory(config.ig_dir().join("terminology"), &mut terminology_registry) {
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
    // Existence checks only: counting the rows of a large index takes minutes.
    let text_missing = index.has_text().map(|has| !has).unwrap_or(false);
    // Rebuild too when the search parameter definitions changed since the
    // index was built (new built-ins after an upgrade, an edited
    // searchparameters/ directory) — otherwise existing data stays invisible
    // to the new parameters until someone runs $reindex.
    let fingerprint = sazare_server::handlers::reindex::index_fingerprint(&search_param_registry);
    let params_changed = index
        .get_meta(sazare_server::handlers::reindex::FINGERPRINT_KEY)
        .ok()
        .flatten()
        .is_none_or(|stored| stored != fingerprint);
    // Whether the index must be rebuilt, and why. The rebuild itself runs once
    // the server is up (index.rebuild: background), before it (blocking), or
    // on POST /$reindex (manual).
    // A v1 index (sazare 0.15 and before) is always replaced, even with no
    // resources: it keeps serving searches until the v2 index takes over.
    let v1 = index.is_v1();
    let rebuild_reason: Option<&'static str> = match index.is_empty() {
        Ok(empty) if empty || text_missing || params_changed || v1 => {
            // An existence check, not a full load of every resource.
            if v1 {
                Some("the index is in the v1 format")
            } else if store.has_any().unwrap_or(false) {
                Some(if empty {
                    "the index is empty"
                } else if text_missing {
                    "the index has no full text yet"
                } else {
                    "the search parameters or the index format changed"
                })
            } else {
                let _ = index.set_meta(sazare_server::handlers::reindex::FINGERPRINT_KEY, &fingerprint);
                None
            }
        }
        Ok(_) => {
            tracing::info!("Search index is up to date");
            None
        }
        Err(e) => {
            tracing::warn!("Failed to inspect the search index: {}", e);
            None
        }
    };

    let bind_addr = format!("{}:{}", config.server.host, config.server.port);
    let loopback = matches!(config.server.host.as_str(), "127.0.0.1" | "localhost" | "::1");
    if !config.auth.enabled && !loopback {
        tracing::warn!(
            "Listening on {} with authentication DISABLED: anyone who can reach this \
             address can read and change all data. Set auth.enabled: true, or bind \
             to 127.0.0.1 (server.host / SAZARE_HOST).",
            bind_addr
        );
    }

    let app_names = apps::discover_app_names(&config);

    // Read-only index connections for searches (after any startup rebuild, so
    // they open the finished index).
    let readers = std::thread::available_parallelism().map(|n| n.get().clamp(2, 8)).unwrap_or(4);
    let index_readers = sazare_store::IndexReaders::open(&index_path, readers)
        .unwrap_or_else(|e| {
            tracing::warn!("Search index readers unavailable ({e}); searches share the writer connection");
            sazare_store::IndexReaders::none()
        });
    let state = Arc::new(AppState {
        store,
        index: Mutex::new(index),
        index_readers: sazare_server::Readers::new(index_readers),
        index_file: std::sync::RwLock::new(index_path.clone()),
        write_gate: Default::default(),
        rebuild: Default::default(),
        audit: Arc::new(Mutex::new(audit_log)),
        config: config.clone(),
        profile_registry,
        terminology_registry,
        search_param_registry,
        compartment_def: CompartmentDef::patient_compartment(),
        jwk_cache: tokio::sync::RwLock::new(sazare_server::auth::JwkCache::new()),
        app_names,
        ws_registry: Arc::new(sazare_server::websocket::WsRegistry::new()),
        webhook: Arc::new(sazare_server::webhook::WebhookManager::new(
            config.webhook.clone(),
        )),
        export_jobs: Arc::new(sazare_server::bulk_export::ExportJobs::in_dir(config.storage.data_dir.join("exports"))),
        seen_jti: std::sync::Mutex::new(std::collections::HashMap::new()),
    });

    // Put back what a crash cut in two between the store and the index,
    // before anything reads or writes; then keep recording where they match.
    if let Err(e) = sazare_server::consistency::catch_up(&state).await {
        tracing::error!("Search index: catching up with the store failed: {e} (POST /$reindex rebuilds it)");
    }
    sazare_server::consistency::spawn_marker(state.clone());

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

    if state.app_names.is_empty() {
        tracing::info!("Apps: none (set apps.dir to serve static apps under /apps/)");
    } else {
        tracing::info!(
            "Apps: {} → /apps/{}/",
            state.app_names.len(),
            state.app_names.join("/, /apps/")
        );
    }

    if let Some(reason) = rebuild_reason {
        match config.index.rebuild {
            RebuildMode::Background => {
                tracing::warn!(
                    "Search index needs rebuilding ({reason}); serving from the current index while a new \
                     one is built in the background — searches may be incomplete until it is done \
                     (progress: GET /health)"
                );
                let st = state.clone();
                tokio::spawn(async move {
                    let _ = index_generation::rebuild(st, reason).await;
                });
            }
            RebuildMode::Blocking => {
                tracing::info!("Search index needs rebuilding ({reason}); rebuilding before serving");
                if let Err(e) = index_generation::rebuild(state.clone(), reason).await {
                    tracing::error!("Index rebuild failed: {e}");
                }
            }
            RebuildMode::Manual => tracing::warn!(
                "Search index needs rebuilding ({reason}); index.rebuild is manual — run POST /$reindex"
            ),
        }
    }

    // Build router
    let rebuild = state.clone();
    let app = build_router(state);

    // Bind TCP listener
    let listener = match tokio::net::TcpListener::bind(&bind_addr).await {
        Ok(l) => l,
        Err(e) => {
            tracing::error!("Failed to bind to {}: {}", bind_addr, e);
            std::process::exit(1);
        }
    };

    // The URL clients on this machine use: a loopback host (0.0.0.0 isn't a
    // connectable address) and the right scheme.
    let local_url = {
        let scheme = if config.server.tls.is_some() { "https" } else { "http" };
        let host = if config.server.host == "0.0.0.0" || config.server.host.is_empty() {
            "127.0.0.1".to_string()
        } else {
            config.server.host.clone()
        };
        format!("{scheme}://{host}:{}", config.server.port)
    };

    // Control port for `sazare-server stop`, and the run info `start`, `stop`
    // and `status` read. Published only now that the port is bound.
    let stop_requested = Arc::new(tokio::sync::Notify::new());
    let token = instance::new_token();
    let control = match instance::serve_control(token.clone(), stop_requested.clone()).await {
        Ok(addr) => addr,
        Err(e) => {
            tracing::error!("Failed to open the control port: {e}");
            std::process::exit(1);
        }
    };
    let _run_info = instance::publish_run_info(
        &paths,
        &instance::RunInfo {
            pid: std::process::id(),
            url: local_url.clone(),
            control: control.to_string(),
            token,
            version: env!("CARGO_PKG_VERSION").to_string(),
            started: chrono::Utc::now().to_rfc3339(),
        },
    )
    .unwrap_or_else(|e| {
        tracing::error!("Cannot write {}: {e}", paths.run.display());
        std::process::exit(1);
    });

    // `--open`: launch the dashboard in the user's browser.
    if want_open {
        open_browser(&local_url);
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
        .with_graceful_shutdown(shutdown_signal(stop_requested.clone()))
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
        .with_graceful_shutdown(shutdown_signal(stop_requested.clone()))
        .await
        .unwrap_or_else(|e| {
            tracing::error!("Server error: {}", e);
        });
    }

    // A background index build would otherwise keep the process alive until
    // it is done (an hour at 10M resources): stop it at its next page, and
    // wait for it to clean up (delete the partial index, log) while the
    // runtime still runs it.
    rebuild.rebuild.cancel();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while rebuild.rebuild.is_running() && std::time::Instant::now() < deadline {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    // Stopped cleanly: the next start has nothing to catch up on.
    if let Err(e) = sazare_server::consistency::mark(&rebuild).await {
        tracing::warn!("Search index: recording the point it matches the store failed: {e}");
    }
    tracing::info!("Server shut down gracefully");
}

/// Best-effort: open `url` in the platform's default browser. Failures are
/// non-fatal (the user can always open the URL printed in the log).
pub(crate) fn open_browser(url: &str) {
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
async fn shutdown_signal(stop_requested: Arc<tokio::sync::Notify>) {
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
        _ = stop_requested.notified() => tracing::info!("Shutting down..."),
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
        // The index generation in use (derived data; a restore rebuilds it anyway).
        index_generation::current(&config.search_index_db_path(), false),
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
