//! Build index v2 from an existing resources.sqlite and time searches on it.
//! usage: cargo run --release -p sazare-store --example v2_bench -- <resources.sqlite> <out-v2.sqlite>
use std::time::Instant;

use sazare_core::{SearchParamRegistry, SearchQuery};
use sazare_store::index_v2::IndexV2;
use sazare_store::{resource_text, IndexBuilder, IndexUpdate, SqliteStore};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let (src, out) = (&args[1], &args[2]);
    let registry = SearchParamRegistry::new();
    let store = SqliteStore::open(src).unwrap();
    let v2 = IndexV2::open(out).unwrap();
    if std::env::var("SKIP_BUILD").is_err() {
        let t = Instant::now();
        let mut n = 0u64;
        for (rt, _) in store.count_by_type().unwrap() {
            let mut after = String::new();
            loop {
                let page = store.list_page(&rt, &after, 1000).unwrap();
                let Some((last, _)) = page.last() else { break };
                after = last.clone();
                let updates: Vec<IndexUpdate> = page.iter().map(|(id, data)| {
                    let r: serde_json::Value = serde_json::from_slice(data).unwrap();
                    IndexUpdate { resource_type: rt.clone(), resource_id: id.clone(),
                        entries: IndexBuilder::extract_indices_with_registry(&registry, &rt, &r), text: resource_text(&r) }
                }).collect();
                v2.replace_many(&updates).unwrap();
                n += page.len() as u64;
                if n.is_multiple_of(500_000) {
                    eprintln!("{n} resources, {:.0}/s", n as f64 / t.elapsed().as_secs_f64());
                }
            }
        }
        println!("build: {n} resources in {:.0} s ({:.0}/s)", t.elapsed().as_secs_f64(), n as f64 / t.elapsed().as_secs_f64());
    }
    let queries = [
        ("Observation", "_count=20"),
        ("Observation", "_sort=-date&_count=20"),
        ("Observation", "code=8867-4&_sort=-date&_count=20"),
        ("Patient", "_has:Observation:subject:code=8867-4&_count=20"),
        ("Observation", "category=vital-signs&status=final&_count=20"),
        ("Observation", "patient=p42&code=http://loinc.org|8867-4"),
        ("Observation", "patient=p42&code=8867-4&date=ge2020-01-01"),
        ("Observation", "code=8867-4&date=ge2020-01-01&_count=20"),
        ("Patient", "gender=female&birthdate=ge1980&_count=20"),
        ("Patient", "family=smith&_count=20"),
        ("Observation", "subject:Patient.family=smith&_count=20"),
    ];
    for (rt, q) in queries {
        let parsed = SearchQuery::parse_with_registry(q, Some(rt), &registry).unwrap();
        let mut times = vec![];
        let mut total = 0;
        for _ in 0..3 {
            let t = Instant::now();
            let (_, tot) = v2.search_with_total(rt, &parsed).unwrap();
            times.push(t.elapsed().as_secs_f64() * 1000.0);
            total = tot;
        }
        times.sort_by(|a, b| a.partial_cmp(b).unwrap());
        println!("{:10.1} ms  {:>9}  {rt}?{q}", times[1], total);
    }
}
