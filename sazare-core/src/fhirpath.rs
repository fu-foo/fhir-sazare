//! Bounded FHIRPath evaluator for FHIR `SearchParameter.expression`s.
//!
//! Compiles the expressions of runtime-loaded search parameters (the
//! `searchparameters/` directory; see `SearchParamRegistry::register_search_parameter`)
//! and evaluates them at index time. It implements the small, regular
//! sub-language that real search-parameter expressions use. Over the base R4
//! `search-parameters.json` plus the US Core 8.0.0 package (1480 expressions)
//! it accepts **1479 (99.9%)**; the one reject is the computed boolean
//! `Patient.deceased.exists() and Patient.deceased != false`. Anything outside
//! the subset is rejected at load time rather than mis-evaluated
//! (reject-don't-guess). Re-measure with the ignored `corpus` test below.
//!
//! Supported constructs:
//! - path navigation:      `Patient.name.given`
//! - union + grouping:     `(Observation.value as Quantity) | (... as Range)`
//! - choice types:         `Observation.value.ofType(Quantity)` / `... as Quantity`
//! - extension sugar:      `Coverage.extension('url').value.ofType(string)`
//! - simple slice filter:  `Patient.telecom.where(system='phone').value`
//!   (conditions may be `and`-ed: `where(a = 'x' and b = 'y')`)
//! - reference type filter: `Observation.subject.where(resolve() is Patient)`
//!   (also `resolve() is A or resolve() is B`) — decided from the reference
//!   string's type segment, without resolving anything
//! - integer indexer:      `Bundle.entry[0].resource`
//!
//! Anything else (`resolve()` navigation, `exists()`, `%vars`, `or`/`not`
//! and other boolean logic outside `where(... and ...)`,
//! arithmetic, non-string comparison) is a parse error.
//!
//! Design invariants that keep this growable to a fuller FHIRPath later without
//! a rewrite: values are modelled as FHIRPath collections (`Vec<&Value>`), each
//! step is a collection→collection transform, and parsing is separated from
//! evaluation so widening the subset only relaxes the parser.

use serde_json::Value;

/// One collection→collection step in a pipeline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// Navigate a member field (arrays are flattened).
    Member(String),
    /// Choice type `base.ofType(Type)` → the `base{Type}` field (FHIR `[x]`).
    Choice(String, String),
    /// `.extension('url')` → extension array elements with that url.
    Extension(String),
    /// `.where(<relative pipeline> = 'literal')` slice filter.
    Where(Vec<Step>, String),
    /// `.where(resolve() is A or resolve() is B)` — keep Reference nodes whose
    /// target type is one of these. Decided from the reference string's type
    /// segment (`Patient/1`, `http://x/fhir/Patient/1`), so no resolution is
    /// needed; contained (`#id`) references don't match.
    RefType(Vec<String>),
    /// `[n]` indexer: the n-th (0-based) item of the collection.
    Index(usize),
}

/// A parsed expression: a union of pipelines (the alternatives of `a | b`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Expr {
    pub alternatives: Vec<Vec<Step>>,
}

/// Parse failure — carries a human-readable reason. Surfaced at SearchParameter
/// load time so unsupported expressions fail loudly, never silently.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError(pub String);

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "unsupported FHIRPath expression: {}", self.0)
    }
}

// ---------------------------------------------------------------------------
// Tokenizer
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
enum Tok {
    Ident(String),
    Str(String),
    Dot,
    Pipe,
    LParen,
    RParen,
    Eq,
    As,
    Index(usize),
}

