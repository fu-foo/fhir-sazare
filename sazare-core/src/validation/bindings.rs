//! Profile-driven required-binding validation.
//!
//! For a resource that declares profiles via `meta.profile`, validate the
//! `code` / `Coding` / `CodeableConcept` elements the profile binds to a
//! *required* value set — but only when that value set is enumerated in the
//! terminology registry. Value sets that reference external code systems
//! (SNOMED, ICD, …) aren't embedded and are skipped, so this never rejects
//! data it can't actually check.
//!
//! Slices (`Observation.code.coding:jlac10`) are checked too, in the one shape
//! that is unambiguous under open slicing: the slice fixes its code system
//! (`<slice>.system` / `<slice>.coding.system` = fixedUri) and the bound value
//! set includes that whole system. Then every coding claiming the system must
//! be a code of that (complete, loaded) system; codings of other systems
//! belong to other slices and are left alone.

use crate::operation_outcome::OperationOutcome;
use crate::validation::registry::{ProfileRegistry, TerminologyRegistry};
use serde_json::Value;

pub fn validate(
    resource: &Value,
    profiles: &ProfileRegistry,
    terms: &TerminologyRegistry,
) -> Result<(), OperationOutcome> {
    let resource_type = resource
        .get("resourceType")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let Some(profile_urls) = resource
        .get("meta")
        .and_then(|m| m.get("profile"))
        .and_then(|p| p.as_array())
    else {
        return Ok(());
    };

    for profile_url in profile_urls.iter().filter_map(|v| v.as_str()) {
        let Some(profile) = profiles.get_profile(profile_url) else {
            continue;
        };
        let Some(elements) = profile
            .get("snapshot")
            .or_else(|| profile.get("differential"))
            .and_then(|d| d.get("element"))
            .and_then(|e| e.as_array())
        else {
            continue;
        };

        for element in elements {
            let id = element.get("id").and_then(|v| v.as_str()).unwrap_or("");
            let binding = match element.get("binding") {
                Some(b) if b.get("strength").and_then(|v| v.as_str()) == Some("required") => b,
                _ => continue,
            };
            let value_set = match binding.get("valueSet").and_then(|v| v.as_str()) {
                Some(vs) => vs.split('|').next().unwrap_or(vs),
                None => continue,
            };
            if id.contains(':') {
                check_slice(resource, resource_type, profile_url, id, element, elements, value_set, terms)?;
                continue;
            }
            if !terms.has_value_set(value_set) {
                continue; // not embedded → leave to a terminology service
            }

            let path = element.get("path").and_then(|v| v.as_str()).unwrap_or("");
            let rel = match path.strip_prefix(&format!("{}.", resource_type)) {
                Some(r) if !r.is_empty() => r,
                _ => continue,
            };
            let mut values: Vec<&Value> = Vec::new();
            collect(resource, &rel.split('.').collect::<Vec<_>>(), &mut values);

            for value in values {
                // Infer the shape from the value (the differential rarely
                // re-declares the element type): a string is a `code`, an object
                // with `coding` is a CodeableConcept, an object with `code` is a
                // Coding. Anything else is skipped.
                let ok = if let Some(code) = value.as_str() {
                    terms.validate_code(value_set, code)
                } else if let Some(codings) = value.get("coding").and_then(|c| c.as_array()) {
                    codings.is_empty()
                        || codings.iter().any(|c| {
                            c.get("code")
                                .and_then(|x| x.as_str())
                                .is_some_and(|c| terms.validate_code(value_set, c))
                        })
                } else if let Some(code) = value.get("code").and_then(|c| c.as_str()) {
                    terms.validate_code(value_set, code)
                } else {
                    true
                };
                if !ok {
                    return Err(OperationOutcome::validation_error(format!(
                        "Profile '{}': element '{}' code is not in the required value set {}",
                        profile_url, path, value_set
                    ))
                    .with_expression(vec![path.to_string()]));
                }
            }
        }
    }
    Ok(())
}

/// Check a required binding on a slice (see the module docs for when).
#[allow(clippy::too_many_arguments)]
fn check_slice(
    resource: &Value,
    resource_type: &str,
    profile_url: &str,
    id: &str,
    element: &Value,
    elements: &[Value],
    value_set: &str,
    terms: &TerminologyRegistry,
) -> Result<(), OperationOutcome> {
    let path = element.get("path").and_then(|v| v.as_str()).unwrap_or("");
    // Extension slices are discriminated by url, which a plain path walk
    // can't see — leave them alone.
    if path.split('.').any(|p| p == "extension" || p == "modifierExtension") {
        return Ok(());
    }
    let fixed_system = elements.iter().find_map(|e| {
        let eid = e.get("id").and_then(|v| v.as_str())?;
        if eid != format!("{id}.system") && eid != format!("{id}.coding.system") {
            return None;
        }
        e.get("fixedUri")
            .or_else(|| e.get("patternUri"))
            .and_then(|v| v.as_str())
    });
    let Some(system) = fixed_system else {
        return Ok(());
    };
    if !terms.value_set_includes_whole_system(value_set, system) {
        return Ok(());
    }
    let Some(rel) = path.strip_prefix(&format!("{resource_type}.")) else {
        return Ok(());
    };
    let mut values: Vec<&Value> = Vec::new();
    collect(resource, &rel.split('.').collect::<Vec<_>>(), &mut values);

    for value in values {
        let codings: Vec<&Value> = match value.get("coding").and_then(|c| c.as_array()) {
            Some(cs) => cs.iter().collect(),
            None => vec![value],
        };
        for coding in codings {
            if coding.get("system").and_then(|v| v.as_str()) != Some(system) {
                continue;
            }
            let Some(code) = coding.get("code").and_then(|v| v.as_str()) else {
                continue;
            };
            if terms.code_system_contains(system, code) == Some(false) {
                return Err(OperationOutcome::validation_error(format!(
                    "Profile '{}': element '{}' code '{}' is not in code system {} (required value set {})",
                    profile_url, id, code, system, value_set
                ))
                .with_expression(vec![path.to_string()]));
            }
        }
    }
    Ok(())
}

