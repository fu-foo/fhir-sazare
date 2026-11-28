//! A resource's shape against the R4 definitions: every element is one the
//! resource type (or the data type it is in) defines, a list where the element
//! repeats and a single value where it doesn't, and each value of the JSON
//! type its FHIR type has. `structure/r4.tsv` (generated from the R4
//! StructureDefinitions, `generate.py` next to it) lists the elements.
//!
//! Also from the definitions: required elements (min 1, met by the value or
//! by `_name` with an extension, as a data-absent-reason), primitive values
//! against their type's regex (`r4-primitives.tsv`) plus what a regex can't
//! say (a real calendar date, the 32-bit range of integer, positiveInt and
//! unsignedInt), no empty values (ele-1: `""`, `{}`), and one type of a
//! choice (`value[x]`).
//!
//! Not checked here: invariants beyond those, terminology — profiles and
//! the other phases do what is checked of those.

use std::collections::HashMap;
use std::sync::LazyLock;

use serde_json::{Map, Value};

use crate::operation_outcome::{IssueSeverity, IssueType, OperationOutcome, OperationOutcomeIssue};

/// An element: whether it repeats, and its types (or the element whose
/// content it has).
struct Element {
    repeats: bool,
    required: bool,
    types: Vec<&'static str>,
    content_of: Option<&'static str>,
}

static ELEMENTS: LazyLock<HashMap<&'static str, Element>> = LazyLock::new(|| {
    include_str!("../../structure/r4.tsv")
        .lines()
        .filter_map(|line| {
            let mut cols = line.split('\t');
            let (path, max, types) = (cols.next()?, cols.next()?, cols.next().unwrap_or(""));
            let required = cols.next().is_some_and(|min| min != "0");
            let (types, content_of) = match types.strip_prefix('@') {
                Some(target) => (vec![], Some(target)),
                None => (types.split(',').filter(|t| !t.is_empty()).collect(), None),
            };
            Some((path, Element { repeats: max != "1" && max != "0", required, types, content_of }))
        })
        .collect()
});

/// The required (min 1) children of each type or backbone path:
/// `Observation` → `status`, `code`; `Observation.component` → `code`;
/// `Extension` → `url`. A choice is named with its `[x]`.
static REQUIRED: LazyLock<HashMap<&'static str, Vec<&'static str>>> = LazyLock::new(|| {
    let mut m: HashMap<&'static str, Vec<&'static str>> = HashMap::new();
    for (path, _) in ELEMENTS.iter().filter(|(_, e)| e.required) {
        if let Some((parent, name)) = path.rsplit_once('.') {
            m.entry(parent).or_default().push(name);
        }
    }
    for names in m.values_mut() {
        names.sort_unstable();
    }
    m
});

/// Each primitive type's value regex, whole-value anchored.
static PRIMITIVES: LazyLock<HashMap<&'static str, regex::Regex>> = LazyLock::new(|| {
    include_str!("../../structure/r4-primitives.tsv")
        .lines()
        .filter_map(|line| {
            let (ty, rx) = line.split_once('\t')?;
            // The R4 regexes are Java's: `\s` is ASCII whitespace there,
            // Unicode whitespace (a no-break space too) in Rust's regex.
            let rx = rx.replace(r"\S", r"[^ \t\n\x0B\f\r]").replace(r"\s", r"[ \t\n\x0B\f\r]");
            Some((ty, regex::Regex::new(&format!("^(?:{rx})$")).expect("an R4 primitive regex")))
        })
        .collect()
});

/// Types whose definitions are in the table (complex data types and
/// resources), so their content can be checked.
static DEFINED: LazyLock<std::collections::HashSet<&'static str>> =
    LazyLock::new(|| ELEMENTS.keys().filter_map(|p| p.split('.').next()).collect());

/// Whether `path` (`Patient.name`, `Person.link.target`) is an R4 element.
pub fn has_element(path: &str) -> bool {
    ELEMENTS.contains_key(path)
}

/// At most this many problems are reported (the first ones).
const MAX_ISSUES: usize = 20;
/// Nesting deeper than this (contained in contained in …) isn't followed.
const MAX_DEPTH: usize = 32;

/// Check `resource`'s shape; `Err` lists what doesn't fit.
pub fn validate(resource: &Value) -> Result<(), OperationOutcome> {
    let Some(obj) = resource.as_object() else {
        return Err(OperationOutcome::error(IssueType::Structure, "A resource is a JSON object"));
    };
    let Some(rt) = obj.get("resourceType").and_then(|v| v.as_str()) else { return Ok(()) };
    if !DEFINED.contains(rt) {
        return Ok(());
    }
    let mut issues = Vec::new();
    check_object(obj, rt, rt, true, 0, &mut issues);
    if issues.is_empty() {
        return Ok(());
    }
    let mut outcome = OperationOutcome::error(IssueType::Structure, issues[0].1.clone());
    outcome.issue = issues
        .into_iter()
        .map(|(loc, msg)| OperationOutcomeIssue {
            severity: IssueSeverity::Error,
            code: IssueType::Structure,
            diagnostics: Some(msg),
            details: None,
            expression: Some(vec![loc]),
        })
        .collect();
    Err(outcome)
}

