use crate::search_param_registry::SearchParamRegistry;

/// FHIR search query parsed from HTTP query parameters
#[derive(Debug, Clone)]
pub struct SearchQuery {
    pub parameters: Vec<SearchParameter>,
    pub chain_parameters: Vec<ChainParameter>,
    pub has_parameters: Vec<HasParameter>,
    pub include: Vec<String>,
    pub revinclude: Vec<String>,
    pub count: Option<usize>,
    pub offset: Option<usize>,
    pub summary: Option<SummaryMode>,
    pub elements: Vec<String>,
    /// `_sort` keys in priority order.
    pub sort: Vec<SortKey>,
    /// `_total=none` — leave `Bundle.total` out (`estimate`/`accurate` and
    /// absence all include it).
    pub omit_total: bool,
}

/// One `_sort` key: a search parameter name (or `_id` / `_lastUpdated`),
/// descending when written with a leading `-`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SortKey {
    pub name: String,
    pub descending: bool,
}

/// A reverse-chained `_has` search parameter (one level).
///
/// `Patient?_has:Observation:patient:code=1234-5` reads as "Patients that are
/// referenced by an Observation whose `code` is 1234-5". It is the mirror image
/// of a forward chain: instead of filtering the searched type by a property of
/// what it points to, it filters the searched type by a property of resources
/// that point *at* it.
#[derive(Debug, Clone)]
pub struct HasParameter {
    /// The resource type that holds the back-reference (e.g. "Observation").
    pub source_type: String,
    /// The reference parameter on the source pointing back at the searched type
    /// (e.g. "patient").
    pub reference_param: String,
    /// The search parameter applied to the source type (e.g. "code").
    pub target_param: String,
    /// The value the source's `target_param` must match.
    pub value: String,
    /// Inferred type of `target_param` on the source resource.
    pub target_param_type: SearchParamType,
}

/// A chained search parameter. One level: `subject:Patient.name=Doe`.
/// Multi-level: `subject:Patient.organization:Organization.name=Acme` — the
/// `links` walk references outward from the source resource, and the terminal
/// `target_param`/`value` apply to the final target type.
#[derive(Debug, Clone)]
pub struct ChainParameter {
    /// Reference hops from the source resource outward (always at least one).
    pub links: Vec<ChainLink>,
    /// The search parameter on the final target resource (e.g. "name")
    pub target_param: String,
    /// The search value (e.g. "Doe")
    pub value: String,
    /// Inferred type of the terminal parameter
    pub target_param_type: SearchParamType,
}

/// One reference hop within a chained search.
#[derive(Debug, Clone)]
pub struct ChainLink {
    /// The reference parameter on the current resource (e.g. "subject")
    pub reference_param: String,
    /// The resource type it points to (e.g. "Patient")
    pub target_type: String,
}

/// _summary parameter modes
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SummaryMode {
    True,
    False,
    Text,
    Count,
    Data,
}

/// A single search parameter
#[derive(Debug, Clone)]
pub struct SearchParameter {
    pub name: String,
    pub value: String,
    pub modifier: Option<String>,
    pub prefix: Option<String>,  // For date searches: ge, le, gt, lt, eq
    pub param_type: SearchParamType,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SearchParamType {
    Token,    // identifier, code
    String,   // name, family
    Date,     // birthdate, date
    Reference, // subject, patient
    Number,   // _count, _offset
    Quantity, // value-quantity: [prefix]number[|system|code]
}

impl SearchQuery {
    /// Parse search query from URL query string
    pub fn parse(query_string: &str) -> Result<Self, String> {
        Self::parse_for_resource(query_string, None)
    }

    /// Parse with resource_type context, allowing the registry to provide
    /// resource-specific param-type inference (e.g. PractitionerRole.specialty
    /// is Token, but the bare-name heuristic returns String).
    pub fn parse_for_resource(query_string: &str, resource_type: Option<&str>) -> Result<Self, String> {
        Self::parse_with_registry(query_string, resource_type, default_registry())
    }

