use axum::{
    extract::{Query, State},
    response::{IntoResponse, Json},
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;

/// Query parameters for `GET /metadata`. `mode=terminology` returns a
/// TerminologyCapabilities resource; anything else (`full` / `normative` /
/// absent) returns the CapabilityStatement.
#[derive(Deserialize, Default)]
pub struct MetadataQuery {
    pub mode: Option<String>,
}

use crate::AppState;
use sazare_core::SearchParamRegistry;

/// Bulk Data `$export` operations declared on a resource type's CapabilityStatement
/// entry, so Inferno's Bulk Data tests see patient/group-level export support.
fn bulk_export_operations_for(resource_type: &str) -> Vec<Value> {
    let def = match resource_type {
        "Patient" => "http://hl7.org/fhir/uv/bulkdata/OperationDefinition/patient-export",
        "Group" => "http://hl7.org/fhir/uv/bulkdata/OperationDefinition/group-export",
        _ => return vec![],
    };
    vec![json!({"name": "export", "definition": def})]
}

/// Health check (GET /health)
pub async fn health_check(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    // `index.state` is "rebuilding" (with progress) while a new index generation
    // is built; the server is up and answering either way.
    Json(json!({
        "status": "ok",
        "version": env!("CARGO_PKG_VERSION"),
        "fhirVersion": "4.0.1",
        "index": state.rebuild.to_json()
    }))
}

/// Minimal TerminologyCapabilities for `GET /metadata?mode=terminology`. sazare
/// uses terminology only for required-binding validation of resources — it is
/// not a terminology server, so this declares no `$expand` / `$validate-code` /
/// `$translate` operations (their absence is the honest signal that they are
/// not offered).
fn terminology_capabilities() -> Value {
    let date = chrono::Utc::now().format("%Y-%m-%d").to_string();
    json!({
        "resourceType": "TerminologyCapabilities",
        "status": "active",
        "date": date,
        "kind": "instance",
        "software": {
            "name": "sazare",
            "version": env!("CARGO_PKG_VERSION"),
        },
        "description": "sazare uses terminology only for required-binding validation of resources. It is not a terminology server: $expand, $validate-code and $translate are not offered."
    })
}

/// Dynamic CapabilityStatement (GET /metadata). `?mode=terminology` returns a
/// TerminologyCapabilities instead (a different resource type, per the spec).
pub async fn capability_statement(
    State(state): State<Arc<AppState>>,
    Query(query): Query<MetadataQuery>,
    headers: axum::http::HeaderMap,
) -> Json<Value> {
    // The address clients reach this server at (proxy headers honored; https
    // under the built-in TLS), not the bind address (0.0.0.0 in Docker).
    let base = super::base_url_from_headers(&headers);
    if query.mode.as_deref() == Some("terminology") {
        return Json(terminology_capabilities());
    }

    let interactions = vec![
        json!({"code": "read"}),
        json!({"code": "vread"}),
        json!({"code": "create"}),
        json!({"code": "update"}),
        json!({"code": "patch"}),
        json!({"code": "delete"}),
        json!({"code": "search-type"}),
        json!({"code": "history-instance"}),
    ];

    // Every R4 resource type: each can be stored and searched.
    let resources: Vec<Value> = sazare_core::r4_types::resource_types()
        .map(|rt| {
            let mut entry = json!({
                "type": rt,
                "versioning": "versioned",
                "readHistory": true,
                "conditionalCreate": true,
                "conditionalUpdate": true,
                "conditionalDelete": "single",
                "searchInclude": include_values(&state.search_param_registry, rt),
                "interaction": interactions,
                "searchParam": get_search_params_from_registry(&state.search_param_registry, rt),
            });
            let profiles = state.profile_registry.supported_profile_urls(rt);
            if !profiles.is_empty() {
                entry["supportedProfile"] = json!(profiles);
            }
            // Every type has $validate; Patient $everything; Patient and Group
            // the Bulk Data exports.
            let mut ops = vec![json!({"name": "validate", "definition": "http://hl7.org/fhir/OperationDefinition/Resource-validate"})];
            if rt == "Patient" {
                ops.push(json!({"name": "everything", "definition": "http://hl7.org/fhir/OperationDefinition/Patient-everything"}));
            }
            ops.extend(bulk_export_operations_for(rt));
            entry["operation"] = json!(ops);
            entry
        })
        .collect();

    // Build security section
    let security = build_security_section(&state.config, &base);

    let mut rest = json!({
        "mode": "server",
        // Advertise the R4 Subscription websocket endpoint so clients can discover it.
        "extension": [{
            "url": "http://hl7.org/fhir/StructureDefinition/capabilitystatement-websocket",
            "valueUri": "/ws"
        }],
        "resource": resources,
        "interaction": [
            {"code": "transaction"},
            {"code": "batch"},
        ],
        "operation": [
            {"name": "export", "definition": "http://hl7.org/fhir/uv/bulkdata/OperationDefinition/export"},
            {"name": "import", "definition": "http://sazare.dev/OperationDefinition/import"},
        ]
    });
    if let Some(sec) = security {
        rest["security"] = sec;
    }

    // `date` is required by the base CapabilityStatement profile. Use the build
    // timestamp's date portion as a stable per-deploy value.
    let date = chrono::Utc::now().format("%Y-%m-%d").to_string();

    Json(json!({
        "resourceType": "CapabilityStatement",
        "status": "active",
        "date": date,
        "kind": "instance",
        "fhirVersion": "4.0.1",
        "format": ["json"],
        "instantiates": [
            "http://hl7.org/fhir/us/core/CapabilityStatement/us-core-server"
        ],
        "software": {
            "name": "sazare",
            "version": env!("CARGO_PKG_VERSION"),
        },
        "implementation": {
            "description": "fhir-sazare - Lightweight FHIR R4 Server",
            "url": base,
        },
        "rest": [rest]
    }))
}

/// Build the security section for CapabilityStatement based on auth config
fn build_security_section(config: &crate::config::ServerConfig, base_url: &str) -> Option<Value> {
    if !config.auth.enabled {
        return None;
    }

    if config.auth.jwt.is_some() {
        // SMART on FHIR OAuth security. The OAuth endpoints are the
        // identity provider's (from its issuer) or, for Backend Services,
        // this server's /token; one not known isn't listed (a valueUri must
        // be a URI).
        let (authorize, token) = oauth_endpoints(config, base_url);
        let mut uris = Vec::new();
        if let Some(a) = authorize {
            uris.push(json!({"url": "authorize", "valueUri": a}));
        }
        if let Some(t) = token {
            uris.push(json!({"url": "token", "valueUri": t}));
        }
        let mut section = json!({
            "service": [
                {
                    "coding": [{
                        "system": "http://terminology.hl7.org/CodeSystem/restful-security-service",
                        "code": "SMART-on-FHIR",
                        "display": "SMART on FHIR"
                    }]
                }
            ],
            "description": format!(
                "OAuth2/SMART on FHIR with JWT validation. SMART configuration: {}/.well-known/smart-configuration",
                base_url
            )
        });
        if !uris.is_empty() {
            section["extension"] = json!([{
                "url": "http://fhir-registry.smarthealthit.org/StructureDefinition/oauth-uris",
                "extension": uris
            }]);
        }
        Some(section)
    } else {
        // Basic/API Key only
        let mut services = vec![];
        if !config.auth.basic_auth.is_empty() {
            services.push(json!({
                "coding": [{
                    "system": "http://terminology.hl7.org/CodeSystem/restful-security-service",
                    "code": "Basic",
                    "display": "Basic Authentication"
                }]
            }));
        }
        if !config.auth.api_keys.is_empty() {
            services.push(json!({
                "coding": [{
                    "system": "http://terminology.hl7.org/CodeSystem/restful-security-service",
                    "code": "OAuth",
                    "display": "API Key (Bearer Token)"
                }]
            }));
        }
        if services.is_empty() {
            return None;
        }
        Some(json!({
            "service": services
        }))
    }
}

/// The OAuth authorize and token endpoints: from the identity provider's
/// issuer, the configured token endpoint, or this server's `/token` when it
/// issues tokens itself (Backend Services).
fn oauth_endpoints(config: &crate::config::ServerConfig, base_url: &str) -> (Option<String>, Option<String>) {
    let jwt = config.auth.jwt.as_ref();
    let issuer = jwt.and_then(|j| j.issuer.clone());
    let authorize = issuer.as_ref().map(|i| format!("{i}/authorize"));
    let token = match config.auth.smart.as_ref() {
        Some(s) => Some(s.token_endpoint.clone().unwrap_or_else(|| format!("{base_url}/token"))),
        None => issuer.map(|i| format!("{i}/token")),
    };
    (authorize, token)
}

/// SMART on FHIR configuration endpoint (GET /.well-known/smart-configuration)
pub async fn smart_configuration(State(state): State<Arc<AppState>>, headers: axum::http::HeaderMap) -> Json<Value> {
    let base = super::base_url_from_headers(&headers);
    let jwt_settings = state.config.auth.jwt.as_ref();
    let smart = state.config.auth.smart.as_ref();

    let issuer = jwt_settings
        .and_then(|j| j.issuer.as_deref())
        .unwrap_or("(not configured)");

    // The token endpoint is live only when Backend Services is configured.
    let token_endpoint = smart.map(|s| {
        s.token_endpoint
            .clone()
            .or_else(|| jwt_settings.and_then(|j| j.issuer.clone()).map(|i| format!("{i}/token")))
            .unwrap_or_else(|| format!("{base}/token"))
    });

    let mut capabilities = vec![
        "launch-standalone",
        "permission-v2",
        "client-confidential-symmetric",
    ];
    let mut grant_types = vec!["authorization_code"];
    if smart.is_some() {
        // SMART Backend Services (server-to-server, asymmetric client auth).
        capabilities.push("client-confidential-asymmetric");
        grant_types.push("client_credentials");
    }

    // authorization_endpoint must be a syntactically valid URI for SMART discovery,
    // even when only Backend Services (which doesn't use it) is implemented.
    let authorization_endpoint = jwt_settings
        .and_then(|j| j.issuer.clone())
        .map(|i| format!("{i}/authorize"))
        .unwrap_or_else(|| "(external - configure in IdP)".to_string());

    let mut config = json!({
        "issuer": issuer,
        "authorization_endpoint": authorization_endpoint,
        "capabilities": capabilities,
        "scopes_supported": [
            "patient/*.read", "patient/*.write",
            "user/*.read", "user/*.write",
            "system/*.read", "system/*.write", "system/*.*"
        ],
        "response_types_supported": ["code"],
        "grant_types_supported": grant_types,
        "code_challenge_methods_supported": ["S256"]
    });

    if let Some(te) = token_endpoint {
        config["token_endpoint"] = json!(te);
        config["token_endpoint_auth_methods_supported"] = json!(["private_key_jwt"]);
        config["token_endpoint_auth_signing_alg_values_supported"] =
            json!(["RS256", "RS384", "ES256", "ES384"]);
    } else {
        config["token_endpoint"] = json!("(external - configure in IdP)");
    }

    Json(config)
}

/// Generate search parameter metadata from the registry
/// `_include` values for a type: each of its reference parameters.
fn include_values(registry: &SearchParamRegistry, resource_type: &str) -> Vec<String> {
    let mut values: Vec<String> = registry
        .get_definitions(resource_type)
        .iter()
        .filter(|d| d.param_type == sazare_core::SearchParamType::Reference)
        .map(|d| format!("{resource_type}:{}", d.name))
        .collect();
    values.sort();
    values.dedup();
    values
}

fn get_search_params_from_registry(registry: &SearchParamRegistry, resource_type: &str) -> Vec<Value> {
    let defs = registry.get_definitions(resource_type);
    let mut params: Vec<Value> = Vec::new();
    let mut seen = std::collections::HashSet::new();

    for def in defs {
        let type_str = match def.param_type {
            sazare_core::SearchParamType::Token => "token",
            sazare_core::SearchParamType::String => "string",
            sazare_core::SearchParamType::Date => "date",
            sazare_core::SearchParamType::Reference => "reference",
            sazare_core::SearchParamType::Number => "number",
            sazare_core::SearchParamType::Quantity => "quantity",
        };
        if seen.insert(def.name.clone()) {
            params.push(json!({"name": def.name, "type": type_str}));
        }
        for alias in &def.aliases {
            if seen.insert(alias.clone()) {
                params.push(json!({"name": alias, "type": type_str}));
            }
        }
    }
    params
}

#[cfg(test)]
mod capability_tests {
    /// Every type with search parameters is an R4 type, and every R4 type has
    /// some (Appointment, Subscription, Medication, … included).
    #[test]
    fn every_r4_type_is_searchable() {
        let registry = sazare_core::SearchParamRegistry::new();
        let not_r4: Vec<&str> = registry.resource_types().filter(|t| !sazare_core::r4_types::is_resource_type(t)).collect();
        assert!(not_r4.is_empty(), "{not_r4:?}");
        for t in ["Appointment", "Subscription", "Medication", "Group"] {
            assert!(registry.has_resource_type(t), "{t}");
        }
        assert!(registry.lookup_param_type("Subscription", "status").is_some());
    }
}
