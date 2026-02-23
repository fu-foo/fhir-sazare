use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Server configuration loaded from YAML file
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ServerConfig {
    pub server: ServerSettings,
    pub auth: AuthSettings,
    pub storage: StorageSettings,
    pub log: LogSettings,
    pub webhook: WebhookSettings,
    pub plugins: PluginSettings,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ServerSettings {
    pub host: String,
    pub port: u16,
    pub tls: Option<TlsSettings>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TlsSettings {
    pub cert_file: String,
    pub key_file: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AuthSettings {
    pub enabled: bool,
    pub api_keys: Vec<ApiKey>,
    pub basic_auth: Vec<BasicAuthUser>,
    pub jwt: Option<JwtSettings>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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
pub struct ApiKey {
    pub name: String,
    pub key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BasicAuthUser {
    pub username: String,
    pub password: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct StorageSettings {
    pub data_dir: PathBuf,
    pub resources_db: String,
    pub search_index_db: String,
    pub audit_db: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct LogSettings {
    pub level: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct WebhookSettings {
    pub enabled: bool,
    pub endpoints: Vec<WebhookEndpoint>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebhookEndpoint {
    pub url: String,
    pub events: Vec<String>,
    #[serde(default)]
    pub headers: std::collections::HashMap<String, String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct PluginSettings {
    pub dir: Option<PathBuf>,
}

impl Default for ServerSettings {
    fn default() -> Self {
        Self {
            host: "0.0.0.0".to_string(),
            port: 8080,
            tls: None,
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
    /// Load configuration from a YAML file
    pub fn load_from_file(path: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let content = std::fs::read_to_string(path)?;
        let config: ServerConfig = serde_yaml::from_str(&content)?;
        Ok(config)
    }

    /// Load configuration with priority: CLI args > env vars > config file > defaults
    pub fn load(config_path: Option<&str>) -> Result<Self, Box<dyn std::error::Error>> {
        let mut config = if let Some(path) = config_path {
            Self::load_from_file(path)?
        } else {
            Self::default()
        };

        // Override with environment variables
        if let Ok(port) = std::env::var("SAZARE_PORT")
            && let Ok(port_num) = port.parse()
        {
            config.server.port = port_num;
        }

        if let Ok(host) = std::env::var("SAZARE_HOST") {
            config.server.host = host;
        }

        if let Ok(data_dir) = std::env::var("SAZARE_DATA_DIR") {
            config.storage.data_dir = PathBuf::from(data_dir);
        }

        if let Ok(plugin_dir) = std::env::var("SAZARE_PLUGIN_DIR") {
            config.plugins.dir = Some(PathBuf::from(plugin_dir));
        }

        Ok(config)
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

    /// Get the resolved plugin directory path, if configured and the directory exists.
    pub fn plugin_dir(&self) -> Option<PathBuf> {
        match &self.plugins.dir {
            Some(dir) if dir.is_dir() => Some(dir.clone()),
            Some(_) => None,
            None => {
                let default = PathBuf::from("plugins");
                default.is_dir().then_some(default)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = ServerConfig::default();
        assert_eq!(config.server.port, 8080);
        assert_eq!(config.server.host, "0.0.0.0");
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
