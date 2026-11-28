//! References in a resource.

use serde_json::Value;

/// The conditional references in `resource` (`Patient?identifier=urn:mrn|42`,
/// R4 http.html#trules), each once: a reference whose text is an R4 type, a
/// `?` and a query. Stored as written, nothing could follow them.
pub fn conditional_references(resource: &Value) -> Vec<String> {
    fn collect(v: &Value, out: &mut Vec<String>) {
        match v {
            Value::Object(map) => {
                if let Some(r) = map.get("reference").and_then(|r| r.as_str())
                    && let Some((t, q)) = r.split_once('?')
                    && !q.is_empty()
                    && crate::r4_types::is_resource_type(t)
                    && !out.iter().any(|o| o == r)
                {
                    out.push(r.to_string());
                }
                map.values().for_each(|v| collect(v, out));
            }
            Value::Array(a) => a.iter().for_each(|v| collect(v, out)),
            _ => {}
        }
    }
    let mut out = Vec::new();
    collect(resource, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    #[test]
    fn finds_each_conditional_reference_once() {
        let r = json!({"resourceType": "Observation",
            "subject": {"reference": "Patient?identifier=urn:mrn|1"},
            "performer": [{"reference": "Patient?identifier=urn:mrn|1"}, {"reference": "Practitioner/p"},
                          {"reference": "Foo?x=1"}, {"reference": "Patient?"}]});
        assert_eq!(super::conditional_references(&r), ["Patient?identifier=urn:mrn|1"]);
    }
}
