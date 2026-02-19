use std::collections::HashMap;

use crate::search_param::SearchParamType;

/// How to extract a value from a FHIR resource JSON
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExtractionMode {
    /// Simple scalar field: `resource["status"]`
    Simple,
    /// Array field access: `resource["name"][*]["family"]`
    ArrayField,
    /// Nested array + scalar: `resource["name"][*]["given"][*]`
    NestedArrayScalar,
    /// CodeableConcept: `resource["code"]["coding"][*]` → code + system
    CodeableConcept,
    /// Identifier array: `resource["identifier"][*]` → value + system
    Identifier,
    /// Reference field: `resource["subject"]["reference"]`
    Reference,
    /// Period start: `resource["period"]["start"]`
    PeriodStart,
}

/// Definition of a single search parameter
#[derive(Debug, Clone)]
pub struct SearchParamDef {
    /// Search parameter name (e.g. "family", "code")
    pub name: String,
    /// FHIR search parameter type
    pub param_type: SearchParamType,
    /// JSON path segments to navigate (e.g. ["name"] for ArrayField, ["subject"] for Reference)
    pub path: Vec<String>,
    /// How to extract values from the resource
    pub extraction: ExtractionMode,
    /// Alias names that should also be indexed (e.g. "patient" for "subject")
    pub aliases: Vec<String>,
}

/// Registry of search parameter definitions per resource type
pub struct SearchParamRegistry {
    definitions: HashMap<String, Vec<SearchParamDef>>,
}

impl SearchParamRegistry {
    /// Create a new registry with default definitions for all supported resource types
    pub fn new() -> Self {
        let mut definitions = HashMap::new();

        definitions.insert("Patient".to_string(), patient_definitions());
        definitions.insert("Observation".to_string(), observation_definitions());
        definitions.insert("Encounter".to_string(), encounter_definitions());
        definitions.insert("Condition".to_string(), condition_definitions());
        definitions.insert("MedicationRequest".to_string(), medication_request_definitions());
        definitions.insert("Procedure".to_string(), procedure_definitions());
        definitions.insert("AllergyIntolerance".to_string(), allergy_intolerance_definitions());
        definitions.insert("DiagnosticReport".to_string(), diagnostic_report_definitions());
        definitions.insert("Immunization".to_string(), immunization_definitions());
        definitions.insert("Task".to_string(), task_definitions());
        definitions.insert("Practitioner".to_string(), practitioner_definitions());
        definitions.insert("Organization".to_string(), organization_definitions());
        definitions.insert("Bundle".to_string(), bundle_definitions());
        definitions.insert("ServiceRequest".to_string(), service_request_definitions());
        definitions.insert("Appointment".to_string(), appointment_definitions());
        definitions.insert("Specimen".to_string(), specimen_definitions());

        Self { definitions }
    }

    /// Get search parameter definitions for a resource type.
    /// Falls back to common parameters if the type is not registered.
    pub fn get_definitions(&self, resource_type: &str) -> &[SearchParamDef] {
        static COMMON: std::sync::LazyLock<Vec<SearchParamDef>> =
            std::sync::LazyLock::new(common_definitions);

        self.definitions
            .get(resource_type)
            .map(|v| v.as_slice())
            .unwrap_or(&COMMON)
    }

    /// Check if a resource type has explicit definitions in the registry.
    pub fn has_resource_type(&self, resource_type: &str) -> bool {
        self.definitions.contains_key(resource_type)
    }

    /// Look up the SearchParamType for a given resource type and parameter name.
    /// Checks aliases as well. Returns None if not found.
    pub fn lookup_param_type(
        &self,
        resource_type: &str,
        param_name: &str,
    ) -> Option<SearchParamType> {
        let defs = self.get_definitions(resource_type);
        for def in defs {
            if def.name == param_name {
                return Some(def.param_type.clone());
            }
            if def.aliases.iter().any(|a| a == param_name) {
                return Some(def.param_type.clone());
            }
        }
        None
    }
}

impl Default for SearchParamRegistry {
    fn default() -> Self {
        Self::new()
    }
}

// --- Per-resource definitions ---

