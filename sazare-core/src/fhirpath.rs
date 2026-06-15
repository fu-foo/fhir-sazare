//! Bounded FHIRPath evaluator for FHIR `SearchParameter.expression`s.
//!
//! **PROTOTYPE — not yet wired into the search index.** This is the keystone
//! component for runtime-loadable custom search parameters (see the design
//! note in `docs/fhirpath-search-subset.md`). It deliberately implements only
//! the small, regular sub-language that real search-parameter expressions use.
//! Running this parser over the real corpus (base R4 + US Core + JP Core, 1478
//! expressions) accepts **97.0%** (1434/1478). The rest is rejected loudly
//! rather than mis-evaluated (reject-don't-guess). The single dominant boundary
//! is `resolve()` (40 of the 44 rejects): reference-target-type filters like
//! `subject.where(resolve() is Patient)` — and those are the base cross-resource
//! reference params that sazare's registry already handles directly.
//!
//! Supported constructs:
//! - path navigation:      `Patient.name.given`
//! - union + grouping:     `(Observation.value as Quantity) | (... as Range)`
//! - choice types:         `Observation.value.ofType(Quantity)` / `... as Quantity`
//! - extension sugar:      `Coverage.extension('url').value.ofType(string)`
//! - simple slice filter:  `Patient.telecom.where(system='phone').value`
//!
//! Anything else (`resolve()`, `exists()`, `%vars`, `[n]` indexers, boolean
//! logic, arithmetic, non-string comparison) is a parse error.
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
            _ => break,
        }
    }
    Ok(steps)
}

/// Intermediate step before `ofType`/`as` is folded into the preceding member.
enum Raw {
    Member(String),
    Extension(String),
    Where(Vec<Step>, String),
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
                let (lhs, lit) = parse_where_cond(p)?;
                Raw::Where(lhs, lit)
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
        Raw::Where(lhs, lit) => steps.push(Step::Where(lhs, lit)),
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

/// `where(<relative pipeline> = 'literal')`. Boolean logic / non-string compares
/// are rejected (they fall outside the supported subset).
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
                    "where() condition must be `path = 'literal'`, found {other:?}"
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
                // anchor: it returns the context node, not a child field.
                if anchor && n.get("resourceType").and_then(Value::as_str) == Some(name.as_str()) {
                    out.push(*n);
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
    }
    out
}

fn navigate<'a>(node: &'a Value, field: &str, out: &mut Vec<&'a Value>) {
    if let Some(v) = node.get(field) {
        match v.as_array() {
            Some(arr) => out.extend(arr.iter()),
            None => out.push(v),
        }
    }
}

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
    fn rejects_constructs_outside_the_subset() {
        // Representatives of every rejected family in the real corpus.
        let rejects = [
            "Observation.subject.where(resolve() is Patient)", // resolve() — the dominant boundary
            "Patient.deceased.exists() and Patient.deceased != false", // exists()/and/!=
            "Patient.name.where(extension('http://hl7.org/fhir/StructureDefinition/iso21090-EN-representation').value.ofType(code)='SYL' and use='usual').text", // bool in where
            "Observation.value.resolve()",   // resolve()
            "Patient.name[0].given",         // indexer
            "%resource.id",                  // env var
            "Patient.active = true",         // bare boolean compare, no path step
        ];
        for r in rejects {
            assert!(parse(r).is_err(), "should reject: {r}");
        }
    }
}
