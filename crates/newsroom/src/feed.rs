//! Module 7 (feed) — the newsroom feed: posts, per-viewer like/repost toggles,
//! and replies. Ported from the legacy `api.py` feed handlers. Like/repost/reply
//! counts are denormalised on the post and maintained alongside their rows.

use serde::Serialize;
use sqlx::FromRow;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::{Actor, NewsroomError, NewsroomService, authz};

/// A feed post with the viewer's own like/repost state.
#[derive(Clone, Debug, FromRow, Serialize)]
pub struct FeedPost {
    pub id: Uuid,
    pub author_id: Uuid,
    pub kind: String,
    pub content: String,
    pub story_id: Option<Uuid>,
    pub asset_id: Option<Uuid>,
    /// The linked story's headline and slug, resolved via `STORY_JOIN`, so a feed
    /// item can open the article. `story_published` gates the public reader link.
    #[sqlx(default)]
    pub story_title: Option<String>,
    #[sqlx(default)]
    pub story_slug: Option<String>,
    #[sqlx(default)]
    pub story_published: bool,
    pub likes: i32,
    pub reposts: i32,
    pub replies: i32,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    // Serialized as `liked`/`reposted` to match the legacy feed contract; the
    // SQL still aliases the columns `liked_by_me`/`reposted_by_me` for `FromRow`.
    #[serde(rename = "liked")]
    pub liked_by_me: bool,
    #[serde(rename = "reposted")]
    pub reposted_by_me: bool,
}

#[derive(Clone, Debug, FromRow, Serialize)]
pub struct FeedReply {
    pub id: Uuid,
    pub post_id: Uuid,
    pub author_id: Uuid,
    pub body: String,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

/// A new feed post.
#[derive(Clone, Debug)]
pub struct FeedPostDraft {
    pub kind: String,
    pub content: String,
    pub story_id: Option<Uuid>,
    pub asset_id: Option<Uuid>,
}

const POST_COLUMNS: &str = "p.id, p.author_id, p.kind, p.content, p.story_id, p.asset_id, \
    p.likes, p.reposts, p.replies, p.created_at, \
    s.title AS story_title, s.slug AS story_slug, \
    COALESCE(s.publication_state = 'published', false) AS story_published, \
    (l.user_id IS NOT NULL) AS liked_by_me, (r.user_id IS NOT NULL) AS reposted_by_me";

/// The `LEFT JOIN` that resolves a post's linked story (title/slug/published) so a
/// feed item can link to it. Kept alongside the like/repost joins in both queries.
const STORY_JOIN: &str = "LEFT JOIN meridian.stories s ON s.id = p.story_id";

impl NewsroomService {
    /// The feed, newest first, annotated with the viewer's like/repost state.
    /// Optionally filtered by post kind.
    ///
    /// # Errors
    /// Propagates database failures.
    pub async fn list_feed(
        &self,
        actor: &Actor,
        kind: Option<&str>,
        limit: i64,
    ) -> Result<Vec<FeedPost>, NewsroomError> {
        Ok(sqlx::query_as::<_, FeedPost>(&format!(
            "SELECT {POST_COLUMNS} FROM meridian.feed_posts p \
             LEFT JOIN meridian.feed_likes l ON l.post_id = p.id AND l.user_id = $1 \
             LEFT JOIN meridian.feed_reposts r ON r.post_id = p.id AND r.user_id = $1 \
             {STORY_JOIN} \
             WHERE ($2::text IS NULL OR p.kind = $2) \
             ORDER BY p.created_at DESC LIMIT $3"
        ))
        .bind(actor.id)
        .bind(kind)
        .bind(limit.clamp(1, 100))
        .fetch_all(self.pool())
        .await?)
    }

    async fn post_view(&self, actor: &Actor, post_id: Uuid) -> Result<FeedPost, NewsroomError> {
        sqlx::query_as::<_, FeedPost>(&format!(
            "SELECT {POST_COLUMNS} FROM meridian.feed_posts p \
             LEFT JOIN meridian.feed_likes l ON l.post_id = p.id AND l.user_id = $1 \
             LEFT JOIN meridian.feed_reposts r ON r.post_id = p.id AND r.user_id = $1 \
             {STORY_JOIN} \
             WHERE p.id = $2"
        ))
        .bind(actor.id)
        .bind(post_id)
        .fetch_optional(self.pool())
        .await?
        .ok_or(NewsroomError::NotFound("Feed post"))
    }