/// The children of `base` (a type name or an element path) in `obj`;
/// `loc` is where in the resource (for messages).
fn check_object(obj: &Map<String, Value>, base: &str, loc: &str, is_resource: bool, depth: usize, issues: &mut Vec<(String, String)>) {
    if depth > MAX_DEPTH {
        return;
    }
    let mut choices: HashMap<String, String> = HashMap::new();
    for (key, value) in obj {
        if issues.len() >= MAX_ISSUES {
            return;
        }
        if is_resource && key == "resourceType" {
            continue;
        }
        let here = format!("{loc}.{key}");
        // `_name`: the id and extensions of the primitive `name` — an
        // Element (or a list of them, null where a value has none).
        if let Some(primitive) = key.strip_prefix('_') {
            let Some((_, _, repeats)) = resolve(base, primitive) else {
                issues.push((here.clone(), format!("Unknown element '{here}'")));
                continue;
            };
            match (repeats, value) {
                (true, Value::Array(items)) if items.is_empty() => {
                    issues.push((here.clone(), format!("'{here}' is an empty array (leave the element out)")));
                }
                (true, Value::Array(items)) => {
                    for (i, item) in items.iter().enumerate().filter(|(_, v)| !v.is_null()) {
                        check_value(item, "Element", &["Element"], &format!("{here}[{i}]"), depth, issues);
                    }
                }
                (true, _) => issues.push((here.clone(), format!("'{here}' repeats: its value is a JSON array"))),
                (false, Value::Array(_)) => issues.push((here.clone(), format!("'{here}' has one value, not an array"))),
                (false, v) => check_value(v, "Element", &["Element"], &here, depth, issues),
            }
            continue;
        }
        let Some((path, types, repeats)) = resolve(base, key) else {
            issues.push((here.clone(), format!("Unknown element '{here}'")));
            continue;
        };
        // A choice has one value: `valueString` and `valueBoolean` together
        // are two.
        if path.ends_with("[x]")
            && let Some(first) = choices.insert(path.clone(), key.clone())
        {
            let name = path.rsplit('.').next().unwrap_or(&path);
            issues.push((here.clone(), format!("'{loc}.{name}' has one value: both '{first}' and '{key}' are given")));
        }
        match (repeats, value) {
            (true, Value::Array(items)) => {
                if items.is_empty() {
                    issues.push((here.clone(), format!("'{here}' is an empty array (leave the element out)")));
                }
                for (i, item) in items.iter().enumerate() {
                    // A null holds the place of a value that only has `_key`.
                    if !item.is_null() {
                        check_value(item, &path, &types, &format!("{here}[{i}]"), depth, issues);
                    }
                }
            }
            (true, _) => issues.push((here.clone(), format!("'{here}' repeats: its value is a JSON array"))),
            (false, Value::Array(_)) => issues.push((here.clone(), format!("'{here}' has one value, not an array"))),
            (false, v) => check_value(v, &path, &types, &here, depth, issues),
        }
    }
    check_required(obj, base, loc, issues);
    // ext-1: an extension has a value or extensions, not both.
    if base == "Extension"
        && obj.get("extension").and_then(|e| e.as_array()).is_some_and(|e| !e.is_empty())
        && obj.keys().any(|k| k.starts_with("value") && resolve("Extension", k).is_some())
    {
        issues.push((loc.to_string(), format!("'{loc}' has a value and extensions: an extension has one or the other (ext-1)")));
    }
}

/// The required children of `base` missing from `obj`: a value, or `_name`
/// with content (an extension such as data-absent-reason).
fn check_required(obj: &Map<String, Value>, base: &str, loc: &str, issues: &mut Vec<(String, String)>) {
    let Some(required) = REQUIRED.get(base) else { return };
    let given = |k: &str| match obj.get(k) {
        None | Some(Value::Null) => false,
        Some(Value::Array(a)) => a.iter().any(|v| !v.is_null()),
        Some(Value::Object(o)) => !o.is_empty(),
        Some(_) => true,
    };
    for name in required {
        if issues.len() >= MAX_ISSUES {
            return;
        }
        let present = match name.strip_suffix("[x]") {
            Some(stem) => obj.keys().any(|k| {
                let k = k.strip_prefix('_').unwrap_or(k);
                k.strip_prefix(stem).is_some_and(|t| t.starts_with(|c: char| c.is_ascii_uppercase())) && resolve(base, k).is_some()
            }) && obj.iter().any(|(k, _)| {
                let bare = k.strip_prefix('_').unwrap_or(k);
                bare.strip_prefix(stem).is_some_and(|t| t.starts_with(|c: char| c.is_ascii_uppercase())) && given(k)
            }),
            None => given(name) || given(&format!("_{name}")),
        };
        if !present {
            let at = format!("{loc}.{name}");
            issues.push((at.clone(), format!("'{at}' is required")));
        }
    }
}

/// The element `key` of `base`: its path (where its own children are, for a
/// backbone element), its type(s) and whether it repeats. A choice
/// (`value[x]`) resolves by the type in the name (`valueQuantity`).
fn resolve(base: &str, key: &str) -> Option<(String, Vec<&'static str>, bool)> {
    let path = format!("{base}.{key}");
    if let Some(el) = ELEMENTS.get(path.as_str()) {
        if let Some(target) = el.content_of {
            let t = ELEMENTS.get(target)?;
            return Some((target.to_string(), t.types.clone(), el.repeats));
        }
        return Some((path, el.types.clone(), el.repeats));
    }
    // A choice: `valueQuantity` for `value[x]`, `multipleBirthBoolean` for
    // `multipleBirth[x]` (the name may have capitals of its own).
    for (split, _) in key.char_indices().skip(1).filter(|(_, c)| c.is_ascii_uppercase()) {
        let (name, ty) = key.split_at(split);
        let Some(el) = ELEMENTS.get(format!("{base}.{name}[x]").as_str()) else { continue };
        let found = el.types.iter().find(|t| {
            let mut c = t.chars();
            c.next().is_some_and(|f| f.to_ascii_uppercase().to_string() + c.as_str() == ty)
        });
        if let Some(t) = found {
            return Some((format!("{base}.{name}[x]"), vec![*t], el.repeats));
        }
    }
    None
}

