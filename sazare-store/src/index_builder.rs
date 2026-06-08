use sazare_core::search_param_registry::{ExtractionMode, SearchParamDef, SearchParamRegistry};
use serde_json::Value;

/// Extract search indices from a FHIR resource
pub struct IndexBuilder;

impl IndexBuilder {
    /// Extract all searchable indices from a resource using a registry.
    /// Returns Vec<(param_name, param_type, value, system)>
    pub fn extract_indices_with_registry(
        registry: &SearchParamRegistry,
        resource_type: &str,
        resource: &Value,
    ) -> Vec<(String, String, String, Option<String>)> {
        let mut indices = Vec::new();
        let defs = registry.get_definitions(resource_type);
        for def in defs {
            Self::extract_by_definition(resource, def, &mut indices);
        }
        indices
    }

    /// Extract all searchable indices using a default registry (backward compatible).
    /// Returns Vec<(param_name, param_type, value, system)>
    pub fn extract_indices(
        resource_type: &str,
        resource: &Value,
    ) -> Vec<(String, String, String, Option<String>)> {
        static DEFAULT_REGISTRY: std::sync::LazyLock<SearchParamRegistry> =
            std::sync::LazyLock::new(SearchParamRegistry::new);
        Self::extract_indices_with_registry(&DEFAULT_REGISTRY, resource_type, resource)
    }

    /// Extract indices according to a single SearchParamDef
    fn extract_by_definition(
        resource: &Value,
        def: &SearchParamDef,
        indices: &mut Vec<(String, String, String, Option<String>)>,
    ) {
        let param_type_str = match def.param_type {
            sazare_core::SearchParamType::Token => "token",
            sazare_core::SearchParamType::String => "string",
            sazare_core::SearchParamType::Date => "date",
            sazare_core::SearchParamType::Reference => "reference",
            sazare_core::SearchParamType::Number => "number",
        };

        match def.extraction {
            ExtractionMode::Simple => {
                Self::extract_simple(resource, &def.path, &def.name, param_type_str, &def.aliases, indices);
            }
            ExtractionMode::ArrayField => {
                Self::extract_array_field(resource, &def.path, &def.name, param_type_str, indices);
            }
            ExtractionMode::NestedArrayScalar => {
                Self::extract_nested_array_scalar(resource, &def.path, &def.name, param_type_str, indices);
            }
            ExtractionMode::CodeableConcept => {
                Self::extract_codeable_concept(resource, &def.path, &def.name, param_type_str, &def.aliases, indices);
            }
            ExtractionMode::Identifier => {
                Self::extract_identifier(resource, &def.path, &def.name, indices);
            }
            ExtractionMode::Reference => {
                Self::extract_reference(resource, &def.path, &def.name, param_type_str, &def.aliases, indices);
            }
            ExtractionMode::PeriodStart => {
                Self::extract_period_start(resource, &def.path, &def.name, param_type_str, indices);
            }
            ExtractionMode::UriArray => {
                Self::extract_uri_array(resource, &def.path, &def.name, param_type_str, indices);
            }
            ExtractionMode::CodingArray => {
                Self::extract_coding_array(resource, &def.path, &def.name, param_type_str, indices);
            }
            ExtractionMode::Coding => {
                Self::extract_coding(resource, &def.path, &def.name, param_type_str, &def.aliases, indices);
            }
            ExtractionMode::Address => {
                Self::extract_address(resource, &def.path, &def.name, param_type_str, indices);
            }
            ExtractionMode::NestedCodeableConcept => {
                Self::extract_nested_codeable_concept(resource, &def.path, &def.name, param_type_str, &def.aliases, indices);
            }
            ExtractionMode::NestedScalar => {
                Self::extract_nested_scalar(resource, &def.path, &def.name, param_type_str, indices);
            }
            ExtractionMode::NestedReference => {
                Self::extract_nested_reference(resource, &def.path, &def.name, param_type_str, &def.aliases, indices);
            }
            ExtractionMode::PeriodRange => {
                Self::extract_period_range(resource, &def.path, &def.name, param_type_str, indices);
            }
            ExtractionMode::ExtensionDate => {
                Self::extract_extension_date(resource, &def.path, &def.name, param_type_str, indices);
            }
            ExtractionMode::JpNameRepresentation => {
                Self::extract_jp_name_by_representation(resource, &def.path, &def.name, param_type_str, indices);
            }
            ExtractionMode::JpExtensionValue => {
                Self::extract_jp_extension_value(resource, &def.path, &def.name, param_type_str, indices);
            }
            ExtractionMode::JpDosagePeriodStart => {
                Self::extract_jp_dosage_period_start(resource, &def.path, &def.name, param_type_str, indices);
            }
        }
    }

    /// JpExtensionValue: index a top-level extension (`path[0]`) identified by
    /// URL (`path[1]`) whose value kind is `path[2]` (`string` / `coding` /
    /// `identifier`). Strings are indexed lowercased; codings and identifiers
    /// are indexed as tokens (code/value + system).
    fn extract_jp_extension_value(
        resource: &Value,
        path: &[String],
        name: &str,
        param_type: &str,
        indices: &mut Vec<(String, String, String, Option<String>)>,
    ) {
        let (url, kind) = match (path.get(1), path.get(2)) {
            (Some(u), Some(k)) => (u.as_str(), k.as_str()),
            _ => return,
        };
        let Some(exts) = resource.get(path[0].as_str()).and_then(|v| v.as_array()) else {
            return;
        };
        for ext in exts.iter().filter(|e| {
            e.get("url").and_then(|u| u.as_str()) == Some(url)
        }) {
            match kind {
                "string" => {
                    if let Some(s) = ext.get("valueString").and_then(|v| v.as_str()) {
                        indices.push((name.to_string(), param_type.to_string(), s.to_lowercase(), None));
                    }
                }
                "coding" => {
                    if let Some(coding) = ext.get("valueCoding") {
                        if let Some(code) = coding.get("code").and_then(|v| v.as_str()) {
                            let system = coding.get("system").and_then(|v| v.as_str()).map(str::to_string);
                            indices.push((name.to_string(), param_type.to_string(), code.to_string(), system));
                        }
                    }
                }
                "identifier" => {
                    if let Some(id) = ext.get("valueIdentifier") {
                        if let Some(value) = id.get("value").and_then(|v| v.as_str()) {
                            let system = id.get("system").and_then(|v| v.as_str()).map(str::to_string);
                            indices.push((name.to_string(), param_type.to_string(), value.to_string(), system));
                        }
                    }
                }
                _ => {}
            }
        }
    }