/// Collect leaf values at a dotted relative path, descending into arrays.
fn collect<'a>(value: &'a Value, parts: &[&str], out: &mut Vec<&'a Value>) {
    if parts.is_empty() {
        match value {
            Value::Array(arr) => out.extend(arr.iter()),
            Value::Null => {}
            _ => out.push(value),
        }
        return;
    }
    match value.get(parts[0]) {
        None => {}
        Some(child) => {
            if parts.len() == 1 {
                match child {
                    Value::Array(arr) => out.extend(arr.iter()),
                    Value::Null => {}
                    _ => out.push(child),
                }
            } else {
                match child {
                    Value::Array(arr) => {
                        for item in arr {
                            collect(item, &parts[1..], out);
                        }
                    }
                    Value::Object(_) => collect(child, &parts[1..], out),
                    _ => {}
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::validation::TerminologyLoader;
    use serde_json::json;

    const PROFILE: &str = "http://x/StructureDefinition/P";

    fn setup() -> (ProfileRegistry, TerminologyRegistry) {
        let mut profiles = ProfileRegistry::new();
        profiles.add_profile(json!({
            "resourceType": "StructureDefinition", "url": PROFILE, "type": "Procedure",
            "snapshot": {"element": [
                {"id": "Procedure.code.coding", "path": "Procedure.code.coding",
                 "slicing": {"discriminator": [{"type": "value", "path": "system"}], "rules": "open"}},
                {"id": "Procedure.code.coding:medical", "path": "Procedure.code.coding",
                 "binding": {"strength": "required", "valueSet": "http://x/vs/medical|1.0"}},
                {"id": "Procedure.code.coding:medical.system", "path": "Procedure.code.coding.system",
                 "fixedUri": "http://x/cs/medical"},
                // Slice bound to an enumerated subset: not checked (other codes
                // of the same system may belong to other slices).
                {"id": "Procedure.category:first", "path": "Procedure.category",
                 "binding": {"strength": "required", "valueSet": "http://x/vs/subset"}},
                {"id": "Procedure.category:first.coding.system", "path": "Procedure.category.coding.system",
                 "fixedUri": "http://x/cs/medical"},
                // Extension slice: discriminated by url, left alone.
                {"id": "Procedure.extension:e", "path": "Procedure.extension",
                 "binding": {"strength": "required", "valueSet": "http://x/vs/medical"}},
                {"id": "Procedure.extension:e.system", "path": "Procedure.extension.system",
                 "fixedUri": "http://x/cs/medical"}
            ]}
        }));
        let mut terms = TerminologyRegistry::new();
        let mut loader = TerminologyLoader::default();
        for r in [
            json!({"resourceType": "CodeSystem", "url": "http://x/cs/medical", "content": "complete",
                   "concept": [{"code": "140000610"}, {"code": "150000000"}]}),
            json!({"resourceType": "ValueSet", "url": "http://x/vs/medical",
                   "compose": {"include": [{"system": "http://x/cs/medical"}]}}),
            json!({"resourceType": "ValueSet", "url": "http://x/vs/subset",
                   "compose": {"include": [{"system": "http://x/cs/medical", "concept": [{"code": "140000610"}]}]}}),
        ] {
            loader.add(&mut terms, &r);
        }
        loader.finish(&mut terms);
        (profiles, terms)
    }

    fn procedure(codings: serde_json::Value) -> serde_json::Value {
        json!({"resourceType": "Procedure", "meta": {"profile": [PROFILE]},
               "code": {"coding": codings},
               "category": {"coding": [{"system": "http://x/cs/medical", "code": "150000000"}]},
               "extension": [{"url": "http://x/e", "system": "http://x/cs/medical", "code": "NOPE"}]})
    }

    #[test]
    fn slice_binding_checks_codings_of_the_fixed_system() {
        let (profiles, terms) = setup();
        let ok = procedure(json!([{"system": "http://x/cs/medical", "code": "140000610"}]));
        assert!(validate(&ok, &profiles, &terms).is_ok());

        let bad = procedure(json!([{"system": "http://x/cs/medical", "code": "NOPE"}]));
        let err = validate(&bad, &profiles, &terms).unwrap_err();
        let msg = serde_json::to_string(&err).unwrap();
        assert!(msg.contains("Procedure.code.coding:medical") && msg.contains("NOPE"), "{msg}");
    }

    #[test]
    fn slice_binding_ignores_other_systems_subsets_and_extensions() {
        let (profiles, terms) = setup();
        // Open slicing: a coding of another system is another slice.
        let other = procedure(json!([
            {"system": "http://x/cs/medical", "code": "150000000"},
            {"system": "http://hospital.local/codes", "code": "NOPE"}
        ]));
        // category (bound to an enumerated subset) holds 150000000, which is
        // outside the subset; extension holds NOPE — neither is checked.
        assert!(validate(&other, &profiles, &terms).is_ok());
    }
}
