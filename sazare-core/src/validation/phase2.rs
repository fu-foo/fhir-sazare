use crate::operation_outcome::OperationOutcome;
use crate::validation::registry::ProfileRegistry;
use serde_json::Value;

/// Phase 2: Extension validation
pub struct Phase2Validator;

impl Phase2Validator {
    /// Validate extensions against profiles
    pub fn validate(
        resource: &Value,
        _registry: &ProfileRegistry,
    ) -> Result<(), OperationOutcome> {
        // Check if resource declares profiles
        if let Some(profiles) = resource
            .get("meta")
            .and_then(|m| m.get("profile"))
            .and_then(|p| p.as_array())
        {
            for _profile_url in profiles {
                // In a full implementation, we would:
                // 1. Load the profile from the registry
                // 2. Check that all extensions are declared in the profile
                // 3. Validate extension cardinality
                // 4. Validate extension types

                // For now, we accept all extensions
            }
        }

        // Check extension structure if present
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
}