fn check_value(value: &Value, path: &str, types: &[&'static str], loc: &str, depth: usize, issues: &mut Vec<(String, String)>) {
    let [ty] = types else { return }; // no type, or a choice not resolved: not checked
    let want = match *ty {
        "boolean" => Some(("a boolean", value.is_boolean())),
        "integer" | "positiveInt" | "unsignedInt" => Some(("an integer", value.is_i64() || value.is_u64())),
        "decimal" => Some(("a number", value.is_number())),
        "string" | "code" | "id" | "uri" | "url" | "canonical" | "oid" | "uuid" | "markdown" | "base64Binary" | "date"
        | "dateTime" | "instant" | "time" | "xhtml" => Some(("a string", value.is_string())),
        _ => None,
    };
    if let Some((what, ok)) = want {
        if !ok {
            issues.push((loc.to_string(), format!("'{loc}' is {} {ty}: {what}, not {}", article(ty), kind(value))));
        } else if let Err(why) = primitive_format(ty, value) {
            issues.push((loc.to_string(), format!("'{loc}' is not a valid {ty}: {why}")));
        }
        return;
    }
    let Some(obj) = value.as_object() else {
        issues.push((loc.to_string(), format!("'{loc}' is {} {ty}: a JSON object, not {}", article(ty), kind(value))));
        return;
    };
    // ele-1: an element has a value or children.
    if obj.is_empty() {
        issues.push((loc.to_string(), format!("'{loc}' is empty (leave the element out)")));
        return;
    }
    match *ty {
        // Its children are defined under its own path.
        "BackboneElement" | "Element" => check_object(obj, path, loc, false, depth + 1, issues),
        // A resource inside (contained, Bundle.entry.resource, …).
        "Resource" => match obj.get("resourceType").and_then(|v| v.as_str()) {
            Some(rt) if crate::r4_types::is_resource_type(rt) => {
                if DEFINED.contains(rt) {
                    check_object(obj, rt, loc, true, depth + 1, issues);
                }
            }
            Some(rt) => issues.push((loc.to_string(), format!("'{loc}': '{rt}' is not a FHIR R4 resource type"))),
            None => issues.push((loc.to_string(), format!("'{loc}' is a resource: it needs a resourceType"))),
        },
        t if DEFINED.contains(t) => check_object(obj, t, loc, false, depth + 1, issues),
        _ => {}
    }
}

/// A primitive value against its type: the R4 regex, and what a regex can't
/// say — a date that exists (not 1990-02-30), and the 32-bit range of
/// integer, positiveInt and unsignedInt.
fn primitive_format(ty: &str, value: &Value) -> Result<(), String> {
    // A number's text as sent (serde_json keeps it, `arbitrary_precision`).
    let text = match value {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        _ => return Ok(()),
    };
    if text.is_empty() {
        return Err("empty (leave the element out)".into());
    }
    if let Some(rx) = PRIMITIVES.get(ty)
        && !rx.is_match(&text)
    {
        return Err(match ty {
            "date" => "a date is YYYY, YYYY-MM or YYYY-MM-DD".into(),
            "dateTime" => "a dateTime is YYYY, YYYY-MM, YYYY-MM-DD or YYYY-MM-DDThh:mm:ss with a time zone (Z or +hh:mm)".into(),
            "instant" => "an instant is YYYY-MM-DDThh:mm:ss with a time zone (Z or +hh:mm)".into(),
            "time" => "a time is hh:mm:ss".into(),
            "integer" | "positiveInt" | "unsignedInt" => "a whole number".into(),
            "decimal" => "a decimal number".into(),
            "uri" | "url" | "canonical" => "no whitespace".into(),
            "code" => "no leading, trailing or double whitespace".into(),
            "id" => "1 to 64 letters, digits, '-' or '.'".into(),
            "oid" => "urn:oid: then digits and dots".into(),
            "uuid" => "urn:uuid: then a lowercase UUID".into(),
            "base64Binary" => "base64 (groups of 4 of A–Z, a–z, 0–9, +, /, =)".into(),
            _ => format!("'{text}' doesn't match the {ty} format"),
        });
    }
    match ty {
        "xhtml" => return narrative(&text),
        "date" | "dateTime" | "instant" if text.len() >= 10 => {
            let ymd = &text[..10];
            let ok = chrono::NaiveDate::parse_from_str(ymd, "%Y-%m-%d").is_ok();
            if !ok {
                return Err(format!("{ymd} is not a day of the calendar"));
            }
        }
        "integer" | "positiveInt" | "unsignedInt" => {
            let n: i64 = text.parse().map_err(|_| "beyond the 32-bit range".to_string())?;
            let (lo, hi) = match ty {
                "positiveInt" => (1, i32::MAX as i64),
                "unsignedInt" => (0, i32::MAX as i64),
                _ => (i32::MIN as i64, i32::MAX as i64),
            };
            if n < lo || n > hi {
                return Err(format!("{n} is outside {lo}..={hi}"));
            }
        }
        _ => {}
    }
    Ok(())
}

/// "a" or "an" before a type name (`an integer`, `an Identifier`).
fn article(ty: &str) -> &'static str {
    if ty.starts_with(['a', 'e', 'i', 'o', 'u', 'A', 'E', 'I', 'O', 'U']) { "an" } else { "a" }
}

