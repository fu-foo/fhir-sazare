use serde_json::Value;
use std::collections::HashMap;

/// Registry for FHIR profiles (StructureDefinitions)
#[derive(Debug, Clone)]
pub struct ProfileRegistry {
    profiles: HashMap<String, Value>,
}

impl ProfileRegistry {
    pub fn new() -> Self {
        Self {
            profiles: HashMap::new(),
        }
    }

    /// Add a profile to the registry
    pub fn add_profile(&mut self, profile: Value) {
        if let Some(url) = profile.get("url").and_then(|v| v.as_str()) {
            self.profiles.insert(url.to_string(), profile);
        }
    }

    /// Get a profile by URL
    pub fn get_profile(&self, url: &str) -> Option<&Value> {
        self.profiles.get(url)
    }

    /// Load multiple profiles
    pub fn load_profiles(&mut self, profiles: Vec<Value>) {
        for profile in profiles {
            self.add_profile(profile);
        }
    }

    /// Get required elements from a profile
    pub fn get_required_elements(&self, profile_url: &str) -> Vec<String> {
        if let Some(profile) = self.get_profile(profile_url) {
            let mut required = Vec::new();

            if let Some(elements) = profile
                .get("differential")
                .or_else(|| profile.get("snapshot"))
                .and_then(|d| d.get("element"))
                .and_then(|e| e.as_array())
            {
                for element in elements {
                    if let Some(min) = element.get("min").and_then(|v| v.as_i64())
                        && min > 0
                        && let Some(path) = element.get("path").and_then(|v| v.as_str())
                    {
                        required.push(path.to_string());
                    }
                }
            }

            required
        } else {
            Vec::new()
        }
    }
}

impl Default for ProfileRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Registry for FHIR terminologies (ValueSets and CodeSystems)
#[derive(Debug, Clone)]
pub struct TerminologyRegistry {
    value_sets: HashMap<String, ValueSet>,
    code_systems: HashMap<String, CodeSystem>,
}

