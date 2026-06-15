use serde_json::Value;
use std::path::Path;

/// Profile loader for StructureDefinition resources
pub struct ProfileLoader;

impl ProfileLoader {
    /// Load StructureDefinitions from a directory
    pub fn load_from_directory(dir_path: impl AsRef<Path>) -> Result<Vec<Value>, String> {
        let mut profiles = Vec::new();
        let dir_path = dir_path.as_ref();

        if !dir_path.exists() {
            return Ok(profiles);
        }

        let entries = std::fs::read_dir(dir_path)
            .map_err(|e| format!("Failed to read directory: {}", e))?;

        for entry in entries {
            let entry = entry.map_err(|e| format!("Failed to read entry: {}", e))?;
            let path = entry.path();

            if path.extension().and_then(|s| s.to_str()) == Some("json") {
                match std::fs::read_to_string(&path) {
                    Ok(content) => {
                        match serde_json::from_str::<Value>(&content) {
                            Ok(profile) => {
                                // Verify it's a StructureDefinition
                                if profile.get("resourceType").and_then(|v| v.as_str())
                                    == Some("StructureDefinition")
                                {
                                    profiles.push(profile);
                                }
                            }
                            Err(e) => {
                                tracing::warn!("Failed to parse profile {:?}: {}", path, e);
                            }
                        }
                    }
                    Err(e) => {
                        tracing::warn!("Failed to read file {:?}: {}", path, e);
                    }
                }
            }
        }

        tracing::info!("Loaded {} profiles from {:?}", profiles.len(), dir_path);
        Ok(profiles)
    }

    /// Get embedded US-Core profiles (no download required)
    pub fn get_embedded_us_core_profiles() -> Vec<Value> {
        tracing::info!("Loading embedded US-Core profiles...");

        // Embedded StructureDefinition JSON files (all 29 US-Core v5.0.1 profiles)
        const US_CORE_PATIENT: &str = include_str!("../profiles/us-core/us-core-patient.json");
        const US_CORE_PRACTITIONER: &str = include_str!("../profiles/us-core/us-core-practitioner.json");
        const US_CORE_PRACTITIONERROLE: &str = include_str!("../profiles/us-core/us-core-practitionerrole.json");
        const US_CORE_ORGANIZATION: &str = include_str!("../profiles/us-core/us-core-organization.json");
        const US_CORE_LOCATION: &str = include_str!("../profiles/us-core/us-core-location.json");
        const US_CORE_RELATEDPERSON: &str = include_str!("../profiles/us-core/us-core-relatedperson.json");
        const US_CORE_CONDITION: &str = include_str!("../profiles/us-core/us-core-condition.json");
        const US_CORE_PROCEDURE: &str = include_str!("../profiles/us-core/us-core-procedure.json");
        const US_CORE_ENCOUNTER: &str = include_str!("../profiles/us-core/us-core-encounter.json");
        const US_CORE_ALLERGYINTOLERANCE: &str = include_str!("../profiles/us-core/us-core-allergyintolerance.json");
        const US_CORE_IMMUNIZATION: &str = include_str!("../profiles/us-core/us-core-immunization.json");
        const US_CORE_CAREPLAN: &str = include_str!("../profiles/us-core/us-core-careplan.json");
        const US_CORE_CARETEAM: &str = include_str!("../profiles/us-core/us-core-careteam.json");
        const US_CORE_GOAL: &str = include_str!("../profiles/us-core/us-core-goal.json");
        const US_CORE_OBSERVATION_LAB: &str = include_str!("../profiles/us-core/us-core-observation-lab.json");
        const US_CORE_VITAL_SIGNS: &str = include_str!("../profiles/us-core/us-core-vital-signs.json");
        const US_CORE_SMOKINGSTATUS: &str = include_str!("../profiles/us-core/us-core-smokingstatus.json");
        const PEDIATRIC_BMI_FOR_AGE: &str = include_str!("../profiles/us-core/pediatric-bmi-for-age.json");
        const PEDIATRIC_WEIGHT_FOR_HEIGHT: &str = include_str!("../profiles/us-core/pediatric-weight-for-height.json");
        const US_CORE_PULSE_OXIMETRY: &str = include_str!("../profiles/us-core/us-core-pulse-oximetry.json");
        const US_CORE_DIAGNOSTICREPORT_LAB: &str = include_str!("../profiles/us-core/us-core-diagnosticreport-lab.json");
        const US_CORE_DIAGNOSTICREPORT_NOTE: &str = include_str!("../profiles/us-core/us-core-diagnosticreport-note.json");
        const US_CORE_DOCUMENTREFERENCE: &str = include_str!("../profiles/us-core/us-core-documentreference.json");
        const US_CORE_MEDICATION: &str = include_str!("../profiles/us-core/us-core-medication.json");
        const US_CORE_MEDICATIONREQUEST: &str = include_str!("../profiles/us-core/us-core-medicationrequest.json");
        const US_CORE_PROVENANCE: &str = include_str!("../profiles/us-core/us-core-provenance.json");
        const US_CORE_SERVICEREQUEST: &str = include_str!("../profiles/us-core/us-core-servicerequest.json");
        const US_CORE_COVERAGE: &str = include_str!("../profiles/us-core/us-core-coverage.json");
        const US_CORE_QUESTIONNAIRERESPONSE: &str = include_str!("../profiles/us-core/us-core-questionnaireresponse.json");

        let mut profiles = Vec::new();

        let embedded_jsons = vec![
            US_CORE_PATIENT,
            US_CORE_PRACTITIONER,
            US_CORE_PRACTITIONERROLE,
            US_CORE_ORGANIZATION,
            US_CORE_LOCATION,
            US_CORE_RELATEDPERSON,
            US_CORE_CONDITION,
            US_CORE_PROCEDURE,
            US_CORE_ENCOUNTER,
            US_CORE_ALLERGYINTOLERANCE,
            US_CORE_IMMUNIZATION,
            US_CORE_CAREPLAN,
            US_CORE_CARETEAM,
            US_CORE_GOAL,
            US_CORE_OBSERVATION_LAB,
            US_CORE_VITAL_SIGNS,
            US_CORE_SMOKINGSTATUS,
            PEDIATRIC_BMI_FOR_AGE,
            PEDIATRIC_WEIGHT_FOR_HEIGHT,
            US_CORE_PULSE_OXIMETRY,
            US_CORE_DIAGNOSTICREPORT_LAB,
            US_CORE_DIAGNOSTICREPORT_NOTE,
            US_CORE_DOCUMENTREFERENCE,
            US_CORE_MEDICATION,
            US_CORE_MEDICATIONREQUEST,
            US_CORE_PROVENANCE,
            US_CORE_SERVICEREQUEST,
            US_CORE_COVERAGE,
            US_CORE_QUESTIONNAIRERESPONSE,
        ];

        for json_str in embedded_jsons {
            match serde_json::from_str::<Value>(json_str) {
                Ok(profile) => profiles.push(profile),
                Err(e) => {
                    tracing::error!("Failed to parse embedded profile: {}", e);
                }
            }
        }

        tracing::info!("Loaded {} embedded US-Core profiles", profiles.len());
        profiles
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

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