fn tokenize(s: &str) -> Result<Vec<Tok>, ParseError> {
    let mut toks = Vec::new();
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        match c {
            '.' => {
                toks.push(Tok::Dot);
                i += 1;
            }
            '|' => {
                toks.push(Tok::Pipe);
                i += 1;
            }
            '(' => {
                toks.push(Tok::LParen);
                i += 1;
            }
            ')' => {
                toks.push(Tok::RParen);
                i += 1;
            }
            '=' => {
                toks.push(Tok::Eq);
                i += 1;
            }
            '[' => {
                let mut j = i + 1;
                while j < chars.len() && chars[j].is_ascii_digit() {
                    j += 1;
                }
                if j == i + 1 || j >= chars.len() || chars[j] != ']' {
                    return Err(ParseError("indexer must be a non-negative integer literal, e.g. [0]".into()));
                }
                let n: String = chars[i + 1..j].iter().collect();
                toks.push(Tok::Index(n.parse().map_err(|_| ParseError(format!("index too large: [{n}]")))?));
                i = j + 1;
            }
            '\'' => {
                let mut val = String::new();
                i += 1;
                while i < chars.len() && chars[i] != '\'' {
                    val.push(chars[i]);
                    i += 1;
                }
                if i >= chars.len() {
                    return Err(ParseError("unterminated string literal".into()));
                }
                i += 1; // closing quote
                toks.push(Tok::Str(val));
            }
            c if c.is_alphabetic() || c == '_' => {
                let mut id = String::new();
                while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_') {
                    id.push(chars[i]);
                    i += 1;
                }
                if id == "as" {
                    toks.push(Tok::As);
                } else {
                    toks.push(Tok::Ident(id));
                }
            }
            other => {
                return Err(ParseError(format!(
                    "unsupported character '{other}' (only path navigation, |, where/ofType/extension/as are supported)"
                )));
            }
        }
    }
    Ok(toks)
}

// ---------------------------------------------------------------------------
// Parser (recursive descent)
// ---------------------------------------------------------------------------

struct Parser {
    toks: Vec<Tok>,
    pos: usize,
}

impl Parser {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.pos)
    }
    fn bump(&mut self) -> Option<Tok> {
        let t = self.toks.get(self.pos).cloned();
        self.pos += 1;
        t
    }
    fn expect(&mut self, t: Tok) -> Result<(), ParseError> {
        if self.peek() == Some(&t) {
            self.pos += 1;
            Ok(())
        } else {
            Err(ParseError(format!("expected {t:?}, found {:?}", self.peek())))
        }
    }
}

/// Parse a SearchParameter expression into an [`Expr`], or fail loudly.
pub fn parse(input: &str) -> Result<Expr, ParseError> {
    let toks = tokenize(input)?;
    if toks.is_empty() {
        return Err(ParseError("empty expression".into()));
    }
    let mut p = Parser { toks, pos: 0 };
    let mut alternatives = vec![parse_steps(&mut p)?];
    while p.peek() == Some(&Tok::Pipe) {
        p.pos += 1;
        alternatives.push(parse_steps(&mut p)?);
    }
    if p.pos != p.toks.len() {
        return Err(ParseError(format!("trailing tokens from {:?}", p.peek())));
    }
    Ok(Expr { alternatives })
}

/// Parse a pipeline of steps until a `|`, `)`, `=`, or end-of-input. A pipeline
/// may begin with a parenthesized sub-pipeline — search expressions commonly
/// group choice-typed pipelines, e.g. `(Observation.value as Quantity) | ...`
/// or `(Observation.value as CodeableConcept).text`. Grouping is precedence
/// only, so the inner steps are inlined.
fn parse_steps(p: &mut Parser) -> Result<Vec<Step>, ParseError> {
    let mut steps = Vec::new();
    if p.peek() == Some(&Tok::LParen) {
        p.pos += 1;
        let inner = parse_steps(p)?;
        p.expect(Tok::RParen)?;
        steps.extend(inner);
    } else {
        reconcile(&mut steps, parse_raw(p)?)?;
    }
    loop {
        match p.peek() {
            Some(Tok::Dot) => {
                p.pos += 1;
                // `.as(Type)` functional choice form.
                if p.peek() == Some(&Tok::As) {
                    p.pos += 1;
                    p.expect(Tok::LParen)?;
                    let ty = parse_ident(p)?;
                    p.expect(Tok::RParen)?;
                    reconcile(&mut steps, Raw::OfType(ty))?;
                } else {
                    reconcile(&mut steps, parse_raw(p)?)?;
                }
            }
            // Infix `expr as Type`.
            Some(Tok::As) => {
                p.pos += 1;
                let ty = parse_ident(p)?;
                reconcile(&mut steps, Raw::OfType(ty))?;
            }
            Some(&Tok::Index(n)) => {
                p.pos += 1;
                steps.push(Step::Index(n));
            }
            _ => break,
        }
    }
    Ok(steps)
}

