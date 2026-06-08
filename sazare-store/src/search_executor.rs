use crate::{SearchIndex, SqliteStore};
use sazare_core::{ChainParameter, SearchParameter, SearchParamType, SearchQuery};
use serde_json::Value;

/// Execute FHIR search queries
pub struct SearchExecutor<'a> {
    store: &'a SqliteStore,
    index: &'a SearchIndex,
}

impl<'a> SearchExecutor<'a> {
    pub fn new(store: &'a SqliteStore, index: &'a SearchIndex) -> Self {
        Self { store, index }
    }

    /// Execute a search query and return matching resource IDs
    pub fn search(
        &self,
        resource_type: &str,
        query: &SearchQuery,
    ) -> Result<Vec<String>, String> {
        let mut result_ids: Option<Vec<String>> = None;

        // Process each search parameter
        for param in &query.parameters {
            let param_results = self.search_parameter(resource_type, param)?;

            // Intersect with existing results (AND logic)
            result_ids = match result_ids {
                None => Some(param_results),
                Some(existing) => {
                    let intersection: Vec<String> = existing
                        .into_iter()
                        .filter(|id| param_results.contains(id))
                        .collect();
                    Some(intersection)
                }
            };

            // Early exit if no results
            if let Some(ref ids) = result_ids
                && ids.is_empty()
            {
                break;
            }
        }

        // Process chain parameters (e.g. subject:Patient.name=Doe)
        for chain in &query.chain_parameters {
            let chain_results = self.search_chain(resource_type, chain)?;

            result_ids = match result_ids {
                None => Some(chain_results),
                Some(existing) => {
                    let intersection: Vec<String> = existing
                        .into_iter()
                        .filter(|id| chain_results.contains(id))
                        .collect();
                    Some(intersection)
                }
            };

            if let Some(ref ids) = result_ids
                && ids.is_empty()
            {
                break;
            }
        }

        // If no search parameters were given, return all resources of this type
        let mut ids = match result_ids {
            Some(ids) => ids,
            None => {
                // No parameters: list all resource IDs (id column only — don't
                // load every resource body just to drop it).
                self.store.list_ids(resource_type).map_err(|e| e.to_string())?
            }
        };

        // Apply pagination
        if let Some(offset) = query.offset {
            ids = ids.into_iter().skip(offset).collect();
        }
        if let Some(count) = query.count {
            ids.truncate(count);
        }

        Ok(ids)
    }

    /// Execute a search query and return matching resource IDs with total count.
    /// Returns (paginated_ids, total_before_pagination).
    pub fn search_with_total(
        &self,
        resource_type: &str,
        query: &SearchQuery,
    ) -> Result<(Vec<String>, usize), String> {
        let mut result_ids: Option<Vec<String>> = None;

        for param in &query.parameters {
            let param_results = self.search_parameter(resource_type, param)?;
            result_ids = match result_ids {
                None => Some(param_results),
                Some(existing) => {
                    let intersection: Vec<String> = existing
                        .into_iter()
                        .filter(|id| param_results.contains(id))
                        .collect();
                    Some(intersection)
                }
            };
            if let Some(ref ids) = result_ids
                && ids.is_empty()
            {
                break;
            }
        }

        for chain in &query.chain_parameters {
            let chain_results = self.search_chain(resource_type, chain)?;
            result_ids = match result_ids {
                None => Some(chain_results),
                Some(existing) => {
                    let intersection: Vec<String> = existing
                        .into_iter()
                        .filter(|id| chain_results.contains(id))
                        .collect();
                    Some(intersection)
                }
            };
            if let Some(ref ids) = result_ids
                && ids.is_empty()
            {
                break;
            }
        }

        let mut ids = match result_ids {
            Some(ids) => ids,
            None => self.store.list_ids(resource_type).map_err(|e| e.to_string())?,
        };

        let total = ids.len();

        // Apply pagination
        if let Some(offset) = query.offset {
            ids = ids.into_iter().skip(offset).collect();
        }
        if let Some(count) = query.count {
            ids.truncate(count);
        }

        Ok((ids, total))
    }

    /// Search for a single parameter
    ///
    /// FHIR spec: comma-separated values in a single param mean OR.
    /// e.g. `intent=order,plan` → resources matching `order` OR `plan`.
    fn search_parameter(
        &self,
        resource_type: &str,
        param: &SearchParameter,
    ) -> Result<Vec<String>, String> {
        let values: Vec<&str> = param.value.split(',').collect();
        if values.len() == 1 {
            return self.search_parameter_single(resource_type, param, &param.value);
        }

        // Multi-value: union results across each value
        let mut union: std::collections::HashSet<String> = std::collections::HashSet::new();
        for v in values {
            let v = v.trim();
            if v.is_empty() {
                continue;
            }
            let ids = self.search_parameter_single(resource_type, param, v)?;
            union.extend(ids);
        }
        Ok(union.into_iter().collect())
    }

