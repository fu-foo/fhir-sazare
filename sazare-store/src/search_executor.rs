use crate::sqlite_index::StringMatch;
use crate::search_index::Engine;
use crate::sqlite_index::V1Index;
use crate::{SearchIndex, SqliteStore};
use sazare_core::{ChainParameter, HasParameter, SearchParameter, SearchParamType, SearchQuery};
use serde_json::Value;
use std::collections::HashSet;

/// Split a chained / reverse-chained value into (prefix, value): date params
/// carry a comparator prefix (`ge2020-01-01`) just like top-level ones.
pub(crate) fn split_prefix(param_type: &SearchParamType, value: &str) -> (Option<String>, String) {
    if *param_type == SearchParamType::Date {
        sazare_core::search_param::parse_date_prefix(value)
    } else if matches!(param_type, SearchParamType::Number | SearchParamType::Quantity) {
        sazare_core::search_param::parse_number_prefix(value)
    } else {
        (None, value.to_string())
    }
}

/// The start of a search error that only the index being built can answer
/// (a v1 index serving during the upgrade): a 503, not a failure.
pub const INDEX_NOT_READY: &str = "The new search index is still being built";

/// Execute FHIR search queries on whichever index engine is open: v2, or a
/// v1 index still serving while its v2 replacement is built.
pub struct SearchExecutor<'a> {
    store: &'a SqliteStore,
    index: &'a SearchIndex,
}

impl<'a> SearchExecutor<'a> {
    pub fn new(store: &'a SqliteStore, index: &'a SearchIndex) -> Self {
        Self { store, index }
    }

    /// The page of ids `query` asks for.
    pub fn search(&self, resource_type: &str, query: &SearchQuery) -> Result<Vec<String>, String> {
        self.search_with_total(resource_type, query).map(|(ids, _)| ids)
    }

