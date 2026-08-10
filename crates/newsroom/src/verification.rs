//! Module 4 — verification: sources (with ground-rule confidentiality),
//! evidence, and claims. Ported from the legacy `api.py` verification handlers
//! and `services.can_reveal_source`.
//!
//! Source confidentiality is enforced on read: a caller who may not reveal a
//! source never receives its identity, motive, or reliability notes.

use serde::Serialize;
use sqlx::FromRow;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::stories::Story;
use crate::{Actor, NewsroomError, NewsroomService, Role, audit, authz};

#[derive(Clone, Debug, FromRow, Serialize)]
pub struct Source {
    pub id: Uuid,
    pub story_id: Uuid,
    pub identity: String,
    pub publishable_attribution: String,
    pub ground_rule: String,
    pub description: Option<String>,
    pub motive: Option<String>,
    pub reliability_notes: Option<String>,
    pub access_level: String,
    pub manager_approved_by_id: Option<Uuid>,
    pub approval_rationale: Option<String>,
    pub created_by_id: Uuid,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(skip_serializing)]
    pub updated_at: OffsetDateTime,
}

/// A source as returned to a caller. `identity`, `motive`, and
/// `reliability_notes` are redacted to `None` when the caller may not reveal the
/// source, regardless of their stored values.
#[derive(Clone, Debug, Serialize)]
pub struct SourceView {
    pub id: Uuid,
    pub story_id: Uuid,
    pub identity: Option<String>,
    pub publishable_attribution: String,
    pub ground_rule: String,
    pub description: Option<String>,
    pub motive: Option<String>,
    pub reliability_notes: Option<String>,
    pub access_level: String,
    pub manager_approved_by_id: Option<Uuid>,
    pub approval_rationale: Option<String>,
    pub created_by_id: Uuid,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

impl SourceView {
    fn from_source(source: Source, reveal: bool) -> Self {
        Self {
            id: source.id,
            story_id: source.story_id,
            identity: reveal.then_some(source.identity),
            publishable_attribution: source.publishable_attribution,
            ground_rule: source.ground_rule,
            description: source.description,
            motive: if reveal { source.motive } else { None },
            reliability_notes: if reveal { source.reliability_notes } else { None },
            access_level: source.access_level,
            manager_approved_by_id: source.manager_approved_by_id,
            approval_rationale: source.approval_rationale,
            created_by_id: source.created_by_id,
            created_at: source.created_at,
        }
    }
}

/// New-source input.
#[derive(Clone, Debug)]
pub struct SourceDraft {
    pub identity: String,
    pub publishable_attribution: String,
    pub ground_rule: String,
    pub description: Option<String>,
    pub motive: Option<String>,
    pub reliability_notes: Option<String>,
    pub access_level: String,
}

#[derive(Clone, Debug, FromRow, Serialize)]
pub struct Evidence {
    pub id: Uuid,
    pub story_id: Uuid,
    pub source_id: Option<Uuid>,
    pub kind: String,
    pub title: String,
    pub uri: Option<String>,
    pub notes: Option<String>,
    pub checksum: Option<String>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub acquired_at: Option<OffsetDateTime>,
    pub verification_status: String,
    pub chain_of_custody: serde_json::Value,
    pub created_by_id: Uuid,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(skip_serializing)]
    pub updated_at: OffsetDateTime,
}

/// New-evidence input.
#[derive(Clone, Debug)]
pub struct EvidenceDraft {
    pub source_id: Option<Uuid>,
    pub kind: String,
    pub title: String,
    pub uri: Option<String>,
    pub notes: Option<String>,
    pub checksum: Option<String>,
    pub acquired_at: Option<OffsetDateTime>,
}

#[derive(Clone, Debug, FromRow, Serialize)]
pub struct Claim {
    pub id: Uuid,
    pub story_id: Uuid,
    pub version_id: Option<Uuid>,
    pub text: String,
    pub locator: Option<String>,
    pub status: String,
    pub evidence_ids: serde_json::Value,
    pub reviewed_by_id: Option<Uuid>,
    pub decision_note: Option<String>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub reviewed_at: Option<OffsetDateTime>,
    #[serde(skip_serializing)]
    pub created_at: OffsetDateTime,
    #[serde(skip_serializing)]
    pub updated_at: OffsetDateTime,
}

