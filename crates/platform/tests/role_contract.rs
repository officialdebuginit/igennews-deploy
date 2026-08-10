#[test]
fn role_bootstrap_denies_cluster_admin_and_runtime_ddl() {
    let sql = include_str!("../../../deploy/postgres/roles.sql");
    for role in [
        "meridian_owner",
        "meridian_migrator",
        "meridian_app",
        "meridian_worker",
        "meridian_readonly",
    ] {
        assert!(sql.contains(role), "missing role contract for {role}");
    }
    assert_eq!(sql.matches("NOSUPERUSER").count(), 5);
    assert_eq!(sql.matches("NOCREATEDB").count(), 5);
    assert_eq!(sql.matches("NOCREATEROLE").count(), 5);
    assert!(sql.contains("REVOKE ALL ON SCHEMA meridian FROM PUBLIC"));
    assert!(sql.contains("GRANT USAGE, CREATE ON SCHEMA meridian TO meridian_migrator"));
    assert!(!sql.contains("GRANT USAGE, CREATE ON SCHEMA meridian TO meridian_app"));
}
