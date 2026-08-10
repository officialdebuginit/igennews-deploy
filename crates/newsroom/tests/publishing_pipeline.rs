//! Live-database coverage for the publishing pipeline: the release scheduler
//! (atomic and per-channel), the reschedule / cancel / channel-schedule actions,
//! and the filtered story search. These rules live in SQL and in the scheduler's
//! control flow, so they can only be proven against a real database.
//!
//! Every row is created under one throwaway admin user and one throwaway desk, and
//! torn down by that scope in a guarded teardown — the suite seeds nothing global
//! and leaves no residue.

use meridian_newsroom::{
    Actor, NewsroomError, NewsroomService, Role,
    media::ReleaseDraft,
    search::SearchFilters,
};
use sqlx::PgPool;
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

/// The rows a test created, all tied to `user_id`/`desk_id` for a scoped teardown.
struct Fixture {
    pool: PgPool,
    user_id: Uuid,
    desk_id: Uuid,
    story_id: Uuid,
    version_id: Uuid,
}

impl Fixture {
    async fn create(pool: PgPool) -> Self {
        let user_id = Uuid::now_v7();
        let desk_id = Uuid::now_v7();
        let story_id = Uuid::now_v7();
        let version_id = Uuid::now_v7();

        // An admin actor so authz is not the thing under test here.
        sqlx::query(
            "INSERT INTO meridian.users (id, email, handle, display_name, password_hash, role, is_active, is_admin) \
             VALUES ($1, $2, $3, 'Pipeline Probe', '', 'reporter', true, true)",
        )
        .bind(user_id)
        .bind(format!("pipeline-{user_id}@probe.invalid"))
        .bind(format!("pipeline-{user_id}"))
        .execute(&pool)
        .await
        .expect("probe user inserted");

        sqlx::query("INSERT INTO meridian.desks (id, name, slug) VALUES ($1, $2, $3)")
            .bind(desk_id)
            .bind(format!("Probe Desk {desk_id}"))
            .bind(format!("probe-desk-{desk_id}"))
            .execute(&pool)
            .await
            .expect("probe desk inserted");

        // A fast-path story: the fast path waives the approval and asset blockers,
        // so with real content (a non-empty body) the readiness gate passes and
        // publishing is driven only by the clock — which is what these tests exercise.
        // (An empty body is NOT waived by the fast path; that is a real gate blocker.)
        sqlx::query(
            "INSERT INTO meridian.stories \
               (id, slug, author_id, title, dek, category, body, workflow_state, desk_id, fast_path, publication_state) \
             VALUES ($1, $2, $3, 'Probe headline', 'Probe dek', 'Technology', \
               '[{\"type\": \"paragraph\", \"text\": \"Probe body.\"}]'::jsonb, 'ready', $4, true, 'not_live')",
        )
        .bind(story_id)
        .bind(format!("probe-story-{story_id}"))
        .bind(user_id)
        .bind(desk_id)
        .execute(&pool)
        .await
        .expect("probe story inserted");

        sqlx::query(
            "INSERT INTO meridian.story_versions (id, story_id, number, title, dek, category, created_by_id) \
             VALUES ($1, $2, 1, 'Probe headline', 'Probe dek', 'Technology', $3)",
        )
        .bind(version_id)
        .bind(story_id)
        .bind(user_id)
        .execute(&pool)
        .await
        .expect("probe version inserted");

        Self { pool, user_id, desk_id, story_id, version_id }
    }

    fn actor(&self) -> Actor {
        Actor { id: self.user_id, role: Role::Reporter, is_admin: true }
    }

    /// Inserts a scheduled release under this fixture's story and returns its id.
    async fn insert_release(
        &self,
        channels: serde_json::Value,
        scheduled_at: OffsetDateTime,
        channel_schedule: serde_json::Value,
    ) -> Uuid {
        let id = Uuid::now_v7();
        sqlx::query(
            "INSERT INTO meridian.releases \
               (id, story_id, version_id, release_type, status, channels, scheduled_at, channel_schedule, created_by_id) \
             VALUES ($1, $2, $3, 'publish', 'scheduled', $4, $5, $6, $7)",
        )
        .bind(id)
        .bind(self.story_id)
        .bind(self.version_id)
        .bind(&channels)
        .bind(scheduled_at)
        .bind(&channel_schedule)
        .bind(self.user_id)
        .execute(&self.pool)
        .await
        .expect("probe release inserted");
        id
    }

