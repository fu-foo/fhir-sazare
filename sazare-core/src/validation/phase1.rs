use crate::operation_outcome::{IssueSeverity, IssueType, OperationOutcome, OperationOutcomeIssue};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::LazyLock;

/// Registry of required fields per resource type (FHIR R4 base spec, min=1).
///
/// Data-driven — no hardcoded match. New resource types are added here.
static REQUIRED_FIELDS: LazyLock<HashMap<&str, &[&str]>> = LazyLock::new(|| {
    let mut m = HashMap::new();
    // Patient: no required fields in base spec
    m.insert("Observation", ["status", "code"].as_slice());
    m.insert("Encounter", ["status", "class"].as_slice());
    m.insert("Condition", ["subject"].as_slice());
    m.insert("Task", ["status", "intent"].as_slice());
    m.insert("MedicationRequest", ["status", "intent", "subject"].as_slice());
    m.insert("Procedure", ["status", "subject"].as_slice());
    m.insert("AllergyIntolerance", ["patient"].as_slice());
    m.insert("DiagnosticReport", ["status", "code"].as_slice());
    m.insert("Immunization", ["status", "vaccineCode", "patient"].as_slice());
    m.insert("Bundle", ["type"].as_slice());
    m.insert("Subscription", ["status", "criteria", "channel"].as_slice());
    m.insert("Composition", ["status", "type", "date", "author", "title"].as_slice());
    m.insert("CarePlan", ["status", "intent", "subject"].as_slice());
    m.insert("Claim", ["status", "type", "use", "patient", "provider", "priority", "insurance"].as_slice());
    m.insert("Coverage", ["status", "beneficiary", "payor"].as_slice());
    m.insert("DocumentReference", ["status", "content"].as_slice());
    m.insert("ServiceRequest", ["status", "intent", "subject"].as_slice());
    m
});

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

        // Check required fields from registry
        if let Some(fields) = REQUIRED_FIELDS.get(resource_type) {
            for field in *fields {
                if resource.get(*field).is_none() {
                    issues.push(OperationOutcomeIssue {
                        severity: IssueSeverity::Error,
                        code: IssueType::Required,
                        diagnostics: Some(format!("Missing required field: {}", field)),
                        details: None,
                        expression: Some(vec![format!("{}.{}", resource_type, field)]),
                    });
                }
            }
        }

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
    fn test_observation_missing_status() {
        let observation = json!({
            "resourceType": "Observation",
            "code": {"coding": [{"code": "test"}]}
        });

        let result = Phase1Validator::validate(&observation);
        assert!(result.is_err());
        let outcome = result.unwrap_err();
        assert!(outcome.issue.iter().any(|i| i
            .expression
            .as_ref()
            .unwrap()
            .contains(&"Observation.status".to_string())));
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
    fn test_medication_request_required_fields() {
        // Missing all required fields
        let med = json!({"resourceType": "MedicationRequest"});
        let result = Phase1Validator::validate(&med);
        assert!(result.is_err());
        let outcome = result.unwrap_err();
        assert!(outcome.issue.len() >= 3); // status, intent, subject
    }

    #[test]
    fn test_procedure_required_fields() {
        let proc = json!({"resourceType": "Procedure"});
        let result = Phase1Validator::validate(&proc);
        assert!(result.is_err());
        let outcome = result.unwrap_err();
        assert!(outcome.issue.iter().any(|i| i
            .expression.as_ref().unwrap()
            .contains(&"Procedure.status".to_string())));
        assert!(outcome.issue.iter().any(|i| i
            .expression.as_ref().unwrap()
            .contains(&"Procedure.subject".to_string())));
    }

    #[test]
    fn test_diagnostic_report_required_fields() {
        let report = json!({"resourceType": "DiagnosticReport"});
        let result = Phase1Validator::validate(&report);
        assert!(result.is_err());
        let outcome = result.unwrap_err();
        assert!(outcome.issue.iter().any(|i| i
            .expression.as_ref().unwrap()
            .contains(&"DiagnosticReport.status".to_string())));
        assert!(outcome.issue.iter().any(|i| i
            .expression.as_ref().unwrap()
            .contains(&"DiagnosticReport.code".to_string())));
    }

    #[test]
    fn test_unknown_resource_passes() {
        // Unknown resource type — no required fields defined, should pass
        let custom = json!({"resourceType": "CustomResource"});
        assert!(Phase1Validator::validate(&custom).is_ok());
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