/// XHTML elements a narrative may hold (R4 txt-1: the basic HTML 4
/// formatting elements, links and images) — the HL7 validator's list.
const NARRATIVE_ELEMENTS: &[&str] = &[
    "p", "br", "div", "h1", "h2", "h3", "h4", "h5", "h6", "a", "span", "b", "em", "i", "strong", "small", "big", "tt",
    "dfn", "q", "var", "abbr", "acronym", "cite", "blockquote", "hr", "address", "bdo", "kbd", "sub", "sup", "ul", "ol",
    "li", "dl", "dt", "dd", "pre", "table", "caption", "colgroup", "col", "thead", "tr", "tfoot", "tbody", "th", "td",
    "code", "samp", "img", "map", "area",
];
/// Attributes any of them may have.
const NARRATIVE_ATTRIBUTES: &[&str] = &[
    "title", "style", "class", "id", "lang", "dir", "accesskey", "tabindex",
    "span", "width", "align", "valign", "char", "charoff", "abbr", "axis", "headers", "scope", "rowspan", "colspan",
];
/// And `element.attribute` ones.
const NARRATIVE_ELEMENT_ATTRIBUTES: &[&str] = &[
    "a.href", "a.name", "a.charset", "a.type", "a.hreflang", "a.rel", "a.rev", "a.shape", "a.coords",
    "img.src", "img.alt", "img.longdesc", "img.height", "img.width", "img.usemap", "img.ismap", "img.border",
    "blockquote.cite", "q.cite", "map.name", "area.shape", "area.coords", "area.href", "area.nohref", "area.alt",
    "table.summary", "table.width", "table.border", "table.frame", "table.rules", "table.cellspacing",
    "table.cellpadding", "pre.space", "td.nowrap",
];
/// Elements of running text: none may hold a block (`div`, `ul`, `ol`,
/// `table`, `pre`) at any depth — the HL7 validator's rule, as it applies it
/// (asked element by element: `h3`, `hr`, `dl` are allowed in them).
const NARRATIVE_TEXT_ELEMENTS: &[&str] = &[
    "p", "span", "a", "h1", "h2", "h3", "h4", "h5", "h6", "em", "i", "strong", "b", "small", "big", "tt", "code", "pre",
    "q", "cite", "sub", "sup", "dfn", "var", "abbr", "acronym", "kbd", "samp", "bdo", "address", "caption", "dt", "dd",
];
const NARRATIVE_BLOCKS: &[&str] = &["div", "ul", "ol", "table", "pre"];
/// In a paragraph, besides those blocks.
const NARRATIVE_NOT_IN_PARAGRAPH: &[&str] = &["p", "blockquote"];
/// The only children some elements have (no text either).
const NARRATIVE_CHILDREN: &[(&str, &[&str])] = &[
    ("ul", &["li"]),
    ("ol", &["li"]),
    ("dl", &["dt", "dd"]),
    ("table", &["caption", "col", "colgroup", "thead", "tbody", "tfoot", "tr"]),
    ("thead", &["tr"]),
    ("tbody", &["tr"]),
    ("tfoot", &["tr"]),
    ("tr", &["td", "th"]),
    ("map", &["area"]),
];
/// Elements that may not hold one of their own kind, at any depth
/// (`<b><span><b>`): the validator's rule.
const NARRATIVE_NO_RECURSION: &[&str] = &[
    "a", "abbr", "acronym", "b", "bdo", "big", "cite", "code", "dfn", "em", "i", "kbd", "q", "samp", "small", "strong",
    "sub", "sup", "tt", "var",
];
/// Elements with no content.
const NARRATIVE_EMPTY: &[&str] = &["br", "hr", "img", "area"];
/// The parents some elements must have.
const NARRATIVE_PARENTS: &[(&str, &[&str])] = &[
    ("li", &["ul", "ol"]),
    ("dt", &["dl"]),
    ("dd", &["dl"]),
    ("tr", &["table", "thead", "tbody", "tfoot"]),
    ("td", &["tr", "thead", "tfoot"]),
    ("th", &["tr", "thead", "tfoot"]),
    ("caption", &["table"]),
    ("thead", &["table"]),
    ("tbody", &["table"]),
    ("tfoot", &["table"]),
    ("colgroup", &["table"]),
    ("col", &["colgroup", "table"]),
];

