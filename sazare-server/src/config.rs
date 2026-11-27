use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Server configuration loaded from YAML file
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[serde(default)]
pub struct ServerConfig {
    pub server: ServerSettings,
    pub auth: AuthSettings,
    pub storage: StorageSettings,
    pub log: LogSettings,
    pub webhook: WebhookSettings,
    pub apps: AppsSettings,
    /// Where an implementation guide's `profiles/`, `searchparameters/` and
    /// `terminology/` directories are (see `ig_dir`).
    pub ig: IgSettings,
    /// Search-index maintenance.
    pub index: IndexSettings,
    pub validation: ValidationSettings,
    pub subscription: SubscriptionSettings,
    pub audit: AuditSettings,
    /// The configuration file this was loaded from, if any (absolute).
    #[serde(skip)]
    pub source: Option<PathBuf>,
    /// Things worth a warning at startup (legacy locations, plaintext
    /// credentials), collected while loading.
    #[serde(skip)]
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[serde(default)]
pub struct AuditSettings {
    /// Delete audit entries older than this many days (checked at startup
    /// and hourly). Unset: keep every entry.
    pub retention_days: Option<u32>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[serde(default)]
pub struct SubscriptionSettings {
    /// Allow rest-hook endpoints on loopback / private / link-local addresses
    /// (localhost, 10.x, 192.168.x, 169.254.x, fc00::/7, …). Off by default
    /// when auth is enabled, so a client that may create Subscriptions can't
    /// make the server send requests into its own network. With auth
    /// disabled (local development) private endpoints are always allowed.
    pub allow_private_endpoints: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[serde(default)]
pub struct ServerSettings {
    pub host: String,
    pub port: u16,
    pub tls: Option<TlsSettings>,
    /// Maximum request body size in MiB (bundles, `$import` NDJSON, ...).
    /// Larger requests are rejected with 413.
    pub max_request_mb: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TlsSettings {
    pub cert_file: String,
    pub key_file: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[serde(default)]
pub struct AuthSettings {
    pub enabled: bool,
    pub api_keys: Vec<ApiKey>,
    pub basic_auth: Vec<BasicAuthUser>,
    pub jwt: Option<JwtSettings>,
    /// SMART Backend Services (server-to-server) token issuance.
    pub smart: Option<SmartSettings>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SmartSettings {
    /// Absolute URL of this server's token endpoint; the value a client
    /// assertion's `aud` must match. Defaults to `<issuer>/token` if unset.
    #[serde(default)]
    pub token_endpoint: Option<String>,
    /// Access-token lifetime in seconds (default 300).
    #[serde(default)]
    pub token_ttl_secs: Option<u64>,
    /// Registered backend-services clients allowed to obtain tokens.
    #[serde(default)]
    pub backend_clients: Vec<BackendClient>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BackendClient {
    /// The `client_id` (also the assertion's `iss`/`sub`).
    pub client_id: String,
    /// URL serving the client's public JWKS (to verify its assertion).
    #[serde(default)]
    pub jwks_url: Option<String>,
    /// Inline public JWKS, as an alternative to `jwks_url`.
    #[serde(default)]
    pub jwks: Option<serde_json::Value>,
    /// Scopes this client may be granted (space- or comma-separated list of
    /// allowed scope patterns). If empty, none is granted.
    #[serde(default)]
    pub allowed_scopes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JwtSettings {
    pub issuer: Option<String>,
    pub audience: Option<String>,
    pub secret: Option<String>,
    pub public_key_file: Option<String>,
    /// JWKS endpoint URL for fetching public keys from an external IdP (e.g. Keycloak).
    /// Example: "https://keycloak.example.com/realms/myrealm/protocol/openid-connect/certs"
    pub jwk_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApiKey {
    pub name: String,
    /// The key itself (plaintext; discouraged — use `key_sha256`).
    #[serde(default)]
    pub key: Option<String>,
    /// Hex SHA-256 of the key, as printed by `sazare-server auth new-api-key`.
    #[serde(default)]
    pub key_sha256: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BasicAuthUser {
    pub username: String,
    /// The password itself (plaintext; discouraged — use `password_hash`).
    #[serde(default)]
    pub password: Option<String>,
    /// Argon2id hash, as printed by `sazare-server auth hash-password`.
    #[serde(default)]
    pub password_hash: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[serde(default)]
pub struct StorageSettings {
    pub data_dir: PathBuf,
    pub resources_db: String,
    pub search_index_db: String,
    pub audit_db: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[serde(default)]
pub struct LogSettings {
    pub level: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[serde(default)]
pub struct WebhookSettings {
    pub enabled: bool,
    pub endpoints: Vec<WebhookEndpoint>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WebhookEndpoint {
    pub url: String,
    pub events: Vec<String>,
    #[serde(default)]
    pub headers: std::collections::HashMap<String, String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[serde(default)]
pub struct ValidationSettings {
    /// Resource type → profile URL applied to resources of that type that
    /// declare no `meta.profile` (e.g. `Patient: http://jpfhir.jp/fhir/core/StructureDefinition/JP_Patient`).
    pub default_profiles: std::collections::BTreeMap<String, String>,
    /// `enforce` (default): reject non-conforming resources, as if they had
    /// declared the profile. `warn`: accept them and report the violations as
    /// warnings (`$validate`, server log) — useful while migrating data.
    pub default_profile_mode: DefaultProfileModeSetting,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DefaultProfileModeSetting {
    #[default]
    Enforce,
    Warn,
}

/// Static app hosting under `/apps/<name>/` (see `apps.rs`). Off unless `dir`
/// is set.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[serde(default)]
pub struct AppsSettings {
    pub dir: Option<PathBuf>,
}

/// How the search index is rebuilt when it has to be (new search parameters,
/// a new index format, a missing index).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[serde(default)]
pub struct IndexSettings {
    pub rebuild: RebuildMode,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RebuildMode {
    /// Serve from the current index while a new one is built beside it, then
    /// switch (default).
    #[default]
    Background,
    /// Build before accepting requests.
    Blocking,
    /// Only on `POST /$reindex`; until then searches use the stale index.
    Manual,
}

/// Implementation-guide directories.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[serde(default)]
pub struct IgSettings {
    /// Directory holding `profiles/`, `searchparameters/`, `terminology/`.
    /// Default: the directory of the configuration file.
    pub dir: Option<PathBuf>,
}

impl Default for ServerSettings {
    fn default() -> Self {
        Self {
            // Loopback by default: an unconfigured server (auth off) must not
            // be reachable from the network. Docker sets SAZARE_HOST=0.0.0.0.
            host: "127.0.0.1".to_string(),
            port: 8080,
            tls: None,
            max_request_mb: 64,
        }
    }
}

impl Default for StorageSettings {
    fn default() -> Self {
        Self {
            data_dir: PathBuf::from("data"),
            resources_db: "resources.sqlite".to_string(),
            search_index_db: "search_index.sqlite".to_string(),
            audit_db: "audit.sqlite".to_string(),
        }
    }
}

impl Default for LogSettings {
    fn default() -> Self {
        Self {
            level: "info".to_string(),
        }
    }
}

impl ServerConfig {
    /// Load configuration from a YAML file (as written: no path resolution,
    /// no environment overrides).
    pub fn load_from_file(path: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let content = std::fs::read_to_string(path)?;
        Ok(serde_yaml::from_str(&content)?)
    }

    /// Load the configuration the server runs with.
    ///
    /// The file is the first of: `explicit` (`--config`), `SAZARE_CONFIG`,
    /// `./config.yaml`, the user's configuration file ([`user_config_file`]);
    /// none is fine (defaults). Unknown keys are an error. Relative paths in the
    /// file are relative to the file; in environment variables, to the current
    /// directory. Every path comes out absolute, so `start`, `stop` and `status`
    /// agree whatever directory they are run from.
    ///
    /// Priority: environment variables > file > defaults.
    pub fn load(explicit: Option<&str>) -> Result<Self, Box<dyn std::error::Error>> {
        let cwd = std::env::current_dir()?;
        let absolute = |base: &Path, p: &Path| normalize(&if p.is_absolute() { p.to_path_buf() } else { base.join(p) });

        let found: Option<PathBuf> = explicit
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("SAZARE_CONFIG").filter(|v| !v.is_empty()).map(PathBuf::from))
            .or_else(|| Some(PathBuf::from("config.yaml")).filter(|p| p.is_file()))
            .or_else(|| user_config_file().filter(|p| p.is_file()));

        let (mut config, data_dir_in_file) = match &found {
            Some(path) => {
                let text = std::fs::read_to_string(path)
                    .map_err(|e| format!("{}: {e}", path.display()))?;
                let config: ServerConfig =
                    serde_yaml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
                let raw: serde_yaml::Value = serde_yaml::from_str(&text).unwrap_or_default();
                let in_file = raw.get("storage").and_then(|s| s.get("data_dir")).is_some();
                (config, in_file)
            }
            None => (Self::default(), false),
        };
        let file = found.as_deref().map(|p| absolute(&cwd, p));
        let base = file.as_deref().and_then(Path::parent).map(Path::to_path_buf).unwrap_or_else(|| cwd.clone());

        // Paths written in the file are relative to the file.
        if data_dir_in_file {
            config.storage.data_dir = absolute(&base, &config.storage.data_dir);
        }
        config.apps.dir = config.apps.dir.as_deref().map(|d| absolute(&base, d));
        config.ig.dir = config.ig.dir.as_deref().map(|d| absolute(&base, d));
        if let Some(tls) = config.server.tls.as_mut() {
            tls.cert_file = absolute(&base, Path::new(&tls.cert_file)).display().to_string();
            tls.key_file = absolute(&base, Path::new(&tls.key_file)).display().to_string();
        }
        if let Some(jwt) = config.auth.jwt.as_mut()
            && let Some(f) = jwt.public_key_file.as_mut()
        {
            *f = absolute(&base, Path::new(f.as_str())).display().to_string();
        }

        // Environment variables win; their relative paths are relative to the
        // current directory.
        if let Ok(port) = std::env::var("SAZARE_PORT")
            && let Ok(port_num) = port.parse()
        {
            config.server.port = port_num;
        }
        if let Ok(host) = std::env::var("SAZARE_HOST") {
            config.server.host = host;
        }
        // An empty variable (common in compose files and unit templates) means unset.
        let env_dir = |name: &str| {
            std::env::var_os(name).filter(|v| !v.is_empty()).map(|d| absolute(&cwd, Path::new(&d)))
        };
        let mut data_dir_set = data_dir_in_file;
        if let Some(d) = env_dir("SAZARE_DATA_DIR") {
            config.storage.data_dir = d;
            data_dir_set = true;
        }
        if let Some(d) = env_dir("SAZARE_APPS_DIR") {
            config.apps.dir = Some(d);
        }
        if let Some(d) = env_dir("SAZARE_IG_DIR") {
            config.ig.dir = Some(d);
        }

        // Defaults that don't depend on where the command is run.
        if !data_dir_set {
            let legacy = cwd.join("data");
            config.storage.data_dir = if legacy.join("resources.sqlite").is_file() {
                config.notes.push(format!(
                    "Using {} because it holds data; the default location is now {}. Set \
                     storage.data_dir (or SAZARE_DATA_DIR) to keep using it from anywhere, or move it.",
                    legacy.display(),
                    user_data_dir().map(|d| d.display().to_string()).unwrap_or_else(|| "?".into())
                ));
                legacy
            } else {
                user_data_dir().unwrap_or(legacy)
            };
        }
        if config.ig.dir.is_none() {
            let has_ig = |d: &Path| ["profiles", "searchparameters", "terminology"].iter().any(|n| d.join(n).is_dir());
            config.ig.dir = Some(if file.is_some() {
                base.clone()
            } else if has_ig(&cwd) {
                config.notes.push(format!(
                    "Reading profiles/, searchparameters/ and terminology/ from the current directory \
                     ({}); set ig.dir (or SAZARE_IG_DIR) to make that independent of where sazare starts.",
                    cwd.display()
                ));
                cwd.clone()
            } else {
                config.storage.data_dir.clone()
            });
        }

        // Credentials: exactly one form each; plaintext is accepted with a warning.
        for k in &config.auth.api_keys {
            match (&k.key, &k.key_sha256) {
                (Some(_), None) => config.notes.push(format!(
                    "API key `{}` is stored in plaintext; replace `key` with the `key_sha256` from \
                     `sazare-server auth new-api-key`.",
                    k.name
                )),
                (None, Some(h)) if h.len() == 64 && h.chars().all(|c| c.is_ascii_hexdigit()) => {}
                (None, Some(_)) => return Err(format!("API key `{}`: key_sha256 must be 64 hex digits", k.name).into()),
                _ => return Err(format!("API key `{}`: set exactly one of key, key_sha256", k.name).into()),
            }
        }
        for u in &config.auth.basic_auth {
            match (&u.password, &u.password_hash) {
                (Some(_), None) => config.notes.push(format!(
                    "Password of `{}` is stored in plaintext; replace `password` with the \
                     `password_hash` from `sazare-server auth hash-password`.",
                    u.username
                )),
                (None, Some(h)) if crate::auth::credentials::is_argon2_hash(h) => {}
                (None, Some(_)) => return Err(format!("user `{}`: password_hash must be an Argon2 hash", u.username).into()),
                _ => return Err(format!("user `{}`: set exactly one of password, password_hash", u.username).into()),
            }
        }

        config.source = file;
        Ok(config)
    }

    /// The directory holding `profiles/`, `searchparameters/`, `terminology/`.
    pub fn ig_dir(&self) -> PathBuf {
        self.ig.dir.clone().unwrap_or_else(|| PathBuf::from("."))
    }

    /// Get the full path to the resources database
    pub fn resources_db_path(&self) -> PathBuf {
        self.storage.data_dir.join(&self.storage.resources_db)
    }

    /// Get the full path to the search index database
    pub fn search_index_db_path(&self) -> PathBuf {
        self.storage.data_dir.join(&self.storage.search_index_db)
    }

    /// Get the full path to the audit database
    pub fn audit_db_path(&self) -> PathBuf {
        self.storage.data_dir.join(&self.storage.audit_db)
    }

    /// The apps directory, if configured and existing. There is no implicit
    /// default: nothing is served unless the operator asks for it.
    pub fn apps_dir(&self) -> Option<PathBuf> {
        self.apps.dir.clone().filter(|d| d.is_dir())
    }
}

/// Where an unconfigured server keeps its data: `$XDG_DATA_HOME/sazare` or
/// `~/.local/share/sazare` (Linux and other Unix), `~/Library/Application
/// Support/sazare/data` (macOS), `%LOCALAPPDATA%\\sazare\\data` (Windows).
pub fn user_data_dir() -> Option<PathBuf> {
    let env = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty()).map(PathBuf::from);
    if cfg!(windows) {
        env("LOCALAPPDATA").map(|d| d.join("sazare").join("data"))
    } else if cfg!(target_os = "macos") {
        env("HOME").map(|h| h.join("Library/Application Support/sazare/data"))
    } else {
        env("XDG_DATA_HOME")
            .or_else(|| env("HOME").map(|h| h.join(".local/share")))
            .map(|d| d.join("sazare"))
    }
}

/// The user's configuration file, read when there is no `--config`,
/// `SAZARE_CONFIG` or `./config.yaml`: `$XDG_CONFIG_HOME/sazare/config.yaml` or
/// `~/.config/sazare/config.yaml`, `~/Library/Application
/// Support/sazare/config.yaml` (macOS), `%APPDATA%\\sazare\\config.yaml` (Windows).
pub fn user_config_file() -> Option<PathBuf> {
    let env = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty()).map(PathBuf::from);
    let dir = if cfg!(windows) {
        env("APPDATA").map(|d| d.join("sazare"))
    } else if cfg!(target_os = "macos") {
        env("HOME").map(|h| h.join("Library/Application Support/sazare"))
    } else {
        env("XDG_CONFIG_HOME").or_else(|| env("HOME").map(|h| h.join(".config"))).map(|d| d.join("sazare"))
    };
    dir.map(|d| d.join("config.yaml"))
}

/// `a/b/../c/./d` → `a/c/d`, without touching the file system (the path may
/// not exist yet).
fn normalize(p: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = ServerConfig::default();
        assert_eq!(config.server.port, 8080);
        assert_eq!(config.server.host, "127.0.0.1");
        assert!(!config.auth.enabled);
    }

    #[test]
    fn test_db_paths() {
        let config = ServerConfig::default();
        assert_eq!(
            config.resources_db_path(),
            PathBuf::from("data/resources.sqlite")
        );
        assert_eq!(
            config.search_index_db_path(),
            PathBuf::from("data/search_index.sqlite")
        );
        assert_eq!(
            config.audit_db_path(),
            PathBuf::from("data/audit.sqlite")
        );
    }
}
