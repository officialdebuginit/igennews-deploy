//! The Phase-0 enforcement gate: a workspace membership, not a global role, is
//! what grants capability inside a sector.
//!
//! These run against the live database because the rule they prove lives in SQL
//! (`desk_memberships`) as much as in Rust. Every fixture row is created with a
//! unique id and removed in a guarded teardown, so the suite leaves no residue
//! and never depends on seeded data.

use meridian_newsroom::{Actor, NewsroomService, Role, authz, stories::StoryFilter};
use sqlx::PgPool;
use uuid::Uuid;

/// Rows this test created, torn down in reverse order regardless of outcome.
struct Fixture {
    pool: PgPool,
    user_id: Uuid,
    tech_desk: Uuid,
    politics_desk: Uuid,
}

impl Fixture {
    async fn create(pool: PgPool, workspace_role: &str) -> Self {
        let user_id = Uuid::now_v7();
        let tech_desk = Uuid::now_v7();
        let politics_desk = Uuid::now_v7();

        sqlx::query(
            "INSERT INTO meridian.users (id, email, handle, display_name, password_hash, role, is_active, is_admin) \
             VALUES ($1, $2, $3, 'Sector Gate Probe', '', 'reporter', true, false)",
        )
        .bind(user_id)
        .bind(format!("sector-gate-{user_id}@probe.invalid"))
        .bind(format!("sector-gate-{user_id}"))
        .execute(&pool)
        .await
        .expect("probe user inserted");

        for (id, name) in [(tech_desk, "tech"), (politics_desk, "politics")] {
            sqlx::query("INSERT INTO meridian.desks (id, name, slug) VALUES ($1, $2, $3)")
                .bind(id)
                .bind(format!("Probe {name}"))
                .bind(format!("probe-{name}-{id}"))
                .execute(&pool)
                .await
                .expect("probe desk inserted");
        }

        // The whole point: a membership in Tech only.
        sqlx::query(
            "INSERT INTO meridian.desk_memberships (desk_id, user_id, role) VALUES ($1, $2, $3)",
        )
        .bind(tech_desk)
        .bind(user_id)
        .bind(workspace_role)
        .execute(&pool)
        .await
        .expect("probe membership inserted");

        Self { pool, user_id, tech_desk, politics_desk }
    }

    fn actor(&self, role: Role) -> Actor {
        Actor { id: self.user_id, role, is_admin: false }
    }

    async fn tear_down(self) {
        let _ = sqlx::query("DELETE FROM meridian.desk_memberships WHERE user_id = $1")
            .bind(self.user_id)
            .execute(&self.pool)
            .await;
        let _ = sqlx::query("DELETE FROM meridian.desks WHERE id = ANY($1)")
            .bind(vec![self.tech_desk, self.politics_desk])
            .execute(&self.pool)
            .await;
        let _ = sqlx::query("DELETE FROM meridian.users WHERE id = $1")
            .bind(self.user_id)
            .execute(&self.pool)
            .await;
    }
}

/// Uses the direct connection: these tests prepare statements, which the
/// `PgBouncer` transaction pool on `DATABASE_URL` does not support.
async fn pool() -> PgPool {
    let url = std::env::var("DATABASE_DIRECT_URL")
        .or_else(|_| std::env::var("DATABASE_URL"))
        .expect("DATABASE_DIRECT_URL for the live authz test");
    PgPool::connect(&url).await.expect("database reachable")
}

/// The gate. Before Phase 0 this could not be expressed: `stories.edit` resolved
/// off the org-level role alone, so a reporter could edit in any sector.
#[tokio::test]
#[ignore = "requires the shared PostgreSQL stack"]
async fn membership_bounds_capability_to_its_own_sector() {
    let fixture = Fixture::create(pool().await, "reporter").await;
    let actor = fixture.actor(Role::Reporter);

    let in_tech = authz::resolve(&fixture.pool, &actor, "stories.edit", Some(fixture.tech_desk))
        .await
        .expect("resolves in tech");
    let in_politics =
        authz::resolve(&fixture.pool, &actor, "stories.edit", Some(fixture.politics_desk))
            .await
            .expect("resolves in politics");

    let outcome = (in_tech.allowed, in_tech.source, in_politics.allowed, in_politics.source);
    fixture.tear_down().await;

    assert_eq!(
        outcome,
        (true, "workspace_membership", false, "no_membership"),
        "a Tech reporter may edit in Tech and nowhere else"
    );
}