/// Intermediate step before `ofType`/`as` is folded into the preceding member.
enum Raw {
    Member(String),
    Extension(String),
    /// One or more `and`-ed equality conditions; each becomes its own
    /// `Step::Where`, since `where(a and b)` ≡ `where(a).where(b)`.
    Where(Vec<(Vec<Step>, String)>),
    RefType(Vec<String>),
    OfType(String),
}

fn parse_raw(p: &mut Parser) -> Result<Raw, ParseError> {
    let id = parse_ident(p)?;
    // Function call?
    if p.peek() == Some(&Tok::LParen) {
        p.pos += 1;
        let raw = match id.as_str() {
            "extension" => {
                let url = match p.bump() {
                    Some(Tok::Str(s)) => s,
                    other => {
                        return Err(ParseError(format!(
                            "extension() expects a string url, found {other:?}"
                        )));
                    }
                };
                Raw::Extension(url)
            }
            "ofType" => Raw::OfType(parse_ident(p)?),
            "where" => {
                if p.peek() == Some(&Tok::Ident("resolve".into())) {
                    Raw::RefType(parse_ref_type_cond(p)?)
                } else {
                    let mut conds = vec![parse_where_cond(p)?];
                    while p.peek() == Some(&Tok::Ident("and".into())) {
                        p.pos += 1;
                        conds.push(parse_where_cond(p)?);
                    }
                    Raw::Where(conds)
                }
            }
            other => {
                return Err(ParseError(format!(
                    "unsupported function '{other}()' (only where/ofType/extension)"
                )));
            }
        };
        p.expect(Tok::RParen)?;
        Ok(raw)
    } else {
        Ok(Raw::Member(id))
    }
}

/// Fold a raw step into the pipeline; `ofType`/`as` merge with the prior member.
fn reconcile(steps: &mut Vec<Step>, raw: Raw) -> Result<(), ParseError> {
    match raw {
        Raw::Member(m) => steps.push(Step::Member(m)),
        Raw::Extension(u) => steps.push(Step::Extension(u)),
        Raw::Where(conds) => {
            steps.extend(conds.into_iter().map(|(lhs, lit)| Step::Where(lhs, lit)))
        }
        Raw::RefType(types) => steps.push(Step::RefType(types)),
        Raw::OfType(ty) => match steps.pop() {
            Some(Step::Member(base)) => steps.push(Step::Choice(base, ty)),
            _ => {
                return Err(ParseError(
                    "ofType()/as must follow a member access (e.g. value.ofType(Quantity))".into(),
                ));
            }
        },
    }
    Ok(())
}

/// One `path = 'literal'` condition of `where(...)` (the caller handles
/// `and`). `or` and non-string compares are rejected (outside the subset).
fn parse_where_cond(p: &mut Parser) -> Result<(Vec<Step>, String), ParseError> {
    let mut lhs = Vec::new();
    reconcile(&mut lhs, parse_raw(p)?)?;
    loop {
        match p.peek() {
            Some(Tok::Dot) => {
                p.pos += 1;
                if p.peek() == Some(&Tok::As) {
                    p.pos += 1;
                    p.expect(Tok::LParen)?;
                    let ty = parse_ident(p)?;
                    p.expect(Tok::RParen)?;
                    reconcile(&mut lhs, Raw::OfType(ty))?;
                } else {
                    reconcile(&mut lhs, parse_raw(p)?)?;
                }
            }
            Some(Tok::As) => {
                p.pos += 1;
                let ty = parse_ident(p)?;
                reconcile(&mut lhs, Raw::OfType(ty))?;
            }
            Some(Tok::Eq) => break,
            other => {
                return Err(ParseError(format!(
                    "where() condition must be `path = 'literal'` (optionally `and`-ed), found {other:?}"
                )));
            }
        }
    }
    p.expect(Tok::Eq)?;
    let lit = match p.bump() {
        Some(Tok::Str(s)) => s,
        other => {
            return Err(ParseError(format!(
                "where() must compare to a string literal, found {other:?}"
            )));
        }
    };
    Ok((lhs, lit))
}