    /// Search for a single parameter with a single value (no comma).
    fn search_parameter_single(
        &self,
        resource_type: &str,
        param: &SearchParameter,
        value: &str,
    ) -> Result<Vec<String>, String> {
        match param.param_type {
            SearchParamType::Token => {
                // For token search, parse system|code format
                let (system, code) = if let Some(idx) = value.find('|') {
                    let (sys, cod) = value.split_at(idx);
                    (Some(sys), &cod[1..])
                } else {
                    (None, value)
                };
                self.index.search_token(resource_type, &param.name, system, code)
                    .map_err(|e| e.to_string())
            }
            SearchParamType::String => {
                let exact = param.modifier.as_deref() == Some("exact");
                self.index.search_string(resource_type, &param.name, value, exact)
                    .map_err(|e| e.to_string())
            }
            SearchParamType::Date => {
                let prefix = param.prefix.as_deref().unwrap_or("eq");
                self.index.search_date_with_prefix(resource_type, &param.name, prefix, value)
                    .map_err(|e| e.to_string())
            }
            SearchParamType::Reference => {
                self.index.search_reference(resource_type, &param.name, value)
                    .map_err(|e| e.to_string())
            }
            SearchParamType::Number => {
                // Number search isn't implemented. Return an explicit error
                // rather than silently matching nothing (which looks like a
                // valid "no results" to the client).
                Err(format!(
                    "Number search is not supported for parameter '{}'",
                    param.name
                ))
            }
        }
    }

    /// Execute a chain search: search the target type first, then find
    /// source resources that reference the matched targets.
    ///
    /// Example: `subject:Patient.name=Doe` on Observation
    /// 1. Search Patient where name=Doe → [Patient/p1, Patient/p2]
    /// 2. Search Observation where subject = Patient/p1 OR Patient/p2
    fn search_chain(
        &self,
        resource_type: &str,
        chain: &ChainParameter,
    ) -> Result<Vec<String>, String> {
        // Step 1: Build a SearchParameter for the target type and search
        let target_param = SearchParameter {
            name: chain.target_param.clone(),
            value: chain.value.clone(),
            modifier: None,
            prefix: if chain.target_param_type == SearchParamType::Date {
                Some("eq".to_string())
            } else {
                None
            },
            param_type: chain.target_param_type.clone(),
        };

        let target_ids = self.search_parameter(&chain.target_type, &target_param)?;

        if target_ids.is_empty() {
            return Ok(Vec::new());
        }

        // Step 2: For each matched target, search source resources by reference
        let mut all_source_ids = Vec::new();
        for target_id in &target_ids {
            let reference = format!("{}/{}", chain.target_type, target_id);
            let ids = self.index.search_reference(
                resource_type,
                &chain.reference_param,
                &reference,
            ).map_err(|e| e.to_string())?;
            for id in ids {
                if !all_source_ids.contains(&id) {
                    all_source_ids.push(id);
                }
            }
        }

        Ok(all_source_ids)
    }

    /// Load full resources for the given IDs
    pub fn load_resources(
        &self,
        resource_type: &str,
        ids: &[String],
    ) -> Result<Vec<Value>, String> {
        let mut resources = Vec::new();

        for id in ids {
            match self.store.get(resource_type, id) {
                Ok(Some(data)) => {
                    let resource: Value = serde_json::from_slice(&data)
                        .map_err(|e| format!("Failed to parse resource: {}", e))?;
                    resources.push(resource);
                }
                Ok(None) => {
                    // Resource was deleted or index is stale, skip
                }
                Err(e) => {
                    return Err(format!("Failed to load resource {}/{}: {}", resource_type, id, e));
                }
            }
        }

        Ok(resources)
    }

    /// Process _revinclude parameter to load resources that reference the search results.
    ///
    /// Each revinclude spec is `TargetType:search-param`, e.g. `Observation:subject`.
    /// For each resource in the search results, find TargetType resources whose
    /// search-param references `{resource_type}/{id}`.
    pub fn process_revincludes(
        &self,
        resources: &[Value],
        resource_type: &str,
        revincludes: &[String],
    ) -> Result<Vec<Value>, String> {
        let mut included = Vec::new();
        let mut seen_ids: Vec<String> = Vec::new();

        for revinclude_spec in revincludes {
            let parts: Vec<&str> = revinclude_spec.split(':').collect();
            if parts.len() != 2 {
                continue;
            }
            let (target_type, search_param) = (parts[0], parts[1]);

            for resource in resources {
                let id = resource.get("id").and_then(|v| v.as_str()).unwrap_or("");
                if id.is_empty() {
                    continue;
                }
                let reference = format!("{}/{}", resource_type, id);

                let matching_ids = self
                    .index
                    .search_reference(target_type, search_param, &reference)
                    .map_err(|e| e.to_string())?;

                for mid in &matching_ids {
                    let key = format!("{}/{}", target_type, mid);
                    if seen_ids.contains(&key) {
                        continue;
                    }
                    seen_ids.push(key);

                    if let Ok(Some(data)) = self.store.get(target_type, mid)
                        && let Ok(val) = serde_json::from_slice::<Value>(&data)
                    {
                        included.push(val);
                    }
                }
            }
        }

        Ok(included)
    }

