use crate::operation_outcome::{IssueSeverity, IssueType, OperationOutcome, OperationOutcomeIssue};
use crate::validation::registry::ProfileRegistry;
use serde_json::Value;

/// Phase 2: Extension validation + Profile-based validation
pub struct Phase2Validator;

impl Phase2Validator {
    /// Validate extensions and profile constraints.
    ///
    /// Returns `Ok(Vec<OperationOutcomeIssue>)` when there are no errors (may contain warnings),
    /// or `Err(OperationOutcome)` when there are hard errors.
    pub fn validate(
        resource: &Value,
        registry: &ProfileRegistry,
    ) -> Result<Vec<OperationOutcomeIssue>, OperationOutcome> {
        let mut issues: Vec<OperationOutcomeIssue> = Vec::new();

        // --- Existing extension structure validation ---
        Self::validate_extensions(resource)?;

        // --- Profile-based validation (Phase 2 enhancement) ---
        if let Some(profiles) = resource
            .get("meta")
            .and_then(|m| m.get("profile"))
            .and_then(|p| p.as_array())
        {
            let resource_type = resource
                .get("resourceType")
                .and_then(|v| v.as_str())
                .unwrap_or("");

            for profile_url_value in profiles {
                let profile_url = match profile_url_value.as_str() {
                    Some(url) => url,
                    None => continue,
                };

                let profile = match registry.get_profile(profile_url) {
                    Some(p) => p,
                    None => {
                        // Profile not in registry - emit a warning, not an error
                        issues.push(OperationOutcomeIssue {
                            severity: IssueSeverity::Warning,
                            code: IssueType::NotFound,
                            diagnostics: Some(format!(
                                "Profile '{}' not found in registry; skipping profile validation",
                                profile_url
                            )),
                            details: None,
                            expression: Some(vec!["meta.profile".to_string()]),
                        });
                        continue;
                    }
                };

                // Extract elements from snapshot (preferred) or differential
                let elements = profile
                    .get("snapshot")
                    .or_else(|| profile.get("differential"))
                    .and_then(|d| d.get("element"))
                    .and_then(|e| e.as_array());

                if let Some(elements) = elements {
                    Self::validate_profile_elements(
                        resource,
                        resource_type,
                        elements,
                        profile_url,
                        &mut issues,
                    );
                }
            }
        }

        // If any issue is an error, return Err
        let has_error = issues
            .iter()
            .any(|i| matches!(i.severity, IssueSeverity::Error | IssueSeverity::Fatal));

        if has_error {
            let mut outcome =
                OperationOutcome::error(IssueType::Invalid, "Profile validation failed");
            // Replace the default issue with our collected issues
            outcome.issue = issues;
            Err(outcome)
        } else {
            Ok(issues)
        }
    }

    /// Validate extension structure (existing Phase 2 logic).
    fn validate_extensions(resource: &Value) -> Result<(), OperationOutcome> {
        if let Some(extensions) = resource.get("extension").and_then(|e| e.as_array()) {
            for (idx, extension) in extensions.iter().enumerate() {
                // Each extension must have a 'url'
                if extension.get("url").is_none() {
                    return Err(OperationOutcome::validation_error(format!(
                        "Extension at index {} is missing required 'url' field",
                        idx
                    ))
                    .with_expression(vec![format!("extension[{}].url", idx)]));
                }

                // Extension must have at least one value[x] or extension
                let has_value = extension
                    .as_object()
                    .map(|obj| {
                        obj.keys()
                            .any(|k| k.starts_with("value") || k == "extension")
                    })
                    .unwrap_or(false);

                if !has_value {
                    return Err(OperationOutcome::validation_error(format!(
                        "Extension at index {} must have either a value or nested extensions",
                        idx
                    ))
                    .with_expression(vec![format!("extension[{}]", idx)]));
                }
            }
        }
        Ok(())
    }