/// A workspace role that does not carry the capability is denied even inside its
/// own sector — membership is necessary, not sufficient.
#[tokio::test]
#[ignore = "requires the shared PostgreSQL stack"]
async fn a_viewer_membership_does_not_grant_editing() {
    let fixture = Fixture::create(pool().await, "viewer").await;
    let actor = fixture.actor(Role::Reporter);

    let decision = authz::resolve(&fixture.pool, &actor, "stories.edit", Some(fixture.tech_desk))
        .await
        .expect("resolves");
    let outcome = (decision.allowed, decision.source);
    fixture.tear_down().await;

    assert_eq!(outcome, (false, "workspace_membership"));
}

/// The four org-wide roles overlay every sector, with or without a membership —
/// this is what keeps the editor-in-chief able to publish anywhere.
#[tokio::test]
#[ignore = "requires the shared PostgreSQL stack"]
async fn a_global_role_reaches_into_a_sector_it_holds_no_membership_in() {
    let fixture = Fixture::create(pool().await, "reporter").await;
    let actor = fixture.actor(Role::EditorInChief);

    let decision =
        authz::resolve(&fixture.pool, &actor, "reviews.approve", Some(fixture.politics_desk))
            .await
            .expect("resolves");
    let outcome = (decision.allowed, decision.source);
    fixture.tear_down().await;

    assert_eq!(outcome, (true, "role_default"));
}

/// Publish authority is per-sector. Before Phase 0 this was `is_publisher`, a
/// global role test: a section editor of any desk could publish every story in
/// the newsroom.
#[tokio::test]
#[ignore = "requires the shared PostgreSQL stack"]
async fn publish_authority_does_not_cross_sectors() {
    let fixture = Fixture::create(pool().await, "section_editor").await;
    let actor = fixture.actor(Role::SectionEditor);

    let own = authz::resolve(&fixture.pool, &actor, "releases.publish", Some(fixture.tech_desk))
        .await
        .expect("resolves in tech");
    let other =
        authz::resolve(&fixture.pool, &actor, "releases.publish", Some(fixture.politics_desk))
            .await
            .expect("resolves in politics");

    let outcome = (own.allowed, other.allowed, other.source);
    fixture.tear_down().await;

    assert_eq!(
        outcome,
        (true, false, "no_membership"),
        "a Tech section editor publishes in Tech, not in Politics"
    );
}

/// The workflow gate that gets a story to `ready` is likewise sector-bound.
#[tokio::test]
#[ignore = "requires the shared PostgreSQL stack"]
async fn advancing_the_workflow_is_bound_to_the_storys_sector() {
    let fixture = Fixture::create(pool().await, "section_editor").await;
    let actor = fixture.actor(Role::SectionEditor);

    let own = authz::resolve(&fixture.pool, &actor, "workflow.advance", Some(fixture.tech_desk))
        .await
        .expect("resolves in tech");
    let other =
        authz::resolve(&fixture.pool, &actor, "workflow.advance", Some(fixture.politics_desk))
            .await
            .expect("resolves in politics");

    let outcome = (own.allowed, other.allowed);
    fixture.tear_down().await;

    assert_eq!(outcome, (true, false));
}

/// A fact checker may decide claims in their sector — the legacy
/// `require_verifier` audience, preserved but now bounded.
#[tokio::test]
#[ignore = "requires the shared PostgreSQL stack"]
async fn the_verifier_audience_is_preserved_inside_the_sector() {
    let fixture = Fixture::create(pool().await, "fact_checker").await;
    let actor = fixture.actor(Role::FactChecker);

    let claims = authz::resolve(&fixture.pool, &actor, "claims.decide", Some(fixture.tech_desk))
        .await
        .expect("resolves");
    // …but a fact checker was never a publisher, in legacy or now.
    let publish =
        authz::resolve(&fixture.pool, &actor, "releases.publish", Some(fixture.tech_desk))
            .await
            .expect("resolves");

    let outcome = (claims.allowed, publish.allowed);
    fixture.tear_down().await;

    assert_eq!(outcome, (true, false));
}

