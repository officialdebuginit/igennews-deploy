//! Module 4 — story core: stories, immutable versions, and the workflow state
//! machine. Ported from the legacy `services.py` (`TRANSITIONS`, `create_version`,
//! `transition_story`, `validate_ready`) and the `api.py` story handlers.
//!
//! Immutability and the dwell history are enforced in the schema
//! (`uq_story_version`, one open `workflow_state_history` row per story); this
//! layer upholds the transition graph, the editor gate, and the audit trail.
//!
//! The READY gate is now fully evaluated: saved version, headline/dek/body,
//! unresolved claims, pending reviews and required approvals (Module 5),
//! anonymous-source approval, and asset rights/alt-text/verification (Module 9).
//! `save_version` invalidates the story's live approvals, as in legacy.

use serde::Serialize;
use sqlx::FromRow;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::{Actor, NewsroomError, NewsroomService, audit, authz};

/// The editorial workflow states, in the legacy `WorkflowState` order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum WorkflowState {
    Intake,
    Proposed,
    Assigned,
    Reporting,
    Drafting,
    DeskReview,
    Verification,
    CopyStandards,
    Ready,
    Parked,
    Archived,
}

impl WorkflowState {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            WorkflowState::Intake => "intake",
            WorkflowState::Proposed => "proposed",
            WorkflowState::Assigned => "assigned",
            WorkflowState::Reporting => "reporting",
            WorkflowState::Drafting => "drafting",
            WorkflowState::DeskReview => "desk_review",
            WorkflowState::Verification => "verification",
            WorkflowState::CopyStandards => "copy_standards",
            WorkflowState::Ready => "ready",
            WorkflowState::Parked => "parked",
            WorkflowState::Archived => "archived",
        }
    }

    #[must_use]
    pub fn from_wire(value: &str) -> Option<WorkflowState> {
        use WorkflowState::{
            Archived, Assigned, CopyStandards, DeskReview, Drafting, Intake, Parked, Proposed,
            Ready, Reporting, Verification,
        };
        Some(match value {
            "intake" => Intake,
            "proposed" => Proposed,
            "assigned" => Assigned,
            "reporting" => Reporting,
            "drafting" => Drafting,
            "desk_review" => DeskReview,
            "verification" => Verification,
            "copy_standards" => CopyStandards,
            "ready" => Ready,
            "parked" => Parked,
            "archived" => Archived,
            _ => return None,
        })
    }

    /// The states reachable from `self`, transcribed verbatim from the legacy
    /// `TRANSITIONS` table.
    #[must_use]
    pub fn legal_targets(self) -> &'static [WorkflowState] {
        use WorkflowState::{
            Archived, Assigned, CopyStandards, DeskReview, Drafting, Intake, Parked, Proposed,
            Ready, Reporting, Verification,
        };
        match self {
            Intake => &[Proposed],
            Proposed => &[Assigned, Parked],
            Assigned => &[Reporting, Parked],
            Reporting => &[Drafting, Parked],
            Drafting => &[DeskReview, Parked],
            DeskReview => &[Drafting, Verification],
            Verification => &[Drafting, CopyStandards],
            CopyStandards => &[Drafting, Ready],
            Ready => &[CopyStandards, Archived],
            Parked => &[Proposed, Assigned, Archived],
            Archived => &[],
        }
    }

    #[must_use]
    pub fn can_transition_to(self, target: WorkflowState) -> bool {
        self.legal_targets().contains(&target)
    }

    /// Transitions that require an editor role, per legacy `transition_story`.
    #[must_use]
    fn requires_editor(self) -> bool {
        matches!(
            self,
            WorkflowState::Verification | WorkflowState::CopyStandards | WorkflowState::Ready
        )
    }
}

#[derive(Clone, Debug, FromRow, Serialize)]
pub struct Story {
    pub id: Uuid,
    pub slug: String,
    pub title: String,
    pub dek: String,
    pub body: serde_json::Value,
    pub category: String,
    pub tags: serde_json::Value,
    pub story_type: String,
    pub workflow_state: String,
    pub publication_state: String,
    pub validity_state: String,
    pub priority: String,
    pub desk_id: Option<Uuid>,
    /// The industry (sub-sector) this story is filed under, within its sector
    /// (`desk_id`). Optional, and always belongs to the story's own sector.
    pub sub_sector_id: Option<Uuid>,
    pub event_id: Option<Uuid>,
    pub pitch_id: Option<Uuid>,
    pub author_id: Uuid,
    pub editor_id: Option<Uuid>,
    pub fact_checker_id: Option<Uuid>,
    pub copy_editor_id: Option<Uuid>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub due_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub scheduled_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub published_at: Option<OffsetDateTime>,
    pub version_number: i32,
    pub fast_path: bool,
    pub fast_path_reason: Option<String>,
    /// Per-story rights overrides (RightsML/ODRL). Each is `None` when the story
    /// carries no override and the outlet default applies at serialization time.
    pub rights_holder: Option<String>,
    pub rights_notice: Option<String>,
    pub usage_terms: Option<String>,
    pub license_url: Option<String>,
    /// Structured reuse constraints (ODRL). `reuse_embargo_until` is `None` for no
    /// embargo; `territory_mode` is `any`/`allow`/`deny`; `territories` is a
    /// comma-separated ISO alpha-2 list read according to the mode.
    #[serde(with = "time::serde::rfc3339::option")]
    pub reuse_embargo_until: Option<OffsetDateTime>,
    pub territory_mode: String,
    pub territories: Option<String>,
    /// IPTC Digital Source Type code (AI-disclosure); `None` = unspecified.
    pub digital_source_type: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
    /// The author's display name, joined in only by `get_story` (single-story
    /// reads). List queries don't select it, so `sqlx(default)` leaves it `None`
    /// there rather than forcing every `SELECT` to carry the join.
    #[sqlx(default)]
    #[serde(default)]
    pub author_name: Option<String>,
}