/// `resolve() is A (or resolve() is B)*` inside `where(...)`.
fn parse_ref_type_cond(p: &mut Parser) -> Result<Vec<String>, ParseError> {
    let mut types = Vec::new();
    loop {
        let resolve = parse_ident(p)?;
        if resolve != "resolve" {
            return Err(ParseError(format!(
                "where() reference filter must be `resolve() is Type`, found '{resolve}'"
            )));
        }
        p.expect(Tok::LParen)?;
        p.expect(Tok::RParen)?;
        match p.bump() {
            Some(Tok::Ident(kw)) if kw == "is" => {}
            other => {
                return Err(ParseError(format!(
                    "only `resolve() is Type` is supported, found {other:?} after resolve()"
                )));
            }
        }
        types.push(parse_ident(p)?);
        if p.peek() == Some(&Tok::Ident("or".into())) {
            p.pos += 1;
        } else {
            return Ok(types);
        }
    }
}

fn parse_ident(p: &mut Parser) -> Result<String, ParseError> {
    match p.bump() {
        Some(Tok::Ident(s)) => Ok(s),
        other => Err(ParseError(format!("expected identifier, found {other:?}"))),
    }
}

// ---------------------------------------------------------------------------
// Evaluator — collection (Vec<&Value>) in, collection out.
// ---------------------------------------------------------------------------

/// Evaluate an expression against a resource, returning the matched nodes.
/// Type-specific flattening to index values (code+system, reference string, …)
/// is the caller's job — that reuses the existing `ExtractionMode` shaping.
pub fn evaluate<'a>(expr: &Expr, root: &'a Value) -> Vec<&'a Value> {
    let mut out = Vec::new();
    for pipeline in &expr.alternatives {
        out.extend(eval_steps(pipeline, root, true));
    }
    out
}

fn eval_steps<'a>(steps: &[Step], root: &'a Value, anchored: bool) -> Vec<&'a Value> {
    let mut cur: Vec<&Value> = vec![root];
    for (i, step) in steps.iter().enumerate() {
        cur = apply(step, &cur, anchored && i == 0);
    }
    cur
}

fn apply<'a>(step: &Step, nodes: &[&'a Value], anchor: bool) -> Vec<&'a Value> {
    let mut out = Vec::new();
    match step {
        Step::Member(name) => {
            for n in nodes {
                // A leading resource-type identifier (`Patient.name`) is an
                // anchor: the context node when it is of that type (or the
                // abstract `Resource` / `DomainResource`), else nothing —
                // `Observation.code` in a union evaluated on a Patient.
                // (FHIR element names never start upper-case.)
                if anchor && name.starts_with(char::is_uppercase) {
                    let ty = n.get("resourceType").and_then(Value::as_str);
                    if ty == Some(name.as_str()) || matches!(name.as_str(), "Resource" | "DomainResource") {
                        out.push(*n);
                    }
                } else {
                    navigate(n, name, &mut out);
                }
            }
        }
        Step::Choice(base, ty) => {
            let field = format!("{base}{}", capitalize(ty));
            for n in nodes {
                navigate(n, &field, &mut out);
            }
        }
        Step::Extension(url) => {
            for n in nodes {
                if let Some(arr) = n.get("extension").and_then(Value::as_array) {
                    for e in arr {
                        if e.get("url").and_then(Value::as_str) == Some(url.as_str()) {
                            out.push(e);
                        }
                    }
                }
            }
        }
        Step::Where(cond, lit) => {
            for n in nodes {
                let vals = eval_steps(cond, n, false);
                if vals.iter().any(|v| v.as_str() == Some(lit.as_str())) {
                    out.push(*n);
                }
            }
        }
        Step::Index(i) => out.extend(nodes.get(*i).copied()),
        Step::RefType(types) => {
            for n in nodes {
                let target = n
                    .get("reference")
                    .and_then(Value::as_str)
                    .and_then(reference_target_type);
                if target.is_some_and(|t| types.iter().any(|ty| ty == t)) {
                    out.push(*n);
                }
            }
        }
    }
    out
}

/// The resource type a reference string points at: the segment before the id
/// in `Type/id`, `Type/id/_history/v` or an absolute `.../Type/id`. `None` for
/// contained (`#id`), urn:uuid or otherwise typeless references.
fn reference_target_type(reference: &str) -> Option<&str> {
    let trimmed = reference.split("/_history/").next()?;
    let mut segs = trimmed.rsplit('/');
    let _id = segs.next()?;
    let ty = segs.next()?;
    ty.chars().next().is_some_and(char::is_uppercase).then_some(ty)
}

