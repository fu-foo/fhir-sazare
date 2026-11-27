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
            "Observation",
            "Procedure",
            "QuestionnaireResponse",
            "RequestGroup",
            "RiskAssessment",
            "ServiceRequest",
            "Specimen",
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
            "EpisodeOfCare",
            "MolecularSequence",
            "NutritionOrder",
            "RelatedPerson",
            "SupplyDelivery",
            "VisionPrescription",
        ] {
            membership.insert(rt.to_string(), vec!["patient".to_string()]);
        }

        // Resources whose patient link is a differently-named (or multiple) field.
        membership.insert("Basic".to_string(), vec!["subject".to_string()]);
        membership.insert("Consent".to_string(), vec!["patient".to_string()]);
        membership.insert(
            "Coverage".to_string(),
            vec!["beneficiary".to_string(), "subscriber".to_string()],
        );
        membership.insert("EnrollmentRequest".to_string(), vec!["candidate".to_string()]);
        membership.insert("SupplyRequest".to_string(), vec!["requester".to_string()]);
        // Nested: the patients a Person links to.
        membership.insert("Person".to_string(), vec!["link.target".to_string()]);
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

    /// Every resource type that can belong to the compartment, sorted.
    pub fn resource_types(&self) -> Vec<&str> {
        let mut types: Vec<&str> = self.membership.keys().map(String::as_str).collect();
        types.sort_unstable();
        types
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
        self.patients_of(resource_type, resource).iter().any(|p| p == patient_id)
    }

    /// The ids of the patients whose compartment `resource` is in: a Patient
    /// its own id; another member type every `Patient/<id>` its compartment
    /// fields reference (a field may hold one Reference or an array, e.g.
    /// Provenance.target). The search index stores these (as `_compartment`),
    /// so a patient-scoped search narrows in the index exactly as this test
    /// filters — one definition for both.
    pub fn patients_of(&self, resource_type: &str, resource: &Value) -> Vec<String> {
        let Some(fields) = self.membership.get(resource_type) else {
            return Vec::new();
        };
        if resource_type == "Patient" {
            return resource.get("id").and_then(|v| v.as_str()).map(|id| vec![id.to_string()]).unwrap_or_default();
        }
        let mut out = Vec::new();
        let mut take = |v: &Value| {
            if let Some(id) = v.get("reference").and_then(|r| r.as_str()).and_then(|r| r.strip_prefix("Patient/")) {
                out.push(id.to_string());
            }
        };
        for field in fields {
            // `a.b`: through each `a` (one or a list) to its `b`.
            let mut values: Vec<&Value> = vec![resource];
            for step in field.split('.') {
                values = values
                    .into_iter()
                    .filter_map(|v| v.get(step))
                    .flat_map(|v| match v {
                        Value::Array(items) => items.iter().collect::<Vec<_>>(),
                        one => vec![one],
                    })
                    .collect();
            }
            values.into_iter().for_each(&mut take);
        }
        out
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

    /// Every compartment field is an element of its type in R4 (an element
    /// that doesn't exist would never put a resource in a compartment).
    #[test]
    fn compartment_fields_are_r4_elements() {
        let def = CompartmentDef::patient_compartment();
        for rt in def.resource_types() {
            if rt == "Patient" {
                continue;
            }
            for field in def.get_reference_fields(rt).unwrap() {
                let path = format!("{rt}.{field}");
                assert!(crate::validation::structure::has_element(&path), "{path} is not an R4 element");
            }
        }
    }

    #[test]
    fn nested_compartment_fields() {
        let def = CompartmentDef::patient_compartment();
        let person = serde_json::json!({"resourceType": "Person", "link": [
            {"target": {"reference": "Patient/a"}}, {"target": {"reference": "RelatedPerson/r"}}, {"target": {"reference": "Patient/b"}}]});
        assert_eq!(def.patients_of("Person", &person), vec!["a", "b"]);
        let eoc = serde_json::json!({"resourceType": "EpisodeOfCare", "patient": {"reference": "Patient/a"}});
        assert_eq!(def.patients_of("EpisodeOfCare", &eoc), vec!["a"]);
    }
}
