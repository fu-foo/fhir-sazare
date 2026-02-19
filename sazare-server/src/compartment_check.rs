use axum::http::StatusCode;
use axum::response::Json;
use sazare_core::compartment::CompartmentDef;
use sazare_core::OperationOutcome;
use serde_json::{json, Value};

use crate::auth::AuthUser;

/// Check if a single resource is accessible under compartment rules.
///
/// Returns Ok(()) if access is allowed, Err with 403 response if denied.
///
/// Rules:
/// - No auth user (auth disabled) → allow
/// - Not patient-scoped (user/system/APIKey/Basic) → allow
/// - Patient-scoped but no patient_id → deny
/// - Otherwise → check compartment membership
pub fn check_compartment_access(
    auth_user: Option<&AuthUser>,
    compartment: &CompartmentDef,
    resource_type: &str,
    resource: &Value,
) -> Result<(), (StatusCode, Json<Value>)> {
    let Some(user) = auth_user else {
        return Ok(());
    };

    if !user.is_patient_scoped() {
        return Ok(());
    }

    let Some(ref patient_id) = user.patient_id else {
        return Err((
            StatusCode::FORBIDDEN,
            Json(json!(OperationOutcome::forbidden(
                "Patient-scoped token without patient context"
            ))),
        ));
    };

    // Non-compartment resources (Practitioner, Organization, Bundle) are readable
    // by patient-scoped tokens for reference resolution
    if !compartment.is_in_compartment(resource_type) {
        return Ok(());
    }

    if compartment.resource_belongs_to_patient(resource_type, resource, patient_id) {
        Ok(())
    } else {
        Err((
            StatusCode::FORBIDDEN,
            Json(json!(OperationOutcome::forbidden(
                "Access denied: resource is not in patient compartment"
            ))),
        ))
    }
}

/// Filter a list of resources by compartment membership.
///
/// Returns only resources that belong to the patient's compartment.
/// If no compartment filtering is needed, returns all resources.
pub fn filter_by_compartment(
    auth_user: Option<&AuthUser>,
    compartment: &CompartmentDef,
    resource_type: &str,
    resources: Vec<Value>,
) -> Vec<Value> {
    let Some(user) = auth_user else {
        return resources;
    };

    if !user.is_patient_scoped() {
        return resources;
    }

    let Some(ref patient_id) = user.patient_id else {
        return Vec::new();
    };

    // Non-compartment resources pass through
    if !compartment.is_in_compartment(resource_type) {
        return resources;
    }

    resources
        .into_iter()
        .filter(|r| compartment.resource_belongs_to_patient(resource_type, r, patient_id))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::AuthType;
    use serde_json::json;

    fn patient_scoped_user(patient_id: &str) -> AuthUser {
        AuthUser {
            user_id: "test-user".to_string(),
            auth_type: AuthType::Jwt,
            scopes: vec!["patient/Observation.read".to_string()],
            patient_id: Some(patient_id.to_string()),
        }
    }

    fn system_user() -> AuthUser {
        AuthUser {
            user_id: "system".to_string(),
            auth_type: AuthType::Jwt,
            scopes: vec!["system/*.*".to_string()],
            patient_id: None,
        }
    }

    fn api_key_user() -> AuthUser {
        AuthUser {
            user_id: "api-client".to_string(),
            auth_type: AuthType::ApiKey,
            scopes: vec![],
            patient_id: None,
        }
    }

    #[test]
    fn test_no_auth_allows_all() {
        let comp = CompartmentDef::patient_compartment();
        let obs = json!({"resourceType": "Observation", "subject": {"reference": "Patient/other"}});
        assert!(check_compartment_access(None, &comp, "Observation", &obs).is_ok());
    }

    #[test]
    fn test_system_scope_allows_all() {
        let comp = CompartmentDef::patient_compartment();
        let user = system_user();
        let obs = json!({"resourceType": "Observation", "subject": {"reference": "Patient/other"}});
        assert!(check_compartment_access(Some(&user), &comp, "Observation", &obs).is_ok());
    }

    #[test]
    fn test_api_key_allows_all() {
        let comp = CompartmentDef::patient_compartment();
        let user = api_key_user();
        let obs = json!({"resourceType": "Observation", "subject": {"reference": "Patient/other"}});
        assert!(check_compartment_access(Some(&user), &comp, "Observation", &obs).is_ok());
    }

    #[test]
    fn test_patient_scoped_allows_own_data() {
        let comp = CompartmentDef::patient_compartment();
        let user = patient_scoped_user("p123");
        let obs = json!({"resourceType": "Observation", "subject": {"reference": "Patient/p123"}});
        assert!(check_compartment_access(Some(&user), &comp, "Observation", &obs).is_ok());
    }

    #[test]
    fn test_patient_scoped_denies_other_data() {
        let comp = CompartmentDef::patient_compartment();
        let user = patient_scoped_user("p123");
        let obs = json!({"resourceType": "Observation", "subject": {"reference": "Patient/other"}});
        assert!(check_compartment_access(Some(&user), &comp, "Observation", &obs).is_err());
    }

    #[test]
    fn test_patient_scoped_allows_non_compartment_resource() {
        let comp = CompartmentDef::patient_compartment();
        let user = patient_scoped_user("p123");
        let org = json!({"resourceType": "Organization", "id": "org1"});
        assert!(check_compartment_access(Some(&user), &comp, "Organization", &org).is_ok());
    }

    #[test]
    fn test_patient_scoped_no_patient_id_denied() {
        let comp = CompartmentDef::patient_compartment();
        let user = AuthUser {
            user_id: "test".to_string(),
            auth_type: AuthType::Jwt,
            scopes: vec!["patient/Observation.read".to_string()],
            patient_id: None,
        };
        let obs = json!({"resourceType": "Observation", "subject": {"reference": "Patient/p123"}});
        assert!(check_compartment_access(Some(&user), &comp, "Observation", &obs).is_err());
    }

    #[test]
    fn test_filter_by_compartment() {
        let comp = CompartmentDef::patient_compartment();
        let user = patient_scoped_user("p123");

        let resources = vec![
            json!({"resourceType": "Observation", "subject": {"reference": "Patient/p123"}}),
            json!({"resourceType": "Observation", "subject": {"reference": "Patient/other"}}),
            json!({"resourceType": "Observation", "subject": {"reference": "Patient/p123"}}),
        ];

        let filtered = filter_by_compartment(Some(&user), &comp, "Observation", resources);
        assert_eq!(filtered.len(), 2);
    }

    #[test]
    fn test_filter_no_auth_returns_all() {
        let comp = CompartmentDef::patient_compartment();
        let resources = vec![
            json!({"resourceType": "Observation", "subject": {"reference": "Patient/other"}}),
        ];
        let filtered = filter_by_compartment(None, &comp, "Observation", resources);
        assert_eq!(filtered.len(), 1);
    }

    #[test]
    fn test_filter_patient_scoped_no_patient_id_returns_empty() {
        let comp = CompartmentDef::patient_compartment();
        let user = AuthUser {
            user_id: "test".to_string(),
            auth_type: AuthType::Jwt,
            scopes: vec!["patient/*.read".to_string()],
            patient_id: None,
        };
        let resources = vec![
            json!({"resourceType": "Observation", "subject": {"reference": "Patient/p123"}}),
        ];
        let filtered = filter_by_compartment(Some(&user), &comp, "Observation", resources);
        assert_eq!(filtered.len(), 0);
    }
}