fn patient_definitions() -> Vec<SearchParamDef> {
    vec![
        SearchParamDef {
            name: "identifier".to_string(),
            param_type: SearchParamType::Token,
            path: vec!["identifier".to_string()],
            extraction: ExtractionMode::Identifier,
            aliases: vec![],
        },
        SearchParamDef {
            name: "family".to_string(),
            param_type: SearchParamType::String,
            path: vec!["name".to_string(), "family".to_string()],
            extraction: ExtractionMode::ArrayField,
            aliases: vec![],
        },
        SearchParamDef {
            name: "given".to_string(),
            param_type: SearchParamType::String,
            path: vec!["name".to_string(), "given".to_string()],
            extraction: ExtractionMode::NestedArrayScalar,
            aliases: vec![],
        },
        SearchParamDef {
            name: "birthdate".to_string(),
            param_type: SearchParamType::Date,
            path: vec!["birthDate".to_string()],
            extraction: ExtractionMode::Simple,
            aliases: vec![],
        },
        SearchParamDef {
            name: "gender".to_string(),
            param_type: SearchParamType::Token,
            path: vec!["gender".to_string()],
            extraction: ExtractionMode::Simple,
            aliases: vec![],
        },
    ]
}

fn observation_definitions() -> Vec<SearchParamDef> {
    vec![
        SearchParamDef {
            name: "code".to_string(),
            param_type: SearchParamType::Token,
            path: vec!["code".to_string()],
            extraction: ExtractionMode::CodeableConcept,
            aliases: vec![],
        },
        SearchParamDef {
            name: "category".to_string(),
            param_type: SearchParamType::Token,
            path: vec!["category".to_string()],
            extraction: ExtractionMode::CodeableConcept,
            aliases: vec![],
        },
        SearchParamDef {
            name: "status".to_string(),
            param_type: SearchParamType::Token,
            path: vec!["status".to_string()],
            extraction: ExtractionMode::Simple,
            aliases: vec![],
        },
        SearchParamDef {
            name: "subject".to_string(),
            param_type: SearchParamType::Reference,
            path: vec!["subject".to_string()],
            extraction: ExtractionMode::Reference,
            aliases: vec!["patient".to_string()],
        },
        SearchParamDef {
            name: "date".to_string(),
            param_type: SearchParamType::Date,
            path: vec!["effectiveDateTime".to_string()],
            extraction: ExtractionMode::Simple,
            aliases: vec![],
        },
    ]
}

fn encounter_definitions() -> Vec<SearchParamDef> {
    vec![
        SearchParamDef {
            name: "status".to_string(),
            param_type: SearchParamType::Token,
            path: vec!["status".to_string()],
            extraction: ExtractionMode::Simple,
            aliases: vec![],
        },
        SearchParamDef {
            name: "subject".to_string(),
            param_type: SearchParamType::Reference,
            path: vec!["subject".to_string()],
            extraction: ExtractionMode::Reference,
            aliases: vec!["patient".to_string()],
        },
        SearchParamDef {
            name: "date".to_string(),
            param_type: SearchParamType::Date,
            path: vec!["period".to_string(), "start".to_string()],
            extraction: ExtractionMode::PeriodStart,
            aliases: vec![],
        },
    ]
}

fn condition_definitions() -> Vec<SearchParamDef> {
    vec![
        SearchParamDef {
            name: "code".to_string(),
            param_type: SearchParamType::Token,
            path: vec!["code".to_string()],
            extraction: ExtractionMode::CodeableConcept,
            aliases: vec![],
        },
        SearchParamDef {
            name: "subject".to_string(),
            param_type: SearchParamType::Reference,
            path: vec!["subject".to_string()],
            extraction: ExtractionMode::Reference,
            aliases: vec!["patient".to_string()],
        },
    ]
}

