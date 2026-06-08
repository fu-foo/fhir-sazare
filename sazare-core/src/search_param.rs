/// FHIR search query parsed from HTTP query parameters
#[derive(Debug, Clone)]
pub struct SearchQuery {
    pub parameters: Vec<SearchParameter>,
    pub chain_parameters: Vec<ChainParameter>,
    pub include: Vec<String>,
    pub revinclude: Vec<String>,
    pub count: Option<usize>,
    pub offset: Option<usize>,
    pub summary: Option<SummaryMode>,
    pub elements: Vec<String>,
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
        let mut parameters = Vec::new();
        let mut chain_parameters = Vec::new();
        let mut include = Vec::new();
        let mut revinclude = Vec::new();
        let mut count = None;
        let mut offset = None;
        let mut summary = None;
        let mut elements = Vec::new();

        if query_string.is_empty() {
            return Ok(Self {
                parameters,
                chain_parameters,
                include,
                revinclude,
                count,
                offset,
                summary,
                elements,
            });
        }

        // Parse query parameters
        for pair in query_string.split('&') {
            let parts: Vec<&str> = pair.splitn(2, '=').collect();
            if parts.len() != 2 {
                continue;
            }

            let key = urlencoding::decode(parts[0]).map_err(|e| e.to_string())?;
            let value = urlencoding::decode(parts[1]).map_err(|e| e.to_string())?;

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

            if key == "_elements" {
                elements = value.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
                continue;
            }

            // Skip other standard result parameters that start with "_"
            // (e.g. _sort, _total, _contained, _containedType)
            // These are not search filters and should be ignored if unsupported.
            // Allowlist underscore-prefixed params that ARE search filters.
            const UNDERSCORE_SEARCH_PARAMS: &[&str] =
                &["_id", "_lastUpdated", "_profile", "_tag", "_security"];
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
                && let Some(chain) = parse_chain(&param_name, mod_str, &value)
            {
                chain_parameters.push(chain);
                continue;
            }

            // Infer parameter type from name (registry-aware when resource_type is provided)
            let param_type = infer_param_type_for_resource(resource_type, &param_name);

            // Parse date prefix (ge, le, gt, lt, eq)
            let (prefix, actual_value) = if param_type == SearchParamType::Date {
                parse_date_prefix(&value)
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
            include,
            revinclude,
            count,
            offset,
            summary,
            elements,
        })
    }

    /// Get all parameters with a specific name
    pub fn get_params(&self, name: &str) -> Vec<&SearchParameter> {
        self.parameters.iter().filter(|p| p.name == name).collect()
    }
}

/// Parse a (possibly multi-level) chained search parameter.
///
/// `reference_param` is the first reference param (left of the first `:`), and
/// `modifier` is everything after it, e.g. `Patient.organization:Organization.name`.
/// Each hop has the shape `<TargetType>.<rest>` where `rest` is either another
/// hop (`<ref>:<Type>.…`) or the terminal search param.
fn parse_chain(reference_param: &str, modifier: &str, value: &str) -> Option<ChainParameter> {
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
        links.push(ChainLink { reference_param: current_ref, target_type });
        return Some(ChainParameter {
            links,
            target_param: after.to_string(),
            value: value.to_string(),
            target_param_type: infer_param_type(after),
        });
    }
}

/// Parse date prefix from value (ge2020-01-01 -> (Some("ge"), "2020-01-01"))
fn parse_date_prefix(value: &str) -> (Option<String>, String) {
    let prefixes = ["ge", "le", "gt", "lt", "eq"];
    for prefix in &prefixes {
        if let Some(rest) = value.strip_prefix(prefix) {
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
    use crate::search_param_registry::SearchParamRegistry;

    static DEFAULT_REGISTRY: std::sync::LazyLock<SearchParamRegistry> =
        std::sync::LazyLock::new(SearchParamRegistry::new);

    // Try registry lookup if resource_type is provided
    if let Some(rt) = resource_type
        && let Some(pt) = DEFAULT_REGISTRY.lookup_param_type(rt, name)
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
        let query = SearchQuery::parse("_sort=name").unwrap();
        assert_eq!(query.parameters.len(), 0);
    }

    #[test]
    fn test_infer_param_type() {
        assert_eq!(infer_param_type("identifier"), SearchParamType::Token);
        assert_eq!(infer_param_type("family"), SearchParamType::String);
        assert_eq!(infer_param_type("birthdate"), SearchParamType::Date);
        assert_eq!(infer_param_type("patient"), SearchParamType::Reference);
    }
}
