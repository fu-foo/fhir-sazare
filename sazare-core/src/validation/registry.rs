use serde_json::Value;
use std::collections::{HashMap, HashSet};

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

    /// Canonical URLs of every loaded profile constraining `resource_type`,
    /// sorted. Used to advertise `supportedProfile` in the CapabilityStatement
    /// directly from what is loaded, so it can't drift from reality.
    pub fn supported_profile_urls(&self, resource_type: &str) -> Vec<String> {
        let mut urls: Vec<String> = self
            .profiles
            .values()
            .filter(|p| p.get("type").and_then(|t| t.as_str()) == Some(resource_type))
            .filter_map(|p| p.get("url").and_then(|u| u.as_str()).map(String::from))
            .collect();
        urls.sort();
        urls
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
    /// ValueSet url → its enumerated codes (only fully resolved sets; codes
    /// of whole included systems live in `code_systems`).
    value_sets: HashMap<String, HashSet<String>>,
    /// ValueSet url → code systems it includes *whole* (`include.system` with
    /// no concept list or filter).
    whole_systems: HashMap<String, Vec<String>>,
    /// CodeSystem url → its codes (only `content: complete` systems).
    code_systems: HashMap<String, HashSet<String>>,
}

/// Counts from [`TerminologyRegistry::load_resources`].
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct TerminologyLoadSummary {
    pub code_systems: usize,
    pub value_sets: usize,
    /// ValueSets that couldn't be fully resolved (filters, references to
    /// other value sets or to code systems not loaded) — not registered, so
    /// their bindings stay unchecked rather than checked against a partial list.
    pub value_sets_skipped: usize,
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

/// Just the parts of a CodeSystem / ValueSet that validation uses. Parsing
/// into this (instead of a full `Value`) skips designations, properties and
/// narrative, so a 23 MB CodeSystem doesn't balloon into hundreds of MB.
#[derive(serde::Deserialize)]
struct LeanTerm {
    #[serde(rename = "resourceType")]
    resource_type: String,
    url: Option<String>,
    content: Option<String>,
    #[serde(default)]
    concept: Vec<LeanConcept>,
    compose: Option<Value>,
    expansion: Option<Value>,
}

#[derive(serde::Deserialize)]
struct LeanConcept {
    code: Option<String>,
    #[serde(default)]
    concept: Vec<LeanConcept>,
}

/// Streaming terminology load: CodeSystems are registered as they arrive;
/// ValueSets (small) are held until every CodeSystem is in, since a ValueSet
/// may include a whole system that comes later.
#[derive(Default)]
pub struct TerminologyLoader {
    value_sets: Vec<Value>,
    summary: TerminologyLoadSummary,
}

impl TerminologyLoader {
    /// Add one resource given as JSON text. Non-terminology resources and
    /// unparseable files are ignored.
    pub fn add_json(&mut self, registry: &mut TerminologyRegistry, json: &str) {
        let Ok(term) = serde_json::from_str::<LeanTerm>(json) else {
            return;
        };
        match term.resource_type.as_str() {
            "CodeSystem" => {
                if term.content.as_deref() != Some("complete") {
                    return;
                }
                let Some(url) = term.url else { return };
                let mut codes = HashSet::new();
                fn walk(concepts: Vec<LeanConcept>, out: &mut HashSet<String>) {
                    for c in concepts {
                        if let Some(code) = c.code {
                            out.insert(code);
                        }
                        walk(c.concept, out);
                    }
                }
                walk(term.concept, &mut codes);
                if !codes.is_empty() {
                    registry.code_systems.insert(url, codes);
                    self.summary.code_systems += 1;
                }
            }
            "ValueSet" => {
                let mut vs = serde_json::Map::new();
                if let Some(url) = term.url {
                    vs.insert("url".into(), Value::String(url));
                }
                if let Some(c) = term.compose {
                    vs.insert("compose".into(), c);
                }
                if let Some(e) = term.expansion {
                    vs.insert("expansion".into(), e);
                }
                self.value_sets.push(Value::Object(vs));
            }
            _ => {}
        }
    }

    /// Add one already-parsed resource.
    pub fn add(&mut self, registry: &mut TerminologyRegistry, resource: &Value) {
        match resource.get("resourceType").and_then(|v| v.as_str()) {
            Some("CodeSystem") => {
                if registry.load_code_system(resource) {
                    self.summary.code_systems += 1;
                }
            }
            Some("ValueSet") => self.value_sets.push(resource.clone()),
            _ => {}
        }
    }

    pub fn finish(mut self, registry: &mut TerminologyRegistry) -> TerminologyLoadSummary {
        for vs in &self.value_sets {
            if registry.load_value_set(vs) {
                self.summary.value_sets += 1;
            } else {
                self.summary.value_sets_skipped += 1;
            }
        }
        self.summary
    }
}

impl TerminologyRegistry {
    pub fn new() -> Self {
        let mut registry = Self {
            value_sets: HashMap::new(),
            whole_systems: HashMap::new(),
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

    /// Load enumerated codes from a FHIR `ValueSet` resource (JSON). Only
    /// ValueSets whose codes are fully known (enumerated concepts, whole code
    /// systems already loaded, or an expansion) are registered.
    pub fn load_value_set_resource(&mut self, json: &str) {
        if let Ok(vs) = serde_json::from_str::<Value>(json) {
            self.load_value_set(&vs);
        }
    }

    /// Load an IG's terminology: `CodeSystem`s first (so ValueSets can include
    /// them whole), then `ValueSet`s. Other resource types are ignored.
    pub fn load_resources(&mut self, resources: &[Value]) -> TerminologyLoadSummary {
        let mut loader = TerminologyLoader::default();
        for r in resources {
            loader.add(self, r);
        }
        loader.finish(self)
    }

    /// Register a `content: complete` CodeSystem with at least one concept
    /// (nested concepts included). Returns whether it was registered.
    pub fn load_code_system(&mut self, cs: &Value) -> bool {
        let Some(url) = cs.get("url").and_then(|v| v.as_str()) else {
            return false;
        };
        if cs.get("content").and_then(|v| v.as_str()) != Some("complete") {
            return false;
        }
        let mut codes = HashSet::new();
        fn walk(concepts: Option<&Value>, out: &mut HashSet<String>) {
            for c in concepts.and_then(|v| v.as_array()).into_iter().flatten() {
                if let Some(code) = c.get("code").and_then(|v| v.as_str()) {
                    out.insert(code.to_string());
                }
                walk(c.get("concept"), out);
            }
        }
        walk(cs.get("concept"), &mut codes);
        if codes.is_empty() {
            return false;
        }
        self.code_systems.insert(url.to_string(), codes);
        true
    }

    /// Resolve and register a ValueSet. All-or-nothing: if any `include`
    /// can't be resolved (filter, nested value set, unknown or incomplete code
    /// system) or there is an `exclude`, nothing is registered — a partial
    /// code list would reject valid codes.
    pub fn load_value_set(&mut self, vs: &Value) -> bool {
        let Some(url) = vs.get("url").and_then(|v| v.as_str()) else {
            return false;
        };
        let mut codes: HashSet<String> = HashSet::new();
        let mut whole: Vec<String> = Vec::new();

        if let Some(compose) = vs.get("compose") {
            if compose.get("exclude").is_some() {
                return false;
            }
            for inc in compose.get("include").and_then(|v| v.as_array()).into_iter().flatten() {
                if inc.get("filter").is_some() || inc.get("valueSet").is_some() {
                    return false;
                }
                if let Some(concepts) = inc.get("concept").and_then(|v| v.as_array()) {
                    codes.extend(
                        concepts
                            .iter()
                            .filter_map(|c| c.get("code").and_then(|v| v.as_str()))
                            .map(str::to_string),
                    );
                } else if let Some(system) = inc.get("system").and_then(|v| v.as_str()) {
                    // Referenced, not copied: membership is checked against
                    // the code system itself.
                    if !self.code_systems.contains_key(system) {
                        return false;
                    }
                    whole.push(system.to_string());
                } else {
                    return false;
                }
            }
        } else {
            fn walk(contains: Option<&Value>, out: &mut HashSet<String>) {
                for c in contains.and_then(|v| v.as_array()).into_iter().flatten() {
                    if let Some(code) = c.get("code").and_then(|v| v.as_str()) {
                        out.insert(code.to_string());
                    }
                    walk(c.get("contains"), out);
                }
            }
            walk(vs.get("expansion").and_then(|e| e.get("contains")), &mut codes);
        }

        if codes.is_empty() && whole.is_empty() {
            return false;
        }
        self.value_sets.insert(url.to_string(), codes);
        if !whole.is_empty() {
            self.whole_systems.insert(url.to_string(), whole);
        }
        true
    }

    pub fn add_value_set(&mut self, value_set: ValueSet) {
        self.value_sets
            .insert(value_set.url, value_set.codes.into_iter().collect());
    }

    pub fn add_code_system(&mut self, code_system: CodeSystem) {
        self.code_systems
            .insert(code_system.url, code_system.codes.into_iter().collect());
    }

    /// True if the ValueSet includes the whole of `system` — so any code
    /// claiming that system must be one of the system's codes.
    pub fn value_set_includes_whole_system(&self, value_set_url: &str, system: &str) -> bool {
        self.whole_systems
            .get(value_set_url)
            .is_some_and(|systems| systems.iter().any(|s| s == system))
    }

    /// Whether a loaded, complete CodeSystem contains `code`; `None` if the
    /// system isn't loaded.
    pub fn code_system_contains(&self, system: &str, code: &str) -> Option<bool> {
        self.code_systems.get(system).map(|codes| codes.contains(code))
    }

    /// Validate a code against a ValueSet
    pub fn validate_code(&self, value_set_url: &str, code: &str) -> bool {
        match self.value_sets.get(value_set_url) {
            Some(codes) => {
                codes.contains(code)
                    || self.whole_systems.get(value_set_url).is_some_and(|systems| {
                        systems
                            .iter()
                            .any(|s| self.code_systems.get(s).is_some_and(|c| c.contains(code)))
                    })
            }
            // If ValueSet is not known, allow the code
            None => true,
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

    fn ig_terms() -> TerminologyRegistry {
        let mut reg = TerminologyRegistry::new();
        let mut loader = TerminologyLoader::default();
        let resources = [
            // ValueSet before its CodeSystem: the loader must still resolve it.
            json!({"resourceType": "ValueSet", "url": "http://x/vs/whole",
                   "compose": {"include": [{"system": "http://x/cs/a"}]}}),
            json!({"resourceType": "CodeSystem", "url": "http://x/cs/a", "content": "complete",
                   "concept": [{"code": "a1", "concept": [{"code": "a1.1"}]}, {"code": "a2"}]}),
            json!({"resourceType": "CodeSystem", "url": "http://x/cs/frag", "content": "fragment",
                   "concept": [{"code": "f1"}]}),
            json!({"resourceType": "CodeSystem", "url": "http://x/cs/empty", "content": "complete"}),
            json!({"resourceType": "ValueSet", "url": "http://x/vs/enum",
                   "compose": {"include": [{"system": "http://x/any", "concept": [{"code": "e1"}]}]}}),
            json!({"resourceType": "ValueSet", "url": "http://x/vs/filter",
                   "compose": {"include": [{"system": "http://x/cs/a", "filter": [{"property": "concept", "op": "is-a", "value": "a1"}]}]}}),
            json!({"resourceType": "ValueSet", "url": "http://x/vs/frag",
                   "compose": {"include": [{"system": "http://x/cs/frag"}]}}),
            json!({"resourceType": "ValueSet", "url": "http://x/vs/empty",
                   "compose": {"include": [{"system": "http://x/cs/empty"}]}}),
            json!({"resourceType": "ValueSet", "url": "http://x/vs/excl",
                   "compose": {"include": [{"system": "http://x/cs/a"}],
                               "exclude": [{"system": "http://x/cs/a", "concept": [{"code": "a2"}]}]}}),
        ];
        for r in &resources {
            loader.add_json(&mut reg, &r.to_string());
        }
        let summary = loader.finish(&mut reg);
        assert_eq!(summary.code_systems, 1, "only the complete, non-empty system");
        assert_eq!(summary.value_sets, 2, "whole + enum");
        assert_eq!(summary.value_sets_skipped, 4, "filter, fragment, empty, exclude");
        reg
    }

    #[test]
    fn test_ig_terminology_resolves_only_complete_value_sets() {
        let reg = ig_terms();
        // Whole-system include: nested concepts count, others don't.
        assert!(reg.validate_code("http://x/vs/whole", "a1.1"));
        assert!(!reg.validate_code("http://x/vs/whole", "zz"));
        assert!(reg.value_set_includes_whole_system("http://x/vs/whole", "http://x/cs/a"));
        assert!(reg.validate_code("http://x/vs/enum", "e1"));
        assert!(!reg.value_set_includes_whole_system("http://x/vs/enum", "http://x/any"));
        // Unresolvable value sets aren't registered, so they never reject.
        for vs in ["http://x/vs/filter", "http://x/vs/frag", "http://x/vs/empty", "http://x/vs/excl"] {
            assert!(!reg.has_value_set(vs), "{vs}");
            assert!(reg.validate_code(vs, "anything"));
        }
        assert_eq!(reg.code_system_contains("http://x/cs/a", "a2"), Some(true));
        assert_eq!(reg.code_system_contains("http://x/cs/a", "zz"), Some(false));
        assert_eq!(reg.code_system_contains("http://x/cs/frag", "f1"), None);
    }
}
