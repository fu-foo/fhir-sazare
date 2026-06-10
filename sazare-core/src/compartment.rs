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
    ///
    /// Covers the resource types in the R4 `patient` CompartmentDefinition that
    /// link to a Patient through a simple top-level reference (or array of
    /// references). Types whose only patient linkage is through deeply nested or
    /// polymorphic paths (e.g. `Appointment.participant.actor`,
    /// `Group.member.entity`, `AuditEvent.agent.who`) are intentionally omitted —
    /// listing them with the wrong field would over-deny legitimate access; they
    /// fall through as "not in compartment". The security-relevant goal is that a
    /// patient-scoped token cannot read another patient's clinical resources, so
    /// every common PHI-bearing type that *can* be matched is listed here.
    pub fn patient_compartment() -> Self {
        let mut membership = HashMap::new();

        // Patient itself is checked by id match, not reference field
        membership.insert("Patient".to_string(), vec![]);

        // subject-linked clinical resources
        for rt in [
            "Account",
            "AdverseEvent",
            "CarePlan",
            "CareTeam",
            "ChargeItem",
            "ClinicalImpression",
            "Communication",
            "CommunicationRequest",
            "Composition",
            "Condition",
            "DeviceRequest",
            "DeviceUseStatement",
            "DiagnosticReport",
            "DocumentManifest",
            "DocumentReference",
            "Encounter",
            "EpisodeOfCare",
            "Flag",
            "Goal",
            "ImagingStudy",
            "Invoice",
            "List",
            "MeasureReport",
            "Media",
            "MedicationAdministration",
            "MedicationDispense",
            "MedicationRequest",
            "MedicationStatement",
            "NutritionOrder",
            "Observation",
            "Procedure",
            "QuestionnaireResponse",
            "RequestGroup",
            "RiskAssessment",
            "ServiceRequest",
            "Specimen",
            "SupplyRequest",
        ] {
            membership.insert(rt.to_string(), vec!["subject".to_string()]);
        }

        // patient-linked resources (use the `patient` element)
        for rt in [
            "AllergyIntolerance",
            "BodyStructure",
            "Claim",
            "CoverageEligibilityRequest",
            "CoverageEligibilityResponse",
            "DetectedIssue",
            "ExplanationOfBenefit",
            "FamilyMemberHistory",
            "Immunization",
            "ImmunizationEvaluation",
            "ImmunizationRecommendation",
            "MolecularSequence",
            "Person",
            "RelatedPerson",
            "SupplyDelivery",
            "VisionPrescription",
        ] {
            membership.insert(rt.to_string(), vec!["patient".to_string()]);
        }

        // Resources whose patient link is a differently-named (or multiple) field.
        membership.insert("Basic".to_string(), vec!["patient".to_string(), "subject".to_string()]);
        membership.insert("Consent".to_string(), vec!["patient".to_string()]);
        membership.insert(
            "Coverage".to_string(),
            vec!["beneficiary".to_string(), "subscriber".to_string()],
        );
        membership.insert("EnrollmentRequest".to_string(), vec!["candidate".to_string(), "subject".to_string()]);
        membership.insert("ResearchSubject".to_string(), vec!["individual".to_string()]);
        // `target` is an array of references.
        membership.insert("Provenance".to_string(), vec!["target".to_string()]);
        membership.insert(
            "Task".to_string(),
            vec!["for".to_string(), "owner".to_string()],
        );

        // Practitioner, Organization, Medication, Location, Bundle, etc. are
        // outside the Patient compartment (not patient-specific data).

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

        // Other resources: check reference fields. A field may hold a single
        // Reference object or an array of them (e.g. Provenance.target).
        let expected_ref = format!("Patient/{}", patient_id);
        let matches_ref = |v: &Value| -> bool {
            v.get("reference").and_then(|r| r.as_str()) == Some(expected_ref.as_str())
        };
        for field in fields {
            match resource.get(field.as_str()) {
                Some(Value::Array(arr)) if arr.iter().any(matches_ref) => return true,
                Some(obj) if matches_ref(obj) => return true,
                _ => {}
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
    fn test_expanded_compartment_types_are_filtered() {
        // Regression: these clinical types used to fall through as
        // "not in compartment", leaking other patients' data to scoped tokens.
        let comp = CompartmentDef::patient_compartment();
        for rt in ["CarePlan", "DocumentReference", "ServiceRequest", "MedicationDispense", "Goal"] {
            assert!(comp.is_in_compartment(rt), "{rt} must be in the patient compartment");
            let mine = json!({"resourceType": rt, "subject": {"reference": "Patient/p1"}});
            let theirs = json!({"resourceType": rt, "subject": {"reference": "Patient/p2"}});
            assert!(comp.resource_belongs_to_patient(rt, &mine, "p1"));
            assert!(!comp.resource_belongs_to_patient(rt, &theirs, "p1"));
        }
    }

    #[test]
    fn test_coverage_uses_beneficiary() {
        let comp = CompartmentDef::patient_compartment();
        let cov = json!({"resourceType": "Coverage", "beneficiary": {"reference": "Patient/p1"}});
        assert!(comp.resource_belongs_to_patient("Coverage", &cov, "p1"));
        assert!(!comp.resource_belongs_to_patient("Coverage", &cov, "p2"));
    }

    #[test]
    fn test_provenance_target_array() {
        let comp = CompartmentDef::patient_compartment();
        let prov = json!({
            "resourceType": "Provenance",
            "target": [
                {"reference": "Observation/o1"},
                {"reference": "Patient/p1"}
            ]
        });
        assert!(comp.resource_belongs_to_patient("Provenance", &prov, "p1"));
        assert!(!comp.resource_belongs_to_patient("Provenance", &prov, "p2"));
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
