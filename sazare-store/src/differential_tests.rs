//! Differential tests for search (milestone 1 of design/search-index-v2.md).
//!
//! Generated datasets × generated queries, run through the search engine and
//! through a reference model that evaluates the same query directly against
//! each resource's extracted index values, by the FHIR rules the engine is
//! meant to implement. Extraction (`IndexBuilder`) is shared on purpose: what
//! is under test is the index and the executor — the part search index v2
//! replaces. When v2 exists it runs against the same model (and against v1).
//!
//! Deterministic seeds; a failure prints the seed, the query and the difference.

use std::collections::{BTreeSet, HashMap};

use sazare_core::{SearchParamRegistry, SearchParamType, SearchParameter, SearchQuery};
use serde_json::{json, Value};

use crate::sqlite_index::fhir_date_range;
use crate::index_v2::IndexV2;
use crate::{resource_text, IndexBuilder, IndexUpdate, SearchExecutor, SearchIndex, SqliteStore};

/// A search implementation under test: the v1 executor or index v2.
trait Backend {
    fn search(&self, rt: &str, q: &SearchQuery) -> Vec<String>;
    fn search_with_total(&self, rt: &str, q: &SearchQuery) -> (Vec<String>, usize);
}

impl Backend for SearchExecutor<'_> {
    fn search(&self, rt: &str, q: &SearchQuery) -> Vec<String> {
        SearchExecutor::search(self, rt, q).unwrap()
    }
    fn search_with_total(&self, rt: &str, q: &SearchQuery) -> (Vec<String>, usize) {
        SearchExecutor::search_with_total(self, rt, q).unwrap()
    }
}

impl Backend for IndexV2 {
    fn search(&self, rt: &str, q: &SearchQuery) -> Vec<String> {
        IndexV2::search(self, rt, q).unwrap()
    }
    fn search_with_total(&self, rt: &str, q: &SearchQuery) -> (Vec<String>, usize) {
        IndexV2::search_with_total(self, rt, q).unwrap()
    }
}

/// A small deterministic PRNG (xorshift64*), so the tests need no dependency
/// and a seed reproduces a failure exactly.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len())]
    }
    fn chance(&mut self, percent: u64) -> bool {
        self.next() % 100 < percent
    }
}

const FAMILIES: &[&str] = &["Smith", "smith", "SMITH", "Smithers", "Garcia", "García", "Chen", "Okafor", "O'Brien", "Ng", "外来", "見本"];
const GIVENS: &[&str] = &["Ada", "Ann", "Anna", "Bo", "Camille", "Jan"];
const GENDERS: &[&str] = &["male", "female", "other", "unknown"];
const STATUSES: &[&str] = &["final", "preliminary", "amended", "cancelled"];
const CODES: &[&str] = &["8867-4", "8310-5", "9279-1", "29463-7"];
const LOINC: &str = "http://loinc.org";
const LOCAL: &str = "urn:local:codes";
const CATEGORIES: &[&str] = &["vital-signs", "laboratory", "survey"];
const ID_SYSTEMS: &[&str] = &["urn:mrn", "urn:ssn"];
const DATES: &[&str] = &[
    "2019", "2020-03", "2020-03-15", "2020-03-15T10:00:00Z", "2021-12-31", "2022-06-01T08:30:00+09:00", "2023",
];

fn date(rng: &mut Rng) -> String {
    rng.pick(DATES).to_string()
}

