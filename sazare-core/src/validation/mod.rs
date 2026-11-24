//! Validation module for FHIR resources
//!
//! Phase 1: Required fields + type checking + cardinality
//! Phase 2: Extension validation (JP-Core)
//! Phase 3: Terminology binding (ValueSet/CodeSystem)

pub mod bindings;
pub mod phase1;
pub mod phase2;
pub mod phase3;
pub mod registry;

pub use registry::{
    DefaultProfileMode, ProfileRegistry, TerminologyLoadSummary, TerminologyLoader, TerminologyRegistry,
};

use crate::operation_outcome::{IssueSeverity, IssueType, OperationOutcome, OperationOutcomeIssue};
use serde_json::Value;

/// Result of validation: success with optional warnings, or failure.
#[derive(Debug)]
pub struct ValidationResult {
    /// Warning-level issues collected during validation (not errors).
    pub warnings: Vec<OperationOutcomeIssue>,
}

/// Validate a resource through all 3 phases.
///
/// Returns `Ok(ValidationResult)` on success (may contain warnings),
/// or `Err(OperationOutcome)` on validation failure.
///
/// A resource that declares no `meta.profile` is additionally validated
/// against its type's default profile, if one is configured
/// ([`ProfileRegistry::set_default_profile`]) — on a copy, so the resource
/// itself is never modified. In `Warn` mode the violations become warnings.
pub fn validate_resource_all_phases(
    resource: &Value,
    profile_registry: &ProfileRegistry,
    terminology_registry: &TerminologyRegistry,
) -> Result<ValidationResult, OperationOutcome> {
    let result = validate_declared(resource, profile_registry, terminology_registry)?;

    let declares_profile = resource
        .get("meta")
        .and_then(|m| m.get("profile"))
        .and_then(|p| p.as_array())
        .is_some_and(|a| !a.is_empty());
    let resource_type = resource.get("resourceType").and_then(|v| v.as_str()).unwrap_or("");
    let Some((url, mode)) = profile_registry
        .default_profile(resource_type)
        .filter(|_| !declares_profile)
    else {
        return Ok(result);
    };

    let mut with_default = resource.clone();
    if let Some(obj) = with_default.as_object_mut() {
        let meta = obj
            .entry("meta")
            .or_insert_with(|| Value::Object(Default::default()));
        if let Some(meta) = meta.as_object_mut() {
            meta.insert("profile".into(), Value::Array(vec![Value::String(url.to_string())]));
        }
    }
    let note = format!(
        "{resource_type} declares no meta.profile; validated against the default profile {url}"
    );
    match validate_declared(&with_default, profile_registry, terminology_registry) {
        Ok(mut r) => {
            r.warnings.push(OperationOutcomeIssue {
                severity: IssueSeverity::Information,
                code: IssueType::Informational,
                diagnostics: Some(note),
                details: None,
                expression: None,
            });
            Ok(r)
        }
        Err(mut outcome) => match mode {
            DefaultProfileMode::Enforce => {
                outcome.issue.push(OperationOutcomeIssue {
                    severity: IssueSeverity::Information,
                    code: IssueType::Informational,
                    diagnostics: Some(note),
                    details: None,
                    expression: None,
                });
                Err(outcome)
            }
            DefaultProfileMode::Warn => {
                let id = resource.get("id").and_then(|v| v.as_str()).unwrap_or("(new)");
                let mut r = result;
                for mut issue in outcome.issue {
                    tracing::warn!(
                        "{}/{} does not conform to default profile {} (warn mode, accepted): {}",
                        resource_type,
                        id,
                        url,
                        issue.diagnostics.as_deref().unwrap_or("")
                    );
                    if matches!(issue.severity, IssueSeverity::Error | IssueSeverity::Fatal) {
                        issue.severity = IssueSeverity::Warning;
                    }
                    issue.diagnostics = Some(format!(
                        "[default profile, not enforced] {}",
                        issue.diagnostics.unwrap_or_default()
                    ));
                    r.warnings.push(issue);
                }
                Ok(r)
            }
        },
    }
}

