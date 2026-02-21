use axum::{
    body::Body,
    extract::{Request, State},
    http::{header, Method, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use jsonwebtoken::{Algorithm, DecodingKey, TokenData, Validation, jwk::JwkSet};
use sazare_core::OperationOutcome;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::RwLock;

use crate::{audit, config::ServerConfig, AppState};

/// Cached JWK key set fetched from an external IdP.
#[derive(Default)]
pub struct JwkCache {
    jwks: Option<JwkSet>,
    fetched_at: Option<std::time::Instant>,
}

/// Cache TTL: 15 minutes
const JWK_CACHE_TTL: std::time::Duration = std::time::Duration::from_secs(15 * 60);

impl JwkCache {
    pub fn new() -> Self {
        Self::default()
    }

    fn is_expired(&self) -> bool {
        match self.fetched_at {
            Some(t) => t.elapsed() > JWK_CACHE_TTL,
            None => true,
        }
    }
}

/// Fetch or return cached JWK set from the configured URL.
async fn get_jwks(
    jwk_url: &str,
    cache: &RwLock<JwkCache>,
) -> Result<JwkSet, String> {
    // Check cache first (read lock)
    {
        let c = cache.read().await;
        if !c.is_expired()
            && let Some(ref jwks) = c.jwks
        {
            return Ok(jwks.clone());
        }
    }

    // Fetch fresh keys (write lock)
    let mut c = cache.write().await;
    // Double-check after acquiring write lock
    if !c.is_expired()
        && let Some(ref jwks) = c.jwks
    {
        return Ok(jwks.clone());
    }

    let response = reqwest::get(jwk_url)
        .await
        .map_err(|e| format!("Failed to fetch JWK from {}: {}", jwk_url, e))?;

    let jwks: JwkSet = response
        .json()
        .await
        .map_err(|e| format!("Failed to parse JWK response: {}", e))?;

    c.jwks = Some(jwks.clone());
    c.fetched_at = Some(std::time::Instant::now());

    Ok(jwks)
}

/// Authenticated user information
#[derive(Debug, Clone)]
pub struct AuthUser {
    pub user_id: String,
    pub auth_type: AuthType,
    pub scopes: Vec<String>,
    pub patient_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum AuthType {
    ApiKey,
    BasicAuth,
    Jwt,
}

impl AuthUser {
    pub fn new(user_id: String, auth_type: AuthType) -> Self {
        Self {
            scopes: Vec::new(),
            user_id,
            auth_type,
            patient_id: None,
        }
    }

    pub fn with_scopes(user_id: String, auth_type: AuthType, scopes: Vec<String>) -> Self {
        Self {
            user_id,
            auth_type,
            scopes,
            patient_id: None,
        }
    }

    /// Returns true if the user has only patient/ scopes (no user/ or system/ scopes).
    pub fn is_patient_scoped(&self) -> bool {
        if self.scopes.is_empty() {
            return false;
        }
        let has_patient = self.scopes.iter().any(|s| s.starts_with("patient/"));
        let has_other = self
            .scopes
            .iter()
            .any(|s| s.starts_with("user/") || s.starts_with("system/"));
        has_patient && !has_other
    }
}

/// JWT claims structure
#[derive(Debug, Serialize, Deserialize)]
struct JwtClaims {
    sub: Option<String>,
    #[serde(default)]
    scope: Option<String>,
    iss: Option<String>,
    aud: Option<serde_json::Value>,
    exp: Option<u64>,
    iat: Option<u64>,
    /// SMART launch context: patient ID
    #[serde(default)]
    patient: Option<String>,
}

/// Authentication middleware
pub async fn auth_middleware(
    State(state): State<Arc<AppState>>,
    mut request: Request<Body>,
    next: Next,
) -> Result<Response, Response> {
    // Skip auth if disabled
    if !state.config.auth.enabled {
        return Ok(next.run(request).await);
    }

    // Allow public endpoints without auth
    let path = request.uri().path();
    if path == "/" || path == "/$status" || path == "/health" || path == "/metadata"
        || path.starts_with("/.well-known/")
        || path.starts_with("/$browse")
    {
        return Ok(next.run(request).await);
    }

    // Extract authorization header
    let auth_header = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok());

    let Some(auth_header) = auth_header else {
        let outcome = OperationOutcome::unauthorized("Missing Authorization header");
        return Err((StatusCode::UNAUTHORIZED, axum::Json(outcome)).into_response());
    };

    // Attempt to authenticate
    let auth_user = if auth_header.starts_with("Bearer ") {
        authenticate_bearer(&state, auth_header).await?
    } else if auth_header.starts_with("Basic ") {
        authenticate_basic(&state.config, auth_header)?
    } else {
        let outcome =
            OperationOutcome::unauthorized("Invalid Authorization header format. Use 'Bearer <token>' or 'Basic <credentials>'");
        return Err((StatusCode::UNAUTHORIZED, axum::Json(outcome)).into_response());
    };

    // Scope check for JWT users
    if auth_user.auth_type == AuthType::Jwt {
        let method = request.method().clone();
        let path = request.uri().path().to_string();
        if let Some((resource_type, action)) = extract_resource_action(&method, &path)
            && !check_scope(&auth_user.scopes, &resource_type, &action)
        {
            let outcome = OperationOutcome::forbidden(format!(
                "Insufficient scope for {}/{}.{}",
                resource_type, resource_type, action
            ));
            return Err((StatusCode::FORBIDDEN, axum::Json(outcome)).into_response());
        }
    }

    // Log successful authentication
    let client_ip = request
        .extensions()
        .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
        .map(|ci| ci.0.ip().to_string())
        .unwrap_or_else(|| "unknown".to_string());
    audit::log_auth_attempt(&client_ip, Some(&auth_user.user_id), true);

    // Insert auth user into request extensions
    request.extensions_mut().insert(auth_user);

    Ok(next.run(request).await)
}

/// Authenticate using Bearer token (API key first, then JWT fallback)
#[allow(clippy::result_large_err)]
async fn authenticate_bearer(state: &Arc<AppState>, auth_header: &str) -> Result<AuthUser, Response> {
    let token = auth_header.trim_start_matches("Bearer ").trim();

    // Try API key match first
    for api_key in &state.config.auth.api_keys {
        if api_key.key == token {
            return Ok(AuthUser::new(api_key.name.clone(), AuthType::ApiKey));
        }
    }

    // Try JWT decode if JWT settings are configured
    if let Some(ref jwt_settings) = state.config.auth.jwt {
        return authenticate_jwt(jwt_settings, token, &state.jwk_cache).await;
    }

    let outcome = OperationOutcome::unauthorized("Invalid API key");
    Err((StatusCode::UNAUTHORIZED, axum::Json(outcome)).into_response())
}

/// Authenticate using JWT token
#[allow(clippy::result_large_err)]
async fn authenticate_jwt(
    jwt_settings: &crate::config::JwtSettings,
    token: &str,
    jwk_cache: &RwLock<JwkCache>,
) -> Result<AuthUser, Response> {
    // Determine decoding key and algorithm
    let (decoding_key, algorithm) = if let Some(ref jwk_url) = jwt_settings.jwk_url {
        // JWK URL mode: fetch keys from external IdP
        let jwks = get_jwks(jwk_url, jwk_cache).await.map_err(|e| {
            let outcome = OperationOutcome::storage_error(e);
            (StatusCode::INTERNAL_SERVER_ERROR, axum::Json(outcome)).into_response()
        })?;

        // Decode JWT header to get kid
        let header = jsonwebtoken::decode_header(token).map_err(|e| {
            let outcome = OperationOutcome::unauthorized(format!("Invalid JWT header: {}", e));
            (StatusCode::UNAUTHORIZED, axum::Json(outcome)).into_response()
        })?;

        let kid = header.kid.as_deref().unwrap_or("");
        let jwk = jwks
            .keys
            .iter()
            .find(|k| k.common.key_id.as_deref() == Some(kid))
            .or_else(|| jwks.keys.first())
            .ok_or_else(|| {
                let outcome = OperationOutcome::unauthorized("No matching JWK found");
                (StatusCode::UNAUTHORIZED, axum::Json(outcome)).into_response()
            })?;

        let key = DecodingKey::from_jwk(jwk).map_err(|e| {
            let outcome = OperationOutcome::unauthorized(format!("Invalid JWK: {}", e));
            (StatusCode::UNAUTHORIZED, axum::Json(outcome)).into_response()
        })?;

        let alg = jwk
            .common
            .key_algorithm
            .and_then(|a| match a {
                jsonwebtoken::jwk::KeyAlgorithm::RS256 => Some(Algorithm::RS256),
                jsonwebtoken::jwk::KeyAlgorithm::RS384 => Some(Algorithm::RS384),
                jsonwebtoken::jwk::KeyAlgorithm::RS512 => Some(Algorithm::RS512),
                jsonwebtoken::jwk::KeyAlgorithm::ES256 => Some(Algorithm::ES256),
                jsonwebtoken::jwk::KeyAlgorithm::ES384 => Some(Algorithm::ES384),
                _ => None,
            })
            .unwrap_or(Algorithm::RS256);

        (key, alg)
    } else if let Some(ref secret) = jwt_settings.secret {
        (DecodingKey::from_secret(secret.as_bytes()), Algorithm::HS256)
    } else if let Some(ref key_file) = jwt_settings.public_key_file {
        let pem = std::fs::read(key_file).map_err(|e| {
            let outcome =
                OperationOutcome::storage_error(format!("Failed to read public key file: {}", e));
            (StatusCode::INTERNAL_SERVER_ERROR, axum::Json(outcome)).into_response()
        })?;
        let key = DecodingKey::from_rsa_pem(&pem).map_err(|e| {
            let outcome =
                OperationOutcome::storage_error(format!("Invalid public key: {}", e));
            (StatusCode::INTERNAL_SERVER_ERROR, axum::Json(outcome)).into_response()
        })?;
        (key, Algorithm::RS256)
    } else {
        let outcome = OperationOutcome::storage_error(
            "JWT is configured but no secret, public_key_file, or jwk_url is set",
        );
        return Err((StatusCode::INTERNAL_SERVER_ERROR, axum::Json(outcome)).into_response());
    };

    // Build validation
    let mut validation = Validation::new(algorithm);

    if let Some(ref issuer) = jwt_settings.issuer {
        validation.set_issuer(&[issuer]);
    }

    if let Some(ref audience) = jwt_settings.audience {
        validation.set_audience(&[audience]);
    } else {
        validation.validate_aud = false;
    }

    let token_data: TokenData<JwtClaims> =
        jsonwebtoken::decode(token, &decoding_key, &validation).map_err(|e| {
            let outcome =
                OperationOutcome::unauthorized(format!("Invalid JWT: {}", e));
            (StatusCode::UNAUTHORIZED, axum::Json(outcome)).into_response()
        })?;

    let user_id = token_data
        .claims
        .sub
        .unwrap_or_else(|| "anonymous".to_string());

    let scopes: Vec<String> = token_data
        .claims
        .scope
        .map(|s| s.split_whitespace().map(String::from).collect())
        .unwrap_or_default();

    let mut auth_user = AuthUser::with_scopes(user_id, AuthType::Jwt, scopes);
    auth_user.patient_id = token_data.claims.patient;
    Ok(auth_user)
}

/// Authenticate using Basic authentication
#[allow(clippy::result_large_err)]
fn authenticate_basic(config: &ServerConfig, auth_header: &str) -> Result<AuthUser, Response> {
    let credentials = auth_header.trim_start_matches("Basic ").trim();

    // Decode base64 credentials
    let decoded = STANDARD.decode(credentials).map_err(|_| {
        let outcome = OperationOutcome::unauthorized("Invalid Base64 encoding in Basic auth");
        (StatusCode::UNAUTHORIZED, axum::Json(outcome)).into_response()
    })?;

    let credentials_str = String::from_utf8(decoded).map_err(|_| {
        let outcome = OperationOutcome::unauthorized("Invalid UTF-8 in Basic auth credentials");
        (StatusCode::UNAUTHORIZED, axum::Json(outcome)).into_response()
    })?;

    // Split username:password
    let parts: Vec<&str> = credentials_str.splitn(2, ':').collect();
    if parts.len() != 2 {
        let outcome =
            OperationOutcome::unauthorized("Invalid Basic auth format. Expected 'username:password'");
        return Err((StatusCode::UNAUTHORIZED, axum::Json(outcome)).into_response());
    }

    let (username, password) = (parts[0], parts[1]);

    // Validate credentials
    for user in &config.auth.basic_auth {
        if user.username == username && user.password == password {
            return Ok(AuthUser::new(username.to_string(), AuthType::BasicAuth));
        }
    }

    let outcome = OperationOutcome::unauthorized("Invalid username or password");
    Err((StatusCode::UNAUTHORIZED, axum::Json(outcome)).into_response())
}

/// Extract resource type and action (read/write) from HTTP method + path
fn extract_resource_action(method: &Method, path: &str) -> Option<(String, String)> {
    let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    if segments.is_empty() {
        return None;
    }

    // Skip non-resource paths
    let first = segments[0];
    if matches!(
        first,
        "health" | "metadata" | "$export" | "$import" | "$status" | ".well-known"
    ) {
        return None;
    }

    let resource_type = first.to_string();
    let action = match *method {
        Method::GET | Method::HEAD => "read".to_string(),
        Method::POST | Method::PUT | Method::PATCH | Method::DELETE => "write".to_string(),
        _ => return None,
    };

    Some((resource_type, action))
}

/// Check if the given scopes allow access to the specified resource_type and action.
///
/// SMART on FHIR v2 scope format: `context/resourceType.action`
/// - context: patient | user | system
/// - resourceType: specific type or `*` (all)
/// - action: read | write | `*` (all)
///
/// Examples: `user/Patient.read`, `system/*.write`, `patient/*.*`
pub fn check_scope(scopes: &[String], resource_type: &str, action: &str) -> bool {
    if scopes.is_empty() {
        return false;
    }

    for scope in scopes {
        if let Some((_context, rest)) = scope.split_once('/')
            && let Some((scope_rt, scope_action)) = rest.split_once('.')
        {
            let rt_match = scope_rt == "*" || scope_rt == resource_type;
            let action_match = scope_action == "*" || scope_action == action;
            if rt_match && action_match {
                return true;
            }
        }
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ApiKey, AuthSettings, BasicAuthUser, JwtSettings};

    fn test_config() -> ServerConfig {
        let mut config = ServerConfig::default();
        config.auth = AuthSettings {
            enabled: true,
            api_keys: vec![ApiKey {
                name: "test-client".to_string(),
                key: "test-api-key-12345".to_string(),
            }],
            basic_auth: vec![BasicAuthUser {
                username: "admin".to_string(),
                password: "admin123".to_string(),
            }],
            jwt: None,
        };
        config
    }

    fn test_config_with_jwt() -> ServerConfig {
        let mut config = test_config();
        config.auth.jwt = Some(JwtSettings {
            issuer: Some("test-issuer".to_string()),
            audience: Some("test-audience".to_string()),
            secret: Some("super-secret-key-for-testing-only-1234567890".to_string()),
            public_key_file: None,
            jwk_url: None,
        });
        config
    }

    /// Build a minimal AppState for auth tests (temp dir for SQLite)
    fn test_app_state(config: ServerConfig) -> Arc<AppState> {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.sqlite");
        let idx_path = dir.path().join("idx.sqlite");
        let audit_path = dir.path().join("audit.sqlite");
        Arc::new(AppState {
            store: sazare_store::SqliteStore::open(&db_path).unwrap(),
            index: tokio::sync::Mutex::new(
                sazare_store::SearchIndex::open(&idx_path).unwrap(),
            ),
            audit: Arc::new(tokio::sync::Mutex::new(
                sazare_store::AuditLog::open(&audit_path).unwrap(),
            )),
            config,
            profile_registry: sazare_core::validation::ProfileRegistry::new(),
            terminology_registry: sazare_core::validation::TerminologyRegistry::new(),
            search_param_registry: sazare_core::SearchParamRegistry::new(),
            compartment_def: sazare_core::CompartmentDef::patient_compartment(),
            jwk_cache: RwLock::new(JwkCache::new()),
        })
    }

    #[tokio::test]
    async fn test_authenticate_bearer_valid() {
        let state = test_app_state(test_config());
        let result = authenticate_bearer(&state, "Bearer test-api-key-12345").await;
        assert!(result.is_ok());
        let auth_user = result.unwrap();
        assert_eq!(auth_user.user_id, "test-client");
        assert_eq!(auth_user.auth_type, AuthType::ApiKey);
    }

    #[tokio::test]
    async fn test_authenticate_bearer_invalid() {
        let state = test_app_state(test_config());
        let result = authenticate_bearer(&state, "Bearer invalid-key").await;
        assert!(result.is_err());
    }

    #[test]
    fn test_authenticate_basic_valid() {
        let config = test_config();
        let credentials = STANDARD.encode("admin:admin123");
        let result = authenticate_basic(&config, &format!("Basic {}", credentials));
        assert!(result.is_ok());
        let auth_user = result.unwrap();
        assert_eq!(auth_user.user_id, "admin");
        assert_eq!(auth_user.auth_type, AuthType::BasicAuth);
    }

    #[test]
    fn test_authenticate_basic_invalid() {
        let config = test_config();
        let credentials = STANDARD.encode("admin:wrongpass");
        let result = authenticate_basic(&config, &format!("Basic {}", credentials));
        assert!(result.is_err());
    }

    // --- JWT tests ---

    fn create_test_jwt(sub: &str, scope: &str, issuer: &str, audience: &str) -> String {
        use jsonwebtoken::{encode, EncodingKey, Header};

        let claims = serde_json::json!({
            "sub": sub,
            "scope": scope,
            "iss": issuer,
            "aud": audience,
            "exp": chrono::Utc::now().timestamp() as u64 + 3600,
            "iat": chrono::Utc::now().timestamp() as u64,
        });

        encode(
            &Header::new(Algorithm::HS256),
            &claims,
            &EncodingKey::from_secret(
                "super-secret-key-for-testing-only-1234567890".as_bytes(),
            ),
        )
        .unwrap()
    }

    #[tokio::test]
    async fn test_jwt_authentication_valid() {
        let state = test_app_state(test_config_with_jwt());
        let token = create_test_jwt(
            "user-1",
            "user/Patient.read user/Observation.read",
            "test-issuer",
            "test-audience",
        );
        let result = authenticate_bearer(&state, &format!("Bearer {}", token)).await;
        assert!(result.is_ok());
        let auth_user = result.unwrap();
        assert_eq!(auth_user.user_id, "user-1");
        assert_eq!(auth_user.auth_type, AuthType::Jwt);
        assert_eq!(auth_user.scopes, vec!["user/Patient.read", "user/Observation.read"]);
    }

    #[tokio::test]
    async fn test_jwt_authentication_invalid_token() {
        let state = test_app_state(test_config_with_jwt());
        let result = authenticate_bearer(&state, "Bearer not-a-valid-jwt").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_jwt_authentication_wrong_issuer() {
        let state = test_app_state(test_config_with_jwt());
        let token = create_test_jwt("user-1", "user/Patient.read", "wrong-issuer", "test-audience");
        let result = authenticate_bearer(&state, &format!("Bearer {}", token)).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_jwt_authentication_wrong_audience() {
        let state = test_app_state(test_config_with_jwt());
        let token = create_test_jwt("user-1", "user/Patient.read", "test-issuer", "wrong-audience");
        let result = authenticate_bearer(&state, &format!("Bearer {}", token)).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_api_key_takes_priority_over_jwt() {
        let state = test_app_state(test_config_with_jwt());
        let result = authenticate_bearer(&state, "Bearer test-api-key-12345").await;
        assert!(result.is_ok());
        let auth_user = result.unwrap();
        assert_eq!(auth_user.auth_type, AuthType::ApiKey);
    }

    // --- Scope check tests ---

    #[test]
    fn test_check_scope_exact_match() {
        assert!(check_scope(
            &["user/Patient.read".to_string()],
            "Patient",
            "read"
        ));
    }

    #[test]
    fn test_check_scope_wildcard_resource() {
        assert!(check_scope(
            &["user/*.read".to_string()],
            "Patient",
            "read"
        ));
        assert!(check_scope(
            &["user/*.read".to_string()],
            "Observation",
            "read"
        ));
    }

    #[test]
    fn test_check_scope_wildcard_action() {
        assert!(check_scope(
            &["user/Patient.*".to_string()],
            "Patient",
            "read"
        ));
        assert!(check_scope(
            &["user/Patient.*".to_string()],
            "Patient",
            "write"
        ));
    }

    #[test]
    fn test_check_scope_wildcard_both() {
        assert!(check_scope(
            &["system/*.*".to_string()],
            "Patient",
            "read"
        ));
        assert!(check_scope(
            &["system/*.*".to_string()],
            "Observation",
            "write"
        ));
    }

    #[test]
    fn test_check_scope_no_match() {
        assert!(!check_scope(
            &["user/Patient.read".to_string()],
            "Patient",
            "write"
        ));
        assert!(!check_scope(
            &["user/Observation.read".to_string()],
            "Patient",
            "read"
        ));
    }

    #[test]
    fn test_check_scope_empty() {
        assert!(!check_scope(&[], "Patient", "read"));
    }

    #[test]
    fn test_check_scope_multiple_scopes() {
        let scopes = vec![
            "user/Patient.read".to_string(),
            "user/Observation.write".to_string(),
        ];
        assert!(check_scope(&scopes, "Patient", "read"));
        assert!(check_scope(&scopes, "Observation", "write"));
        assert!(!check_scope(&scopes, "Patient", "write"));
    }

    // --- extract_resource_action tests ---

    #[test]
    fn test_extract_resource_action_read() {
        let result = extract_resource_action(&Method::GET, "/Patient/123");
        assert_eq!(result, Some(("Patient".to_string(), "read".to_string())));
    }

    #[test]
    fn test_extract_resource_action_search() {
        let result = extract_resource_action(&Method::GET, "/Patient");
        assert_eq!(result, Some(("Patient".to_string(), "read".to_string())));
    }

    #[test]
    fn test_extract_resource_action_create() {
        let result = extract_resource_action(&Method::POST, "/Patient");
        assert_eq!(result, Some(("Patient".to_string(), "write".to_string())));
    }

    #[test]
    fn test_extract_resource_action_update() {
        let result = extract_resource_action(&Method::PUT, "/Patient/123");
        assert_eq!(result, Some(("Patient".to_string(), "write".to_string())));
    }

    #[test]
    fn test_extract_resource_action_delete() {
        let result = extract_resource_action(&Method::DELETE, "/Patient/123");
        assert_eq!(result, Some(("Patient".to_string(), "write".to_string())));
    }

    #[test]
    fn test_extract_resource_action_skip_metadata() {
        assert!(extract_resource_action(&Method::GET, "/metadata").is_none());
        assert!(extract_resource_action(&Method::GET, "/health").is_none());
    }

    // --- is_patient_scoped tests ---

    #[test]
    fn test_is_patient_scoped_true() {
        let user = AuthUser {
            user_id: "test".to_string(),
            auth_type: AuthType::Jwt,
            scopes: vec!["patient/Observation.read".to_string(), "patient/Patient.read".to_string()],
            patient_id: Some("p123".to_string()),
        };
        assert!(user.is_patient_scoped());
    }

    #[test]
    fn test_is_patient_scoped_false_with_user_scope() {
        let user = AuthUser {
            user_id: "test".to_string(),
            auth_type: AuthType::Jwt,
            scopes: vec!["patient/Observation.read".to_string(), "user/Patient.read".to_string()],
            patient_id: Some("p123".to_string()),
        };
        assert!(!user.is_patient_scoped());
    }

    #[test]
    fn test_is_patient_scoped_false_empty_scopes() {
        let user = AuthUser::new("test".to_string(), AuthType::ApiKey);
        assert!(!user.is_patient_scoped());
    }

    #[test]
    fn test_is_patient_scoped_false_system_scope() {
        let user = AuthUser::with_scopes(
            "system".to_string(),
            AuthType::Jwt,
            vec!["system/*.*".to_string()],
        );
        assert!(!user.is_patient_scoped());
    }

    // --- JWT patient claim test ---

    #[tokio::test]
    async fn test_jwt_with_patient_claim() {
        use jsonwebtoken::{encode, EncodingKey, Header};

        let state = test_app_state(test_config_with_jwt());
        let claims = serde_json::json!({
            "sub": "patient-user",
            "scope": "patient/Observation.read patient/Patient.read",
            "iss": "test-issuer",
            "aud": "test-audience",
            "exp": chrono::Utc::now().timestamp() as u64 + 3600,
            "iat": chrono::Utc::now().timestamp() as u64,
            "patient": "p456"
        });

        let token = encode(
            &Header::new(Algorithm::HS256),
            &claims,
            &EncodingKey::from_secret("super-secret-key-for-testing-only-1234567890".as_bytes()),
        )
        .unwrap();

        let result = authenticate_bearer(&state, &format!("Bearer {}", token)).await;
        assert!(result.is_ok());
        let auth_user = result.unwrap();
        assert_eq!(auth_user.patient_id, Some("p456".to_string()));
        assert!(auth_user.is_patient_scoped());
    }

    #[tokio::test]
    async fn test_jwt_without_patient_claim() {
        let state = test_app_state(test_config_with_jwt());
        let token = create_test_jwt(
            "user-1",
            "user/Patient.read",
            "test-issuer",
            "test-audience",
        );
        let result = authenticate_bearer(&state, &format!("Bearer {}", token)).await;
        assert!(result.is_ok());
        let auth_user = result.unwrap();
        assert_eq!(auth_user.patient_id, None);
        assert!(!auth_user.is_patient_scoped());
    }
}
