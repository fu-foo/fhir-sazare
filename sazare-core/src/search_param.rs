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

/// A chained search parameter: `subject:Patient.name=Doe`
#[derive(Debug, Clone)]
pub struct ChainParameter {
    /// The reference parameter on the source resource (e.g. "subject")
    pub reference_param: String,
    /// The target resource type (e.g. "Patient")
    pub target_type: String,
    /// The search parameter on the target resource (e.g. "name")
    pub target_param: String,
    /// The search value (e.g. "Doe")
    pub value: String,
    /// Inferred type of the target parameter
    pub target_param_type: SearchParamType,
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

            // Parse parameter name and modifier
            let (param_name, modifier) = if let Some(idx) = key.find(':') {
                let (name, mod_part) = key.split_at(idx);
                (name.to_string(), Some(mod_part[1..].to_string()))
            } else {
                (key.to_string(), None)
            };

            // Detect chain search: modifier contains "." (e.g. "Patient.name")
            if let Some(ref mod_str) = modifier
                && let Some(dot_idx) = mod_str.find('.')
            {
                let target_type = mod_str[..dot_idx].to_string();
                let target_param = mod_str[dot_idx + 1..].to_string();
                if !target_type.is_empty() && !target_param.is_empty() {
                    let target_param_type = infer_param_type(&target_param);
                    chain_parameters.push(ChainParameter {
                        reference_param: param_name,
                        target_type,
                        target_param: target_param.clone(),
                        value: value.to_string(),
                        target_param_type,
                    });
                    continue;
                }
            }

            // Infer parameter type from name
            let param_type = infer_param_type(&param_name);

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
        | "priority" | "requisition" => SearchParamType::Token,
        "name" | "family" | "given" | "address" => SearchParamType::String,
        "birthdate" | "date" | "period" => SearchParamType::Date,
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
        assert_eq!(chain.reference_param, "subject");
        assert_eq!(chain.target_type, "Patient");
        assert_eq!(chain.target_param, "name");
        assert_eq!(chain.value, "Doe");
        assert_eq!(chain.target_param_type, SearchParamType::String);
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
    fn test_infer_param_type() {
        assert_eq!(infer_param_type("identifier"), SearchParamType::Token);
        assert_eq!(infer_param_type("family"), SearchParamType::String);
        assert_eq!(infer_param_type("birthdate"), SearchParamType::Date);
        assert_eq!(infer_param_type("patient"), SearchParamType::Reference);
    }
}
