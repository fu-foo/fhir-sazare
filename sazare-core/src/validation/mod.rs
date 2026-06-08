//! Validation module for FHIR resources
//!
//! Phase 1: Required fields + type checking + cardinality
//! Phase 2: Extension validation (JP-Core)
//! Phase 3: Terminology binding (ValueSet/CodeSystem)

pub mod jp_extensions;
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

    // JP Core extension value-type structure check (anywhere in the resource).
    jp_extensions::validate(resource)?;

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

    fn us_core_registry() -> ProfileRegistry {
        let mut reg = ProfileRegistry::new();
        reg.load_profiles(crate::profile_loader::ProfileLoader::get_embedded_us_core_profiles());
        reg
    }

    #[test]
    fn test_pattern_codeable_concept_enforced() {
        // US Core Smoking Status fixes Observation.code to LOINC 72166-2 via
        // patternCodeableConcept.
        let url = "http://hl7.org/fhir/us/core/StructureDefinition/us-core-smokingstatus";
        let obs = |code: &str| {
            json!({
                "resourceType": "Observation",
                "meta": {"profile": [url]},
                "status": "final",
                "code": {"coding": [{"system": "http://loinc.org", "code": code}]},
                "subject": {"reference": "Patient/x"},
                "issued": "2025-12-01T00:00:00Z",
                "valueCodeableConcept": {"coding": [{"system": "http://snomed.info/sct", "code": "266919005"}]}
            })
        };
        // Wrong code violates the fixed pattern.
        assert!(
            validate_resource_all_phases(&obs("99999-9"), &us_core_registry(), &TerminologyRegistry::new()).is_err(),
            "smoking status with the wrong code should fail"
        );
        // The fixed code passes.
        let ok = validate_resource_all_phases(&obs("72166-2"), &us_core_registry(), &TerminologyRegistry::new());
        assert!(ok.is_ok(), "smoking status with code 72166-2 should pass: {:?}", ok.err());
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
    fn test_jp_core_required_slice_enforced() {
        // JP_MedicationRequest requires the identifier slices rpNumber and
        // orderInRp (each fixes identifier.system). Identifiers with other
        // systems do not satisfy them.
        let bad = json!({
            "resourceType": "MedicationRequest",
            "meta": {"profile": ["http://jpfhir.jp/fhir/core/StructureDefinition/JP_MedicationRequest"]},
            "status": "active",
            "intent": "order",
            "subject": {"reference": "Patient/x"},
            "medicationCodeableConcept": {"text": "test"},
            "authoredOn": "2025-12-01",
            "identifier": [
                {"system": "urn:example:other-a", "value": "1"},
                {"system": "urn:example:other-b", "value": "2"}
            ]
        });
        assert!(
            validate_resource_all_phases(&bad, &jp_core_registry(), &TerminologyRegistry::new()).is_err(),
            "MedicationRequest without the required identifier slices should fail"
        );

        // With the fixed slice systems present, it passes.
        let good = json!({
            "resourceType": "MedicationRequest",
            "meta": {"profile": ["http://jpfhir.jp/fhir/core/StructureDefinition/JP_MedicationRequest"]},
            "status": "active",
            "intent": "order",
            "subject": {"reference": "Patient/x"},
            "medicationCodeableConcept": {"text": "test"},
            "authoredOn": "2025-12-01",
            "identifier": [
                {"system": "http://jpfhir.jp/fhir/core/mhlw/IdSystem/Medication-RPGroupNumber", "value": "1"},
                {"system": "http://jpfhir.jp/fhir/core/mhlw/IdSystem/MedicationAdministrationIndex", "value": "1"}
            ]
        });
        let result = validate_resource_all_phases(&good, &jp_core_registry(), &TerminologyRegistry::new());
        assert!(result.is_ok(), "conforming JP_MedicationRequest should pass: {:?}", result.err());
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
