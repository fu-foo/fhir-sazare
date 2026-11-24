use crate::sqlite_index::StringMatch;
use crate::{SearchIndex, SqliteStore};
use sazare_core::{ChainParameter, HasParameter, SearchParameter, SearchParamType, SearchQuery};
use serde_json::Value;
use std::collections::HashSet;

/// Split a chained / reverse-chained value into (prefix, value): date params
/// carry a comparator prefix (`ge2020-01-01`) just like top-level ones.
fn split_prefix(param_type: &SearchParamType, value: &str) -> (Option<String>, String) {
    if *param_type == SearchParamType::Date {
        sazare_core::search_param::parse_date_prefix(value)
    } else if *param_type == SearchParamType::Number {
        sazare_core::search_param::parse_number_prefix(value)
    } else {
        (None, value.to_string())
    }
}

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
        self.search_with_total(resource_type, query).map(|(ids, _)| ids)
    }

    /// Execute a search query and return matching resource IDs with total count.
    /// Returns (paginated_ids, total_before_pagination).
    ///
    /// Ids are returned sorted, so paging is stable across requests and matches
    /// the parameterless listing (which is `ORDER BY id`).
    pub fn search_with_total(
        &self,
        resource_type: &str,
        query: &SearchQuery,
    ) -> Result<(Vec<String>, usize), String> {
        let mut ids = self.matching_ids(resource_type, query)?;
        let total = ids.len();

        // Apply pagination
        let offset = query.offset.unwrap_or(0).min(ids.len());
        ids.drain(..offset);
        if let Some(count) = query.count {
            ids.truncate(count);
        }

        Ok((ids, total))
    }

    /// All ids matching every parameter of `query` (AND across parameters,
    /// chains and `_has`), sorted, before pagination.
    fn matching_ids(
        &self,
        resource_type: &str,
        query: &SearchQuery,
    ) -> Result<Vec<String>, String> {
        let mut result: Option<Vec<String>> = None;

        // Each criterion narrows the running result (AND). The membership test
        // goes through a HashSet: a Vec::contains here is O(n*m) and took
        // seconds once both sides held tens of thousands of ids.
        let mut narrow = |ids: Vec<String>| {
            result = Some(match result.take() {
                None => ids,
                Some(existing) => {
                    let keep: HashSet<String> = ids.into_iter().collect();
                    existing.into_iter().filter(|id| keep.contains(id)).collect()
                }
            });
            result.as_ref().is_some_and(|ids| ids.is_empty())
        };

        'criteria: {
            for param in &query.parameters {
                if narrow(self.search_parameter(resource_type, param)?) {
                    break 'criteria;
                }
            }
            for chain in &query.chain_parameters {
                if narrow(self.search_chain(resource_type, chain)?) {
                    break 'criteria;
                }
            }
            for has in &query.has_parameters {
                if narrow(self.search_has(resource_type, has)?) {
                    break 'criteria;
                }
            }
        }

        match result {
            Some(mut ids) => {
                ids.sort_unstable();
                Ok(ids)
            }
            // No parameters: list all resource IDs (id column only — don't load
            // every resource body just to drop it). Already ORDER BY id.
            None => self.store.list_ids(resource_type).map_err(|e| e.to_string()),
        }
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
        // `:missing` — presence/absence of the parameter, independent of value.
        if param.modifier.as_deref() == Some("missing") {
            let want_missing = match param.value.trim() {
                "true" => true,
                "false" => false,
                other => return Err(format!(":missing expects true|false, got '{other}'")),
            };
            let present = self
                .index
                .ids_with_param(resource_type, &param.name)
                .map_err(|e| e.to_string())?;
            if !want_missing {
                return Ok(present);
            }
            let present: std::collections::HashSet<String> = present.into_iter().collect();
            let all = self.store.list_ids(resource_type).map_err(|e| e.to_string())?;
            return Ok(all.into_iter().filter(|id| !present.contains(id)).collect());
        }

        // `:not` — every resource of the type EXCEPT those matching the value(s).
        // FHIR: a resource with no value for the param is included in `:not`.
        if param.modifier.as_deref() == Some("not") {
            let mut inner = param.clone();
            inner.modifier = None;
            let matched: std::collections::HashSet<String> = self
                .search_parameter(resource_type, &inner)?
                .into_iter()
                .collect();
            let all = self.store.list_ids(resource_type).map_err(|e| e.to_string())?;
            return Ok(all.into_iter().filter(|id| !matched.contains(id)).collect());
        }

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
                // FHIR token forms: `system|code`, `code` (any system),
                // `|code` (no system), `system|` (any code in the system).
                match value.split_once('|') {
                    Some((system, "")) => self
                        .index
                        .search_token_system_only(resource_type, &param.name, system)
                        .map_err(|e| e.to_string()),
                    Some(("", code)) => self
                        .index
                        .search_token_no_system(resource_type, &param.name, code)
                        .map_err(|e| e.to_string()),
                    Some((system, code)) => self
                        .index
                        .search_token(resource_type, &param.name, Some(system), code)
                        .map_err(|e| e.to_string()),
                    None => self
                        .index
                        .search_token(resource_type, &param.name, None, value)
                        .map_err(|e| e.to_string()),
                }
            }
            SearchParamType::String => {
                let mode = match param.modifier.as_deref() {
                    Some("exact") => StringMatch::Exact,
                    Some("contains") => StringMatch::Contains,
                    _ => StringMatch::Prefix,
                };
                self.index.search_string(resource_type, &param.name, value, mode)
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
                if value.parse::<f64>().is_err() {
                    return Err(format!(
                        "Invalid number '{}' for parameter '{}'",
                        value, param.name
                    ));
                }
                let prefix = param.prefix.as_deref().unwrap_or("eq");
                self.index
                    .search_number_with_prefix(resource_type, &param.name, prefix, value)
                    .map_err(|e| e.to_string())
            }
        }
    }

    /// Execute a chain search: search the target type first, then find
    /// source resources that reference the matched targets.
    ///
    /// Example: `subject:Patient.name=Doe` on Observation
    /// 1. Search Patient where name=Doe → [Patient/p1, Patient/p2]
    /// 2. Search Observation where subject = Patient/p1 OR Patient/p2
    ///
    /// Resolve a (possibly multi-level) chained search. The terminal parameter
    /// is searched against the final target type, then each reference hop is
    /// walked backward toward `resource_type`. For
    /// `Observation?subject:Patient.organization:Organization.name=Acme`:
    ///   1. `Organization?name=Acme` -> org ids
    ///   2. `Patient` whose `organization` references those orgs -> patient ids
    ///   3. `Observation` whose `subject` references those patients -> result
    fn search_chain(
        &self,
        resource_type: &str,
        chain: &ChainParameter,
    ) -> Result<Vec<String>, String> {
        let Some(last) = chain.links.last() else {
            return Ok(Vec::new());
        };

        // Search the final target type by the terminal parameter.
        let (prefix, value) = split_prefix(&chain.target_param_type, &chain.value);
        let terminal = SearchParameter {
            name: chain.target_param.clone(),
            value,
            modifier: None,
            prefix,
            param_type: chain.target_param_type.clone(),
        };
        let mut current_ids = self.search_parameter(&last.target_type, &terminal)?;

        // Walk hops backward. For hop i, the resources holding `reference_param`
        // are of the previous hop's target type (or `resource_type` at i == 0).
        for i in (0..chain.links.len()).rev() {
            if current_ids.is_empty() {
                return Ok(Vec::new());
            }
            let link = &chain.links[i];
            let source_type: &str = if i == 0 {
                resource_type
            } else {
                &chain.links[i - 1].target_type
            };

            let references: Vec<String> = current_ids
                .iter()
                .map(|cid| format!("{}/{}", link.target_type, cid))
                .collect();
            current_ids = self
                .index
                .search_reference_any(source_type, &link.reference_param, &references)
                .map_err(|e| e.to_string())?;
        }

        Ok(current_ids)
    }

    /// Execute a one-level `_has` reverse chain: find source resources matching
    /// the inner search parameter, then return the searched-type resources they
    /// reference back. The mirror of [`search_chain`].
    ///
    /// `Patient?_has:Observation:patient:code=1234-5`:
    /// 1. `Observation?code=1234-5` -> matching observation ids
    /// 2. follow each observation's `patient` reference -> the Patient ids
    fn search_has(
        &self,
        resource_type: &str,
        has: &HasParameter,
    ) -> Result<Vec<String>, String> {
        let (prefix, value) = split_prefix(&has.target_param_type, &has.value);
        let inner = SearchParameter {
            name: has.target_param.clone(),
            value,
            modifier: None,
            prefix,
            param_type: has.target_param_type.clone(),
        };
        let source_ids = self.search_parameter(&has.source_type, &inner)?;
        if source_ids.is_empty() {
            return Ok(Vec::new());
        }
        self.index
            .referenced_targets(&has.source_type, &has.reference_param, &source_ids, resource_type)
            .map_err(|e| e.to_string())
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
        let mut seen_ids: HashSet<String> = HashSet::new();

        let references: Vec<String> = resources
            .iter()
            .filter_map(|r| r.get("id").and_then(|v| v.as_str()))
            .filter(|id| !id.is_empty())
            .map(|id| format!("{resource_type}/{id}"))
            .collect();

        for revinclude_spec in revincludes {
            let parts: Vec<&str> = revinclude_spec.split(':').collect();
            if parts.len() != 2 {
                continue;
            }
            let (target_type, search_param) = (parts[0], parts[1]);

            let mut matching_ids = self
                .index
                .search_reference_any(target_type, search_param, &references)
                .map_err(|e| e.to_string())?;
            matching_ids.sort_unstable();

            for mid in &matching_ids {
                if !seen_ids.insert(format!("{target_type}/{mid}")) {
                    continue;
                }
                if let Ok(Some(data)) = self.store.get(target_type, mid)
                    && let Ok(val) = serde_json::from_slice::<Value>(&data)
                {
                    included.push(val);
                }
            }
        }

        Ok(included)
    }

    /// Process _include parameter to load related resources.
    ///
    /// `registry` maps a search-parameter name to the JSON element it reads, so
    /// `_include`s whose parameter name differs from the element resolve
    /// correctly (`Observation:patient` → `subject`, hyphenated
    /// `general-practitioner` → `generalPractitioner`). Results are de-duplicated
    /// (a resource SHALL appear once in a searchset Bundle).
    pub fn process_includes(
        &self,
        resources: &[Value],
        includes: &[String],
        registry: &sazare_core::SearchParamRegistry,
    ) -> Result<Vec<Value>, String> {
        let mut included = Vec::new();
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

        for include_spec in includes {
            // Spec: `SourceType:search-param` or `SourceType:search-param:TargetType`.
            // The optional target type is only a filter on the included results.
            let parts: Vec<&str> = include_spec.split(':').collect();
            if parts.len() < 2 || parts.len() > 3 {
                continue;
            }
            let source_type = parts[0];
            let search_param = parts[1];
            let target_filter = parts.get(2).copied();

            // Resolve the JSON element the parameter reads (registry first, then
            // the parameter name itself / its choice + camelCase fallbacks).
            let element = registry
                .reference_element(source_type, search_param)
                .unwrap_or_else(|| search_param.to_string());

            for resource in resources {
                for reference in extract_references(resource, &element) {
                    if let Some((ref_type, ref_id)) = parse_reference(&reference) {
                        if let Some(t) = target_filter
                            && t != ref_type
                        {
                            continue;
                        }
                        let key = format!("{ref_type}/{ref_id}");
                        if !seen.insert(key) {
                            continue;
                        }
                        if let Ok(Some(data)) = self.store.get(ref_type, ref_id) {
                            included.push(serde_json::from_slice(&data).unwrap_or_default());
                        }
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
    // Prefer the element named exactly like the field; fall back to the
    // choice-type variant `<field>Reference` (FHIR capitalizes the type suffix),
    // then to a hyphen→camelCase form (`general-practitioner` →
    // `generalPractitioner`) for params not resolved via the registry.
    let camel = hyphen_to_camel(field);
    let value = resource
        .get(field)
        .or_else(|| resource.get(format!("{field}Reference")))
        .or_else(|| if camel != field { resource.get(&camel) } else { None });
    let value = match value {
        Some(v) => v,
        None => return Vec::new(),
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

/// Convert a hyphenated search-param name to the camelCase JSON element it most
/// likely maps to (`general-practitioner` → `generalPractitioner`).
fn hyphen_to_camel(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut upper = false;
    for c in s.chars() {
        if c == '-' {
            upper = true;
        } else if upper {
            out.extend(c.to_uppercase());
            upper = false;
        } else {
            out.push(c);
        }
    }
    out
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

    // --- Integration tests over an in-memory store + index ---

    fn put(store: &SqliteStore, rt: &str, id: &str, body: serde_json::Value) {
        let data = serde_json::to_vec(&body).unwrap();
        store.put_with_version(rt, id, "1", &data).unwrap();
    }

    fn sorted(mut v: Vec<String>) -> Vec<String> {
        v.sort();
        v
    }

    #[test]
    fn test_repeated_param_is_anded() {
        let store = SqliteStore::open(":memory:").unwrap();
        let index = SearchIndex::open(":memory:").unwrap();
        // o1 in [2024-06-15], o2 in [2023-01-01]
        put(&store, "Observation", "o1", serde_json::json!({"resourceType":"Observation","id":"o1"}));
        put(&store, "Observation", "o2", serde_json::json!({"resourceType":"Observation","id":"o2"}));
        index.add_index("Observation", "o1", "date", "date", Some("2024-06-15"), None).unwrap();
        index.add_index("Observation", "o2", "date", "date", Some("2023-01-01"), None).unwrap();

        let exec = SearchExecutor::new(&store, &index);
        // date=ge2024-01-01 AND date=le2024-12-31 → only o1
        let q = SearchQuery::parse_for_resource("date=ge2024-01-01&date=le2024-12-31", Some("Observation")).unwrap();
        assert_eq!(exec.search("Observation", &q).unwrap(), vec!["o1"]);
    }

    #[test]
    fn test_date_prefix_in_chain_and_has() {
        let store = SqliteStore::open(":memory:").unwrap();
        let index = SearchIndex::open(":memory:").unwrap();
        put(&store, "Patient", "p1", serde_json::json!({"resourceType":"Patient","id":"p1"}));
        put(&store, "Patient", "p2", serde_json::json!({"resourceType":"Patient","id":"p2"}));
        put(&store, "Observation", "o1", serde_json::json!({"resourceType":"Observation","id":"o1"}));
        put(&store, "Observation", "o2", serde_json::json!({"resourceType":"Observation","id":"o2"}));
        index.add_index("Patient", "p1", "birthdate", "date", Some("1970-01-01"), None).unwrap();
        index.add_index("Patient", "p2", "birthdate", "date", Some("2000-01-01"), None).unwrap();
        index.add_index("Observation", "o1", "subject", "reference", Some("Patient/p1"), None).unwrap();
        index.add_index("Observation", "o2", "subject", "reference", Some("Patient/p2"), None).unwrap();
        index.add_index("Observation", "o1", "date", "date", Some("2019-06-01"), None).unwrap();
        index.add_index("Observation", "o2", "date", "date", Some("2024-06-01"), None).unwrap();
        let exec = SearchExecutor::new(&store, &index);

        let q = SearchQuery::parse_for_resource("_has:Observation:subject:date=ge2020-01-01", Some("Patient")).unwrap();
        assert_eq!(exec.search("Patient", &q).unwrap(), vec!["p2"]);
        let q = SearchQuery::parse_for_resource("subject:Patient.birthdate=lt1980-01-01", Some("Observation")).unwrap();
        assert_eq!(exec.search("Observation", &q).unwrap(), vec!["o1"]);
    }

    #[test]
    fn test_missing_and_not_modifiers() {
        let store = SqliteStore::open(":memory:").unwrap();
        let index = SearchIndex::open(":memory:").unwrap();
        put(&store, "Patient", "p1", serde_json::json!({"resourceType":"Patient","id":"p1"}));
        put(&store, "Patient", "p2", serde_json::json!({"resourceType":"Patient","id":"p2"}));
        put(&store, "Patient", "p3", serde_json::json!({"resourceType":"Patient","id":"p3"}));
        // Only p1, p2 have a gender indexed.
        index.add_index("Patient", "p1", "gender", "token", Some("male"), None).unwrap();
        index.add_index("Patient", "p2", "gender", "token", Some("female"), None).unwrap();

        let exec = SearchExecutor::new(&store, &index);

        // gender:missing=true → p3
        let q = SearchQuery::parse_for_resource("gender:missing=true", Some("Patient")).unwrap();
        assert_eq!(exec.search("Patient", &q).unwrap(), vec!["p3"]);

        // gender:missing=false → p1, p2
        let q = SearchQuery::parse_for_resource("gender:missing=false", Some("Patient")).unwrap();
        assert_eq!(sorted(exec.search("Patient", &q).unwrap()), vec!["p1", "p2"]);

        // gender:not=male → everything except p1 (p2 matches female, p3 has none)
        let q = SearchQuery::parse_for_resource("gender:not=male", Some("Patient")).unwrap();
        assert_eq!(sorted(exec.search("Patient", &q).unwrap()), vec!["p2", "p3"]);
    }
}
