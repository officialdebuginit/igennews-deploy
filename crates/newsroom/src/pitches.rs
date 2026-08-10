//! Module 4 — pitches, and the pitch decision that commissions a story. Ported
//! from the legacy `api.py` pitch handlers. The `notify()` side effects are
//! written here alongside the audit trail.

use serde::Serialize;
use sqlx::FromRow;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::{Actor, NewsroomError, NewsroomService, audit, authz};
use crate::stories::Story;

const PITCH_STATUSES: [&str; 6] = [
    "proposed",
    "needs_detail",
    "commissioned",
    "parked",
    "declined",
    "merged",
];

#[derive(Clone, Debug, FromRow, Serialize)]
pub struct Pitch {
    pub id: Uuid,
    pub headline: String,
    pub summary: String,
    pub angle: Option<String>,
    pub desk_id: Option<Uuid>,
    pub event_id: Option<Uuid>,
    pub created_by_id: Uuid,
    pub assignee_id: Option<Uuid>,
    pub editor_id: Option<Uuid>,
    pub status: String,
    pub priority: String,
    pub target_audience: Option<String>,
    pub expected_format: Option<String>,
    pub key_questions: serde_json::Value,
    pub likely_sources: serde_json::Value,
    pub risks: serde_json::Value,
    #[serde(with = "time::serde::rfc3339::option")]
    pub target_at: Option<OffsetDateTime>,
    pub decision_note: Option<String>,
    // Not part of the legacy pitch contract.
    #[serde(skip_serializing)]
    pub decided_by_id: Option<Uuid>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(skip_serializing)]
    pub updated_at: OffsetDateTime,
}

/// New-pitch input; `created_by` comes from the actor.
#[derive(Clone, Debug)]
pub struct PitchDraft {
    pub headline: String,
    pub summary: String,
    pub angle: Option<String>,
    pub desk_id: Option<Uuid>,
    pub event_id: Option<Uuid>,
    pub assignee_id: Option<Uuid>,
    pub editor_id: Option<Uuid>,
    pub priority: String,
    pub target_audience: Option<String>,
    pub expected_format: Option<String>,
    pub key_questions: serde_json::Value,
    pub likely_sources: serde_json::Value,
    pub risks: serde_json::Value,
    pub target_at: Option<OffsetDateTime>,
}

/// A pitch decision.
#[derive(Clone, Debug)]
pub struct PitchDecision {
    pub status: String,
    pub note: Option<String>,
    pub assignee_id: Option<Uuid>,
    pub editor_id: Option<Uuid>,
}

/// A decision either updates the pitch or commissions a story.
#[derive(Debug, Serialize)]
#[serde(untagged)]
pub enum PitchOutcome {
    Story(Story),
    Pitch(Pitch),
}

/// Filters for the pitch list.
#[derive(Clone, Debug, Default)]
pub struct PitchFilter {
    pub status: Option<String>,
    pub desk_id: Option<Uuid>,
}

impl NewsroomService {
    /// The sector a pitch belongs to, for scoping the commissioning decision.
    async fn desk_for_pitch(&self, pitch_id: Uuid) -> Result<Option<Uuid>, NewsroomError> {
        Ok(
            sqlx::query_scalar("SELECT desk_id FROM meridian.pitches WHERE id = $1")
                .bind(pitch_id)
                .fetch_optional(self.pool())
                .await?
                .flatten(),
        )
    }

    /// Loads a pitch or returns [`NewsroomError::NotFound`].
    ///
    /// # Errors
    /// Propagates database failures.
    pub async fn get_pitch(&self, pitch_id: Uuid) -> Result<Pitch, NewsroomError> {
        sqlx::query_as::<_, Pitch>("SELECT * FROM meridian.pitches WHERE id = $1")
            .bind(pitch_id)
            .fetch_optional(self.pool())
            .await?
            .ok_or(NewsroomError::NotFound("Pitch"))
    }

    /// Lists pitches newest-first, applying whichever filters are set.
    ///
    /// # Errors
    /// Propagates database failures.
    pub async fn list_pitches(&self, filter: &PitchFilter) -> Result<Vec<Pitch>, NewsroomError> {
        Ok(sqlx::query_as::<_, Pitch>(
            "SELECT * FROM meridian.pitches \
             WHERE ($1::text IS NULL OR status = $1) \
               AND ($2::uuid IS NULL OR desk_id = $2) \
             ORDER BY created_at DESC",
        )
        .bind(&filter.status)
        .bind(filter.desk_id)
        .fetch_all(self.pool())
        .await?)
    }

