//! SMART Backend Services token endpoint.
//!
//! `POST /token` implements the OAuth2 client-credentials grant with
//! `private_key_jwt` client authentication (SMART Backend Services). A client
//! presents a JWT assertion signed by its private key; the server verifies it
//! against the client's registered public JWKS and issues a short-lived bearer
//! access token (HS256, signed with the configured `auth.jwt.secret`) that the
//! normal auth middleware then validates.

use std::sync::Arc;

use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    Form, Json,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use jsonwebtoken::{jwk::JwkSet, Algorithm, DecodingKey, EncodingKey, Header, Validation};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::config::BackendClient;
use crate::AppState;

const JWT_BEARER: &str = "urn:ietf:params:oauth:client-assertion-type:jwt-bearer";

#[derive(Deserialize)]
pub struct TokenRequest {
    grant_type: String,
    #[serde(default)]
    client_assertion_type: Option<String>,
    #[serde(default)]
    client_assertion: Option<String>,
    #[serde(default)]
    scope: Option<String>,
}

#[derive(Serialize)]
struct AccessTokenClaims<'a> {
    iss: &'a str,
    sub: &'a str,
    scope: &'a str,
    exp: u64,
    iat: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    aud: Option<&'a str>,
}

fn oauth_error(status: StatusCode, error: &str, desc: &str) -> Response {
    (status, Json(json!({"error": error, "error_description": desc}))).into_response()
}