/// Validate against the profiles the resource itself declares.
fn validate_declared(
    resource: &Value,
    profile_registry: &ProfileRegistry,
    terminology_registry: &TerminologyRegistry,
) -> Result<ValidationResult, OperationOutcome> {
    // Phase 1: Required fields, types, cardinality
    phase1::Phase1Validator::validate(resource)?;

    // Phase 2: Extension validation + Profile-based validation
    let phase2_warnings = phase2::Phase2Validator::validate(resource, profile_registry)?;

    // Phase 3: Terminology binding
    phase3::Phase3Validator::validate(resource, terminology_registry)?;

    // Profile-driven required bindings (validated against embedded value sets).
    bindings::validate(resource, profile_registry, terminology_registry)?;

    Ok(ValidationResult {
        warnings: phase2_warnings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_validate_all_phases_valid_patient() {
        let patient = json!({
            "resourceType": "Patient",
            "gender": "male",
            "name": [{"family": "Doe"}]
        });

        let profile_reg = ProfileRegistry::new();
        let terminology_reg = TerminologyRegistry::new();

        assert!(validate_resource_all_phases(&patient, &profile_reg, &terminology_reg).is_ok());
    }

    #[test]
    fn test_validate_all_phases_missing_resource_type() {
        let resource = json!({
            "name": [{"family": "Doe"}]
        });

        let profile_reg = ProfileRegistry::new();
        let terminology_reg = TerminologyRegistry::new();

        assert!(validate_resource_all_phases(&resource, &profile_reg, &terminology_reg).is_err());
    }

    #[test]
    fn test_validate_all_phases_invalid_gender() {
        let patient = json!({
            "resourceType": "Patient",
            "gender": "invalid_gender"
        });

        let profile_reg = ProfileRegistry::new();
        let terminology_reg = TerminologyRegistry::new();

        assert!(validate_resource_all_phases(&patient, &profile_reg, &terminology_reg).is_err());
    }

    fn registry_with_default(mode: DefaultProfileMode) -> ProfileRegistry {
        let mut reg = ProfileRegistry::new();
        reg.add_profile(json!({
            "resourceType": "StructureDefinition",
            "url": "http://x/StructureDefinition/P",
            "type": "Patient",
            "differential": {"element": [
                {"id": "Patient.identifier", "path": "Patient.identifier", "min": 1}
            ]}
        }));
        reg.set_default_profile("Patient", "http://x/StructureDefinition/P", mode).unwrap();
        reg
    }

    #[test]
    fn test_default_profile_enforce() {
        let reg = registry_with_default(DefaultProfileMode::Enforce);
        let terms = TerminologyRegistry::new();
        let bare = json!({"resourceType": "Patient", "gender": "male"});
        let err = validate_resource_all_phases(&bare, &reg, &terms).unwrap_err();
        let text = serde_json::to_string(&err).unwrap();
        assert!(text.contains("Patient.identifier") && text.contains("default profile"), "{text}");

        let ok = json!({"resourceType": "Patient", "identifier": [{"value": "1"}]});
        assert!(validate_resource_all_phases(&ok, &reg, &terms).is_ok());

        // A resource that declares its own profile isn't held to the default.
        let declared = json!({"resourceType": "Patient", "meta": {"profile": ["http://x/other"]}});
        assert!(validate_resource_all_phases(&declared, &reg, &terms).is_ok());
    }

    #[test]
    fn test_default_profile_warn_accepts_with_warnings() {
        let reg = registry_with_default(DefaultProfileMode::Warn);
        let bare = json!({"resourceType": "Patient", "gender": "male"});
        let r = validate_resource_all_phases(&bare, &reg, &TerminologyRegistry::new()).unwrap();
        assert!(r.warnings.iter().any(|w| w.severity == IssueSeverity::Warning
            && w.diagnostics.as_deref().is_some_and(|d| d.contains("not enforced") && d.contains("Patient.identifier"))));
        // Base validation still applies in warn mode.
        let invalid = json!({"resourceType": "Patient", "gender": "nope"});
        assert!(validate_resource_all_phases(&invalid, &reg, &TerminologyRegistry::new()).is_err());
    }

    #[test]
    fn test_set_default_profile_rejects_unknown_or_mismatched() {
        let mut reg = registry_with_default(DefaultProfileMode::Enforce);
        assert!(reg.set_default_profile("Patient", "http://x/missing", DefaultProfileMode::Enforce).is_err());
        assert!(reg
            .set_default_profile("Observation", "http://x/StructureDefinition/P", DefaultProfileMode::Enforce)
            .is_err());
    }

    fn us_core_registry() -> ProfileRegistry {
        let mut reg = ProfileRegistry::new();
        reg.load_profiles(crate::profile_loader::ProfileLoader::get_embedded_us_core_profiles());
        reg
    }

    #[test]
    fn test_pattern_codeable_concept_enforced() {
        // US Core Body Weight fixes Observation.code to LOINC 29463-7 via
        // patternCodeableConcept.
        let url = "http://hl7.org/fhir/us/core/StructureDefinition/us-core-body-weight";
        let obs = |code: &str| {
            json!({
                "resourceType": "Observation",
                "meta": {"profile": [url]},
                "status": "final",
                "category": [{"coding": [{"system": "http://terminology.hl7.org/CodeSystem/observation-category", "code": "vital-signs"}]}],
                "code": {"coding": [{"system": "http://loinc.org", "code": code}]},
                "subject": {"reference": "Patient/x"},
                "effectiveDateTime": "2025-12-01",
                "valueQuantity": {"value": 70, "unit": "kg", "system": "http://unitsofmeasure.org", "code": "kg"}
            })
        };
        // Wrong code violates the fixed pattern.
        assert!(
            validate_resource_all_phases(&obs("99999-9"), &us_core_registry(), &TerminologyRegistry::new()).is_err(),
            "body weight with the wrong code should fail"
        );
        // The fixed code passes.
        let ok = validate_resource_all_phases(&obs("29463-7"), &us_core_registry(), &TerminologyRegistry::new());
        assert!(ok.is_ok(), "body weight with code 29463-7 should pass: {:?}", ok.err());
    }
}