/// One generated dataset: Patients and Observations with references, repeated
/// and missing values, case and accent variants, periods (some open-ended).
fn dataset(rng: &mut Rng, patients: usize, observations: usize) -> Vec<Value> {
    let mut out = Vec::new();
    for i in 0..patients {
        let mut p = json!({"resourceType": "Patient", "id": format!("p{i}")});
        if rng.chance(90) {
            let mut names = vec![];
            for _ in 0..1 + rng.below(2) {
                let mut n = json!({"family": rng.pick(FAMILIES)});
                if rng.chance(70) {
                    n["given"] = json!([rng.pick(GIVENS), rng.pick(GIVENS)][..1 + rng.below(2)]);
                }
                names.push(n);
            }
            p["name"] = json!(names);
        }
        if rng.chance(80) {
            p["gender"] = json!(rng.pick(GENDERS));
        }
        if rng.chance(75) {
            p["birthDate"] = json!(rng.pick(&["1980", "1980-05", "1980-05-17", "1999-12-31", "2001-01-01"]));
        }
        if rng.chance(70) {
            let mut id = json!({"value": format!("MRN{}", rng.below(20))});
            if rng.chance(80) {
                id["system"] = json!(rng.pick(ID_SYSTEMS));
            }
            p["identifier"] = json!([id]);
        }
        out.push(p);
    }
    for i in 0..observations {
        let mut o = json!({"resourceType": "Observation", "id": format!("o{i}")});
        if rng.chance(85) {
            o["status"] = json!(rng.pick(STATUSES));
        }
        let mut codings = vec![];
        for _ in 0..1 + rng.below(2) {
            let mut c = json!({"code": rng.pick(CODES)});
            match rng.below(3) {
                0 => c["system"] = json!(LOINC),
                1 => c["system"] = json!(LOCAL),
                _ => {}
            }
            codings.push(c);
        }
        o["code"] = json!({"coding": codings});
        if rng.chance(70) {
            o["category"] = json!([{"coding": [{"system": "http://terminology.hl7.org/CodeSystem/observation-category", "code": rng.pick(CATEGORIES)}]}]);
        }
        if rng.chance(90) {
            o["subject"] = json!({"reference": format!("Patient/p{}", rng.below(patients.max(1)))});
        }
        match rng.below(4) {
            0 => {}
            1 => o["effectivePeriod"] = json!({"start": date(rng)}),
            2 => o["effectivePeriod"] = json!({"start": "2020-01-01", "end": date(rng)}),
            _ => o["effectiveDateTime"] = json!(date(rng)),
        }
        out.push(o);
    }
    out
}

type Entries = Vec<(String, String, String, Option<String>)>;

/// The reference model: does `entries` (one resource's extracted values) match
/// one parameter of a query?
fn param_matches(entries: &Entries, p: &SearchParameter) -> bool {
    let has = |name: &str| entries.iter().any(|(n, _, _, _)| n == name);
    match p.modifier.as_deref() {
        Some("missing") => return (p.value == "true") != has(&p.name),
        Some("not") => {
            let mut inner = p.clone();
            inner.modifier = None;
            return !param_matches(entries, &inner);
        }
        _ => {}
    }
    p.value.split(',').map(str::trim).filter(|v| !v.is_empty()).any(|v| {
        entries.iter().filter(|(n, _, _, _)| *n == p.name).any(|(_, _, value, system)| match p.param_type {
            SearchParamType::Token => match v.split_once('|') {
                Some(("", code)) => value == code && system.is_none(),
                Some((sys, "")) => system.as_deref() == Some(sys),
                Some((sys, code)) => value == code && system.as_deref() == Some(sys),
                None => value == v,
            },
            SearchParamType::String => match p.modifier.as_deref() {
                Some("exact") => value == v,
                Some("contains") => value.to_lowercase().contains(&v.to_lowercase()),
                _ => value.to_lowercase().starts_with(&v.to_lowercase()),
            },
            SearchParamType::Reference => value == v,
            SearchParamType::Date => {
                let Some((qs, qe)) = fhir_date_range(v) else { return false };
                let (s, e) = match value.split_once('/') {
                    Some((lo, hi)) => (
                        fhir_date_range(lo).map(|r| r.0),
                        if hi.is_empty() { Some(i64::MAX) } else { fhir_date_range(hi).map(|r| r.1) },
                    ),
                    None => match fhir_date_range(value) {
                        Some((a, b)) => (Some(a), Some(b)),
                        None => (None, None),
                    },
                };
                let (Some(s), Some(e)) = (s, e) else { return false };
                match p.prefix.as_deref().unwrap_or("eq") {
                    "ne" => s < qs || e > qe,
                    "gt" => e > qe,
                    "ge" => e > qs,
                    "lt" => s < qs,
                    "le" => s < qe,
                    "sa" => s >= qe,
                    "eb" => e <= qs,
                    "ap" => s < qe && e > qs,
                    _ => s >= qs && e <= qe,
                }
            }
            SearchParamType::Number => false,
        })
    })
}

