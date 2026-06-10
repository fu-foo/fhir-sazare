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

/// Maximum accepted lifetime of a client assertion. SMART Backend Services
/// recommends assertions live no longer than ~5 minutes; a far-future `exp`
/// would widen the replay window, so we reject it.
const ASSERTION_MAX_LIFETIME_SECS: u64 = 300;

/// Signature algorithms acceptable for a given JWK, derived from the *key type*
/// — never from the attacker-controlled JWT header. This is the defense against
/// algorithm-confusion: an asymmetric key can only ever verify with its own
/// asymmetric algorithm family, so a forged `alg=HS256` assertion (HMAC'd with
/// the public key bytes) is rejected because HS* is never in this set.
fn allowed_algorithms(jwk: &jsonwebtoken::jwk::Jwk) -> Vec<Algorithm> {
    use jsonwebtoken::jwk::{AlgorithmParameters, EllipticCurve};
    match &jwk.algorithm {
        AlgorithmParameters::RSA(_) => vec![
            Algorithm::RS256,
            Algorithm::RS384,
            Algorithm::RS512,
            Algorithm::PS256,
            Algorithm::PS384,
            Algorithm::PS512,
        ],
        AlgorithmParameters::EllipticCurve(ec) => match ec.curve {
            EllipticCurve::P256 => vec![Algorithm::ES256],
            EllipticCurve::P384 => vec![Algorithm::ES384],
            _ => vec![],
        },
        AlgorithmParameters::OctetKeyPair(_) => vec![Algorithm::EdDSA],
        // Symmetric keys must not appear in a published client JWKS used to
        // verify assertions — refuse rather than enable an HMAC bypass.
        AlgorithmParameters::OctetKey(_) => vec![],
    }
}

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
    // When the header names a `kid`, require an exact match — never silently
    // fall back to an arbitrary key. Only when no `kid` is given do we accept a
    // sole key from the set.
    let jwk = match header.kid.as_ref() {
        Some(kid) => jwks.keys.iter().find(|k| k.common.key_id.as_deref() == Some(kid)),
        None if jwks.keys.len() == 1 => jwks.keys.first(),
        None => None,
    };
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
    // The accepted algorithm is pinned to the JWK's key type — NOT taken from the
    // attacker-supplied `header.alg` — to prevent algorithm-confusion bypass.
    let algs = allowed_algorithms(jwk);
    if algs.is_empty() {
        return oauth_error(
            StatusCode::BAD_REQUEST,
            "invalid_client",
            "Unsupported client key type for assertion verification",
        );
    }
    if !algs.contains(&header.alg) {
        return oauth_error(
            StatusCode::BAD_REQUEST,
            "invalid_client",
            "Assertion algorithm does not match the registered client key",
        );
    }
    let mut validation = Validation::new(algs[0]);
    validation.algorithms = algs;
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
    let assertion_claims = match jsonwebtoken::decode::<Value>(assertion, &decoding_key, &validation) {
        Ok(data) => data.claims,
        Err(e) => {
            return oauth_error(
                StatusCode::BAD_REQUEST,
                "invalid_client",
                &format!("Assertion verification failed: {e}"),
            )
        }
    };

    // SMART Backend Services: `sub` and `iss` must both equal the client_id.
    if assertion_claims.get("sub").and_then(|v| v.as_str()) != Some(client_id) {
        return oauth_error(
            StatusCode::BAD_REQUEST,
            "invalid_client",
            "Assertion 'sub' must equal the client_id",
        );
    }

    // Bound the assertion lifetime to limit the replay window.
    let now = now_secs(&state);
    let exp = assertion_claims.get("exp").and_then(|v| v.as_u64()).unwrap_or(0);
    if exp > now + ASSERTION_MAX_LIFETIME_SECS {
        return oauth_error(
            StatusCode::BAD_REQUEST,
            "invalid_client",
            "Assertion expiry is too far in the future",
        );
    }

    // One-time use: reject a replayed `jti` and record this one until it expires.
    let Some(jti) = assertion_claims.get("jti").and_then(|v| v.as_str()) else {
        return oauth_error(
            StatusCode::BAD_REQUEST,
            "invalid_client",
            "Assertion is missing the required 'jti' claim",
        );
    };
    {
        let mut seen = match state.seen_jti.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        seen.retain(|_, &mut e| e > now);
        if seen.contains_key(jti) {
            return oauth_error(
                StatusCode::BAD_REQUEST,
                "invalid_client",
                "Assertion 'jti' has already been used (replay)",
            );
        }
        seen.insert(jti.to_string(), exp.max(now + ASSERTION_MAX_LIFETIME_SECS));
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

#[cfg(test)]
mod tests {
    use super::*;
    use jsonwebtoken::jwk::Jwk;

    fn jwk_from(json: serde_json::Value) -> Jwk {
        serde_json::from_value(json).expect("valid jwk")
    }

    #[test]
    fn ec_p256_key_never_allows_hmac() {
        // An EC public key must only ever verify with ES256 — never an HMAC
        // algorithm, which is the algorithm-confusion bypass we defend against.
        let jwk = jwk_from(json!({
            "kty": "EC", "crv": "P-256", "alg": "ES256", "use": "sig", "kid": "k1",
            "x": "f83OJ3D2xF1Bg8vub9tLe1gHMzV76e8Tus9uPHvRVEU",
            "y": "x_FEzRu9m36HLN_tue659LNpXW6pCyStikYjKIWI5a0"
        }));
        let algs = allowed_algorithms(&jwk);
        assert_eq!(algs, vec![Algorithm::ES256]);
        assert!(!algs.contains(&Algorithm::HS256));
    }

    #[test]
    fn rsa_key_allows_only_rsa_family() {
        let jwk = jwk_from(json!({
            "kty": "RSA", "alg": "RS256", "use": "sig", "kid": "r1",
            "n": "0vx7agoebGcQSuuPiLJXZptN9nndrQmbXEps2aiAFbWhM78LhWx4cbbfAAtVT86zwu1RK7aPFFxuhDR1L6tSoc_BJECPebWKRXjBZCiFV4n3oknjhMstn64tZ_2W-5JsGY4Hc5n9yBXArwl93lqt7_RN5w6Cf0h4QyQ5v-65YGjQR0_FDW2QvzqY368QQMicAtaSqzs8KJZgnYb9c7d0zgdAZHzu6qMQvRL5hajrn1n91CbOpbISD08qNLyrdkt-bFTWhAI4vMQFh6WeZu0fM4lFd2NcRwr3XPksINHaQ-G_xBniIqbw0Ls1jF44-csFCur-kEgU8awapJzKnqDKgw",
            "e": "AQAB"
        }));
        let algs = allowed_algorithms(&jwk);
        assert!(algs.contains(&Algorithm::RS256));
        assert!(!algs.contains(&Algorithm::HS256));
        assert!(!algs.contains(&Algorithm::ES256));
    }

    #[test]
    fn symmetric_key_is_rejected() {
        // A symmetric key in a published client JWKS must never be usable.
        let jwk = jwk_from(json!({
            "kty": "oct", "alg": "HS256", "use": "sig", "kid": "s1",
            "k": "GawgguFyGrWKav7AX4VKUg"
        }));
        assert!(allowed_algorithms(&jwk).is_empty());
    }
}
