//! Module 7 (search) — full-text story search over the generated `tsvector`,
//! plus facets and saved searches. Ported from the legacy `awareness/search.py`.
//!
//! Since migration `0017` the index covers the jsonb block body as well as the
//! text metadata, and the fields are weighted (title A, dek B, category C, body D)
//! so `ts_rank` distinguishes a headline match from a passing mention. `reindex`
//! is a genuine no-op rather than a stub: `PostgreSQL` maintains a generated column,
//! so there is never anything to rebuild.

use serde::Serialize;
use sqlx::FromRow;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::{Actor, NewsroomError, NewsroomService, authz};

/// One ranked search hit.
#[derive(Clone, Debug, FromRow, Serialize)]
pub struct SearchHit {
    pub id: Uuid,
    pub slug: String,
    pub title: String,
    pub dek: String,
    pub category: String,
    pub workflow_state: String,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
    pub rank: f32,
}

/// The optional narrowing applied to a search, all combined with AND.
#[derive(Clone, Debug, Default)]
pub struct SearchFilters {
    pub workflow_state: Option<String>,
    pub category: Option<String>,
    pub desk_id: Option<Uuid>,
    /// Inclusive lower bound on `updated_at`.
    pub since: Option<OffsetDateTime>,
    /// Exclusive upper bound on `updated_at`.
    pub until: Option<OffsetDateTime>,
    pub limit: i64,
    pub offset: i64,
}

/// A page of search results with the total match count and query-aware facets.
#[derive(Debug, Serialize)]
pub struct SearchPage {
    pub hits: Vec<SearchHit>,
    pub total: i64,
    pub facets: SearchFacets,
}

/// A hit from a non-story corpus (coverage events, corrections), reduced to a
/// common shape so the search UI can list every kind the same way.
#[derive(Clone, Debug, FromRow, Serialize)]
pub struct CorpusHit {
    pub id: Uuid,
    pub kind: String,
    pub title: String,
    pub subtitle: String,
    pub meta: String,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
}

#[derive(Clone, Debug, FromRow, Serialize)]
pub struct SavedSearch {
    pub id: Uuid,
    pub user_id: Uuid,
    pub name: String,
    pub query: String,
    pub filters_json: serde_json::Value,
    pub is_shared: bool,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(skip_serializing)]
    pub updated_at: OffsetDateTime,
}

/// A saved search to create.
#[derive(Clone, Debug)]
pub struct SavedSearchInput {
    pub name: String,
    pub query: String,
    pub filters: serde_json::Value,
    pub is_shared: bool,
}

/// Facet counts over the story corpus, for search refinement.
#[derive(Debug, Default, Serialize)]
pub struct SearchFacets {
    pub workflow_state: std::collections::BTreeMap<String, i64>,
    pub category: std::collections::BTreeMap<String, i64>,
}

impl NewsroomService {
    /// Facet counts (by workflow state and category) over all stories.
    ///
    /// # Errors
    /// Propagates database failures.
    pub async fn search_facets(&self) -> Result<SearchFacets, NewsroomError> {
        let by_state: Vec<(String, i64)> = sqlx::query_as(
            "SELECT workflow_state, count(*) FROM meridian.stories GROUP BY workflow_state",
        )
        .fetch_all(self.pool())
        .await?;
        let by_category: Vec<(String, i64)> = sqlx::query_as(
            "SELECT category, count(*) FROM meridian.stories GROUP BY category",
        )
        .fetch_all(self.pool())
        .await?;
        Ok(SearchFacets {
            workflow_state: by_state.into_iter().collect(),
            category: by_category.into_iter().collect(),
        })
    }

    /// Ranked full-text search over stories. An empty query returns nothing.
    ///
    /// # Errors
    /// Propagates database failures.
    pub async fn search_stories(
        &self,
        query: &str,
        limit: i64,
    ) -> Result<Vec<SearchHit>, NewsroomError> {
        if query.trim().is_empty() {
            return Ok(Vec::new());
        }
        Ok(sqlx::query_as::<_, SearchHit>(
            "SELECT id, slug, title, COALESCE(dek, '') AS dek, category, workflow_state, updated_at, \
               ts_rank(search_tsv, plainto_tsquery('english', $1)) AS rank \
             FROM meridian.stories \
             WHERE search_tsv @@ plainto_tsquery('english', $1) \
             ORDER BY rank DESC, updated_at DESC LIMIT $2",
        )
        .bind(query)
        .bind(limit.clamp(1, 100))
        .fetch_all(self.pool())
        .await?)
    }

