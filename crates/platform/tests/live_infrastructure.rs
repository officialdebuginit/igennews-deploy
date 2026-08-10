use meridian_platform::{Platform, PlatformConfig};

#[tokio::test]
#[ignore = "requires the shared PostgreSQL, PgBouncer, and RustFS stack"]
async fn shared_infrastructure_round_trips() {
    let platform = Platform::initialize(PlatformConfig::from_env().expect("valid live config"))
        .await
        .expect("platform initializes");

    let readiness = platform.readiness().await;
    assert_eq!(readiness.status, "ready");
    assert!(readiness.dependencies.pooled_database);
    assert!(readiness.dependencies.direct_database);
    assert!(readiness.dependencies.object_storage);

    platform
        .notification_round_trip()
        .await
        .expect("direct LISTEN/NOTIFY round trip");
    platform
        .storage_round_trip()
        .await
        .expect("RustFS put/head/get/presign/delete round trip");
}