fn navigate<'a>(node: &'a Value, field: &str, out: &mut Vec<&'a Value>) {
    let mut push = |v: &'a Value| match v.as_array() {
        Some(arr) => out.extend(arr.iter()),
        None => out.push(v),
    };
    if let Some(v) = node.get(field) {
        push(v);
        return;
    }
    // A choice element named without its type (`ServiceRequest.occurrence`,
    // as several R4 expressions do) is stored as `<field><Type>` in JSON
    // (`occurrenceDateTime`). Only known type names: `performer` must not
    // pick up `performerType`.
    if let Some(obj) = node.as_object() {
        for (k, v) in obj {
            if k.strip_prefix(field).is_some_and(|ty| CHOICE_TYPES.contains(&ty)) {
                push(v);
            }
        }
    }
}

/// The type suffixes of FHIR R4 choice (`[x]`) elements in JSON.
const CHOICE_TYPES: &[&str] = &[
    "Base64Binary", "Boolean", "Canonical", "Code", "Date", "DateTime", "Decimal", "Id", "Instant",
    "Integer", "Markdown", "Oid", "PositiveInt", "String", "Time", "UnsignedInt", "Uri", "Url",
    "Uuid", "Address", "Age", "Annotation", "Attachment", "CodeableConcept", "Coding",
    "ContactPoint", "Count", "Distance", "Duration", "HumanName", "Identifier", "Money", "Period",
    "Quantity", "Range", "Ratio", "Reference", "SampledData", "Signature", "Timing",
    "ContactDetail", "Contributor", "DataRequirement", "Expression", "ParameterDefinition",
    "RelatedArtifact", "TriggerDefinition", "UsageContext", "Dosage", "Meta",
];

fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn strs(nodes: &[&Value]) -> Vec<String> {
        nodes.iter().filter_map(|v| v.as_str().map(String::from)).collect()
    }

    #[test]
    fn path_navigation_and_arrays() {
        let patient = json!({
            "resourceType": "Patient",
            "name": [
                {"family": "Brown", "given": ["John", "Q"]},
                {"family": "Smith", "given": ["Amy"]}
            ]
        });
        let e = parse("Patient.name.family").unwrap();
        assert_eq!(strs(&evaluate(&e, &patient)), vec!["Brown", "Smith"]);
        let g = parse("Patient.name.given").unwrap();
        assert_eq!(strs(&evaluate(&g, &patient)), vec!["John", "Q", "Amy"]);
    }

    #[test]
    fn leading_type_is_an_anchor_not_a_field() {
        let obs = json!({"resourceType": "Observation", "status": "final"});
        let e = parse("Observation.status").unwrap();
        assert_eq!(strs(&evaluate(&e, &obs)), vec!["final"]);
    }

    #[test]
    fn choice_type_oftype_and_as() {
        let obs = json!({
            "resourceType": "Observation",
            "valueQuantity": {"value": 9.5, "unit": "kg"}
        });
        for expr in ["Observation.value.ofType(Quantity)", "Observation.value as Quantity"] {
            let e = parse(expr).unwrap();
            let r = evaluate(&e, &obs);
            assert_eq!(r.len(), 1, "{expr}");
            assert_eq!(r[0].get("unit").unwrap(), "kg", "{expr}");
        }
    }

    #[test]
    fn union_of_pipelines() {
        let a = json!({
            "resourceType": "AllergyIntolerance",
            "code": {"text": "peanut"},
            "reaction": [{"substance": {"text": "histamine"}}]
        });
        let e = parse("AllergyIntolerance.code | AllergyIntolerance.reaction.substance").unwrap();
        let r = evaluate(&e, &a);
        let texts: Vec<&str> = r.iter().filter_map(|v| v.get("text").and_then(Value::as_str)).collect();
        assert_eq!(texts, vec!["peanut", "histamine"]);
    }

    #[test]
    fn where_slice_filter() {
        let patient = json!({
            "resourceType": "Patient",
            "telecom": [
                {"system": "phone", "value": "555-1234"},
                {"system": "email", "value": "a@b.com"}
            ]
        });
        let e = parse("Patient.telecom.where(system='phone').value").unwrap();
        assert_eq!(strs(&evaluate(&e, &patient)), vec!["555-1234"]);
    }

    #[test]
    fn real_jp_core_extension_expression() {
        // jp-insured-personnumber, verbatim from JP Core 1.2.0.
        let cov = json!({
            "resourceType": "Coverage",
            "extension": [{
                "url": "http://jpfhir.jp/fhir/core/Extension/StructureDefinition/JP_Coverage_InsuredPersonNumber",
                "valueString": "12345678"
            }]
        });
        let e = parse("Coverage.extension('http://jpfhir.jp/fhir/core/Extension/StructureDefinition/JP_Coverage_InsuredPersonNumber').value.ofType(string)").unwrap();
        assert_eq!(strs(&evaluate(&e, &cov)), vec!["12345678"]);
    }

    #[test]
    fn real_jp_core_nested_extension_choice_member() {
        // jp-medication-start: dosageInstruction.extension('url').value.ofType(Period).start
        let mr = json!({
            "resourceType": "MedicationRequest",
            "dosageInstruction": [{
                "extension": [{
                    "url": "http://jpfhir.jp/fhir/core/Extension/StructureDefinition/JP_MedicationDosage_PeriodOfUse",
                    "valuePeriod": {"start": "2026-06-15", "end": "2026-06-30"}
                }]
            }]
        });
        let e = parse("MedicationRequest.dosageInstruction.extension('http://jpfhir.jp/fhir/core/Extension/StructureDefinition/JP_MedicationDosage_PeriodOfUse').value.ofType(Period).start").unwrap();
        assert_eq!(strs(&evaluate(&e, &mr)), vec!["2026-06-15"]);
    }

    #[test]
    fn real_jp_core_kana_and_in_where() {
        // jp-kana-sort, verbatim from JP Core 1.2.0: `and`-ed where conditions.
        let pat = json!({
            "resourceType": "Patient",
            "name": [
                {"extension": [{"url": "http://hl7.org/fhir/StructureDefinition/iso21090-EN-representation", "valueCode": "IDE"}],
                 "use": "usual", "text": "見本 一"},
                {"extension": [{"url": "http://hl7.org/fhir/StructureDefinition/iso21090-EN-representation", "valueCode": "SYL"}],
                 "use": "usual", "text": "ミホン ハジメ"},
                {"extension": [{"url": "http://hl7.org/fhir/StructureDefinition/iso21090-EN-representation", "valueCode": "SYL"}],
                 "use": "old", "text": "テスト ハジメ"}
            ]
        });
        let e = parse("Patient.name.where(extension('http://hl7.org/fhir/StructureDefinition/iso21090-EN-representation').value.ofType(code)='SYL' and use='usual').text").unwrap();
        assert_eq!(strs(&evaluate(&e, &pat)), vec!["ミホン ハジメ"]);
    }

    #[test]
    fn where_resolve_is_filters_reference_target_type() {
        let obs = json!({
            "resourceType": "Observation",
            "performer": [
                {"reference": "Practitioner/pr1"},
                {"reference": "Organization/o1"},
                {"reference": "http://example.org/fhir/Practitioner/pr2/_history/3"},
                {"reference": "#contained-1"},
                {"display": "no reference"}
            ]
        });
        let e = parse("Observation.performer.where(resolve() is Practitioner)").unwrap();
        let refs: Vec<&str> = evaluate(&e, &obs)
            .iter()
            .map(|n| n["reference"].as_str().unwrap())
            .collect();
        assert_eq!(refs, vec!["Practitioner/pr1", "http://example.org/fhir/Practitioner/pr2/_history/3"]);

        let e = parse("Observation.performer.where(resolve() is Practitioner or resolve() is Organization)").unwrap();
        assert_eq!(evaluate(&e, &obs).len(), 3);
    }

    #[test]
    fn indexer_picks_nth_item() {
        let b = json!({"resourceType": "Bundle", "entry": [
            {"resource": {"resourceType": "Composition", "id": "c"}},
            {"resource": {"resourceType": "Patient", "id": "p"}}
        ]});
        let ids = |x: &str| -> Vec<String> {
            evaluate(&parse(x).unwrap(), &b).iter().map(|n| n["id"].as_str().unwrap().to_string()).collect()
        };
        assert_eq!(ids("Bundle.entry[0].resource"), vec!["c"]);
        assert_eq!(ids("Bundle.entry[1].resource"), vec!["p"]);
        assert!(ids("Bundle.entry[5].resource").is_empty());
    }

    #[test]
    fn reference_target_type_forms() {
        assert_eq!(reference_target_type("Patient/1"), Some("Patient"));
        assert_eq!(reference_target_type("Patient/1/_history/2"), Some("Patient"));
        assert_eq!(reference_target_type("https://x.org/fhir/Patient/1"), Some("Patient"));
        assert_eq!(reference_target_type("#c1"), None);
        assert_eq!(reference_target_type("urn:uuid:1234"), None);
    }

    #[test]
    fn rejects_constructs_outside_the_subset() {
        // Representatives of every rejected family in the real corpus.
        let rejects = [
            "Observation.subject.where(resolve().name = 'x')", // resolve() navigation
            "Observation.subject.where(resolve() as Patient)", // only `is` is supported
            "Patient.deceased.exists() and Patient.deceased != false", // exists()/and/!=
            "Patient.telecom.where(system='phone' or system='fax').value", // `or` in where
            "Observation.value.resolve()",   // resolve()
            "Patient.name[first].given",     // non-literal indexer
            "%resource.id",                  // env var
            "Patient.active = true",         // bare boolean compare, no path step
        ];
        for r in rejects {
            assert!(parse(r).is_err(), "should reject: {r}");
        }
    }
}