fn medication_request_definitions() -> Vec<SearchParamDef> {
    vec![
        SearchParamDef {
            name: "status".to_string(),
            param_type: SearchParamType::Token,
            path: vec!["status".to_string()],
            extraction: ExtractionMode::Simple,
            aliases: vec![],
        },
        SearchParamDef {
            name: "subject".to_string(),
            param_type: SearchParamType::Reference,
            path: vec!["subject".to_string()],
            extraction: ExtractionMode::Reference,
            aliases: vec!["patient".to_string()],
        },
        SearchParamDef {
            name: "intent".to_string(),
            param_type: SearchParamType::Token,
            path: vec!["intent".to_string()],
            extraction: ExtractionMode::Simple,
            aliases: vec![],
        },
        SearchParamDef {
            name: "identifier".to_string(),
            param_type: SearchParamType::Token,
            path: vec!["identifier".to_string()],
            extraction: ExtractionMode::Identifier,
            aliases: vec![],
        },
    ]
}

fn procedure_definitions() -> Vec<SearchParamDef> {
    vec![
        SearchParamDef {
            name: "status".to_string(),
            param_type: SearchParamType::Token,
            path: vec!["status".to_string()],
            extraction: ExtractionMode::Simple,
            aliases: vec![],
        },
        SearchParamDef {
            name: "subject".to_string(),
            param_type: SearchParamType::Reference,
            path: vec!["subject".to_string()],
            extraction: ExtractionMode::Reference,
            aliases: vec!["patient".to_string()],
        },
        SearchParamDef {
            name: "code".to_string(),
            param_type: SearchParamType::Token,
            path: vec!["code".to_string()],
            extraction: ExtractionMode::CodeableConcept,
            aliases: vec![],
        },
        SearchParamDef {
            name: "date".to_string(),
            param_type: SearchParamType::Date,
            path: vec!["performedDateTime".to_string()],
            extraction: ExtractionMode::Simple,
            aliases: vec![],
        },
        SearchParamDef {
            name: "identifier".to_string(),
            param_type: SearchParamType::Token,
            path: vec!["identifier".to_string()],
            extraction: ExtractionMode::Identifier,
            aliases: vec![],
        },
    ]
}

fn allergy_intolerance_definitions() -> Vec<SearchParamDef> {
    vec![
        SearchParamDef {
            name: "patient".to_string(),
            param_type: SearchParamType::Reference,
            path: vec!["patient".to_string()],
            extraction: ExtractionMode::Reference,
            aliases: vec![],
        },
        SearchParamDef {
            name: "clinical-status".to_string(),
            param_type: SearchParamType::Token,
            path: vec!["clinicalStatus".to_string()],
            extraction: ExtractionMode::CodeableConcept,
            aliases: vec!["status".to_string()],
        },
        SearchParamDef {
            name: "code".to_string(),
            param_type: SearchParamType::Token,
            path: vec!["code".to_string()],
            extraction: ExtractionMode::CodeableConcept,
            aliases: vec![],
        },
        SearchParamDef {
            name: "identifier".to_string(),
            param_type: SearchParamType::Token,
            path: vec!["identifier".to_string()],
            extraction: ExtractionMode::Identifier,
            aliases: vec![],
        },
    ]
}

fn diagnostic_report_definitions() -> Vec<SearchParamDef> {
    vec![
        SearchParamDef {
            name: "status".to_string(),
            param_type: SearchParamType::Token,
            path: vec!["status".to_string()],
            extraction: ExtractionMode::Simple,
            aliases: vec![],
        },
        SearchParamDef {
            name: "subject".to_string(),
            param_type: SearchParamType::Reference,
            path: vec!["subject".to_string()],
            extraction: ExtractionMode::Reference,
            aliases: vec!["patient".to_string()],
        },
        SearchParamDef {
            name: "code".to_string(),
            param_type: SearchParamType::Token,
            path: vec!["code".to_string()],
            extraction: ExtractionMode::CodeableConcept,
            aliases: vec![],
        },
        SearchParamDef {
            name: "date".to_string(),
            param_type: SearchParamType::Date,
            path: vec!["effectiveDateTime".to_string()],
            extraction: ExtractionMode::Simple,
            aliases: vec![],
        },
        SearchParamDef {
            name: "identifier".to_string(),
            param_type: SearchParamType::Token,
            path: vec!["identifier".to_string()],
            extraction: ExtractionMode::Identifier,
            aliases: vec![],
        },
    ]
}

