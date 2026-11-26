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

/// Fetch or return the cached JWK set from the configured URL. `refresh`
/// forces a fetch (a token named a `kid` the cache doesn't have, e.g. after
/// the IdP rotated keys), at most once per [`JWK_MIN_REFRESH`] so tokens with
/// made-up `kid`s can't make the server hammer the IdP.
///
/// The fetch runs without holding the cache lock, with the shared client's
/// timeouts: a slow IdP delays only the requests that need fresh keys.
async fn get_jwks(
    jwk_url: &str,
    cache: &RwLock<JwkCache>,
    refresh: bool,
) -> Result<JwkSet, String> {
    {
        let c = cache.read().await;
        let recently = c.fetched_at.is_some_and(|t| t.elapsed() < JWK_MIN_REFRESH);
        if let Some(ref jwks) = c.jwks
            && (!c.is_expired() && (!refresh || recently))
        {
            return Ok(jwks.clone());
        }
    }

    let jwks: JwkSet = crate::http_client()
        .get(jwk_url)
        .send()
        .await
        .map_err(|e| format!("Failed to fetch JWK from {}: {}", jwk_url, e))?
        .json()
        .await
        .map_err(|e| format!("Failed to parse JWK response: {}", e))?;

    let mut c = cache.write().await;
    c.jwks = Some(jwks.clone());
    c.fetched_at = Some(std::time::Instant::now());
    Ok(jwks)
}

/// The PEM of `public_key_file`, read once (not on every request).
fn read_key_file(path: &str) -> std::io::Result<Vec<u8>> {
    static CACHE: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<String, Vec<u8>>>> =
        std::sync::OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    if let Some(pem) = cache.lock().unwrap_or_else(|e| e.into_inner()).get(path) {
        return Ok(pem.clone());
    }
    let pem = std::fs::read(path)?;
    cache.lock().unwrap_or_else(|e| e.into_inner()).insert(path.to_string(), pem.clone());
    Ok(pem)
}

/// Minimum time between forced JWKS refreshes.
const JWK_MIN_REFRESH: std::time::Duration = std::time::Duration::from_secs(30);

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
        if self.auth_type != AuthType::Jwt || self.scopes.is_empty() {
            return false;
        }
        let has_patient = self.scopes.iter().any(|s| s.starts_with("patient/"));
        let has_other = self
            .scopes
            .iter()
            .any(|s| s.starts_with("user/") || s.starts_with("system/"));
        has_patient && !has_other
    }

    /// What this user may do with `resource_type` for `perm` (a SMART v2
    /// permission letter: c, r, u, d or s). API keys and Basic users are
    /// operators and may do anything.
    pub fn grant(&self, resource_type: &str, perm: char) -> Option<Grant> {
        match self.auth_type {
            AuthType::ApiKey | AuthType::BasicAuth => Some(Grant::Full),
            AuthType::Jwt => scope_grant(&self.scopes, resource_type, perm),
        }
    }

    /// Whether this user's `perm` access to `resource_type` is limited to the
    /// patient compartment: no `user/` or `system/` scope grants exactly that
    /// permission on that type. Decided per type *and* permission, so
    /// `patient/Observation.r user/Observation.c` reads Observations only in
    /// the compartment, and `patient/Observation.rs user/Practitioner.rs`
    /// restricts Observations, not Practitioners. Operators (API key, Basic)
    /// are never restricted.
    pub fn compartment_restricted(&self, resource_type: &str, perm: char) -> bool {
        self.grant(resource_type, perm) != Some(Grant::Full)
    }
}

