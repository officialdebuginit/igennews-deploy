//! Integrity checks for the differential-parity fixtures
//! (`contracts/parity-fixtures.json`, consumed by `scripts/differential-parity.sh
//! --fixtures`). A fixture that names an operation the ledger does not know, or
//! that disagrees with the ledger's method/path, would silently probe nothing —
//! so the fixture set is pinned to the reviewed ledger here.

use std::{
    collections::{HashMap, HashSet},
    fs,
    path::PathBuf,
};

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn read_json(name: &str) -> Vec<serde_json::Value> {
    let path = workspace_root().join(name);
    let contents = fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {name}: {e}"));
    serde_json::from_str(&contents).unwrap_or_else(|e| panic!("parse {name}: {e}"))
}

#[test]
fn parity_fixtures_reference_real_unique_ledger_operations() {
    // Ledger operation_id -> "METHOD PATH", the authoritative identity.
    let ledger: HashMap<String, String> = read_json("contracts/legacy-endpoints.json")
        .into_iter()
        .map(|op| {
            let id = op["operation_id"].as_str().expect("operation_id").to_owned();
            let method = op["method"].as_str().expect("method");
            let path = op["path"].as_str().expect("path");
            (id, format!("{method} {path}"))
        })
        .collect();

    let fixtures = read_json("contracts/parity-fixtures.json");
    assert!(!fixtures.is_empty(), "parity fixtures must not be empty");

    let mut seen = HashSet::new();
    for fixture in &fixtures {
        let id = fixture["operation_id"]
            .as_str()
            .expect("fixture operation_id");
        assert!(seen.insert(id.to_owned()), "duplicate fixture for {id}");

        let ledger_identity = ledger
            .get(id)
            .unwrap_or_else(|| panic!("fixture {id} is not a known ledger operation"));

        let method = fixture["method"].as_str().expect("fixture method");
        // The fixture path may append a query string; compare on the path prefix.
        let path = fixture["path"].as_str().expect("fixture path");
        let base_path = path.split('?').next().unwrap_or(path);
        assert_eq!(
            format!("{method} {base_path}"),
            *ledger_identity,
            "fixture {id} disagrees with the ledger method/path"
        );

        assert!(
            fixture["auth"].is_boolean() || fixture.get("auth").is_none(),
            "fixture {id} 'auth' must be a boolean when present"
        );
    }
}
