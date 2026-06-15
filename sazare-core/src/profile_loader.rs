use serde_json::Value;
use std::path::Path;

// `US_CORE_PROFILE_JSON: &[&str]` — every file under profiles/us-core/, embedded
// by build.rs so the set can't drift from the directory.
include!(concat!(env!("OUT_DIR"), "/us_core_embedded.rs"));

/// Profile loader for StructureDefinition resources
pub struct ProfileLoader;

impl ProfileLoader {
    /// Load StructureDefinitions from a directory
    pub fn load_from_directory(dir_path: impl AsRef<Path>) -> Result<Vec<Value>, String> {
        Self::load_resources_from_directory(dir_path, "StructureDefinition")
    }

    /// Load every JSON resource of the given `resourceType` from a directory.
    /// Used for runtime-supplied conformance content (StructureDefinitions in
    /// `profiles/`, SearchParameters in `searchparameters/`).
    pub fn load_resources_from_directory(
        dir_path: impl AsRef<Path>,
        resource_type: &str,
    ) -> Result<Vec<Value>, String> {
        let mut resources = Vec::new();
        let dir_path = dir_path.as_ref();

        if !dir_path.exists() {
            return Ok(resources);
        }

        let entries = std::fs::read_dir(dir_path)
            .map_err(|e| format!("Failed to read directory: {}", e))?;

        for entry in entries {
            let entry = entry.map_err(|e| format!("Failed to read entry: {}", e))?;
            let path = entry.path();

            if path.extension().and_then(|s| s.to_str()) == Some("json") {
                match std::fs::read_to_string(&path) {
                    Ok(content) => match serde_json::from_str::<Value>(&content) {
                        Ok(resource) => {
                            if resource.get("resourceType").and_then(|v| v.as_str())
                                == Some(resource_type)
                            {
                                resources.push(resource);
                            }
                        }
                        Err(e) => {
                            tracing::warn!("Failed to parse {:?}: {}", path, e);
                        }
                    },
                    Err(e) => {
                        tracing::warn!("Failed to read file {:?}: {}", path, e);
                    }
                }
            }
        }

        tracing::info!(
            "Loaded {} {} resource(s) from {:?}",
            resources.len(),
            resource_type,
            dir_path
        );
        Ok(resources)
    }

    /// Every embedded US Core StructureDefinition — the full US Core 8.0.0 set,
    /// generated from `profiles/us-core/` by build.rs (no download required).
    pub fn get_embedded_us_core_profiles() -> Vec<Value> {
        US_CORE_PROFILE_JSON
            .iter()
            .filter_map(|json| match serde_json::from_str::<Value>(json) {
                Ok(profile) => Some(profile),
                Err(e) => {
                    tracing::error!("Failed to parse embedded US Core profile: {}", e);
                    None
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    /// Guards against the embedded US Core set silently shrinking or going stale
    /// (it was once 29 of 54 profiles, at v5.0.1). build.rs embeds the whole
    /// directory, so this asserts the directory holds the complete US Core 8.0.0
    /// set, including the granular vital-signs and USCDI Observations that were
    /// previously missing.
    #[test]
    fn embedded_us_core_is_complete_and_v8() {
        let profiles = ProfileLoader::get_embedded_us_core_profiles();
        assert_eq!(
            profiles.len(),
            54,
            "expected the full US Core 8.0.0 profile set (54), got {}",
            profiles.len()
        );
        for p in &profiles {
            let v = p.get("version").and_then(|v| v.as_str()).unwrap_or("");
            assert!(
                v.starts_with("8."),
                "non-v8 profile {:?}: version {v}",
                p.get("url")
            );
        }
        let urls: std::collections::HashSet<&str> = profiles
            .iter()
            .filter_map(|p| p.get("url").and_then(|u| u.as_str()))
            .collect();
        // Profiles that were absent before the full embed.
        for must in [
            "http://hl7.org/fhir/us/core/StructureDefinition/us-core-blood-pressure",
            "http://hl7.org/fhir/us/core/StructureDefinition/us-core-body-weight",
            "http://hl7.org/fhir/us/core/StructureDefinition/us-core-heart-rate",
            "http://hl7.org/fhir/us/core/StructureDefinition/us-core-observation-pregnancystatus",
            "http://hl7.org/fhir/us/core/StructureDefinition/us-core-observation-occupation",
            "http://hl7.org/fhir/us/core/StructureDefinition/us-core-average-blood-pressure",
        ] {
            assert!(urls.contains(must), "missing US Core profile: {must}");
        }
    }

    #[test]
    fn test_load_from_empty_directory() {
        let temp_dir = TempDir::new().unwrap();
        let profiles = ProfileLoader::load_from_directory(temp_dir.path()).unwrap();
        assert_eq!(profiles.len(), 0);
    }

    #[test]
    fn test_load_from_directory_with_profiles() {
        let temp_dir = TempDir::new().unwrap();

        let profile = serde_json::json!({
            "resourceType": "StructureDefinition",
            "url": "http://example.com/StructureDefinition/TestProfile",
            "name": "TestProfile",
            "status": "active"
        });

        let file_path = temp_dir.path().join("test-profile.json");
        fs::write(&file_path, serde_json::to_string_pretty(&profile).unwrap()).unwrap();

        let profiles = ProfileLoader::load_from_directory(temp_dir.path()).unwrap();

        assert_eq!(profiles.len(), 1);
        assert_eq!(
            profiles[0].get("resourceType").and_then(|v| v.as_str()),
            Some("StructureDefinition")
        );
    }

    #[test]
    fn test_load_nonexistent_directory() {
        let profiles = ProfileLoader::load_from_directory("/nonexistent/path").unwrap();
        assert_eq!(profiles.len(), 0);
    }
}