    /// Inserts a release with full control over the fields the reader/expiry gates
    /// key off: status, embargo, expiry and published_at.
    #[allow(clippy::too_many_arguments)]
    async fn insert_release_detailed(
        &self,
        channels: serde_json::Value,
        status: &str,
        scheduled_at: Option<OffsetDateTime>,
        embargo_until: Option<OffsetDateTime>,
        expires_at: Option<OffsetDateTime>,
        published_at: Option<OffsetDateTime>,
    ) -> Uuid {
        let id = Uuid::now_v7();
        sqlx::query(
            "INSERT INTO meridian.releases \
               (id, story_id, version_id, release_type, status, channels, channel_schedule, \
                scheduled_at, embargo_until, expires_at, published_at, created_by_id) \
             VALUES ($1, $2, $3, 'publish', $4, $5, '{}'::jsonb, $6, $7, $8, $9, $10)",
        )
        .bind(id)
        .bind(self.story_id)
        .bind(self.version_id)
        .bind(status)
        .bind(&channels)
        .bind(scheduled_at)
        .bind(embargo_until)
        .bind(expires_at)
        .bind(published_at)
        .bind(self.user_id)
        .execute(&self.pool)
        .await
        .expect("detailed release inserted");
        id
    }

    /// The fixture story's public slug.
    async fn slug(&self) -> String {
        sqlx::query_scalar("SELECT slug FROM meridian.stories WHERE id = $1")
            .bind(self.story_id)
            .fetch_one(&self.pool)
            .await
            .expect("story slug")
    }

    async fn set_publication_state(&self, state: &str) {
        sqlx::query("UPDATE meridian.stories SET publication_state = $2 WHERE id = $1")
            .bind(self.story_id)
            .bind(state)
            .execute(&self.pool)
            .await
            .expect("set publication state");
    }

    /// Inserts an extra story (for search) and returns its id.
    async fn insert_story(&self, title: &str, category: &str, state: &str) -> Uuid {
        let id = Uuid::now_v7();
        sqlx::query(
            "INSERT INTO meridian.stories \
               (id, slug, author_id, title, dek, category, body, workflow_state, desk_id, publication_state) \
             VALUES ($1, $2, $3, $4, 'A probe dek', $5, '[]'::jsonb, $6, $7, 'not_live')",
        )
        .bind(id)
        .bind(format!("probe-{id}"))
        .bind(self.user_id)
        .bind(title)
        .bind(category)
        .bind(state)
        .bind(self.desk_id)
        .execute(&self.pool)
        .await
        .expect("probe search story inserted");
        id
    }

    /// A non-fast-path story with an empty body (so the readiness gate blocks it),
    /// its version, and a past-due scheduled release. Returns the release id.
    async fn insert_blocked_release(&self) -> Uuid {
        let story = Uuid::now_v7();
        let version = Uuid::now_v7();
        sqlx::query(
            "INSERT INTO meridian.stories \
               (id, slug, author_id, title, dek, category, body, workflow_state, desk_id, publication_state) \
             VALUES ($1, $2, $3, 'Blocked headline', 'Blocked dek', 'Technology', '[]'::jsonb, 'ready', $4, 'not_live')",
        )
        .bind(story)
        .bind(format!("blocked-{story}"))
        .bind(self.user_id)
        .bind(self.desk_id)
        .execute(&self.pool)
        .await
        .expect("blocked story inserted");
        sqlx::query(
            "INSERT INTO meridian.story_versions (id, story_id, number, title, dek, category, created_by_id) \
             VALUES ($1, $2, 1, 'Blocked headline', 'Blocked dek', 'Technology', $3)",
        )
        .bind(version)
        .bind(story)
        .bind(self.user_id)
        .execute(&self.pool)
        .await
        .expect("blocked version inserted");
        let id = Uuid::now_v7();
        let past = OffsetDateTime::now_utc() - Duration::hours(1);
        sqlx::query(
            "INSERT INTO meridian.releases \
               (id, story_id, version_id, release_type, status, channels, scheduled_at, created_by_id) \
             VALUES ($1, $2, $3, 'publish', 'scheduled', '[\"probe_a\"]'::jsonb, $4, $5)",
        )
        .bind(id)
        .bind(story)
        .bind(version)
        .bind(past)
        .bind(self.user_id)
        .execute(&self.pool)
        .await
        .expect("blocked release inserted");
        id
    }