/// A narrative's div (R4 txt-1, txt-2), parsed as XML: one `<div>` in the
/// XHTML namespace, well-formed, only the elements and attributes txt-1
/// allows (an allow-list, as the HL7 validator's — a block-list of what can
/// run was bypassed by `<scriptx>` before `<script>`, `<svg/onload=…>`, a tab
/// before `onclick`), the validator's structure (no block in running text, a
/// list item in a list, table parts in a table, no `<b>` in a `<b>` — nor a
/// link in a link —, nothing in `<br>`), no
/// `javascript:`/`vbscript:`/`data:` link (a `data:` image aside), and some
/// content. Duplicate ids are only a warning there, and are accepted.
fn narrative(div: &str) -> Result<(), String> {
    const XHTML: &str = "http://www.w3.org/1999/xhtml";
    // Only XML's own entities (&amp; &lt; &gt; &quot; &apos;) and numeric
    // references: the HL7 validator refuses HTML's named ones (&nbsp; too).
    let doc = roxmltree::Document::parse(div).map_err(|e| {
        let hint = if e.to_string().contains("entity") {
            " — only &amp; &lt; &gt; &quot; &apos; are named; write others as numbers (&#160; for a no-break space)"
        } else {
            ""
        };
        format!("not well-formed XHTML ({e}){hint}")
    })?;
    let root = doc.root_element();
    if root.tag_name().name() != "div" || root.tag_name().namespace() != Some(XHTML) {
        return Err(format!("a narrative is one <div> in the XHTML namespace ({XHTML})"));
    }
    let mut content = false;
    for node in root.descendants() {
        if node.is_text() && node.text().is_some_and(|t| !t.trim().is_empty()) {
            content = true;
        }
        if !node.is_element() {
            continue;
        }
        let name = node.tag_name().name();
        if node.tag_name().namespace() != Some(XHTML) || !NARRATIVE_ELEMENTS.contains(&name) {
            return Err(format!("<{name}> is not allowed in a narrative (txt-1)"));
        }
        if name == "img" {
            content = true;
        }
        let ancestors = || node.ancestors().skip(1).filter(|a| a.is_element()).map(|a| a.tag_name().name());
        if let Some((_, parents)) = NARRATIVE_PARENTS.iter().find(|(n, _)| *n == name) {
            let parent = node.parent_element().map(|p| p.tag_name().name()).unwrap_or("");
            if !parents.contains(&parent) {
                return Err(format!("<{name}> belongs in <{}>, not in <{parent}> (txt-1)", parents.join("> or <")));
            }
        }
        if NARRATIVE_BLOCKS.contains(&name)
            && let Some(text) = ancestors().find(|a| NARRATIVE_TEXT_ELEMENTS.contains(a))
        {
            return Err(format!("<{name}> is a block: not inside <{text}> (txt-1)"));
        }
        if NARRATIVE_NOT_IN_PARAGRAPH.contains(&name) && ancestors().any(|a| a == "p") {
            return Err(format!("<{name}> is not allowed inside a paragraph (txt-1)"));
        }
        if NARRATIVE_NO_RECURSION.contains(&name) && ancestors().any(|a| a == name) {
            return Err(format!("<{name}> inside <{name}> is not allowed (txt-1)"));
        }
        if NARRATIVE_EMPTY.contains(&name)
            && node.children().any(|c| c.is_element() || c.text().is_some_and(|t| !t.trim().is_empty()))
        {
            return Err(format!("<{name}> has no content (txt-1)"));
        }
        if let Some((_, children)) = NARRATIVE_CHILDREN.iter().find(|(n, _)| *n == name) {
            for child in node.children() {
                if child.is_text() && child.text().is_some_and(|t| !t.trim().is_empty()) {
                    return Err(format!("<{name}> holds no text directly, only <{}> (txt-1)", children.join(">, <")));
                }
                if child.is_element() && !children.contains(&child.tag_name().name()) {
                    return Err(format!("<{name}> holds only <{}>, not <{}> (txt-1)", children.join(">, <"), child.tag_name().name()));
                }
            }
        }
        for attr in node.attributes() {
            let an = attr.name();
            let xml = attr.namespace() == Some("http://www.w3.org/XML/1998/namespace");
            // xml:lang anywhere; xml:space on <pre> (the validator's `pre.space`).
            let xml_attribute = xml && (an == "lang" || (an == "space" && name == "pre"));
            let allowed = xml_attribute
                || (attr.namespace().is_none()
                    && (NARRATIVE_ATTRIBUTES.contains(&an) || NARRATIVE_ELEMENT_ATTRIBUTES.contains(&format!("{name}.{an}").as_str())));
            if !allowed {
                return Err(format!("the attribute '{an}' on <{name}> is not allowed in a narrative (txt-1)"));
            }
            if matches!(an, "href" | "src" | "longdesc" | "cite") {
                // The scheme as a browser reads it: whitespace and controls
                // dropped (`java\tscript:`), references already resolved.
                let url: String = attr.value().chars().filter(|c| !c.is_whitespace() && !c.is_control()).collect::<String>().to_ascii_lowercase();
                let image = name == "img" && an == "src" && url.starts_with("data:image/");
                if !image && ["javascript:", "vbscript:", "data:"].iter().any(|s| url.starts_with(s)) {
                    return Err(format!("a {} link is not allowed in a narrative (txt-1)", url.split(':').next().unwrap_or("")));
                }
            }
        }
    }
    if !content {
        return Err("a narrative has some content (txt-2)".into());
    }
    Ok(())
}

fn kind(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "an array",
        Value::Object(_) => "an object",
    }
}

#[cfg(test)]
mod tests {
    use super::validate;
    use serde_json::json;

    fn problems(v: serde_json::Value) -> Vec<String> {
        match validate(&v) {
            Ok(()) => vec![],
            Err(o) => o.issue.into_iter().filter_map(|i| i.diagnostics).collect(),
        }
    }

