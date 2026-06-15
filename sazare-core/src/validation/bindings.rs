//! Profile-driven required-binding validation.
//!
//! For a resource that declares profiles via `meta.profile`, validate the
//! `code` / `Coding` / `CodeableConcept` elements the profile binds to a
//! *required* value set — but only when that value set is enumerated in the
//! terminology registry. Value sets that reference external code systems
//! (SNOMED, ICD, …) aren't embedded and are skipped, so this never rejects
//! data it can't actually check.

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
            if id.contains(':') {
                continue; // plain elements only
            }
            let binding = match element.get("binding") {
                Some(b) if b.get("strength").and_then(|v| v.as_str()) == Some("required") => b,
                _ => continue,
            };
            let value_set = match binding.get("valueSet").and_then(|v| v.as_str()) {
                Some(vs) => vs.split('|').next().unwrap_or(vs),
                None => continue,
            };
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