#[derive(Debug, Clone)]
pub struct ValueSet {
    pub url: String,
    pub codes: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct CodeSystem {
    pub url: String,
    pub codes: Vec<String>,
}

impl TerminologyRegistry {
    pub fn new() -> Self {
        let mut registry = Self {
            value_sets: HashMap::new(),
            code_systems: HashMap::new(),
        };

        // Add common FHIR terminologies
        registry.add_value_set(ValueSet {
            url: "http://hl7.org/fhir/ValueSet/administrative-gender".to_string(),
            codes: vec![
                "male".to_string(),
                "female".to_string(),
                "other".to_string(),
                "unknown".to_string(),
            ],
        });

        registry.add_value_set(ValueSet {
            url: "http://hl7.org/fhir/ValueSet/observation-status".to_string(),
            codes: vec![
                "registered".to_string(),
                "preliminary".to_string(),
                "final".to_string(),
                "amended".to_string(),
                "corrected".to_string(),
                "cancelled".to_string(),
                "entered-in-error".to_string(),
                "unknown".to_string(),
            ],
        });

        registry.add_value_set(ValueSet {
            url: "http://hl7.org/fhir/ValueSet/task-status".to_string(),
            codes: vec![
                "draft".to_string(),
                "requested".to_string(),
                "received".to_string(),
                "accepted".to_string(),
                "rejected".to_string(),
                "ready".to_string(),
                "cancelled".to_string(),
                "in-progress".to_string(),
                "on-hold".to_string(),
                "failed".to_string(),
                "completed".to_string(),
                "entered-in-error".to_string(),
            ],
        });

        // Common required code bindings (small, stable FHIR R4 value sets).
        let vs = |url: &str, codes: &[&str]| ValueSet {
            url: url.to_string(),
            codes: codes.iter().map(|c| c.to_string()).collect(),
        };
        registry.add_value_set(vs(
            "http://hl7.org/fhir/ValueSet/encounter-status",
            &["planned", "arrived", "triaged", "in-progress", "onleave", "finished",
              "cancelled", "entered-in-error", "unknown"],
        ));
        registry.add_value_set(vs(
            "http://hl7.org/fhir/ValueSet/medicationrequest-status",
            &["active", "on-hold", "cancelled", "completed", "entered-in-error",
              "stopped", "draft", "unknown"],
        ));
        registry.add_value_set(vs(
            "http://hl7.org/fhir/ValueSet/medicationrequest-intent",
            &["proposal", "plan", "order", "original-order", "reflex-order",
              "filler-order", "instance-order", "option"],
        ));
        registry.add_value_set(vs(
            "http://hl7.org/fhir/ValueSet/event-status",
            &["preparation", "in-progress", "not-done", "on-hold", "stopped",
              "completed", "entered-in-error", "unknown"],
        ));
        registry.add_value_set(vs(
            "http://hl7.org/fhir/ValueSet/immunization-status",
            &["completed", "entered-in-error", "not-done"],
        ));
        registry.add_value_set(vs(
            "http://hl7.org/fhir/ValueSet/allergy-intolerance-criticality",
            &["low", "high", "unable-to-assess"],
        ));
        registry.add_value_set(vs(
            "http://hl7.org/fhir/ValueSet/medicationdispense-status",
            &["preparation", "in-progress", "cancelled", "on-hold", "completed",
              "entered-in-error", "stopped", "declined", "unknown"],
        ));
        registry.add_value_set(vs(
            "http://hl7.org/fhir/ValueSet/request-status",
            &["draft", "active", "on-hold", "revoked", "completed",
              "entered-in-error", "unknown"],
        ));
        registry.add_value_set(vs(
            "http://hl7.org/fhir/ValueSet/request-intent",
            &["proposal", "plan", "directive", "order", "original-order",
              "reflex-order", "filler-order", "instance-order", "option"],
        ));
        registry.add_value_set(vs(
            "http://hl7.org/fhir/ValueSet/diagnostic-report-status",
            &["registered", "partial", "preliminary", "final", "amended",
              "corrected", "appended", "cancelled", "entered-in-error", "unknown"],
        ));
        registry.add_value_set(vs(
            "http://hl7.org/fhir/ValueSet/specimen-status",
            &["available", "unavailable", "unsatisfactory", "entered-in-error"],
        ));
        registry.add_value_set(vs(
            "http://hl7.org/fhir/ValueSet/fm-status",
            &["active", "cancelled", "draft", "entered-in-error"],
        ));
        registry.add_value_set(vs(
            "http://hl7.org/fhir/ValueSet/care-team-status",
            &["proposed", "active", "suspended", "inactive", "entered-in-error"],
        ));
        registry.add_value_set(vs(
            "http://hl7.org/fhir/ValueSet/goal-status",
            &["proposed", "planned", "accepted", "active", "on-hold", "completed",
              "cancelled", "entered-in-error", "rejected"],
        ));
        // CodeableConcept-typed status bindings.
        registry.add_value_set(vs(
            "http://hl7.org/fhir/ValueSet/condition-clinical",
            &["active", "recurrence", "relapse", "inactive", "remission", "resolved"],
        ));
        registry.add_value_set(vs(
            "http://hl7.org/fhir/ValueSet/condition-ver-status",
            &["unconfirmed", "provisional", "differential", "confirmed", "refuted",
              "entered-in-error"],
        ));
        registry.add_value_set(vs(
            "http://hl7.org/fhir/ValueSet/allergyintolerance-clinical",
            &["active", "inactive", "resolved"],
        ));
        registry.add_value_set(vs(
            "http://hl7.org/fhir/ValueSet/allergyintolerance-verification",
            &["unconfirmed", "presumed", "confirmed", "refuted", "entered-in-error"],
        ));

        registry
    }

    /// True if a ValueSet with this URL is known (enumerated) in the registry.
    pub fn has_value_set(&self, url: &str) -> bool {
        self.value_sets.contains_key(url)
    }