/// How far a scope reaches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Grant {
    /// Any resource of the type (user/ or system/ scope, operators).
    Full,
    /// Only resources in the token's patient compartment (patient/ scope).
    Patient,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Context {
    Patient,
    User,
    System,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Scope {
    context: Context,
    resource_type: String,
    /// Permission letters out of "cruds".
    perms: String,
}

/// Parse a SMART scope: v1 `ctx/Type.read|write|*` or v2 `ctx/Type.cruds`
/// (any in-order subset). Scopes with a query (`?category=...`, SMART v2
/// granular scopes) narrow access in a way this server doesn't evaluate, so
/// they grant nothing rather than more than intended.
fn parse_scope(scope: &str) -> Option<Scope> {
    let (ctx, rest) = scope.split_once('/')?;
    let context = match ctx {
        "patient" => Context::Patient,
        "user" => Context::User,
        "system" => Context::System,
        _ => return None,
    };
    let (rt, action) = rest.split_once('.')?;
    if rt.is_empty() || action.contains('?') {
        return None;
    }
    let perms = match action {
        "read" => "rs".to_string(),
        "write" => "cud".to_string(),
        "*" => "cruds".to_string(),
        v2 if !v2.is_empty() && is_in_order_subset(v2, "cruds") => v2.to_string(),
        _ => return None,
    };
    Some(Scope { context, resource_type: rt.to_string(), perms })
}

fn is_in_order_subset(s: &str, of: &str) -> bool {
    let mut rest = of.chars();
    s.chars().all(|c| rest.by_ref().any(|o| o == c))
}

fn parsed_scopes(scopes: &[String]) -> Vec<Scope> {
    scopes.iter().filter_map(|s| parse_scope(s)).collect()
}

/// The widest grant `scopes` give for `perm` on `resource_type`.
pub fn scope_grant(scopes: &[String], resource_type: &str, perm: char) -> Option<Grant> {
    let mut best = None;
    for s in parsed_scopes(scopes) {
        if (s.resource_type == "*" || s.resource_type == resource_type) && s.perms.contains(perm) {
            if s.context != Context::Patient {
                return Some(Grant::Full);
            }
            best = Some(Grant::Patient);
        }
    }
    best
}

/// Whether `scopes` include a `system/` scope on all resource types with
/// every permission in `perms` — what Bulk Data and administration need.
pub fn has_system_scope(scopes: &[String], perms: &str) -> bool {
    parsed_scopes(scopes).iter().any(|s| {
        s.context == Context::System && s.resource_type == "*" && perms.chars().all(|p| s.perms.contains(p))
    })
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
// Both arms are full HTTP responses by design (axum middleware convention).
#[allow(clippy::result_large_err)]
pub async fn auth_middleware(
    State(state): State<Arc<AppState>>,
    mut request: Request<Body>,
    next: Next,
) -> Result<Response, Response> {
    // Skip auth if disabled
    if !state.config.auth.enabled {
        return Ok(next.run(request).await);
    }

    // Allow public endpoints without auth (FHIR spec requirements)
    let path = request.uri().path();
    if path == "/health" || path == "/metadata" || path == "/$apps"
        || path == "/token"
        || path.starts_with("/.well-known/")
        || crate::apps::is_app_path(&state, path)
    {
        return Ok(next.run(request).await);
    }

    // `GET /` is the dashboard page; `POST /` is the FHIR system endpoint
    // (transaction/batch Bundles) and takes the same credentials as any API call.
    let is_dashboard = (path == "/" && request.method() == Method::GET)
        || path == "/$status" || path.starts_with("/$browse")
        || path == "/ui" || path.starts_with("/ui/");

    let client_ip = request
        .extensions()
        .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
        .map(|ci| ci.0.ip());

    // Brute-force guard: an address with too many recent failed logins is
    // refused for the rest of the window.
    if let Some(ip) = client_ip
        && failed_logins::is_locked(ip)
    {
        let outcome = OperationOutcome::new(
            sazare_core::operation_outcome::IssueSeverity::Error,
            sazare_core::operation_outcome::IssueType::Throttled,
            "Too many failed authentication attempts; try again later",
        );
        return Err((StatusCode::TOO_MANY_REQUESTS, axum::Json(outcome)).into_response());
    }
    let ip_str = client_ip.map(|ip| ip.to_string()).unwrap_or_else(|| "unknown".to_string());
    let fail = |resp: Response| -> Response {
        audit::log_auth_attempt(&ip_str, None, false);
        if let Some(ip) = client_ip {
            failed_logins::record(ip);
        }
        resp
    };

    // Extract authorization header
    let auth_header = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok());

    let Some(auth_header) = auth_header else {
        if is_dashboard {
            // Browser will show native login dialog
            return Err(Response::builder()
                .status(StatusCode::UNAUTHORIZED)
                .header(header::WWW_AUTHENTICATE, "Basic realm=\"sazare\"")
                .body(Body::empty())
                .unwrap());
        }
        let outcome = OperationOutcome::unauthorized("Missing Authorization header");
        return Err((StatusCode::UNAUTHORIZED, axum::Json(outcome)).into_response());
    };

    // Attempt to authenticate
    let auth_user = if is_dashboard {
        // Dashboard only accepts Basic auth (browser dialog)
        if auth_header.starts_with("Basic ") {
            authenticate_basic(&state.config, auth_header).await.map_err(|_| {
                fail(
                    Response::builder()
                        .status(StatusCode::UNAUTHORIZED)
                        .header(header::WWW_AUTHENTICATE, "Basic realm=\"sazare\"")
                        .body(Body::empty())
                        .unwrap(),
                )
            })?
        } else {
            return Err(Response::builder()
                .status(StatusCode::UNAUTHORIZED)
                .header(header::WWW_AUTHENTICATE, "Basic realm=\"sazare\"")
                .body(Body::empty())
                .unwrap());
        }
    } else if auth_header.starts_with("Bearer ") {
        authenticate_bearer(&state, auth_header).await.map_err(fail)?
    } else if auth_header.starts_with("Basic ") {
        authenticate_basic(&state.config, auth_header).await.map_err(fail)?
    } else {
        let outcome =
            OperationOutcome::unauthorized("Invalid Authorization header format. Use 'Bearer <token>' or 'Basic <credentials>'");
        return Err((StatusCode::UNAUTHORIZED, axum::Json(outcome)).into_response());
    };

    // CSRF: browsers resend cached Basic credentials automatically, even on a
    // request another site triggers. Refuse state-changing requests the
    // browser itself marks as cross-site (non-browser clients send no
    // Sec-Fetch-Site and are unaffected; the dashboard is same-origin).
    let unsafe_method = !matches!(*request.method(), Method::GET | Method::HEAD | Method::OPTIONS);
    if unsafe_method
        && auth_user.auth_type == AuthType::BasicAuth
        && request
            .headers()
            .get("sec-fetch-site")
            .and_then(|v| v.to_str().ok())
            == Some("cross-site")
    {
        let outcome = OperationOutcome::forbidden("Cross-site request refused");
        return Err((StatusCode::FORBIDDEN, axum::Json(outcome)).into_response());
    }

    // Server administration (rebuilding the index, loading sample data) is
    // for operators, not for app tokens.
    if matches!(path, "/$reindex" | "/$demo" | "/metrics") && !is_admin(&auth_user) {
        let outcome = OperationOutcome::forbidden(
            "This operation requires an API key, Basic auth, or a token with a system/*.write scope",
        );
        return Err((StatusCode::FORBIDDEN, axum::Json(outcome)).into_response());
    }

    // Scope check for JWT users
    if auth_user.auth_type == AuthType::Jwt {
        let method = request.method().clone();
        let path = request.uri().path().to_string();
        if let Some((resource_type, perm)) = extract_resource_action(&method, &path) {
            if auth_user.grant(&resource_type, perm).is_none() {
                let outcome = OperationOutcome::forbidden(format!(
                    "Insufficient scope: {resource_type} needs the '{perm}' permission"
                ));
                return Err((StatusCode::FORBIDDEN, axum::Json(outcome)).into_response());
            }
            if let Err(msg) = patient_scope_may(&auth_user, &resource_type, perm) {
                let outcome = OperationOutcome::forbidden(msg);
                return Err((StatusCode::FORBIDDEN, axum::Json(outcome)).into_response());
            }
        }
    }

    // Log successful authentication
    audit::log_auth_attempt(&ip_str, Some(&auth_user.user_id), true);

    // Insert auth user into request extensions
    request.extensions_mut().insert(auth_user);

    Ok(next.run(request).await)
}

/// Authenticate using Bearer token (API key first, then JWT fallback)
#[allow(clippy::result_large_err)]
async fn authenticate_bearer(state: &Arc<AppState>, auth_header: &str) -> Result<AuthUser, Response> {
    let token = auth_header.trim_start_matches("Bearer ").trim();

    // Try API key match first
    // Keys are compared as SHA-256 digests (`key_sha256`), or as stored when
    // a plaintext `key` is configured. API keys are long random values, so a
    // fast hash is the right tool (unlike passwords).
    let token_digest = credentials::sha256_hex(token);
    for api_key in &state.config.auth.api_keys {
        let ok = match (&api_key.key, &api_key.key_sha256) {
            (_, Some(digest)) => ct_eq(&digest.to_ascii_lowercase(), &token_digest),
            (Some(key), None) => ct_eq(key, token),
            (None, None) => false,
        };
        if ok {
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
        let fetch_failed = |e: String| {
            let outcome = OperationOutcome::storage_error(e);
            (StatusCode::INTERNAL_SERVER_ERROR, axum::Json(outcome)).into_response()
        };
        let mut jwks = get_jwks(jwk_url, jwk_cache, false).await.map_err(fetch_failed)?;

        // Decode JWT header to get kid
        let header = jsonwebtoken::decode_header(token).map_err(|e| {
            let outcome = OperationOutcome::unauthorized(format!("Invalid JWT header: {}", e));
            (StatusCode::UNAUTHORIZED, axum::Json(outcome)).into_response()
        })?;

        // A token naming a kid gets exactly that key — refetching once if the
        // cache predates a key rotation. Only a token without a kid may use
        // the set's single key.
        let jwk = match header.kid.as_deref() {
            Some(kid) => {
                if !jwks.keys.iter().any(|k| k.common.key_id.as_deref() == Some(kid)) {
                    jwks = get_jwks(jwk_url, jwk_cache, true).await.map_err(fetch_failed)?;
                }
                jwks.keys.iter().find(|k| k.common.key_id.as_deref() == Some(kid))
            }
            None if jwks.keys.len() == 1 => jwks.keys.first(),
            None => None,
        }
        .ok_or_else(|| {
            let outcome = OperationOutcome::unauthorized("No matching JWK found for the token's kid");
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
        let pem = read_key_file(key_file).map_err(|e| {
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
async fn authenticate_basic(config: &ServerConfig, auth_header: &str) -> Result<AuthUser, Response> {
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
        if user.username != username {
            continue;
        }
        let ok = match (&user.password, &user.password_hash) {
            (_, Some(hash)) => credentials::verify_password_async(username, password, hash).await,
            (Some(plain), None) => ct_eq(plain, password),
            (None, None) => false,
        };
        if ok {
            return Ok(AuthUser::new(username.to_string(), AuthType::BasicAuth));
        }
    }

    let outcome = OperationOutcome::unauthorized("Invalid username or password");
    Err((StatusCode::UNAUTHORIZED, axum::Json(outcome)).into_response())
}

/// Hashed credentials: Argon2id for passwords, SHA-256 for API keys.
pub mod credentials {
    use argon2::password_hash::{rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
    use argon2::Argon2;
    use sha2::{Digest, Sha256};
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    use std::time::{Duration, Instant};

    pub fn sha256_hex(s: &str) -> String {
        Sha256::digest(s.as_bytes()).iter().map(|b| format!("{b:02x}")).collect()
    }

    /// An Argon2id hash (PHC string) of `password`, for `password_hash`.
    pub fn hash_password(password: &str) -> Result<String, String> {
        let salt = SaltString::generate(&mut OsRng);
        Argon2::default()
            .hash_password(password.as_bytes(), &salt)
            .map(|h| h.to_string())
            .map_err(|e| e.to_string())
    }

    /// Whether `hash` is a well-formed Argon2 PHC string (checked at startup, so
    /// a truncated hash fails there rather than at every login).
    pub fn is_argon2_hash(hash: &str) -> bool {
        PasswordHash::new(hash).is_ok_and(|h| {
            h.algorithm.as_str().starts_with("argon2") && h.salt.is_some() && h.hash.is_some_and(|o| o.len() >= 16)
        })
    }

    /// Argon2 verifications running at once. Each takes tens of milliseconds of
    /// CPU and ~19 MB of memory; unbounded, a burst of bad logins could
    /// exhaust both.
    const MAX_CONCURRENT_VERIFY: usize = 4;

    /// [`verify_password`] off the async worker threads (Argon2 is CPU-bound on
    /// purpose), at most [`MAX_CONCURRENT_VERIFY`] at a time.
    pub async fn verify_password_async(user: &str, password: &str, hash: &str) -> bool {
        static SLOTS: OnceLock<tokio::sync::Semaphore> = OnceLock::new();
        let (user, password, hash) = (user.to_string(), password.to_string(), hash.to_string());
        if remembered(&user, &password, &hash) {
            return true;
        }
        let Ok(_slot) = SLOTS.get_or_init(|| tokio::sync::Semaphore::new(MAX_CONCURRENT_VERIFY)).acquire().await else {
            return false;
        };
        tokio::task::spawn_blocking(move || verify_password(&user, &password, &hash))
            .await
            .unwrap_or(false)
    }

    fn cache_key(user: &str, password: &str, hash: &str) -> (String, String) {
        (user.to_string(), sha256_hex(&format!("{hash}\0{password}")))
    }

    fn remembered(user: &str, password: &str, hash: &str) -> bool {
        let c = cache().lock().unwrap_or_else(|e| e.into_inner());
        c.get(&cache_key(user, password, hash)).is_some_and(|t| t.elapsed() < REMEMBER)
    }

    /// A new random API key (256 bits, hex).
    pub fn new_api_key() -> String {
        use argon2::password_hash::rand_core::RngCore;
        let mut bytes = [0u8; 32];
        OsRng.fill_bytes(&mut bytes);
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    /// How long a successful password check is remembered: Argon2 is slow on
    /// purpose (tens of ms), and Basic auth sends the password with every
    /// request.
    const REMEMBER: Duration = Duration::from_secs(300);

    fn cache() -> &'static Mutex<HashMap<(String, String), Instant>> {
        static C: OnceLock<Mutex<HashMap<(String, String), Instant>>> = OnceLock::new();
        C.get_or_init(Default::default)
    }

    /// Check `password` against an Argon2 `hash`. Successful checks are cached
    /// (keyed by user and a SHA-256 of hash+password, never the password) for
    /// [`REMEMBER`]; failures are not.
    pub fn verify_password(user: &str, password: &str, hash: &str) -> bool {
        let key = cache_key(user, password, hash);
        if remembered(user, password, hash) {
            return true;
        }
        let ok = PasswordHash::new(hash)
            .map(|h| Argon2::default().verify_password(password.as_bytes(), &h).is_ok())
            .unwrap_or(false);
        if ok {
            let mut c = cache().lock().unwrap_or_else(|e| e.into_inner());
            if c.len() > 1000 {
                c.retain(|_, t| t.elapsed() < REMEMBER);
            }
            c.insert(key, Instant::now());
        }
        ok
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn password_hash_round_trip() {
            let h = hash_password("correct horse").unwrap();
            assert!(is_argon2_hash(&h));
            assert!(!is_argon2_hash("$argon2id$garbage"));
            assert!(!is_argon2_hash("$argon2id$v=19$broken"));
            assert!(!is_argon2_hash("$argon2id$v=19$m=19456,t=2,p=1$c2FsdHNhbHQ"));
            assert!(h.starts_with("$argon2id$"));
            assert!(verify_password("u", "correct horse", &h));
            assert!(verify_password("u", "correct horse", &h)); // cached
            assert!(!verify_password("u", "wrong", &h));
            assert!(!verify_password("u", "correct horse", "not a hash"));
        }

        #[tokio::test]
        async fn async_verification_is_off_thread_and_correct() {
            let h = hash_password("another password").unwrap();
            // Many concurrent bad attempts: all refused, none blocks the runtime.
            let bad: Vec<_> = (0..8).map(|i| {
                let h = h.clone();
                tokio::spawn(async move { verify_password_async("v", &format!("wrong{i}"), &h).await })
            }).collect();
            for t in bad {
                assert!(!t.await.unwrap());
            }
            assert!(verify_password_async("v", "another password", &h).await);
        }

        #[test]
        fn api_key_digest() {
            let k = new_api_key();
            assert_eq!(k.len(), 64);
            assert_eq!(sha256_hex("abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        }
    }
}

/// Per-address failed-login counter: `MAX_FAILURES` failures within `WINDOW`
/// lock the address out until the window ends. In memory, per process.
///
/// Loopback is never locked: behind a reverse proxy every request comes from
/// 127.0.0.1, and locking it would lock out every user (rate-limit at the
/// proxy instead). Failures are still audit-logged.
pub(crate) mod failed_logins {
    use std::collections::HashMap;
    use std::net::IpAddr;
    use std::sync::{Mutex, OnceLock};
    use std::time::{Duration, Instant};

    pub const MAX_FAILURES: u32 = 10;
    pub const WINDOW: Duration = Duration::from_secs(60);

    fn table() -> &'static Mutex<HashMap<IpAddr, (u32, Instant)>> {
        static T: OnceLock<Mutex<HashMap<IpAddr, (u32, Instant)>>> = OnceLock::new();
        T.get_or_init(|| Mutex::new(HashMap::new()))
    }

    pub fn record(ip: IpAddr) {
        if ip.is_loopback() {
            return;
        }
        let mut t = table().lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        // Keep the table bounded: drop expired windows when it grows.
        if t.len() > 10_000 {
            t.retain(|_, (_, start)| now.duration_since(*start) < WINDOW);
        }
        let e = t.entry(ip).or_insert((0, now));
        if now.duration_since(e.1) >= WINDOW {
            *e = (0, now);
        }
        e.0 += 1;
    }

    pub fn is_locked(ip: IpAddr) -> bool {
        let t = table().lock().unwrap_or_else(|e| e.into_inner());
        t.get(&ip)
            .is_some_and(|(n, start)| *n >= MAX_FAILURES && start.elapsed() < WINDOW)
    }
}

/// Operators: API keys and Basic users (configured by the operator), or a JWT
/// with a system-level write scope on all resources (`system/*.write`,
/// `system/*.*`, `system/*.cud`, `system/*.cruds`).
pub fn is_admin(user: &AuthUser) -> bool {
    match user.auth_type {
        AuthType::ApiKey | AuthType::BasicAuth => true,
        AuthType::Jwt => has_system_scope(&user.scopes, "cud"),
    }
}

/// Writes a patient-scoped (patient app) token may not make even when its
/// scopes match: Subscriptions fire for every patient's data, so creating one
/// would reach beyond the patient's compartment.
pub fn patient_scope_may(user: &AuthUser, resource_type: &str, perm: char) -> Result<(), String> {
    if resource_type == "Subscription" && perm != 'r' && perm != 's' && user.compartment_restricted("Subscription", perm) {
        return Err("Patient-scoped tokens may not create or change Subscriptions".to_string());
    }
    Ok(())
}

/// The resource type and SMART permission (c, r, u, d, s) a request needs.
/// `None` for paths that are not resource interactions (authorized elsewhere).
fn extract_resource_action(method: &Method, path: &str) -> Option<(String, char)> {
    let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    let first = *segments.first()?;

    // Skip non-resource paths. The Bulk Data operation endpoints ($export and
    // its async status/file/cancel paths) are gated by token presence + system
    // scope at the operation level, not by per-resource CRUD scope.
    if matches!(
        first,
        "health"
            | "metadata"
            | "$export"
            | "$export-status"
            | "$export-file"
            | "$import"
            | "$status"
            | ".well-known"
            | "apps"
            | "$apps"
    ) {
        return None;
    }
    // Operation-level bulk export on a resource type, e.g. Patient/$export or
    // Group/{id}/$export.
    if segments.last() == Some(&"$export") {
        return None;
    }
    // The WebSocket channel delivers Subscription notifications.
    if first == "ws" {
        return Some(("Subscription".to_string(), 'r'));
    }

    let second = segments.get(1).copied();
    let last = segments.last().copied().unwrap_or("");
    let perm = match *method {
        Method::GET | Method::HEAD => match (segments.len(), second) {
            (1, _) | (2, Some("_history")) | (2, Some("_search")) => 's',
            _ => 'r',
        },
        Method::POST => {
            if second == Some("_search") {
                's'
            } else if matches!(last, "$validate" | "$everything") {
                'r'
            } else {
                'c'
            }
        }
        Method::PUT | Method::PATCH => 'u',
        Method::DELETE => 'd',
        _ => return None,
    };
    Some((first.to_string(), perm))
}

/// Constant-time byte comparison for secrets, to avoid leaking how many leading
/// bytes of an API key / password matched via response timing. (Length is not
/// hidden, which is acceptable for these credentials.)
fn ct_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_failed_logins_lock_after_limit_but_never_loopback() {
        let ip: std::net::IpAddr = "203.0.113.77".parse().unwrap();
        for _ in 0..failed_logins::MAX_FAILURES - 1 {
            failed_logins::record(ip);
        }
        assert!(!failed_logins::is_locked(ip));
        failed_logins::record(ip);
        assert!(failed_logins::is_locked(ip));

        let lo: std::net::IpAddr = "127.0.0.1".parse().unwrap();
        for _ in 0..failed_logins::MAX_FAILURES * 2 {
            failed_logins::record(lo);
        }
        assert!(!failed_logins::is_locked(lo));
    }
    use crate::config::{ApiKey, AuthSettings, BasicAuthUser, JwtSettings};

    fn test_config() -> ServerConfig {
        ServerConfig {
            auth: AuthSettings {
                enabled: true,
                api_keys: vec![ApiKey {
                    name: "test-client".to_string(),
                    key: Some("test-api-key-12345".to_string()),
                    key_sha256: None,
                }],
                basic_auth: vec![BasicAuthUser {
                    username: "admin".to_string(),
                    password: Some("admin123".to_string()),
                    password_hash: None,
                }],
                jwt: None,
                smart: None,
            },
            ..ServerConfig::default()
        }
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
            app_names: Vec::new(),
            ws_registry: Arc::new(crate::websocket::WsRegistry::new()),
            webhook: Arc::new(crate::webhook::WebhookManager::new(Default::default())),
            export_jobs: Arc::new(crate::bulk_export::ExportJobs::new()),
            seen_jti: std::sync::Mutex::new(std::collections::HashMap::new()),
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

    #[tokio::test]
    async fn test_authenticate_basic_valid() {
        let config = test_config();
        let credentials = STANDARD.encode("admin:admin123");
        let result = authenticate_basic(&config, &format!("Basic {}", credentials)).await;
        assert!(result.is_ok());
        let auth_user = result.unwrap();
        assert_eq!(auth_user.user_id, "admin");
        assert_eq!(auth_user.auth_type, AuthType::BasicAuth);
    }

    #[tokio::test]
    async fn test_authenticate_basic_invalid() {
        let config = test_config();
        let credentials = STANDARD.encode("admin:wrongpass");
        let result = authenticate_basic(&config, &format!("Basic {}", credentials)).await;
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

    // --- Scope tests ---

    fn sc(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn test_scope_v1_forms() {
        let s = sc(&["user/Patient.read"]);
        assert_eq!(scope_grant(&s, "Patient", 'r'), Some(Grant::Full));
        assert_eq!(scope_grant(&s, "Patient", 's'), Some(Grant::Full));
        assert_eq!(scope_grant(&s, "Patient", 'c'), None);
        assert_eq!(scope_grant(&s, "Observation", 'r'), None);
        let s = sc(&["user/*.write"]);
        for p in ['c', 'u', 'd'] {
            assert_eq!(scope_grant(&s, "Observation", p), Some(Grant::Full));
        }
        assert_eq!(scope_grant(&s, "Observation", 'r'), None);
        let s = sc(&["system/*.*"]);
        for p in "cruds".chars() {
            assert_eq!(scope_grant(&s, "Anything", p), Some(Grant::Full));
        }
        assert_eq!(scope_grant(&[], "Patient", 'r'), None);
    }

    #[test]
    fn test_scope_v2_forms() {
        let s = sc(&["patient/Observation.rs"]);
        assert_eq!(scope_grant(&s, "Observation", 'r'), Some(Grant::Patient));
        assert_eq!(scope_grant(&s, "Observation", 's'), Some(Grant::Patient));
        assert_eq!(scope_grant(&s, "Observation", 'c'), None);
        let s = sc(&["user/Patient.cruds"]);
        assert_eq!(scope_grant(&s, "Patient", 'd'), Some(Grant::Full));
        // Out of order, unknown letters, granular (query) scopes grant nothing.
        assert_eq!(scope_grant(&sc(&["user/Patient.sr"]), "Patient", 'r'), None);
        assert_eq!(scope_grant(&sc(&["user/Patient.rx"]), "Patient", 'r'), None);
        assert_eq!(scope_grant(&sc(&["user/Observation.rs?category=laboratory"]), "Observation", 'r'), None);
        assert_eq!(scope_grant(&sc(&["admin/*.*"]), "Patient", 'r'), None);
    }

    #[test]
    fn test_system_scope_and_admin() {
        assert!(has_system_scope(&sc(&["system/*.read"]), "r"));
        assert!(!has_system_scope(&sc(&["user/*.read"]), "r"));
        assert!(!has_system_scope(&sc(&["system/Patient.read"]), "r"));
        let jwt = |s: &[&str]| AuthUser::with_scopes("t".into(), AuthType::Jwt, sc(s));
        assert!(is_admin(&jwt(&["system/*.write"])));
        assert!(is_admin(&jwt(&["system/*.cruds"])));
        assert!(is_admin(&jwt(&["system/*.cud"])));
        assert!(!is_admin(&jwt(&["user/*.write"])));
        assert!(!is_admin(&jwt(&["system/*.read"])));
    }

    #[test]
    fn test_compartment_restriction_is_per_type_and_permission() {
        let u = AuthUser::with_scopes(
            "t".into(),
            AuthType::Jwt,
            sc(&["patient/Observation.rs", "user/Practitioner.rs"]),
        );
        assert!(u.compartment_restricted("Observation", 'r'));
        assert!(!u.compartment_restricted("Practitioner", 'r'));
        // A type no scope covers stays restricted.
        assert!(u.compartment_restricted("Encounter", 'r'));
        let u = AuthUser::with_scopes("t".into(), AuthType::Jwt, sc(&["patient/*.rs", "user/*.rs"]));
        assert!(!u.compartment_restricted("Observation", 'r'));
        // Mixed per permission on one type: user/ create doesn't lift the
        // patient/ restriction on reading.
        let u = AuthUser::with_scopes(
            "t".into(),
            AuthType::Jwt,
            sc(&["patient/Observation.r", "user/Observation.c"]),
        );
        assert!(u.compartment_restricted("Observation", 'r'));
        assert!(!u.compartment_restricted("Observation", 'c'));
        let u = AuthUser::new("k".into(), AuthType::ApiKey);
        assert!(!u.compartment_restricted("Observation", 'r'));
    }

    // --- extract_resource_action tests ---

    fn act(m: Method, p: &str) -> Option<(String, char)> {
        extract_resource_action(&m, p)
    }

    #[test]
    fn test_extract_resource_action() {
        let pa = |c| Some(("Patient".to_string(), c));
        assert_eq!(act(Method::GET, "/Patient/123"), pa('r'));
        assert_eq!(act(Method::GET, "/Patient/123/_history/2"), pa('r'));
        assert_eq!(act(Method::GET, "/Patient"), pa('s'));
        assert_eq!(act(Method::GET, "/Patient/_history"), pa('s'));
        assert_eq!(act(Method::POST, "/Patient/_search"), pa('s'));
        assert_eq!(act(Method::POST, "/Patient/$validate"), pa('r'));
        assert_eq!(act(Method::POST, "/Patient"), pa('c'));
        assert_eq!(act(Method::PUT, "/Patient/123"), pa('u'));
        assert_eq!(act(Method::PATCH, "/Patient/123"), pa('u'));
        assert_eq!(act(Method::DELETE, "/Patient/123"), pa('d'));
        assert_eq!(act(Method::GET, "/ws"), Some(("Subscription".to_string(), 'r')));
        assert!(act(Method::GET, "/metadata").is_none());
        assert!(act(Method::GET, "/health").is_none());
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
