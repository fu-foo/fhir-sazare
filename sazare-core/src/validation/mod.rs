//! Validation module for FHIR resources
//!
//! Phase 1: Required fields + type checking + cardinality
//! Phase 2: Extension validation (JP-Core)
//! Phase 3: Terminology binding (ValueSet/CodeSystem)

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

    fn jp_core_registry() -> ProfileRegistry {
        let mut reg = ProfileRegistry::new();
        reg.load_profiles(crate::profile_loader::ProfileLoader::get_embedded_jp_core_profiles());
        reg
    }

    #[test]
    fn test_jp_core_patient_valid() {
        // A JP_Patient with the mandatory identifier (and identifier.value).
        let patient = json!({
            "resourceType": "Patient",
            "meta": {"profile": ["http://jpfhir.jp/fhir/core/StructureDefinition/JP_Patient"]},
            "identifier": [{
                "system": "urn:oid:1.2.392.100495.20.3.51.1",
                "value": "00000010"
            }],
            "name": [{"family": "山田", "given": ["太郎"]}],
            "gender": "male"
        });

        let result = validate_resource_all_phases(&patient, &jp_core_registry(), &TerminologyRegistry::new());
        assert!(result.is_ok(), "valid JP_Patient should pass: {:?}", result.err());
    }

    #[test]
    fn test_jp_core_patient_missing_identifier() {
        // JP_Patient requires identifier (min=1); omitting it must fail.
        let patient = json!({
            "resourceType": "Patient",
            "meta": {"profile": ["http://jpfhir.jp/fhir/core/StructureDefinition/JP_Patient"]},
            "name": [{"family": "山田", "given": ["太郎"]}],
            "gender": "male"
        });

        assert!(
            validate_resource_all_phases(&patient, &jp_core_registry(), &TerminologyRegistry::new()).is_err(),
            "JP_Patient without identifier should fail profile validation"
        );
    }
}