    /// Creates a feed post authored by the actor.
    ///
    /// # Errors
    /// Propagates database failures.
    pub async fn create_feed_post(
        &self,
        actor: &Actor,
        draft: &FeedPostDraft,
    ) -> Result<FeedPost, NewsroomError> {
        let mut tx = self.pool().begin().await?;
        let id = Uuid::now_v7();
        sqlx::query(
            "INSERT INTO meridian.feed_posts (id, author_id, kind, content, story_id, asset_id) \
             VALUES ($1,$2,$3,$4,$5,$6)",
        )
        .bind(id)
        .bind(actor.id)
        .bind(&draft.kind)
        .bind(&draft.content)
        .bind(draft.story_id)
        .bind(draft.asset_id)
        .execute(&mut *tx)
        .await?;
        crate::audit(
            &mut *tx,
            actor.id,
            "feed.post.created",
            "feed_post",
            &id.to_string(),
            None,
            Some(serde_json::json!({ "kind": draft.kind })),
        )
        .await?;
        tx.commit().await?;
        self.post_view(actor, id).await
    }

    /// Toggles the actor's like on a post, maintaining the counter.
    ///
    /// # Errors
    /// [`NewsroomError::NotFound`]; database failures.
    pub async fn toggle_feed_like(
        &self,
        actor: &Actor,
        post_id: Uuid,
    ) -> Result<FeedPost, NewsroomError> {
        self.toggle_reaction(actor, post_id, "feed_likes", "likes").await?;
        self.post_view(actor, post_id).await
    }

    /// Toggles the actor's repost on a post, maintaining the counter.
    ///
    /// # Errors
    /// [`NewsroomError::NotFound`]; database failures.
    pub async fn toggle_feed_repost(
        &self,
        actor: &Actor,
        post_id: Uuid,
    ) -> Result<FeedPost, NewsroomError> {
        self.toggle_reaction(actor, post_id, "feed_reposts", "reposts").await?;
        self.post_view(actor, post_id).await
    }