    /// JpDosagePeriodStart: index the `valuePeriod.start` of the extension
    /// (`path[1]`) carried under each `MedicationRequest.dosageInstruction`
    /// (`path[0]`), as a date.
    fn extract_jp_dosage_period_start(
        resource: &Value,
        path: &[String],
        name: &str,
        param_type: &str,
        indices: &mut Vec<(String, String, String, Option<String>)>,
    ) {
        let url = match path.get(1) {
            Some(u) => u.as_str(),
            None => return,
        };
        let Some(dosages) = resource.get(path[0].as_str()).and_then(|v| v.as_array()) else {
            return;
        };
        for dosage in dosages {
            let Some(exts) = dosage.get("extension").and_then(|v| v.as_array()) else {
                continue;
            };
            for ext in exts.iter().filter(|e| {
                e.get("url").and_then(|u| u.as_str()) == Some(url)
            }) {
                if let Some(start) = ext
                    .get("valuePeriod")
                    .and_then(|p| p.get("start"))
                    .and_then(|v| v.as_str())
                {
                    indices.push((name.to_string(), param_type.to_string(), start.to_string(), None));
                }
            }
        }
    }

    /// JpNameRepresentation: index the `text`, `family` and `given` of HumanName
    /// entries (`path[0]`, e.g. `name`) whose `iso21090-EN-representation`
    /// extension equals the code in `path[1]` (`SYL` for kana, `IDE` for kanji).
    /// Enables search by Japanese phonetic or ideographic name.
    fn extract_jp_name_by_representation(
        resource: &Value,
        path: &[String],
        name: &str,
        param_type: &str,
        indices: &mut Vec<(String, String, String, Option<String>)>,
    ) {
        const REPRESENTATION_URL: &str =
            "http://hl7.org/fhir/StructureDefinition/iso21090-EN-representation";

        let representation = match path.get(1) {
            Some(code) => code.as_str(),
            None => return,
        };
        let Some(array) = resource.get(path[0].as_str()).and_then(|v| v.as_array()) else {
            return;
        };
        for human_name in array {
            let matches_representation = human_name
                .get("extension")
                .and_then(|v| v.as_array())
                .map(|exts| {
                    exts.iter().any(|e| {
                        e.get("url").and_then(|u| u.as_str()) == Some(REPRESENTATION_URL)
                            && e.get("valueCode").and_then(|c| c.as_str()) == Some(representation)
                    })
                })
                .unwrap_or(false);
            if !matches_representation {
                continue;
            }

            let mut push = |s: &str| {
                indices.push((name.to_string(), param_type.to_string(), s.to_lowercase(), None));
            };
            if let Some(text) = human_name.get("text").and_then(|v| v.as_str()) {
                push(text);
            }
            if let Some(family) = human_name.get("family").and_then(|v| v.as_str()) {
                push(family);
            }
            if let Some(givens) = human_name.get("given").and_then(|v| v.as_array()) {
                for g in givens.iter().filter_map(|v| v.as_str()) {
                    push(g);
                }
            }
        }
    }

    /// ExtensionDate: find the extension whose `url` equals `path[1]` under the
    /// container `path[0]` (typically "extension") and index its `valueDateTime`.
    fn extract_extension_date(
        resource: &Value,
        path: &[String],
        name: &str,
        param_type: &str,
        indices: &mut Vec<(String, String, String, Option<String>)>,
    ) {
        if path.len() < 2 {
            return;
        }
        let Some(extensions) = resource.get(path[0].as_str()).and_then(|v| v.as_array()) else {
            return;
        };
        for ext in extensions {
            if ext.get("url").and_then(|v| v.as_str()) == Some(path[1].as_str())
                && let Some(dt) = ext.get("valueDateTime").and_then(|v| v.as_str())
            {
                indices.push((name.to_string(), param_type.to_string(), dt.to_string(), None));
            }
        }
    }

    /// PeriodRange: navigate to a Period object and emit a `"start/end"` value so
    /// the store can index the full date range. Omits the trailing endpoint when
    /// the Period is open-ended (`"start/"`).
    fn extract_period_range(
        resource: &Value,
        path: &[String],
        name: &str,
        param_type: &str,
        indices: &mut Vec<(String, String, String, Option<String>)>,
    ) {
        let mut current = resource;
        for segment in path {
            match current.get(segment.as_str()) {
                Some(v) => current = v,
                None => return,
            }
        }
        let start = current.get("start").and_then(|v| v.as_str());
        let end = current.get("end").and_then(|v| v.as_str());
        let value = match (start, end) {
            (Some(s), Some(e)) => format!("{}/{}", s, e),
            (Some(s), None) => format!("{}/", s),
            (None, Some(e)) => format!("/{}", e),
            (None, None) => return,
        };
        indices.push((name.to_string(), param_type.to_string(), value, None));
    }

    /// NestedScalar: `path[0]` is an array (e.g. `target`), `path[1]` a scalar
    /// field on each element (e.g. `dueDate`).
    fn extract_nested_scalar(
        resource: &Value,
        path: &[String],
        name: &str,
        param_type: &str,
        indices: &mut Vec<(String, String, String, Option<String>)>,
    ) {
        if path.len() < 2 {
            return;
        }
        let Some(elements) = resource.get(path[0].as_str()).and_then(|v| v.as_array()) else {
            return;
        };
        for element in elements {
            if let Some(s) = element.get(path[1].as_str()).and_then(|v| v.as_str()) {
                let value = if param_type == "string" { s.to_lowercase() } else { s.to_string() };
                indices.push((name.to_string(), param_type.to_string(), value, None));
            }
        }
    }

    /// NestedReference: `path[0]` is an array (e.g. `location`), `path[1]` a
    /// Reference field on each element (e.g. `location`). Indexes the full
    /// reference and the bare resource id, mirroring `extract_reference`.
    fn extract_nested_reference(
        resource: &Value,
        path: &[String],
        name: &str,
        param_type: &str,
        aliases: &[String],
        indices: &mut Vec<(String, String, String, Option<String>)>,
    ) {
        if path.len() < 2 {
            return;
        }
        let Some(elements) = resource.get(path[0].as_str()).and_then(|v| v.as_array()) else {
            return;
        };
        for element in elements {
            let Some(reference) = element
                .get(path[1].as_str())
                .and_then(|r| r.get("reference"))
                .and_then(|v| v.as_str())
            else {
                continue;
            };
            indices.push((name.to_string(), param_type.to_string(), reference.to_string(), None));
            for alias in aliases {
                indices.push((alias.to_string(), param_type.to_string(), reference.to_string(), None));
            }
            let trimmed = reference.split("/_history/").next().unwrap_or(reference);
            let bare = trimmed.rsplit('/').next().unwrap_or("");
            if !bare.is_empty() && bare != reference {
                indices.push((name.to_string(), param_type.to_string(), bare.to_string(), None));
                for alias in aliases {
                    indices.push((alias.to_string(), param_type.to_string(), bare.to_string(), None));
                }
            }
        }
    }