fn immunization_definitions() -> Vec<SearchParamDef> {
    vec![
        SearchParamDef {
            name: "patient".to_string(),
            param_type: SearchParamType::Reference,
            path: vec!["patient".to_string()],
            extraction: ExtractionMode::Reference,
            aliases: vec![],
        },
        SearchParamDef {
            name: "status".to_string(),
            param_type: SearchParamType::Token,
            path: vec!["status".to_string()],
            extraction: ExtractionMode::Simple,
            aliases: vec![],
        },
        SearchParamDef {
            name: "date".to_string(),
            param_type: SearchParamType::Date,
            path: vec!["occurrenceDateTime".to_string()],
            extraction: ExtractionMode::Simple,
            aliases: vec![],
        },
        SearchParamDef {
            name: "vaccine-code".to_string(),
            param_type: SearchParamType::Token,
            path: vec!["vaccineCode".to_string()],
            extraction: ExtractionMode::CodeableConcept,
            aliases: vec![],
        },
        SearchParamDef {
            name: "identifier".to_string(),
            param_type: SearchParamType::Token,
            path: vec!["identifier".to_string()],
            extraction: ExtractionMode::Identifier,
            aliases: vec![],
        },
    ]
}

fn task_definitions() -> Vec<SearchParamDef> {
    vec![
        SearchParamDef {
            name: "status".to_string(),
            param_type: SearchParamType::Token,
            path: vec!["status".to_string()],
            extraction: ExtractionMode::Simple,
            aliases: vec![],
        },
        SearchParamDef {
            name: "subject".to_string(),
            param_type: SearchParamType::Reference,
            path: vec!["for".to_string()],
            extraction: ExtractionMode::Reference,
            aliases: vec!["patient".to_string()],
        },
        SearchParamDef {
            name: "owner".to_string(),
            param_type: SearchParamType::Reference,
            path: vec!["owner".to_string()],
            extraction: ExtractionMode::Reference,
            aliases: vec![],
        },
        SearchParamDef {
            name: "code".to_string(),
            param_type: SearchParamType::Token,
            path: vec!["code".to_string()],
            extraction: ExtractionMode::CodeableConcept,
            aliases: vec![],
        },
        SearchParamDef {
            name: "identifier".to_string(),
            param_type: SearchParamType::Token,
            path: vec!["identifier".to_string()],
            extraction: ExtractionMode::Identifier,
            aliases: vec![],
        },
    ]
}

fn practitioner_definitions() -> Vec<SearchParamDef> {
    vec![
        SearchParamDef {
            name: "identifier".to_string(),
            param_type: SearchParamType::Token,
            path: vec!["identifier".to_string()],
            extraction: ExtractionMode::Identifier,
            aliases: vec![],
        },
        SearchParamDef {
            name: "family".to_string(),
            param_type: SearchParamType::String,
            path: vec!["name".to_string(), "family".to_string()],
            extraction: ExtractionMode::ArrayField,
            aliases: vec![],
        },
        SearchParamDef {
            name: "given".to_string(),
            param_type: SearchParamType::String,
            path: vec!["name".to_string(), "given".to_string()],
            extraction: ExtractionMode::NestedArrayScalar,
            aliases: vec![],
        },
    ]
}

fn organization_definitions() -> Vec<SearchParamDef> {
    vec![
        SearchParamDef {
            name: "identifier".to_string(),
            param_type: SearchParamType::Token,
            path: vec!["identifier".to_string()],
            extraction: ExtractionMode::Identifier,
            aliases: vec![],
        },
        SearchParamDef {
            name: "name".to_string(),
            param_type: SearchParamType::String,
            path: vec!["name".to_string()],
            extraction: ExtractionMode::Simple,
            aliases: vec![],
        },
        SearchParamDef {
            name: "type".to_string(),
            param_type: SearchParamType::Token,
            path: vec!["type".to_string()],
            extraction: ExtractionMode::CodeableConcept,
            aliases: vec![],
        },
    ]
}

fn bundle_definitions() -> Vec<SearchParamDef> {
    vec![
        SearchParamDef {
            name: "identifier".to_string(),
            param_type: SearchParamType::Token,
            path: vec!["identifier".to_string()],
            extraction: ExtractionMode::Identifier,
            aliases: vec![],
        },
        SearchParamDef {
            name: "type".to_string(),
            param_type: SearchParamType::Token,
            path: vec!["type".to_string()],
            extraction: ExtractionMode::Simple,
            aliases: vec![],
        },
    ]
}