/// Corpus check, run on demand:
/// `FHIRPATH_CORPUS=expressions.tsv cargo test -p sazare-core corpus -- --ignored --nocapture`
/// where each line is `source<TAB>id<TAB>expression` (e.g. extracted from the
/// R4 `search-parameters.json` and an IG package). Prints the acceptance rate
/// and every rejected expression with its reason.
#[cfg(test)]
mod choice_and_anchor {
    use super::*;
    use serde_json::json;

    fn eval(expr: &str, r: &Value) -> Vec<Value> {
        evaluate(&parse(expr).unwrap(), r).into_iter().cloned().collect()
    }

    #[test]
    fn a_choice_element_without_its_type_finds_the_typed_key() {
        let sr = json!({"resourceType": "ServiceRequest", "occurrencePeriod": {"start": "2020"}, "performerType": {"text": "x"}});
        assert_eq!(eval("ServiceRequest.occurrence", &sr), [json!({"start": "2020"})]);
        // Not a type name: no match.
        assert!(eval("ServiceRequest.performer", &sr).is_empty());
        let sp = json!({"resourceType": "Specimen", "collection": {"collectedDateTime": "2021-01-01"}});
        assert_eq!(eval("Specimen.collection.collected", &sp), [json!("2021-01-01")]);
        // The exact key wins.
        let o = json!({"resourceType": "Observation", "status": "final", "statusReason": "x"});
        assert_eq!(eval("Observation.status", &o), [json!("final")]);
    }

    #[test]
    fn another_types_branch_of_a_union_is_empty() {
        let p = json!({"resourceType": "Patient", "identifier": [{"value": "p"}], "Observation": {"identifier": "trap"}});
        assert_eq!(eval("Observation.identifier | Patient.identifier", &p), [json!({"value": "p"})]);
        assert_eq!(eval("Resource.id", &json!({"resourceType": "Patient", "id": "1"})), [json!("1")]);
    }
}

#[cfg(test)]
mod corpus {
    #[test]
    #[ignore]
    fn corpus_acceptance() {
        let path = std::env::var("FHIRPATH_CORPUS").expect("set FHIRPATH_CORPUS");
        let text = std::fs::read_to_string(path).unwrap();
        let (mut ok, mut total) = (0, 0);
        for line in text.lines() {
            let mut cols = line.splitn(3, '\t');
            let (Some(src), Some(id), Some(expr)) = (cols.next(), cols.next(), cols.next()) else {
                continue;
            };
            total += 1;
            match super::parse(expr) {
                Ok(_) => ok += 1,
                Err(e) => println!("REJECT {src}/{id}: {e}\n    {expr}"),
            }
        }
        println!("accepted {ok}/{total} ({:.1}%)", 100.0 * ok as f64 / total as f64);
    }
}