    /// Address: navigate to `path[0]` (an Address object or array of Addresses).
    /// If only the field segment is given, index every component as a string;
    /// if a component segment is also given (e.g. `["address", "city"]`), index
    /// just that component. `line` is an array of strings; all other components
    /// are scalars.
    fn extract_address(
        resource: &Value,
        path: &[String],
        name: &str,
        param_type: &str,
        indices: &mut Vec<(String, String, String, Option<String>)>,
    ) {
        let Some(field) = path.first() else { return };
        let Some(value) = resource.get(field.as_str()) else { return };
        // Normalize to a list of Address objects (Location.address is a single
        // object; Organization.address / Patient.address are arrays).
        let addresses: Vec<&Value> = if let Some(arr) = value.as_array() {
            arr.iter().collect()
        } else {
            vec![value]
        };

        let push = |s: &str, indices: &mut Vec<(String, String, String, Option<String>)>| {
            indices.push((name.to_string(), param_type.to_string(), s.to_lowercase(), None));
        };

        // Specific component requested (e.g. address-city).
        if let Some(component) = path.get(1) {
            for addr in &addresses {
                match addr.get(component.as_str()) {
                    Some(Value::String(s)) => push(s, indices),
                    Some(Value::Array(arr)) => {
                        for v in arr {
                            if let Some(s) = v.as_str() {
                                push(s, indices);
                            }
                        }
                    }
                    _ => {}
                }
            }
            return;
        }

        // Whole address: index every component.
        for addr in &addresses {
            if let Some(lines) = addr.get("line").and_then(|v| v.as_array()) {
                for line in lines {
                    if let Some(s) = line.as_str() {
                        push(s, indices);
                    }
                }
            }
            for component in ["city", "district", "state", "postalCode", "country", "text"] {
                if let Some(s) = addr.get(component).and_then(|v| v.as_str()) {
                    push(s, indices);
                }
            }
        }
    }

    /// NestedCodeableConcept: `path[0]` is an array (e.g. `participant`), `path[1]`
    /// is a CodeableConcept (or array of them) on each element (e.g. `role`).
    fn extract_nested_codeable_concept(
        resource: &Value,
        path: &[String],
        name: &str,
        param_type: &str,
        aliases: &[String],
        indices: &mut Vec<(String, String, String, Option<String>)>,
    ) {
        if path.len() < 2 {
            return;
        }
        let Some(elements) = resource.get(path[0].as_str()).and_then(|v| v.as_array()) else {
            return;
        };
        for element in elements {
            let Some(target) = element.get(path[1].as_str()) else { continue };
            let concepts: Vec<&Value> = if let Some(arr) = target.as_array() {
                arr.iter().collect()
            } else {
                vec![target]
            };
            for concept in concepts {
                if let Some(codings) = concept.get("coding").and_then(|v| v.as_array()) {
                    for coding in codings {
                        if let Some(code) = coding.get("code").and_then(|v| v.as_str()) {
                            let system = coding.get("system").and_then(|v| v.as_str()).map(|s| s.to_string());
                            indices.push((name.to_string(), param_type.to_string(), code.to_string(), system.clone()));
                            for alias in aliases {
                                indices.push((alias.to_string(), param_type.to_string(), code.to_string(), system.clone()));
                            }
                        }
                    }
                }
            }
        }
    }

    /// Coding: navigate path to a single Coding object (e.g. Encounter.class).
    /// Yields `code` + `system`.
    fn extract_coding(
        resource: &Value,
        path: &[String],
        name: &str,
        param_type: &str,
        aliases: &[String],
        indices: &mut Vec<(String, String, String, Option<String>)>,
    ) {
        if path.is_empty() {
            return;
        }
        let mut current = resource;
        for segment in path {
            match current.get(segment.as_str()) {
                Some(v) => current = v,
                None => return,
            }
        }
        if let Some(code) = current.get("code").and_then(|v| v.as_str()) {
            let system = current.get("system").and_then(|v| v.as_str()).map(|s| s.to_string());
            indices.push((name.to_string(), param_type.to_string(), code.to_string(), system.clone()));
            for alias in aliases {
                indices.push((alias.to_string(), param_type.to_string(), code.to_string(), system.clone()));
            }
        }
    }

    /// CodingArray: navigate path to an array of Coding objects (e.g. meta.tag).
    fn extract_coding_array(
        resource: &Value,
        path: &[String],
        name: &str,
        param_type: &str,
        indices: &mut Vec<(String, String, String, Option<String>)>,
    ) {
        let mut current = resource;
        for segment in path {
            match current.get(segment.as_str()) {
                Some(v) => current = v,
                None => return,
            }
        }
        if let Some(arr) = current.as_array() {
            for coding in arr {
                if let Some(code) = coding.get("code").and_then(|v| v.as_str()) {
                    let system = coding.get("system").and_then(|v| v.as_str()).map(|s| s.to_string());
                    indices.push((name.to_string(), param_type.to_string(), code.to_string(), system));
                }
            }
        }
    }

    /// UriArray: navigate path to an array of scalar strings (e.g. meta.profile).
    fn extract_uri_array(
        resource: &Value,
        path: &[String],
        name: &str,
        param_type: &str,
        indices: &mut Vec<(String, String, String, Option<String>)>,
    ) {
        let mut current = resource;
        for segment in path {
            match current.get(segment.as_str()) {
                Some(v) => current = v,
                None => return,
            }
        }
        if let Some(arr) = current.as_array() {
            for v in arr {
                if let Some(s) = v.as_str() {
                    indices.push((name.to_string(), param_type.to_string(), s.to_string(), None));
                }
            }
        }
    }

    /// Simple: navigate path to a scalar value
    fn extract_simple(
        resource: &Value,
        path: &[String],
        name: &str,
        param_type: &str,
        aliases: &[String],
        indices: &mut Vec<(String, String, String, Option<String>)>,
    ) {
        let mut current = resource;
        for segment in path {
            match current.get(segment.as_str()) {
                Some(v) => current = v,
                None => return,
            }
        }
        if let Some(s) = current.as_str() {
            let value = if param_type == "string" {
                s.to_lowercase()
            } else {
                s.to_string()
            };
            indices.push((name.to_string(), param_type.to_string(), value.clone(), None));
            for alias in aliases {
                indices.push((alias.to_string(), param_type.to_string(), value.clone(), None));
            }
        }
    }