impl Story {
    /// Whether the actor is on the story's team.
    ///
    /// The other half of the legacy `can_edit_story` — the "or an editor role"
    /// escape — is now the sector-scoped `stories.edit_any` capability, resolved
    /// by [`NewsroomService::may_edit_story`]. Splitting them is what stops an
    /// editor in one sector editing stories in another.
    #[must_use]
    pub(crate) fn is_team_member(&self, actor: &Actor) -> bool {
        [
            Some(self.author_id),
            self.editor_id,
            self.fact_checker_id,
            self.copy_editor_id,
        ]
        .into_iter()
        .flatten()
        .any(|member| member == actor.id)
    }

    #[must_use]
    fn state(&self) -> Option<WorkflowState> {
        WorkflowState::from_wire(&self.workflow_state)
    }
}

#[derive(Clone, Debug, FromRow, Serialize)]
pub struct StoryVersion {
    pub id: Uuid,
    pub story_id: Uuid,
    pub number: i32,
    pub title: String,
    pub dek: String,
    pub body: serde_json::Value,
    pub category: String,
    pub tags: serde_json::Value,
    pub change_summary: Option<String>,
    pub created_by_id: Uuid,
    pub filed: bool,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

/// The result of a publish-readiness check.
#[derive(Debug, Serialize)]
pub struct Readiness {
    pub ready: bool,
    pub blockers: Vec<String>,
    pub fast_path: bool,
}

/// New-story input; `slug` is caller-supplied as in the legacy contract.
#[derive(Clone, Debug)]
pub struct StoryDraft {
    pub slug: String,
    pub title: String,
    pub dek: String,
    pub body: serde_json::Value,
    pub category: String,
    pub tags: serde_json::Value,
    pub story_type: String,
    pub priority: String,
    pub desk_id: Option<Uuid>,
    /// The industry (sub-sector) within `desk_id`. Validated to belong to the sector.
    pub sub_sector_id: Option<Uuid>,
    pub event_id: Option<Uuid>,
    pub author_id: Option<Uuid>,
    pub editor_id: Option<Uuid>,
    pub fact_checker_id: Option<Uuid>,
    pub copy_editor_id: Option<Uuid>,
}

/// The columns the asset publish gate reads.
#[derive(FromRow)]
struct AssetGateRow {
    id: Uuid,
    kind: String,
    rights: Option<String>,
    alt_text: Option<String>,
    verification_status: String,
    /// When the licence lapses. The column has existed since migration `0009`
    /// and **nothing has ever read it** — so the gate accepted a rights record
    /// that had already expired, which is the one case a rights *expiry* column
    /// exists to catch.
    rights_expires_at: Option<OffsetDateTime>,
}

/// A partial story update. A `None` field is left unchanged (legacy
/// `exclude_unset`). Clearing a nullable field back to null is not expressible
/// through this port and is deferred to the differential-parity pass.
#[derive(Clone, Debug, Default)]
pub struct StoryPatch {
    pub title: Option<String>,
    pub dek: Option<String>,
    pub body: Option<serde_json::Value>,
    pub category: Option<String>,
    pub tags: Option<serde_json::Value>,
    pub priority: Option<String>,
    pub desk_id: Option<Uuid>,
    /// The industry (sub-sector). `Some` sets it (validated against the story's
    /// sector); `None` leaves it unchanged, consistent with the other fields here.
    pub sub_sector_id: Option<Uuid>,
    pub author_id: Option<Uuid>,
    pub editor_id: Option<Uuid>,
    pub fact_checker_id: Option<Uuid>,
    pub copy_editor_id: Option<Uuid>,
    pub due_at: Option<OffsetDateTime>,
    /// Per-story rights overrides. Like every other field here a `None` leaves the
    /// column unchanged; an empty string clears the override back to the outlet
    /// default (handled by [`Self::rights_bind`] mapping `""` to SQL `NULL`).
    pub rights_holder: Option<String>,
    pub rights_notice: Option<String>,
    pub usage_terms: Option<String>,
    pub license_url: Option<String>,
    /// Reuse constraints. `reuse_embargo_until` is an RFC 3339 string (or `""` to
    /// clear the embargo); `territory_mode` is `any`/`allow`/`deny`; `territories`
    /// is the country-code list (`""` clears it). Same unchanged/clear semantics as
    /// the rights fields above.
    pub reuse_embargo_until: Option<String>,
    pub territory_mode: Option<String>,
    pub territories: Option<String>,
    pub digital_source_type: Option<String>,
}

impl StoryPatch {
    /// Maps a rights-field patch value to what the `UPDATE` should bind: `None`
    /// leaves the column unchanged (COALESCE keeps the old value), `Some("")`
    /// deliberately clears the override to `NULL`, and `Some(text)` sets it.
    ///
    /// Unlike the other patch fields, rights fields must be *clearable* — a desk
    /// that set a bespoke licence needs to be able to take it back off — so these
    /// four columns use `NULLIF($, '<UNCHANGED>')` rather than a plain COALESCE.
    fn rights_bind(value: &Option<String>) -> Option<String> {
        match value {
            None => Some(RIGHTS_UNCHANGED.to_owned()),
            Some(text) if text.trim().is_empty() => None,
            Some(text) => Some(text.clone()),
        }
    }
}

/// Sentinel bound for a rights field the patch does not touch. The `UPDATE`
/// keeps the existing value when a column equals this, so a real value can never
/// collide with it in practice.
const RIGHTS_UNCHANGED: &str = "\u{1e}__meridian_rights_unchanged__\u{1e}";

/// Optional filters for the story list.
#[derive(Clone, Debug, Default)]
pub struct StoryFilter {
    pub workflow: Option<String>,
    pub publication: Option<String>,
    pub desk_id: Option<Uuid>,
    pub author_id: Option<Uuid>,
}

const STORY_COLUMNS: &str = "id, slug, title, dek, body, category, tags, story_type, \
    workflow_state, publication_state, validity_state, priority, desk_id, sub_sector_id, \
    event_id, pitch_id, \
    author_id, editor_id, fact_checker_id, copy_editor_id, due_at, scheduled_at, published_at, \
    version_number, fast_path, fast_path_reason, rights_holder, rights_notice, usage_terms, \
    license_url, reuse_embargo_until, territory_mode, territories, digital_source_type, \
    created_at, updated_at";

impl NewsroomService {
    /// Loads a story or returns [`NewsroomError::NotFound`].
    ///
    /// # Errors
    /// Propagates database failures.
    pub async fn get_story(&self, story_id: Uuid) -> Result<Story, NewsroomError> {
        sqlx::query_as::<_, Story>(
            "SELECT s.*, u.display_name AS author_name \
             FROM meridian.stories s \
             LEFT JOIN meridian.users u ON u.id = s.author_id \
             WHERE s.id = $1",
        )
        .bind(story_id)
        .fetch_optional(self.pool())
        .await?
        .ok_or(NewsroomError::NotFound("Story"))
    }