    /// Ranked full-text search narrowed server-side by `filters`, returning a page
    /// of hits, the total match count (for pagination), and facet counts computed
    /// over the query + desk + date filters (so the state/category counts show how
    /// the *current* result set distributes, guiding the next click).
    ///
    /// An empty query with no filters returns nothing, matching the plain search;
    /// an empty query *with* filters browses the filtered corpus by recency.
    ///
    /// # Errors
    /// Propagates database failures.
    pub async fn search_stories_filtered(
        &self,
        actor: &Actor,
        query: &str,
        filters: &SearchFilters,
    ) -> Result<SearchPage, NewsroomError> {
        let q = query.trim();
        let no_filters = filters.workflow_state.is_none()
            && filters.category.is_none()
            && filters.desk_id.is_none()
            && filters.since.is_none()
            && filters.until.is_none();
        if q.is_empty() && no_filters {
            return Ok(SearchPage { hits: Vec::new(), total: 0, facets: SearchFacets::default() });
        }
        let limit = filters.limit.clamp(1, 100);
        let offset = filters.offset.max(0);
        // Desk-private visibility, mirroring `list_stories`: `dashboard.view_global`
        // sees every desk; everyone else sees only desks they belong to (plus
        // org-level stories with no desk). Without this, search surfaced other desks'
        // unpublished drafts to any authenticated user. Bound as the last two params
        // ($sees_all, $actor) of each query.
        let sees_all = authz::has(self.pool(), actor, "dashboard.view_global", None).await?;

        // Bound conditions shared by the result, count and facet queries. Each
        // `($n IS NULL OR col = $n)` is a no-op when the filter is absent.
        let hits = sqlx::query_as::<_, SearchHit>(
            "SELECT id, slug, title, COALESCE(dek, '') AS dek, category, workflow_state, updated_at, \
               CASE WHEN $1 <> '' THEN ts_rank(search_tsv, plainto_tsquery('english', $1)) ELSE 0 END AS rank \
             FROM meridian.stories \
             WHERE ($1 = '' OR search_tsv @@ plainto_tsquery('english', $1)) \
               AND ($2::text IS NULL OR workflow_state = $2) \
               AND ($3::text IS NULL OR category = $3) \
               AND ($4::uuid IS NULL OR desk_id = $4) \
               AND ($5::timestamptz IS NULL OR updated_at >= $5) \
               AND ($6::timestamptz IS NULL OR updated_at < $6) \
               AND ($9::bool OR desk_id IS NULL \
                    OR desk_id IN (SELECT desk_id FROM meridian.desk_memberships WHERE user_id = $10)) \
             ORDER BY rank DESC, updated_at DESC LIMIT $7 OFFSET $8",
        )
        .bind(q)
        .bind(&filters.workflow_state)
        .bind(&filters.category)
        .bind(filters.desk_id)
        .bind(filters.since)
        .bind(filters.until)
        .bind(limit)
        .bind(offset)
        .bind(sees_all)
        .bind(actor.id)
        .fetch_all(self.pool())
        .await?;

        let total: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM meridian.stories \
             WHERE ($1 = '' OR search_tsv @@ plainto_tsquery('english', $1)) \
               AND ($2::text IS NULL OR workflow_state = $2) \
               AND ($3::text IS NULL OR category = $3) \
               AND ($4::uuid IS NULL OR desk_id = $4) \
               AND ($5::timestamptz IS NULL OR updated_at >= $5) \
               AND ($6::timestamptz IS NULL OR updated_at < $6) \
               AND ($7::bool OR desk_id IS NULL \
                    OR desk_id IN (SELECT desk_id FROM meridian.desk_memberships WHERE user_id = $8))",
        )
        .bind(q)
        .bind(&filters.workflow_state)
        .bind(&filters.category)
        .bind(filters.desk_id)
        .bind(filters.since)
        .bind(filters.until)
        .bind(sees_all)
        .bind(actor.id)
        .fetch_one(self.pool())
        .await?;