    /// Parse against a specific registry — the server's runtime one, which
    /// also holds custom SearchParameters loaded from `searchparameters/`, so
    /// their declared type (date, token, …) drives prefix parsing and the
    /// index lookup instead of the name heuristic.
    pub fn parse_with_registry(
        query_string: &str,
        resource_type: Option<&str>,
        registry: &SearchParamRegistry,
    ) -> Result<Self, String> {
        let mut parameters = Vec::new();
        let mut chain_parameters = Vec::new();
        let mut has_parameters = Vec::new();
        let mut include = Vec::new();
        let mut revinclude = Vec::new();
        let mut count = None;
        let mut offset = None;
        let mut summary = None;
        let mut elements = Vec::new();
        let mut sort = Vec::new();
        let mut omit_total = false;

        if query_string.is_empty() {
            return Ok(Self {
                parameters,
                chain_parameters,
                has_parameters,
                include,
                revinclude,
                count,
                offset,
                summary,
                elements,
                sort,
                omit_total,
            });
        }

        // Parse query parameters
        for pair in query_string.split('&') {
            let parts: Vec<&str> = pair.splitn(2, '=').collect();
            if parts.len() != 2 {
                continue;
            }

            let key = decode_query_value(parts[0], false)?;
            // Kept raw so date values can be decoded with a literal `+`
            // once the parameter type is known (see `decode_query_value`).
            let raw_value = parts[1];
            let value = decode_query_value(raw_value, false)?;

            // Reverse chain: `_has:Type:reference-param:search-param=value`.
            // Parsed before the generic `_`-prefixed skip below so it isn't
            // dropped as an unsupported result parameter.
            if key.starts_with("_has:") {
                if let Some(has) = parse_has(&key, raw_value, registry) {
                    has_parameters.push(has);
                }
                continue;
            }

            // Handle special parameters
            if key == "_include" {
                include.push(value.to_string());
                continue;
            }

            if key == "_revinclude" {
                revinclude.push(value.to_string());
                continue;
            }

            if key == "_count" {
                count = value.parse().ok();
                continue;
            }

            if key == "_offset" {
                offset = value.parse().ok();
                continue;
            }

            if key == "_summary" {
                summary = match value.as_ref() {
                    "true" => Some(SummaryMode::True),
                    "false" => Some(SummaryMode::False),
                    "text" => Some(SummaryMode::Text),
                    "count" => Some(SummaryMode::Count),
                    "data" => Some(SummaryMode::Data),
                    _ => None,
                };
                continue;
            }

            if key == "_total" {
                omit_total = value.trim() == "none";
                continue;
            }

            if key == "_sort" {
                for k in value.split(',').map(str::trim).filter(|k| !k.is_empty()) {
                    let (name, descending) = match k.strip_prefix('-') {
                        Some(rest) => (rest, true),
                        None => (k, false),
                    };
                    sort.push(SortKey { name: name.to_string(), descending });
                }
                continue;
            }

            if key == "_elements" {
                elements = value.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
                continue;
            }

            // Skip other standard result parameters that start with "_"
            // (e.g. _sort, _total, _contained, _containedType)
            // These are not search filters and should be ignored if unsupported.
            // Allowlist underscore-prefixed params that ARE search filters.
            const UNDERSCORE_SEARCH_PARAMS: &[&str] =
                &["_id", "_lastUpdated", "_profile", "_tag", "_security", "_text", "_content"];
            if key.starts_with('_') && !UNDERSCORE_SEARCH_PARAMS.contains(&key.as_ref()) {
                continue;
            }

            // Parse parameter name and modifier
            let (param_name, modifier) = if let Some(idx) = key.find(':') {
                let (name, mod_part) = key.split_at(idx);
                (name.to_string(), Some(mod_part[1..].to_string()))
            } else {
                (key.to_string(), None)
            };

            // Detect chain search: modifier contains "." (e.g. "Patient.name",
            // or multi-level "Patient.organization:Organization.name").
            if let Some(ref mod_str) = modifier
                && mod_str.contains('.')
                && let Some(chain) = parse_chain(&param_name, mod_str, raw_value, registry)
            {
                // Each hop is a search plus a reference lookup; an unbounded
                // chain lets one request do unbounded work.
                if chain.links.len() > MAX_CHAIN_DEPTH {
                    return Err(format!(
                        "Chained search '{key}' has {} levels; at most {MAX_CHAIN_DEPTH} are supported",
                        chain.links.len()
                    ));
                }
                chain_parameters.push(chain);
                continue;
            }

            // Infer parameter type from name (registry-aware when resource_type is provided)
            let param_type = infer_with_registry(registry, resource_type, &param_name);

            // Parse date prefix (ge, le, gt, lt, eq)
            let (prefix, actual_value) = if param_type == SearchParamType::Date {
                parse_date_prefix(&decode_query_value(raw_value, true)?)
            } else if param_type == SearchParamType::Number {
                let (prefix, number) = parse_number_prefix(&value);
                // Reject at parse time (a 400), comma-separated alternatives
                // each checked (each with its own prefix); `:missing` takes
                // true/false instead.
                let alts = alternatives(&number);
                if modifier.as_deref() != Some("missing")
                    && (alts.is_empty() || alts.iter().any(|n| parse_number_prefix(n).1.trim().parse::<f64>().is_err()))
                {
                    return Err(format!("Invalid number '{value}' for parameter '{param_name}'"));
                }
                (prefix, number)
            } else if param_type == SearchParamType::Quantity {
                // `[prefix]number[|system|code]`: the number is checked here
                // (a 400), the system and code are matched by the index.
                // With pipes it is `number|system|code`: one pipe (`5.4|mg`,
                // a token habit) would silently match nothing.
                let (prefix, rest) = parse_number_prefix(&value);
                let alts = alternatives(&rest);
                if modifier.as_deref() != Some("missing")
                    && (alts.is_empty() || alts.iter().any(|q| {
                        let q = parse_number_prefix(q).1;
                        !matches!(q.matches('|').count(), 0 | 2) || q.split('|').next().unwrap_or("").trim().parse::<f64>().is_err()
                    }))
                {
                    return Err(format!(
                        "Invalid quantity '{value}' for parameter '{param_name}': expected [prefix]number, number|system|code or number||code"
                    ));
                }
                (prefix, rest)
            } else {
                (None, value.to_string())
            };

            parameters.push(SearchParameter {
                name: param_name,
                value: actual_value,
                modifier,
                prefix,
                param_type,
            });
        }

        Ok(Self {
            parameters,
            chain_parameters,
            has_parameters,
            include,
            revinclude,
            count,
            offset,
            summary,
            elements,
            sort,
            omit_total,
        })
    }