    /// Lists stories newest-first, applying whichever filters are set.
    ///
    /// # Errors
    /// Propagates database failures.
    pub async fn list_stories(
        &self,
        actor: &Actor,
        filter: &StoryFilter,
    ) -> Result<Vec<Story>, NewsroomError> {
        // Sector visibility, mirroring `can_view_desk`: holders of
        // `dashboard.view_global` see every desk; everyone else sees the desks
        // they belong to. Two deliberate escapes keep the list honest — a story
        // with no desk is org-level and visible to all, and a story you are on
        // the team of is always visible even if you hold no membership in its
        // sector (an outside contributor assigned to one piece).
        let sees_all = authz::has(self.pool(), actor, "dashboard.view_global", None).await?;
        Ok(sqlx::query_as::<_, Story>(
            "SELECT * FROM meridian.stories \
             WHERE ($1::text IS NULL OR workflow_state = $1) \
               AND ($2::text IS NULL OR publication_state = $2) \
               AND ($3::uuid IS NULL OR desk_id = $3) \
               AND ($4::uuid IS NULL OR author_id = $4) \
               AND ($5::boolean \
                    OR desk_id IS NULL \
                    OR desk_id IN (SELECT desk_id FROM meridian.desk_memberships \
                                    WHERE user_id = $6) \
                    OR author_id = $6 OR editor_id = $6 \
                    OR fact_checker_id = $6 OR copy_editor_id = $6) \
             ORDER BY updated_at DESC",
        )
        .bind(&filter.workflow)
        .bind(&filter.publication)
        .bind(filter.desk_id)
        .bind(filter.author_id)
        .bind(sees_all)
        .bind(actor.id)
        .fetch_all(self.pool())
        .await?)
    }

    /// Whether `actor` may edit `story`: on its team, or holding
    /// `stories.edit_any` **in the story's own sector**.
    ///
    /// This replaces the legacy `can_edit_story`, whose "or an editor role" arm
    /// was a global role test — a section editor of any desk could edit every
    /// story in the newsroom. Resolving the override against `story.desk_id` is
    /// what confines it to the sector the editor actually leads.
    ///
    /// # Errors
    /// Propagates database failures from capability resolution.
    pub(crate) async fn may_edit_story(
        &self,
        actor: &Actor,
        story: &Story,
    ) -> Result<bool, NewsroomError> {
        if story.is_team_member(actor) {
            return Ok(true);
        }
        authz::has(self.pool(), actor, "stories.edit_any", story.desk_id).await
    }