    /// Process _include parameter to load related resources
    pub fn process_includes(
        &self,
        resources: &[Value],
        includes: &[String],
    ) -> Result<Vec<Value>, String> {
        let mut included = Vec::new();

        for include_spec in includes {
            // Parse include spec: ResourceType:search-param
            let parts: Vec<&str> = include_spec.split(':').collect();
            if parts.len() != 2 {
                continue;
            }

            let (_source_type, search_param) = (parts[0], parts[1]);

            // Extract references from source resources. A single search param can
            // resolve multiple references (array-valued elements like `performer`),
            // so fan out over all of them.
            for resource in resources {
                for reference in extract_references(resource, search_param) {
                    if let Some((ref_type, ref_id)) = parse_reference(&reference)
                        && let Ok(Some(data)) = self.store.get(ref_type, ref_id)
                    {
                        let included_resource: Value =
                            serde_json::from_slice(&data).unwrap_or_default();
                        included.push(included_resource);
                    }
                }
            }
        }

        Ok(included)
    }
}

/// Extract the reference strings an `_include`/`_revinclude` search param points
/// at within a source resource. Handles the three shapes a FHIR reference element
/// can take:
///   - a single Reference object (e.g. `subject`)
///   - an array of Reference objects (e.g. `performer`, `result`)
///   - a choice-type element stored under `<field>Reference` (e.g. the
///     `medication` search param resolves `medicationReference`)
///
/// A choice-type element bound to a non-reference (e.g. `medicationCodeableConcept`)
/// yields nothing, since there is no resource to include.
fn extract_references(resource: &Value, field: &str) -> Vec<String> {
    // Prefer the element named exactly like the search param; fall back to the
    // choice-type variant `<field>Reference` (FHIR capitalizes the type suffix).
    let value = match resource.get(field) {
        Some(v) => v,
        None => match resource.get(format!("{field}Reference")) {
            Some(v) => v,
            None => return Vec::new(),
        },
    };

    let ref_of = |v: &Value| {
        v.get("reference")
            .and_then(|r| r.as_str())
            .map(|s| s.to_string())
    };

    match value {
        Value::Array(arr) => arr.iter().filter_map(ref_of).collect(),
        Value::Object(_) => ref_of(value).into_iter().collect(),
        _ => Vec::new(),
    }
}

/// Parse a FHIR reference string (e.g., "Patient/123")
fn parse_reference(reference: &str) -> Option<(&str, &str)> {
    let parts: Vec<&str> = reference.split('/').collect();
    if parts.len() == 2 {
        Some((parts[0], parts[1]))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_reference() {
        let (resource_type, id) = parse_reference("Patient/123").unwrap();
        assert_eq!(resource_type, "Patient");
        assert_eq!(id, "123");
    }

    #[test]
    fn test_parse_reference_invalid() {
        assert!(parse_reference("InvalidReference").is_none());
    }

    #[test]
    fn test_extract_references_single() {
        let resource = serde_json::json!({
            "subject": { "reference": "Patient/123" }
        });
        assert_eq!(extract_references(&resource, "subject"), vec!["Patient/123"]);
    }

    #[test]
    fn test_extract_references_choice_type() {
        // `_include=MedicationRequest:medication` must resolve `medicationReference`.
        let resource = serde_json::json!({
            "medicationReference": { "reference": "Medication/med-1" }
        });
        assert_eq!(
            extract_references(&resource, "medication"),
            vec!["Medication/med-1"]
        );
    }

    #[test]
    fn test_extract_references_choice_type_codeable_concept_yields_nothing() {
        // A CodeableConcept-valued choice has no resource to include.
        let resource = serde_json::json!({
            "medicationCodeableConcept": { "text": "aspirin" }
        });
        assert!(extract_references(&resource, "medication").is_empty());
    }

    #[test]
    fn test_extract_references_array() {
        // Array-valued elements (e.g. performer) resolve to multiple references.
        let resource = serde_json::json!({
            "performer": [
                { "reference": "Practitioner/p1" },
                { "reference": "Organization/o1" }
            ]
        });
        assert_eq!(
            extract_references(&resource, "performer"),
            vec!["Practitioner/p1", "Organization/o1"]
        );
    }

    #[test]
    fn test_extract_references_missing() {
        let resource = serde_json::json!({ "status": "active" });
        assert!(extract_references(&resource, "subject").is_empty());
    }
}