    /// Validate resource against profile element definitions.
    fn validate_profile_elements(
        resource: &Value,
        resource_type: &str,
        elements: &[Value],
        profile_url: &str,
        issues: &mut Vec<OperationOutcomeIssue>,
    ) {
        for element in elements {
            let path = match element.get("path").and_then(|v| v.as_str()) {
                Some(p) => p,
                None => continue,
            };

            // Skip the root element (e.g., "Observation")
            if path == resource_type || !path.contains('.') {
                continue;
            }

            // Extract the relative path after the resource type prefix
            let relative_path = match path.strip_prefix(&format!("{}.", resource_type)) {
                Some(rp) => rp,
                None => continue,
            };

            let min = element.get("min").and_then(|v| v.as_u64());
            let max_str = element.get("max").and_then(|v| v.as_str());
            let must_support = element
                .get("mustSupport")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);

            // --- Required element validation (min >= 1) ---
            if let Some(min_val) = min
                && min_val >= 1
            {
                let count = Self::count_element(resource, relative_path);
                if count < min_val {
                    issues.push(OperationOutcomeIssue {
                        severity: IssueSeverity::Error,
                        code: IssueType::Required,
                        diagnostics: Some(format!(
                            "Profile '{}' requires element '{}' (min={}) but found {} occurrence(s)",
                            profile_url, path, min_val, count
                        )),
                        details: None,
                        expression: Some(vec![path.to_string()]),
                    });
                }
            }

            // --- Max cardinality validation ---
            if let Some(max) = max_str
                && max != "*"
                && let Ok(max_val) = max.parse::<u64>()
            {
                let count = Self::count_element(resource, relative_path);
                if count > max_val {
                    issues.push(OperationOutcomeIssue {
                        severity: IssueSeverity::Error,
                        code: IssueType::BusinessRule,
                        diagnostics: Some(format!(
                            "Profile '{}': element '{}' exceeds max cardinality (max={}, found={})",
                            profile_url, path, max_val, count
                        )),
                        details: None,
                        expression: Some(vec![path.to_string()]),
                    });
                }
            }

            // --- Must-Support validation (warning level) ---
            if must_support {
                let count = Self::count_element(resource, relative_path);
                if count == 0 {
                    issues.push(OperationOutcomeIssue {
                        severity: IssueSeverity::Warning,
                        code: IssueType::BusinessRule,
                        diagnostics: Some(format!(
                            "Profile '{}': must-support element '{}' is not present",
                            profile_url, path
                        )),
                        details: None,
                        expression: Some(vec![path.to_string()]),
                    });
                }
            }
        }
    }

    /// Count occurrences of an element at a given relative path in the resource.
    ///
    /// Handles simple paths like "status", "code", "subject" as well as
    /// dotted paths like "code.coding" by walking into nested objects.
    /// Arrays count as the number of items.
    fn count_element(resource: &Value, relative_path: &str) -> u64 {
        let parts: Vec<&str> = relative_path.split('.').collect();
        Self::count_at_path(resource, &parts)
    }

    fn count_at_path(value: &Value, parts: &[&str]) -> u64 {
        if parts.is_empty() {
            return match value {
                Value::Array(arr) => arr.len() as u64,
                Value::Null => 0,
                _ => 1,
            };
        }

        let field = parts[0];
        let remaining = &parts[1..];

        match value.get(field) {
            None => 0,
            Some(child) => {
                if remaining.is_empty() {
                    // Final segment: count this element
                    match child {
                        Value::Array(arr) => arr.len() as u64,
                        Value::Null => 0,
                        _ => 1,
                    }
                } else {
                    // Intermediate segment: recurse
                    match child {
                        Value::Array(arr) => {
                            // Sum counts across all array items
                            arr.iter().map(|item| Self::count_at_path(item, remaining)).sum()
                        }
                        Value::Object(_) => Self::count_at_path(child, remaining),
                        _ => 0,
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_valid_extension() {
        let resource = json!({
            "resourceType": "Patient",
            "extension": [{
                "url": "http://example.com/extension",
                "valueString": "test"
            }]
        });

        let registry = ProfileRegistry::new();
        assert!(Phase2Validator::validate(&resource, &registry).is_ok());
    }

    #[test]
    fn test_extension_without_url() {
        let resource = json!({
            "resourceType": "Patient",
            "extension": [{
                "valueString": "test"
            }]
        });

        let registry = ProfileRegistry::new();
        assert!(Phase2Validator::validate(&resource, &registry).is_err());
    }

    #[test]
    fn test_extension_without_value() {
        let resource = json!({
            "resourceType": "Patient",
            "extension": [{
                "url": "http://example.com/extension"
            }]
        });

        let registry = ProfileRegistry::new();
        assert!(Phase2Validator::validate(&resource, &registry).is_err());
    }

    #[test]
    fn test_profile_not_found_returns_warning() {
        let resource = json!({
            "resourceType": "Observation",
            "meta": {
                "profile": ["http://example.com/StructureDefinition/Unknown"]
            },
            "status": "final",
            "code": {"coding": [{"system": "http://loinc.org", "code": "12345-6"}]}
        });

        let registry = ProfileRegistry::new();
        let result = Phase2Validator::validate(&resource, &registry);
        assert!(result.is_ok());
        let warnings = result.unwrap();
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].severity, IssueSeverity::Warning);
    }

    #[test]
    fn test_profile_required_element_missing() {
        let resource = json!({
            "resourceType": "Observation",
            "meta": {
                "profile": ["http://example.com/StructureDefinition/TestProfile"]
            },
            "status": "final",
            "code": {"coding": [{"system": "http://loinc.org", "code": "12345-6"}]}
        });

        let mut registry = ProfileRegistry::new();
        registry.add_profile(json!({
            "resourceType": "StructureDefinition",
            "url": "http://example.com/StructureDefinition/TestProfile",
            "snapshot": {
                "element": [
                    {"path": "Observation", "min": 0, "max": "*"},
                    {"path": "Observation.status", "min": 1, "max": "1"},
                    {"path": "Observation.code", "min": 1, "max": "1"},
                    {"path": "Observation.subject", "min": 1, "max": "1"},
                    {"path": "Observation.valueQuantity", "min": 1, "max": "1"}
                ]
            }
        }));

        let result = Phase2Validator::validate(&resource, &registry);
        assert!(result.is_err());
        let outcome = result.unwrap_err();
        // Should have errors for missing subject and valueQuantity
        let errors: Vec<_> = outcome
            .issue
            .iter()
            .filter(|i| i.severity == IssueSeverity::Error)
            .collect();
        assert_eq!(errors.len(), 2);
    }

    #[test]
    fn test_profile_max_cardinality_exceeded() {
        let resource = json!({
            "resourceType": "Observation",
            "meta": {
                "profile": ["http://example.com/StructureDefinition/TestProfile"]
            },
            "status": "final",
            "code": {"coding": [{"system": "http://loinc.org", "code": "12345-6"}]},
            "category": [
                {"coding": [{"code": "a"}]},
                {"coding": [{"code": "b"}]},
                {"coding": [{"code": "c"}]}
            ]
        });

        let mut registry = ProfileRegistry::new();
        registry.add_profile(json!({
            "resourceType": "StructureDefinition",
            "url": "http://example.com/StructureDefinition/TestProfile",
            "snapshot": {
                "element": [
                    {"path": "Observation", "min": 0, "max": "*"},
                    {"path": "Observation.status", "min": 1, "max": "1"},
                    {"path": "Observation.code", "min": 1, "max": "1"},
                    {"path": "Observation.category", "min": 0, "max": "2"}
                ]
            }
        }));

        let result = Phase2Validator::validate(&resource, &registry);
        assert!(result.is_err());
        let outcome = result.unwrap_err();
        let cardinality_errors: Vec<_> = outcome
            .issue
            .iter()
            .filter(|i| {
                i.diagnostics
                    .as_ref()
                    .map(|d| d.contains("max cardinality"))
                    .unwrap_or(false)
            })
            .collect();
        assert_eq!(cardinality_errors.len(), 1);
    }

    #[test]
    fn test_profile_must_support_warning() {
        let resource = json!({
            "resourceType": "Observation",
            "meta": {
                "profile": ["http://example.com/StructureDefinition/TestProfile"]
            },
            "status": "final",
            "code": {"coding": [{"system": "http://loinc.org", "code": "12345-6"}]}
        });

        let mut registry = ProfileRegistry::new();
        registry.add_profile(json!({
            "resourceType": "StructureDefinition",
            "url": "http://example.com/StructureDefinition/TestProfile",
            "snapshot": {
                "element": [
                    {"path": "Observation", "min": 0, "max": "*"},
                    {"path": "Observation.status", "min": 1, "max": "1"},
                    {"path": "Observation.code", "min": 1, "max": "1"},
                    {"path": "Observation.category", "min": 0, "max": "*", "mustSupport": true}
                ]
            }
        }));

        let result = Phase2Validator::validate(&resource, &registry);
        // Should be Ok since mustSupport is only a warning
        assert!(result.is_ok());
        let warnings = result.unwrap();
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].severity, IssueSeverity::Warning);
        assert!(warnings[0]
            .diagnostics
            .as_ref()
            .unwrap()
            .contains("must-support"));
    }

    #[test]
    fn test_profile_all_required_present() {
        let resource = json!({
            "resourceType": "Observation",
            "meta": {
                "profile": ["http://example.com/StructureDefinition/TestProfile"]
            },
            "status": "final",
            "code": {"coding": [{"system": "http://loinc.org", "code": "12345-6"}]},
            "subject": {"reference": "Patient/123"}
        });

        let mut registry = ProfileRegistry::new();
        registry.add_profile(json!({
            "resourceType": "StructureDefinition",
            "url": "http://example.com/StructureDefinition/TestProfile",
            "snapshot": {
                "element": [
                    {"path": "Observation", "min": 0, "max": "*"},
                    {"path": "Observation.status", "min": 1, "max": "1"},
                    {"path": "Observation.code", "min": 1, "max": "1"},
                    {"path": "Observation.subject", "min": 1, "max": "1"}
                ]
            }
        }));

        let result = Phase2Validator::validate(&resource, &registry);
        assert!(result.is_ok());
        assert!(result.unwrap().is_empty());
    }

    #[test]
    fn test_count_nested_path() {
        let resource = json!({
            "code": {
                "coding": [
                    {"system": "http://loinc.org", "code": "12345-6"}
                ]
            }
        });

        assert_eq!(Phase2Validator::count_element(&resource, "code.coding"), 1);
        assert_eq!(Phase2Validator::count_element(&resource, "code"), 1);
        assert_eq!(Phase2Validator::count_element(&resource, "subject"), 0);
    }
}