    /// Whether `actor` may **read** `story`, under the desk-private model — the same
    /// visibility [`Self::list_stories`] enforces: an org-level story (no desk), a
    /// story the viewer is on the team of, a story in a desk they belong to, or any
    /// story when they hold `dashboard.view_global`. Anyone assigned to review it is
    /// also admitted, so the review console keeps working for a reviewer who is not a
    /// desk member.
    ///
    /// # Errors
    /// Propagates capability-resolution / database failures.
    pub(crate) async fn may_view_story(
        &self,
        actor: &Actor,
        story: &Story,
    ) -> Result<bool, NewsroomError> {
        if story.desk_id.is_none() || story.is_team_member(actor) {
            return Ok(true);
        }
        if authz::has(self.pool(), actor, "dashboard.view_global", None).await? {
            return Ok(true);
        }
        let allowed: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM meridian.desk_memberships \
                            WHERE desk_id = $1 AND user_id = $2) \
                 OR EXISTS (SELECT 1 FROM meridian.reviews \
                            WHERE story_id = $3 AND assigned_to_id = $2)",
        )
        .bind(story.desk_id)
        .bind(actor.id)
        .bind(story.id)
        .fetch_one(self.pool())
        .await?;
        Ok(allowed)
    }

    /// Fetch a story only if `actor` may read it — the single gate the read endpoints
    /// call so a story's working data (versions, evidence, claims, approvals,
    /// comments, releases) is desk-private like the story record itself. Returns the
    /// story so a caller that also needs it avoids a second fetch.
    ///
    /// # Errors
    /// [`NewsroomError::Forbidden`] when the viewer may not see the story;
    /// [`NewsroomError::NotFound`] for an unknown story; database failures.
    pub async fn require_story_read(
        &self,
        actor: &Actor,
        story_id: Uuid,
    ) -> Result<Story, NewsroomError> {
        let story = self.get_story(story_id).await?;
        if !self.may_view_story(actor, &story).await? {
            return Err(NewsroomError::Forbidden {
                capability: "stories.view".to_owned(),
                reason: "You cannot view this story".to_owned(),
            });
        }
        Ok(story)
    }

    /// The sector a story belongs to, for scoping a capability check when only
    /// the story id is in hand. A missing story yields `None`, which resolves as
    /// an unscoped check and is then denied by the caller's own not-found path.
    ///
    /// # Errors
    /// Propagates database failures.
    pub(crate) async fn desk_for_story(
        &self,
        story_id: Uuid,
    ) -> Result<Option<Uuid>, NewsroomError> {
        Ok(
            sqlx::query_scalar("SELECT desk_id FROM meridian.stories WHERE id = $1")
                .bind(story_id)
                .fetch_optional(self.pool())
                .await?
                .flatten(),
        )
    }

    /// Creates a story; requires the `stories.create` capability.
    ///
    /// # Errors
    /// [`NewsroomError::Forbidden`]; [`NewsroomError::Conflict`] on a duplicate
    /// slug; database failures.
    /// Whether a story already exists with this slug. Used by feed ingest to skip
    /// items already imported — the slug is derived from the headline, so a repeated
    /// wire item resolves to the same slug and is not duplicated.
    ///
    /// # Errors
    /// Propagates database failures.
    pub async fn slug_exists(&self, slug: &str) -> Result<bool, NewsroomError> {
        let exists: bool =
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM meridian.stories WHERE slug = $1)")
                .bind(slug)
                .fetch_one(self.pool())
                .await?;
        Ok(exists)
    }

    pub async fn create_story(
        &self,
        actor: &Actor,
        draft: &StoryDraft,
    ) -> Result<Story, NewsroomError> {
        authz::require(self.pool(), actor, "stories.create", draft.desk_id).await?;
        // The industry (if any) must belong to the story's sector.
        let sub_sector_id = self
            .validate_sub_sector_for_desk(draft.sub_sector_id, draft.desk_id)
            .await?;
        let author_id = draft.author_id.unwrap_or(actor.id);
        let mut tx = self.pool().begin().await?;
        let story = sqlx::query_as::<_, Story>(&format!(
            "INSERT INTO meridian.stories \
             (id, slug, title, dek, body, category, tags, story_type, priority, desk_id, \
              sub_sector_id, event_id, \
              author_id, editor_id, fact_checker_id, copy_editor_id) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16) RETURNING {STORY_COLUMNS}"
        ))
        .bind(Uuid::now_v7())
        .bind(&draft.slug)
        .bind(&draft.title)
        .bind(&draft.dek)
        .bind(&draft.body)
        .bind(&draft.category)
        .bind(&draft.tags)
        .bind(&draft.story_type)
        .bind(&draft.priority)
        .bind(draft.desk_id)
        .bind(sub_sector_id)
        .bind(draft.event_id)
        .bind(author_id)
        .bind(draft.editor_id)
        .bind(draft.fact_checker_id)
        .bind(draft.copy_editor_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(duplicate_slug)?;
        audit(
            &mut *tx,
            actor.id,
            "story.created",
            "story",
            &story.id.to_string(),
            None,
            Some(serde_json::json!({ "title": story.title })),
        )
        .await?;
        Self::enqueue_event(
            &mut *tx,
            "article.created",
            &story.id.to_string(),
            None,
            serde_json::json!({ "story_id": story.id, "slug": story.slug, "title": story.title }),
            Some(actor.id),
        )
        .await?;
        tx.commit().await?;
        Ok(story)
    }

    /// Applies a partial update; requires `stories.edit` and story-team access.
    ///
    /// # Errors
    /// [`NewsroomError::Forbidden`]; [`NewsroomError::NotFound`]; database failures.
    pub async fn update_story(
        &self,
        actor: &Actor,
        story_id: Uuid,
        patch: &StoryPatch,
    ) -> Result<Story, NewsroomError> {
        let story = self.get_story(story_id).await?;
        authz::require(self.pool(), actor, "stories.edit", story.desk_id).await?;
        if !self.may_edit_story(actor, &story).await? {
            return Err(team_only());
        }
        // If the industry is being set, it must belong to the story's (possibly
        // updated) sector — resolve against the desk the story will end up on.
        let effective_desk = patch.desk_id.or(story.desk_id);
        let sub_sector_id = self
            .validate_sub_sector_for_desk(patch.sub_sector_id, effective_desk)
            .await?;
        let mut tx = self.pool().begin().await?;
        let updated = sqlx::query_as::<_, Story>(&format!(
            "UPDATE meridian.stories SET \
               title = COALESCE($2, title), \
               dek = COALESCE($3, dek), \
               body = COALESCE($4, body), \
               category = COALESCE($5, category), \
               tags = COALESCE($6, tags), \
               priority = COALESCE($7, priority), \
               desk_id = COALESCE($8, desk_id), \
               author_id = COALESCE($9, author_id), \
               editor_id = COALESCE($10, editor_id), \
               fact_checker_id = COALESCE($11, fact_checker_id), \
               copy_editor_id = COALESCE($12, copy_editor_id), \
               due_at = COALESCE($13, due_at), \
               rights_holder = CASE WHEN $14 = $18 THEN rights_holder ELSE $14 END, \
               rights_notice = CASE WHEN $15 = $18 THEN rights_notice ELSE $15 END, \
               usage_terms   = CASE WHEN $16 = $18 THEN usage_terms   ELSE $16 END, \
               license_url   = CASE WHEN $17 = $18 THEN license_url   ELSE $17 END, \
               reuse_embargo_until = CASE WHEN $19 = $18 THEN reuse_embargo_until ELSE NULLIF($19, $18)::timestamptz END, \
               territory_mode = COALESCE($20, territory_mode), \
               territories   = CASE WHEN $21 = $18 THEN territories ELSE $21 END, \
               digital_source_type = COALESCE($22, digital_source_type), \
               sub_sector_id = COALESCE($23, sub_sector_id), \
               updated_at = now() \
             WHERE id = $1 RETURNING {STORY_COLUMNS}"
        ))
        .bind(story_id)
        .bind(&patch.title)
        .bind(&patch.dek)
        .bind(&patch.body)
        .bind(&patch.category)
        .bind(&patch.tags)
        .bind(&patch.priority)
        .bind(patch.desk_id)
        .bind(patch.author_id)
        .bind(patch.editor_id)
        .bind(patch.fact_checker_id)
        .bind(patch.copy_editor_id)
        .bind(patch.due_at)
        .bind(StoryPatch::rights_bind(&patch.rights_holder))
        .bind(StoryPatch::rights_bind(&patch.rights_notice))
        .bind(StoryPatch::rights_bind(&patch.usage_terms))
        .bind(StoryPatch::rights_bind(&patch.license_url))
        .bind(RIGHTS_UNCHANGED)
        .bind(StoryPatch::rights_bind(&patch.reuse_embargo_until))
        .bind(patch.territory_mode.as_deref().filter(|m| !m.trim().is_empty()))
        .bind(StoryPatch::rights_bind(&patch.territories))
        .bind(patch.digital_source_type.as_deref().filter(|d| !d.trim().is_empty()))
        .bind(sub_sector_id)
        .fetch_one(&mut *tx)
        .await?;
        audit(
            &mut *tx,
            actor.id,
            "story.updated",
            "story",
            &story_id.to_string(),
            None,
            None,
        )
        .await?;
        Self::enqueue_event(
            &mut *tx,
            "article.updated",
            &story_id.to_string(),
            None,
            serde_json::json!({ "story_id": story_id, "slug": updated.slug, "title": updated.title }),
            Some(actor.id),
        )
        .await?;
        tx.commit().await?;
        Ok(updated)
    }

    /// Deletes a story; requires `stories.delete`, and `stories.delete_any`
    /// (desk-scoped) to delete someone else's.
    ///
    /// # Errors
    /// [`NewsroomError::Forbidden`]; [`NewsroomError::NotFound`]; database failures.
    pub async fn delete_story(&self, actor: &Actor, story_id: Uuid) -> Result<(), NewsroomError> {
        let story = self.get_story(story_id).await?;
        authz::require(self.pool(), actor, "stories.delete", story.desk_id).await?;
        if story.author_id != actor.id
            && !authz::has(self.pool(), actor, "stories.delete_any", story.desk_id).await?
        {
            return Err(NewsroomError::Forbidden {
                capability: "stories.delete_any".to_owned(),
                reason: "Not authorized to delete someone else's story".to_owned(),
            });
        }
        let mut tx = self.pool().begin().await?;
        sqlx::query("DELETE FROM meridian.stories WHERE id = $1")
            .bind(story_id)
            .execute(&mut *tx)
            .await?;
        audit(
            &mut *tx,
            actor.id,
            "story.deleted",
            "story",
            &story_id.to_string(),
            Some(serde_json::json!({ "title": story.title })),
            None,
        )
        .await?;
        Self::enqueue_event(
            &mut *tx,
            "article.deleted",
            &story_id.to_string(),
            None,
            serde_json::json!({ "story_id": story_id, "title": story.title }),
            Some(actor.id),
        )
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// The story's versions, newest first.
    ///
    /// # Errors
    /// Propagates database failures.
    pub async fn list_versions(&self, story_id: Uuid) -> Result<Vec<StoryVersion>, NewsroomError> {
        Ok(sqlx::query_as::<_, StoryVersion>(
            "SELECT * FROM meridian.story_versions WHERE story_id = $1 ORDER BY number DESC",
        )
        .bind(story_id)
        .fetch_all(self.pool())
        .await?)
    }

    /// Saves an immutable version snapshot of the story's current content.
    /// Requires the actor to be on the story team.
    ///
    /// Legacy also invalidates the story's live approvals here; approvals arrive
    /// with Module 5, so that step is intentionally not performed yet.
    ///
    /// # Errors
    /// [`NewsroomError::Forbidden`]; database failures.
    pub async fn save_version(
        &self,
        actor: &Actor,
        story: &Story,
        change_summary: Option<&str>,
        filed: bool,
    ) -> Result<StoryVersion, NewsroomError> {
        if !self.may_edit_story(actor, story).await? {
            return Err(team_only());
        }
        let mut tx = self.pool().begin().await?;
        let next_number: i32 = sqlx::query_scalar(
            // Filing a new version also clears any fast-path bypass: exactly like the
            // live approvals voided below, a bypass earned against the previous text
            // must not carry over to content that has now changed.
            "UPDATE meridian.stories SET version_number = version_number + 1, \
               fast_path = false, fast_path_reason = NULL, updated_at = now() \
             WHERE id = $1 RETURNING version_number",
        )
        .bind(story.id)
        .fetch_one(&mut *tx)
        .await?;
        let version = sqlx::query_as::<_, StoryVersion>(
            "INSERT INTO meridian.story_versions \
             (id, story_id, number, title, dek, body, category, tags, change_summary, created_by_id, filed) \
             SELECT $1, s.id, $2, s.title, s.dek, s.body, s.category, s.tags, $3, $4, $5 \
             FROM meridian.stories s WHERE s.id = $6 RETURNING *",
        )
        .bind(Uuid::now_v7())
        .bind(next_number)
        .bind(change_summary)
        .bind(actor.id)
        .bind(filed)
        .bind(story.id)
        .fetch_one(&mut *tx)
        .await?;
        // Filing a new version invalidates the story's live approvals: they were
        // granted against content that has now changed.
        let invalidated = sqlx::query_scalar::<_, i64>(
            "WITH voided AS ( \
               UPDATE meridian.approvals SET is_valid = false, invalidated_at = now() \
               WHERE story_id = $1 AND is_valid RETURNING 1 ) \
             SELECT count(*) FROM voided",
        )
        .bind(story.id)
        .fetch_one(&mut *tx)
        .await?;
        audit(
            &mut *tx,
            actor.id,
            "story.version.created",
            "story_version",
            &version.id.to_string(),
            None,
            Some(serde_json::json!({
                "story_id": story.id,
                "number": version.number,
                "filed": filed,
                "invalidated_approvals": invalidated,
            })),
        )
        .await?;
        tx.commit().await?;
        Ok(version)
    }

    /// Whether a story is publishable. Portable blockers are evaluated; the
    /// review/approval/asset gates (Modules 5 & 9) are recorded as an outstanding
    /// blocker so readiness is never reported true before those gates exist.
    ///
    /// # Errors
    /// Propagates database failures.
    pub async fn publish_readiness(&self, story: &Story) -> Result<Readiness, NewsroomError> {
        let blockers = self.readiness_blockers(story, story.fast_path).await?;
        Ok(Readiness {
            ready: blockers.is_empty(),
            blockers,
            fast_path: story.fast_path,
        })
    }

    pub(crate) async fn readiness_blockers(
        &self,
        story: &Story,
        fast_path: bool,
    ) -> Result<Vec<String>, NewsroomError> {
        let mut blockers = Vec::new();
        let latest_version: Option<Uuid> = sqlx::query_scalar(
            "SELECT id FROM meridian.story_versions WHERE story_id = $1 ORDER BY number DESC LIMIT 1",
        )
        .bind(story.id)
        .fetch_optional(self.pool())
        .await?;
        if latest_version.is_none() {
            blockers.push("The story has no saved version".to_owned());
        }
        if story.title.trim().is_empty()
            || story.dek.trim().is_empty()
            || story.body.as_array().is_none_or(Vec::is_empty)
        {
            blockers.push("Headline, dek, and body are required".to_owned());
        }
        let unresolved: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM meridian.claims \
             WHERE story_id = $1 AND status NOT IN ('verified', 'rejected')",
        )
        .bind(story.id)
        .fetch_one(self.pool())
        .await?;
        if unresolved > 0 {
            blockers.push(format!("{unresolved} claims remain unresolved"));
        }
        // Reviews and approvals are recorded against a version; with none there is
        // nothing for them to point at (and the missing version already blocks).
        if let Some(version_id) = latest_version {
            let pending_reviews: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM meridian.reviews \
                 WHERE story_id = $1 AND version_id = $2 AND decision = 'pending'",
            )
            .bind(story.id)
            .bind(version_id)
            .fetch_one(self.pool())
            .await?;
            if pending_reviews > 0 {
                blockers.push(format!("{pending_reviews} reviews are pending"));
            }
            let approved_kinds: Vec<String> = sqlx::query_scalar(
                "SELECT kind FROM meridian.approvals \
                 WHERE story_id = $1 AND version_id = $2 AND is_valid \
                   AND decision IN ('approved', 'waived')",
            )
            .bind(story.id)
            .bind(version_id)
            .fetch_all(self.pool())
            .await?;
            let mut missing: Vec<&str> = ["copy", "desk", "fact_check"]
                .into_iter()
                .filter(|required| !approved_kinds.iter().any(|kind| kind == required))
                .collect();
            missing.sort_unstable();
            if !missing.is_empty() && !fast_path {
                blockers.push(format!("Missing approvals: {}", missing.join(", ")));
            }
        }
        let anonymous_unapproved: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM meridian.sources \
             WHERE story_id = $1 AND ground_rule <> 'on_record' AND manager_approved_by_id IS NULL",
        )
        .bind(story.id)
        .fetch_one(self.pool())
        .await?;
        if anonymous_unapproved > 0 {
            blockers.push("Anonymous/background sources require manager approval".to_owned());
        }
        // The fast path waives the approval and asset blockers — and only those.
        // Everything above (content, claims, pending reviews, confidential-source
        // approval) has already been checked and still stands; stop before assets.
        if fast_path {
            return Ok(blockers);
        }
        // Asset gate (Module 9): each asset needs a rights record, image/graphic
        // assets need alt text, and every asset must be verified.
        let assets = sqlx::query_as::<_, AssetGateRow>(
            "SELECT id, kind, rights, alt_text, verification_status, rights_expires_at \
             FROM meridian.assets WHERE story_id = $1",
        )
        .bind(story.id)
        .fetch_all(self.pool())
        .await?;
        let now = OffsetDateTime::now_utc();
        for asset in assets {
            if asset.rights.is_none_or(|value| value.trim().is_empty()) {
                blockers.push(format!("Asset {} has no rights record", asset.id));
            }
            // A lapsed licence is not a lesser problem than a missing one — it is
            // the same exposure with a paper trail saying it was known about.
            if asset.rights_expires_at.is_some_and(|at| at <= now) {
                blockers.push(format!("Asset {} has expired usage rights", asset.id));
            }
            if matches!(asset.kind.as_str(), "image" | "graphic")
                && asset.alt_text.is_none_or(|value| value.trim().is_empty())
            {
                blockers.push(format!("Asset {} has no alt text", asset.id));
            }
            if asset.verification_status != "verified" {
                blockers.push(format!("Asset {} is not verified", asset.id));
            }
        }
        Ok(blockers)
    }

    /// Applies a workflow transition, upholding the legal graph, the editor gate,
    /// the fail-closed READY gate, the dwell history, and the audit trail.
    ///
    /// # Errors
    /// [`NewsroomError::Forbidden`] off the team or without an editor role;
    /// [`NewsroomError::Conflict`] for an illegal transition or an unmet READY
    /// gate; [`NewsroomError::Unprocessable`] for a fast path without a reason;
    /// database failures.
    pub async fn transition_story(
        &self,
        actor: &Actor,
        story: &Story,
        target: WorkflowState,
        reason: Option<&str>,
        fast_path: bool,
    ) -> Result<Story, NewsroomError> {
        if !self.may_edit_story(actor, story).await? {
            return Err(team_only());
        }
        let current = story
            .state()
            .ok_or_else(|| NewsroomError::Conflict("Story is in an unknown state".to_owned()))?;
        if !current.can_transition_to(target) {
            return Err(NewsroomError::Conflict(format!(
                "Cannot transition {} to {}",
                current.as_str(),
                target.as_str()
            )));
        }
        if target.requires_editor() {
            // Resolved in the story's own sector: advancing a Politics story
            // needs the capability in Politics, not merely an editor title
            // somewhere in the newsroom.
            authz::require(self.pool(), actor, "workflow.advance", story.desk_id).await?;
        }
        let mut set_fast_path = false;
        if target == WorkflowState::Ready {
            if fast_path {
                authz::require(self.pool(), actor, "workflow.fast_path", story.desk_id).await?;
                if reason.is_none_or(|value| value.trim().is_empty()) {
                    return Err(NewsroomError::Unprocessable(
                        "Fast path requires a reason".to_owned(),
                    ));
                }
                set_fast_path = true;
            }
            // The gate is evaluated in every case; the fast path only waives the
            // approval and asset blockers, never content, claims, pending reviews
            // or confidential-source approval.
            let blockers = self.readiness_blockers(story, fast_path).await?;
            if !blockers.is_empty() {
                return Err(NewsroomError::Conflict(format!(
                    "Story is not ready: {}",
                    blockers.join("; ")
                )));
            }
        }

        let mut tx = self.pool().begin().await?;
        // Close the open dwell row, then open the next; the schema guarantees at
        // most one open row per story.
        sqlx::query(
            "UPDATE meridian.workflow_state_history SET exited_at = now() \
             WHERE story_id = $1 AND exited_at IS NULL",
        )
        .bind(story.id)
        .execute(&mut *tx)
        .await?;
        let latest_version: Option<Uuid> = sqlx::query_scalar(
            "SELECT id FROM meridian.story_versions WHERE story_id = $1 ORDER BY number DESC LIMIT 1",
        )
        .bind(story.id)
        .fetch_optional(&mut *tx)
        .await?;
        sqlx::query(
            "INSERT INTO meridian.workflow_state_history \
             (id, story_id, version_id, from_state, to_state, actor_id, entered_at, reason, fast_path) \
             VALUES ($1,$2,$3,$4,$5,$6, now(), $7, $8)",
        )
        .bind(Uuid::now_v7())
        .bind(story.id)
        .bind(latest_version)
        .bind(current.as_str())
        .bind(target.as_str())
        .bind(actor.id)
        .bind(reason)
        .bind(fast_path)
        .execute(&mut *tx)
        .await?;
        let updated = sqlx::query_as::<_, Story>(&format!(
            "UPDATE meridian.stories SET workflow_state = $2, updated_at = now(), \
               fast_path = fast_path OR $3, \
               fast_path_reason = CASE WHEN $3 THEN $4 ELSE fast_path_reason END \
             WHERE id = $1 RETURNING {STORY_COLUMNS}"
        ))
        .bind(story.id)
        .bind(target.as_str())
        .bind(set_fast_path)
        .bind(reason)
        .fetch_one(&mut *tx)
        .await?;
        audit(
            &mut *tx,
            actor.id,
            "story.workflow.transitioned",
            "story",
            &story.id.to_string(),
            Some(serde_json::json!({ "workflow_state": current.as_str() })),
            Some(serde_json::json!({ "workflow_state": target.as_str() })),
        )
        .await?;
        let stage_payload = serde_json::json!({
            "story_id": story.id, "slug": updated.slug,
            "from": current.as_str(), "to": target.as_str(),
        });
        Self::enqueue_event(&mut *tx, "article.stage_changed", &story.id.to_string(), None, stage_payload, Some(actor.id)).await?;
        tx.commit().await?;
        Ok(updated)
    }
}

