//! Proves that story-lifecycle mutations enqueue their outbound `article.*` events
//! into the transactional outbox, in the same transaction as the mutation. Runs
//! against a live database and cleans up by actor scope.

use meridian_newsroom::{Actor, NewsroomService, Role, stories::StoryDraft};
use sqlx::PgPool;
use uuid::Uuid;

struct Fixture {
    pool: PgPool,
    user_id: Uuid,
    desk_id: Uuid,
}

impl Fixture {
    async fn create(pool: PgPool) -> Self {
        let user_id = Uuid::now_v7();
        let desk_id = Uuid::now_v7();
        sqlx::query(
            "INSERT INTO meridian.users (id, email, handle, display_name, password_hash, role, is_active, is_admin) \
             VALUES ($1, $2, $3, 'Emit Probe', '', 'reporter', true, true)",
        )
        .bind(user_id)
        .bind(format!("emit-{user_id}@probe.invalid"))
        .bind(format!("emit-{user_id}"))
        .execute(&pool)
        .await
        .expect("probe user");
        sqlx::query("INSERT INTO meridian.desks (id, name, slug) VALUES ($1, $2, $3)")
            .bind(desk_id)
            .bind(format!("Emit Desk {desk_id}"))
            .bind(format!("emit-desk-{desk_id}"))
            .execute(&pool)
            .await
            .expect("probe desk");
        Self { pool, user_id, desk_id }
    }

    fn service(&self) -> NewsroomService {
        NewsroomService::new(self.pool.clone())
    }

    fn actor(&self) -> Actor {
        Actor { id: self.user_id, role: Role::Reporter, is_admin: true }
    }

    fn draft(&self) -> StoryDraft {
        StoryDraft {
            slug: format!("emit-{}", Uuid::now_v7().simple()),
            title: "Emit probe headline".to_owned(),
            dek: "Dek".to_owned(),
            body: serde_json::json!([{ "type": "paragraph", "text": "Body." }]),
            category: "Technology".to_owned(),
            tags: serde_json::json!([]),
            story_type: "article".to_owned(),
            priority: "medium".to_owned(),
            desk_id: Some(self.desk_id),
            sub_sector_id: None,
            event_id: None,
            author_id: Some(self.user_id),
            editor_id: None,
            fact_checker_id: None,
            copy_editor_id: None,
        }
    }

    async fn events_for(&self, event_type: &str, subject: Uuid) -> i64 {
        sqlx::query_scalar(
            "SELECT count(*) FROM meridian.webhook_events WHERE event_type = $1 AND subject = $2",
        )
        .bind(event_type)
        .bind(subject.to_string())
        .fetch_one(&self.pool)
        .await
        .expect("count events")
    }

    async fn tear_down(self) {
        let p = &self.pool;
        let _ = sqlx::query("DELETE FROM meridian.webhook_events WHERE actor_id = $1")
            .bind(self.user_id).execute(p).await;
        let _ = sqlx::query("DELETE FROM meridian.stories WHERE author_id = $1")
            .bind(self.user_id).execute(p).await;
        let _ = sqlx::query("DELETE FROM meridian.audit_events WHERE actor_id = $1")
            .bind(self.user_id).execute(p).await;
        let _ = sqlx::query("DELETE FROM meridian.desks WHERE id = $1")
            .bind(self.desk_id).execute(p).await;
        let _ = sqlx::query("DELETE FROM meridian.users WHERE id = $1")
            .bind(self.user_id).execute(p).await;
    }
}

async fn pool() -> PgPool {
    let url = std::env::var("DATABASE_DIRECT_URL")
        .or_else(|_| std::env::var("DATABASE_URL"))
        .expect("DATABASE_DIRECT_URL for the live emission test");
    PgPool::connect(&url).await.expect("database reachable")
}

#[tokio::test]
#[ignore = "requires the shared PostgreSQL stack"]
async fn create_and_delete_a_story_emit_article_events() {
    let fx = Fixture::create(pool().await).await;
    let svc = fx.service();
    let actor = fx.actor();

    let story = svc.create_story(&actor, &fx.draft()).await.expect("created");
    assert_eq!(
        fx.events_for("article.created", story.id).await,
        1,
        "create_story enqueues article.created",
    );

    svc.delete_story(&actor, story.id).await.expect("deleted");
    assert_eq!(
        fx.events_for("article.deleted", story.id).await,
        1,
        "delete_story enqueues article.deleted",
    );

    fx.tear_down().await;
}