    /// Load enumerated codes from a FHIR `ValueSet` resource (JSON), taking the
    /// codes from `compose.include[].concept[]` and `expansion.contains[]`.
    /// ValueSets that only reference whole code systems (no enumerated codes)
    /// are ignored — they need a terminology service, not embedding.
    pub fn load_value_set_resource(&mut self, json: &str) {
        let vs: Value = match serde_json::from_str(json) {
            Ok(v) => v,
            Err(_) => return,
        };
        let Some(url) = vs.get("url").and_then(|v| v.as_str()) else {
            return;
        };
        let mut codes: Vec<String> = Vec::new();
        if let Some(includes) = vs
            .get("compose")
            .and_then(|c| c.get("include"))
            .and_then(|v| v.as_array())
        {
            for inc in includes {
                if let Some(concepts) = inc.get("concept").and_then(|v| v.as_array()) {
                    codes.extend(
                        concepts
                            .iter()
                            .filter_map(|c| c.get("code").and_then(|v| v.as_str()))
                            .map(|s| s.to_string()),
                    );
                }
            }
        }
        if let Some(contains) = vs
            .get("expansion")
            .and_then(|e| e.get("contains"))
            .and_then(|v| v.as_array())
        {
            codes.extend(
                contains
                    .iter()
                    .filter_map(|c| c.get("code").and_then(|v| v.as_str()))
                    .map(|s| s.to_string()),
            );
        }
        if !codes.is_empty() {
            self.add_value_set(ValueSet {
                url: url.to_string(),
                codes,
            });
        }
    }

    pub fn add_value_set(&mut self, value_set: ValueSet) {
        self.value_sets.insert(value_set.url.clone(), value_set);
    }

    pub fn add_code_system(&mut self, code_system: CodeSystem) {
        self.code_systems
            .insert(code_system.url.clone(), code_system);
    }

    /// Validate a code against a ValueSet
    pub fn validate_code(&self, value_set_url: &str, code: &str) -> bool {
        if let Some(value_set) = self.value_sets.get(value_set_url) {
            value_set.codes.contains(&code.to_string())
        } else {
            // If ValueSet is not known, allow the code
            true
        }
    }

    /// Validate a CodeableConcept against a ValueSet
    pub fn validate_codeable_concept(&self, value_set_url: &str, concept: &Value) -> bool {
        if let Some(codings) = concept.get("coding").and_then(|v| v.as_array()) {
            for coding in codings {
                if let Some(code) = coding.get("code").and_then(|v| v.as_str())
                    && self.validate_code(value_set_url, code)
                {
                    return true;
                }
            }
            false
        } else {
            // No coding, check if text is present
            concept.get("text").is_some()
        }
    }
}

impl Default for TerminologyRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_profile_registry() {
        let mut registry = ProfileRegistry::new();

        let profile = json!({
            "resourceType": "StructureDefinition",
            "url": "http://example.com/StructureDefinition/TestProfile",
            "name": "TestProfile"
        });

        registry.add_profile(profile.clone());

        assert!(registry
            .get_profile("http://example.com/StructureDefinition/TestProfile")
            .is_some());
    }

    #[test]
    fn test_terminology_registry_gender() {
        let registry = TerminologyRegistry::new();

        assert!(registry.validate_code(
            "http://hl7.org/fhir/ValueSet/administrative-gender",
            "male"
        ));
        assert!(registry.validate_code(
            "http://hl7.org/fhir/ValueSet/administrative-gender",
            "female"
        ));
        assert!(!registry.validate_code(
            "http://hl7.org/fhir/ValueSet/administrative-gender",
            "invalid"
        ));
    }

    #[test]
    fn test_validate_codeable_concept() {
        let registry = TerminologyRegistry::new();

        let concept = json!({
            "coding": [{
                "system": "http://hl7.org/fhir/administrative-gender",
                "code": "male"
            }]
        });

        assert!(registry.validate_codeable_concept(
            "http://hl7.org/fhir/ValueSet/administrative-gender",
            &concept
        ));
    }
}
