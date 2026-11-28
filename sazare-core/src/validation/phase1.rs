use crate::operation_outcome::{IssueSeverity, IssueType, OperationOutcome, OperationOutcomeIssue};
use serde_json::Value;

/// Phase 1: Basic validation (required fields, types, cardinality)
pub struct Phase1Validator;

impl Phase1Validator {
    /// Validate a resource's basic structure
    pub fn validate(resource: &Value) -> Result<(), OperationOutcome> {
        let mut issues = Vec::new();

        // Check resourceType is present
        let resource_type = match resource.get("resourceType").and_then(|v| v.as_str()) {
            Some(rt) => rt,
            None => {
                let mut outcome = OperationOutcome::error(
                    IssueType::Required,
                    "Missing required field: resourceType",
                );
                outcome.issue[0].expression = Some(vec!["resourceType".to_string()]);
                return Err(outcome);
            }
        };

        // An R4 resource type, not any string (it would be stored, but
        // nothing could search it).
        if !crate::r4_types::is_resource_type(resource_type) {
            let mut outcome = OperationOutcome::error(
                IssueType::Invalid,
                format!("'{resource_type}' is not a FHIR R4 resource type"),
            );
            outcome.issue[0].expression = Some(vec!["resourceType".to_string()]);
            return Err(outcome);
        }

        // A FHIR id: 1–64 of A–Z, a–z, 0–9, '-' and '.'.
        if let Some(id) = resource.get("id") {
            let ok = id.as_str().is_some_and(super::is_fhir_id);
            if !ok {
                let mut outcome = OperationOutcome::error(
                    IssueType::Invalid,
                    format!("Invalid id {id}: an id is 1 to 64 letters, digits, '-' or '.'"),
                );
                outcome.issue[0].expression = Some(vec![format!("{resource_type}.id")]);
                return Err(outcome);
            }
        }

        // Required elements: every R4 one, from the definitions (structure.rs).

        // Data quality warnings (non-blocking)
        check_identifier_quality(resource, resource_type, &mut issues);

        // Only fail on Error-severity issues; warnings are non-blocking
        let has_errors = issues.iter().any(|i| i.severity == IssueSeverity::Error);
        if has_errors {
            Err(OperationOutcome {
                resource_type: "OperationOutcome".to_string(),
                id: None,
                issue: issues,
            })
        } else {
            Ok(())
        }
    }
}

/// Warn if identifiers lack both value and system.
fn check_identifier_quality(
    resource: &Value,
    resource_type: &str,
    issues: &mut Vec<OperationOutcomeIssue>,
) {
    if let Some(identifiers) = resource.get("identifier").and_then(|v| v.as_array()) {
        for (idx, identifier) in identifiers.iter().enumerate() {
            if identifier.get("value").is_none() && identifier.get("system").is_none() {
                issues.push(OperationOutcomeIssue {
                    severity: IssueSeverity::Warning,
                    code: IssueType::Value,
                    diagnostics: Some(format!(
                        "Identifier at index {} should have either 'value' or 'system'",
                        idx
                    )),
                    details: None,
                    expression: Some(vec![format!("{}.identifier[{}]", resource_type, idx)]),
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_valid_patient() {
        let patient = json!({
            "resourceType": "Patient",
            "name": [{"family": "Smith"}]
        });

        assert!(Phase1Validator::validate(&patient).is_ok());
    }

    #[test]
    fn test_missing_resource_type() {
        let resource = json!({
            "name": [{"family": "Smith"}]
        });

        assert!(Phase1Validator::validate(&resource).is_err());
    }

    #[test]
    fn test_valid_observation() {
        let observation = json!({
            "resourceType": "Observation",
            "status": "final",
            "code": {"coding": [{"code": "test"}]}
        });

        assert!(Phase1Validator::validate(&observation).is_ok());
    }

    #[test]
    fn test_non_r4_resource_type_is_refused() {
        // Not an R4 type: refused (it could be stored, but never searched).
        let custom = json!({"resourceType": "CustomResource"});
        assert!(Phase1Validator::validate(&custom).is_err());
        // An R4 type without required fields of its own passes.
        assert!(Phase1Validator::validate(&json!({"resourceType": "Basic", "code": {"text": "x"}})).is_ok());
    }

    #[test]
    fn test_identifier_quality_warning_does_not_fail() {
        // Warning-only issues should not cause validation failure
        let patient = json!({
            "resourceType": "Patient",
            "identifier": [{"use": "official"}]
        });
        // Passes — warnings are non-blocking
        assert!(Phase1Validator::validate(&patient).is_ok());
    }
}