/// The reference model for a whole query: every parameter, every chain (one
/// hop) and every `_has` must hold.
fn query_matches(
    all: &HashMap<(String, String), Entries>,
    resource_type: &str,
    id: &str,
    q: &SearchQuery,
) -> bool {
    let e = &all[&(resource_type.to_string(), id.to_string())];
    let plain = |name: &str, value: &str, param_type: &SearchParamType| SearchParameter {
        name: name.to_string(),
        value: value.to_string(),
        modifier: None,
        prefix: None,
        param_type: param_type.clone(),
    };
    q.parameters.iter().all(|p| param_matches(e, p))
        && q.chain_parameters.iter().all(|c| {
            let link = &c.links[0];
            let target = plain(&c.target_param, &c.value, &c.target_param_type);
            e.iter()
                .filter(|(n, _, _, _)| *n == link.reference_param)
                .filter_map(|(_, _, v, _)| v.strip_prefix(&format!("{}/", link.target_type)))
                .any(|tid| {
                    all.get(&(link.target_type.clone(), tid.to_string()))
                        .is_some_and(|te| param_matches(te, &target))
                })
        })
        && q.has_parameters.iter().all(|h| {
            let inner = plain(&h.target_param, &h.value, &h.target_param_type);
            let me = format!("{resource_type}/{id}");
            all.iter().any(|((t, _), se)| {
                *t == h.source_type
                    && se.iter().any(|(n, _, v, _)| *n == h.reference_param && *v == me)
                    && param_matches(se, &inner)
            })
        })
}

/// A random query for `resource_type`, as a query string.
fn query(rng: &mut Rng, resource_type: &str, patients: usize) -> String {
    let mut parts = vec![];
    for _ in 0..1 + rng.below(3) {
        let part = match (resource_type, rng.below(6)) {
            ("Patient", 0) => {
                let m = rng.pick(&["", ":exact", ":contains"]);
                let v = rng.pick(&["smi", "Smith", "smith", "gar", "García", "garc", "Ng", "外", "見本", "obr", "o'b"]);
                format!("family{m}={v}")
            }
            ("Patient", 1) => format!("given={}", rng.pick(&["a", "An", "ann", "bo", "camille"])),
            ("Patient", 2) => format!("gender={}", if rng.chance(30) { "male,female".into() } else { rng.pick(GENDERS).to_string() }),
            ("Patient", 3) => format!("birthdate={}{}", rng.pick(&["", "ge", "le", "gt", "lt", "ne", "ap", "sa", "eb"]), rng.pick(&["1980", "1980-05", "1980-05-17", "2000"])),
            ("Patient", 4) if rng.chance(40) => match rng.below(2) {
                0 => format!("_has:Observation:subject:status={}", rng.pick(STATUSES)),
                _ => format!("_has:Observation:subject:code={}", rng.pick(CODES)),
            },
            ("Patient", 4) => {
                let v = format!("MRN{}", rng.below(20));
                match rng.below(4) {
                    0 => format!("identifier={}|{v}", rng.pick(ID_SYSTEMS)),
                    1 => format!("identifier=|{v}"),
                    2 => format!("identifier={}|", rng.pick(ID_SYSTEMS)),
                    _ => format!("identifier={v}"),
                }
            }
            ("Patient", _) => format!("{}:{}={}", rng.pick(&["gender", "birthdate", "family"]), rng.pick(&["missing", "missing", "not"]),
                                     rng.pick(&["true", "false"])).replace(":not=true", ":not=male").replace(":not=false", ":not=female"),
            (_, 0) => format!("status={}", if rng.chance(30) { "final,amended".into() } else { rng.pick(STATUSES).to_string() }),
            (_, 1) => match rng.below(4) {
                0 => format!("code={LOINC}|{}", rng.pick(CODES)),
                1 => format!("code=|{}", rng.pick(CODES)),
                2 => format!("code={LOCAL}|"),
                _ => format!("code={}", rng.pick(CODES)),
            },
            (_, 2) => format!("category={}", rng.pick(CATEGORIES)),
            (_, 3) => {
                let p = rng.below(patients.max(1));
                if rng.chance(50) { format!("subject=Patient/p{p}") } else { format!("patient=p{p}") }
            }
            (_, 4) if rng.chance(40) => match rng.below(2) {
                0 => format!("subject:Patient.family={}", rng.pick(&["smi", "Gar", "ng", "o'b"])),
                _ => format!("subject:Patient.gender={}", rng.pick(GENDERS)),
            },
            (_, 4) => format!("date={}{}", rng.pick(&["", "ge", "le", "gt", "lt", "ne", "ap", "sa", "eb"]), date(rng)),
            (_, _) => format!("{}:{}", rng.pick(&["status", "category", "date"]), rng.pick(&["missing=true", "missing=false", "not=final"]))
                .replace("category:not=final", "category:not=laboratory")
                .replace("date:not=final", "date:missing=true"),
        };
        parts.push(part);
    }
    parts.join("&")
}