    #[test]
    fn a_well_formed_resource_passes() {
        assert!(problems(json!({
            "resourceType": "Patient", "id": "p", "meta": {"versionId": "1", "profile": ["http://x"]},
            "text": {"status": "generated", "div": "<div xmlns=\"http://www.w3.org/1999/xhtml\">x</div>"},
            "name": [{"family": "Doe", "given": ["Jo", null], "_given": [null, {"extension": [{"url": "u", "valueString": "x"}]}]}],
            "gender": "female", "_gender": {"id": "g"}, "birthDate": "1990-01-01", "deceasedBoolean": false,
            "contact": [{"name": {"family": "Kin"}, "telecom": [{"system": "phone", "value": "1"}]}],
            "extension": [{"url": "http://e", "valueCodeableConcept": {"coding": [{"system": "s", "code": "c"}]}}],
            "contained": [{"resourceType": "Organization", "id": "o", "name": "Org"}],
            "managingOrganization": {"reference": "#o"}, "multipleBirthInteger": 2
        })).is_empty());
        assert!(problems(json!({
            "resourceType": "Observation", "status": "final", "code": {"text": "x"},
            "valueQuantity": {"value": 1.5, "unit": "mg"},
            "component": [{"code": {"text": "c"}, "valueInteger": 3}],
            "referenceRange": [{"low": {"value": 1}}]
        })).is_empty());
        // contentReference: Questionnaire.item.item is an item.
        assert!(problems(json!({
            "resourceType": "Questionnaire", "status": "draft",
            "item": [{"linkId": "1", "type": "group", "item": [{"linkId": "1.1", "type": "string", "text": "q"}]}]
        })).is_empty());
    }

    #[test]
    fn what_doesnt_fit_is_named() {
        let p = problems(json!({
            "resourceType": "Patient", "bogus": 1, "gender": 5, "name": {"family": "Doe"},
            "birthDate": ["1990"], "contact": [{"nmae": {}}], "deceasedString": "x",
            "contained": [{"resourceType": "Foo"}]
        }));
        let has = |s: &str| p.iter().any(|m| m.contains(s));
        assert!(has("Unknown element 'Patient.bogus'"), "{p:?}");
        assert!(has("'Patient.gender' is a code: a string, not a number"), "{p:?}");
        assert!(has("'Patient.name' repeats"), "{p:?}");
        assert!(has("'Patient.birthDate' has one value"), "{p:?}");
        assert!(has("Unknown element 'Patient.contact[0].nmae'"), "{p:?}");
        assert!(has("Unknown element 'Patient.deceasedString'"), "{p:?}");
        assert!(has("'Foo' is not a FHIR R4 resource type"), "{p:?}");
        // Inside a data type too.
        let p = problems(json!({"resourceType": "Patient", "name": [{"family": ["Doe"]}]}));
        assert!(p.iter().any(|m| m.contains("'Patient.name[0].family' has one value")), "{p:?}");
    }

    #[test]
    fn required_elements() {
        // Missing everywhere: the resource, a backbone, a data type.
        let p = problems(json!({"resourceType": "MedicationAdministration"}));
        for el in ["status", "subject", "effective[x]"] {
            assert!(p.iter().any(|m| m == &format!("'MedicationAdministration.{el}' is required")), "{el}: {p:?}");
        }
        let p = problems(json!({"resourceType": "Observation", "status": "final", "code": {"text": "x"},
            "component": [{"valueString": "a"}], "extension": [{"valueString": "no url"}]}));
        assert!(p.iter().any(|m| m == "'Observation.component[0].code' is required"), "{p:?}");
        assert!(p.iter().any(|m| m == "'Observation.extension[0].url' is required"), "{p:?}");
        // Met by `_status` with an extension (a data-absent-reason) instead of a value.
        assert!(problems(json!({"resourceType": "Observation", "code": {"text": "x"},
            "_status": {"extension": [{"url": "http://hl7.org/fhir/StructureDefinition/data-absent-reason", "valueCode": "unknown"}]}})).is_empty());
        // A choice by any of its types.
        assert!(problems(json!({"resourceType": "MedicationAdministration", "status": "completed",
            "medicationCodeableConcept": {"text": "m"}, "subject": {"reference": "Patient/p"},
            "effectivePeriod": {"start": "2026-01-01"}})).is_empty());
    }

    #[test]
    fn primitive_formats() {
        let bad = |field: &str, v: serde_json::Value| {
            let mut r = json!({"resourceType": "Observation", "status": "final", "code": {"text": "x"}});
            r[field] = v;
            problems(r)
        };
        for (field, v) in [
            ("effectiveDateTime", json!("2026-11-28T25:00:00Z")),
            ("effectiveDateTime", json!("2026-11-28T10:00:00")),
            ("effectiveDateTime", json!("2026-02-30")),
            ("effectiveDateTime", json!("abc")),
            ("valueInteger", json!(5_000_000_000_i64)),
            ("valueInteger", json!(1.5)),
            ("valueTime", json!("25:00:00")),
            ("issued", json!("2026-11-28")),
            ("valueString", json!("")),
        ] {
            let p = bad(field, v.clone());
            assert!(!p.is_empty(), "{field}={v}: accepted");
        }
        for (field, v) in [
            ("effectiveDateTime", json!("2026")),
            ("effectiveDateTime", json!("2026-11")),
            ("effectiveDateTime", json!("2026-11-28T10:00:00+09:00")),
            ("effectiveDateTime", json!("2024-02-29")),
            ("valueInteger", json!(-2147483648_i64)),
            ("valueString", json!("a\u{a0}no-break space, and\nlines")),
            ("issued", json!("2026-11-28T10:00:00.123Z")),
        ] {
            assert!(bad(field, v.clone()).is_empty(), "{field}={v}");
        }
        let p = problems(json!({"resourceType": "Patient", "birthDate": "1990-13-40",
            "identifier": [{"system": "urn mrn", "value": "1"}], "multipleBirthInteger": 1,
            "contact": [{"gender": " male"}]}));
        assert!(p.iter().any(|m| m.contains("'Patient.birthDate' is not a valid date")), "{p:?}");
        assert!(p.iter().any(|m| m.contains("'Patient.identifier[0].system' is not a valid uri")), "{p:?}");
        assert!(p.iter().any(|m| m.contains("'Patient.contact[0].gender' is not a valid code")), "{p:?}");
        // positiveInt (≥ 1) and unsignedInt (≥ 0) bounds.
        let appt = |minutes: i64, priority: i64| problems(json!({"resourceType": "Appointment", "status": "booked",
            "participant": [{"status": "accepted", "actor": {"reference": "Patient/p"}}],
            "minutesDuration": minutes, "priority": priority}));
        let p = appt(0, -1);
        assert!(p.iter().any(|m| m.contains("'Appointment.minutesDuration' is not a valid positiveInt")), "{p:?}");
        assert!(p.iter().any(|m| m.contains("'Appointment.priority' is not a valid unsignedInt")), "{p:?}");
        assert!(appt(1, 0).is_empty(), "{:?}", appt(1, 0));
    }