fn team_only() -> NewsroomError {
    NewsroomError::Forbidden {
        capability: "stories.team".to_owned(),
        reason: "You are not on this story team".to_owned(),
    }
}

/// Maps a unique-violation on the slug index to a domain conflict.
fn duplicate_slug(error: sqlx::Error) -> NewsroomError {
    if let sqlx::Error::Database(ref db_error) = error
        && db_error.is_unique_violation()
    {
        return NewsroomError::Conflict("A story with that slug already exists".to_owned());
    }
    NewsroomError::Database(error)
}

#[cfg(test)]
mod tests {
    use super::{Story, WorkflowState};
    use crate::Actor;
    use crate::capabilities::Role;
    use uuid::Uuid;

    /// Every state, so a new variant cannot slip past the tests below.
    const ALL_STATES: [WorkflowState; 11] = [
        WorkflowState::Intake,
        WorkflowState::Proposed,
        WorkflowState::Assigned,
        WorkflowState::Reporting,
        WorkflowState::Drafting,
        WorkflowState::DeskReview,
        WorkflowState::Verification,
        WorkflowState::CopyStandards,
        WorkflowState::Ready,
        WorkflowState::Parked,
        WorkflowState::Archived,
    ];

    /// The complete legacy adjacency table, written out independently of the
    /// implementation.
    ///
    /// The graph was transcribed verbatim from the legacy `TRANSITIONS` table, and
    /// a spot-check of three edges cannot catch a typo in the other eight lists.
    /// This asserts the whole thing both ways: every listed edge is legal and
    /// every unlisted edge is refused.
    fn expected_graph() -> [(WorkflowState, &'static [WorkflowState]); 11] {
        use WorkflowState::{
            Archived, Assigned, CopyStandards, DeskReview, Drafting, Intake, Parked, Proposed,
            Ready, Reporting, Verification,
        };
        [
            (Intake, &[Proposed]),
            (Proposed, &[Assigned, Parked]),
            (Assigned, &[Reporting, Parked]),
            (Reporting, &[Drafting, Parked]),
            (Drafting, &[DeskReview, Parked]),
            (DeskReview, &[Drafting, Verification]),
            (Verification, &[Drafting, CopyStandards]),
            (CopyStandards, &[Drafting, Ready]),
            (Ready, &[CopyStandards, Archived]),
            (Parked, &[Proposed, Assigned, Archived]),
            (Archived, &[]),
        ]
    }

    #[test]
    fn the_transition_graph_matches_the_legacy_table_exactly() {
        for (from, expected) in expected_graph() {
            let actual = from.legal_targets();
            assert_eq!(
                actual,
                expected,
                "targets for {} diverged from the legacy table",
                from.as_str()
            );
            // And the complement: nothing outside the list is reachable.
            for candidate in ALL_STATES {
                let legal = expected.contains(&candidate);
                assert_eq!(
                    from.can_transition_to(candidate),
                    legal,
                    "{} -> {} should be {}",
                    from.as_str(),
                    candidate.as_str(),
                    if legal { "legal" } else { "refused" }
                );
            }
        }
    }

    #[test]
    fn no_state_transitions_to_itself() {
        for state in ALL_STATES {
            assert!(
                !state.can_transition_to(state),
                "{} must not loop to itself",
                state.as_str()
            );
        }
    }

    #[test]
    fn archived_is_the_only_terminal_state() {
        for state in ALL_STATES {
            let terminal = state.legal_targets().is_empty();
            assert_eq!(
                terminal,
                state == WorkflowState::Archived,
                "{} terminality is wrong",
                state.as_str()
            );
        }
    }

    /// A state nothing can reach is dead workflow — a story could never arrive
    /// there, so any gate on it would be unreachable too.
    #[test]
    fn every_state_is_reachable_from_intake() {
        let mut seen = vec![WorkflowState::Intake];
        let mut frontier = vec![WorkflowState::Intake];
        while let Some(state) = frontier.pop() {
            for target in state.legal_targets() {
                if !seen.contains(target) {
                    seen.push(*target);
                    frontier.push(*target);
                }
            }
        }
        for state in ALL_STATES {
            assert!(
                seen.contains(&state),
                "{} is unreachable from intake",
                state.as_str()
            );
        }
    }

    /// Drafting is the rework hub: every review stage must be able to bounce a
    /// story back to it, or "request changes" has nowhere to go.
    #[test]
    fn every_review_stage_can_send_a_story_back_to_drafting() {
        for stage in [
            WorkflowState::DeskReview,
            WorkflowState::Verification,
            WorkflowState::CopyStandards,
        ] {
            assert!(
                stage.can_transition_to(WorkflowState::Drafting),
                "{} must be able to request changes",
                stage.as_str()
            );
        }
    }

    #[test]
    fn only_the_gated_stages_require_the_advance_capability() {
        for state in ALL_STATES {
            let gated = matches!(
                state,
                WorkflowState::Verification | WorkflowState::CopyStandards | WorkflowState::Ready
            );
            assert_eq!(
                state.requires_editor(),
                gated,
                "{} editor gate is wrong",
                state.as_str()
            );
        }
    }

    #[test]
    fn every_workflow_state_round_trips_through_its_wire_value() {
        for state in ALL_STATES {
            assert_eq!(
                WorkflowState::from_wire(state.as_str()),
                Some(state),
                "{} did not round-trip",
                state.as_str()
            );
        }
        assert_eq!(WorkflowState::from_wire("nope"), None);
        assert_eq!(WorkflowState::from_wire(""), None);
        // Wire values are snake_case, never the Rust variant spelling.
        assert_eq!(WorkflowState::from_wire("DeskReview"), None);
    }

    fn story_with_team(
        author: Uuid,
        editor: Option<Uuid>,
        fact_checker: Option<Uuid>,
        copy_editor: Option<Uuid>,
    ) -> Story {
        Story {
            id: Uuid::nil(),
            slug: String::new(),
            title: String::new(),
            dek: String::new(),
            body: serde_json::Value::Array(Vec::new()),
            category: String::new(),
            tags: serde_json::Value::Array(Vec::new()),
            story_type: String::new(),
            workflow_state: "drafting".to_owned(),
            publication_state: "not_live".to_owned(),
            validity_state: "current".to_owned(),
            priority: "medium".to_owned(),
            desk_id: None,
            sub_sector_id: None,
            event_id: None,
            pitch_id: None,
            author_id: author,
            editor_id: editor,
            fact_checker_id: fact_checker,
            copy_editor_id: copy_editor,
            due_at: None,
            scheduled_at: None,
            published_at: None,
            version_number: 1,
            fast_path: false,
            fast_path_reason: None,
            rights_holder: None,
            rights_notice: None,
            usage_terms: None,
            license_url: None,
            reuse_embargo_until: None,
            territory_mode: "any".to_owned(),
            territories: None,
            digital_source_type: None,
            created_at: time::OffsetDateTime::UNIX_EPOCH,
            updated_at: time::OffsetDateTime::UNIX_EPOCH,
            author_name: None,
        }
    }

    /// `is_team_member` is now a *pure* team check — the "or an editor role" arm
    /// moved to the sector-scoped `stories.edit_any` capability in Phase 0. This
    /// pins that separation: no role, however senior, satisfies it.
    #[test]
    fn team_membership_is_purely_about_the_team_not_the_role() {
        let author = Uuid::from_u128(1);
        let editor = Uuid::from_u128(2);
        let checker = Uuid::from_u128(3);
        let copy = Uuid::from_u128(4);
        let stranger = Uuid::from_u128(9);
        let story = story_with_team(author, Some(editor), Some(checker), Some(copy));

        for (member, label) in [
            (author, "author"),
            (editor, "editor"),
            (checker, "fact checker"),
            (copy, "copy editor"),
        ] {
            let actor = Actor { id: member, role: Role::Viewer, is_admin: false };
            assert!(story.is_team_member(&actor), "{label} is on the team");
        }

        // An editor-in-chief who is not on the team is not a team member; their
        // reach comes from the capability, resolved against the story's sector.
        for role in [Role::EditorInChief, Role::SectionEditor, Role::Admin] {
            let actor = Actor { id: stranger, role, is_admin: false };
            assert!(
                !story.is_team_member(&actor),
                "{} off the team is not a team member",
                role.as_str()
            );
        }
    }

    #[test]
    fn an_unset_team_slot_never_matches() {
        let author = Uuid::from_u128(1);
        let story = story_with_team(author, None, None, None);
        // A nil-uuid actor must not match the empty editor/checker/copy slots.
        let actor = Actor { id: Uuid::nil(), role: Role::Reporter, is_admin: false };
        assert!(!story.is_team_member(&actor));
    }
}