/// Build a store and index for `resources`; return them with each resource's
/// extracted entries.
#[allow(clippy::type_complexity)]
fn load(resources: &[Value], registry: &SearchParamRegistry) -> (SqliteStore, SearchIndex, IndexV2, HashMap<(String, String), Entries>) {
    let store = SqliteStore::open(":memory:").unwrap();
    let index = SearchIndex::open(":memory:").unwrap();
    let mut entries = HashMap::new();
    let mut updates = vec![];
    for r in resources {
        let rt = r["resourceType"].as_str().unwrap().to_string();
        let id = r["id"].as_str().unwrap().to_string();
        store.put_with_version(&rt, &id, "1", r.to_string().as_bytes()).unwrap();
        let e = IndexBuilder::extract_indices_with_registry(registry, &rt, r);
        updates.push(IndexUpdate { resource_type: rt.clone(), resource_id: id.clone(), entries: e.clone(), text: resource_text(r) });
        entries.insert((rt, id), e);
    }
    index.replace_many(&updates).unwrap();
    let v2 = IndexV2::open(":memory:").unwrap();
    v2.replace_many(&updates).unwrap();
    (store, index, v2, entries)
}

#[derive(Clone, Copy)]
enum Which {
    V1,
    V2,
}

fn run(which: Which, seed: u64, patients: usize, observations: usize, queries: usize) {
    let mut rng = Rng(seed.max(1));
    let registry = SearchParamRegistry::new();
    let resources = dataset(&mut rng, patients, observations);
    let (store, index, v2, entries) = load(&resources, &registry);
    let v1 = SearchExecutor::new(&store, &index);
    let exec: &dyn Backend = match which {
        Which::V1 => &v1,
        Which::V2 => &v2,
    };
    let mut failures = vec![];
    let mut non_empty = 0;
    for _ in 0..queries {
        let rt = if rng.chance(40) { "Patient" } else { "Observation" };
        let q = query(&mut rng, rt, patients);
        let parsed = match SearchQuery::parse_with_registry(&q, Some(rt), &registry) {
            Ok(p) => p,
            Err(e) => panic!("seed {seed}: generated query {rt}?{q} doesn't parse: {e}"),
        };
        let got: BTreeSet<String> = exec.search(rt, &parsed).into_iter().collect();
        let want: BTreeSet<String> = entries
            .keys()
            .filter(|(t, id)| t == rt && query_matches(&entries, rt, id, &parsed))
            .map(|(_, id)| id.clone())
            .collect();
        if !want.is_empty() {
            non_empty += 1;
        }
        if got != want {
            let extra: Vec<_> = got.difference(&want).take(5).collect();
            let missing: Vec<_> = want.difference(&got).take(5).collect();
            failures.push(format!("{rt}?{q}\n    engine has extra {extra:?}, misses {missing:?}"));
        }
    }
    // The generator must exercise matches, not mostly empty results.
    assert!(non_empty * 3 >= queries, "seed {seed}: only {non_empty} of {queries} queries match anything");
    assert!(
        failures.is_empty(),
        "seed {seed}: {} of {queries} queries differ from the reference model:\n  {}",
        failures.len(),
        failures.iter().take(10).cloned().collect::<Vec<_>>().join("\n  ")
    );
}

#[test]
fn search_matches_the_reference_model() {
    for seed in 1..=8u64 {
        run(Which::V1, seed * 7919, 60, 240, 400);
    }
}

