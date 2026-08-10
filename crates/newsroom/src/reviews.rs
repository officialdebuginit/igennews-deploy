//! Module 5 — reviews, approvals, and comments. Ported from the legacy `api.py`
//! review/approval/comment handlers.
//!
//! The pivotal rule: deciding a review upserts the one approval per
//! (story, version, kind), valid only when approved or waived. Those approvals
//! are what the story READY gate reads. `notify()`/mention side effects are
//! written here alongside the audit trail.

use serde::Serialize;
use sqlx::FromRow;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::{Actor, NewsroomError, NewsroomService, audit, authz};

const REVIEW_KINDS: [&str; 7] = [
    "desk",
    "fact_check",
    "copy",
    "standards",
    "legal",
    "visuals",
    "audience",
];
const DECISIONS: [&str; 5] = [
    "pending",
    "approved",
    "changes_requested",
    "rejected",
    "waived",
];

/// Whether a decision string grants a valid approval.
fn decision_is_valid(decision: &str) -> bool {
    matches!(decision, "approved" | "waived")
}

#[derive(Clone, Debug, FromRow, Serialize)]
pub struct Review {
    pub id: Uuid,
    pub story_id: Uuid,
    pub version_id: Uuid,
    pub kind: String,
    pub assigned_to_id: Uuid,
    pub requested_by_id: Uuid,
    pub decision: String,
    pub notes: Option<String>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub decided_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(skip_serializing)]
    pub updated_at: OffsetDateTime,
}

#[derive(Clone, Debug, FromRow, Serialize)]
pub struct Approval {
    pub id: Uuid,
    pub story_id: Uuid,
    pub version_id: Uuid,
    pub review_id: Option<Uuid>,
    pub kind: String,
    pub approver_id: Uuid,
    pub decision: String,
    pub rationale: Option<String>,
    pub is_valid: bool,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    // Not part of the legacy approval contract.
    #[serde(skip_serializing)]
    pub invalidated_at: Option<OffsetDateTime>,
}

#[derive(Clone, Debug, FromRow, Serialize)]
pub struct Comment {
    pub id: Uuid,
    pub story_id: Uuid,
    pub version_id: Option<Uuid>,
    pub author_id: Uuid,
    pub body: String,
    pub locator: Option<String>,
    pub resolved: bool,
    pub resolved_by_id: Option<Uuid>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub resolved_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(skip_serializing)]
    pub updated_at: OffsetDateTime,
}

/// A review request.
#[derive(Clone, Debug)]
pub struct ReviewRequest {
    pub version_id: Uuid,
    pub kind: String,
    pub assigned_to_id: Uuid,
}

/// Filters for the review list.
#[derive(Clone, Debug, Default)]
pub struct ReviewFilter {
    pub decision: Option<String>,
    pub story_id: Option<Uuid>,
}

/// A new comment.
#[derive(Clone, Debug)]
pub struct CommentDraft {
    pub body: String,
    pub locator: Option<String>,
    pub version_id: Option<Uuid>,
}

impl NewsroomService {
    /// The sector owning the story a comment hangs off.
    async fn desk_for_comment(&self, comment_id: Uuid) -> Result<Option<Uuid>, NewsroomError> {
        Ok(sqlx::query_scalar(
            "SELECT s.desk_id FROM meridian.stories s \
             JOIN meridian.comments c ON c.story_id = s.id WHERE c.id = $1",
        )
        .bind(comment_id)
        .fetch_optional(self.pool())
        .await?
        .flatten())
    }

