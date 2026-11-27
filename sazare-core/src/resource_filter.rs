//! Resource field filtering for _summary and _elements parameters

use crate::search_param::SummaryMode;
use serde_json::Value;

/// Fields that are always retained regardless of filtering
const ALWAYS_KEEP: &[&str] = &["resourceType", "id", "meta"];

/// The summary elements of each R4 resource type (`isSummary`, from the R4
/// StructureDefinitions: `summary-elements/r4.tsv`, `generate.py` next to it).
static SUMMARY: std::sync::LazyLock<std::collections::HashMap<&'static str, Vec<&'static str>>> =
    std::sync::LazyLock::new(|| {
        include_str!("../summary-elements/r4.tsv")
            .lines()
            .filter_map(|line| {
                let mut cols = line.split('\t');
                Some((cols.next()?, cols.collect()))
            })
            .collect()
    });

/// Summary fields of a resource type; a generic set for a type R4 doesn't
/// define.
fn summary_fields(resource_type: &str) -> &'static [&'static str] {
    match SUMMARY.get(resource_type) {
        Some(fields) => fields.as_slice(),
        None => &["identifier", "status", "code", "name", "subject", "date", "type"],
    }
}

/// Mark a resource returned without some of its elements (FHIR: the
/// `SUBSETTED` tag), so it isn't taken for — or saved back as — the whole.
fn mark_subsetted(resource: &mut Value) {
    let Some(obj) = resource.as_object_mut() else { return };
    let meta = obj.entry("meta").or_insert_with(|| Value::Object(Default::default()));
    let Some(meta) = meta.as_object_mut() else { return };
    let tags = meta.entry("tag").or_insert_with(|| Value::Array(vec![]));
    if let Some(tags) = tags.as_array_mut()
        && !tags.iter().any(|t| t.get("code").and_then(|c| c.as_str()) == Some("SUBSETTED"))
    {
        tags.push(serde_json::json!({
            "system": "http://terminology.hl7.org/CodeSystem/v3-ObservationValue",
            "code": "SUBSETTED"
        }));
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
            mark_subsetted(resource);
        }
        SummaryMode::Data => {
            remove_field(resource, "text");
            mark_subsetted(resource);
        }
        SummaryMode::True => {
            let rt = resource.get("resourceType")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let fields = summary_fields(rt);
            retain_fields(resource, fields);
            mark_subsetted(resource);
        }
    }
}

/// Apply _elements filtering to a resource.
/// Keeps only the specified fields (plus resourceType, id, meta).
pub fn apply_elements(resource: &mut Value, elements: &[String]) {
    let element_strs: Vec<&str> = elements.iter().map(|s| s.as_str()).collect();
    retain_fields(resource, &element_strs);
    mark_subsetted(resource);
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

    #[test]
    fn summary_follows_the_r4_definitions_and_is_tagged() {
        // DiagnosticReport had no list of its own: a guess kept `code`,
        // `subject`... and dropped `issued` and `performer`.
        let mut r = serde_json::json!({
            "resourceType": "DiagnosticReport", "id": "d", "status": "final",
            "code": {"text": "x"}, "issued": "2024-01-01T00:00:00Z",
            "performer": [{"reference": "Practitioner/1"}], "conclusion": "long text"
        });
        apply_summary(&mut r, &SummaryMode::True);
        assert!(r.get("issued").is_some() && r.get("performer").is_some());
        assert!(r.get("conclusion").is_none(), "not a summary element");
        assert_eq!(r["meta"]["tag"][0]["code"], "SUBSETTED");
        // A choice element is kept under its typed name.
        let mut o = serde_json::json!({"resourceType": "Observation", "status": "final", "valueQuantity": {"value": 1}, "note": [{"text": "n"}]});
        apply_summary(&mut o, &SummaryMode::True);
        assert!(o.get("valueQuantity").is_some() && o.get("note").is_none());
        // _elements tags too, once.
        let mut p = serde_json::json!({"resourceType": "Patient", "name": [], "gender": "male"});
        apply_elements(&mut p, &["name".to_string()]);
        apply_elements(&mut p, &["name".to_string()]);
        assert_eq!(p["meta"]["tag"].as_array().unwrap().len(), 1);
        // _summary=false is the whole resource: no tag.
        let mut f = serde_json::json!({"resourceType": "Patient"});
        apply_summary(&mut f, &SummaryMode::False);
        assert!(f.get("meta").is_none());
    }
}
