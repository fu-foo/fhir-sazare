use serde_json::Value;
use std::collections::HashMap;

/// Patient compartment definition per FHIR R4.
///
/// Defines which reference fields on each resource type link it to a Patient.
pub struct CompartmentDef {
    /// resource_type → list of reference field names that point to Patient
    membership: HashMap<String, Vec<String>>,
}

impl CompartmentDef {
    /// Create the standard FHIR R4 Patient compartment definition.
    pub fn patient_compartment() -> Self {
        let mut membership = HashMap::new();

        // Patient itself is checked by id match, not reference field
        membership.insert("Patient".to_string(), vec![]);

        membership.insert("Observation".to_string(), vec!["subject".to_string()]);
        membership.insert("Encounter".to_string(), vec!["subject".to_string()]);
        membership.insert("Condition".to_string(), vec!["subject".to_string()]);
        membership.insert("MedicationRequest".to_string(), vec!["subject".to_string()]);
        membership.insert("Procedure".to_string(), vec!["subject".to_string()]);
        membership.insert(
            "AllergyIntolerance".to_string(),
            vec!["patient".to_string()],
        );
        membership.insert("DiagnosticReport".to_string(), vec!["subject".to_string()]);
        membership.insert("Immunization".to_string(), vec!["patient".to_string()]);
        membership.insert(
            "Task".to_string(),
            vec!["for".to_string(), "owner".to_string()],
        );

        // Practitioner, Organization, Bundle are outside the Patient compartment

        Self { membership }
    }

    /// Check if a resource type can belong to the Patient compartment.
    pub fn is_in_compartment(&self, resource_type: &str) -> bool {
        self.membership.contains_key(resource_type)
    }

    /// Get the reference fields that link a resource type to a Patient.
    /// Returns None if the resource type is not in the compartment.
    pub fn get_reference_fields(&self, resource_type: &str) -> Option<&[String]> {
        self.membership.get(resource_type).map(|v| v.as_slice())
    }

    /// Check if a resource belongs to a specific patient.
    ///
    /// - For Patient resources: checks if `resource.id == patient_id`
    /// - For other resources: checks if any reference field points to `Patient/{patient_id}`
    /// - For non-compartment resources: returns false
    pub fn resource_belongs_to_patient(
        &self,
        resource_type: &str,
        resource: &Value,
        patient_id: &str,
    ) -> bool {
        let fields = match self.membership.get(resource_type) {
            Some(f) => f,
            None => return false,
        };

        // Patient: check id match
        if resource_type == "Patient" {
            return resource
                .get("id")
                .and_then(|v| v.as_str())
                .is_some_and(|id| id == patient_id);
        }

        // Other resources: check reference fields
        let expected_ref = format!("Patient/{}", patient_id);
        for field in fields {
            if let Some(ref_obj) = resource.get(field.as_str())
                && let Some(reference) = ref_obj.get("reference").and_then(|v| v.as_str())
                && reference == expected_ref
            {
                return true;
            }
        }

        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_patient_compartment_membership() {
        let comp = CompartmentDef::patient_compartment();
        assert!(comp.is_in_compartment("Patient"));
        assert!(comp.is_in_compartment("Observation"));
        assert!(comp.is_in_compartment("Encounter"));
        assert!(comp.is_in_compartment("Task"));
        assert!(!comp.is_in_compartment("Practitioner"));
        assert!(!comp.is_in_compartment("Organization"));
        assert!(!comp.is_in_compartment("Bundle"));
    }

    #[test]
    fn test_patient_belongs_to_self() {
        let comp = CompartmentDef::patient_compartment();
        let patient = json!({
            "resourceType": "Patient",
            "id": "p123"
        });
        assert!(comp.resource_belongs_to_patient("Patient", &patient, "p123"));
        assert!(!comp.resource_belongs_to_patient("Patient", &patient, "other"));
    }

    #[test]
    fn test_observation_belongs_to_patient() {
        let comp = CompartmentDef::patient_compartment();
        let obs = json!({
            "resourceType": "Observation",
            "subject": {"reference": "Patient/p123"}
        });
        assert!(comp.resource_belongs_to_patient("Observation", &obs, "p123"));
        assert!(!comp.resource_belongs_to_patient("Observation", &obs, "other"));
    }

    #[test]
    fn test_allergy_uses_patient_field() {
        let comp = CompartmentDef::patient_compartment();
        let allergy = json!({
            "resourceType": "AllergyIntolerance",
            "patient": {"reference": "Patient/p456"}
        });
        assert!(comp.resource_belongs_to_patient("AllergyIntolerance", &allergy, "p456"));
        assert!(!comp.resource_belongs_to_patient("AllergyIntolerance", &allergy, "other"));
    }

    #[test]
    fn test_task_multiple_fields() {
        let comp = CompartmentDef::patient_compartment();

        // Task with "for" pointing to patient
        let task1 = json!({
            "resourceType": "Task",
            "for": {"reference": "Patient/p789"},
            "owner": {"reference": "Practitioner/dr1"}
        });
        assert!(comp.resource_belongs_to_patient("Task", &task1, "p789"));

        // Task with "owner" pointing to patient (unlikely but valid per spec)
        let task2 = json!({
            "resourceType": "Task",
            "for": {"reference": "Organization/org1"},
            "owner": {"reference": "Patient/p789"}
        });
        assert!(comp.resource_belongs_to_patient("Task", &task2, "p789"));
    }

    #[test]
    fn test_non_compartment_resource() {
        let comp = CompartmentDef::patient_compartment();
        let org = json!({
            "resourceType": "Organization",
            "id": "org1"
        });
        assert!(!comp.resource_belongs_to_patient("Organization", &org, "p123"));
    }

    #[test]
    fn test_missing_reference_field() {
        let comp = CompartmentDef::patient_compartment();
        let obs = json!({
            "resourceType": "Observation",
            "status": "final"
        });
        assert!(!comp.resource_belongs_to_patient("Observation", &obs, "p123"));
    }
}
