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

use crate::operation_outcome::OperationOutcome;
use serde_json::Value;

/// Validate a resource through all 3 phases.
///
/// Returns Ok(()) on success, Err(OperationOutcome) on validation failure.
pub fn validate_resource_all_phases(
    resource: &Value,
    profile_registry: &ProfileRegistry,
    terminology_registry: &TerminologyRegistry,
) -> Result<(), OperationOutcome> {
    // Phase 1: Required fields, types, cardinality
    phase1::Phase1Validator::validate(resource)?;

    // Phase 2: Extension validation
    phase2::Phase2Validator::validate(resource, profile_registry)?;

    // Phase 3: Terminology binding
    phase3::Phase3Validator::validate(resource, terminology_registry)?;

    Ok(())
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
}