#[test]
fn index_v2_matches_the_reference_model() {
    for seed in 1..=8u64 {
        run(Which::V2, seed * 7919, 60, 240, 400);
    }
}

/// The sort key a resource has for `param` in the reference model: lowest
/// value ascending, highest descending (dates by range start, text
/// case-insensitively); `None` without a value.
fn sort_key(entries: &Entries, param: &str, descending: bool) -> Option<String> {
    let keys = entries.iter().filter(|(n, _, _, _)| n == param).map(|(_, t, v, _)| {
        if t == "date" {
            let start = v.split('/').next().and_then(fhir_date_range).map(|r| r.0).unwrap_or(i64::MIN);
            format!("{:020}", start as i128 - i64::MIN as i128)
        } else {
            v.to_lowercase()
        }
    });
    if descending { keys.max() } else { keys.min() }
}

fn run_sorted(which: Which, seed: u64, patients: usize, observations: usize, queries: usize) {
    let mut rng = Rng(seed.max(1));
    let registry = SearchParamRegistry::new();
    let resources = dataset(&mut rng, patients, observations);
    let (store, index, v2, entries) = load(&resources, &registry);
    let v1 = SearchExecutor::new(&store, &index);
    let exec: &dyn Backend = match which {
        Which::V1 => &v1,
        Which::V2 => &v2,
    };
    for _ in 0..queries {
        let rt = if rng.chance(40) { "Patient" } else { "Observation" };
        let key = if rt == "Patient" { *rng.pick(&["family", "birthdate"]) } else { "date" };
        let desc = rng.chance(50);
        let filter = if rng.chance(50) { format!("{}&", query(&mut rng, rt, patients)) } else { String::new() };
        let q = format!("{filter}_sort={}{key}", if desc { "-" } else { "" });
        let mut parsed = SearchQuery::parse_with_registry(&q, Some(rt), &registry).unwrap();
        let (all, total) = exec.search_with_total(rt, &parsed);
        // Total and membership: the reference model.
        let want: BTreeSet<String> = entries
            .keys()
            .filter(|(t, id)| t == rt && query_matches(&entries, rt, id, &parsed))
            .map(|(_, id)| id.clone())
            .collect();
        assert_eq!(total, want.len(), "seed {seed}: total of {rt}?{q}");
        assert_eq!(all.iter().cloned().collect::<BTreeSet<_>>(), want, "seed {seed}: members of {rt}?{q}");
        // Order: keys in order, resources without a value last. (Ties may come in
        // any order: the order of equal keys is unspecified.)
        let keys: Vec<Option<String>> =
            all.iter().map(|id| sort_key(&entries[&(rt.to_string(), id.clone())], key, desc)).collect();
        let first_missing = keys.iter().position(Option::is_none).unwrap_or(keys.len());
        assert!(keys[first_missing..].iter().all(Option::is_none), "seed {seed}: missing values not last in {rt}?{q}");
        let present: Vec<&String> = keys[..first_missing].iter().flatten().collect();
        assert!(
            present.windows(2).all(|w| if desc { w[0] >= w[1] } else { w[0] <= w[1] }),
            "seed {seed}: {rt}?{q} out of order: {present:?}"
        );
        // Paging: consecutive pages are the full order, cut up.
        let size = 1 + rng.below(7);
        let mut paged = vec![];
        for offset in (0..all.len() + size).step_by(size) {
            parsed.count = Some(size);
            parsed.offset = Some(offset);
            let (page, page_total) = exec.search_with_total(rt, &parsed);
            assert_eq!(page_total, total, "seed {seed}: total on a page of {rt}?{q}");
            paged.extend(page);
        }
        assert_eq!(paged, all, "seed {seed}: pages of {rt}?{q} (size {size})");
    }
}

#[test]
fn sorted_searches_follow_the_keys_and_pages_add_up() {
    for seed in 1..=6u64 {
        run_sorted(Which::V1, seed * 104_729, 50, 200, 60);
    }
}

#[test]
fn index_v2_sorts_and_pages_like_the_model() {
    for seed in 1..=6u64 {
        run_sorted(Which::V2, seed * 104_729, 50, 200, 60);
    }
}
