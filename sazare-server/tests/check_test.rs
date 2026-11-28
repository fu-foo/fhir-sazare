//! `sazare-server check`: stored resources against the R4 rules, read-only.

use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_sazare-server");

#[test]
fn check_lists_what_doesnt_fit_and_leaves_the_store_alone() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    let config = dir.path().join("config.yaml");
    std::fs::write(&config, format!("storage:\n  data_dir: \"{}\"\n", data.display())).unwrap();
    {
        // Stored as an older version would have: unchecked.
        let store = sazare_store::SqliteStore::open(data.join("resources.sqlite")).unwrap();
        store.put("Patient", "ok", br#"{"resourceType":"Patient","id":"ok","birthDate":"1990-02-28"}"#).unwrap();
        store.put("Patient", "bad", br#"{"resourceType":"Patient","id":"bad","birthDate":"1990-02-30","name":[{}]}"#).unwrap();
        store.put("Observation", "o", br#"{"resourceType":"Observation","id":"o","code":{"text":"x"}}"#).unwrap();
    }
    let modified = std::fs::metadata(data.join("resources.sqlite")).unwrap().modified().unwrap();
    let out = Command::new(BIN).args(["check", "--config", config.to_str().unwrap()]).output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{stdout}{stderr}");
    assert!(stdout.contains("Patient/bad: 'Patient.birthDate' is not a valid date"), "{stdout}");
    assert!(stdout.contains("Patient/bad: 'Patient.name[0]' is empty"), "{stdout}");
    assert!(stdout.contains("Observation/o: 'Observation.status' is required"), "{stdout}");
    assert!(!stdout.contains("Patient/ok"), "{stdout}");
    assert!(stderr.contains("2 of 3 resources don't meet the R4 rules"), "{stderr}");
    // Read-only: nothing written.
    assert_eq!(std::fs::metadata(data.join("resources.sqlite")).unwrap().modified().unwrap(), modified);

    // All good: exit 0.
    let good = tempfile::tempdir().unwrap();
    let gdata = good.path().join("data");
    std::fs::create_dir_all(&gdata).unwrap();
    sazare_store::SqliteStore::open(gdata.join("resources.sqlite")).unwrap()
        .put("Patient", "ok", br#"{"resourceType":"Patient","id":"ok"}"#).unwrap();
    let gconfig = good.path().join("config.yaml");
    std::fs::write(&gconfig, format!("storage:\n  data_dir: \"{}\"\n", gdata.display())).unwrap();
    let out = Command::new(BIN).args(["check", "--config", gconfig.to_str().unwrap()]).output().unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(String::from_utf8_lossy(&out.stderr).contains("The 1 resource meets the R4 rules."));
}
