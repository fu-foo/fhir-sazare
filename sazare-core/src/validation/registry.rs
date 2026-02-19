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

        registry
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