    async fn attempt_count(&self, release_id: Uuid) -> i64 {
        sqlx::query_scalar("SELECT count(*) FROM meridian.release_attempts WHERE release_id = $1")
            .bind(release_id)
            .fetch_one(&self.pool)
            .await
            .expect("attempt count")
    }

    async fn insert_coverage(&self, title: &str) -> Uuid {
        let id = Uuid::now_v7();
        sqlx::query("INSERT INTO meridian.coverage_events (id, title, owner_id) VALUES ($1, $2, $3)")
            .bind(id)
            .bind(title)
            .bind(self.user_id)
            .execute(&self.pool)
            .await
            .expect("coverage event inserted");
        id
    }

    async fn insert_correction(&self, release_id: Uuid, description: &str) -> Uuid {
        let id = Uuid::now_v7();
        sqlx::query(
            "INSERT INTO meridian.corrections \
               (id, story_id, release_id, reported_by_id, classification, description) \
             VALUES ($1, $2, $3, $4, 'correction', $5)",
        )
        .bind(id)
        .bind(self.story_id)
        .bind(release_id)
        .bind(self.user_id)
        .bind(description)
        .execute(&self.pool)
        .await
        .expect("correction inserted");
        id
    }

    async fn status_of(&self, release_id: Uuid) -> String {
        sqlx::query_scalar("SELECT status FROM meridian.releases WHERE id = $1")
            .bind(release_id)
            .fetch_one(&self.pool)
            .await
            .expect("release status")
    }

    /// The channel names in a release's receipts.
    async fn delivered(&self, release_id: Uuid) -> Vec<String> {
        let receipts: serde_json::Value =
            sqlx::query_scalar("SELECT receipts FROM meridian.releases WHERE id = $1")
                .bind(release_id)
                .fetch_one(&self.pool)
                .await
                .expect("release receipts");
        let mut names = Vec::new();
        if let Some(array) = receipts.as_array() {
            for receipt in array {
                if let Some(channel) = receipt.get("channel").and_then(|c| c.as_str()) {
                    names.push(channel.to_owned());
                }
            }
        }
        names
    }

    async fn tear_down(self) {
        let p = &self.pool;
        let u = self.user_id;
        let _ = sqlx::query("DELETE FROM meridian.release_attempts WHERE release_id IN (SELECT id FROM meridian.releases WHERE created_by_id = $1)").bind(u).execute(p).await;
        let _ = sqlx::query("DELETE FROM meridian.corrections WHERE reported_by_id = $1").bind(u).execute(p).await;
        let _ = sqlx::query("DELETE FROM meridian.releases WHERE created_by_id = $1").bind(u).execute(p).await;
        let _ = sqlx::query("DELETE FROM meridian.story_versions WHERE created_by_id = $1").bind(u).execute(p).await;
        let _ = sqlx::query("DELETE FROM meridian.coverage_events WHERE owner_id = $1").bind(u).execute(p).await;
        let _ = sqlx::query("DELETE FROM meridian.stories WHERE author_id = $1").bind(u).execute(p).await;
        let _ = sqlx::query("DELETE FROM meridian.audit_events WHERE actor_id = $1").bind(u).execute(p).await;
        let _ = sqlx::query("DELETE FROM meridian.desks WHERE id = $1").bind(self.desk_id).execute(p).await;
        let _ = sqlx::query("DELETE FROM meridian.users WHERE id = $1").bind(u).execute(p).await;
    }
}

/// Direct connection: these tests prepare statements, which the PgBouncer
/// transaction pool on `DATABASE_URL` does not support.
async fn pool() -> PgPool {
    let url = std::env::var("DATABASE_DIRECT_URL")
        .or_else(|_| std::env::var("DATABASE_URL"))
        .expect("DATABASE_DIRECT_URL for the live publishing test");
    PgPool::connect(&url).await.expect("database reachable")
}

