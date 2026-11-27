//! A resource's shape against the R4 definitions: every element is one the
//! resource type (or the data type it is in) defines, a list where the element
//! repeats and a single value where it doesn't, and each value of the JSON
//! type its FHIR type has. `structure/r4.tsv` (generated from the R4
//! StructureDefinitions, `generate.py` next to it) lists the elements.
//!
//! Not checked here: minimum cardinality, value formats beyond the JSON type,
//! invariants — profiles and the other phases do what is checked of those.

use std::collections::HashMap;
use std::sync::LazyLock;

use serde_json::{Map, Value};

use crate::operation_outcome::{IssueSeverity, IssueType, OperationOutcome, OperationOutcomeIssue};

/// An element: whether it repeats, and its types (or the element whose
/// content it has).
struct Element {
    repeats: bool,
    types: Vec<&'static str>,
    content_of: Option<&'static str>,
}

static ELEMENTS: LazyLock<HashMap<&'static str, Element>> = LazyLock::new(|| {
    include_str!("../../structure/r4.tsv")
        .lines()
        .filter_map(|line| {
            let mut cols = line.split('\t');
            let (path, max, types) = (cols.next()?, cols.next()?, cols.next().unwrap_or(""));
            let (types, content_of) = match types.strip_prefix('@') {
                Some(target) => (vec![], Some(target)),
                None => (types.split(',').filter(|t| !t.is_empty()).collect(), None),
            };
            Some((path, Element { repeats: max != "1" && max != "0", types, content_of }))
        })
        .collect()
});

/// Types whose definitions are in the table (complex data types and
/// resources), so their content can be checked.
static DEFINED: LazyLock<std::collections::HashSet<&'static str>> =
    LazyLock::new(|| ELEMENTS.keys().filter_map(|p| p.split('.').next()).collect());

/// Whether `path` (`Patient.name`, `Person.link.target`) is an R4 element.
pub fn has_element(path: &str) -> bool {
    ELEMENTS.contains_key(path)
}

/// At most this many problems are reported (the first ones).
const MAX_ISSUES: usize = 20;
/// Nesting deeper than this (contained in contained in …) isn't followed.
const MAX_DEPTH: usize = 32;

/// Check `resource`'s shape; `Err` lists what doesn't fit.
pub fn validate(resource: &Value) -> Result<(), OperationOutcome> {
    let Some(obj) = resource.as_object() else {
        return Err(OperationOutcome::error(IssueType::Structure, "A resource is a JSON object"));
    };
    let Some(rt) = obj.get("resourceType").and_then(|v| v.as_str()) else { return Ok(()) };
    if !DEFINED.contains(rt) {
        return Ok(());
    }
    let mut issues = Vec::new();
    check_object(obj, rt, rt, true, 0, &mut issues);
    if issues.is_empty() {
        return Ok(());
    }
    let mut outcome = OperationOutcome::error(IssueType::Structure, issues[0].1.clone());
    outcome.issue = issues
        .into_iter()
        .map(|(loc, msg)| OperationOutcomeIssue {
            severity: IssueSeverity::Error,
            code: IssueType::Structure,
            diagnostics: Some(msg),
            details: None,
            expression: Some(vec![loc]),
        })
        .collect();
    Err(outcome)
}

/// The children of `base` (a type name or an element path) in `obj`;
/// `loc` is where in the resource (for messages).
fn check_object(obj: &Map<String, Value>, base: &str, loc: &str, is_resource: bool, depth: usize, issues: &mut Vec<(String, String)>) {
    if depth > MAX_DEPTH {
        return;
    }
    for (key, value) in obj {
        if issues.len() >= MAX_ISSUES {
            return;
        }
        if is_resource && key == "resourceType" {
            continue;
        }
        let here = format!("{loc}.{key}");
        // `_name`: the id and extensions of the primitive `name`.
        if let Some(primitive) = key.strip_prefix('_') {
            if resolve(base, primitive).is_none() && !obj.contains_key(primitive) {
                issues.push((here.clone(), format!("Unknown element '{here}'")));
            }
            continue;
        }
        let Some((path, types, repeats)) = resolve(base, key) else {
            issues.push((here.clone(), format!("Unknown element '{here}'")));
            continue;
        };
        match (repeats, value) {
            (true, Value::Array(items)) => {
                if items.is_empty() {
                    issues.push((here.clone(), format!("'{here}' is an empty array (leave the element out)")));
                }
                for (i, item) in items.iter().enumerate() {
                    // A null holds the place of a value that only has `_key`.
                    if !item.is_null() {
                        check_value(item, &path, &types, &format!("{here}[{i}]"), depth, issues);
                    }
                }
            }
            (true, _) => issues.push((here.clone(), format!("'{here}' repeats: its value is a JSON array"))),
            (false, Value::Array(_)) => issues.push((here.clone(), format!("'{here}' has one value, not an array"))),
            (false, v) => check_value(v, &path, &types, &here, depth, issues),
        }
    }
}

