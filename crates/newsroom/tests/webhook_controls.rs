//! Live-database coverage for the advanced webhook controls (migration 0040):
//! custom headers on create, secret rotation, enable/disable, a partial config
//! patch, reserved-header rejection, and the test-ping path that hand-creates a
//! `webhook.ping` delivery. These exercise SQL and authz, so they run against a
//! real database and clean up after themselves by actor scope.

use meridian_newsroom::{
    Actor, NewsroomService, Role,
    webhooks::{EndpointInput, EndpointPatch},
};
use sqlx::PgPool;
use uuid::Uuid;

struct Fixture {
    pool: PgPool,
    user_id: Uuid,
}

impl Fixture {
    async fn create(pool: PgPool) -> Self {
        let user_id = Uuid::now_v7();
        sqlx::query(
            "INSERT INTO meridian.users (id, email, handle, display_name, password_hash, role, is_active, is_admin) \
             VALUES ($1, $2, $3, 'Webhook Probe', '', 'reporter', true, true)",
        )
        .bind(user_id)
        .bind(format!("wh-{user_id}@probe.invalid"))
        .bind(format!("wh-{user_id}"))
        .execute(&pool)
        .await
        .expect("probe user inserted");
        Self { pool, user_id }
    }

    fn service(&self) -> NewsroomService {
        NewsroomService::new(self.pool.clone())
    }

    fn actor(&self) -> Actor {
        Actor { id: self.user_id, role: Role::Reporter, is_admin: true }
    }

    async fn tear_down(self) {
        let p = &self.pool;
        // Deliveries cascade from endpoints; events are actor-scoped.
        let _ = sqlx::query(
            "DELETE FROM meridian.webhook_endpoints WHERE created_by_id = $1",
        )
        .bind(self.user_id)
        .execute(p)
        .await;
        let _ = sqlx::query("DELETE FROM meridian.webhook_events WHERE actor_id = $1")
            .bind(self.user_id)
            .execute(p)
            .await;
        let _ = sqlx::query("DELETE FROM meridian.users WHERE id = $1")
            .bind(self.user_id)
            .execute(p)
            .await;
    }
}

/// Direct connection: the service prepares statements, which the PgBouncer
/// transaction pool does not support.
async fn pool() -> PgPool {
    let url = std::env::var("DATABASE_DIRECT_URL")
        .or_else(|_| std::env::var("DATABASE_URL"))
        .expect("DATABASE_DIRECT_URL for the live webhook-controls test");
    PgPool::connect(&url).await.expect("database reachable")
}

fn base_input(url: &str) -> EndpointInput {
    EndpointInput {
        url: url.to_owned(),
        description: "controls probe".to_owned(),
        event_types: vec!["article.*".to_owned()],
        desk_filter: None,
        desk_id: None,
        headers: None,
    }
}

#[tokio::test]
#[ignore = "requires the shared PostgreSQL stack"]
async fn create_persists_custom_headers_and_secret_is_returned_once() {
    let fx = Fixture::create(pool().await).await;
    let svc = fx.service();
    let actor = fx.actor();

    let mut input = base_input("https://example.invalid/hooks/a");
    input.headers = Some(serde_json::json!({ "X-Tenant": "meridian", "Authorization": "Bearer t" }));
    let created = svc
        .create_webhook_endpoint(&actor, &input)
        .await
        .expect("endpoint created");

    assert!(created.secret.starts_with("whsec_"), "secret is returned once on create");
    assert_eq!(
        created.endpoint.headers,
        serde_json::json!({ "X-Tenant": "meridian", "Authorization": "Bearer t" }),
        "custom headers persist",
    );

    // Listing never leaks the secret back out.
    let listed = svc.list_webhook_endpoints(&actor).await.expect("list");
    let row = listed.iter().find(|e| e.id == created.endpoint.id).expect("endpoint listed");
    let json = serde_json::to_value(row).expect("serialize");
    assert!(json.get("secret").is_none(), "secret is never serialized");

    fx.tear_down().await;
}

#[tokio::test]
#[ignore = "requires the shared PostgreSQL stack"]
async fn rotate_secret_replaces_the_signing_key() {
    let fx = Fixture::create(pool().await).await;
    let svc = fx.service();
    let actor = fx.actor();

    let created = svc
        .create_webhook_endpoint(&actor, &base_input("https://example.invalid/hooks/b"))
        .await
        .expect("endpoint created");
    let first = created.secret;

    let rotated = svc
        .rotate_webhook_secret(&actor, created.endpoint.id)
        .await
        .expect("rotated");

    assert!(rotated.starts_with("whsec_"));
    assert_ne!(rotated, first, "rotation yields a different secret");

    fx.tear_down().await;
}