    /// Inserts-or-removes a per-user reaction row and adjusts the post's counter
    /// by ±1 in one transaction. `table` and `counter` are fixed internal
    /// identifiers, never caller input.
    async fn toggle_reaction(
        &self,
        actor: &Actor,
        post_id: Uuid,
        table: &str,
        counter: &str,
    ) -> Result<(), NewsroomError> {
        let exists: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM meridian.feed_posts WHERE id = $1)")
            .bind(post_id)
            .fetch_one(self.pool())
            .await?;
        if !exists {
            return Err(NewsroomError::NotFound("Feed post"));
        }
        let mut tx = self.pool().begin().await?;
        let removed = sqlx::query(&format!(
            "DELETE FROM meridian.{table} WHERE post_id = $1 AND user_id = $2"
        ))
        .bind(post_id)
        .bind(actor.id)
        .execute(&mut *tx)
        .await?;
        if removed.rows_affected() > 0 {
            sqlx::query(&format!(
                "UPDATE meridian.feed_posts SET {counter} = GREATEST({counter} - 1, 0), updated_at = now() WHERE id = $1"
            ))
            .bind(post_id)
            .execute(&mut *tx)
            .await?;
        } else {
            sqlx::query(&format!(
                "INSERT INTO meridian.{table} (post_id, user_id) VALUES ($1, $2)"
            ))
            .bind(post_id)
            .bind(actor.id)
            .execute(&mut *tx)
            .await?;
            sqlx::query(&format!(
                "UPDATE meridian.feed_posts SET {counter} = {counter} + 1, updated_at = now() WHERE id = $1"
            ))
            .bind(post_id)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// A post's replies, oldest first.
    ///
    /// # Errors
    /// Propagates database failures.
    pub async fn list_feed_replies(
        &self,
        post_id: Uuid,
    ) -> Result<Vec<FeedReply>, NewsroomError> {
        Ok(sqlx::query_as::<_, FeedReply>(
            "SELECT id, post_id, author_id, body, created_at FROM meridian.feed_replies \
             WHERE post_id = $1 ORDER BY created_at",
        )
        .bind(post_id)
        .fetch_all(self.pool())
        .await?)
    }

    /// Adds a reply to a post, incrementing its reply counter.
    ///
    /// # Errors
    /// [`NewsroomError::NotFound`]; database failures.
    pub async fn create_feed_reply(
        &self,
        actor: &Actor,
        post_id: Uuid,
        body: &str,
    ) -> Result<FeedReply, NewsroomError> {
        let exists: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM meridian.feed_posts WHERE id = $1)")
            .bind(post_id)
            .fetch_one(self.pool())
            .await?;
        if !exists {
            return Err(NewsroomError::NotFound("Feed post"));
        }
        let mut tx = self.pool().begin().await?;
        let reply = sqlx::query_as::<_, FeedReply>(
            "INSERT INTO meridian.feed_replies (id, post_id, author_id, body) \
             VALUES ($1,$2,$3,$4) RETURNING id, post_id, author_id, body, created_at",
        )
        .bind(Uuid::now_v7())
        .bind(post_id)
        .bind(actor.id)
        .bind(body)
        .fetch_one(&mut *tx)
        .await?;
        sqlx::query("UPDATE meridian.feed_posts SET replies = replies + 1, updated_at = now() WHERE id = $1")
            .bind(post_id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(reply)
    }

    /// Deletes a reply (author or editor), decrementing the post's counter.
    ///
    /// # Errors
    /// [`NewsroomError::Forbidden`]; [`NewsroomError::NotFound`]; database failures.
    pub async fn delete_feed_reply(
        &self,
        actor: &Actor,
        reply_id: Uuid,
    ) -> Result<(), NewsroomError> {
        let row: Option<(Uuid, Uuid)> =
            sqlx::query_as("SELECT post_id, author_id FROM meridian.feed_replies WHERE id = $1")
                .bind(reply_id)
                .fetch_optional(self.pool())
                .await?;
        let (post_id, author_id) = row.ok_or(NewsroomError::NotFound("Reply"))?;
        if author_id != actor.id {
            // The feed is a cross-sector surface, so moderation is org-wide.
            authz::require(self.pool(), actor, "feed.moderate", None).await?;
        }
        let mut tx = self.pool().begin().await?;
        sqlx::query("DELETE FROM meridian.feed_replies WHERE id = $1")
            .bind(reply_id)
            .execute(&mut *tx)
            .await?;
        sqlx::query("UPDATE meridian.feed_posts SET replies = GREATEST(replies - 1, 0), updated_at = now() WHERE id = $1")
            .bind(post_id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Deletes a feed post (author or editor).
    ///
    /// # Errors
    /// [`NewsroomError::Forbidden`]; [`NewsroomError::NotFound`]; database failures.
    pub async fn delete_feed_post(
        &self,
        actor: &Actor,
        post_id: Uuid,
    ) -> Result<(), NewsroomError> {
        let author_id: Option<Uuid> =
            sqlx::query_scalar("SELECT author_id FROM meridian.feed_posts WHERE id = $1")
                .bind(post_id)
                .fetch_optional(self.pool())
                .await?;
        let author_id = author_id.ok_or(NewsroomError::NotFound("Feed post"))?;
        if author_id != actor.id {
            authz::require(self.pool(), actor, "feed.moderate", None).await?;
        }
        let mut tx = self.pool().begin().await?;
        sqlx::query("DELETE FROM meridian.feed_posts WHERE id = $1")
            .bind(post_id)
            .execute(&mut *tx)
            .await?;
        crate::audit(
            &mut *tx,
            actor.id,
            "feed.post.deleted",
            "feed_post",
            &post_id.to_string(),
            None,
            None,
        )
        .await?;
        tx.commit().await?;
        Ok(())
    }
}
