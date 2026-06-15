use axum::{
    extract::State,
    response::{IntoResponse, Json},
};
use serde_json::{json, Value};
use std::sync::Arc;

use crate::AppState;
use sazare_core::SearchParamRegistry;

/// FHIR supported resource types
pub const SUPPORTED_RESOURCE_TYPES: &[&str] = &[
    "Patient",
    "Observation",
    "Encounter",
    "Condition",
    "Task",
    "Practitioner",
    "Organization",
    "AllergyIntolerance",
    "DiagnosticReport",
    "Immunization",
    "MedicationRequest",
    "Procedure",
    "Bundle",
    "Provenance",
    "CarePlan",
    "CareTeam",
    "RelatedPerson",
    "Location",
    "PractitionerRole",
    "Goal",
    "Coverage",
    "Device",
    "MedicationDispense",
    "DocumentReference",
    "ServiceRequest",
    "Specimen",
    "QuestionnaireResponse",
    "Group",
];

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

/// US Core v7 & v8 profiles supported per resource type. Declared in
/// `CapabilityStatement.rest.resource[].supportedProfile` so that Inferno's
/// US Core Server test recognizes the server as US Core conformant. URLs are
/// unversioned, so they satisfy both the v7 and v8 test suites.
fn us_core_profiles_for(resource_type: &str) -> Vec<&'static str> {
    match resource_type {
        "Patient" => vec!["http://hl7.org/fhir/us/core/StructureDefinition/us-core-patient"],
        "Practitioner" => vec!["http://hl7.org/fhir/us/core/StructureDefinition/us-core-practitioner"],
        "Organization" => vec!["http://hl7.org/fhir/us/core/StructureDefinition/us-core-organization"],
        "Encounter" => vec!["http://hl7.org/fhir/us/core/StructureDefinition/us-core-encounter"],
        "Condition" => vec![
            "http://hl7.org/fhir/us/core/StructureDefinition/us-core-condition-encounter-diagnosis",
            "http://hl7.org/fhir/us/core/StructureDefinition/us-core-condition-problems-health-concerns",
        ],
        "Observation" => vec![
            "http://hl7.org/fhir/us/core/StructureDefinition/us-core-observation-lab",
            "http://hl7.org/fhir/us/core/StructureDefinition/us-core-vital-signs",
            "http://hl7.org/fhir/us/core/StructureDefinition/us-core-blood-pressure",
            "http://hl7.org/fhir/us/core/StructureDefinition/us-core-average-blood-pressure",
            "http://hl7.org/fhir/us/core/StructureDefinition/us-core-body-weight",
            "http://hl7.org/fhir/us/core/StructureDefinition/us-core-body-height",
            "http://hl7.org/fhir/us/core/StructureDefinition/us-core-body-temperature",
            "http://hl7.org/fhir/us/core/StructureDefinition/us-core-bmi",
            "http://hl7.org/fhir/us/core/StructureDefinition/us-core-heart-rate",
            "http://hl7.org/fhir/us/core/StructureDefinition/us-core-respiratory-rate",
            "http://hl7.org/fhir/us/core/StructureDefinition/us-core-pulse-oximetry",
            "http://hl7.org/fhir/us/core/StructureDefinition/us-core-head-circumference",
            "http://hl7.org/fhir/us/core/StructureDefinition/us-core-smokingstatus",
            "http://hl7.org/fhir/us/core/StructureDefinition/us-core-simple-observation",
            "http://hl7.org/fhir/us/core/StructureDefinition/us-core-observation-clinical-result",
            "http://hl7.org/fhir/us/core/StructureDefinition/us-core-observation-screening-assessment",
            "http://hl7.org/fhir/us/core/StructureDefinition/us-core-observation-pregnancystatus",
            "http://hl7.org/fhir/us/core/StructureDefinition/us-core-observation-pregnancyintent",
            "http://hl7.org/fhir/us/core/StructureDefinition/us-core-observation-occupation",
            "http://hl7.org/fhir/us/core/StructureDefinition/us-core-observation-sexual-orientation",
            "http://hl7.org/fhir/us/core/StructureDefinition/us-core-observation-adi-documentation",
            "http://hl7.org/fhir/us/core/StructureDefinition/us-core-treatment-intervention-preference",
            "http://hl7.org/fhir/us/core/StructureDefinition/us-core-care-experience-preference",
        ],
        "AllergyIntolerance" => vec!["http://hl7.org/fhir/us/core/StructureDefinition/us-core-allergyintolerance"],
        "DiagnosticReport" => vec![
            "http://hl7.org/fhir/us/core/StructureDefinition/us-core-diagnosticreport-lab",
            "http://hl7.org/fhir/us/core/StructureDefinition/us-core-diagnosticreport-note",
        ],
        "Immunization" => vec!["http://hl7.org/fhir/us/core/StructureDefinition/us-core-immunization"],
        "MedicationRequest" => vec!["http://hl7.org/fhir/us/core/StructureDefinition/us-core-medicationrequest"],
        "Procedure" => vec!["http://hl7.org/fhir/us/core/StructureDefinition/us-core-procedure"],
        "Provenance" => vec!["http://hl7.org/fhir/us/core/StructureDefinition/us-core-provenance"],
        "CarePlan" => vec!["http://hl7.org/fhir/us/core/StructureDefinition/us-core-careplan"],
        "CareTeam" => vec!["http://hl7.org/fhir/us/core/StructureDefinition/us-core-careteam"],
        "RelatedPerson" => vec!["http://hl7.org/fhir/us/core/StructureDefinition/us-core-relatedperson"],
        "Location" => vec!["http://hl7.org/fhir/us/core/StructureDefinition/us-core-location"],
        "PractitionerRole" => vec!["http://hl7.org/fhir/us/core/StructureDefinition/us-core-practitionerrole"],
        "Goal" => vec!["http://hl7.org/fhir/us/core/StructureDefinition/us-core-goal"],
        "Coverage" => vec!["http://hl7.org/fhir/us/core/StructureDefinition/us-core-coverage"],
        "Device" => vec!["http://hl7.org/fhir/us/core/StructureDefinition/us-core-implantable-device"],
        "MedicationDispense" => vec!["http://hl7.org/fhir/us/core/StructureDefinition/us-core-medicationdispense"],
        "DocumentReference" => vec![
            "http://hl7.org/fhir/us/core/StructureDefinition/us-core-documentreference",
            "http://hl7.org/fhir/us/core/StructureDefinition/us-core-adi-documentreference",
        ],
        "ServiceRequest" => vec!["http://hl7.org/fhir/us/core/StructureDefinition/us-core-servicerequest"],
        "Specimen" => vec!["http://hl7.org/fhir/us/core/StructureDefinition/us-core-specimen"],
        "QuestionnaireResponse" => vec!["http://hl7.org/fhir/us/core/StructureDefinition/us-core-questionnaireresponse"],
        _ => vec![],
    }
}

