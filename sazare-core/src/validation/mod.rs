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

pub use registry::{ProfileRegistry, TerminologyRegistry};

use crate::operation_outcome::{OperationOutcome, OperationOutcomeIssue};
use serde_json::Value;

/// Result of validation: success with optional warnings, or failure.
pub struct ValidationResult {
    /// Warning-level issues collected during validation (not errors).
    pub warnings: Vec<OperationOutcomeIssue>,
}

/// Validate a resource through all 3 phases.
///
/// Returns `Ok(ValidationResult)` on success (may contain warnings),
/// or `Err(OperationOutcome)` on validation failure.
pub fn validate_resource_all_phases(
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