fn service_request_definitions() -> Vec<SearchParamDef> {
    vec![
        SearchParamDef {
            name: "status".to_string(),
            param_type: SearchParamType::Token,
            path: vec!["status".to_string()],
            extraction: ExtractionMode::Simple,
            aliases: vec![],
        },
        SearchParamDef {
            name: "identifier".to_string(),
            param_type: SearchParamType::Token,
            path: vec!["identifier".to_string()],
            extraction: ExtractionMode::Identifier,
            aliases: vec![],
        },
        SearchParamDef {
            name: "subject".to_string(),
            param_type: SearchParamType::Reference,
            path: vec!["subject".to_string()],
            extraction: ExtractionMode::Reference,
            aliases: vec!["patient".to_string()],
        },
        SearchParamDef {
            name: "code".to_string(),
            param_type: SearchParamType::Token,
            path: vec!["code".to_string()],
            extraction: ExtractionMode::CodeableConcept,
            aliases: vec![],
        },
        SearchParamDef {
            name: "intent".to_string(),
            param_type: SearchParamType::Token,
            path: vec!["intent".to_string()],
            extraction: ExtractionMode::Simple,
            aliases: vec![],
        },
        SearchParamDef {
            name: "priority".to_string(),
            param_type: SearchParamType::Token,
            path: vec!["priority".to_string()],
            extraction: ExtractionMode::Simple,
            aliases: vec![],
        },
        SearchParamDef {
            name: "encounter".to_string(),
            param_type: SearchParamType::Reference,
            path: vec!["encounter".to_string()],
            extraction: ExtractionMode::Reference,
            aliases: vec![],
        },
        SearchParamDef {
            name: "requester".to_string(),
            param_type: SearchParamType::Reference,
            path: vec!["requester".to_string()],
            extraction: ExtractionMode::Reference,
            aliases: vec![],
        },
        SearchParamDef {
            name: "requisition".to_string(),
            param_type: SearchParamType::Token,
            path: vec!["requisition".to_string()],
            extraction: ExtractionMode::Identifier,
            aliases: vec![],
        },
    ]
}

fn appointment_definitions() -> Vec<SearchParamDef> {
    vec![
        SearchParamDef {
            name: "status".to_string(),
            param_type: SearchParamType::Token,
            path: vec!["status".to_string()],
            extraction: ExtractionMode::Simple,
            aliases: vec![],
        },
        SearchParamDef {
            name: "identifier".to_string(),
            param_type: SearchParamType::Token,
            path: vec!["identifier".to_string()],
            extraction: ExtractionMode::Identifier,
            aliases: vec![],
        },
        SearchParamDef {
            name: "date".to_string(),
            param_type: SearchParamType::Date,
            path: vec!["start".to_string()],
            extraction: ExtractionMode::Simple,
            aliases: vec![],
        },
    ]
}

fn specimen_definitions() -> Vec<SearchParamDef> {
    vec![
        SearchParamDef {
            name: "status".to_string(),
            param_type: SearchParamType::Token,
            path: vec!["status".to_string()],
            extraction: ExtractionMode::Simple,
            aliases: vec![],
        },
        SearchParamDef {
            name: "identifier".to_string(),
            param_type: SearchParamType::Token,
            path: vec!["identifier".to_string()],
            extraction: ExtractionMode::Identifier,
            aliases: vec![],
        },
        SearchParamDef {
            name: "subject".to_string(),
            param_type: SearchParamType::Reference,
            path: vec!["subject".to_string()],
            extraction: ExtractionMode::Reference,
            aliases: vec!["patient".to_string()],
        },
        SearchParamDef {
            name: "type".to_string(),
            param_type: SearchParamType::Token,
            path: vec!["type".to_string()],
            extraction: ExtractionMode::CodeableConcept,
            aliases: vec![],
        },
    ]
}