    /// ArrayField: path[0] is an array, path[1] is a field in each element
    fn extract_array_field(
        resource: &Value,
        path: &[String],
        name: &str,
        param_type: &str,
        indices: &mut Vec<(String, String, String, Option<String>)>,
    ) {
        if path.len() < 2 {
            return;
        }
        if let Some(array) = resource.get(path[0].as_str()).and_then(|v| v.as_array()) {
            for item in array {
                if let Some(val) = item.get(path[1].as_str()).and_then(|v| v.as_str()) {
                    let value = if param_type == "string" {
                        val.to_lowercase()
                    } else {
                        val.to_string()
                    };
                    indices.push((name.to_string(), param_type.to_string(), value, None));
                }
            }
        }
    }

    /// NestedArrayScalar: path[0] is an array, path[1] is an array inside each element
    fn extract_nested_array_scalar(
        resource: &Value,
        path: &[String],
        name: &str,
        param_type: &str,
        indices: &mut Vec<(String, String, String, Option<String>)>,
    ) {
        if path.len() < 2 {
            return;
        }
        if let Some(outer) = resource.get(path[0].as_str()).and_then(|v| v.as_array()) {
            for item in outer {
                if let Some(inner) = item.get(path[1].as_str()).and_then(|v| v.as_array()) {
                    for val in inner {
                        if let Some(s) = val.as_str() {
                            let value = if param_type == "string" {
                                s.to_lowercase()
                            } else {
                                s.to_string()
                            };
                            indices.push((name.to_string(), param_type.to_string(), value, None));
                        }
                    }
                }
            }
        }
    }

    /// CodeableConcept: navigate to path, then iterate coding[] for code+system
    fn extract_codeable_concept(
        resource: &Value,
        path: &[String],
        name: &str,
        param_type: &str,
        aliases: &[String],
        indices: &mut Vec<(String, String, String, Option<String>)>,
    ) {
        if path.is_empty() {
            return;
        }
        let mut current = resource;
        for segment in path {
            match current.get(segment.as_str()) {
                Some(v) => current = v,
                None => return,
            }
        }
        // CodeableConcept may be a single object with "coding" or an array of CodeableConcepts
        let concepts = if current.is_array() {
            current.as_array().unwrap().iter().collect::<Vec<_>>()
        } else {
            vec![current]
        };
        for concept in concepts {
            if let Some(codings) = concept.get("coding").and_then(|v| v.as_array()) {
                for coding in codings {
                    if let Some(code_value) = coding.get("code").and_then(|v| v.as_str()) {
                        let system = coding.get("system").and_then(|v| v.as_str()).map(|s| s.to_string());
                        indices.push((name.to_string(), param_type.to_string(), code_value.to_string(), system.clone()));
                        for alias in aliases {
                            indices.push((alias.to_string(), param_type.to_string(), code_value.to_string(), system.clone()));
                        }
                    }
                }
            }
        }
    }

    /// Identifier: navigate to path, extract value+system from each element.
    /// Handles both array (e.g. `identifier`) and single object (e.g. `requisition`).
    fn extract_identifier(
        resource: &Value,
        path: &[String],
        name: &str,
        indices: &mut Vec<(String, String, String, Option<String>)>,
    ) {
        if path.is_empty() {
            return;
        }
        let mut current = resource;
        for segment in path {
            match current.get(segment.as_str()) {
                Some(v) => current = v,
                None => return,
            }
        }
        if let Some(identifiers) = current.as_array() {
            for identifier in identifiers {
                if let Some(value) = identifier.get("value").and_then(|v| v.as_str()) {
                    let system = identifier.get("system").and_then(|v| v.as_str()).map(|s| s.to_string());
                    indices.push((name.to_string(), "token".to_string(), value.to_string(), system));
                }
            }
        } else if current.is_object() {
            // Single Identifier object (e.g. ServiceRequest.requisition)
            if let Some(value) = current.get("value").and_then(|v| v.as_str()) {
                let system = current.get("system").and_then(|v| v.as_str()).map(|s| s.to_string());
                indices.push((name.to_string(), "token".to_string(), value.to_string(), system));
            }
        }
    }

    /// Reference: navigate to path, then get .reference field.
    /// Handles both single Reference objects (e.g. Observation.subject) and
    /// arrays of References (e.g. Provenance.target).
    /// Indexes both the full reference and the bare resource id so FHIR clients
    /// can search using either form (`?patient=Patient/123` or `?patient=123`).
    fn extract_reference(
        resource: &Value,
        path: &[String],
        name: &str,
        param_type: &str,
        aliases: &[String],
        indices: &mut Vec<(String, String, String, Option<String>)>,
    ) {
        if path.is_empty() {
            return;
        }
        let mut current = resource;
        for segment in path {
            match current.get(segment.as_str()) {
                Some(v) => current = v,
                None => return,
            }
        }
        // Accept either a single Reference object or an array of References.
        let refs: Vec<&Value> = if current.is_array() {
            current.as_array().unwrap().iter().collect()
        } else {
            vec![current]
        };
        for ref_obj in refs {
            let Some(reference) = ref_obj.get("reference").and_then(|v| v.as_str()) else {
                continue;
            };
            indices.push((name.to_string(), param_type.to_string(), reference.to_string(), None));
            for alias in aliases {
                indices.push((alias.to_string(), param_type.to_string(), reference.to_string(), None));
            }
            // Also index the bare resource id (last segment, ignoring /_history/...)
            // so `?patient=patient-example` matches "Patient/patient-example".
            let trimmed = reference.split("/_history/").next().unwrap_or(reference);
            let bare = trimmed.rsplit('/').next().unwrap_or("");
            if !bare.is_empty() && bare != reference {
                indices.push((name.to_string(), param_type.to_string(), bare.to_string(), None));
                for alias in aliases {
                    indices.push((alias.to_string(), param_type.to_string(), bare.to_string(), None));
                }
            }
        }
    }