/// The story list is filtered to the sectors the viewer belongs to. This is the
/// read half of the boundary: scoping writes without scoping reads would still
/// leak every desk's drafts into one list.
#[tokio::test]
#[ignore = "requires the shared PostgreSQL stack"]
async fn the_story_list_shows_only_sectors_the_viewer_belongs_to() {
    let fixture = Fixture::create(pool().await, "reporter").await;

    // One story in each desk, authored by somebody else entirely.
    let stranger = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO meridian.users (id, email, handle, display_name, password_hash, role, is_active, is_admin) \
         VALUES ($1, $2, $3, 'Stranger', '', 'reporter', true, false)",
    )
    .bind(stranger)
    .bind(format!("stranger-{stranger}@probe.invalid"))
    .bind(format!("stranger-{stranger}"))
    .execute(&fixture.pool)
    .await
    .expect("stranger inserted");

    let mut story_ids = Vec::new();
    for (desk, tag) in [(fixture.tech_desk, "tech"), (fixture.politics_desk, "politics")] {
        let id = Uuid::now_v7();
        sqlx::query(
            "INSERT INTO meridian.stories (id, slug, title, dek, body, category, tags, \
             story_type, workflow_state, publication_state, validity_state, priority, \
             desk_id, author_id) \
             VALUES ($1,$2,'Probe','', '[]'::jsonb, 'General', '[]'::jsonb, 'article', \
             'drafting','not_live','current','medium',$3,$4)",
        )
        .bind(id)
        .bind(format!("probe-{tag}-{id}"))
        .bind(desk)
        .bind(stranger)
        .execute(&fixture.pool)
        .await
        .expect("probe story inserted");
        story_ids.push(id);
    }

    let service = NewsroomService::new(fixture.pool.clone());
    let visible = service
        .list_stories(&fixture.actor(Role::Reporter), &StoryFilter::default())
        .await
        .expect("lists stories")
        .into_iter()
        .filter(|story| story_ids.contains(&story.id))
        .map(|story| story.desk_id)
        .collect::<Vec<_>>();

    let _ = sqlx::query("DELETE FROM meridian.stories WHERE id = ANY($1)")
        .bind(&story_ids)
        .execute(&fixture.pool)
        .await;
    let _ = sqlx::query("DELETE FROM meridian.users WHERE id = $1")
        .bind(stranger)
        .execute(&fixture.pool)
        .await;
    let tech = fixture.tech_desk;
    fixture.tear_down().await;

    assert_eq!(
        visible,
        vec![Some(tech)],
        "a Tech-only reporter sees the Tech story and not the Politics one"
    );
}

/// A *global* capability denied inside a sector must say so for the right reason.
/// The explanation drives the admin's effective-permission viewer, so attributing
/// `roles.manage` to "no membership in this sector" would be a right answer with a
/// wrong reason — and that viewer is the only tool for auditing a denial.
#[tokio::test]
#[ignore = "requires the shared PostgreSQL stack"]
async fn a_global_capability_is_not_denied_for_want_of_a_membership() {
    let fixture = Fixture::create(pool().await, "reporter").await;
    let actor = fixture.actor(Role::Reporter);

    // Asked inside a sector the reporter *does* belong to, and one they do not.
    let inside = authz::resolve(&fixture.pool, &actor, "roles.manage", Some(fixture.tech_desk))
        .await
        .expect("resolves in tech");
    let outside =
        authz::resolve(&fixture.pool, &actor, "roles.manage", Some(fixture.politics_desk))
            .await
            .expect("resolves in politics");
    // …and a sector capability, which *should* cite the membership.
    let sector_cap =
        authz::resolve(&fixture.pool, &actor, "stories.edit", Some(fixture.politics_desk))
            .await
            .expect("resolves");

    let outcome = (
        inside.allowed,
        inside.source,
        outside.allowed,
        outside.source,
        sector_cap.source,
    );
    fixture.tear_down().await;

    assert_eq!(
        outcome,
        (false, "default_deny", false, "default_deny", "no_membership"),
        "a global capability denies by role default; only a sector capability cites membership"
    );
}

/// An unscoped question keeps its org-level answer so list endpoints still work;
/// narrowing to visible sectors is the query's job, not the resolver's.
#[tokio::test]
#[ignore = "requires the shared PostgreSQL stack"]
async fn an_unscoped_check_still_answers_at_org_level() {
    let fixture = Fixture::create(pool().await, "reporter").await;
    let actor = fixture.actor(Role::Reporter);

    let decision = authz::resolve(&fixture.pool, &actor, "stories.edit", None)
        .await
        .expect("resolves");
    let outcome = (decision.allowed, decision.source);
    fixture.tear_down().await;

    assert_eq!(outcome, (true, "role_default"));
}