/// Health check (GET /health)
pub async fn health_check() -> impl IntoResponse {
    Json(json!({
        "status": "ok",
        "version": env!("CARGO_PKG_VERSION"),
        "fhirVersion": "4.0.1"
    }))
}

/// Dynamic CapabilityStatement (GET /metadata)
pub async fn capability_statement(State(state): State<Arc<AppState>>) -> Json<Value> {
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

    let resources: Vec<Value> = SUPPORTED_RESOURCE_TYPES
        .iter()
        .map(|rt| {
            let mut entry = json!({
                "type": rt,
                "versioning": "versioned",
                "readHistory": true,
                "conditionalCreate": true,
                "interaction": interactions,
                "searchParam": get_search_params_from_registry(&state.search_param_registry, rt),
            });
            let profiles = us_core_profiles_for(rt);
            if !profiles.is_empty() {
                entry["supportedProfile"] = json!(profiles);
            }
            let ops = bulk_export_operations_for(rt);
            if !ops.is_empty() {
                entry["operation"] = json!(ops);
            }
            entry
        })
        .collect();

    // Build security section
    let security = build_security_section(&state.config);

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
            "url": format!("http://{}:{}", state.config.server.host, state.config.server.port),
        },
        "rest": [rest]
    }))
}

/// Build the security section for CapabilityStatement based on auth config
fn build_security_section(config: &crate::config::ServerConfig) -> Option<Value> {
    if !config.auth.enabled {
        return None;
    }

    if config.auth.jwt.is_some() {
        // SMART on FHIR OAuth security
        let base_url = format!(
            "http://{}:{}",
            config.server.host, config.server.port
        );
        Some(json!({
            "extension": [{
                "url": "http://fhir-registry.smarthealthit.org/StructureDefinition/oauth-uris",
                "extension": [
                    {
                        "url": "authorize",
                        "valueUri": "(external - configure in IdP)"
                    },
                    {
                        "url": "token",
                        "valueUri": "(external - configure in IdP)"
                    }
                ]
            }],
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
        }))
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

/// SMART on FHIR configuration endpoint (GET /.well-known/smart-configuration)
pub async fn smart_configuration(State(state): State<Arc<AppState>>) -> Json<Value> {
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
            .unwrap_or_else(|| "/token".to_string())
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