#[tokio::test]
#[ignore = "requires the shared PostgreSQL stack"]
async fn disable_then_enable_toggles_active_and_clears_disabled_at() {
    let fx = Fixture::create(pool().await).await;
    let svc = fx.service();
    let actor = fx.actor();

    let created = svc
        .create_webhook_endpoint(&actor, &base_input("https://example.invalid/hooks/c"))
        .await
        .expect("endpoint created");
    let id = created.endpoint.id;
    assert!(created.endpoint.active, "endpoints start active");

    let disabled = svc
        .update_webhook_endpoint(&actor, id, &EndpointPatch { active: Some(false), ..Default::default() })
        .await
        .expect("disabled");
    assert!(!disabled.active);

    // Stamp a disabled_at, as the auto-disable path would, and prove re-enable clears it.
    sqlx::query("UPDATE meridian.webhook_endpoints SET disabled_at = now() WHERE id = $1")
        .bind(id)
        .execute(&fx.pool)
        .await
        .expect("stamp disabled_at");

    let enabled = svc
        .update_webhook_endpoint(&actor, id, &EndpointPatch { active: Some(true), ..Default::default() })
        .await
        .expect("enabled");
    assert!(enabled.active);
    assert!(enabled.disabled_at.is_none(), "re-enabling clears the auto-disable stamp");

    fx.tear_down().await;
}

#[tokio::test]
#[ignore = "requires the shared PostgreSQL stack"]
async fn patch_updates_headers_and_events_but_rejects_reserved_headers() {
    let fx = Fixture::create(pool().await).await;
    let svc = fx.service();
    let actor = fx.actor();

    let created = svc
        .create_webhook_endpoint(&actor, &base_input("https://example.invalid/hooks/d"))
        .await
        .expect("endpoint created");
    let id = created.endpoint.id;

    let patched = svc
        .update_webhook_endpoint(
            &actor,
            id,
            &EndpointPatch {
                event_types: Some(vec!["*.published".to_owned()]),
                headers: Some(serde_json::json!({ "X-Env": "prod" })),
                ..Default::default()
            },
        )
        .await
        .expect("patched");
    assert_eq!(patched.event_types, vec!["*.published".to_owned()]);
    assert_eq!(patched.headers, serde_json::json!({ "X-Env": "prod" }));

    // A reserved transport/signing header cannot be overridden.
    let rejected = svc
        .update_webhook_endpoint(
            &actor,
            id,
            &EndpointPatch {
                headers: Some(serde_json::json!({ "webhook-signature": "forged" })),
                ..Default::default()
            },
        )
        .await;
    assert!(rejected.is_err(), "reserved header override is rejected");

    fx.tear_down().await;
}

#[tokio::test]
#[ignore = "requires the shared PostgreSQL stack"]
async fn test_ping_enqueues_a_single_delivery_for_the_endpoint() {
    let fx = Fixture::create(pool().await).await;
    let svc = fx.service();
    let actor = fx.actor();

    let created = svc
        .create_webhook_endpoint(&actor, &base_input("https://example.invalid/hooks/e"))
        .await
        .expect("endpoint created");
    let id = created.endpoint.id;

    svc.send_webhook_test(&actor, id).await.expect("ping enqueued");

    // Exactly one pending webhook.ping delivery targets this endpoint, pre-fanned-out
    // so the dispatcher's fan_out step never duplicates it.
    let deliveries: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM meridian.webhook_deliveries \
         WHERE endpoint_id = $1 AND event_type = 'webhook.ping' AND status = 'pending'",
    )
    .bind(id)
    .fetch_one(&fx.pool)
    .await
    .expect("count deliveries");
    assert_eq!(deliveries, 1, "one ping delivery is queued for this endpoint");

    let fanned_out: bool = sqlx::query_scalar(
        "SELECT fanned_out FROM meridian.webhook_events \
         WHERE actor_id = $1 AND event_type = 'webhook.ping'",
    )
    .bind(fx.user_id)
    .fetch_one(&fx.pool)
    .await
    .expect("ping event exists");
    assert!(fanned_out, "the ping event is pre-fanned-out");

    fx.tear_down().await;
}