/// New-claim input.
#[derive(Clone, Debug)]
pub struct ClaimDraft {
    pub text: String,
    pub locator: Option<String>,
    pub version_id: Option<Uuid>,
    pub evidence_ids: serde_json::Value,
}

const VERIFICATION_STATUSES: [&str; 5] =
    ["unverified", "in_progress", "verified", "disputed", "rejected"];

/// One entry in an evidence item's chain of custody.
///
/// The shape is chosen here rather than inherited: `chain_of_custody` is jsonb
/// with no writer anywhere, so nothing constrained it. A custody record needs to
/// answer *who did what to this, when* — so an entry is exactly
/// `{at, actor_id, action, note}`, and entries are only ever appended.
///
/// `action` is `"recorded"` at creation and `"status:<new-status>"` on a
/// transition, which keeps the vocabulary open without inventing a second field
/// to distinguish the two.
fn custody_entry(actor_id: Uuid, action: &str, note: Option<&str>) -> serde_json::Value {
    serde_json::json!({
        "at": OffsetDateTime::now_utc()
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap_or_default(),
        "actor_id": actor_id,
        "action": action,
        "note": note,
    })
}
const GROUND_RULES: [&str; 4] = ["on_record", "background", "deep_background", "off_record"];

impl NewsroomService {
    /// Loads a source or returns [`NewsroomError::NotFound`].
    ///
    /// # Errors
    /// Propagates database failures.
    pub async fn get_source(&self, source_id: Uuid) -> Result<Source, NewsroomError> {
        sqlx::query_as::<_, Source>("SELECT * FROM meridian.sources WHERE id = $1")
            .bind(source_id)
            .fetch_optional(self.pool())
            .await?
            .ok_or(NewsroomError::NotFound("Source"))
    }

