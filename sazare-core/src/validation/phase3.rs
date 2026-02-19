use crate::operation_outcome::OperationOutcome;
use crate::validation::registry::TerminologyRegistry;
use serde_json::Value;

/// Phase 3: Terminology binding validation
pub struct Phase3Validator;

impl Phase3Validator {
    /// Validate terminology bindings
    pub fn validate(
        resource: &Value,
        registry: &TerminologyRegistry,
    ) -> Result<(), OperationOutcome> {
        let resource_type = resource
            .get("resourceType")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        match resource_type {
            "Patient" => Self::validate_patient(resource, registry),
            "Observation" => Self::validate_observation(resource, registry),
            "Task" => Self::validate_task(resource, registry),
            _ => Ok(()),
        }
    }

    fn validate_patient(
        resource: &Value,
        registry: &TerminologyRegistry,
    ) -> Result<(), OperationOutcome> {
        // Validate gender (binding to administrative-gender ValueSet)
        if let Some(gender) = resource.get("gender").and_then(|v| v.as_str())
            && !registry.validate_code(
                "http://hl7.org/fhir/ValueSet/administrative-gender",
                gender,
            )
        {
            return Err(OperationOutcome::validation_error(format!(
                "Invalid gender code: '{}'. Must be one of: male, female, other, unknown",
                gender
            ))
            .with_expression(vec!["Patient.gender".to_string()]));
        }

        Ok(())
    }

    fn validate_observation(
        resource: &Value,
        registry: &TerminologyRegistry,
    ) -> Result<(), OperationOutcome> {
        // Validate status (binding to observation-status ValueSet)
        if let Some(status) = resource.get("status").and_then(|v| v.as_str())
            && !registry.validate_code("http://hl7.org/fhir/ValueSet/observation-status", status)
        {
            return Err(OperationOutcome::validation_error(format!(
                "Invalid observation status: '{}'",
                status
            ))
            .with_expression(vec!["Observation.status".to_string()]));
        }

        Ok(())
    }

    fn validate_task(
        resource: &Value,
        registry: &TerminologyRegistry,
    ) -> Result<(), OperationOutcome> {
        // Validate status (binding to task-status ValueSet)
        if let Some(status) = resource.get("status").and_then(|v| v.as_str())
            && !registry.validate_code("http://hl7.org/fhir/ValueSet/task-status", status)
        {
            return Err(OperationOutcome::validation_error(format!(
                "Invalid task status: '{}'",
                status
            ))
            .with_expression(vec!["Task.status".to_string()]));
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_valid_patient_gender() {
        let patient = json!({
            "resourceType": "Patient",
            "gender": "male"
        });

        let registry = TerminologyRegistry::new();
        assert!(Phase3Validator::validate(&patient, &registry).is_ok());
    }

    #[test]
    fn test_invalid_patient_gender() {
        let patient = json!({
            "resourceType": "Patient",
            "gender": "invalid"
        });

        let registry = TerminologyRegistry::new();
        assert!(Phase3Validator::validate(&patient, &registry).is_err());
    }

    #[test]
    fn test_valid_observation_status() {
        let observation = json!({
            "resourceType": "Observation",
            "status": "final",
            "code": {"coding": [{"code": "test"}]}
        });

        let registry = TerminologyRegistry::new();
        assert!(Phase3Validator::validate(&observation, &registry).is_ok());
    }

    #[test]
    fn test_invalid_observation_status() {
        let observation = json!({
            "resourceType": "Observation",
            "status": "invalid",
            "code": {"coding": [{"code": "test"}]}
        });

        let registry = TerminologyRegistry::new();
        assert!(Phase3Validator::validate(&observation, &registry).is_err());
    }
}