    /// Requests a review of a story version; the actor must be on the story team,
    /// the version must belong to the story, and the assignee must exist.
    ///
    /// # Errors
    /// [`NewsroomError::Forbidden`]; [`NewsroomError::Unprocessable`] for a bad
    /// kind or a cross-story version; [`NewsroomError::NotFound`]; database failures.
    pub async fn request_review(
        &self,
        actor: &Actor,
        story_id: Uuid,
        request: &ReviewRequest,
    ) -> Result<Review, NewsroomError> {
        let story = self.get_story(story_id).await?;
        if !self.may_edit_story(actor, &story).await? {
            return Err(team_only());
        }
        if !REVIEW_KINDS.contains(&request.kind.as_str()) {
            return Err(NewsroomError::Unprocessable(format!(
                "'{}' is not a review kind",
                request.kind
            )));
        }
        let version_story: Option<Uuid> = sqlx::query_scalar(
            "SELECT story_id FROM meridian.story_versions WHERE id = $1",
        )
        .bind(request.version_id)
        .fetch_optional(self.pool())
        .await?;
        match version_story {
            None => return Err(NewsroomError::NotFound("Story version")),
            Some(owner) if owner != story_id => {
                return Err(NewsroomError::Unprocessable(
                    "Version belongs to another story".to_owned(),
                ));
            }
            Some(_) => {}
        }
        let assignee_exists: bool =
            sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM meridian.users WHERE id = $1)")
                .bind(request.assigned_to_id)
                .fetch_one(self.pool())
                .await?;
        if !assignee_exists {
            return Err(NewsroomError::NotFound("User"));
        }
        let mut tx = self.pool().begin().await?;
        let review = sqlx::query_as::<_, Review>(
            "INSERT INTO meridian.reviews \
             (id, story_id, version_id, kind, assigned_to_id, requested_by_id) \
             VALUES ($1,$2,$3,$4,$5,$6) RETURNING *",
        )
        .bind(Uuid::now_v7())
        .bind(story_id)
        .bind(request.version_id)
        .bind(&request.kind)
        .bind(request.assigned_to_id)
        .bind(actor.id)
        .fetch_one(&mut *tx)
        .await?;
        crate::awareness::notify(
            &mut tx,
            &crate::awareness::NotifyInput {
                user_id: request.assigned_to_id,
                kind: "review.requested",
                title: "Review requested",
                body: None,
                entity_type: "review",
                entity_id: &review.id.to_string(),
                priority: "normal",
                group_key: None,
            },
        )
        .await?;
        audit(
            &mut *tx,
            actor.id,
            "review.requested",
            "review",
            &review.id.to_string(),
            None,
            Some(serde_json::json!({ "kind": review.kind })),
        )
        .await?;
        Self::enqueue_event(
            &mut *tx,
            "review.requested",
            &review.id.to_string(),
            None,
            serde_json::json!({ "review_id": review.id, "kind": review.kind }),
            Some(actor.id),
        )
        .await?;
        tx.commit().await?;
        Ok(review)
    }

    /// Lists reviews newest-first. Non-editors see only reviews assigned to them.
    ///
    /// # Errors
    /// Propagates database failures.
    pub async fn list_reviews(
        &self,
        actor: &Actor,
        filter: &ReviewFilter,
    ) -> Result<Vec<Review>, NewsroomError> {
        // Someone without `reviews.view_all` is scoped to their own assignments
        // by passing their id; a holder passes NULL and sees everything.
        let scope = if authz::has(self.pool(), actor, "reviews.view_all", None).await? {
            None
        } else {
            Some(actor.id)
        };
        Ok(sqlx::query_as::<_, Review>(
            "SELECT * FROM meridian.reviews \
             WHERE ($1::uuid IS NULL OR assigned_to_id = $1) \
               AND ($2::text IS NULL OR decision = $2) \
               AND ($3::uuid IS NULL OR story_id = $3) \
             ORDER BY created_at DESC",
        )
        .bind(scope)
        .bind(&filter.decision)
        .bind(filter.story_id)
        .fetch_all(self.pool())
        .await?)
    }

    /// Records a review decision and upserts the matching approval. The actor
    /// must be the assignee or an editor.
    ///
    /// # Errors
    /// [`NewsroomError::Forbidden`]; [`NewsroomError::Unprocessable`] for a bad
    /// decision; [`NewsroomError::NotFound`]; database failures.
    pub async fn decide_review(
        &self,
        actor: &Actor,
        review_id: Uuid,
        decision: &str,
        notes: Option<&str>,
    ) -> Result<Review, NewsroomError> {
        if !DECISIONS.contains(&decision) {
            return Err(NewsroomError::Unprocessable(format!(
                "'{decision}' is not a decision"
            )));
        }
        let review = sqlx::query_as::<_, Review>("SELECT * FROM meridian.reviews WHERE id = $1")
            .bind(review_id)
            .fetch_optional(self.pool())
            .await?
            .ok_or(NewsroomError::NotFound("Review"))?;
        // Sign-off must be independent: a story's author cannot decide a review of
        // their own story, even one assigned to them — otherwise one person could
        // self-assign and self-approve all three required sign-offs.
        let story = self.get_story(review.story_id).await?;
        if story.author_id == actor.id {
            return Err(NewsroomError::Forbidden {
                capability: "reviews.decide".to_owned(),
                reason: "An author cannot sign off a review of their own story".to_owned(),
            });
        }
        if actor.id != review.assigned_to_id {
            let desk = self.desk_for_story(review.story_id).await?;
            authz::require(self.pool(), actor, "reviews.decide_any", desk).await?;
        }
        let valid = decision_is_valid(decision);
        let mut tx = self.pool().begin().await?;
        let updated = sqlx::query_as::<_, Review>(
            "UPDATE meridian.reviews SET decision = $2, notes = $3, decided_at = now(), \
               updated_at = now() WHERE id = $1 RETURNING *",
        )
        .bind(review_id)
        .bind(decision)
        .bind(notes)
        .fetch_one(&mut *tx)
        .await?;
        // Upsert the one approval per (story, version, kind).
        sqlx::query(
            "INSERT INTO meridian.approvals \
             (id, story_id, version_id, review_id, kind, approver_id, decision, rationale, is_valid) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9) \
             ON CONFLICT (story_id, version_id, kind) DO UPDATE SET \
               review_id = EXCLUDED.review_id, \
               approver_id = EXCLUDED.approver_id, \
               decision = EXCLUDED.decision, \
               rationale = EXCLUDED.rationale, \
               is_valid = EXCLUDED.is_valid, \
               invalidated_at = CASE WHEN EXCLUDED.is_valid THEN NULL ELSE now() END",
        )
        .bind(Uuid::now_v7())
        .bind(review.story_id)
        .bind(review.version_id)
        .bind(review.id)
        .bind(&review.kind)
        .bind(actor.id)
        .bind(decision)
        .bind(notes)
        .bind(valid)
        .execute(&mut *tx)
        .await?;
        crate::awareness::notify(
            &mut tx,
            &crate::awareness::NotifyInput {
                user_id: review.requested_by_id,
                kind: "review.decided",
                title: &format!("Review {decision}"),
                body: notes,
                entity_type: "review",
                entity_id: &review.id.to_string(),
                priority: "normal",
                group_key: None,
            },
        )
        .await?;
        audit(
            &mut *tx,
            actor.id,
            "review.decided",
            "review",
            &review.id.to_string(),
            None,
            Some(serde_json::json!({ "decision": decision })),
        )
        .await?;
        let review_event = if decision == "approved" {
            "review.approved"
        } else {
            "review.rejected"
        };
        Self::enqueue_event(
            &mut *tx,
            review_event,
            &review.id.to_string(),
            None,
            serde_json::json!({ "review_id": review.id, "kind": review.kind, "decision": decision }),
            Some(actor.id),
        )
        .await?;
        tx.commit().await?;
        Ok(updated)
    }

    /// A story's approvals.
    ///
    /// # Errors
    /// Propagates database failures.
    pub async fn list_approvals(&self, story_id: Uuid) -> Result<Vec<Approval>, NewsroomError> {
        Ok(sqlx::query_as::<_, Approval>(
            "SELECT * FROM meridian.approvals WHERE story_id = $1 ORDER BY created_at",
        )
        .bind(story_id)
        .fetch_all(self.pool())
        .await?)
    }

    /// Adds a comment to a story (any authenticated user may comment).
    ///
    /// # Errors
    /// [`NewsroomError::NotFound`] for an unknown story; database failures.
    pub async fn create_comment(
        &self,
        actor: &Actor,
        story_id: Uuid,
        draft: &CommentDraft,
    ) -> Result<Comment, NewsroomError> {
        self.get_story(story_id).await?;
        let mut tx = self.pool().begin().await?;
        let comment = sqlx::query_as::<_, Comment>(
            "INSERT INTO meridian.comments (id, story_id, version_id, author_id, body, locator) \
             VALUES ($1,$2,$3,$4,$5,$6) RETURNING *",
        )
        .bind(Uuid::now_v7())
        .bind(story_id)
        .bind(draft.version_id)
        .bind(actor.id)
        .bind(&draft.body)
        .bind(&draft.locator)
        .fetch_one(&mut *tx)
        .await?;
        audit(
            &mut *tx,
            actor.id,
            "comment.created",
            "comment",
            &comment.id.to_string(),
            None,
            Some(serde_json::json!({ "story_id": story_id })),
        )
        .await?;
        Self::enqueue_event(
            &mut *tx,
            "comment.created",
            &comment.id.to_string(),
            None,
            serde_json::json!({ "comment_id": comment.id, "story_id": story_id }),
            Some(actor.id),
        )
        .await?;
        tx.commit().await?;
        Ok(comment)
    }

    /// A story's comments, oldest first.
    ///
    /// # Errors
    /// Propagates database failures.
    pub async fn list_comments(&self, story_id: Uuid) -> Result<Vec<Comment>, NewsroomError> {
        Ok(sqlx::query_as::<_, Comment>(
            "SELECT * FROM meridian.comments WHERE story_id = $1 ORDER BY created_at",
        )
        .bind(story_id)
        .fetch_all(self.pool())
        .await?)
    }

    /// Marks a comment resolved. Gated like [`Self::delete_comment`]: the comment's
    /// author, or a holder of `comments.moderate` in its desk. Without this, any
    /// authenticated user could clear review feedback on any desk's story.
    ///
    /// # Errors
    /// [`NewsroomError::Forbidden`] for a non-author without `comments.moderate`;
    /// [`NewsroomError::NotFound`]; database failures.
    pub async fn resolve_comment(
        &self,
        actor: &Actor,
        comment_id: Uuid,
    ) -> Result<Comment, NewsroomError> {
        let author_id: Option<Uuid> =
            sqlx::query_scalar("SELECT author_id FROM meridian.comments WHERE id = $1")
                .bind(comment_id)
                .fetch_optional(self.pool())
                .await?;
        let author_id = author_id.ok_or(NewsroomError::NotFound("Comment"))?;
        if author_id != actor.id {
            let desk = self.desk_for_comment(comment_id).await?;
            authz::require(self.pool(), actor, "comments.moderate", desk).await?;
        }
        let mut tx = self.pool().begin().await?;
        let comment = sqlx::query_as::<_, Comment>(
            "UPDATE meridian.comments SET resolved = true, resolved_by_id = $2, resolved_at = now(), \
               updated_at = now() WHERE id = $1 RETURNING *",
        )
        .bind(comment_id)
        .bind(actor.id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(NewsroomError::NotFound("Comment"))?;
        audit(
            &mut *tx,
            actor.id,
            "comment.resolved",
            "comment",
            &comment.id.to_string(),
            None,
            None,
        )
        .await?;
        Self::enqueue_event(
            &mut *tx,
            "comment.resolved",
            &comment.id.to_string(),
            None,
            serde_json::json!({ "comment_id": comment.id }),
            Some(actor.id),
        )
        .await?;
        tx.commit().await?;
        Ok(comment)
    }

    /// Deletes a comment; the author or an editor may delete it.
    ///
    /// # Errors
    /// [`NewsroomError::Forbidden`]; [`NewsroomError::NotFound`]; database failures.
    pub async fn delete_comment(
        &self,
        actor: &Actor,
        comment_id: Uuid,
    ) -> Result<(), NewsroomError> {
        let author_id: Option<Uuid> =
            sqlx::query_scalar("SELECT author_id FROM meridian.comments WHERE id = $1")
                .bind(comment_id)
                .fetch_optional(self.pool())
                .await?;
        let author_id = author_id.ok_or(NewsroomError::NotFound("Comment"))?;
        if author_id != actor.id {
            let desk = self.desk_for_comment(comment_id).await?;
            authz::require(self.pool(), actor, "comments.moderate", desk).await?;
        }
        let mut tx = self.pool().begin().await?;
        sqlx::query("DELETE FROM meridian.comments WHERE id = $1")
            .bind(comment_id)
            .execute(&mut *tx)
            .await?;
        audit(
            &mut *tx,
            actor.id,
            "comment.deleted",
            "comment",
            &comment_id.to_string(),
            None,
            None,
        )
        .await?;
        Self::enqueue_event(
            &mut *tx,
            "comment.deleted",
            &comment_id.to_string(),
            None,
            serde_json::json!({ "comment_id": comment_id }),
            Some(actor.id),
        )
        .await?;
        tx.commit().await?;
        Ok(())
    }
}

fn team_only() -> NewsroomError {
    NewsroomError::Forbidden {
        capability: "stories.team".to_owned(),
        reason: "You are not on this story team".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::decision_is_valid;

    #[test]
    fn only_approved_and_waived_are_valid() {
        assert!(decision_is_valid("approved"));
        assert!(decision_is_valid("waived"));
        assert!(!decision_is_valid("changes_requested"));
        assert!(!decision_is_valid("rejected"));
        assert!(!decision_is_valid("pending"));
    }
}