    /// The legacy `can_reveal_source`: senior editors, the story's author or
    /// editor, the source's creator, or anyone with an explicit access grant.
    ///
    /// # Errors
    /// Propagates database failures.
    async fn can_reveal_source(
        &self,
        actor: &Actor,
        source: &Source,
        story: &Story,
    ) -> Result<bool, NewsroomError> {
        // Editor-in-chief and standards/legal are true org-wide roles, so they
        // answer newsroom-wide. A managing editor is a per-desk role, so resolve
        // their reach against the story's own desk (via the confidential-source
        // capability) rather than the bare org-level role — otherwise a Sports
        // managing editor could unmask a Politics source.
        if matches!(actor.role, Role::EditorInChief | Role::StandardsLegal) {
            return Ok(true);
        }
        if authz::has(self.pool(), actor, "sources.approve", story.desk_id).await? {
            return Ok(true);
        }
        if [Some(story.author_id), story.editor_id, Some(source.created_by_id)]
            .into_iter()
            .flatten()
            .any(|member| member == actor.id)
        {
            return Ok(true);
        }
        let granted: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM meridian.source_access_grants \
             WHERE source_id = $1 AND user_id = $2)",
        )
        .bind(source.id)
        .bind(actor.id)
        .fetch_one(self.pool())
        .await?;
        Ok(granted)
    }

    /// Creates a source on a story the actor is on, granting the creator access.
    ///
    /// # Errors
    /// [`NewsroomError::Forbidden`] off the team;
    /// [`NewsroomError::Unprocessable`] for an unknown ground rule; database
    /// failures.
    pub async fn create_source(
        &self,
        actor: &Actor,
        story: &Story,
        draft: &SourceDraft,
    ) -> Result<Source, NewsroomError> {
        if !self.may_edit_story(actor, story).await? {
            return Err(team_only());
        }
        if !GROUND_RULES.contains(&draft.ground_rule.as_str()) {
            return Err(NewsroomError::Unprocessable(format!(
                "'{}' is not a ground rule",
                draft.ground_rule
            )));
        }
        let mut tx = self.pool().begin().await?;
        let source = sqlx::query_as::<_, Source>(
            "INSERT INTO meridian.sources \
             (id, story_id, identity, publishable_attribution, ground_rule, description, motive, \
              reliability_notes, access_level, created_by_id) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10) RETURNING *",
        )
        .bind(Uuid::now_v7())
        .bind(story.id)
        .bind(&draft.identity)
        .bind(&draft.publishable_attribution)
        .bind(&draft.ground_rule)
        .bind(&draft.description)
        .bind(&draft.motive)
        .bind(&draft.reliability_notes)
        .bind(&draft.access_level)
        .bind(actor.id)
        .fetch_one(&mut *tx)
        .await?;
        sqlx::query(
            "INSERT INTO meridian.source_access_grants (source_id, user_id, granted_by_id) \
             VALUES ($1, $2, $2)",
        )
        .bind(source.id)
        .bind(actor.id)
        .execute(&mut *tx)
        .await?;
        audit(
            &mut *tx,
            actor.id,
            "source.created",
            "source",
            &source.id.to_string(),
            None,
            Some(serde_json::json!({ "story_id": story.id, "ground_rule": source.ground_rule })),
        )
        .await?;
        Self::enqueue_event(
            &mut *tx,
            "source.created",
            &source.id.to_string(),
            None,
            serde_json::json!({ "source_id": source.id, "story_id": story.id }),
            Some(actor.id),
        )
        .await?;
        tx.commit().await?;
        Ok(source)
    }

    /// Lists a story's sources, redacting confidential fields per source.
    ///
    /// # Errors
    /// Propagates database failures.
    pub async fn list_sources(
        &self,
        actor: &Actor,
        story: &Story,
    ) -> Result<Vec<SourceView>, NewsroomError> {
        let sources = sqlx::query_as::<_, Source>(
            "SELECT * FROM meridian.sources WHERE story_id = $1 ORDER BY created_at",
        )
        .bind(story.id)
        .fetch_all(self.pool())
        .await?;
        let mut views = Vec::with_capacity(sources.len());
        for source in sources {
            let reveal = self.can_reveal_source(actor, &source, story).await?;
            views.push(SourceView::from_source(source, reveal));
        }
        Ok(views)
    }

    /// Records manager approval of a source; requires an editor role.
    ///
    /// # Errors
    /// [`NewsroomError::Forbidden`]; [`NewsroomError::NotFound`]; database failures.
    pub async fn approve_source(
        &self,
        actor: &Actor,
        source_id: Uuid,
        rationale: Option<&str>,
    ) -> Result<Source, NewsroomError> {
        // Source approval is a sector judgement: it is resolved in the desk that
        // owns the story the source belongs to.
        let source_desk = self.desk_for_source(source_id).await?;
        authz::require(self.pool(), actor, "sources.approve", source_desk).await?;
        let mut tx = self.pool().begin().await?;
        let source = sqlx::query_as::<_, Source>(
            "UPDATE meridian.sources SET manager_approved_by_id = $2, approval_rationale = $3, \
               updated_at = now() WHERE id = $1 RETURNING *",
        )
        .bind(source_id)
        .bind(actor.id)
        .bind(rationale)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(NewsroomError::NotFound("Source"))?;
        audit(
            &mut *tx,
            actor.id,
            "source.approved",
            "source",
            &source.id.to_string(),
            None,
            Some(serde_json::json!({ "ground_rule": source.ground_rule })),
        )
        .await?;
        Self::enqueue_event(
            &mut *tx,
            "source.approved",
            &source.id.to_string(),
            None,
            serde_json::json!({ "source_id": source.id }),
            Some(actor.id),
        )
        .await?;
        tx.commit().await?;
        Ok(source)
    }

    /// Grants another user access to a source. The actor must be able to reveal
    /// the source and hold an editor role.
    ///
    /// # Errors
    /// [`NewsroomError::Forbidden`]; [`NewsroomError::NotFound`]; database failures.
    pub async fn grant_source_access(
        &self,
        actor: &Actor,
        source_id: Uuid,
        user_id: Uuid,
    ) -> Result<(), NewsroomError> {
        let source = self.get_source(source_id).await?;
        let story = self.get_story(source.story_id).await?;
        let reveal = self.can_reveal_source(actor, &source, &story).await?;
        // Granting is a per-desk manager action: Admin / EIC / standards-legal answer
        // org-wide, otherwise require the confidential-source capability in the
        // story's own desk rather than a bare workspace role (which would let a
        // Sports editor grant access to a Politics source).
        let can_grant = matches!(
            actor.role,
            Role::Admin | Role::EditorInChief | Role::StandardsLegal
        ) || authz::has(self.pool(), actor, "sources.approve", story.desk_id).await?;
        if !reveal || !can_grant {
            return Err(NewsroomError::Forbidden {
                capability: "sources.grant".to_owned(),
                reason: "Cannot grant access to this source".to_owned(),
            });
        }
        let target_exists: bool =
            sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM meridian.users WHERE id = $1)")
                .bind(user_id)
                .fetch_one(self.pool())
                .await?;
        if !target_exists {
            return Err(NewsroomError::NotFound("User"));
        }
        let mut tx = self.pool().begin().await?;
        sqlx::query(
            "INSERT INTO meridian.source_access_grants (source_id, user_id, granted_by_id) \
             VALUES ($1, $2, $3) ON CONFLICT (source_id, user_id) DO NOTHING",
        )
        .bind(source_id)
        .bind(user_id)
        .bind(actor.id)
        .execute(&mut *tx)
        .await?;
        audit(
            &mut *tx,
            actor.id,
            "source.access.granted",
            "source",
            &source_id.to_string(),
            None,
            Some(serde_json::json!({ "user_id": user_id })),
        )
        .await?;
        Self::enqueue_event(
            &mut *tx,
            "source.access_granted",
            &source_id.to_string(),
            None,
            serde_json::json!({ "source_id": source_id, "user_id": user_id }),
            Some(actor.id),
        )
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Adds an evidence item; the actor must be on the story team. A referenced
    /// source must belong to the same story.
    ///
    /// # Errors
    /// [`NewsroomError::Forbidden`]; [`NewsroomError::Unprocessable`]; database failures.
    pub async fn create_evidence(
        &self,
        actor: &Actor,
        story: &Story,
        draft: &EvidenceDraft,
    ) -> Result<Evidence, NewsroomError> {
        if !self.may_edit_story(actor, story).await? {
            return Err(team_only());
        }
        if let Some(source_id) = draft.source_id {
            let source = self.get_source(source_id).await?;
            if source.story_id != story.id {
                return Err(NewsroomError::Unprocessable(
                    "Source belongs to another story".to_owned(),
                ));
            }
        }
        let mut tx = self.pool().begin().await?;
        // The first custody entry. `chain_of_custody` has existed since migration
        // `0005` with nothing ever writing it, so every evidence item carried an
        // empty provenance log — the one thing a custody column exists to prevent.
        let origin = custody_entry(actor.id, "recorded", draft.notes.as_deref());
        let evidence = sqlx::query_as::<_, Evidence>(
            "INSERT INTO meridian.evidence \
             (id, story_id, source_id, kind, title, uri, notes, checksum, acquired_at, created_by_id, \
              chain_of_custody) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11) RETURNING *",
        )
        .bind(Uuid::now_v7())
        .bind(story.id)
        .bind(draft.source_id)
        .bind(&draft.kind)
        .bind(&draft.title)
        .bind(&draft.uri)
        .bind(&draft.notes)
        .bind(&draft.checksum)
        .bind(draft.acquired_at)
        .bind(actor.id)
        .bind(serde_json::Value::Array(vec![origin]))
        .fetch_one(&mut *tx)
        .await?;
        audit(
            &mut *tx,
            actor.id,
            "evidence.created",
            "evidence",
            &evidence.id.to_string(),
            None,
            Some(serde_json::json!({ "story_id": story.id, "kind": evidence.kind })),
        )
        .await?;
        Self::enqueue_event(
            &mut *tx,
            "evidence.created",
            &evidence.id.to_string(),
            None,
            serde_json::json!({ "evidence_id": evidence.id, "story_id": story.id, "kind": evidence.kind }),
            Some(actor.id),
        )
        .await?;
        tx.commit().await?;
        Ok(evidence)
    }

    /// Moves an evidence item to another verification status, appending the
    /// change to its chain of custody.
    ///
    /// Evidence had no transition path at all: it was created with the default
    /// status and could never move, so `verification_status` was decorative and
    /// the asset gate's notion of "verified" could never be satisfied deliberately.
    ///
    /// The custody log is **appended to, never rewritten** — `||` on the existing
    /// array rather than a replacement — because a provenance record that can be
    /// edited is not a provenance record.
    ///
    /// # Errors
    /// [`NewsroomError::Unprocessable`] for a status outside the registry;
    /// [`NewsroomError::NotFound`] for unknown evidence; [`team_only`] when the
    /// caller may not edit the story; database failures.
    pub async fn set_evidence_status(
        &self,
        actor: &Actor,
        evidence_id: Uuid,
        status: &str,
        note: Option<&str>,
    ) -> Result<Evidence, NewsroomError> {
        if !VERIFICATION_STATUSES.contains(&status) {
            return Err(NewsroomError::Unprocessable(format!(
                "'{status}' is not a verification status"
            )));
        }
        let story_id: Uuid =
            sqlx::query_scalar("SELECT story_id FROM meridian.evidence WHERE id = $1")
                .bind(evidence_id)
                .fetch_optional(self.pool())
                .await?
                .ok_or(NewsroomError::NotFound("Evidence"))?;
        let story = self.get_story(story_id).await?;
        if !self.may_edit_story(actor, &story).await? {
            return Err(team_only());
        }

        let mut tx = self.pool().begin().await?;
        let entry = custody_entry(actor.id, &format!("status:{status}"), note);
        let evidence = sqlx::query_as::<_, Evidence>(
            "UPDATE meridian.evidence \
             SET verification_status = $2, \
                 chain_of_custody = chain_of_custody || $3::jsonb, \
                 updated_at = now() \
             WHERE id = $1 RETURNING *",
        )
        .bind(evidence_id)
        .bind(status)
        .bind(serde_json::Value::Array(vec![entry]))
        .fetch_one(&mut *tx)
        .await?;
        audit(
            &mut *tx,
            actor.id,
            "evidence.status_changed",
            "evidence",
            &evidence.id.to_string(),
            None,
            Some(serde_json::json!({ "status": status })),
        )
        .await?;
        Self::enqueue_event(
            &mut *tx,
            "evidence.status_changed",
            &evidence.id.to_string(),
            None,
            serde_json::json!({ "evidence_id": evidence.id, "status": status }),
            Some(actor.id),
        )
        .await?;
        tx.commit().await?;
        Ok(evidence)
    }

    /// A story's evidence items.
    ///
    /// # Errors
    /// Propagates database failures.
    pub async fn list_evidence(&self, story_id: Uuid) -> Result<Vec<Evidence>, NewsroomError> {
        Ok(sqlx::query_as::<_, Evidence>(
            "SELECT * FROM meridian.evidence WHERE story_id = $1 ORDER BY created_at",
        )
        .bind(story_id)
        .fetch_all(self.pool())
        .await?)
    }

    /// Adds a claim; the actor must be on the story team.
    ///
    /// # Errors
    /// [`NewsroomError::Forbidden`]; database failures.
    pub async fn create_claim(
        &self,
        actor: &Actor,
        story: &Story,
        draft: &ClaimDraft,
    ) -> Result<Claim, NewsroomError> {
        if !self.may_edit_story(actor, story).await? {
            return Err(team_only());
        }
        let mut tx = self.pool().begin().await?;
        let claim = sqlx::query_as::<_, Claim>(
            "INSERT INTO meridian.claims (id, story_id, version_id, text, locator, evidence_ids) \
             VALUES ($1,$2,$3,$4,$5,$6) RETURNING *",
        )
        .bind(Uuid::now_v7())
        .bind(story.id)
        .bind(draft.version_id)
        .bind(&draft.text)
        .bind(&draft.locator)
        .bind(&draft.evidence_ids)
        .fetch_one(&mut *tx)
        .await?;
        audit(
            &mut *tx,
            actor.id,
            "claim.created",
            "claim",
            &claim.id.to_string(),
            None,
            Some(serde_json::json!({ "story_id": story.id })),
        )
        .await?;
        Self::enqueue_event(
            &mut *tx,
            "claim.created",
            &claim.id.to_string(),
            None,
            serde_json::json!({ "claim_id": claim.id, "story_id": story.id }),
            Some(actor.id),
        )
        .await?;
        tx.commit().await?;
        Ok(claim)
    }

    /// A story's claims.
    ///
    /// # Errors
    /// Propagates database failures.
    pub async fn list_claims(&self, story_id: Uuid) -> Result<Vec<Claim>, NewsroomError> {
        Ok(sqlx::query_as::<_, Claim>(
            "SELECT * FROM meridian.claims WHERE story_id = $1 ORDER BY created_at",
        )
        .bind(story_id)
        .fetch_all(self.pool())
        .await?)
    }

    /// Records a verification decision on a claim; requires a verifier role.
    ///
    /// # Errors
    /// [`NewsroomError::Forbidden`]; [`NewsroomError::Unprocessable`] for a bad
    /// status; [`NewsroomError::NotFound`]; database failures.
    pub async fn decide_claim(
        &self,
        actor: &Actor,
        claim_id: Uuid,
        status: &str,
        note: Option<&str>,
    ) -> Result<Claim, NewsroomError> {
        let claim_desk = self.desk_for_claim(claim_id).await?;
        authz::require(self.pool(), actor, "claims.decide", claim_desk).await?;
        if !VERIFICATION_STATUSES.contains(&status) {
            return Err(NewsroomError::Unprocessable(format!(
                "'{status}' is not a verification status"
            )));
        }
        let mut tx = self.pool().begin().await?;
        let claim = sqlx::query_as::<_, Claim>(
            "UPDATE meridian.claims SET status = $2, decision_note = $3, reviewed_by_id = $4, \
               reviewed_at = now(), updated_at = now() WHERE id = $1 RETURNING *",
        )
        .bind(claim_id)
        .bind(status)
        .bind(note)
        .bind(actor.id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(NewsroomError::NotFound("Claim"))?;
        audit(
            &mut *tx,
            actor.id,
            "claim.decided",
            "claim",
            &claim.id.to_string(),
            None,
            Some(serde_json::json!({ "status": claim.status })),
        )
        .await?;
        let claim_event = if claim.status == "rejected" || claim.status == "disputed" {
            "claim.rejected"
        } else {
            "claim.approved"
        };
        Self::enqueue_event(
            &mut *tx,
            claim_event,
            &claim.id.to_string(),
            None,
            serde_json::json!({ "claim_id": claim.id, "status": claim.status }),
            Some(actor.id),
        )
        .await?;
        tx.commit().await?;
        Ok(claim)
    }
}

impl NewsroomService {
    /// The sector owning the story a source belongs to.
    async fn desk_for_source(&self, source_id: Uuid) -> Result<Option<Uuid>, NewsroomError> {
        Ok(sqlx::query_scalar(
            "SELECT s.desk_id FROM meridian.stories s \
             JOIN meridian.sources src ON src.story_id = s.id WHERE src.id = $1",
        )
        .bind(source_id)
        .fetch_optional(self.pool())
        .await?
        .flatten())
    }

    /// The sector owning the story a claim belongs to.
    async fn desk_for_claim(&self, claim_id: Uuid) -> Result<Option<Uuid>, NewsroomError> {
        Ok(sqlx::query_scalar(
            "SELECT s.desk_id FROM meridian.stories s \
             JOIN meridian.claims c ON c.story_id = s.id WHERE c.id = $1",
        )
        .bind(claim_id)
        .fetch_optional(self.pool())
        .await?
        .flatten())
    }
}

fn team_only() -> NewsroomError {
    NewsroomError::Forbidden {
        capability: "stories.team".to_owned(),
        reason: "You are not on this story team".to_owned(),
    }
}