    /// The page of ids `query` asks for (`_sort`, `_offset`, `_count`) and
    /// the total number of matches.
    pub fn search_with_total(&self, resource_type: &str, query: &SearchQuery) -> Result<(Vec<String>, usize), String> {
        // An index from before quantities were indexed (v1, 0.16), still
        // serving while its replacement is built.
        if !self.index.has_quantities() && uses_quantity(query) {
            return Err(format!("{INDEX_NOT_READY}: quantity searches need it (GET /health shows the progress)"));
        }
        match self.index.engine() {
            Engine::V1(index) => V1Executor::new(self.store, index).search_with_total(resource_type, query),
            Engine::V2(index) => index.search_with_total(resource_type, query),
        }
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

            // A known parameter: its references exactly as the index extracts
            // them (any extraction, FHIRPath included). Otherwise the element
            // named like the parameter (choice + camelCase fallbacks).
            let known = registry
                .get_definitions(source_type)
                .iter()
                .any(|d| d.param_type == SearchParamType::Reference && (d.name == search_param || d.aliases.iter().any(|a| a == search_param)));

            for resource in resources {
                // `Observation:patient` applies to Observations only.
                if resource.get("resourceType").and_then(Value::as_str) != Some(source_type) {
                    continue;
                }
                let references: Vec<String> = if known {
                    crate::IndexBuilder::extract_param(registry, source_type, resource, search_param)
                        .into_iter()
                        .filter(|(_, t, _, _)| t == "reference")
                        .map(|(_, _, v, _)| v)
                        .collect()
                } else {
                    extract_references(resource, search_param)
                };
                for reference in references {
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

/// Whether a query has a quantity criterion (directly, in a chain or `_has`).
fn uses_quantity(q: &SearchQuery) -> bool {
    q.parameters.iter().any(|p| p.param_type == SearchParamType::Quantity)
        || q.chain_parameters.iter().any(|c| c.target_param_type == SearchParamType::Quantity)
        || q.has_parameters.iter().any(|h| h.target_param_type == SearchParamType::Quantity)
        || q.sort.iter().any(|k| k.param_type == Some(SearchParamType::Quantity))
}

/// Searches on a v1 index.
pub struct V1Executor<'a> {
    store: &'a SqliteStore,
    index: &'a V1Index,
}

impl<'a> V1Executor<'a> {
    pub fn new(store: &'a SqliteStore, index: &'a V1Index) -> Self {
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
        if !query.sort.is_empty() {
            self.sort_ids(resource_type, &query.sort, &mut ids)?;
        }

        // Apply pagination
        let offset = query.offset.unwrap_or(0).min(ids.len());
        ids.drain(..offset);
        if let Some(count) = query.count {
            ids.truncate(count);
        }

        Ok((ids, total))
    }

    /// Order `ids` by `_sort` keys (before pagination). Per key, a resource
    /// with several values sorts by its lowest (ascending) or highest
    /// (descending) one; resources without a value go last either way. Ties
    /// fall through to the next key, then to the id, so paging is stable.
    fn sort_ids(
        &self,
        resource_type: &str,
        keys: &[sazare_core::SortKey],
        ids: &mut [String],
    ) -> Result<(), String> {
        use std::cmp::Ordering;
        use std::collections::HashMap;

        #[derive(Clone, PartialEq, PartialOrd)]
        enum Val {
            Num(f64),
            Text(String),
        }
        fn cmp_val(a: &Val, b: &Val) -> Ordering {
            match (a, b) {
                (Val::Num(x), Val::Num(y)) => x.total_cmp(y),
                (Val::Text(x), Val::Text(y)) => x.cmp(y),
                (Val::Num(_), Val::Text(_)) => Ordering::Less,
                (Val::Text(_), Val::Num(_)) => Ordering::Greater,
            }
        }

        // One map per key: id -> its representative value for that direction.
        let mut maps: Vec<(HashMap<String, Val>, bool)> = Vec::new();
        for key in keys {
            let mut map: HashMap<String, Val> = HashMap::new();
            if key.name == "_id" {
                for id in ids.iter() {
                    map.insert(id.clone(), Val::Text(id.clone()));
                }
            } else {
                // Narrowed result sets read only their own values; large ones
                // read the whole type in one pass. Measured on 50k
                // Observations: by id wins at ~6k ids (31 vs 52 ms) and loses
                // at 10k (43 vs 22 ms), hence the cut-off in between.
                const BY_ID_LIMIT: usize = 8000;
                let only = (ids.len() <= BY_ID_LIMIT).then_some(&*ids);
                let rows = self
                    .index
                    .sort_values(resource_type, &key.name, only)
                    .map_err(|e| e.to_string())?;
                for (id, num, text) in rows {
                    let v = match (num, text) {
                        (Some(n), _) => Val::Num(n),
                        (None, Some(t)) => Val::Text(t),
                        (None, None) => continue,
                    };
                    map.entry(id)
                        .and_modify(|cur| {
                            let better = if key.descending {
                                cmp_val(&v, cur) == Ordering::Greater
                            } else {
                                cmp_val(&v, cur) == Ordering::Less
                            };
                            if better {
                                *cur = v.clone();
                            }
                        })
                        .or_insert(v);
                }
            }
            maps.push((map, key.descending));
        }

        ids.sort_by(|a, b| {
            for (map, desc) in &maps {
                let ord = match (map.get(a), map.get(b)) {
                    (Some(x), Some(y)) => {
                        let o = cmp_val(x, y);
                        if *desc { o.reverse() } else { o }
                    }
                    (Some(_), None) => Ordering::Less,
                    (None, Some(_)) => Ordering::Greater,
                    (None, None) => Ordering::Equal,
                };
                if ord != Ordering::Equal {
                    return ord;
                }
            }
            a.cmp(b)
        });
        Ok(())
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
        fn narrow(result: &mut Option<Vec<String>>, ids: Vec<String>) -> bool {
            *result = Some(match result.take() {
                None => ids,
                Some(existing) => {
                    let keep: HashSet<String> = ids.into_iter().collect();
                    existing.into_iter().filter(|id| keep.contains(id)).collect()
                }
            });
            result.as_ref().is_some_and(|ids| ids.is_empty())
        }

        'criteria: {
            // Most selective kinds first (a reference or id usually leaves a
            // handful of candidates), then let each further parameter look
            // only at the candidates left (`set_scope`): `patient=X&code=Y`
            // then reads X's few resources, not every resource coded Y.
            let mut params: Vec<&SearchParameter> = query.parameters.iter().collect();
            params.sort_by_key(|p| selectivity_rank(p));
            let scoped = std::cell::Cell::new(false);
            let _clear = ScopeGuard { index: self.index, active: &scoped };
            for param in params {
                let within = if scoped.get() { result.as_deref() } else { None };
                let ids = self.search_parameter_within(resource_type, param, within)?;
                if narrow(&mut result, ids) {
                    break 'criteria;
                }
                if let Some(current) = result.as_ref()
                    && current.len() <= SCOPE_MAX
                {
                    self.index.set_scope(resource_type, current).map_err(|e| e.to_string())?;
                    scoped.set(true);
                }
            }
            // Chains and _has search other resource types: no scope.
            self.index.clear_scope();
            scoped.set(false);
            for chain in &query.chain_parameters {
                if narrow(&mut result, self.search_chain(resource_type, chain)?) {
                    break 'criteria;
                }
            }
            for has in &query.has_parameters {
                if narrow(&mut result, self.search_has(resource_type, has)?) {
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
        self.search_parameter_within(resource_type, param, None)
    }

    /// [`Self::search_parameter`] where only `within` can still match (the
    /// candidates earlier criteria left): `:not` / `:missing` then complement
    /// against those instead of every resource of the type. The result may
    /// include ids outside `within`; the caller intersects anyway.
    fn search_parameter_within(
        &self,
        resource_type: &str,
        param: &SearchParameter,
        within: Option<&[String]>,
    ) -> Result<Vec<String>, String> {
        let universe = || -> Result<Vec<String>, String> {
            match within {
                Some(ids) => Ok(ids.to_vec()),
                None => self.store.list_ids(resource_type).map_err(|e| e.to_string()),
            }
        };
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
            let all = universe()?;
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
            let all = universe()?;
            return Ok(all.into_iter().filter(|id| !matched.contains(id)).collect());
        }

        // Each alternative with its own prefix (`ge2020,lt2019`); one alone
        // is searched as written.
        let alts = sazare_core::search_param::prefixed_alternatives(param);
        if !param.value.contains(',') {
            return self.search_parameter_single(resource_type, param, &param.value);
        }

        // Multi-value: union results across each value
        let mut union: std::collections::HashSet<String> = std::collections::HashSet::new();
        for (prefix, v) in alts {
            let alt = SearchParameter { prefix, ..param.clone() };
            let ids = self.search_parameter_single(resource_type, &alt, &v)?;
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
        // Full-text parameters: narrative (`_text`) or the whole resource
        // (`_content`).
        match param.name.as_str() {
            "_text" => {
                return self.index.search_text(resource_type, "narrative", value).map_err(|e| e.to_string());
            }
            "_content" => {
                return self.index.search_text(resource_type, "content", value).map_err(|e| e.to_string());
            }
            _ => {}
        }
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
            // Indexed by v2 only: a v1 index serves only while its v2
            // replacement is built.
            SearchParamType::Quantity => Err(format!(
                "{INDEX_NOT_READY}: searching '{}' needs it (GET /health shows the progress)",
                param.name
            )),
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
        let terminal = terminal_parameter(&chain.target_param, &chain.target_param_type, &chain.value);
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
        let inner = terminal_parameter(&has.target_param, &has.target_param_type, &has.value);
        let source_ids = self.search_parameter(&has.source_type, &inner)?;
        if source_ids.is_empty() {
            return Ok(Vec::new());
        }
        self.index
            .referenced_targets(&has.source_type, &has.reference_param, &source_ids, resource_type)
            .map_err(|e| e.to_string())
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

/// Candidate sets up to this size restrict the remaining parameters of a
/// search to themselves (a temp table of ids); larger ones aren't worth it.
const SCOPE_MAX: usize = 20_000;

/// Order in which to evaluate a search's parameters: the kinds that usually
/// narrow most go first. Only the order changes — the result is the same AND.
fn selectivity_rank(p: &SearchParameter) -> u8 {
    match (p.name.as_str(), p.modifier.as_deref(), &p.param_type) {
        // Negations and presence tests read the whole type: last.
        (_, Some("not" | "missing"), _) => 6,
        ("_text" | "_content", _, _) => 5,
        ("_id", _, _) => 0,
        (_, _, SearchParamType::Reference) => 1,
        ("identifier", _, _) => 1,
        (_, _, SearchParamType::Token) => 2,
        (_, _, SearchParamType::String) => 3,
        (_, _, SearchParamType::Date | SearchParamType::Number | SearchParamType::Quantity) => 4,
    }
}

/// Ends an index scope however the search returns (including `?`).
struct ScopeGuard<'a> {
    index: &'a V1Index,
    active: &'a std::cell::Cell<bool>,
}

impl Drop for ScopeGuard<'_> {
    fn drop(&mut self) {
        if self.active.get() {
            self.index.clear_scope();
        }
    }
}


/// The parameter at the end of a chain or `_has` (`family:exact=Doe`): its
/// modifier split off the name, its prefix off the value. With the modifier
/// left in the name, no parameter matched and the search was empty.
pub(crate) fn terminal_parameter(param: &str, param_type: &SearchParamType, value: &str) -> SearchParameter {
    let (name, modifier) = match param.split_once(':') {
        Some((n, m)) => (n.to_string(), Some(m.to_string())),
        None => (param.to_string(), None),
    };
    let (prefix, value) = if modifier.as_deref() == Some("missing") {
        (None, value.to_string())
    } else {
        split_prefix(param_type, value)
    };
    SearchParameter { name, value, modifier, prefix, param_type: param_type.clone() }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Narrowing (selectivity order + candidate scope) must not change any
    /// result: compare multi-parameter searches with a brute-force filter.
    #[test]
    fn narrowed_searches_match_brute_force() {
        use crate::{resource_text, IndexBuilder, IndexUpdate};
        use sazare_core::SearchParamRegistry;
        let registry = SearchParamRegistry::new();
        let store = SqliteStore::open(":memory:").unwrap();
        let index = SearchIndex::open(":memory:").unwrap();
        let mut all = Vec::new();
        let mut updates = Vec::new();
        for i in 0..300 {
            let id = format!("o{i:03}");
            let mut obs = serde_json::json!({
                "resourceType": "Observation", "id": id,
                "status": if i % 3 == 0 { "final" } else { "preliminary" },
                "code": {"coding": [{"system": "http://loinc.org", "code": format!("c{}", i % 7)}]},
                "subject": {"reference": format!("Patient/p{}", i % 10)},
                "effectiveDateTime": format!("20{:02}-01-01", 10 + i % 12)
            });
            if i % 5 == 0 {
                obs.as_object_mut().unwrap().remove("status");
            }
            store.put_with_version("Observation", &id, "1", obs.to_string().as_bytes()).unwrap();
            updates.push(IndexUpdate {
                resource_type: "Observation".into(),
                resource_id: id.clone(),
                entries: IndexBuilder::extract_indices_with_registry(&registry, "Observation", &obs),
                text: resource_text(&obs),
            });
            all.push(obs);
        }
        index.replace_many(&updates).unwrap();
        let exec = SearchExecutor::new(&store, &index);

        let field = |o: &Value, f: &str| o.pointer(f).and_then(|v| v.as_str()).map(str::to_string);
        let check = |q: &str, keep: &dyn Fn(&Value) -> bool| {
            let query = SearchQuery::parse_with_registry(q, Some("Observation"), &registry).unwrap();
            let got = exec.search("Observation", &query).unwrap();
            let mut want: Vec<String> = all.iter().filter(|o| keep(o)).map(|o| field(o, "/id").unwrap()).collect();
            want.sort();
            assert_eq!(got, want, "{q}");
        };
        let subj = |o: &Value| field(o, "/subject/reference");
        let code = |o: &Value| field(o, "/code/coding/0/code");
        let status = |o: &Value| field(o, "/status");
        let year = |o: &Value| field(o, "/effectiveDateTime").map(|d| d[..4].parse::<i32>().unwrap()).unwrap();

        check("code=c3&patient=p4", &|o| code(o).as_deref() == Some("c3") && subj(o).as_deref() == Some("Patient/p4"));
        check("subject=Patient/p4&code=c3", &|o| code(o).as_deref() == Some("c3") && subj(o).as_deref() == Some("Patient/p4"));
        check("patient=p4&status:not=final", &|o| subj(o).as_deref() == Some("Patient/p4") && status(o).as_deref() != Some("final"));
        check("patient=p4&status:missing=true", &|o| subj(o).as_deref() == Some("Patient/p4") && status(o).is_none());
        check("patient=p4&status:missing=false", &|o| subj(o).as_deref() == Some("Patient/p4") && status(o).is_some());
        check("patient=p2&date=ge2015", &|o| subj(o).as_deref() == Some("Patient/p2") && year(o) >= 2015);
        check("code=c1,c2&patient=p1,p3&status=preliminary", &|o| {
            matches!(code(o).as_deref(), Some("c1" | "c2"))
                && matches!(subj(o).as_deref(), Some("Patient/p1" | "Patient/p3"))
                && status(o).as_deref() == Some("preliminary")
        });
        check("_id=o007&code=c0", &|o| field(o, "/id").as_deref() == Some("o007") && code(o).as_deref() == Some("c0"));
        // Scope doesn't leak into the next search.
        check("code=c3", &|o| code(o).as_deref() == Some("c3"));
    }

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
    fn test_sort_keys_direction_missing_and_ties() {
        let store = SqliteStore::open(":memory:").unwrap();
        let index = SearchIndex::open(":memory:").unwrap();
        for id in ["p1", "p2", "p3", "p4"] {
            put(&store, "Patient", id, serde_json::json!({"resourceType":"Patient","id":id}));
            index.add_index("Patient", id, "_id", "token", Some(id), None).unwrap();
        }
        // p4 has no birthdate; p1 and p3 share one.
        index.add_index("Patient", "p1", "birthdate", "date", Some("1980-05-01"), None).unwrap();
        index.add_index("Patient", "p2", "birthdate", "date", Some("1970-01-01"), None).unwrap();
        index.add_index("Patient", "p3", "birthdate", "date", Some("1980-05-01"), None).unwrap();
        // Multi-valued string: p1 has "Zed" and "Abe".
        index.add_index("Patient", "p1", "family", "string", Some("zed"), None).unwrap();
        index.add_index("Patient", "p1", "family", "string", Some("abe"), None).unwrap();
        index.add_index("Patient", "p2", "family", "string", Some("moe"), None).unwrap();
        index.add_index("Patient", "p3", "family", "string", Some("bob"), None).unwrap();
        let exec = SearchExecutor::new(&store, &index);
        let run = |q: &str| exec.search("Patient", &SearchQuery::parse_for_resource(q, Some("Patient")).unwrap()).unwrap();

        // Ascending; tie (p1, p3) broken by id; missing (p4) last.
        assert_eq!(run("_sort=birthdate"), vec!["p2", "p1", "p3", "p4"]);
        // Descending; missing still last.
        assert_eq!(run("_sort=-birthdate"), vec!["p1", "p3", "p2", "p4"]);
        // Second key breaks the tie.
        assert_eq!(run("_sort=-birthdate,-family"), vec!["p1", "p3", "p2", "p4"]);
        assert_eq!(run("_sort=-birthdate,family"), vec!["p1", "p3", "p2", "p4"]);
        // Multi-valued: ascending uses p1's lowest ("abe"), descending its highest ("zed").
        assert_eq!(run("_sort=family"), vec!["p1", "p3", "p2", "p4"]);
        assert_eq!(run("_sort=-family"), vec!["p1", "p2", "p3", "p4"]);
        // Sorting happens before pagination.
        assert_eq!(run("_sort=-_id&_count=2"), vec!["p4", "p3"]);
        assert_eq!(run("_sort=-_id&_count=2&_offset=2"), vec!["p2", "p1"]);
    }

    #[test]
    fn test_missing_and_not_modifiers() {
        let store = SqliteStore::open(":memory:").unwrap();
        let index = SearchIndex::open(":memory:").unwrap();
        put(&store, "Patient", "p1", serde_json::json!({"resourceType":"Patient","id":"p1"}));
        put(&store, "Patient", "p2", serde_json::json!({"resourceType":"Patient","id":"p2"}));
        put(&store, "Patient", "p3", serde_json::json!({"resourceType":"Patient","id":"p3"}));
        // Every stored resource is indexed (its _id at least), as the server does.
        for id in ["p1", "p2", "p3"] {
            index.add_index("Patient", id, "_id", "token", Some(id), None).unwrap();
        }
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