fn common_definitions() -> Vec<SearchParamDef> {
    vec![
        SearchParamDef {
            name: "status".to_string(),
            param_type: SearchParamType::Token,
            path: vec!["status".to_string()],
            extraction: ExtractionMode::Simple,
            aliases: vec![],
        },
        SearchParamDef {
            name: "identifier".to_string(),
            param_type: SearchParamType::Token,
            path: vec!["identifier".to_string()],
            extraction: ExtractionMode::Identifier,
            aliases: vec![],
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_registry_has_all_16_resource_types() {
        let registry = SearchParamRegistry::new();
        let types = [
            "Patient", "Observation", "Encounter", "Condition",
            "MedicationRequest", "Procedure", "AllergyIntolerance",
            "DiagnosticReport", "Immunization", "Task",
            "Practitioner", "Organization", "Bundle",
            "ServiceRequest", "Appointment", "Specimen",
        ];
        for rt in &types {
            assert!(
                !registry.get_definitions(rt).is_empty(),
                "Missing definitions for {}",
                rt
            );
        }
    }

    #[test]
    fn test_fallback_for_unknown_resource() {
        let registry = SearchParamRegistry::new();
        let defs = registry.get_definitions("UnknownResource");
        assert_eq!(defs.len(), 2);
        assert_eq!(defs[0].name, "status");
        assert_eq!(defs[1].name, "identifier");
    }

    #[test]
    fn test_lookup_param_type() {
        let registry = SearchParamRegistry::new();

        // Direct name
        assert_eq!(
            registry.lookup_param_type("Patient", "family"),
            Some(SearchParamType::String)
        );
        assert_eq!(
            registry.lookup_param_type("Observation", "code"),
            Some(SearchParamType::Token)
        );
        assert_eq!(
            registry.lookup_param_type("Observation", "subject"),
            Some(SearchParamType::Reference)
        );

        // Alias lookup
        assert_eq!(
            registry.lookup_param_type("Observation", "patient"),
            Some(SearchParamType::Reference)
        );

        // Not found
        assert_eq!(
            registry.lookup_param_type("Patient", "nonexistent"),
            None
        );
    }

    #[test]
    fn test_task_subject_uses_for_path() {
        let registry = SearchParamRegistry::new();
        let defs = registry.get_definitions("Task");
        let subject_def = defs.iter().find(|d| d.name == "subject").unwrap();
        assert_eq!(subject_def.path, vec!["for".to_string()]);
        assert!(subject_def.aliases.contains(&"patient".to_string()));
    }

    #[test]
    fn test_allergy_intolerance_clinical_status_alias() {
        let registry = SearchParamRegistry::new();
        assert_eq!(
            registry.lookup_param_type("AllergyIntolerance", "status"),
            Some(SearchParamType::Token)
        );
    }

    #[test]
    fn test_observation_category_search_param() {
        let registry = SearchParamRegistry::new();
        assert_eq!(
            registry.lookup_param_type("Observation", "category"),
            Some(SearchParamType::Token)
        );
    }

    #[test]
    fn test_service_request_definitions() {
        let registry = SearchParamRegistry::new();
        let defs = registry.get_definitions("ServiceRequest");
        assert!(defs.iter().any(|d| d.name == "status"));
        assert!(defs.iter().any(|d| d.name == "subject"));
        assert!(defs.iter().any(|d| d.name == "code"));
        assert!(defs.iter().any(|d| d.name == "requisition"));
        assert!(defs.iter().any(|d| d.name == "priority"));
        assert!(defs.iter().any(|d| d.name == "encounter"));
        // patient alias on subject
        assert_eq!(
            registry.lookup_param_type("ServiceRequest", "patient"),
            Some(SearchParamType::Reference)
        );
    }

    #[test]
    fn test_specimen_definitions() {
        let registry = SearchParamRegistry::new();
        let defs = registry.get_definitions("Specimen");
        assert!(defs.iter().any(|d| d.name == "status"));
        assert!(defs.iter().any(|d| d.name == "subject"));
        assert!(defs.iter().any(|d| d.name == "type"));
        assert_eq!(
            registry.lookup_param_type("Specimen", "patient"),
            Some(SearchParamType::Reference)
        );
    }
}