/// Decode a JWT payload without verifying the signature (to read `iss`/`kid`).
fn unverified_payload(jwt: &str) -> Option<Value> {
    let payload = jwt.split('.').nth(1)?;
    let bytes = URL_SAFE_NO_PAD.decode(payload).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Fetch a client's public JWKS, preferring the inline value over the URL.
async fn client_jwks(client: &BackendClient) -> Result<JwkSet, String> {
    if let Some(inline) = &client.jwks {
        return serde_json::from_value(inline.clone()).map_err(|e| format!("bad inline jwks: {e}"));
    }
    if let Some(url) = &client.jwks_url {
        let resp = reqwest::get(url)
            .await
            .map_err(|e| format!("jwks fetch failed: {e}"))?;
        return resp
            .json::<JwkSet>()
            .await
            .map_err(|e| format!("bad jwks json: {e}"));
    }
    Err("client has no jwks or jwks_url".into())
}

fn now_secs(state: &AppState) -> u64 {
    let _ = state;
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Grant the requested scopes, restricted to the client's allowlist if it has one.
fn grant_scopes(requested: &str, client: &BackendClient) -> String {
    let requested: Vec<&str> = requested.split_whitespace().collect();
    if client.allowed_scopes.is_empty() {
        return requested.join(" ");
    }
    requested
        .into_iter()
        .filter(|s| client.allowed_scopes.iter().any(|a| a == s))
        .collect::<Vec<_>>()
        .join(" ")
}

/// `POST /token` — SMART Backend Services client-credentials grant.
pub async fn token(State(state): State<Arc<AppState>>, Form(req): Form<TokenRequest>) -> Response {
    let Some(smart) = state.config.auth.smart.as_ref() else {
        return oauth_error(
            StatusCode::NOT_FOUND,
            "invalid_request",
            "Backend Services token endpoint is not configured",
        );
    };
    let Some(jwt_cfg) = state.config.auth.jwt.as_ref() else {
        return oauth_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "server_error",
            "No JWT signing secret configured",
        );
    };
    let Some(secret) = jwt_cfg.secret.as_ref() else {
        return oauth_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "server_error",
            "No JWT signing secret configured",
        );
    };

    if req.grant_type != "client_credentials" {
        return oauth_error(
            StatusCode::BAD_REQUEST,
            "unsupported_grant_type",
            "Only client_credentials is supported",
        );
    }
    if req.client_assertion_type.as_deref() != Some(JWT_BEARER) {
        return oauth_error(
            StatusCode::BAD_REQUEST,
            "invalid_client",
            "client_assertion_type must be jwt-bearer",
        );
    }
    let Some(assertion) = req.client_assertion.as_deref() else {
        return oauth_error(
            StatusCode::BAD_REQUEST,
            "invalid_client",
            "client_assertion is required",
        );
    };

    // Identify the client from the (unverified) assertion `iss`.
    let Some(payload) = unverified_payload(assertion) else {
        return oauth_error(StatusCode::BAD_REQUEST, "invalid_client", "Malformed assertion");
    };
    let client_id = payload.get("iss").and_then(|v| v.as_str()).unwrap_or("");
    let Some(client) = smart.backend_clients.iter().find(|c| c.client_id == client_id) else {
        return oauth_error(StatusCode::BAD_REQUEST, "invalid_client", "Unknown client");
    };

    // Resolve the client's signing key (by kid) from its JWKS.
    let jwks = match client_jwks(client).await {
        Ok(j) => j,
        Err(e) => return oauth_error(StatusCode::BAD_REQUEST, "invalid_client", &e),
    };
    let header = match jsonwebtoken::decode_header(assertion) {
        Ok(h) => h,
        Err(_) => {
            return oauth_error(StatusCode::BAD_REQUEST, "invalid_client", "Bad assertion header")
        }
    };
    let jwk = header
        .kid
        .as_ref()
        .and_then(|kid| jwks.keys.iter().find(|k| k.common.key_id.as_deref() == Some(kid)))
        .or_else(|| jwks.keys.first());
    let Some(jwk) = jwk else {
        return oauth_error(StatusCode::BAD_REQUEST, "invalid_client", "No matching key in JWKS");
    };
    let decoding_key = match DecodingKey::from_jwk(jwk) {
        Ok(k) => k,
        Err(e) => {
            return oauth_error(StatusCode::BAD_REQUEST, "invalid_client", &format!("Bad JWK: {e}"))
        }
    };

    // Verify the assertion: signature, audience (= token endpoint), and expiry.
    let mut validation = Validation::new(header.alg);
    let token_endpoint = smart
        .token_endpoint
        .clone()
        .or_else(|| jwt_cfg.issuer.as_ref().map(|i| format!("{i}/token")));
    if let Some(aud) = &token_endpoint {
        validation.set_audience(&[aud]);
    } else {
        validation.validate_aud = false;
    }
    validation.set_required_spec_claims(&["exp"]);
    if let Err(e) = jsonwebtoken::decode::<Value>(assertion, &decoding_key, &validation) {
        return oauth_error(
            StatusCode::BAD_REQUEST,
            "invalid_client",
            &format!("Assertion verification failed: {e}"),
        );
    }

    // Issue the access token.
    let requested_scope = req.scope.as_deref().unwrap_or("system/*.read");
    let granted = grant_scopes(requested_scope, client);
    if granted.is_empty() {
        return oauth_error(
            StatusCode::BAD_REQUEST,
            "invalid_scope",
            "None of the requested scopes are allowed for this client",
        );
    }

    let ttl = smart.token_ttl_secs.unwrap_or(300);
    let iat = now_secs(&state);
    let claims = AccessTokenClaims {
        iss: jwt_cfg.issuer.as_deref().unwrap_or("sazare"),
        sub: client_id,
        scope: &granted,
        iat,
        exp: iat + ttl,
        aud: jwt_cfg.audience.as_deref(),
    };
    let access_token = match jsonwebtoken::encode(
        &Header::new(Algorithm::HS256),
        &claims,
        &EncodingKey::from_secret(secret.as_bytes()),
    ) {
        Ok(t) => t,
        Err(e) => {
            return oauth_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "server_error",
                &format!("Token signing failed: {e}"),
            )
        }
    };

    Json(json!({
        "access_token": access_token,
        "token_type": "bearer",
        "expires_in": ttl,
        "scope": granted,
    }))
    .into_response()
}