        // Facets ignore the state/category selectors themselves, so choosing one
        // value still shows the sibling counts to switch to.
        let by_state: Vec<(String, i64)> = sqlx::query_as(
            "SELECT workflow_state, count(*) FROM meridian.stories \
             WHERE ($1 = '' OR search_tsv @@ plainto_tsquery('english', $1)) \
               AND ($2::uuid IS NULL OR desk_id = $2) \
               AND ($3::timestamptz IS NULL OR updated_at >= $3) \
               AND ($4::timestamptz IS NULL OR updated_at < $4) \
               AND ($5::bool OR desk_id IS NULL \
                    OR desk_id IN (SELECT desk_id FROM meridian.desk_memberships WHERE user_id = $6)) \
             GROUP BY workflow_state",
        )
        .bind(q)
        .bind(filters.desk_id)
        .bind(filters.since)
        .bind(filters.until)
        .bind(sees_all)
        .bind(actor.id)
        .fetch_all(self.pool())
        .await?;
        let by_category: Vec<(String, i64)> = sqlx::query_as(
            "SELECT category, count(*) FROM meridian.stories \
             WHERE ($1 = '' OR search_tsv @@ plainto_tsquery('english', $1)) \
               AND ($2::uuid IS NULL OR desk_id = $2) \
               AND ($3::timestamptz IS NULL OR updated_at >= $3) \
               AND ($4::timestamptz IS NULL OR updated_at < $4) \
               AND ($5::bool OR desk_id IS NULL \
                    OR desk_id IN (SELECT desk_id FROM meridian.desk_memberships WHERE user_id = $6)) \
             GROUP BY category",
        )
        .bind(q)
        .bind(filters.desk_id)
        .bind(filters.since)
        .bind(filters.until)
        .bind(sees_all)
        .bind(actor.id)
        .fetch_all(self.pool())
        .await?;