/// The element `key` of `base`: its path (where its own children are, for a
/// backbone element), its type(s) and whether it repeats. A choice
/// (`value[x]`) resolves by the type in the name (`valueQuantity`).
fn resolve(base: &str, key: &str) -> Option<(String, Vec<&'static str>, bool)> {
    let path = format!("{base}.{key}");
    if let Some(el) = ELEMENTS.get(path.as_str()) {
        if let Some(target) = el.content_of {
            let t = ELEMENTS.get(target)?;
            return Some((target.to_string(), t.types.clone(), el.repeats));
        }
        return Some((path, el.types.clone(), el.repeats));
    }
    // A choice: `valueQuantity` for `value[x]`, `multipleBirthBoolean` for
    // `multipleBirth[x]` (the name may have capitals of its own).
    for (split, _) in key.char_indices().skip(1).filter(|(_, c)| c.is_ascii_uppercase()) {
        let (name, ty) = key.split_at(split);
        let Some(el) = ELEMENTS.get(format!("{base}.{name}[x]").as_str()) else { continue };
        let found = el.types.iter().find(|t| {
            let mut c = t.chars();
            c.next().is_some_and(|f| f.to_ascii_uppercase().to_string() + c.as_str() == ty)
        });
        if let Some(t) = found {
            return Some((format!("{base}.{name}[x]"), vec![*t], el.repeats));
        }
    }
    None
}

fn check_value(value: &Value, path: &str, types: &[&'static str], loc: &str, depth: usize, issues: &mut Vec<(String, String)>) {
    let [ty] = types else { return }; // no type, or a choice not resolved: not checked
    let want = match *ty {
        "boolean" => Some(("a boolean", value.is_boolean())),
        "integer" | "positiveInt" | "unsignedInt" => Some(("an integer", value.is_i64() || value.is_u64())),
        "decimal" => Some(("a number", value.is_number())),
        "string" | "code" | "id" | "uri" | "url" | "canonical" | "oid" | "uuid" | "markdown" | "base64Binary" | "date"
        | "dateTime" | "instant" | "time" | "xhtml" => Some(("a string", value.is_string())),
        _ => None,
    };
    if let Some((what, ok)) = want {
        if !ok {
            issues.push((loc.to_string(), format!("'{loc}' is a {ty}: {what}, not {}", kind(value))));
        }
        return;
    }
    let Some(obj) = value.as_object() else {
        issues.push((loc.to_string(), format!("'{loc}' is a {ty}: a JSON object, not {}", kind(value))));
        return;
    };
    match *ty {
        // Its children are defined under its own path.
        "BackboneElement" | "Element" => check_object(obj, path, loc, false, depth + 1, issues),
        // A resource inside (contained, Bundle.entry.resource, …).
        "Resource" => match obj.get("resourceType").and_then(|v| v.as_str()) {
            Some(rt) if crate::r4_types::is_resource_type(rt) => {
                if DEFINED.contains(rt) {
                    check_object(obj, rt, loc, true, depth + 1, issues);
                }
            }
            Some(rt) => issues.push((loc.to_string(), format!("'{loc}': '{rt}' is not a FHIR R4 resource type"))),
            None => issues.push((loc.to_string(), format!("'{loc}' is a resource: it needs a resourceType"))),
        },
        t if DEFINED.contains(t) => check_object(obj, t, loc, false, depth + 1, issues),
        _ => {}
    }
}

fn kind(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "an array",
        Value::Object(_) => "an object",
    }
}

#[cfg(test)]
mod tests {
    use super::validate;
    use serde_json::json;

