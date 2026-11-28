use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Core FHIR resource structure
#[derive(Debug, Clone, Serialize)]
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
#[derive(Debug, Clone, Serialize, Default)]
pub struct Meta {
    #[serde(rename = "versionId", skip_serializing_if = "Option::is_none")]
    pub version_id: Option<String>,

    #[serde(rename = "lastUpdated", skip_serializing_if = "Option::is_none")]
    pub last_updated: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile: Option<Vec<String>>,

    /// Every other meta element (`tag`, `security`, `extension`, ...), kept
    /// as sent — dropping them lost `_tag` / `_security` searchability.
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

// Read through a `Value`, not serde's derived `flatten`: that buffers the
// other elements in serde's own content type, which can't hold a number
// beyond 64 bits — a decimal like 123456789012345678901234567890 (valid in
// R4, kept as written with `arbitrary_precision`) was refused.
impl<'de> Deserialize<'de> for Resource {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Resource::from_value(Value::deserialize(d)?).map_err(serde::de::Error::custom)
    }
}

impl<'de> Deserialize<'de> for Meta {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Meta::from_value(Value::deserialize(d)?).map_err(serde::de::Error::custom)
    }
}

impl Meta {
    /// Meta from a parsed `Value`, split by hand (see [`Resource::from_value`]).
    pub fn from_value(value: Value) -> Result<Self, String> {
        let Value::Object(mut map) = value else {
            return Err("meta is a JSON object".into());
        };
        fn string(v: Option<Value>, name: &str) -> Result<Option<String>, String> {
            match v {
                None | Some(Value::Null) => Ok(None),
                Some(Value::String(s)) => Ok(Some(s)),
                Some(_) => Err(format!("meta.{name} must be a string")),
            }
        }
        let version_id = string(map.remove("versionId"), "versionId")?;
        let last_updated = string(map.remove("lastUpdated"), "lastUpdated")?;
        let source = string(map.remove("source"), "source")?;
        let profile = match map.remove("profile") {
            None | Some(Value::Null) => None,
            Some(Value::Array(items)) => Some(
                items
                    .into_iter()
                    .map(|v| match v {
                        Value::String(s) => Ok(s),
                        _ => Err("meta.profile holds strings".to_string()),
                    })
                    .collect::<Result<Vec<_>, _>>()?,
            ),
            Some(_) => return Err("meta.profile is a list".into()),
        };
        Ok(Meta { version_id, last_updated, source, profile, extra: map })
    }
}

impl Resource {
    /// A resource from a parsed `Value`, split by hand. `serde_json::from_value`
    /// would re-read every number through its deserializer, which with
    /// `arbitrary_precision` turns some into floats (`0.000000000000000000001`
    /// came back as `1e-21`); this keeps each `Value` as it is.
    pub fn from_value(value: Value) -> Result<Self, String> {
        let Value::Object(mut map) = value else {
            return Err("a resource is a JSON object".into());
        };
        let resource_type = match map.remove("resourceType") {
            Some(Value::String(t)) => t,
            Some(_) => return Err("resourceType must be a string".into()),
            None => return Err("missing field `resourceType`".into()),
        };
        let id = match map.remove("id") {
            None | Some(Value::Null) => None,
            Some(Value::String(i)) => Some(i),
            Some(_) => return Err("id must be a string".into()),
        };
        let meta = match map.remove("meta") {
            None | Some(Value::Null) => None,
            Some(m) => Some(Meta::from_value(m)?),
        };
        Ok(Resource { resource_type, id, meta, rest: Value::Object(map) })
    }

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

