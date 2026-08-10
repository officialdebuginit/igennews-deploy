use std::{collections::HashSet, fs, path::PathBuf};

#[test]
fn legacy_endpoint_ledger_is_unique_and_unowned_by_rust() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let contents = fs::read_to_string(root.join("contracts/legacy-endpoints.json"))
        .expect("read legacy endpoint ledger");
    let endpoints: Vec<serde_json::Value> =
        serde_json::from_str(&contents).expect("parse legacy endpoint ledger");

    assert_eq!(endpoints.len(), 161, "review all legacy API count drift");
    let mut identities = HashSet::new();
    for endpoint in endpoints {
        let method = endpoint["method"].as_str().expect("endpoint method");
        let path = endpoint["path"].as_str().expect("endpoint path");
        assert!(
            identities.insert(format!("{method} {path}")),
            "duplicate endpoint"
        );
        assert_eq!(endpoint["owner"], "legacy");
        assert_eq!(endpoint["migration_status"], "not_started");
        assert!(endpoint["operation_id"].is_string());
        assert!(endpoint["migration_risk"].is_string());
    }
}