/// The common case: an empty `channel_schedule` publishes every channel at once
/// the moment the base slot passes.
#[tokio::test]
#[ignore = "requires the shared PostgreSQL stack"]
async fn atomic_publish_sends_all_channels_at_once() {
    let fx = Fixture::create(pool().await).await;
    let service = NewsroomService::new(fx.pool.clone());
    let past = OffsetDateTime::now_utc() - Duration::hours(1);
    let release = fx
        .insert_release(serde_json::json!(["probe_a", "probe_b"]), past, serde_json::json!({}))
        .await;

    service.process_due_releases(&fx.actor(), "worker").await.expect("sweep runs");

    let status = fx.status_of(release).await;
    let mut delivered = fx.delivered(release).await;
    delivered.sort();
    fx.tear_down().await;

    assert_eq!(status, "published", "an empty schedule publishes atomically");
    assert_eq!(delivered, vec!["probe_a", "probe_b"], "both channels went out at once");
}

/// A per-channel schedule sends each channel at its own time: the early channel
/// goes out while the release stays scheduled, and only once the late channel's
/// time passes does the release become published.
#[tokio::test]
#[ignore = "requires the shared PostgreSQL stack"]
async fn per_channel_schedule_staggers_publish() {
    let fx = Fixture::create(pool().await).await;
    let service = NewsroomService::new(fx.pool.clone());
    let now = OffsetDateTime::now_utc();
    // Base slot in the future so only the per-channel times drive the sweep.
    let base = now + Duration::days(3);
    let early = (now - Duration::minutes(5)).format(&time::format_description::well_known::Rfc3339).unwrap();
    let late = (now + Duration::days(2)).format(&time::format_description::well_known::Rfc3339).unwrap();
    let release = fx
        .insert_release(
            serde_json::json!(["probe_a", "probe_b"]),
            base,
            serde_json::json!({ "probe_a": early, "probe_b": late }),
        )
        .await;

    // First sweep: only probe_a is due.
    service.process_due_releases(&fx.actor(), "worker").await.expect("first sweep");
    let after_first = fx.status_of(release).await;
    let delivered_first = fx.delivered(release).await;

    // Bring probe_b's time into the past, then sweep again.
    sqlx::query("UPDATE meridian.releases SET channel_schedule = jsonb_set(channel_schedule, '{probe_b}', to_jsonb($2::text)) WHERE id = $1")
        .bind(release)
        .bind((now - Duration::minutes(1)).format(&time::format_description::well_known::Rfc3339).unwrap())
        .execute(&fx.pool)
        .await
        .expect("advance probe_b time");
    service.process_due_releases(&fx.actor(), "worker").await.expect("second sweep");
    let after_second = fx.status_of(release).await;
    let mut delivered_second = fx.delivered(release).await;
    delivered_second.sort();

    fx.tear_down().await;

    assert_eq!(after_first, "scheduled", "release stays scheduled until every channel is out");
    assert_eq!(delivered_first, vec!["probe_a"], "only the early channel went out first");
    assert_eq!(after_second, "published", "the release publishes once the late channel is out");
    assert_eq!(delivered_second, vec!["probe_a", "probe_b"], "both channels are recorded, no duplicates");
}