    /// Get all parameters with a specific name
    pub fn get_params(&self, name: &str) -> Vec<&SearchParameter> {
        self.parameters.iter().filter(|p| p.name == name).collect()
    }
}

/// Most reference hops a chained search may take
/// (`subject:Patient.organization:Organization.partof:Organization.name` is 3).
pub const MAX_CHAIN_DEPTH: usize = 4;

/// Parse a (possibly multi-level) chained search parameter.
///
/// `reference_param` is the first reference param (left of the first `:`), and
/// `modifier` is everything after it, e.g. `Patient.organization:Organization.name`.
/// Each hop has the shape `<TargetType>.<rest>` where `rest` is either another
/// hop (`<ref>:<Type>.…`) or the terminal search param.
fn parse_chain(
    reference_param: &str,
    modifier: &str,
    raw_value: &str,
    registry: &SearchParamRegistry,
) -> Option<ChainParameter> {
    let mut links = Vec::new();
    let mut current_ref = reference_param.to_string();
    let mut rest = modifier;

    loop {
        // Each hop starts with the target type, then '.'.
        let dot = rest.find('.')?;
        let target_type = rest[..dot].to_string();
        if target_type.is_empty() {
            return None;
        }
        let after = &rest[dot + 1..];

        // `after` is a further hop iff it contains "<ref>:<Type>." — i.e. a ':'
        // whose suffix still has a '.'. Otherwise it is the terminal param (a
        // bare param name, possibly with its own ':modifier').
        if let Some(colon) = after.find(':') {
            let post = &after[colon + 1..];
            if post.contains('.') {
                let next_ref = after[..colon].to_string();
                if next_ref.is_empty() {
                    return None;
                }
                links.push(ChainLink { reference_param: current_ref, target_type });
                current_ref = next_ref;
                rest = post;
                continue;
            }
        }

        if after.is_empty() {
            return None;
        }
        // The terminal param belongs to the last hop's target type; look it
        // up there (without any `:modifier`) so custom params resolve too.
        let terminal_name = after.split(':').next().unwrap_or(after);
        let target_param_type = registry
            .lookup_param_type(&target_type, terminal_name)
            .unwrap_or_else(|| infer_param_type(after));
        let value =
            decode_query_value(raw_value, target_param_type == SearchParamType::Date).ok()?;
        links.push(ChainLink { reference_param: current_ref, target_type });
        return Some(ChainParameter {
            links,
            target_param: after.to_string(),
            value,
            target_param_type,
        });
    }
}

/// Parse a one-level `_has` parameter key of the form
/// `_has:<SourceType>:<reference-param>:<search-param>`.
///
/// Nested reverse chains (`_has:…:_has:…`) are not yet supported and return
/// `None` so the parameter is ignored rather than mis-parsed.
fn parse_has(key: &str, raw_value: &str, registry: &SearchParamRegistry) -> Option<HasParameter> {
    // key == "_has:Observation:patient:code"
    let rest = key.strip_prefix("_has:")?;
    let parts: Vec<&str> = rest.split(':').collect();
    if parts.len() != 3 {
        return None;
    }
    let source_type = parts[0];
    let reference_param = parts[1];
    let target_param = parts[2];
    if source_type.is_empty() || reference_param.is_empty() || target_param.is_empty() {
        return None;
    }
    // Nested `_has` (target_param == "_has") is out of scope for now.
    if target_param == "_has" {
        return None;
    }
    let target_param_type = infer_with_registry(registry, Some(source_type), target_param);
    let value = decode_query_value(raw_value, target_param_type == SearchParamType::Date).ok()?;
    Some(HasParameter {
        source_type: source_type.to_string(),
        reference_param: reference_param.to_string(),
        target_param: target_param.to_string(),
        value,
        target_param_type,
    })
}

/// Percent-decode one query-string key or value. Per
/// `application/x-www-form-urlencoded` (what HTML forms, `curl
/// --data-urlencode` and POST `_search` bodies produce), `+` means a space.
///
/// Date values are the exception: they never contain spaces, but clients
/// often send a timezone offset unencoded (`2024-01-01T10:00:00+09:00`), so
/// for them a `+` is kept literal. `%2B` always decodes to `+`.
fn decode_query_value(raw: &str, plus_is_literal: bool) -> Result<String, String> {
    let spaced;
    let input = if plus_is_literal {
        raw
    } else {
        spaced = raw.replace('+', " ");
        &spaced
    };
    urlencoding::decode(input).map(|s| s.into_owned()).map_err(|e| e.to_string())
}

/// Parse a number prefix (`gt5.4` → (Some("gt"), "5.4")); defaults to `eq`.
/// A prefix only counts if a number follows (digit, sign or decimal point).
pub fn parse_number_prefix(value: &str) -> (Option<String>, String) {
    const PREFIXES: [&str; 9] = ["eq", "ne", "gt", "lt", "ge", "le", "sa", "eb", "ap"];
    for prefix in PREFIXES {
        if let Some(rest) = value.strip_prefix(prefix)
            && rest.chars().next().is_some_and(|c| c.is_ascii_digit() || matches!(c, '-' | '+' | '.'))
        {
            return (Some(prefix.to_string()), rest.to_string());
        }
    }
    (Some("eq".to_string()), value.to_string())
}

/// The comma-separated alternatives of a value (OR), trimmed, empty ones left out.
fn alternatives(value: &str) -> Vec<&str> {
    value.split(',').map(str::trim).filter(|v| !v.is_empty()).collect()
}

/// The alternatives of a date, number or quantity parameter, each with its own
/// prefix: the first's was taken off at parse time (`p.prefix`), the others
/// still carry theirs (`ge2020,lt2019`, `gt140,90` — the second is `eq`).
/// Any other parameter: its alternatives, no prefix.
pub fn prefixed_alternatives(p: &SearchParameter) -> Vec<(Option<String>, String)> {
    // Positions before dropping empty ones: in `,lt90` the `lt90` is second
    // and carries its own prefix.
    p.value
        .split(',')
        .map(str::trim)
        .enumerate()
        .filter(|(_, v)| !v.is_empty())
        .map(|(i, v)| match (&p.param_type, i) {
            (SearchParamType::Date | SearchParamType::Number | SearchParamType::Quantity, 0) => {
                (p.prefix.clone(), v.to_string())
            }
            (SearchParamType::Date, _) => parse_date_prefix(v),
            (SearchParamType::Number | SearchParamType::Quantity, _) => parse_number_prefix(v),
            _ => (None, v.to_string()),
        })
        .collect()
}

/// Parse date prefix from value (ge2020-01-01 -> (Some("ge"), "2020-01-01")).
/// Recognizes all FHIR comparator prefixes: eq, ne, gt, lt, ge, le, sa, eb, ap.
pub fn parse_date_prefix(value: &str) -> (Option<String>, String) {
    const PREFIXES: [&str; 9] = ["eq", "ne", "gt", "lt", "ge", "le", "sa", "eb", "ap"];
    // A prefix only counts if what follows looks like a date (starts with a
    // digit), so a literal value that happens to start with these letters isn't
    // misparsed.
    for prefix in PREFIXES {
        if let Some(rest) = value.strip_prefix(prefix)
            && rest.chars().next().is_some_and(|c| c.is_ascii_digit())
        {
            return (Some(prefix.to_string()), rest.to_string());
        }
    }
    (Some("eq".to_string()), value.to_string())
}

/// Infer search parameter type from parameter name (backward-compatible, no resource context)
fn infer_param_type(name: &str) -> SearchParamType {
    infer_param_type_for_resource(None, name)
}

/// Infer search parameter type, optionally using resource-specific registry definitions.
/// Falls back to name-based heuristics if no registry match is found.
pub fn infer_param_type_for_resource(resource_type: Option<&str>, name: &str) -> SearchParamType {
    infer_with_registry(default_registry(), resource_type, name)
}

/// The built-in registry (no custom SearchParameters), for callers without a
/// runtime registry at hand.
fn default_registry() -> &'static SearchParamRegistry {
    static DEFAULT_REGISTRY: std::sync::LazyLock<SearchParamRegistry> =
        std::sync::LazyLock::new(SearchParamRegistry::new);
    &DEFAULT_REGISTRY
}

