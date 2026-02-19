//! Resource field filtering for _summary and _elements parameters

use crate::search_param::SummaryMode;
use serde_json::Value;

/// Fields that are always retained regardless of filtering
const ALWAYS_KEEP: &[&str] = &["resourceType", "id", "meta"];

/// Summary fields by resource type (isSummary=true in StructureDefinition).
/// For unknown types, fall back to a generic set.
fn summary_fields(resource_type: &str) -> &'static [&'static str] {
    match resource_type {
        "Patient" => &[
            "identifier", "active", "name", "telecom", "gender",
            "birthDate", "deceased", "deceasedBoolean", "deceasedDateTime",
            "address", "managingOrganization", "link",
        ],
        "Observation" => &[
            "identifier", "status", "category", "code", "subject",
            "encounter", "effective", "effectiveDateTime", "effectivePeriod",
            "issued", "value", "valueQuantity", "valueCodeableConcept",
            "valueString", "dataAbsentReason", "interpretation",
            "hasMember",
        ],
        "Encounter" => &[
            "identifier", "status", "class", "type", "subject",
            "participant", "period", "location",
        ],
        "Condition" => &[
            "identifier", "clinicalStatus", "verificationStatus",
            "category", "severity", "code", "subject",
            "encounter", "onset", "onsetDateTime", "abatement",
            "recordedDate",
        ],
        _ => &[
            "identifier", "status", "code", "name", "subject",
            "date", "type",
        ],
    }
}

/// Apply _summary filtering to a resource.
pub fn apply_summary(resource: &mut Value, mode: &SummaryMode) {
    match mode {
        SummaryMode::False => { /* return everything */ }
        SummaryMode::Count => {
            // For _summary=count, resources are not included in the Bundle at all.
            // This is handled at the handler level, not here.
        }
        SummaryMode::Text => {
            retain_fields(resource, &["text"]);
        }
        SummaryMode::Data => {
            remove_field(resource, "text");
        }
        SummaryMode::True => {
            let rt = resource.get("resourceType")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let fields = summary_fields(rt);
            retain_fields(resource, fields);
        }
    }
}

/// Apply _elements filtering to a resource.
/// Keeps only the specified fields (plus resourceType, id, meta).
pub fn apply_elements(resource: &mut Value, elements: &[String]) {
    let element_strs: Vec<&str> = elements.iter().map(|s| s.as_str()).collect();
    retain_fields(resource, &element_strs);
}

/// Keep only the specified fields plus ALWAYS_KEEP fields.
fn retain_fields(resource: &mut Value, fields: &[&str]) {
    if let Some(obj) = resource.as_object_mut() {
        obj.retain(|key, _| {
            ALWAYS_KEEP.contains(&key.as_str()) || fields.contains(&key.as_str())
        });
    }
}

/// Remove a single field from the resource.
fn remove_field(resource: &mut Value, field: &str) {
    if let Some(obj) = resource.as_object_mut() {
        obj.remove(field);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_apply_elements() {
        let mut resource = json!({
            "resourceType": "Patient",
            "id": "123",
            "meta": {"versionId": "1"},
            "name": [{"family": "Doe"}],
            "gender": "male",
            "birthDate": "1990-01-01",
            "address": [{"city": "Springfield"}]
        });

        apply_elements(&mut resource, &["name".to_string(), "gender".to_string()]);

        assert_eq!(resource.get("resourceType").unwrap(), "Patient");
        assert_eq!(resource.get("id").unwrap(), "123");
        assert!(resource.get("meta").is_some());
        assert!(resource.get("name").is_some());
        assert!(resource.get("gender").is_some());
        assert!(resource.get("birthDate").is_none());
        assert!(resource.get("address").is_none());
    }

    #[test]
    fn test_apply_summary_true() {
        let mut resource = json!({
            "resourceType": "Patient",
            "id": "123",
            "meta": {"versionId": "1"},
            "name": [{"family": "Doe"}],
            "gender": "male",
            "text": {"status": "generated", "div": "<div>...</div>"},
            "contact": [{"name": {"text": "Emergency"}}]
        });

        apply_summary(&mut resource, &SummaryMode::True);

        assert!(resource.get("name").is_some());
        assert!(resource.get("gender").is_some());
        assert!(resource.get("id").is_some());
        // text is NOT a summary field
        assert!(resource.get("text").is_none());
        // contact is NOT a summary field
        assert!(resource.get("contact").is_none());
    }

    #[test]
    fn test_apply_summary_text() {
        let mut resource = json!({
            "resourceType": "Patient",
            "id": "123",
            "meta": {"versionId": "1"},
            "name": [{"family": "Doe"}],
            "text": {"status": "generated", "div": "<div>...</div>"}
        });

        apply_summary(&mut resource, &SummaryMode::Text);

        assert!(resource.get("text").is_some());
        assert!(resource.get("id").is_some());
        assert!(resource.get("meta").is_some());
        assert!(resource.get("name").is_none());
    }

    #[test]
    fn test_apply_summary_data() {
        let mut resource = json!({
            "resourceType": "Patient",
            "id": "123",
            "meta": {"versionId": "1"},
            "name": [{"family": "Doe"}],
            "text": {"status": "generated", "div": "<div>...</div>"}
        });

        apply_summary(&mut resource, &SummaryMode::Data);

        assert!(resource.get("name").is_some());
        assert!(resource.get("text").is_none());
    }

    #[test]
    fn test_apply_summary_false() {
        let mut resource = json!({
            "resourceType": "Patient",
            "id": "123",
            "name": [{"family": "Doe"}],
            "text": {"div": "<div>...</div>"}
        });

        let original = resource.clone();
        apply_summary(&mut resource, &SummaryMode::False);

        assert_eq!(resource, original);
    }

    #[test]
    fn test_elements_always_keeps_required() {
        let mut resource = json!({
            "resourceType": "Patient",
            "id": "123",
            "meta": {"versionId": "1"},
            "name": [{"family": "Doe"}],
            "gender": "male"
        });

        // Even with empty elements, resourceType/id/meta are kept
        apply_elements(&mut resource, &["gender".to_string()]);

        assert!(resource.get("resourceType").is_some());
        assert!(resource.get("id").is_some());
        assert!(resource.get("meta").is_some());
        assert!(resource.get("gender").is_some());
        assert!(resource.get("name").is_none());
    }
}