    /// Creates a pitch attributed to the actor.
    ///
    /// # Errors
    /// Propagates database failures.
    pub async fn create_pitch(
        &self,
        actor: &Actor,
        draft: &PitchDraft,
    ) -> Result<Pitch, NewsroomError> {
        if !["low", "medium", "high", "urgent"].contains(&draft.priority.as_str()) {
            return Err(NewsroomError::Unprocessable(
                "priority must be low, medium, high, or urgent".to_owned(),
            ));
        }
        let mut tx = self.pool().begin().await?;
        let pitch = sqlx::query_as::<_, Pitch>(
            "INSERT INTO meridian.pitches \
             (id, headline, summary, angle, desk_id, event_id, created_by_id, assignee_id, editor_id, \
              priority, target_audience, expected_format, key_questions, likely_sources, risks, target_at) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16) RETURNING *",
        )
        .bind(Uuid::now_v7())
        .bind(&draft.headline)
        .bind(&draft.summary)
        .bind(&draft.angle)
        .bind(draft.desk_id)
        .bind(draft.event_id)
        .bind(actor.id)
        .bind(draft.assignee_id)
        .bind(draft.editor_id)
        .bind(&draft.priority)
        .bind(&draft.target_audience)
        .bind(&draft.expected_format)
        .bind(&draft.key_questions)
        .bind(&draft.likely_sources)
        .bind(&draft.risks)
        .bind(draft.target_at)
        .fetch_one(&mut *tx)
        .await?;
        if let Some(editor) = pitch.editor_id {
            crate::awareness::notify(
                &mut tx,
                &crate::awareness::NotifyInput {
                    user_id: editor,
                    kind: "pitch.created",
                    title: "New pitch",
                    body: Some(&pitch.headline),
                    entity_type: "pitch",
                    entity_id: &pitch.id.to_string(),
                    priority: "normal",
                    group_key: None,
                },
            )
            .await?;
        }
        audit(
            &mut *tx,
            actor.id,
            "pitch.created",
            "pitch",
            &pitch.id.to_string(),
            None,
            Some(serde_json::json!({ "headline": pitch.headline })),
        )
        .await?;
        Self::enqueue_event(
            &mut *tx,
            "pitch.created",
            &pitch.id.to_string(),
            None,
            serde_json::json!({
                "id": pitch.id,
                "headline": pitch.headline,
                "status": pitch.status,
                "desk_id": pitch.desk_id,
            }),
            Some(actor.id),
        )
        .await?;
        tx.commit().await?;
        Ok(pitch)
    }