fn infer_with_registry(
    registry: &SearchParamRegistry,
    resource_type: Option<&str>,
    name: &str,
) -> SearchParamType {
    // Try registry lookup if resource_type is provided
    if let Some(rt) = resource_type
        && let Some(pt) = registry.lookup_param_type(rt, name)
    {
        return pt;
    }

    // Fallback: name-based heuristics
    match name {
        "identifier" | "code" | "status" | "gender" | "intent"
        | "vaccine-code" | "clinical-status" | "type" | "category"
        | "priority" | "requisition"
        | "_id" | "_profile" | "_tag" | "_security" => SearchParamType::Token,
        "name" | "family" | "given" | "address" => SearchParamType::String,
        "birthdate" | "date" | "period" | "_lastUpdated" => SearchParamType::Date,
        "subject" | "patient" | "encounter" | "owner"
        | "requester" => SearchParamType::Reference,
        _ => SearchParamType::String,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_empty_query() {
        let query = SearchQuery::parse("").unwrap();
        assert_eq!(query.parameters.len(), 0);
    }

    #[test]
    fn test_parse_simple_param() {
        let query = SearchQuery::parse("family=Smith").unwrap();
        assert_eq!(query.parameters.len(), 1);
        assert_eq!(query.parameters[0].name, "family");
        assert_eq!(query.parameters[0].value, "Smith");
        assert_eq!(query.parameters[0].param_type, SearchParamType::String);
    }

    #[test]
    fn test_parse_with_modifier() {
        let query = SearchQuery::parse("name:exact=John").unwrap();
        assert_eq!(query.parameters.len(), 1);
        assert_eq!(query.parameters[0].name, "name");
        assert_eq!(query.parameters[0].value, "John");
        assert_eq!(query.parameters[0].modifier, Some("exact".to_string()));
    }

    #[test]
    fn test_parse_multiple_params() {
        let query = SearchQuery::parse("family=Smith&given=John").unwrap();
        assert_eq!(query.parameters.len(), 2);
    }

    #[test]
    fn test_parse_include() {
        let query = SearchQuery::parse("family=Smith&_include=Patient:organization").unwrap();
        assert_eq!(query.parameters.len(), 1);
        assert_eq!(query.include.len(), 1);
        assert_eq!(query.include[0], "Patient:organization");
    }

    #[test]
    fn test_parse_count_offset() {
        let query = SearchQuery::parse("_count=10&_offset=20").unwrap();
        assert_eq!(query.count, Some(10));
        assert_eq!(query.offset, Some(20));
    }

    #[test]
    fn test_parse_summary() {
        let query = SearchQuery::parse("_summary=true").unwrap();
        assert_eq!(query.summary, Some(SummaryMode::True));

        let query = SearchQuery::parse("_summary=count").unwrap();
        assert_eq!(query.summary, Some(SummaryMode::Count));

        let query = SearchQuery::parse("_summary=data").unwrap();
        assert_eq!(query.summary, Some(SummaryMode::Data));
    }

    #[test]
    fn test_parse_elements() {
        let query = SearchQuery::parse("_elements=identifier,name,birthDate").unwrap();
        assert_eq!(query.elements, vec!["identifier", "name", "birthDate"]);
    }

    #[test]
    fn test_parse_elements_empty() {
        let query = SearchQuery::parse("family=Smith").unwrap();
        assert!(query.elements.is_empty());
        assert_eq!(query.summary, None);
    }

    #[test]
    fn test_parse_chain_param() {
        let query = SearchQuery::parse("subject:Patient.name=Doe").unwrap();
        assert_eq!(query.parameters.len(), 0);
        assert_eq!(query.chain_parameters.len(), 1);

        let chain = &query.chain_parameters[0];
        assert_eq!(chain.links.len(), 1);
        assert_eq!(chain.links[0].reference_param, "subject");
        assert_eq!(chain.links[0].target_type, "Patient");
        assert_eq!(chain.target_param, "name");
        assert_eq!(chain.value, "Doe");
        assert_eq!(chain.target_param_type, SearchParamType::String);
    }

    #[test]
    fn test_parse_chain_multi_level() {
        // Observation -> subject -> Patient -> organization -> Organization.name
        let query =
            SearchQuery::parse("subject:Patient.organization:Organization.name=Acme").unwrap();
        assert_eq!(query.chain_parameters.len(), 1);
        let chain = &query.chain_parameters[0];
        assert_eq!(chain.links.len(), 2);
        assert_eq!(chain.links[0].reference_param, "subject");
        assert_eq!(chain.links[0].target_type, "Patient");
        assert_eq!(chain.links[1].reference_param, "organization");
        assert_eq!(chain.links[1].target_type, "Organization");
        assert_eq!(chain.target_param, "name");
        assert_eq!(chain.value, "Acme");
    }

    #[test]
    fn test_parse_chain_three_level() {
        let query = SearchQuery::parse(
            "patient:Patient.organization:Organization.partof:Organization.name=Health",
        )
        .unwrap();
        let chain = &query.chain_parameters[0];
        assert_eq!(chain.links.len(), 3);
        assert_eq!(chain.links[2].reference_param, "partof");
        assert_eq!(chain.links[2].target_type, "Organization");
        assert_eq!(chain.target_param, "name");
    }

    #[test]
    fn test_parse_has_param() {
        // Patients referenced by an Observation whose code is 1234-5.
        let query = SearchQuery::parse("_has:Observation:patient:code=1234-5").unwrap();
        assert_eq!(query.has_parameters.len(), 1);
        let has = &query.has_parameters[0];
        assert_eq!(has.source_type, "Observation");
        assert_eq!(has.reference_param, "patient");
        assert_eq!(has.target_param, "code");
        assert_eq!(has.value, "1234-5");
        assert_eq!(has.target_param_type, SearchParamType::Token);
        // It is not mistaken for a regular or chain parameter.
        assert!(query.parameters.is_empty());
        assert!(query.chain_parameters.is_empty());
    }

    #[test]
    fn test_parse_has_with_regular_params() {
        let query = SearchQuery::parse("gender=male&_has:Observation:patient:code=1234-5").unwrap();
        assert_eq!(query.parameters.len(), 1);
        assert_eq!(query.parameters[0].name, "gender");
        assert_eq!(query.has_parameters.len(), 1);
        assert_eq!(query.has_parameters[0].source_type, "Observation");
    }

    #[test]
    fn test_parse_has_malformed_ignored() {
        // Missing the search-param segment -> not a valid one-level _has.
        let query = SearchQuery::parse("_has:Observation:patient=x").unwrap();
        assert!(query.has_parameters.is_empty());
        // Nested _has is out of scope for now and is dropped, not mis-parsed.
        let nested = SearchQuery::parse("_has:Group:member:_has=y").unwrap();
        assert!(nested.has_parameters.is_empty());
    }

    #[test]
    fn test_parse_chain_depth_is_limited() {
        // 4 hops: accepted.
        let ok = "subject:Patient.organization:Organization.partof:Organization.partof:Organization.name=x";
        let q = SearchQuery::parse_for_resource(ok, Some("Observation")).unwrap();
        assert_eq!(q.chain_parameters[0].links.len(), 4);
        // 5 hops: rejected with a clear message.
        let deep = "subject:Patient.organization:Organization.partof:Organization.partof:Organization.partof:Organization.name=x";
        let err = SearchQuery::parse_for_resource(deep, Some("Observation")).unwrap_err();
        assert!(err.contains("at most 4"), "{err}");
    }

    #[test]
    fn test_parse_chain_with_regular_params() {
        let query = SearchQuery::parse("status=final&subject:Patient.gender=male").unwrap();
        assert_eq!(query.parameters.len(), 1);
        assert_eq!(query.parameters[0].name, "status");
        assert_eq!(query.chain_parameters.len(), 1);
        assert_eq!(query.chain_parameters[0].target_param, "gender");
    }

    #[test]
    fn test_modifier_not_chain() {
        // name:exact is a modifier, not a chain
        let query = SearchQuery::parse("name:exact=John").unwrap();
        assert_eq!(query.parameters.len(), 1);
        assert_eq!(query.chain_parameters.len(), 0);
        assert_eq!(query.parameters[0].modifier, Some("exact".to_string()));
    }

    #[test]
    fn test_parse_profile() {
        let query = SearchQuery::parse("_profile=http://example.org/StructureDefinition/A").unwrap();
        assert_eq!(query.parameters.len(), 1);
        assert_eq!(query.parameters[0].name, "_profile");
        assert_eq!(query.parameters[0].value, "http://example.org/StructureDefinition/A");
        assert_eq!(query.parameters[0].param_type, SearchParamType::Token);
    }

    #[test]
    fn test_parse_common_fhir_params() {
        let query = SearchQuery::parse(
            "_id=abc&_tag=http://x|t1&_security=http://x|s1&_lastUpdated=ge2024-01-01"
        ).unwrap();
        assert_eq!(query.parameters.len(), 4);
        let names: Vec<&str> = query.parameters.iter().map(|p| p.name.as_str()).collect();
        assert!(names.contains(&"_id"));
        assert!(names.contains(&"_tag"));
        assert!(names.contains(&"_security"));
        assert!(names.contains(&"_lastUpdated"));
        let last_updated = query.parameters.iter().find(|p| p.name == "_lastUpdated").unwrap();
        assert_eq!(last_updated.param_type, SearchParamType::Date);
        assert_eq!(last_updated.prefix.as_deref(), Some("ge"));
        assert_eq!(last_updated.value, "2024-01-01");
    }

    #[test]
    fn test_parse_unknown_underscore_param_skipped() {
        let query = SearchQuery::parse("_contained=true").unwrap();
        assert_eq!(query.parameters.len(), 0);
        assert!(SearchQuery::parse("_total=none").unwrap().omit_total);
        assert!(!SearchQuery::parse("_total=accurate").unwrap().omit_total);
    }

    #[test]
    fn test_parse_sort_keys() {
        let query = SearchQuery::parse("_sort=-date,status,_id").unwrap();
        assert_eq!(query.parameters.len(), 0);
        assert_eq!(
            query.sort,
            vec![
                SortKey { name: "date".into(), descending: true },
                SortKey { name: "status".into(), descending: false },
                SortKey { name: "_id".into(), descending: false },
            ]
        );
    }

    #[test]
    fn test_parse_all_date_prefixes() {
        for (p, raw) in [
            ("ne", "ne2024-01-01"),
            ("sa", "sa2024-01-01"),
            ("eb", "eb2024-01-01"),
            ("ap", "ap2024-01-01"),
            ("gt", "gt2024-01-01"),
        ] {
            let (prefix, val) = parse_date_prefix(raw);
            assert_eq!(prefix.as_deref(), Some(p));
            assert_eq!(val, "2024-01-01");
        }
        // No prefix → defaults to eq, value untouched.
        let (prefix, val) = parse_date_prefix("2024-01-01");
        assert_eq!(prefix.as_deref(), Some("eq"));
        assert_eq!(val, "2024-01-01");
    }

    #[test]
    fn test_number_prefix_parsing() {
        assert_eq!(parse_number_prefix("gt5.4"), (Some("gt".into()), "5.4".into()));
        assert_eq!(parse_number_prefix("lt-1"), (Some("lt".into()), "-1".into()));
        assert_eq!(parse_number_prefix("ap.5"), (Some("ap".into()), ".5".into()));
        assert_eq!(parse_number_prefix("100"), (Some("eq".into()), "100".into()));
        // Not a prefix unless a number follows.
        assert_eq!(parse_number_prefix("gtx"), (Some("eq".into()), "gtx".into()));
    }

    #[test]
    fn test_invalid_number_value_is_a_parse_error() {
        let mut reg = SearchParamRegistry::new();
        reg.register_search_parameter(&serde_json::json!({
            "resourceType": "SearchParameter", "code": "probability",
            "base": ["RiskAssessment"], "type": "number",
            "expression": "RiskAssessment.prediction.probability.ofType(decimal)"
        }))
        .unwrap();
        let parse = |q: &str| SearchQuery::parse_with_registry(q, Some("RiskAssessment"), &reg);
        assert!(parse("probability=abc").is_err());
        assert!(parse("probability=gt0.2,abc").is_err());
        let q = parse("probability=gt0.2").unwrap();
        assert_eq!(q.parameters[0].param_type, SearchParamType::Number);
        assert_eq!(q.parameters[0].prefix.as_deref(), Some("gt"));
        assert!(parse("probability:missing=true").is_ok());
    }

    #[test]
    fn test_plus_decodes_as_space_except_in_dates() {
        // Form encoding: `+` is a space (curl --data-urlencode, HTML forms).
        let q = SearchQuery::parse_for_resource("family:exact=van+Dyke", Some("Patient")).unwrap();
        assert_eq!(q.parameters[0].value, "van Dyke");
        // An encoded plus stays a plus.
        let q = SearchQuery::parse_for_resource("code=A%2BB", Some("Observation")).unwrap();
        assert_eq!(q.parameters[0].value, "A+B");
        // Dates keep a literal `+` (unencoded timezone offset).
        let q = SearchQuery::parse_for_resource("date=ge2024-01-01T10:00:00+09:00", Some("Observation")).unwrap();
        assert_eq!(q.parameters[0].prefix.as_deref(), Some("ge"));
        assert_eq!(q.parameters[0].value, "2024-01-01T10:00:00+09:00");
        // Same rules for chain and _has targets.
        let q = SearchQuery::parse_for_resource("subject:Patient.name=van+Dyke", Some("Observation")).unwrap();
        assert_eq!(q.chain_parameters[0].value, "van Dyke");
        let q = SearchQuery::parse_for_resource(
            "_has:Observation:subject:date=ge2024-01-01T10:00:00+09:00", Some("Patient"),
        ).unwrap();
        assert_eq!(q.has_parameters[0].value, "ge2024-01-01T10:00:00+09:00");
        let q = SearchQuery::parse_for_resource("_has:Observation:subject:code=a+b", Some("Patient")).unwrap();
        assert_eq!(q.has_parameters[0].value, "a b");
    }

    #[test]
    fn test_parse_with_registry_uses_custom_param_types() {
        let mut reg = SearchParamRegistry::new();
        reg.register_search_parameter(&serde_json::json!({
            "resourceType": "SearchParameter", "code": "jp-medication-start",
            "base": ["MedicationRequest"], "type": "date",
            "expression": "MedicationRequest.authoredOn"
        }))
        .unwrap();
        reg.register_search_parameter(&serde_json::json!({
            "resourceType": "SearchParameter", "code": "jp-insurance-organizationno",
            "base": ["Organization"], "type": "token",
            "expression": "Organization.identifier"
        }))
        .unwrap();

        let q = SearchQuery::parse_with_registry(
            "jp-medication-start=ge2020-01-01", Some("MedicationRequest"), &reg,
        ).unwrap();
        assert_eq!(q.parameters[0].param_type, SearchParamType::Date);
        assert_eq!(q.parameters[0].prefix.as_deref(), Some("ge"));
        assert_eq!(q.parameters[0].value, "2020-01-01");

        let q = SearchQuery::parse_with_registry(
            "jp-insurance-organizationno=sys|123", Some("Organization"), &reg,
        ).unwrap();
        assert_eq!(q.parameters[0].param_type, SearchParamType::Token);

        // Reverse chain and chain resolve the custom type on their target type.
        let q = SearchQuery::parse_with_registry(
            "_has:MedicationRequest:subject:jp-medication-start=ge2020-01-01", Some("Patient"), &reg,
        ).unwrap();
        assert_eq!(q.has_parameters[0].target_param_type, SearchParamType::Date);
        let q = SearchQuery::parse_with_registry(
            "performer:Organization.jp-insurance-organizationno=123", Some("Observation"), &reg,
        ).unwrap();
        assert_eq!(q.chain_parameters[0].target_param_type, SearchParamType::Token);

        // Without the runtime registry the name heuristic falls back to string.
        let q = SearchQuery::parse_for_resource("jp-medication-start=ge2020-01-01", Some("MedicationRequest")).unwrap();
        assert_eq!(q.parameters[0].param_type, SearchParamType::String);
    }

    #[test]
    fn test_infer_param_type() {
        assert_eq!(infer_param_type("identifier"), SearchParamType::Token);
        assert_eq!(infer_param_type("family"), SearchParamType::String);
        assert_eq!(infer_param_type("birthdate"), SearchParamType::Date);
        assert_eq!(infer_param_type("patient"), SearchParamType::Reference);
    }

    #[test]
    fn number_and_quantity_alternatives_keep_their_prefixes() {
        let q = SearchQuery::parse_with_registry("value-quantity=,lt90", Some("Observation"), &crate::SearchParamRegistry::new()).unwrap();
        assert_eq!(prefixed_alternatives(&q.parameters[0]), [(Some("lt".to_string()), "90".to_string())]);
        let q = SearchQuery::parse_with_registry("value-quantity=gt140,90", Some("Observation"), &crate::SearchParamRegistry::new()).unwrap();
        assert_eq!(
            prefixed_alternatives(&q.parameters[0]),
            [(Some("gt".to_string()), "140".to_string()), (Some("eq".to_string()), "90".to_string())]
        );
        for bad in ["value-quantity=", "value-quantity=,,", "value-quantity=5.4|mg", "value-quantity=gt5,lt"] {
            assert!(SearchQuery::parse_with_registry(bad, Some("Observation"), &crate::SearchParamRegistry::new()).is_err(), "{bad}");
        }
        assert!(SearchQuery::parse_with_registry("value-quantity:missing=true", Some("Observation"), &crate::SearchParamRegistry::new()).is_ok());
    }

}