    #[test]
    fn empty_values_and_two_choices() {
        let p = problems(json!({"resourceType": "Patient", "name": [{}], "link": [{}], "_gender": {}}));
        assert!(p.iter().any(|m| m == "'Patient.name[0]' is empty (leave the element out)"), "{p:?}");
        assert!(p.iter().any(|m| m.contains("'Patient._gender' is empty")), "{p:?}");
        // Patient.link has required `other` and `type`.
        assert!(p.iter().any(|m| m.contains("'Patient.link[0]' is empty")), "{p:?}");
        let p = problems(json!({"resourceType": "Patient", "link": [{"type": "seealso"}]}));
        assert!(p.iter().any(|m| m == "'Patient.link[0].other' is required"), "{p:?}");
        let p = problems(json!({"resourceType": "Observation", "status": "final", "code": {"text": "x"},
            "valueString": "a", "valueBoolean": true}));
        assert!(p.iter().any(|m| m.contains("'Observation.value[x]' has one value")), "{p:?}");
        // `_name` holds an Element: only id and extension.
        let p = problems(json!({"resourceType": "Patient", "_gender": {"bogus": 1}}));
        assert!(p.iter().any(|m| m.contains("Unknown element 'Patient._gender.bogus'")), "{p:?}");
    }

    #[test]
    fn ids_urls_extensions_and_narratives() {
        let p = problems(json!({"resourceType": "Patient",
            "contained": [{"resourceType": "Organization", "id": "a b"}],
            "extension": [{"url": "http://x y", "valueString": "a"},
                          {"url": "http://x", "valueString": "a", "extension": [{"url": "b", "valueString": "c"}]}],
            "name": [{"given": ["a"], "_given": []}]}));
        let has = |s: &str| p.iter().any(|m| m.contains(s));
        assert!(has("'Patient.contained[0].id' is not a valid id"), "{p:?}");
        assert!(has("'Patient.extension[0].url' is not a valid uri"), "{p:?}");
        assert!(has("'Patient.extension[1]' has a value and extensions"), "{p:?}");
        assert!(has("'Patient.name[0]._given' is an empty array"), "{p:?}");
    }

