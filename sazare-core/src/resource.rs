use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Core FHIR resource structure
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Resource {
    #[serde(rename = "resourceType")]
    pub resource_type: String,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub meta: Option<Meta>,

    /// All other fields are stored here
    #[serde(flatten)]
    pub rest: Value,
}

/// FHIR resource metadata
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Meta {
    #[serde(rename = "versionId", skip_serializing_if = "Option::is_none")]
    pub version_id: Option<String>,

    #[serde(rename = "lastUpdated", skip_serializing_if = "Option::is_none")]
    pub last_updated: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile: Option<Vec<String>>,
}

impl Resource {
    /// Create a new resource
    pub fn new(resource_type: impl Into<String>) -> Self {
        Self {
            resource_type: resource_type.into(),
            id: None,
            meta: None,
            rest: Value::Object(serde_json::Map::new()),
        }
    }

    /// Parse a resource from JSON
    pub fn from_json(json: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(json)
    }

    /// Convert the resource to JSON
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }

    /// Convert the resource to pretty-printed JSON
    pub fn to_json_pretty(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_patient() {
        let json = r#"{
            "resourceType": "Patient",
            "id": "123",
            "meta": {
                "versionId": "1",
                "lastUpdated": "2024-01-01T00:00:00Z"
            },
            "name": [{"family": "Doe", "given": ["Jane"]}]
        }"#;

        let resource = Resource::from_json(json).unwrap();
        assert_eq!(resource.resource_type, "Patient");
        assert_eq!(resource.id, Some("123".to_string()));
        assert!(resource.meta.is_some());
    }

    #[test]
    fn test_roundtrip() {
        let json = r#"{"resourceType":"Patient","id":"456"}"#;
        let resource = Resource::from_json(json).unwrap();
        let output = resource.to_json().unwrap();
        
        // Re-parse and compare
        let reparsed = Resource::from_json(&output).unwrap();
        assert_eq!(reparsed.resource_type, "Patient");
        assert_eq!(reparsed.id, Some("456".to_string()));
    }
}