/// Embargo (finding E): a release may be published — and syndicated to partners —
/// while still not showable to readers. The reader-facing queries must hide the
/// story until its current release's embargo lifts, then serve it.
#[tokio::test]
#[ignore = "requires the shared PostgreSQL stack"]
async fn embargo_hides_the_article_from_readers_until_it_lifts() {
    let fx = Fixture::create(pool().await).await;
    let service = NewsroomService::new(fx.pool.clone());
    let now = OffsetDateTime::now_utc();
    // Past slot so the sweep publishes it now; embargoed from readers for an hour.
    let release = fx
        .insert_release_detailed(
            serde_json::json!(["probe_a"]),
            "scheduled",
            Some(now - Duration::hours(1)),
            Some(now + Duration::hours(1)),
            None,
            None,
        )
        .await;
    service.process_due_releases(&fx.actor(), "worker").await.expect("sweep publishes");
    let slug = fx.slug().await;

    let hidden = service.public_article(&slug).await.expect("reader query");
    let listed = service.published_summaries(50, 0, None, None).await.expect("summaries query");
    let listed_before = listed.iter().any(|s| s.slug == slug);

    // Lift the embargo into the past; the story becomes visible.
    sqlx::query("UPDATE meridian.releases SET embargo_until = $2 WHERE id = $1")
        .bind(release)
        .bind(now - Duration::minutes(1))
        .execute(&fx.pool)
        .await
        .expect("lift embargo");
    let shown = service.public_article(&slug).await.expect("reader query");
    fx.tear_down().await;

    assert!(hidden.is_none(), "an embargoed story is not served to the public reader");
    assert!(!listed_before, "an embargoed story is absent from the public list");
    assert!(shown.is_some(), "once the embargo lifts the story is served");
}

/// Expiry (finding G): when an older release's expiry passes but a newer release
/// still holds the same story published (e.g. a correction re-release), the story
/// must stay live — only the older release is taken down.
#[tokio::test]
#[ignore = "requires the shared PostgreSQL stack"]
async fn expiry_keeps_the_story_live_when_a_newer_release_still_holds_it() {
    let fx = Fixture::create(pool().await).await;
    let service = NewsroomService::new(fx.pool.clone());
    let now = OffsetDateTime::now_utc();
    // R1: published earlier, expiry already passed (due for takedown).
    let r1 = fx
        .insert_release_detailed(
            serde_json::json!(["probe_a"]),
            "published",
            None,
            None,
            Some(now - Duration::minutes(1)),
            Some(now - Duration::hours(2)),
        )
        .await;
    // R2: a newer, still-live publication of the same story, no expiry.
    fx.insert_release_detailed(
        serde_json::json!(["probe_a"]),
        "published",
        None,
        None,
        None,
        Some(now - Duration::minutes(30)),
    )
    .await;
    fx.set_publication_state("published").await;

    service.process_due_releases(&fx.actor(), "worker").await.expect("sweep runs expiry");

    let r1_status = fx.status_of(r1).await;
    let story_state: String = sqlx::query_scalar("SELECT publication_state FROM meridian.stories WHERE id = $1")
        .bind(fx.story_id)
        .fetch_one(&fx.pool)
        .await
        .expect("story state");
    fx.tear_down().await;

    assert_eq!(r1_status, "expired", "the older release with a passed expiry is taken down");
    assert_eq!(story_state, "published", "the story stays live because a newer release still holds it");
}

/// Fast-path re-authorisation (finding B): a persisted `fast_path` flag alone must
/// not disarm the gate at release. `create_release` re-requires a recorded reason
/// (the capability is re-checked too, but the fixture actor is admin).
#[tokio::test]
#[ignore = "requires the shared PostgreSQL stack"]
async fn fast_path_release_requires_a_recorded_reason() {
    let fx = Fixture::create(pool().await).await;
    let service = NewsroomService::new(fx.pool.clone());
    let draft = ReleaseDraft {
        version_id: fx.version_id,
        channels: serde_json::json!(["probe_a"]),
        scheduled_at: Some(OffsetDateTime::now_utc() + Duration::days(1)),
        embargo_until: None,
        expires_at: None,
    };

    // The fixture story is fast_path=true but carries no fast_path_reason.
    let refused = service.create_release(&fx.actor(), fx.story_id, &draft).await;
    let refused_ok = matches!(&refused, Err(NewsroomError::Conflict(m)) if m.contains("reason"));

    // Record a reason; now the release is allowed.
    sqlx::query("UPDATE meridian.stories SET fast_path_reason = 'deadline' WHERE id = $1")
        .bind(fx.story_id)
        .execute(&fx.pool)
        .await
        .expect("set fast-path reason");
    let allowed = service.create_release(&fx.actor(), fx.story_id, &draft).await;
    let allowed_ok = allowed.is_ok();
    fx.tear_down().await;

    assert!(refused_ok, "a fast-path release with no recorded reason is refused, got {refused:?}");
    assert!(allowed_ok, "with a recorded reason the fast-path release is created: {allowed:?}");
}