        Ok(SearchPage {
            hits,
            total,
            facets: SearchFacets {
                workflow_state: by_state.into_iter().collect(),
                category: by_category.into_iter().collect(),
            },
        })
    }

    /// Substring search over a non-story corpus — `coverage` events or
    /// `corrections` — reduced to [`CorpusHit`], with an optional desk (coverage
    /// only) and `updated_at` range, paginated. Returns the page and the total.
    ///
    /// Uses `ILIKE` rather than the story `tsvector`: these tables have no search
    /// column, and a case-insensitive substring is the honest match for short
    /// operational text. An unknown `kind` yields an empty page, not an error.
    ///
    /// # Errors
    /// Propagates database failures.
    pub async fn search_corpus(
        &self,
        actor: &Actor,
        kind: &str,
        query: &str,
        desk_id: Option<Uuid>,
        since: Option<OffsetDateTime>,
        until: Option<OffsetDateTime>,
        limit: i64,
        offset: i64,
    ) -> Result<(Vec<CorpusHit>, i64), NewsroomError> {
        let q = query.trim();
        let like = format!("%{q}%");
        let limit = limit.clamp(1, 100);
        let offset = offset.max(0);
        // Desk-private, like `search_stories_filtered`: `dashboard.view_global` sees
        // every desk, everyone else only their own. The corrections branch had no
        // desk filter at all (it leaked every desk's correction text/classification);
        // coverage accepted a client desk_id without checking membership.
        let sees_all = authz::has(self.pool(), actor, "dashboard.view_global", None).await?;
        match kind {
            "coverage" => {
                let hits = sqlx::query_as::<_, CorpusHit>(
                    "SELECT id, 'coverage' AS kind, title, \
                            COALESCE(location, '') AS subtitle, \
                            COALESCE(status, 'planned') AS meta, updated_at \
                     FROM meridian.coverage_events \
                     WHERE ($1 = '' OR title ILIKE $2 OR description ILIKE $2 OR location ILIKE $2) \
                       AND ($3::uuid IS NULL OR desk_id = $3) \
                       AND ($4::timestamptz IS NULL OR updated_at >= $4) \
                       AND ($5::timestamptz IS NULL OR updated_at < $5) \
                       AND ($8::bool OR desk_id IS NULL \
                            OR desk_id IN (SELECT desk_id FROM meridian.desk_memberships WHERE user_id = $9)) \
                     ORDER BY updated_at DESC LIMIT $6 OFFSET $7",
                )
                .bind(q)
                .bind(&like)
                .bind(desk_id)
                .bind(since)
                .bind(until)
                .bind(limit)
                .bind(offset)
                .bind(sees_all)
                .bind(actor.id)
                .fetch_all(self.pool())
                .await?;
                let total: i64 = sqlx::query_scalar(
                    "SELECT count(*) FROM meridian.coverage_events \
                     WHERE ($1 = '' OR title ILIKE $2 OR description ILIKE $2 OR location ILIKE $2) \
                       AND ($3::uuid IS NULL OR desk_id = $3) \
                       AND ($4::timestamptz IS NULL OR updated_at >= $4) \
                       AND ($5::timestamptz IS NULL OR updated_at < $5) \
                       AND ($6::bool OR desk_id IS NULL \
                            OR desk_id IN (SELECT desk_id FROM meridian.desk_memberships WHERE user_id = $7))",
                )
                .bind(q)
                .bind(&like)
                .bind(desk_id)
                .bind(since)
                .bind(until)
                .bind(sees_all)
                .bind(actor.id)
                .fetch_one(self.pool())
                .await?;
                Ok((hits, total))
            }
            "corrections" => {
                let hits = sqlx::query_as::<_, CorpusHit>(
                    "SELECT c.id, 'correction' AS kind, \
                            COALESCE(NULLIF(s.title, ''), 'Correction') AS title, \
                            COALESCE(NULLIF(c.description, ''), c.public_note, '') AS subtitle, \
                            c.classification || ' \u{00b7} ' || c.status AS meta, c.updated_at \
                     FROM meridian.corrections c LEFT JOIN meridian.stories s ON s.id = c.story_id \
                     WHERE ($1 = '' OR c.description ILIKE $2 OR c.public_note ILIKE $2 \
                            OR c.classification ILIKE $2 OR s.title ILIKE $2) \
                       AND ($3::timestamptz IS NULL OR c.updated_at >= $3) \
                       AND ($4::timestamptz IS NULL OR c.updated_at < $4) \
                       AND ($7::bool OR s.desk_id IS NULL \
                            OR s.desk_id IN (SELECT desk_id FROM meridian.desk_memberships WHERE user_id = $8)) \
                     ORDER BY c.updated_at DESC LIMIT $5 OFFSET $6",
                )
                .bind(q)
                .bind(&like)
                .bind(since)
                .bind(until)
                .bind(limit)
                .bind(offset)
                .bind(sees_all)
                .bind(actor.id)
                .fetch_all(self.pool())
                .await?;
                let total: i64 = sqlx::query_scalar(
                    "SELECT count(*) FROM meridian.corrections c \
                            LEFT JOIN meridian.stories s ON s.id = c.story_id \
                     WHERE ($1 = '' OR c.description ILIKE $2 OR c.public_note ILIKE $2 \
                            OR c.classification ILIKE $2 OR s.title ILIKE $2) \
                       AND ($3::timestamptz IS NULL OR c.updated_at >= $3) \
                       AND ($4::timestamptz IS NULL OR c.updated_at < $4) \
                       AND ($5::bool OR s.desk_id IS NULL \
                            OR s.desk_id IN (SELECT desk_id FROM meridian.desk_memberships WHERE user_id = $6))",
                )
                .bind(q)
                .bind(&like)
                .bind(since)
                .bind(until)
                .bind(sees_all)
                .bind(actor.id)
                .fetch_one(self.pool())
                .await?;
                Ok((hits, total))
            }
            _ => Ok((Vec::new(), 0)),
        }
    }

    /// The caller's saved searches, plus any shared ones.
    ///
    /// # Errors
    /// Propagates database failures.
    pub async fn list_saved_searches(
        &self,
        actor: &Actor,
    ) -> Result<Vec<SavedSearch>, NewsroomError> {
        Ok(sqlx::query_as::<_, SavedSearch>(
            "SELECT * FROM meridian.saved_searches \
             WHERE user_id = $1 OR is_shared ORDER BY created_at DESC",
        )
        .bind(actor.id)
        .fetch_all(self.pool())
        .await?)
    }

    /// Saves a search for the caller.
    ///
    /// # Errors
    /// Propagates database failures.
    pub async fn create_saved_search(
        &self,
        actor: &Actor,
        input: &SavedSearchInput,
    ) -> Result<SavedSearch, NewsroomError> {
        Ok(sqlx::query_as::<_, SavedSearch>(
            "INSERT INTO meridian.saved_searches (id, user_id, name, query, filters_json, is_shared) \
             VALUES ($1,$2,$3,$4,$5,$6) RETURNING *",
        )
        .bind(Uuid::now_v7())
        .bind(actor.id)
        .bind(&input.name)
        .bind(&input.query)
        .bind(&input.filters)
        .bind(input.is_shared)
        .fetch_one(self.pool())
        .await?)
    }

    /// Deletes one of the caller's saved searches.
    ///
    /// # Errors
    /// [`NewsroomError::NotFound`]; database failures.
    pub async fn delete_saved_search(
        &self,
        actor: &Actor,
        search_id: Uuid,
    ) -> Result<(), NewsroomError> {
        let deleted = sqlx::query(
            "DELETE FROM meridian.saved_searches WHERE id = $1 AND user_id = $2",
        )
        .bind(search_id)
        .bind(actor.id)
        .execute(self.pool())
        .await?;
        if deleted.rows_affected() == 0 {
            return Err(NewsroomError::NotFound("Saved search"));
        }
        Ok(())
    }
}