    fn problems(v: serde_json::Value) -> Vec<String> {
        match validate(&v) {
            Ok(()) => vec![],
            Err(o) => o.issue.into_iter().filter_map(|i| i.diagnostics).collect(),
        }
    }

    #[test]
    fn a_well_formed_resource_passes() {
        assert!(problems(json!({
            "resourceType": "Patient", "id": "p", "meta": {"versionId": "1", "profile": ["http://x"]},
            "text": {"status": "generated", "div": "<div xmlns=\"http://www.w3.org/1999/xhtml\">x</div>"},
            "name": [{"family": "Doe", "given": ["Jo", null], "_given": [null, {"extension": [{"url": "u", "valueString": "x"}]}]}],
            "gender": "female", "_gender": {"id": "g"}, "birthDate": "1990-01-01", "deceasedBoolean": false,
            "contact": [{"name": {"family": "Kin"}, "telecom": [{"system": "phone", "value": "1"}]}],
            "extension": [{"url": "http://e", "valueCodeableConcept": {"coding": [{"system": "s", "code": "c"}]}}],
            "contained": [{"resourceType": "Organization", "id": "o", "name": "Org"}],
            "managingOrganization": {"reference": "#o"}, "multipleBirthInteger": 2
        })).is_empty());
        assert!(problems(json!({
            "resourceType": "Observation", "status": "final", "code": {"text": "x"},
            "valueQuantity": {"value": 1.5, "unit": "mg"},
            "component": [{"code": {"text": "c"}, "valueInteger": 3}],
            "referenceRange": [{"low": {"value": 1}}]
        })).is_empty());
        // contentReference: Questionnaire.item.item is an item.
        assert!(problems(json!({
            "resourceType": "Questionnaire", "status": "draft",
            "item": [{"linkId": "1", "type": "group", "item": [{"linkId": "1.1", "type": "string", "text": "q"}]}]
        })).is_empty());
    }

    #[test]
    fn what_doesnt_fit_is_named() {
        let p = problems(json!({
            "resourceType": "Patient", "bogus": 1, "gender": 5, "name": {"family": "Doe"},
            "birthDate": ["1990"], "contact": [{"nmae": {}}], "deceasedString": "x",
            "contained": [{"resourceType": "Foo"}]
        }));
        let has = |s: &str| p.iter().any(|m| m.contains(s));
        assert!(has("Unknown element 'Patient.bogus'"), "{p:?}");
        assert!(has("'Patient.gender' is a code: a string, not a number"), "{p:?}");
        assert!(has("'Patient.name' repeats"), "{p:?}");
        assert!(has("'Patient.birthDate' has one value"), "{p:?}");
        assert!(has("Unknown element 'Patient.contact[0].nmae'"), "{p:?}");
        assert!(has("Unknown element 'Patient.deceasedString'"), "{p:?}");
        assert!(has("'Foo' is not a FHIR R4 resource type"), "{p:?}");
        // Inside a data type too.
        let p = problems(json!({"resourceType": "Patient", "name": [{"family": ["Doe"]}]}));
        assert!(p.iter().any(|m| m.contains("'Patient.name[0].family' has one value")), "{p:?}");
    }

    /// The R4 examples (examples-json.zip, as NDJSON in `R4_EXAMPLES`) all
    /// fit: `R4_EXAMPLES=all.ndjson cargo test -p sazare-core -- --ignored r4_examples`.
    #[test]
    #[ignore]
    fn r4_examples_fit() {
        let path = std::env::var("R4_EXAMPLES").expect("R4_EXAMPLES: the R4 examples as NDJSON");
        let text = std::fs::read_to_string(path).unwrap();
        let mut failed = vec![];
        let mut n = 0;
        for line in text.lines() {
            let v: serde_json::Value = serde_json::from_str(line).unwrap();
            n += 1;
            if let Err(o) = validate(&v) {
                let id = format!("{}/{}", v["resourceType"].as_str().unwrap_or("?"), v["id"].as_str().unwrap_or("?"));
                failed.push(format!("{id}: {}", o.issue.iter().filter_map(|i| i.diagnostics.clone()).collect::<Vec<_>>().join("; ")));
            }
        }
        assert!(failed.is_empty(), "{} of {n} examples don't fit:\n{}", failed.len(), failed.join("\n"));
    }
}