/// Reschedule guard (minor finding): moving a scheduled release to a slot at or
/// after its own expiry is unexpressible — it would publish then be taken down in
/// the same sweep. A slot before the expiry is fine.
#[tokio::test]
#[ignore = "requires the shared PostgreSQL stack"]
async fn reschedule_rejects_a_slot_at_or_after_expiry() {
    let fx = Fixture::create(pool().await).await;
    let service = NewsroomService::new(fx.pool.clone());
    let now = OffsetDateTime::now_utc();
    let release = fx
        .insert_release_detailed(
            serde_json::json!(["probe_a"]),
            "scheduled",
            Some(now + Duration::hours(1)),
            None,
            Some(now + Duration::days(1)),
            None,
        )
        .await;

    let bad = service.reschedule_release(&fx.actor(), release, now + Duration::days(2)).await;
    let bad_refused = matches!(&bad, Err(NewsroomError::Unprocessable(m)) if m.contains("expiry"));

    let good = service.reschedule_release(&fx.actor(), release, now + Duration::hours(6)).await;
    let good_ok = good.is_ok();
    fx.tear_down().await;

    assert!(bad_refused, "a reschedule at/after the release's expiry is refused, got {bad:?}");
    assert!(good_ok, "a reschedule before the expiry is accepted: {good:?}");
}

/// Reschedule moves a scheduled release; cancel takes it out of the queue; and
/// neither action is allowed once the release is no longer scheduled.
#[tokio::test]
#[ignore = "requires the shared PostgreSQL stack"]
async fn reschedule_and_cancel_guard_state() {
    let fx = Fixture::create(pool().await).await;
    let service = NewsroomService::new(fx.pool.clone());
    let base = OffsetDateTime::now_utc() + Duration::days(1);
    let release = fx.insert_release(serde_json::json!(["probe_a"]), base, serde_json::json!({})).await;

    let new_time = OffsetDateTime::now_utc() + Duration::days(5);
    let moved = service.reschedule_release(&fx.actor(), release, new_time).await.expect("reschedule");
    let cancelled = service.cancel_release(&fx.actor(), release).await.expect("cancel");
    let after_cancel = service.reschedule_release(&fx.actor(), release, new_time).await;

    fx.tear_down().await;

    assert_eq!(moved.scheduled_at, Some(new_time), "reschedule set the new slot");
    assert_eq!(cancelled.status, "cancelled", "cancel took it out of the queue");
    assert!(after_cancel.is_err(), "a cancelled release cannot be rescheduled");
}

/// Setting a per-channel schedule keeps only known channels with valid times, and
/// rejects a non-RFC-3339 instant.
#[tokio::test]
#[ignore = "requires the shared PostgreSQL stack"]
async fn set_channel_schedule_cleans_and_validates() {
    let fx = Fixture::create(pool().await).await;
    let service = NewsroomService::new(fx.pool.clone());
    let base = OffsetDateTime::now_utc() + Duration::days(1);
    let release = fx.insert_release(serde_json::json!(["probe_a", "probe_b"]), base, serde_json::json!({})).await;

    // probe_a valid, probe_b empty (drop), unknown channel (drop).
    let cleaned = service
        .set_channel_schedule(
            &fx.actor(),
            release,
            &serde_json::json!({
                "probe_a": "2026-08-05T09:00:00Z",
                "probe_b": "",
                "not_a_channel": "2026-08-05T10:00:00Z"
            }),
        )
        .await
        .expect("valid schedule accepted");

    // A malformed time is rejected.
    let bad = service
        .set_channel_schedule(&fx.actor(), release, &serde_json::json!({ "probe_a": "yesterday" }))
        .await;

    let stored = cleaned.channel_schedule;
    fx.tear_down().await;

    assert_eq!(
        stored,
        serde_json::json!({ "probe_a": "2026-08-05T09:00:00Z" }),
        "only known channels with valid times are kept"
    );
    assert!(bad.is_err(), "a non-RFC-3339 time is rejected");
}