    #[test]
    fn narratives_are_parsed_against_the_txt_1_allow_list() {
        let div = |inner: &str| {
            let d = format!(r#"<div xmlns="http://www.w3.org/1999/xhtml">{inner}</div>"#);
            problems(json!({"resourceType": "Patient", "text": {"status": "generated", "div": d}}))
        };
        for ok in [
            "<p>Jo <b>Doe</b></p><a href=\"http://x\">x</a>",
            "<table border=\"1\"><tr><td colspan=\"2\">a</td></tr></table>",
            "<p xml:lang=\"ja\" lang=\"ja\" style=\"color:red\">山田&#160;太郎 &amp; one onset; javascript: in prose</p>",
            "<pre xml:space=\"preserve\">a</pre>",
            "<span>a<span>b</span></span><h2>a<h2>b</h2></h2>",
            "<table><colgroup><p>a</p></colgroup><tr><td>b</td></tr></table>",
            "<p id=\"d1\">a</p><p id=\"d1\">b</p>",
            "<p>a<b>b</b><h3>c</h3><hr/><dl><dt>t</dt><dd>d</dd></dl></p>",
            "<ul>\n  <li><p>a</p><div>b</div></li>\n</ul><blockquote><p>q</p><div>d</div></blockquote>",
            "<table><caption>c</caption><colgroup><col/></colgroup><thead><tr><th><div>h</div></th></tr></thead><tbody><tr><td><table><tr><td>n</td></tr></table></td></tr></tbody></table>",
            "<img src=\"data:image/png;base64,iVBORw0KGgo=\" alt=\"x\"/>",
        ] {
            assert!(div(ok).is_empty(), "{ok}: {:?}", div(ok));
        }
        // What got through the string search, and the rest of what txt-1 refuses.
        for bad in [
            "<scriptx>a</scriptx><script>alert(1)</script>",
            "<styles>a</styles><style>p{}</style>",
            "<svg/onload=\"alert(1)\">a</svg>",
            "<p\tonclick=\"x()\">a</p>",
            "<p\nonclick=\"x()\">a</p>",
            "<a href=\"javascript&#58;alert(1)\">a</a>",
            "<a href=\"java\tscript:alert(1)\">a</a>",
            "<a href=\"data:text/html,x\">a</a>",
            "<a href=\"vbscript:x\">a</a>",
            "<video>a</video>",
            "<foo>a</foo>",
            "<p formaction=\"x\">a</p>",
            "<p>a</div>",
            "<b><i>a</b></i>",
            " ",
            "<p/>",
            // HTML's named entities: the validator refuses them all.
            "<p>a&nbsp;b</p>",
            "<p>a&Tab;b</p>",
            // The validator's structure.
            "<p><div>a</div></p>",
            "<p>a<span>b<p>c</p></span></p>",
            "<span>a<b>b<div>c</div></b></span>",
            "<h1>a<ol><li>b</li></ol></h1>",
            "<li>a</li>",
            "<dd>a</dd>",
            "<tr><td>a</td></tr>",
            "<table><th>a</th></table>",
            "<table>text<tr><td>a</td></tr></table>",
            "<ul>a<li>b</li></ul>",
            "<ul><p>a</p></ul>",
            "<dl><div>a</div></dl>",
            "<a href=\"#x\"><span><a href=\"#y\">a</a></span></a>",
            "<b>a<b>b</b></b>",
            "<em>a<span><em>b</em></span></em>",
            "<code>a<code>b</code></code>",
            "<p>a<br>b</br></p>",
            "<img src=\"x.png\" alt=\"a\">b</img>",
            "<hr>x</hr><p>a</p>",
            "<table><thead><td>a</td></thead></table>",
            "<p space=\"preserve\">a</p>",
        ] {
            assert!(!div(bad).is_empty(), "{bad:?} accepted");
        }
        let plain = problems(json!({"resourceType": "Patient", "text": {"status": "generated", "div": "plain"}}));
        assert!(!plain.is_empty());
        let no_ns = problems(json!({"resourceType": "Patient", "text": {"status": "generated", "div": "<div>a</div>"}}));
        assert!(no_ns.iter().any(|m| m.contains("XHTML namespace")), "{no_ns:?}");
    }

    /// Official R4 examples with errors of their own: each still fails (so the
    /// list can't go stale), and every other example fits.
    const KNOWN_INVALID: &[(&str, &str)] = &[
        ("Questionnaire/qs1", "nested items without their required linkId"),
        ("SearchParameter/codesystem-extensions-CodeSystem-author", "no base (required)"),
        ("SearchParameter/codesystem-extensions-CodeSystem-effective", "no base (required)"),
        ("SearchParameter/codesystem-extensions-CodeSystem-end", "no base (required)"),
        ("SearchParameter/codesystem-extensions-CodeSystem-keyword", "no base (required)"),
        ("SearchParameter/codesystem-extensions-CodeSystem-workflow", "no base (required)"),
        ("SearchParameter/valueset-extensions-ValueSet-author", "no base (required)"),
        ("SearchParameter/valueset-extensions-ValueSet-effective", "no base (required)"),
        ("SearchParameter/valueset-extensions-ValueSet-end", "no base (required)"),
        ("SearchParameter/valueset-extensions-ValueSet-keyword", "no base (required)"),
        ("SearchParameter/valueset-extensions-ValueSet-workflow", "no base (required)"),
        ("SearchParameter/questionnaireresponse-extensions-QuestionnaireResponse-item-subject", "an id of 67 characters (64 at most)"),
        ("ActivityDefinition/heart-valve-replacement", "a narrative without content (txt-2)"),
        ("ActivityDefinition/blood-tubes-supply", "a narrative without content (txt-2)"),
        ("EventDefinition/example", "a narrative without content (txt-2)"),
        ("Questionnaire/zika-virus-exposure-assessment", "a narrative without content (txt-2)"),
    ];

    /// The R4 examples (examples-json.zip, as NDJSON in `R4_EXAMPLES`) fit,
    /// but for `KNOWN_INVALID`:
    /// `R4_EXAMPLES=all.ndjson cargo test -p sazare-core -- --ignored r4_examples`.
    #[test]
    #[ignore]
    fn r4_examples_fit() {
        let path = std::env::var("R4_EXAMPLES").expect("R4_EXAMPLES: the R4 examples as NDJSON");
        let text = std::fs::read_to_string(path).unwrap();
        let mut failed = vec![];
        let mut known_failing = std::collections::HashSet::new();
        let mut n = 0;
        for line in text.lines() {
            let v: serde_json::Value = serde_json::from_str(line).unwrap();
            n += 1;
            let id = format!("{}/{}", v["resourceType"].as_str().unwrap_or("?"), v["id"].as_str().unwrap_or("?"));
            let known = KNOWN_INVALID.iter().any(|(k, _)| *k == id);
            match validate(&v) {
                Err(_) if known => {
                    known_failing.insert(id);
                }
                Err(o) => failed.push(format!("{id}: {}", o.issue.iter().filter_map(|i| i.diagnostics.clone()).collect::<Vec<_>>().join("; "))),
                Ok(()) if known => failed.push(format!("{id}: listed as invalid, but it fits now — take it off KNOWN_INVALID")),
                Ok(()) => {}
            }
        }
        assert!(failed.is_empty(), "{} of {n} examples:\n{}", failed.len(), failed.join("\n"));
        let missing: Vec<_> = KNOWN_INVALID.iter().filter(|(k, _)| !known_failing.contains(*k)).collect();
        assert!(missing.is_empty(), "KNOWN_INVALID lists examples not in the file: {missing:?}");
    }
}
