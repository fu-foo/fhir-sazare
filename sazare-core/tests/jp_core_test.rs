//! JP Core profile validation.
//!
//! Guards against shipping an embedded JP Core profile that would reject
//! conforming data: every official example resource (taken from the
//! `jp-core.r4` package) for an embedded profile must pass validation.

use sazare_core::profile_loader::ProfileLoader;
use sazare_core::validation::{validate_resource_all_phases, ProfileRegistry, TerminologyRegistry};
use serde_json::Value;
use std::fs;
use std::path::PathBuf;

fn jp_core_registry() -> ProfileRegistry {
    let mut reg = ProfileRegistry::new();
    reg.load_profiles(ProfileLoader::get_embedded_jp_core_profiles());
    reg
}

#[test]
fn official_jp_core_examples_validate() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/jp-core");
    let reg = jp_core_registry();
    let term = TerminologyRegistry::new();

    let mut checked = 0;
    for entry in fs::read_dir(&dir).expect("fixtures dir should exist") {
        let path = entry.unwrap().path();
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        let resource: Value =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        let result = validate_resource_all_phases(&resource, &reg, &term);
        assert!(
            result.is_ok(),
            "official JP Core example {:?} should validate, got: {:?}",
            path.file_name().unwrap(),
            result.err()
        );
        checked += 1;
    }
    assert!(checked >= 12, "expected >= 12 example fixtures, checked {}", checked);
}