/// Filtered search narrows server-side by category and reports query-aware facet
/// counts over the matching set.
#[tokio::test]
#[ignore = "requires the shared PostgreSQL stack"]
async fn filtered_search_narrows_and_facets() {
    let fx = Fixture::create(pool().await).await;
    let service = NewsroomService::new(fx.pool.clone());
    // A distinctive term so the corpus outside this fixture cannot interfere.
    let term = format!("zzqprobe{}", &fx.user_id.simple().to_string()[..8]);
    fx.insert_story(&format!("{term} alpha"), "Technology", "ready").await;
    fx.insert_story(&format!("{term} beta"), "Technology", "drafting").await;
    fx.insert_story(&format!("{term} gamma"), "Business", "ready").await;

    let all = service
        .search_stories_filtered(&fx.actor(), &term, &SearchFilters { limit: 50, ..Default::default() })
        .await
        .expect("unfiltered search");

    let tech_only = service
        .search_stories_filtered(
            &fx.actor(),
            &term,
            &SearchFilters { category: Some("Technology".to_owned()), limit: 50, ..Default::default() },
        )
        .await
        .expect("category-filtered search");

    fx.tear_down().await;

    assert_eq!(all.total, 3, "all three probe stories match the term");
    assert_eq!(*all.facets.category.get("Technology").unwrap_or(&0), 2, "two Technology stories");
    assert_eq!(*all.facets.category.get("Business").unwrap_or(&0), 1, "one Business story");
    assert_eq!(tech_only.total, 2, "the category filter narrows server-side");
    assert!(
        tech_only.hits.iter().all(|h| h.category == "Technology"),
        "every filtered hit is in the chosen category"
    );
}

/// A release that fails the readiness gate is left in the queue, and a repeated
/// sweep does not pile up identical gate-failure rows (the dedup guard).
#[tokio::test]
#[ignore = "requires the shared PostgreSQL stack"]
async fn gate_blocked_release_stays_scheduled_and_dedups_failures() {
    let fx = Fixture::create(pool().await).await;
    let service = NewsroomService::new(fx.pool.clone());
    let release = fx.insert_blocked_release().await;

    service.process_due_releases(&fx.actor(), "worker").await.expect("first sweep");
    let after_first = fx.status_of(release).await;
    let attempts_first = fx.attempt_count(release).await;

    service.process_due_releases(&fx.actor(), "worker").await.expect("second sweep");
    let attempts_second = fx.attempt_count(release).await;

    fx.tear_down().await;

    assert_eq!(after_first, "scheduled", "a gate-blocked release is left in the queue");
    assert_eq!(attempts_first, 1, "the gate failure is recorded once");
    assert_eq!(attempts_second, 1, "an unchanged gate failure is not recorded again");
}

/// The corpus search reaches coverage events and corrections, tagging each hit
/// with its kind.
#[tokio::test]
#[ignore = "requires the shared PostgreSQL stack"]
async fn corpus_search_finds_coverage_and_corrections() {
    let fx = Fixture::create(pool().await).await;
    let service = NewsroomService::new(fx.pool.clone());
    let term = format!("zzcorpus{}", &fx.user_id.simple().to_string()[..8]);

    fx.insert_coverage(&format!("{term} briefing")).await;
    let base = OffsetDateTime::now_utc() + Duration::days(1);
    let release = fx.insert_release(serde_json::json!(["probe_a"]), base, serde_json::json!({})).await;
    fx.insert_correction(release, &format!("{term} misstated figure")).await;

    let (coverage, cov_total) = service
        .search_corpus(&fx.actor(), "coverage", &term, None, None, None, 25, 0)
        .await
        .expect("coverage search");
    let (corrections, cor_total) = service
        .search_corpus(&fx.actor(), "corrections", &term, None, None, None, 25, 0)
        .await
        .expect("corrections search");

    let cov_kind = coverage.first().map(|h| h.kind.clone());
    let cor_kind = corrections.first().map(|h| h.kind.clone());
    fx.tear_down().await;

    assert_eq!(cov_total, 1, "the coverage event is found by its title");
    assert_eq!(cov_kind.as_deref(), Some("coverage"), "coverage hits are tagged coverage");
    assert_eq!(cor_total, 1, "the correction is found by its description");
    assert_eq!(cor_kind.as_deref(), Some("correction"), "correction hits are tagged correction");
}