    /// PeriodStart: navigate to first path segment, then get .start (or second segment)
    fn extract_period_start(
        resource: &Value,
        path: &[String],
        name: &str,
        param_type: &str,
        indices: &mut Vec<(String, String, String, Option<String>)>,
    ) {
        if path.is_empty() {
            return;
        }
        let mut current = resource;
        for segment in path {
            match current.get(segment.as_str()) {
                Some(v) => current = v,
                None => return,
            }
        }
        if let Some(s) = current.as_str() {
            indices.push((name.to_string(), param_type.to_string(), s.to_string(), None));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_extract_patient_indices() {
        let patient = json!({
            "resourceType": "Patient",
            "identifier": [{"system": "urn:oid:1.2.3", "value": "12345"}],
            "name": [{"family": "Smith", "given": ["John"]}],
            "birthDate": "1990-01-01",
            "gender": "male"
        });

        let indices = IndexBuilder::extract_indices("Patient", &patient);
        assert!(indices.len() >= 4);

        assert!(indices.iter().any(|(name, _, _, _)| name == "identifier"));
        assert!(indices.iter().any(|(name, _, _, _)| name == "family"));
        assert!(indices.iter().any(|(name, _, _, _)| name == "given"));
        assert!(indices.iter().any(|(name, _, _, _)| name == "birthdate"));
        assert!(indices.iter().any(|(name, _, _, _)| name == "gender"));

        // Check system is captured
        let id_idx = indices.iter().find(|(name, _, _, _)| name == "identifier").unwrap();
        assert_eq!(id_idx.3, Some("urn:oid:1.2.3".to_string()));
    }

    #[test]
    fn test_extract_patient_name_combined() {
        // US Core `name` indexes against every HumanName component
        let patient = json!({
            "resourceType": "Patient",
            "name": [{
                "family": "Smith",
                "given": ["Amy", "V."],
                "prefix": ["Mrs."]
            }]
        });

        let indices = IndexBuilder::extract_indices("Patient", &patient);

        // String values are lowercased on extract
        assert!(indices.iter().any(|(n, _, v, _)| n == "name" && v == "smith"));
        assert!(indices.iter().any(|(n, _, v, _)| n == "name" && v == "amy"));
        assert!(indices.iter().any(|(n, _, v, _)| n == "name" && v == "v."));
        assert!(indices.iter().any(|(n, _, v, _)| n == "name" && v == "mrs."));

        // Existing family/given index entries still present
        assert!(indices.iter().any(|(n, _, v, _)| n == "family" && v == "smith"));
        assert!(indices.iter().any(|(n, _, v, _)| n == "given" && v == "amy"));
    }

    #[test]
    fn test_extract_service_request_based_on() {
        // A LabItem ServiceRequest pointing at its parent LabOrder via basedOn.
        let item = json!({
            "resourceType": "ServiceRequest",
            "status": "active",
            "intent": "order",
            "basedOn": [{"reference": "ServiceRequest/order-123"}]
        });

        let indices = IndexBuilder::extract_indices("ServiceRequest", &item);

        // Full reference and bare id are both indexed (powers _revinclude and
        // ?based-on=ServiceRequest/order-123 / ?based-on=order-123).
        assert!(indices
            .iter()
            .any(|(n, t, v, _)| n == "based-on" && t == "reference" && v == "ServiceRequest/order-123"));
        assert!(indices
            .iter()
            .any(|(n, _, v, _)| n == "based-on" && v == "order-123"));
    }

    #[test]
    fn test_extract_observation_indices() {
        let observation = json!({
            "resourceType": "Observation",
            "status": "final",
            "code": {"coding": [{"code": "8310-5"}]},
            "subject": {"reference": "Patient/123"}
        });

        let indices = IndexBuilder::extract_indices("Observation", &observation);
        assert!(indices.iter().any(|(name, _, _, _)| name == "status"));
        assert!(indices.iter().any(|(name, _, _, _)| name == "code"));
        assert!(indices.iter().any(|(name, _, _, _)| name == "subject"));
    }

    #[test]
    fn test_observation_subject_patient_alias() {
        let observation = json!({
            "resourceType": "Observation",
            "status": "final",
            "code": {"coding": [{"code": "8310-5"}]},
            "subject": {"reference": "Patient/123"}
        });

        let indices = IndexBuilder::extract_indices("Observation", &observation);
        assert!(indices.iter().any(|(name, _, _, _)| name == "patient"));
        let patient_idx = indices.iter().find(|(name, _, _, _)| name == "patient").unwrap();
        assert_eq!(patient_idx.2, "Patient/123");
    }

    #[test]
    fn test_extract_medication_request_indices() {
        let med_req = json!({
            "resourceType": "MedicationRequest",
            "status": "active",
            "intent": "order",
            "subject": {"reference": "Patient/456"},
            "identifier": [{"system": "http://example.org", "value": "MR-001"}]
        });

        let indices = IndexBuilder::extract_indices("MedicationRequest", &med_req);
        assert!(indices.iter().any(|(name, _, val, _)| name == "status" && val == "active"));
        assert!(indices.iter().any(|(name, _, val, _)| name == "intent" && val == "order"));
        assert!(indices.iter().any(|(name, _, val, _)| name == "subject" && val == "Patient/456"));
        assert!(indices.iter().any(|(name, _, val, _)| name == "patient" && val == "Patient/456"));
        assert!(indices.iter().any(|(name, _, val, _)| name == "identifier" && val == "MR-001"));
    }

    #[test]
    fn test_extract_task_indices() {
        let task = json!({
            "resourceType": "Task",
            "status": "in-progress",
            "for": {"reference": "Patient/789"},
            "owner": {"reference": "Practitioner/001"},
            "code": {"coding": [{"system": "http://example.org", "code": "fulfill"}]},
            "identifier": [{"value": "TASK-001"}]
        });

        let indices = IndexBuilder::extract_indices("Task", &task);
        assert!(indices.iter().any(|(name, _, val, _)| name == "status" && val == "in-progress"));
        assert!(indices.iter().any(|(name, _, val, _)| name == "subject" && val == "Patient/789"));
        assert!(indices.iter().any(|(name, _, val, _)| name == "patient" && val == "Patient/789"));
        assert!(indices.iter().any(|(name, _, val, _)| name == "owner" && val == "Practitioner/001"));
        assert!(indices.iter().any(|(name, _, val, _)| name == "code" && val == "fulfill"));
        assert!(indices.iter().any(|(name, _, val, _)| name == "identifier" && val == "TASK-001"));
    }

    #[test]
    fn test_reference_indexed_with_bare_id() {
        // FHIR clients search references as either "Patient/123" or "123";
        // both forms must be retrievable from the index.
        let observation = json!({
            "resourceType": "Observation",
            "status": "final",
            "code": {"coding": [{"code": "8310-5"}]},
            "subject": {"reference": "Patient/abc-123"}
        });

        let indices = IndexBuilder::extract_indices("Observation", &observation);

        // Full reference present
        assert!(indices.iter().any(|(n, _, v, _)| n == "subject" && v == "Patient/abc-123"));
        // Bare id also indexed under same param name
        assert!(indices.iter().any(|(n, _, v, _)| n == "subject" && v == "abc-123"));
        // Same for the alias
        assert!(indices.iter().any(|(n, _, v, _)| n == "patient" && v == "Patient/abc-123"));
        assert!(indices.iter().any(|(n, _, v, _)| n == "patient" && v == "abc-123"));
    }

    #[test]
    fn test_reference_with_history_strips_version() {
        // `Patient/123/_history/4` should still yield bare id "123", not "4".
        let observation = json!({
            "resourceType": "Observation",
            "status": "final",
            "code": {"coding": [{"code": "8310-5"}]},
            "subject": {"reference": "Patient/123/_history/4"}
        });

        let indices = IndexBuilder::extract_indices("Observation", &observation);
        assert!(indices.iter().any(|(n, _, v, _)| n == "subject" && v == "123"));
        // Should NOT extract "4" as a bare id
        assert!(!indices.iter().any(|(n, _, v, _)| n == "subject" && v == "4"));
    }

    #[test]
    fn test_reference_array_indexed() {
        // Provenance.target is an array of References; each element should be indexed.
        let provenance = json!({
            "resourceType": "Provenance",
            "target": [
                {"reference": "Patient/patient-example"},
                {"reference": "Encounter/enc-example"}
            ],
            "recorded": "2025-12-01T10:00:00Z"
        });

        let indices = IndexBuilder::extract_indices("Provenance", &provenance);
        // Both full and bare forms for both targets
        assert!(indices.iter().any(|(n, _, v, _)| n == "target" && v == "Patient/patient-example"));
        assert!(indices.iter().any(|(n, _, v, _)| n == "target" && v == "patient-example"));
        assert!(indices.iter().any(|(n, _, v, _)| n == "target" && v == "Encounter/enc-example"));
        assert!(indices.iter().any(|(n, _, v, _)| n == "target" && v == "enc-example"));
        // Alias `patient` works too
        assert!(indices.iter().any(|(n, _, v, _)| n == "patient" && v == "Patient/patient-example"));
    }

    #[test]
    fn test_reference_already_bare_no_duplicate() {
        // If the reference has no "/", the bare form equals the canonical form
        // and we should not push duplicate entries.
        let observation = json!({
            "resourceType": "Observation",
            "status": "final",
            "code": {"coding": [{"code": "8310-5"}]},
            "subject": {"reference": "abc-123"}
        });

        let indices = IndexBuilder::extract_indices("Observation", &observation);
        let subject_count = indices.iter().filter(|(n, _, v, _)| n == "subject" && v == "abc-123").count();
        assert_eq!(subject_count, 1, "bare-only reference must not be indexed twice");
    }

    #[test]
    fn test_extract_unknown_resource_common_indices() {
        // Unknown resource types fall back to FHIR-common params only (_profile).
        let resource = json!({
            "resourceType": "CustomResource",
            "meta": {"profile": ["http://example.org/StructureDefinition/Custom"]}
        });

        let indices = IndexBuilder::extract_indices("CustomResource", &resource);
        assert!(indices.iter().any(|(name, _, val, _)|
            name == "_profile" && val == "http://example.org/StructureDefinition/Custom"
        ));
    }

    #[test]
    fn test_extract_common_fhir_params() {
        let patient = json!({
            "resourceType": "Patient",
            "id": "p-1",
            "meta": {
                "lastUpdated": "2024-06-01T12:00:00Z",
                "tag": [{"system": "http://example.org/tag", "code": "vip"}],
                "security": [{"system": "http://hl7.org/fhir/v3/Confidentiality", "code": "R"}]
            }
        });

        let indices = IndexBuilder::extract_indices("Patient", &patient);
        assert!(indices.iter().any(|(n, _, v, _)| n == "_id" && v == "p-1"));
        assert!(indices.iter().any(|(n, _, v, _)| n == "_lastUpdated" && v == "2024-06-01T12:00:00Z"));
        assert!(indices.iter().any(|(n, _, v, s)|
            n == "_tag" && v == "vip" && s.as_deref() == Some("http://example.org/tag")
        ));
        assert!(indices.iter().any(|(n, _, v, s)|
            n == "_security" && v == "R" && s.as_deref() == Some("http://hl7.org/fhir/v3/Confidentiality")
        ));
    }

    #[test]
    fn test_extract_profile_on_registered_resource() {
        let sr = json!({
            "resourceType": "ServiceRequest",
            "status": "active",
            "intent": "order",
            "meta": {"profile": [
                "http://example.org/StructureDefinition/ServiceRequestA",
                "http://example.org/StructureDefinition/Common"
            ]}
        });

        let indices = IndexBuilder::extract_indices("ServiceRequest", &sr);
        assert!(indices.iter().any(|(name, _, val, _)|
            name == "_profile" && val == "http://example.org/StructureDefinition/ServiceRequestA"
        ));
        assert!(indices.iter().any(|(name, _, val, _)|
            name == "_profile" && val == "http://example.org/StructureDefinition/Common"
        ));
    }

    #[test]
    fn test_extract_encounter_period_start() {
        let encounter = json!({
            "resourceType": "Encounter",
            "status": "finished",
            "subject": {"reference": "Patient/123"},
            "period": {"start": "2024-01-15T10:00:00Z"}
        });

        let indices = IndexBuilder::extract_indices("Encounter", &encounter);
        assert!(indices.iter().any(|(name, _, val, _)| name == "date" && val == "2024-01-15T10:00:00Z"));
        assert!(indices.iter().any(|(name, _, val, _)| name == "patient" && val == "Patient/123"));
    }

    #[test]
    fn test_extract_encounter_class_type_identifier() {
        let encounter = json!({
            "resourceType": "Encounter",
            "status": "finished",
            "class": {
                "system": "http://terminology.hl7.org/CodeSystem/v3-ActCode",
                "code": "AMB"
            },
            "type": [{
                "coding": [{"system": "http://www.ama-assn.org/go/cpt", "code": "99213"}]
            }],
            "identifier": [{"system": "http://hospital.example/encs", "value": "ENC-1"}]
        });

        let indices = IndexBuilder::extract_indices("Encounter", &encounter);
        assert!(indices.iter().any(|(n, _, v, s)|
            n == "class" && v == "AMB" && s.as_deref() == Some("http://terminology.hl7.org/CodeSystem/v3-ActCode")
        ));
        assert!(indices.iter().any(|(n, _, v, _)| n == "type" && v == "99213"));
        assert!(indices.iter().any(|(n, _, v, _)| n == "identifier" && v == "ENC-1"));
    }

    #[test]
    fn test_extract_condition_category_status_onset() {
        let condition = json!({
            "resourceType": "Condition",
            "category": [{
                "coding": [{"system": "http://terminology.hl7.org/CodeSystem/condition-category", "code": "problem-list-item"}]
            }],
            "clinicalStatus": {
                "coding": [{"system": "http://terminology.hl7.org/CodeSystem/condition-clinical", "code": "active"}]
            },
            "subject": {"reference": "Patient/123"},
            "onsetDateTime": "2020-05-15"
        });

        let indices = IndexBuilder::extract_indices("Condition", &condition);
        assert!(indices.iter().any(|(n, _, v, _)| n == "category" && v == "problem-list-item"));
        assert!(indices.iter().any(|(n, _, v, _)| n == "clinical-status" && v == "active"));
        assert!(indices.iter().any(|(n, _, v, _)| n == "onset-date" && v == "2020-05-15"));
    }

    #[test]
    fn test_extract_patient_death_date() {
        let patient = json!({
            "resourceType": "Patient",
            "name": [{"family": "X"}],
            "deceasedDateTime": "2024-09-01"
        });
        let indices = IndexBuilder::extract_indices("Patient", &patient);
        assert!(indices.iter().any(|(n, _, v, _)| n == "death-date" && v == "2024-09-01"));
    }

    #[test]
    fn test_extract_observation_combo_code() {
        let observation = json!({
            "resourceType": "Observation",
            "status": "final",
            "code": {"coding": [{"system": "http://loinc.org", "code": "85354-9"}]},
            "subject": {"reference": "Patient/123"}
        });
        let indices = IndexBuilder::extract_indices("Observation", &observation);
        // combo-code mirrors `code` for top-level; component access tracked separately
        assert!(indices.iter().any(|(n, _, v, _)| n == "combo-code" && v == "85354-9"));
        assert!(indices.iter().any(|(n, _, v, _)| n == "code" && v == "85354-9"));
    }

    #[test]
    fn test_extract_immunization_indices() {
        let immunization = json!({
            "resourceType": "Immunization",
            "status": "completed",
            "patient": {"reference": "Patient/123"},
            "occurrenceDateTime": "2024-03-15",
            "vaccineCode": {"coding": [{"system": "http://hl7.org/fhir/sid/cvx", "code": "08"}]},
            "identifier": [{"value": "IMM-001"}]
        });

        let indices = IndexBuilder::extract_indices("Immunization", &immunization);
        assert!(indices.iter().any(|(name, _, val, _)| name == "status" && val == "completed"));
        assert!(indices.iter().any(|(name, _, val, _)| name == "patient" && val == "Patient/123"));
        assert!(indices.iter().any(|(name, _, val, _)| name == "date" && val == "2024-03-15"));
        assert!(indices.iter().any(|(name, _, val, _)| name == "vaccine-code" && val == "08"));
        assert!(indices.iter().any(|(name, _, val, _)| name == "identifier" && val == "IMM-001"));
    }

    #[test]
    fn test_extract_with_registry() {
        let registry = SearchParamRegistry::new();
        let patient = json!({
            "resourceType": "Patient",
            "identifier": [{"system": "urn:oid:1.2.3", "value": "12345"}],
            "name": [{"family": "Smith", "given": ["John"]}],
            "birthDate": "1990-01-01",
            "gender": "male"
        });

        let indices = IndexBuilder::extract_indices_with_registry(&registry, "Patient", &patient);
        assert!(indices.len() >= 4);
        assert!(indices.iter().any(|(name, _, _, _)| name == "family"));
    }

    #[test]
    fn test_extract_observation_category() {
        let observation = json!({
            "resourceType": "Observation",
            "status": "final",
            "category": [{
                "coding": [{
                    "system": "http://terminology.hl7.org/CodeSystem/observation-category",
                    "code": "laboratory"
                }]
            }],
            "code": {"coding": [{"code": "8310-5"}]},
            "subject": {"reference": "Patient/123"}
        });

        let indices = IndexBuilder::extract_indices("Observation", &observation);
        assert!(indices.iter().any(|(name, _, val, _)| name == "category" && val == "laboratory"));
    }

    #[test]
    fn test_extract_service_request_indices() {
        let sr = json!({
            "resourceType": "ServiceRequest",
            "status": "active",
            "intent": "order",
            "priority": "routine",
            "subject": {"reference": "Patient/123"},
            "encounter": {"reference": "Encounter/456"},
            "requester": {"reference": "Practitioner/789"},
            "code": {"coding": [{"system": "urn:oid:1.2.392.200119.4.504", "code": "3D010"}]},
            "identifier": [{"value": "SR-001"}],
            "requisition": {"system": "urn:demo:requisition", "value": "ORD-001"}
        });

        let indices = IndexBuilder::extract_indices("ServiceRequest", &sr);
        assert!(indices.iter().any(|(name, _, val, _)| name == "status" && val == "active"));
        assert!(indices.iter().any(|(name, _, val, _)| name == "intent" && val == "order"));
        assert!(indices.iter().any(|(name, _, val, _)| name == "priority" && val == "routine"));
        assert!(indices.iter().any(|(name, _, val, _)| name == "subject" && val == "Patient/123"));
        assert!(indices.iter().any(|(name, _, val, _)| name == "patient" && val == "Patient/123"));
        assert!(indices.iter().any(|(name, _, val, _)| name == "encounter" && val == "Encounter/456"));
        assert!(indices.iter().any(|(name, _, val, _)| name == "requester" && val == "Practitioner/789"));
        assert!(indices.iter().any(|(name, _, val, _)| name == "code" && val == "3D010"));
        assert!(indices.iter().any(|(name, _, val, _)| name == "identifier" && val == "SR-001"));
        // Single Identifier object (not array)
        assert!(indices.iter().any(|(name, _, val, sys)|
            name == "requisition" && val == "ORD-001" && *sys == Some("urn:demo:requisition".to_string())
        ));
    }

    #[test]
    fn test_extract_specimen_indices() {
        let specimen = json!({
            "resourceType": "Specimen",
            "status": "available",
            "subject": {"reference": "Patient/123"},
            "type": {"coding": [{"system": "http://terminology.hl7.org/CodeSystem/v2-0487", "code": "BLD"}]},
            "identifier": [{"value": "SP-001"}]
        });

        let indices = IndexBuilder::extract_indices("Specimen", &specimen);
        assert!(indices.iter().any(|(name, _, val, _)| name == "status" && val == "available"));
        assert!(indices.iter().any(|(name, _, val, _)| name == "subject" && val == "Patient/123"));
        assert!(indices.iter().any(|(name, _, val, _)| name == "patient" && val == "Patient/123"));
        assert!(indices.iter().any(|(name, _, val, _)| name == "type" && val == "BLD"));
        assert!(indices.iter().any(|(name, _, val, _)| name == "identifier" && val == "SP-001"));
    }

    #[test]
    fn test_extract_location_address() {
        // Location.address is a single Address object (not an array).
        let location = json!({
            "resourceType": "Location",
            "name": "Health Clinic",
            "address": {
                "line": ["100 Health Way"],
                "city": "Boston",
                "state": "MA",
                "postalCode": "02115",
                "country": "US"
            }
        });

        let indices = IndexBuilder::extract_indices("Location", &location);
        // Whole-address param matches any component (values are lowercased).
        assert!(indices.iter().any(|(n, t, v, _)| n == "address" && t == "string" && v == "boston"));
        assert!(indices.iter().any(|(n, _, v, _)| n == "address" && v == "ma"));
        assert!(indices.iter().any(|(n, _, v, _)| n == "address" && v == "100 health way"));
        // Granular params index only their component.
        assert!(indices.iter().any(|(n, _, v, _)| n == "address-city" && v == "boston"));
        assert!(indices.iter().any(|(n, _, v, _)| n == "address-state" && v == "ma"));
        assert!(indices.iter().any(|(n, _, v, _)| n == "address-postalcode" && v == "02115"));
        assert!(indices.iter().any(|(n, _, v, _)| n == "address-country" && v == "us"));
        assert!(!indices.iter().any(|(n, _, v, _)| n == "address-city" && v == "ma"));
    }

    #[test]
    fn test_extract_organization_address_array() {
        // Organization.address is an array of Address objects.
        let org = json!({
            "resourceType": "Organization",
            "name": "Acme Health",
            "address": [{"city": "Tulsa", "state": "OK"}]
        });

        let indices = IndexBuilder::extract_indices("Organization", &org);
        assert!(indices.iter().any(|(n, _, v, _)| n == "address" && v == "tulsa"));
        assert!(indices.iter().any(|(n, _, v, _)| n == "name" && v == "acme health"));
    }

    #[test]
    fn test_extract_care_team_role() {
        // CareTeam.participant.role — role reached through the participant array.
        let care_team = json!({
            "resourceType": "CareTeam",
            "status": "active",
            "subject": {"reference": "Patient/123"},
            "participant": [
                {"role": [{"coding": [{"system": "http://nucc.org/provider-taxonomy", "code": "207Q00000X"}]}]},
                {"role": [{"coding": [{"system": "http://terminology.hl7.org/CodeSystem/v3-RoleCode", "code": "MTH"}]}]}
            ]
        });

        let indices = IndexBuilder::extract_indices("CareTeam", &care_team);
        assert!(indices.iter().any(|(n, t, v, sys)|
            n == "role" && t == "token" && v == "207Q00000X"
                && *sys == Some("http://nucc.org/provider-taxonomy".to_string())
        ));
        assert!(indices.iter().any(|(n, _, v, _)| n == "role" && v == "MTH"));
        assert!(indices.iter().any(|(n, _, v, _)| n == "patient" && v == "Patient/123"));
    }

    #[test]
    fn test_extract_related_person_name() {
        let rp = json!({
            "resourceType": "RelatedPerson",
            "patient": {"reference": "Patient/123"},
            "name": [{"family": "Smith", "given": ["Jane"]}]
        });

        let indices = IndexBuilder::extract_indices("RelatedPerson", &rp);
        assert!(indices.iter().any(|(n, t, v, _)| n == "name" && t == "string" && v == "smith"));
        assert!(indices.iter().any(|(n, _, v, _)| n == "name" && v == "jane"));
        assert!(indices.iter().any(|(n, _, v, _)| n == "patient" && v == "Patient/123"));
    }

    #[test]
    fn test_extract_goal_target_date() {
        // Goal.target.dueDate — dueDate reached through the target array.
        let goal = json!({
            "resourceType": "Goal",
            "lifecycleStatus": "active",
            "subject": {"reference": "Patient/123"},
            "description": {"text": "Maintain HbA1c below 7%"},
            "target": [{"dueDate": "2026-06-01"}]
        });

        let indices = IndexBuilder::extract_indices("Goal", &goal);
        assert!(indices.iter().any(|(n, t, v, _)| n == "target-date" && t == "date" && v == "2026-06-01"));
    }

    #[test]
    fn test_extract_encounter_location_and_discharge() {
        let enc = json!({
            "resourceType": "Encounter",
            "status": "finished",
            "subject": {"reference": "Patient/123"},
            "hospitalization": {
                "dischargeDisposition": {
                    "coding": [{"system": "http://terminology.hl7.org/CodeSystem/discharge-disposition", "code": "home"}]
                }
            },
            "location": [{"location": {"reference": "Location/loc-1"}}]
        });

        let indices = IndexBuilder::extract_indices("Encounter", &enc);
        // discharge-disposition (CodeableConcept under an object path)
        assert!(indices.iter().any(|(n, t, v, _)| n == "discharge-disposition" && t == "token" && v == "home"));
        // location reached through the location array — full ref and bare id
        assert!(indices.iter().any(|(n, t, v, _)| n == "location" && t == "reference" && v == "Location/loc-1"));
        assert!(indices.iter().any(|(n, _, v, _)| n == "location" && v == "loc-1"));
    }

    #[test]
    fn test_extract_condition_recorded_abatement_encounter() {
        let cond = json!({
            "resourceType": "Condition",
            "subject": {"reference": "Patient/123"},
            "encounter": {"reference": "Encounter/enc-1"},
            "onsetDateTime": "2025-12-01",
            "abatementDateTime": "2025-12-15",
            "recordedDate": "2025-12-01T09:15:00-05:00"
        });

        let indices = IndexBuilder::extract_indices("Condition", &cond);
        assert!(indices.iter().any(|(n, t, _, _)| n == "recorded-date" && t == "date"));
        assert!(indices.iter().any(|(n, t, v, _)| n == "abatement-date" && t == "date" && v == "2025-12-15"));
        assert!(indices.iter().any(|(n, _, v, _)| n == "encounter" && v == "Encounter/enc-1"));
        assert!(indices.iter().any(|(n, _, v, _)| n == "encounter" && v == "enc-1"));
    }

    #[test]
    fn test_extract_service_request_category_authored() {
        let sr = json!({
            "resourceType": "ServiceRequest",
            "status": "active",
            "intent": "order",
            "subject": {"reference": "Patient/123"},
            "category": [{"coding": [{"system": "http://snomed.info/sct", "code": "108252007"}]}],
            "authoredOn": "2025-12-01T09:30:00-05:00"
        });

        let indices = IndexBuilder::extract_indices("ServiceRequest", &sr);
        assert!(indices.iter().any(|(n, t, v, _)| n == "category" && t == "token" && v == "108252007"));
        assert!(indices.iter().any(|(n, t, _, _)| n == "authored" && t == "date"));
    }

    #[test]
    fn test_extract_condition_asserted_date_extension() {
        let cond = json!({
            "resourceType": "Condition",
            "extension": [
                {"url": "http://hl7.org/fhir/StructureDefinition/condition-assertedDate", "valueDateTime": "2020-05-15"}
            ],
            "subject": {"reference": "Patient/123"}
        });

        let indices = IndexBuilder::extract_indices("Condition", &cond);
        assert!(indices.iter().any(|(n, t, v, _)| n == "asserted-date" && t == "date" && v == "2020-05-15"));
    }
}
