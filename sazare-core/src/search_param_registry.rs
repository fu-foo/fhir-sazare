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
    /// Array of scalar URIs/strings: `resource["meta"]["profile"][*]` → value (system=None)
    UriArray,
    /// Array of Codings: `resource["meta"]["tag"][*]` → code + system
    CodingArray,
    /// Single Coding object (e.g. `Encounter.class` is one Coding, not a CodeableConcept).
    /// Yields `code` + `system`.
    Coding,
    /// Address datatype (single object or array). When `path` points at the address
    /// field (e.g. `["address"]`) every component (line, city, district, state,
    /// postalCode, country, text) is indexed as a string. When `path` includes a
    /// component (e.g. `["address", "city"]`) only that component is indexed.
    Address,
    /// CodeableConcept reached through one intermediate array element, e.g.
    /// `CareTeam.participant.role` where `participant` is an array. Yields code + system.
    NestedCodeableConcept,
    /// Scalar reached through one intermediate array element, e.g.
    /// `Goal.target.dueDate` where `target` is an array. `path[0]` is the array
    /// field, `path[1]` the scalar field on each element.
    NestedScalar,
    /// Reference reached through one intermediate array element, e.g.
    /// `Encounter.location.location` where `location` is an array of
    /// BackboneElements each carrying a Reference. `path[0]` is the array field,
    /// `path[1]` the Reference field on each element. Indexes full ref + bare id.
    NestedReference,
    /// Period datatype (e.g. `Observation.effectivePeriod`). `path[0]` is the
    /// Period field; emits a `"start/end"` value so the index stores the full
    /// date range (end omitted if the Period is open-ended).
    PeriodRange,
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
        definitions.insert("Provenance".to_string(), provenance_definitions());
        definitions.insert("CarePlan".to_string(), care_plan_definitions());
        definitions.insert("CareTeam".to_string(), care_team_definitions());
        definitions.insert("RelatedPerson".to_string(), related_person_definitions());
        definitions.insert("Location".to_string(), location_definitions());
        definitions.insert("PractitionerRole".to_string(), practitioner_role_definitions());
        definitions.insert("Goal".to_string(), goal_definitions());
        definitions.insert("Coverage".to_string(), coverage_definitions());
        definitions.insert("Device".to_string(), device_definitions());
        definitions.insert("MedicationDispense".to_string(), medication_dispense_definitions());
        definitions.insert("DocumentReference".to_string(), document_reference_definitions());
        definitions.insert("QuestionnaireResponse".to_string(), questionnaire_response_definitions());

        // Append FHIR-common parameters (e.g. _profile) to every resource-specific list
        let common = common_fhir_params();
        for defs in definitions.values_mut() {
            for c in &common {
                defs.push(c.clone());
            }
        }

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
            name: "death-date".to_string(),
            param_type: SearchParamType::Date,
            path: vec!["deceasedDateTime".to_string()],
            extraction: ExtractionMode::Simple,
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
        // US Core `name`: combined search across all HumanName components.
        // Five defs share the same param name so values are indexed under
        // a single "name" bucket; the CapabilityStatement dedup keeps the
        // declaration to one entry.
        SearchParamDef {
            name: "name".to_string(),
            param_type: SearchParamType::String,
            path: vec!["name".to_string(), "family".to_string()],
            extraction: ExtractionMode::ArrayField,
            aliases: vec![],
        },
        SearchParamDef {
            name: "name".to_string(),
            param_type: SearchParamType::String,
            path: vec!["name".to_string(), "given".to_string()],
            extraction: ExtractionMode::NestedArrayScalar,
            aliases: vec![],
        },
        SearchParamDef {
            name: "name".to_string(),
            param_type: SearchParamType::String,
            path: vec!["name".to_string(), "text".to_string()],
            extraction: ExtractionMode::ArrayField,
            aliases: vec![],
        },
        SearchParamDef {
            name: "name".to_string(),
            param_type: SearchParamType::String,
            path: vec!["name".to_string(), "prefix".to_string()],
            extraction: ExtractionMode::NestedArrayScalar,
            aliases: vec![],
        },
        SearchParamDef {
            name: "name".to_string(),
            param_type: SearchParamType::String,
            path: vec!["name".to_string(), "suffix".to_string()],
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
        // Observation.effective[x] may be an effectivePeriod (e.g. average blood
        // pressure). Index it as a full date range so range-aware date searches
        // match correctly (an instant `eq` won't match the wider Period).
        SearchParamDef {
            name: "date".to_string(),
            param_type: SearchParamType::Date,
            path: vec!["effectivePeriod".to_string()],
            extraction: ExtractionMode::PeriodRange,
            aliases: vec![],
        },
        // `combo-code` searches Observation.code OR Observation.component.code.
        // Component-level access requires array-of-CodeableConcept walking and is
        // tracked separately; for now we index the top-level code under combo-code
        // which covers single-value Observations (e.g. lab results).
        SearchParamDef {
            name: "combo-code".to_string(),
            param_type: SearchParamType::Token,
            path: vec!["code".to_string()],
            extraction: ExtractionMode::CodeableConcept,
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
        SearchParamDef {
            name: "class".to_string(),
            param_type: SearchParamType::Token,
            path: vec!["class".to_string()],
            extraction: ExtractionMode::Coding,
            aliases: vec![],
        },
        SearchParamDef {
            name: "type".to_string(),
            param_type: SearchParamType::Token,
            path: vec!["type".to_string()],
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
        SearchParamDef {
            name: "discharge-disposition".to_string(),
            param_type: SearchParamType::Token,
            path: vec!["hospitalization".to_string(), "dischargeDisposition".to_string()],
            extraction: ExtractionMode::CodeableConcept,
            aliases: vec![],
        },
        // Encounter.location.location — `location` is an array of BackboneElements,
        // each carrying a `location` Reference.
        SearchParamDef {
            name: "location".to_string(),
            param_type: SearchParamType::Reference,
            path: vec!["location".to_string(), "location".to_string()],
            extraction: ExtractionMode::NestedReference,
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
        SearchParamDef {
            name: "category".to_string(),
            param_type: SearchParamType::Token,
            path: vec!["category".to_string()],
            extraction: ExtractionMode::CodeableConcept,
            aliases: vec![],
        },
        SearchParamDef {
            name: "clinical-status".to_string(),
            param_type: SearchParamType::Token,
            path: vec!["clinicalStatus".to_string()],
            extraction: ExtractionMode::CodeableConcept,
            aliases: vec![],
        },
        SearchParamDef {
            name: "onset-date".to_string(),
            param_type: SearchParamType::Date,
            path: vec!["onsetDateTime".to_string()],
            extraction: ExtractionMode::Simple,
            aliases: vec![],
        },
        SearchParamDef {
            name: "recorded-date".to_string(),
            param_type: SearchParamType::Date,
            path: vec!["recordedDate".to_string()],
            extraction: ExtractionMode::Simple,
            aliases: vec![],
        },
        SearchParamDef {
            name: "abatement-date".to_string(),
            param_type: SearchParamType::Date,
            path: vec!["abatementDateTime".to_string()],
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
        SearchParamDef {
            name: "authoredon".to_string(),
            param_type: SearchParamType::Date,
            path: vec!["authoredOn".to_string()],
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
            name: "category".to_string(),
            param_type: SearchParamType::Token,
            path: vec!["category".to_string()],
            extraction: ExtractionMode::CodeableConcept,
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
        SearchParamDef {
            name: "name".to_string(),
            param_type: SearchParamType::String,
            path: vec!["name".to_string(), "family".to_string()],
            extraction: ExtractionMode::ArrayField,
            aliases: vec![],
        },
        SearchParamDef {
            name: "name".to_string(),
            param_type: SearchParamType::String,
            path: vec!["name".to_string(), "given".to_string()],
            extraction: ExtractionMode::NestedArrayScalar,
            aliases: vec![],
        },
        SearchParamDef {
            name: "name".to_string(),
            param_type: SearchParamType::String,
            path: vec!["name".to_string(), "text".to_string()],
            extraction: ExtractionMode::ArrayField,
            aliases: vec![],
        },
        SearchParamDef {
            name: "name".to_string(),
            param_type: SearchParamType::String,
            path: vec!["name".to_string(), "prefix".to_string()],
            extraction: ExtractionMode::NestedArrayScalar,
            aliases: vec![],
        },
        SearchParamDef {
            name: "name".to_string(),
            param_type: SearchParamType::String,
            path: vec!["name".to_string(), "suffix".to_string()],
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
        SearchParamDef {
            name: "address".to_string(),
            param_type: SearchParamType::String,
            path: vec!["address".to_string()],
            extraction: ExtractionMode::Address,
            aliases: vec![],
        },
    ]
}

fn practitioner_role_definitions() -> Vec<SearchParamDef> {
    vec![
        SearchParamDef {
            name: "practitioner".to_string(),
            param_type: SearchParamType::Reference,
            path: vec!["practitioner".to_string()],
            extraction: ExtractionMode::Reference,
            aliases: vec![],
        },
        SearchParamDef {
            name: "specialty".to_string(),
            param_type: SearchParamType::Token,
            path: vec!["specialty".to_string()],
            extraction: ExtractionMode::CodeableConcept,
            aliases: vec![],
        },
        SearchParamDef {
            name: "role".to_string(),
            param_type: SearchParamType::Token,
            path: vec!["code".to_string()],
            extraction: ExtractionMode::CodeableConcept,
            aliases: vec![],
        },
    ]
}

fn goal_definitions() -> Vec<SearchParamDef> {
    vec![
        SearchParamDef {
            name: "patient".to_string(),
            param_type: SearchParamType::Reference,
            path: vec!["subject".to_string()],
            extraction: ExtractionMode::Reference,
            aliases: vec!["subject".to_string()],
        },
        SearchParamDef {
            name: "lifecycle-status".to_string(),
            param_type: SearchParamType::Token,
            path: vec!["lifecycleStatus".to_string()],
            extraction: ExtractionMode::Simple,
            aliases: vec![],
        },
        SearchParamDef {
            name: "target-date".to_string(),
            param_type: SearchParamType::Date,
            path: vec!["target".to_string(), "dueDate".to_string()],
            extraction: ExtractionMode::NestedScalar,
            aliases: vec![],
        },
        SearchParamDef {
            name: "description".to_string(),
            param_type: SearchParamType::Token,
            path: vec!["description".to_string()],
            extraction: ExtractionMode::CodeableConcept,
            aliases: vec![],
        },
    ]
}

fn coverage_definitions() -> Vec<SearchParamDef> {
    vec![
        SearchParamDef {
            name: "patient".to_string(),
            param_type: SearchParamType::Reference,
            path: vec!["beneficiary".to_string()],
            extraction: ExtractionMode::Reference,
            aliases: vec!["beneficiary".to_string()],
        },
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

fn device_definitions() -> Vec<SearchParamDef> {
    vec![
        SearchParamDef {
            name: "patient".to_string(),
            param_type: SearchParamType::Reference,
            path: vec!["patient".to_string()],
            extraction: ExtractionMode::Reference,
            aliases: vec![],
        },
        SearchParamDef {
            name: "type".to_string(),
            param_type: SearchParamType::Token,
            path: vec!["type".to_string()],
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
    ]
}

fn medication_dispense_definitions() -> Vec<SearchParamDef> {
    vec![
        SearchParamDef {
            name: "patient".to_string(),
            param_type: SearchParamType::Reference,
            path: vec!["subject".to_string()],
            extraction: ExtractionMode::Reference,
            aliases: vec!["subject".to_string()],
        },
        SearchParamDef {
            name: "status".to_string(),
            param_type: SearchParamType::Token,
            path: vec!["status".to_string()],
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

fn document_reference_definitions() -> Vec<SearchParamDef> {
    vec![
        SearchParamDef {
            name: "patient".to_string(),
            param_type: SearchParamType::Reference,
            path: vec!["subject".to_string()],
            extraction: ExtractionMode::Reference,
            aliases: vec!["subject".to_string()],
        },
        SearchParamDef {
            name: "type".to_string(),
            param_type: SearchParamType::Token,
            path: vec!["type".to_string()],
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
            name: "date".to_string(),
            param_type: SearchParamType::Date,
            path: vec!["date".to_string()],
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
            name: "period".to_string(),
            param_type: SearchParamType::Date,
            path: vec!["context".to_string(), "period".to_string(), "start".to_string()],
            extraction: ExtractionMode::PeriodStart,
            aliases: vec![],
        },
    ]
}

fn questionnaire_response_definitions() -> Vec<SearchParamDef> {
    vec![
        SearchParamDef {
            name: "patient".to_string(),
            param_type: SearchParamType::Reference,
            path: vec!["subject".to_string()],
            extraction: ExtractionMode::Reference,
            aliases: vec!["subject".to_string()],
        },
        SearchParamDef {
            // QuestionnaireResponse.questionnaire is a canonical (plain string),
            // not a Reference object, so extract the scalar directly.
            name: "questionnaire".to_string(),
            param_type: SearchParamType::Reference,
            path: vec!["questionnaire".to_string()],
            extraction: ExtractionMode::Simple,
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
            name: "authored".to_string(),
            param_type: SearchParamType::Date,
            path: vec!["authored".to_string()],
            extraction: ExtractionMode::Simple,
            aliases: vec![],
        },
    ]
}

fn location_definitions() -> Vec<SearchParamDef> {
    vec![
        SearchParamDef {
            name: "name".to_string(),
            param_type: SearchParamType::String,
            path: vec!["name".to_string()],
            extraction: ExtractionMode::Simple,
            aliases: vec![],
        },
        SearchParamDef {
            name: "address".to_string(),
            param_type: SearchParamType::String,
            path: vec!["address".to_string()],
            extraction: ExtractionMode::Address,
            aliases: vec![],
        },
        SearchParamDef {
            name: "address-city".to_string(),
            param_type: SearchParamType::String,
            path: vec!["address".to_string(), "city".to_string()],
            extraction: ExtractionMode::Address,
            aliases: vec![],
        },
        SearchParamDef {
            name: "address-state".to_string(),
            param_type: SearchParamType::String,
            path: vec!["address".to_string(), "state".to_string()],
            extraction: ExtractionMode::Address,
            aliases: vec![],
        },
        SearchParamDef {
            name: "address-postalcode".to_string(),
            param_type: SearchParamType::String,
            path: vec!["address".to_string(), "postalCode".to_string()],
            extraction: ExtractionMode::Address,
            aliases: vec![],
        },
        SearchParamDef {
            name: "address-country".to_string(),
            param_type: SearchParamType::String,
            path: vec!["address".to_string(), "country".to_string()],
            extraction: ExtractionMode::Address,
            aliases: vec![],
        },
    ]
}

fn related_person_definitions() -> Vec<SearchParamDef> {
    vec![
        SearchParamDef {
            name: "patient".to_string(),
            param_type: SearchParamType::Reference,
            path: vec!["patient".to_string()],
            extraction: ExtractionMode::Reference,
            aliases: vec![],
        },
        SearchParamDef {
            name: "identifier".to_string(),
            param_type: SearchParamType::Token,
            path: vec!["identifier".to_string()],
            extraction: ExtractionMode::Identifier,
            aliases: vec![],
        },
        // RelatedPerson.name is a HumanName array, indexed like Patient.name so that
        // `name=` matches family, given, or the formatted text.
        SearchParamDef {
            name: "name".to_string(),
            param_type: SearchParamType::String,
            path: vec!["name".to_string(), "family".to_string()],
            extraction: ExtractionMode::ArrayField,
            aliases: vec![],
        },
        SearchParamDef {
            name: "name".to_string(),
            param_type: SearchParamType::String,
            path: vec!["name".to_string(), "given".to_string()],
            extraction: ExtractionMode::NestedArrayScalar,
            aliases: vec![],
        },
        SearchParamDef {
            name: "name".to_string(),
            param_type: SearchParamType::String,
            path: vec!["name".to_string(), "text".to_string()],
            extraction: ExtractionMode::ArrayField,
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
        SearchParamDef {
            name: "category".to_string(),
            param_type: SearchParamType::Token,
            path: vec!["category".to_string()],
            extraction: ExtractionMode::CodeableConcept,
            aliases: vec![],
        },
        SearchParamDef {
            name: "authored".to_string(),
            param_type: SearchParamType::Date,
            path: vec!["authoredOn".to_string()],
            extraction: ExtractionMode::Simple,
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

fn care_plan_definitions() -> Vec<SearchParamDef> {
    vec![
        SearchParamDef {
            name: "subject".to_string(),
            param_type: SearchParamType::Reference,
            path: vec!["subject".to_string()],
            extraction: ExtractionMode::Reference,
            aliases: vec!["patient".to_string()],
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
            name: "date".to_string(),
            param_type: SearchParamType::Date,
            path: vec!["period".to_string(), "start".to_string()],
            extraction: ExtractionMode::PeriodStart,
            aliases: vec![],
        },
    ]
}

fn care_team_definitions() -> Vec<SearchParamDef> {
    vec![
        SearchParamDef {
            name: "subject".to_string(),
            param_type: SearchParamType::Reference,
            path: vec!["subject".to_string()],
            extraction: ExtractionMode::Reference,
            aliases: vec!["patient".to_string()],
        },
        SearchParamDef {
            name: "status".to_string(),
            param_type: SearchParamType::Token,
            path: vec!["status".to_string()],
            extraction: ExtractionMode::Simple,
            aliases: vec![],
        },
        // CareTeam.participant.role — `participant` is an array, so the role
        // CodeableConcept is reached through one intermediate array element.
        SearchParamDef {
            name: "role".to_string(),
            param_type: SearchParamType::Token,
            path: vec!["participant".to_string(), "role".to_string()],
            extraction: ExtractionMode::NestedCodeableConcept,
            aliases: vec![],
        },
    ]
}

fn provenance_definitions() -> Vec<SearchParamDef> {
    vec![
        SearchParamDef {
            name: "target".to_string(),
            param_type: SearchParamType::Reference,
            path: vec!["target".to_string()],
            extraction: ExtractionMode::Reference,
            aliases: vec!["patient".to_string()],
        },
        SearchParamDef {
            name: "recorded".to_string(),
            param_type: SearchParamType::Date,
            path: vec!["recorded".to_string()],
            extraction: ExtractionMode::Simple,
            aliases: vec![],
        },
    ]
}

/// Fallback for resource types without explicit definitions. Kept minimal
/// to avoid indexing non-existent fields on arbitrary resources.
fn common_definitions() -> Vec<SearchParamDef> {
    common_fhir_params()
}

/// Parameters defined by the base FHIR spec that apply to every resource
/// (`_id`, `_lastUpdated`, `_profile`, `_tag`, `_security`).
/// Appended to every resource-specific list.
fn common_fhir_params() -> Vec<SearchParamDef> {
    vec![
        SearchParamDef {
            name: "_id".to_string(),
            param_type: SearchParamType::Token,
            path: vec!["id".to_string()],
            extraction: ExtractionMode::Simple,
            aliases: vec![],
        },
        SearchParamDef {
            name: "_lastUpdated".to_string(),
            param_type: SearchParamType::Date,
            path: vec!["meta".to_string(), "lastUpdated".to_string()],
            extraction: ExtractionMode::Simple,
            aliases: vec![],
        },
        SearchParamDef {
            name: "_profile".to_string(),
            param_type: SearchParamType::Token,
            path: vec!["meta".to_string(), "profile".to_string()],
            extraction: ExtractionMode::UriArray,
            aliases: vec![],
        },
        SearchParamDef {
            name: "_tag".to_string(),
            param_type: SearchParamType::Token,
            path: vec!["meta".to_string(), "tag".to_string()],
            extraction: ExtractionMode::CodingArray,
            aliases: vec![],
        },
        SearchParamDef {
            name: "_security".to_string(),
            param_type: SearchParamType::Token,
            path: vec!["meta".to_string(), "security".to_string()],
            extraction: ExtractionMode::CodingArray,
            aliases: vec![],
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_registry_has_all_resource_types() {
        let registry = SearchParamRegistry::new();
        let types = [
            "Patient", "Observation", "Encounter", "Condition",
            "MedicationRequest", "Procedure", "AllergyIntolerance",
            "DiagnosticReport", "Immunization", "Task",
            "Practitioner", "Organization", "Bundle",
            "ServiceRequest", "Appointment", "Specimen",
            "Provenance",
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
    fn test_provenance_target_param() {
        let registry = SearchParamRegistry::new();
        assert_eq!(
            registry.lookup_param_type("Provenance", "target"),
            Some(SearchParamType::Reference)
        );
        // Alias `patient` for compartment-style searches
        assert_eq!(
            registry.lookup_param_type("Provenance", "patient"),
            Some(SearchParamType::Reference)
        );
    }

    #[test]
    fn test_fallback_for_unknown_resource() {
        let registry = SearchParamRegistry::new();
        let defs = registry.get_definitions("UnknownResource");
        // Fallback exposes FHIR-common params only.
        let names: Vec<&str> = defs.iter().map(|d| d.name.as_str()).collect();
        assert!(names.contains(&"_id"));
        assert!(names.contains(&"_lastUpdated"));
        assert!(names.contains(&"_profile"));
        assert!(names.contains(&"_tag"));
        assert!(names.contains(&"_security"));
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
    fn test_patient_name_param_registered() {
        let registry = SearchParamRegistry::new();
        assert_eq!(
            registry.lookup_param_type("Patient", "name"),
            Some(SearchParamType::String)
        );
        // Existing family/given still work
        assert_eq!(
            registry.lookup_param_type("Patient", "family"),
            Some(SearchParamType::String)
        );
        assert_eq!(
            registry.lookup_param_type("Patient", "given"),
            Some(SearchParamType::String)
        );
    }

    #[test]
    fn test_practitioner_name_param_registered() {
        let registry = SearchParamRegistry::new();
        assert_eq!(
            registry.lookup_param_type("Practitioner", "name"),
            Some(SearchParamType::String)
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