    /// Records a pitch decision. A `commissioned` decision creates the story
    /// (once — an existing commissioned story is returned unchanged). Requires an
    /// editor role.
    ///
    /// # Errors
    /// [`NewsroomError::Forbidden`] without an editor role;
    /// [`NewsroomError::Unprocessable`] for a bad status or a commission without
    /// an assignee; [`NewsroomError::NotFound`]; database failures.
    // The decision, the idempotent commission, and the notify/audit trail form
    // one transaction; splitting it would scatter that atomicity.
    #[allow(clippy::too_many_lines)]
    pub async fn decide_pitch(
        &self,
        actor: &Actor,
        pitch_id: Uuid,
        decision: &PitchDecision,
    ) -> Result<PitchOutcome, NewsroomError> {
        let pitch_desk = self.desk_for_pitch(pitch_id).await?;
        authz::require(self.pool(), actor, "pitches.decide", pitch_desk).await?;
        if !PITCH_STATUSES.contains(&decision.status.as_str()) {
            return Err(NewsroomError::Unprocessable(format!(
                "'{}' is not a pitch status",
                decision.status
            )));
        }
        let pitch = self.get_pitch(pitch_id).await?;
        let before = pitch.status.clone();

        let mut tx = self.pool().begin().await?;
        let updated = sqlx::query_as::<_, Pitch>(
            "UPDATE meridian.pitches SET status = $2, decision_note = $3, decided_by_id = $4, \
               assignee_id = COALESCE($5, assignee_id), \
               editor_id = COALESCE($6, COALESCE(editor_id, $4)), \
               updated_at = now() \
             WHERE id = $1 RETURNING *",
        )
        .bind(pitch_id)
        .bind(&decision.status)
        .bind(&decision.note)
        .bind(actor.id)
        .bind(decision.assignee_id)
        .bind(decision.editor_id)
        .fetch_one(&mut *tx)
        .await?;
        audit(
            &mut *tx,
            actor.id,
            "pitch.decided",
            "pitch",
            &pitch_id.to_string(),
            Some(serde_json::json!({ "status": before })),
            Some(serde_json::json!({ "status": updated.status, "note": decision.note })),
        )
        .await?;

        if updated.status != "commissioned" {
            if updated.status == "declined" {
                Self::enqueue_event(
                    &mut *tx,
                    "pitch.declined",
                    &pitch_id.to_string(),
                    None,
                    serde_json::json!({
                        "pitch_id": pitch_id,
                        "headline": updated.headline,
                        "note": decision.note,
                    }),
                    Some(actor.id),
                )
                .await?;
            }
            tx.commit().await?;
            return Ok(PitchOutcome::Pitch(updated));
        }
        let Some(assignee_id) = updated.assignee_id else {
            return Err(NewsroomError::Unprocessable(
                "Commissioned pitch requires an assignee".to_owned(),
            ));
        };
        // Commission is idempotent: one story per pitch.
        if let Some(existing) =
            sqlx::query_as::<_, Story>("SELECT * FROM meridian.stories WHERE pitch_id = $1")
                .bind(pitch_id)
                .fetch_optional(&mut *tx)
                .await?
        {
            tx.commit().await?;
            return Ok(PitchOutcome::Story(existing));
        }
        let slug = self.unique_story_slug(&mut tx, &updated.headline, pitch_id).await?;
        let story = sqlx::query_as::<_, Story>(
            "INSERT INTO meridian.stories \
             (id, slug, title, dek, category, priority, desk_id, event_id, pitch_id, author_id, \
              editor_id, due_at, workflow_state) \
             VALUES ($1,$2,$3,$4,'General',$5,$6,$7,$8,$9,$10,$11,'assigned') RETURNING *",
        )
        .bind(Uuid::now_v7())
        .bind(&slug)
        .bind(&updated.headline)
        .bind(&updated.summary)
        .bind(&updated.priority)
        .bind(updated.desk_id)
        .bind(updated.event_id)
        .bind(pitch_id)
        .bind(assignee_id)
        .bind(updated.editor_id)
        .bind(updated.target_at)
        .fetch_one(&mut *tx)
        .await?;
        crate::awareness::notify(
            &mut tx,
            &crate::awareness::NotifyInput {
                user_id: story.author_id,
                kind: "story.assigned",
                title: "Story assigned",
                body: Some(&story.title),
                entity_type: "story",
                entity_id: &story.id.to_string(),
                priority: "normal",
                group_key: None,
            },
        )
        .await?;
        audit(
            &mut *tx,
            actor.id,
            "story.commissioned",
            "story",
            &story.id.to_string(),
            None,
            Some(serde_json::json!({ "pitch_id": pitch_id })),
        )
        .await?;
        Self::enqueue_event(
            &mut *tx,
            "pitch.commissioned",
            &pitch_id.to_string(),
            None,
            serde_json::json!({ "pitch_id": pitch_id, "story_id": story.id, "headline": updated.headline }),
            Some(actor.id),
        )
        .await?;
        Self::enqueue_event(
            &mut *tx,
            "article.commissioned",
            &story.id.to_string(),
            None,
            serde_json::json!({ "story_id": story.id, "slug": story.slug, "pitch_id": pitch_id, "title": story.title }),
            Some(actor.id),
        )
        .await?;
        tx.commit().await?;
        Ok(PitchOutcome::Story(story))
    }

    /// Derives a unique story slug from a headline, matching the legacy scheme.
    async fn unique_story_slug(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        headline: &str,
        pitch_id: Uuid,
    ) -> Result<String, NewsroomError> {
        let base = slugify(headline).unwrap_or_else(|| {
            format!("story-{}", &pitch_id.simple().to_string()[..8])
        });
        let mut slug = base.clone();
        let mut suffix = 2;
        loop {
            let taken: bool =
                sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM meridian.stories WHERE slug = $1)")
                    .bind(&slug)
                    .fetch_one(&mut **tx)
                    .await?;
            if !taken {
                return Ok(slug);
            }
            slug = format!("{base}-{suffix}");
            suffix += 1;
        }
    }
}

/// Lowercases, collapses whitespace to hyphens, truncates to 240, and trims
/// stray hyphens. Returns `None` when nothing usable remains.
fn slugify(headline: &str) -> Option<String> {
    let joined = headline
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join("-");
    let truncated: String = joined.chars().take(240).collect();
    let trimmed = truncated.trim_matches('-');
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::slugify;

    #[test]
    fn slugify_matches_legacy_scheme() {
        assert_eq!(slugify("Council Votes On Budget").as_deref(), Some("council-votes-on-budget"));
        assert_eq!(slugify("  Spaced   Out  ").as_deref(), Some("spaced-out"));
        assert_eq!(slugify("!!!").as_deref(), Some("!!!")); // punctuation is kept, only hyphens trimmed
        assert_eq!(slugify("   ").as_deref(), None);
    }
}
