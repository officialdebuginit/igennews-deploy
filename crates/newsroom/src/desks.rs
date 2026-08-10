//! Desk structure domain + repository, ported from the legacy `desks/service.py`.
//!
//! A desk is what the UI calls a workspace; there is no separate Workspace
//! entity. Membership deliberately does not inherit down the `parent_id` tree —
//! that is reporting/rollup structure only.
//!
//! `analytics` aggregates `stories`, `tasks` and `workflow_state_history` — all of
//! which now exist — so `desk_analytics`, `sla_breaches` and `median_hours_in_state`
//! are implemented. A workflow state with no SLA row cannot breach: silence over an
//! invented threshold.

use serde::Serialize;
use sqlx::FromRow;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::{Actor, NewsroomError, NewsroomService, audit, authz};

const WORKFLOW_STATES: [&str; 11] = [
    "intake",
    "proposed",
    "assigned",
    "reporting",
    "drafting",
    "desk_review",
    "verification",
    "copy_standards",
    "ready",
    "parked",
    "archived",
];

#[derive(Clone, Debug, FromRow, Serialize)]
pub struct Desk {
    pub id: Uuid,
    pub name: String,
    pub slug: String,
    pub description: Option<String>,
    pub lead_user_id: Option<Uuid>,
    pub parent_id: Option<Uuid>,
    pub settings: serde_json::Value,
    pub is_archived: bool,
    #[serde(with = "time::serde::rfc3339::option")]
    pub archived_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(skip_serializing)]
    pub updated_at: OffsetDateTime,
}

#[derive(Clone, Debug, FromRow, Serialize)]
pub struct DeskSla {
    #[serde(skip)]
    pub id: Uuid,
    #[serde(skip)]
    pub desk_id: Uuid,
    pub workflow_state: String,
    pub target_hours: f64,
    pub warn_at_percent: i32,
    pub is_active: bool,
}

#[derive(Clone, Debug, FromRow, Serialize)]
pub struct DeskSchedule {
    pub desk_id: Uuid,
    pub timezone: String,
    pub hours: serde_json::Value,
    pub on_call_user_id: Option<Uuid>,
    pub notes: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
}

#[derive(Clone, Debug, FromRow, Serialize)]
pub struct DeskInvitation {
    pub id: Uuid,
    pub desk_id: Uuid,
    pub user_id: Uuid,
    pub invited_by_id: Option<Uuid>,
    pub desk_role: String,
    pub status: String,
    pub message: Option<String>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub responded_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

/// The eight workspace roles a desk membership may carry, matching the
/// `desk_memberships_role_check` constraint added in migration `0016`.
pub const WORKSPACE_ROLES: [&str; 8] = [
    "section_editor",
    "managing_editor",
    "reporter",
    "copy_editor",
    "fact_checker",
    "producer",
    "contributor",
    "viewer",
];

/// Rejects a role the `desk_memberships` constraint would refuse.
///
/// Without this the constraint violation surfaces as a **500**: a bad role in the
/// request body is a caller error, and answering it with an internal-error status
/// both misleads the caller and hides a real fault behind the same code. Migration
/// `0016` deliberately narrowed this column, so the validation has to move up with
/// it.
///
/// # Errors
/// [`NewsroomError::Unprocessable`] when the role is not one of the eight.
pub fn validate_workspace_role(role: &str) -> Result<(), NewsroomError> {
    if WORKSPACE_ROLES.contains(&role) {
        return Ok(());
    }
    Err(NewsroomError::Unprocessable(format!(
        "'{role}' is not a workspace role; expected one of {}",
        WORKSPACE_ROLES.join(", ")
    )))
}

/// One node of the desk hierarchy response.
#[derive(Clone, Debug, Serialize)]
pub struct DeskTreeNode {
    pub id: Uuid,
    pub name: String,
    pub slug: String,
    pub is_archived: bool,
    pub children: Vec<DeskTreeNode>,
}

/// A requested SLA row, validated before it replaces the stored set.
#[derive(Clone, Debug)]
pub struct SlaEntry {
    pub workflow_state: String,
    pub target_hours: f64,
    pub warn_at_percent: i32,
    pub is_active: bool,
}

/// The mutable fields of a desk schedule.
#[derive(Clone, Debug)]
pub struct ScheduleInput {
    pub timezone: String,
    pub hours: serde_json::Value,
    pub on_call_user_id: Option<Uuid>,
    pub notes: Option<String>,
}

/// A partial desk update; a `None` field is left unchanged.
#[derive(Clone, Debug, Default)]
pub struct DeskPatch {
    pub name: Option<String>,
    pub slug: Option<String>,
    pub description: Option<String>,
    pub lead_user_id: Option<Uuid>,
    pub parent_id: Option<Uuid>,
    /// The desk's registry profile (policy, documents, metadata) as a jsonb blob.
    /// `None` leaves it unchanged; `Some(value)` replaces it wholesale.
    pub settings: Option<serde_json::Value>,
}

/// A request to join a sector, with KYC document references. The three trailing
/// fields are joined in only by the list queries (`#[sqlx(default)]` elsewhere).
#[derive(Clone, Debug, FromRow, Serialize)]
pub struct SectorApplication {
    pub id: Uuid,
    pub desk_id: Uuid,
    pub user_id: Uuid,
    pub role: String,
    pub status: String,
    pub message: Option<String>,
    pub kyc_documents: serde_json::Value,
    pub decision_note: Option<String>,
    pub reviewed_by: Option<Uuid>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub reviewed_at: Option<OffsetDateTime>,
    /// Post-quantum verification certificate (FIPS 204 ML-DSA), set on approval.
    pub verification_signature: Option<String>,
    pub verification_alg: Option<String>,
    pub verification_pubkey: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[sqlx(default)]
    #[serde(default)]
    pub applicant_name: Option<String>,
    #[sqlx(default)]
    #[serde(default)]
    pub sector_name: Option<String>,
    #[sqlx(default)]
    #[serde(default)]
    pub sector_slug: Option<String>,
}

/// An approved application's KYC certificate, with the signed statement recreated
/// and re-verified. Serialized to the applicant and to any reviewer who wants to
/// independently confirm the post-quantum signature.
#[derive(Clone, Debug, Serialize)]
pub struct CertificateView {
    pub application_id: Uuid,
    pub user_id: Uuid,
    pub desk_id: Uuid,
    pub decision: String,
    #[serde(with = "time::serde::rfc3339::option")]
    pub issued_at: Option<OffsetDateTime>,
    pub document_count: usize,
    /// The exact canonical statement the signature covers, rebuilt from the row.
    pub statement: String,
    pub signature: String,
    pub algorithm: String,
    pub public_key: String,
    /// `true` when the signature verifies against the statement and public key.
    pub valid: bool,
}

/// The `sector_applications` table columns the [`SectorApplication`] row needs
/// (the joined name fields are supplied separately per query).
const APPLICATION_COLUMNS: &str = "id, desk_id, user_id, role, status, message, \
    kyc_documents, decision_note, reviewed_by, reviewed_at, verification_signature, \
    verification_alg, verification_pubkey, created_at";

/// A workflow state whose median dwell has reached its SLA threshold.
#[derive(Clone, Debug, Serialize)]
pub struct SlaBreach {
    pub workflow_state: String,
    pub target_hours: f64,
    pub median_hours: f64,
    pub status: String,
}

/// Aggregate desk analytics, ported from the legacy `desks.service.analytics`.
#[derive(Debug, Serialize)]
pub struct DeskAnalytics {
    pub desk_id: Uuid,
    pub included_desk_ids: Vec<Uuid>,
    pub stories_by_state: std::collections::BTreeMap<String, i64>,
    pub total_stories: i64,
    pub open_tasks: i64,
    pub overdue_tasks: i64,
    pub member_count: i64,
    pub sla_breaches: Vec<SlaBreach>,
    #[serde(with = "time::serde::rfc3339")]
    pub generated_at: OffsetDateTime,
}

impl NewsroomService {
    /// Every desk, newest first; archived desks included only when asked.
    ///
    /// # Errors
    /// Propagates database failures.
    pub async fn list_desks(
        &self,
        actor: &Actor,
        include_archived: bool,
    ) -> Result<Vec<Desk>, NewsroomError> {
        // The same rule `can_view_desk` applies one desk at a time, expressed as a
        // set so the sector switcher can be driven straight off this list: a
        // global-dashboard holder sees every desk, everyone else sees the desks
        // they hold a membership in.
        let sees_all = authz::has(self.pool(), actor, "dashboard.view_global", None).await?;
        Ok(sqlx::query_as::<_, Desk>(
            "SELECT * FROM meridian.desks \
             WHERE ($1 OR NOT is_archived) \
               AND ($2::boolean OR id IN (SELECT desk_id FROM meridian.desk_memberships \
                                           WHERE user_id = $3)) \
             ORDER BY name",
        )
        .bind(include_archived)
        .bind(sees_all)
        .bind(actor.id)
        .fetch_all(self.pool())
        .await?)
    }

    /// Creates a desk; requires `desks.manage`.
    ///
    /// # Errors
    /// [`NewsroomError::Forbidden`]; [`NewsroomError::Conflict`] on a duplicate
    /// name or slug; database failures.
    pub async fn create_desk(
        &self,
        actor: &Actor,
        name: &str,
        slug: &str,
        description: Option<&str>,
        lead_user_id: Option<Uuid>,
        parent_id: Option<Uuid>,
    ) -> Result<Desk, NewsroomError> {
        // Creating a *top-level* sector is org-wide and admin-only (sectors.admin);
        // creating a sub-desk within a sector is delegated to a desk-scoped
        // desks.manage on the parent. This closes the gap where desks.manage asked
        // with a None scope fell through to the whole EDITORS audience — see the
        // sector-taxonomy doc §3.4.
        match parent_id {
            Some(parent) => {
                authz::require(self.pool(), actor, "desks.manage", Some(parent)).await?;
            }
            None => authz::require(self.pool(), actor, "sectors.admin", None).await?,
        }
        let mut tx = self.pool().begin().await?;
        let desk = sqlx::query_as::<_, Desk>(
            "INSERT INTO meridian.desks (id, name, slug, description, lead_user_id, parent_id) \
             VALUES ($1,$2,$3,$4,$5,$6) RETURNING *",
        )
        .bind(Uuid::now_v7())
        .bind(name)
        .bind(slug)
        .bind(description)
        .bind(lead_user_id)
        .bind(parent_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(duplicate_desk)?;
        audit(
            &mut *tx,
            actor.id,
            "desk.created",
            "desk",
            &desk.id.to_string(),
            None,
            Some(serde_json::json!({ "name": desk.name })),
        )
        .await?;
        Self::enqueue_event(
            &mut *tx,
            "desk.created",
            &desk.id.to_string(),
            None,
            serde_json::json!({ "desk_id": desk.id, "name": desk.name, "slug": desk.slug }),
            Some(actor.id),
        )
        .await?;
        tx.commit().await?;
        Ok(desk)
    }

    /// Updates a desk's editable fields; requires desk-admin rights. Re-parenting
    /// is validated for cycles.
    ///
    /// # Errors
    /// [`NewsroomError::Forbidden`]; [`NewsroomError::NotFound`];
    /// [`NewsroomError::Unprocessable`] for a cycle; database failures.
    pub async fn update_desk(
        &self,
        actor: &Actor,
        desk_id: Uuid,
        patch: &DeskPatch,
    ) -> Result<Desk, NewsroomError> {
        let (name, slug, description, lead_user_id, parent_id) = (
            patch.name.as_deref(),
            patch.slug.as_deref(),
            patch.description.as_deref(),
            patch.lead_user_id,
            patch.parent_id,
        );
        let desk = self.get_desk(desk_id).await?;
        self.require_desk_admin(actor, &desk).await?;
        if let Some(parent) = parent_id {
            // Reuse the cycle-safe re-parent guard.
            self.set_parent(actor, &desk, Some(parent)).await?;
        }
        let mut tx = self.pool().begin().await?;
        let updated = sqlx::query_as::<_, Desk>(
            "UPDATE meridian.desks SET \
               name = COALESCE($2, name), slug = COALESCE($3, slug), \
               description = COALESCE($4, description), \
               lead_user_id = COALESCE($5, lead_user_id), \
               settings = COALESCE($6, settings), updated_at = now() \
             WHERE id = $1 RETURNING *",
        )
        .bind(desk_id)
        .bind(name)
        .bind(slug)
        .bind(description)
        .bind(lead_user_id)
        .bind(&patch.settings)
        .fetch_one(&mut *tx)
        .await
        .map_err(duplicate_desk)?;
        audit(
            &mut *tx,
            actor.id,
            "desk.updated",
            "desk",
            &desk_id.to_string(),
            None,
            None,
        )
        .await?;
        Self::enqueue_event(
            &mut *tx,
            "desk.updated",
            &desk_id.to_string(),
            None,
            serde_json::json!({ "desk_id": desk_id }),
            Some(actor.id),
        )
        .await?;
        // A settings change is a sector-policy change (the policy lives in
        // desks.settings.policy — see the taxonomy doc §4); emit the distinct event.
        if patch.settings.is_some() {
            Self::enqueue_event(
                &mut *tx,
                "sector.policy.updated",
                &desk_id.to_string(),
                Some(&updated.slug),
                serde_json::json!({ "desk_id": desk_id, "slug": updated.slug }),
                Some(actor.id),
            )
            .await?;
        }
        tx.commit().await?;
        Ok(updated)
    }

    /// Deletes a desk; requires `desks.manage`.
    ///
    /// # Errors
    /// [`NewsroomError::Forbidden`]; [`NewsroomError::NotFound`]; database failures.
    pub async fn delete_desk(&self, actor: &Actor, desk_id: Uuid) -> Result<(), NewsroomError> {
        // Deleting a top-level sector is admin-only (sectors.admin); a sub-desk is
        // governed by desk-scoped desks.manage on the desk itself (doc §3.3).
        let desk = self.get_desk(desk_id).await?;
        if desk.parent_id.is_none() {
            authz::require(self.pool(), actor, "sectors.admin", None).await?;
        } else {
            authz::require(self.pool(), actor, "desks.manage", Some(desk_id)).await?;
        }
        let mut tx = self.pool().begin().await?;
        let deleted = sqlx::query("DELETE FROM meridian.desks WHERE id = $1")
            .bind(desk_id)
            .execute(&mut *tx)
            .await?;
        if deleted.rows_affected() == 0 {
            return Err(NewsroomError::NotFound("Desk"));
        }
        audit(
            &mut *tx,
            actor.id,
            "desk.deleted",
            "desk",
            &desk_id.to_string(),
            None,
            None,
        )
        .await?;
        Self::enqueue_event(
            &mut *tx,
            "desk.deleted",
            &desk_id.to_string(),
            None,
            serde_json::json!({ "desk_id": desk_id }),
            Some(actor.id),
        )
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Loads a desk or returns [`NewsroomError::NotFound`].
    ///
    /// # Errors
    /// Propagates database failures; not-found is a domain error.
    pub async fn get_desk(&self, desk_id: Uuid) -> Result<Desk, NewsroomError> {
        sqlx::query_as::<_, Desk>("SELECT * FROM meridian.desks WHERE id = $1")
            .bind(desk_id)
            .fetch_optional(self.pool())
            .await?
            .ok_or(NewsroomError::NotFound("Desk"))
    }

    fn is_lead(desk: &Desk, actor: &Actor) -> bool {
        desk.lead_user_id == Some(actor.id)
    }

    /// A desk's own lead may manage it; otherwise `desks.manage` is required.
    /// Lead-ness is a row-level fact the capability registry cannot express.
    pub(crate) async fn require_desk_admin(
        &self,
        actor: &Actor,
        desk: &Desk,
    ) -> Result<(), NewsroomError> {
        if Self::is_lead(desk, actor) {
            return Ok(());
        }
        authz::require(self.pool(), actor, "desks.manage", Some(desk.id)).await
    }

    async fn require_can_invite(&self, actor: &Actor, desk: &Desk) -> Result<(), NewsroomError> {
        if Self::is_lead(desk, actor) {
            return Ok(());
        }
        authz::require(self.pool(), actor, "desks.invite", Some(desk.id)).await
    }

    // --- Hierarchy ----------------------------------------------------------

    /// Every desk id at or below `desk_id`, breadth-first. Desk trees are tiny,
    /// so this iterates in Rust rather than a recursive CTE, and the `seen`
    /// guard means a pre-existing cycle cannot loop forever.
    ///
    /// # Errors
    /// Propagates database failures.
    pub async fn descendant_ids(
        &self,
        desk_id: Uuid,
        include_self: bool,
    ) -> Result<Vec<Uuid>, NewsroomError> {
        let mut collected: Vec<Uuid> = if include_self { vec![desk_id] } else { Vec::new() };
        let mut seen: Vec<Uuid> = vec![desk_id];
        let mut frontier: Vec<Uuid> = vec![desk_id];
        while !frontier.is_empty() {
            let children: Vec<Uuid> = sqlx::query_scalar(
                "SELECT id FROM meridian.desks WHERE parent_id = ANY($1)",
            )
            .bind(&frontier)
            .fetch_all(self.pool())
            .await?;
            frontier = children
                .into_iter()
                .filter(|child| !seen.contains(child))
                .collect();
            seen.extend(frontier.iter().copied());
            collected.extend(frontier.iter().copied());
        }
        Ok(collected)
    }

    /// The desk chain from `desk`'s parent up to the root, cycle-safe.
    ///
    /// # Errors
    /// Propagates database failures.
    pub async fn ancestors(&self, desk: &Desk) -> Result<Vec<Desk>, NewsroomError> {
        let mut chain: Vec<Desk> = Vec::new();
        let mut seen: Vec<Uuid> = vec![desk.id];
        let mut current = desk.parent_id;
        while let Some(parent_id) = current {
            if seen.contains(&parent_id) {
                break;
            }
            let Some(parent) = sqlx::query_as::<_, Desk>("SELECT * FROM meridian.desks WHERE id = $1")
                .bind(parent_id)
                .fetch_optional(self.pool())
                .await?
            else {
                break;
            };
            seen.push(parent.id);
            current = parent.parent_id;
            chain.push(parent);
        }
        Ok(chain)
    }

    /// The desk hierarchy rooted at `desk_id`, children sorted by name.
    ///
    /// # Errors
    /// Propagates database failures; not-found for an unknown root.
    pub async fn desk_tree(&self, desk_id: Uuid) -> Result<DeskTreeNode, NewsroomError> {
        let root = self.get_desk(desk_id).await?;
        let ids = self.descendant_ids(desk_id, true).await?;
        let rows = sqlx::query_as::<_, Desk>(
            "SELECT * FROM meridian.desks WHERE id = ANY($1) ORDER BY name",
        )
        .bind(&ids)
        .fetch_all(self.pool())
        .await?;
        Ok(build_tree(&root, &rows))
    }

    /// Re-parents a desk, rejecting anything that would create a cycle.
    ///
    /// # Errors
    /// [`NewsroomError::Unprocessable`] for a self-parent or a move under a
    /// descendant; [`NewsroomError::NotFound`] for an unknown parent.
    pub async fn set_parent(
        &self,
        actor: &Actor,
        desk: &Desk,
        parent_id: Option<Uuid>,
    ) -> Result<(), NewsroomError> {
        self.require_desk_admin(actor, desk).await?;
        match parent_id {
            None => {
                sqlx::query("UPDATE meridian.desks SET parent_id = NULL, updated_at = now() WHERE id = $1")
                    .bind(desk.id)
                    .execute(self.pool())
                    .await?;
            }
            Some(parent) => {
                if parent == desk.id {
                    return Err(NewsroomError::Unprocessable(
                        "A desk cannot be its own parent".to_owned(),
                    ));
                }
                let exists: bool =
                    sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM meridian.desks WHERE id = $1)")
                        .bind(parent)
                        .fetch_one(self.pool())
                        .await?;
                if !exists {
                    return Err(NewsroomError::NotFound("Parent desk"));
                }
                if self.descendant_ids(desk.id, true).await?.contains(&parent) {
                    return Err(NewsroomError::Unprocessable(
                        "that move would create a cycle".to_owned(),
                    ));
                }
                sqlx::query("UPDATE meridian.desks SET parent_id = $2, updated_at = now() WHERE id = $1")
                    .bind(desk.id)
                    .bind(parent)
                    .execute(self.pool())
                    .await?;
            }
        }
        Ok(())
    }

    // --- Archiving ----------------------------------------------------------

    /// Archives a desk (idempotent) and records the audit event.
    ///
    /// # Errors
    /// [`NewsroomError::Forbidden`] without desk-admin rights; database failures.
    pub async fn archive(&self, actor: &Actor, desk: &Desk) -> Result<(), NewsroomError> {
        self.require_desk_admin(actor, desk).await?;
        if desk.is_archived {
            return Ok(());
        }
        let mut tx = self.pool().begin().await?;
        sqlx::query(
            "UPDATE meridian.desks SET is_archived = true, archived_at = now(), updated_at = now() WHERE id = $1",
        )
        .bind(desk.id)
        .execute(&mut *tx)
        .await?;
        audit(
            &mut *tx,
            actor.id,
            "desk.archived",
            "desk",
            &desk.id.to_string(),
            None,
            Some(serde_json::json!({ "is_archived": true })),
        )
        .await?;
        Self::enqueue_event(
            &mut *tx,
            "desk.archived",
            &desk.id.to_string(),
            None,
            serde_json::json!({ "desk_id": desk.id }),
            Some(actor.id),
        )
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Unarchives a desk and records the audit event.
    ///
    /// # Errors
    /// [`NewsroomError::Forbidden`] without desk-admin rights; database failures.
    pub async fn unarchive(&self, actor: &Actor, desk: &Desk) -> Result<(), NewsroomError> {
        self.require_desk_admin(actor, desk).await?;
        let mut tx = self.pool().begin().await?;
        sqlx::query(
            "UPDATE meridian.desks SET is_archived = false, archived_at = NULL, updated_at = now() WHERE id = $1",
        )
        .bind(desk.id)
        .execute(&mut *tx)
        .await?;
        audit(
            &mut *tx,
            actor.id,
            "desk.unarchived",
            "desk",
            &desk.id.to_string(),
            None,
            Some(serde_json::json!({ "is_archived": false })),
        )
        .await?;
        Self::enqueue_event(
            &mut *tx,
            "desk.unarchived",
            &desk.id.to_string(),
            None,
            serde_json::json!({ "desk_id": desk.id }),
            Some(actor.id),
        )
        .await?;
        tx.commit().await?;
        Ok(())
    }

    // --- SLAs ---------------------------------------------------------------

    /// The desk's configured SLA rows.
    ///
    /// # Errors
    /// Propagates database failures.
    pub async fn get_slas(&self, desk_id: Uuid) -> Result<Vec<DeskSla>, NewsroomError> {
        Ok(sqlx::query_as::<_, DeskSla>(
            "SELECT * FROM meridian.desk_slas WHERE desk_id = $1 ORDER BY workflow_state",
        )
        .bind(desk_id)
        .fetch_all(self.pool())
        .await?)
    }

    /// Replaces the desk's SLA set with `entries`, deleting any not present.
    ///
    /// # Errors
    /// [`NewsroomError::Unprocessable`] for a non-positive target or unknown
    /// workflow state; [`NewsroomError::Forbidden`]; database failures.
    pub async fn replace_slas(
        &self,
        actor: &Actor,
        desk: &Desk,
        entries: &[SlaEntry],
    ) -> Result<Vec<DeskSla>, NewsroomError> {
        self.require_desk_admin(actor, desk).await?;
        for entry in entries {
            if entry.target_hours <= 0.0 {
                return Err(NewsroomError::Unprocessable(
                    "target_hours must be greater than zero".to_owned(),
                ));
            }
            if !WORKFLOW_STATES.contains(&entry.workflow_state.as_str()) {
                return Err(NewsroomError::Unprocessable(format!(
                    "'{}' is not a workflow state",
                    entry.workflow_state
                )));
            }
        }
        let mut tx = self.pool().begin().await?;
        // Replace-in-place: upsert every requested state, then remove any state
        // that the request no longer lists.
        let mut keep: Vec<String> = Vec::with_capacity(entries.len());
        for entry in entries {
            keep.push(entry.workflow_state.clone());
            sqlx::query(
                "INSERT INTO meridian.desk_slas \
                 (id, desk_id, workflow_state, target_hours, warn_at_percent, is_active, updated_at) \
                 VALUES ($1, $2, $3, $4, $5, $6, now()) \
                 ON CONFLICT (desk_id, workflow_state) DO UPDATE SET \
                   target_hours = EXCLUDED.target_hours, \
                   warn_at_percent = EXCLUDED.warn_at_percent, \
                   is_active = EXCLUDED.is_active, \
                   updated_at = now()",
            )
            .bind(Uuid::now_v7())
            .bind(desk.id)
            .bind(&entry.workflow_state)
            .bind(entry.target_hours)
            .bind(entry.warn_at_percent)
            .bind(entry.is_active)
            .execute(&mut *tx)
            .await?;
        }
        sqlx::query("DELETE FROM meridian.desk_slas WHERE desk_id = $1 AND NOT (workflow_state = ANY($2))")
            .bind(desk.id)
            .bind(&keep)
            .execute(&mut *tx)
            .await?;
        audit(
            &mut *tx,
            actor.id,
            "desk.slas.updated",
            "desk",
            &desk.id.to_string(),
            None,
            Some(serde_json::json!({ "count": entries.len() })),
        )
        .await?;
        tx.commit().await?;
        self.get_slas(desk.id).await
    }

    // --- Schedule -----------------------------------------------------------

    /// The desk's coverage schedule, if one has been set.
    ///
    /// # Errors
    /// Propagates database failures.
    pub async fn get_schedule(&self, desk_id: Uuid) -> Result<Option<DeskSchedule>, NewsroomError> {
        Ok(
            sqlx::query_as::<_, DeskSchedule>("SELECT * FROM meridian.desk_schedules WHERE desk_id = $1")
                .bind(desk_id)
                .fetch_optional(self.pool())
                .await?,
        )
    }

    /// Creates or updates the desk's schedule and records the audit event.
    ///
    /// # Errors
    /// [`NewsroomError::Forbidden`]; database failures.
    pub async fn set_schedule(
        &self,
        actor: &Actor,
        desk: &Desk,
        input: &ScheduleInput,
    ) -> Result<DeskSchedule, NewsroomError> {
        self.require_desk_admin(actor, desk).await?;
        let mut tx = self.pool().begin().await?;
        let schedule = sqlx::query_as::<_, DeskSchedule>(
            "INSERT INTO meridian.desk_schedules \
             (desk_id, timezone, hours, on_call_user_id, notes, updated_at) \
             VALUES ($1, $2, $3, $4, $5, now()) \
             ON CONFLICT (desk_id) DO UPDATE SET \
               timezone = EXCLUDED.timezone, \
               hours = EXCLUDED.hours, \
               on_call_user_id = EXCLUDED.on_call_user_id, \
               notes = EXCLUDED.notes, \
               updated_at = now() \
             RETURNING *",
        )
        .bind(desk.id)
        .bind(&input.timezone)
        .bind(&input.hours)
        .bind(input.on_call_user_id)
        .bind(&input.notes)
        .fetch_one(&mut *tx)
        .await?;
        audit(
            &mut *tx,
            actor.id,
            "desk.schedule.updated",
            "desk",
            &desk.id.to_string(),
            None,
            Some(serde_json::json!({ "timezone": input.timezone })),
        )
        .await?;
        tx.commit().await?;
        Ok(schedule)
    }

    // --- Invitations --------------------------------------------------------

    /// Invites a user to a desk.
    ///
    /// Notifies the invitee in the same transaction as the invitation row and the
    /// audit event, so an invitation can never exist unannounced.
    ///
    /// # Errors
    /// [`NewsroomError::Conflict`] for an archived desk, an existing member, or a
    /// pending invitation; [`NewsroomError::NotFound`] for an unknown user;
    /// [`NewsroomError::Forbidden`]; database failures.
    pub async fn invite(
        &self,
        actor: &Actor,
        desk: &Desk,
        user_id: Uuid,
        desk_role: &str,
        message: Option<&str>,
    ) -> Result<DeskInvitation, NewsroomError> {
        validate_workspace_role(desk_role)?;
        self.require_can_invite(actor, desk).await?;
        if desk.is_archived {
            return Err(NewsroomError::Conflict(
                "Cannot invite to an archived desk".to_owned(),
            ));
        }
        let user_exists: bool =
            sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM meridian.users WHERE id = $1)")
                .bind(user_id)
                .fetch_one(self.pool())
                .await?;
        if !user_exists {
            return Err(NewsroomError::NotFound("User"));
        }
        let already_member: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM meridian.desk_memberships WHERE desk_id = $1 AND user_id = $2)",
        )
        .bind(desk.id)
        .bind(user_id)
        .fetch_one(self.pool())
        .await?;
        if already_member {
            return Err(NewsroomError::Conflict(
                "That person is already a member of this desk".to_owned(),
            ));
        }
        let pending: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM meridian.desk_invitations \
             WHERE desk_id = $1 AND user_id = $2 AND status = 'pending')",
        )
        .bind(desk.id)
        .bind(user_id)
        .fetch_one(self.pool())
        .await?;
        if pending {
            return Err(NewsroomError::Conflict(
                "That person already has a pending invitation".to_owned(),
            ));
        }
        let mut tx = self.pool().begin().await?;
        let invitation = sqlx::query_as::<_, DeskInvitation>(
            "INSERT INTO meridian.desk_invitations \
             (id, desk_id, user_id, invited_by_id, desk_role, status, message, created_at) \
             VALUES ($1, $2, $3, $4, $5, 'pending', $6, now()) RETURNING *",
        )
        .bind(Uuid::now_v7())
        .bind(desk.id)
        .bind(user_id)
        .bind(actor.id)
        .bind(desk_role)
        .bind(message)
        .fetch_one(&mut *tx)
        .await?;
        // Notify the invitee in the same transaction as the invitation.
        crate::awareness::notify(
            &mut tx,
            &crate::awareness::NotifyInput {
                user_id,
                kind: "desk.invitation",
                title: &format!("You have been invited to the {} desk", desk.name),
                body: message,
                entity_type: "desk",
                entity_id: &desk.id.to_string(),
                priority: "normal",
                group_key: None,
            },
        )
        .await?;
        audit(
            &mut *tx,
            actor.id,
            "desk.invitation.created",
            "desk",
            &desk.id.to_string(),
            None,
            Some(serde_json::json!({ "user_id": user_id, "role": desk_role })),
        )
        .await?;
        tx.commit().await?;
        Ok(invitation)
    }

    /// Accepts or declines an invitation. A mismatched actor returns not-found
    /// rather than forbidden, so the response never confirms the invitation
    /// exists or who it was for.
    ///
    /// # Errors
    /// [`NewsroomError::NotFound`] for an unknown or foreign invitation;
    /// [`NewsroomError::Conflict`] if already resolved; database failures.
    pub async fn respond_to_invitation(
        &self,
        actor: &Actor,
        invitation_id: Uuid,
        accept: bool,
    ) -> Result<DeskInvitation, NewsroomError> {
        let mut tx = self.pool().begin().await?;
        let invitation = sqlx::query_as::<_, DeskInvitation>(
            "SELECT * FROM meridian.desk_invitations WHERE id = $1 FOR UPDATE",
        )
        .bind(invitation_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(NewsroomError::NotFound("Invitation"))?;
        if invitation.user_id != actor.id {
            return Err(NewsroomError::NotFound("Invitation"));
        }
        if invitation.status != "pending" {
            return Err(NewsroomError::Conflict(format!(
                "This invitation was already {}",
                invitation.status
            )));
        }
        let new_status = if accept { "accepted" } else { "declined" };
        let updated = sqlx::query_as::<_, DeskInvitation>(
            "UPDATE meridian.desk_invitations SET status = $2, responded_at = now() WHERE id = $1 RETURNING *",
        )
        .bind(invitation_id)
        .bind(new_status)
        .fetch_one(&mut *tx)
        .await?;
        if accept {
            sqlx::query(
                "INSERT INTO meridian.desk_memberships (desk_id, user_id, role, joined_at) \
                 VALUES ($1, $2, $3, now()) ON CONFLICT (desk_id, user_id) DO NOTHING",
            )
            .bind(invitation.desk_id)
            .bind(actor.id)
            .bind(&invitation.desk_role)
            .execute(&mut *tx)
            .await?;
        }
        audit(
            &mut *tx,
            actor.id,
            &format!("desk.invitation.{new_status}"),
            "desk",
            &invitation.desk_id.to_string(),
            None,
            None,
        )
        .await?;
        tx.commit().await?;
        Ok(updated)
    }

    /// The pending invitations addressed to `actor`.
    ///
    /// # Errors
    /// Propagates database failures.
    pub async fn pending_invitations_for(
        &self,
        actor: &Actor,
    ) -> Result<Vec<DeskInvitation>, NewsroomError> {
        Ok(sqlx::query_as::<_, DeskInvitation>(
            "SELECT * FROM meridian.desk_invitations \
             WHERE user_id = $1 AND status = 'pending' ORDER BY created_at DESC",
        )
        .bind(actor.id)
        .fetch_all(self.pool())
        .await?)
    }

    // --- Sector applications (Phase 3) -----------------------------------

    /// Every non-archived sector, for the browse/apply directory — no membership
    /// filter, so a user can discover sectors they are not yet in. Read-only; the
    /// registry profile rides along in `settings`.
    ///
    /// # Errors
    /// Propagates database failures.
    pub async fn sectors_directory(&self) -> Result<Vec<Desk>, NewsroomError> {
        Ok(sqlx::query_as::<_, Desk>(
            "SELECT * FROM meridian.desks WHERE is_archived = false ORDER BY name",
        )
        .fetch_all(self.pool())
        .await?)
    }

    /// A user's request to join a sector, with KYC document references. Admin
    /// approval turns it into a `desk_membership`.
    pub async fn apply_to_sector(
        &self,
        actor: &Actor,
        desk_id: Uuid,
        role: &str,
        message: Option<&str>,
        kyc_documents: &serde_json::Value,
    ) -> Result<SectorApplication, NewsroomError> {
        // Already a member? Nothing to apply for.
        let member: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM meridian.desk_memberships WHERE desk_id = $1 AND user_id = $2)",
        )
        .bind(desk_id)
        .bind(actor.id)
        .fetch_one(self.pool())
        .await?;
        if member {
            return Err(NewsroomError::Conflict("You are already a member of this sector".to_owned()));
        }
        sqlx::query_as::<_, SectorApplication>(&format!(
            "INSERT INTO meridian.sector_applications (id, desk_id, user_id, role, message, kyc_documents) \
             VALUES ($1,$2,$3,$4,$5,$6) RETURNING {APPLICATION_COLUMNS}"
        ))
        .bind(Uuid::now_v7())
        .bind(desk_id)
        .bind(actor.id)
        .bind(role)
        .bind(message)
        .bind(kyc_documents)
        .fetch_one(self.pool())
        .await
        .map_err(|error| match &error {
            sqlx::Error::Database(db) if db.constraint() == Some("sector_applications_one_pending") => {
                NewsroomError::Conflict("You already have a pending application for this sector".to_owned())
            }
            _ => NewsroomError::from(error),
        })
    }

    /// The signed-in user's own applications, newest first, with sector names.
    ///
    /// # Errors
    /// Propagates database failures.
    pub async fn my_applications(
        &self,
        actor: &Actor,
    ) -> Result<Vec<SectorApplication>, NewsroomError> {
        Ok(sqlx::query_as::<_, SectorApplication>(
            "SELECT a.*, NULL::text AS applicant_name, d.name AS sector_name, d.slug AS sector_slug \
             FROM meridian.sector_applications a JOIN meridian.desks d ON d.id = a.desk_id \
             WHERE a.user_id = $1 ORDER BY a.created_at DESC",
        )
        .bind(actor.id)
        .fetch_all(self.pool())
        .await?)
    }

    /// Every pending application across sectors, for the admin approval queue;
    /// requires the org-level `desks.manage` capability.
    ///
    /// # Errors
    /// [`NewsroomError::Forbidden`]; database failures.
    pub async fn pending_applications(
        &self,
        actor: &Actor,
    ) -> Result<Vec<SectorApplication>, NewsroomError> {
        authz::require(self.pool(), actor, "desks.manage", None).await?;
        Ok(sqlx::query_as::<_, SectorApplication>(
            "SELECT a.*, u.display_name AS applicant_name, d.name AS sector_name, d.slug AS sector_slug \
             FROM meridian.sector_applications a \
             JOIN meridian.users u ON u.id = a.user_id \
             JOIN meridian.desks d ON d.id = a.desk_id \
             WHERE a.status = 'pending' ORDER BY a.created_at",
        )
        .fetch_all(self.pool())
        .await?)
    }

    /// The applications for a *single* sector — the queue a delegated sector manager
    /// sees. Gated on desk-scoped `desks.manage` (or desk lead), so a delegate sees
    /// only their own sector's applicants and never another sector's (doc §5, §7.8).
    ///
    /// # Errors
    /// [`NewsroomError::Forbidden`] without the grant; [`NewsroomError::NotFound`] for
    /// an unknown sector; propagates database failures.
    pub async fn sector_applications(
        &self,
        actor: &Actor,
        desk_id: Uuid,
    ) -> Result<Vec<SectorApplication>, NewsroomError> {
        let desk = self.get_desk(desk_id).await?;
        self.require_desk_admin(actor, &desk).await?;
        Ok(sqlx::query_as::<_, SectorApplication>(
            "SELECT a.*, u.display_name AS applicant_name, d.name AS sector_name, d.slug AS sector_slug \
             FROM meridian.sector_applications a \
             JOIN meridian.users u ON u.id = a.user_id \
             JOIN meridian.desks d ON d.id = a.desk_id \
             WHERE a.desk_id = $1 ORDER BY a.created_at DESC",
        )
        .bind(desk_id)
        .fetch_all(self.pool())
        .await?)
    }

    /// Approves or rejects an application; on approval, creates the sector
    /// membership. Requires `desks.manage` scoped to the application's sector.
    ///
    /// # Errors
    /// [`NewsroomError::Forbidden`]; [`NewsroomError::NotFound`]; database failures.
    pub async fn review_application(
        &self,
        actor: &Actor,
        application_id: Uuid,
        approve: bool,
        decision_note: Option<&str>,
    ) -> Result<SectorApplication, NewsroomError> {
        let app = sqlx::query_as::<_, SectorApplication>(&format!(
            "SELECT {APPLICATION_COLUMNS}, NULL::text AS applicant_name, NULL::text AS sector_name, \
             NULL::text AS sector_slug FROM meridian.sector_applications WHERE id = $1"
        ))
        .bind(application_id)
        .fetch_optional(self.pool())
        .await?
        .ok_or(NewsroomError::NotFound("Application"))?;
        authz::require(self.pool(), actor, "desks.manage", Some(app.desk_id)).await?;
        // Only a pending application can be reviewed. Without this guard, reviewing
        // an already-approved application again (a second admin, a stale queue, or a
        // direct API call) would flip its status and destroy its certificate while
        // leaving the granted desk membership in place — an approved-then-"rejected"
        // user who still has access.
        if app.status != "pending" {
            return Err(NewsroomError::Conflict(
                "This application has already been reviewed".to_owned(),
            ));
        }
        let new_status = if approve { "approved" } else { "rejected" };
        // Sign a post-quantum (ML-DSA) verification certificate on approval. The
        // signed timestamp is generated here and also stored as reviewed_at, so the
        // certificate and the row agree.
        // Truncate to microseconds before signing *and* storing: Postgres
        // `timestamptz` keeps only microsecond precision, so a nanosecond-precise
        // instant would format one way here and a different way after the round
        // trip — breaking re-verification. Truncating first makes the stored row
        // reproduce this exact certificate message.
        let now = OffsetDateTime::now_utc();
        let issued_at = now
            .replace_nanosecond((now.nanosecond() / 1_000) * 1_000)
            .unwrap_or(now);
        let issued_str = issued_at
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap_or_default();
        let (signature, alg, pubkey) = if approve {
            let signer = crate::kyc_signing::KycSigner::from_env();
            let doc_count = app.kyc_documents.as_array().map_or(0, Vec::len);
            let message = crate::kyc_signing::certificate_message(
                &application_id.to_string(),
                &app.user_id.to_string(),
                &app.desk_id.to_string(),
                doc_count,
                "approved",
                &issued_str,
            );
            signer.sign_b64(&message).map_or((None, None, None), |sig| {
                (Some(sig), Some(crate::kyc_signing::KYC_SIGNING_ALG.to_owned()), Some(signer.public_key_b64()))
            })
        } else {
            (None, None, None)
        };
        let mut tx = self.pool().begin().await?;
        let updated = sqlx::query_as::<_, SectorApplication>(&format!(
            "UPDATE meridian.sector_applications \
               SET status = $2, decision_note = $3, reviewed_by = $4, reviewed_at = $5, \
                   verification_signature = $6, verification_alg = $7, verification_pubkey = $8, \
                   updated_at = now() \
             WHERE id = $1 RETURNING {APPLICATION_COLUMNS}, NULL::text AS applicant_name, \
             NULL::text AS sector_name, NULL::text AS sector_slug"
        ))
        .bind(application_id)
        .bind(new_status)
        .bind(decision_note)
        .bind(actor.id)
        .bind(issued_at)
        .bind(&signature)
        .bind(&alg)
        .bind(&pubkey)
        .fetch_one(&mut *tx)
        .await?;
        if approve {
            // The same membership row an accepted invitation would have created.
            sqlx::query(
                "INSERT INTO meridian.desk_memberships (desk_id, user_id, role, joined_at) \
                 VALUES ($1, $2, $3, now()) ON CONFLICT (desk_id, user_id) DO NOTHING",
            )
            .bind(app.desk_id)
            .bind(app.user_id)
            .bind(&app.role)
            .execute(&mut *tx)
            .await?;
        }
        audit(
            &mut *tx,
            actor.id,
            &format!("sector.application.{new_status}"),
            "desk",
            &app.desk_id.to_string(),
            None,
            None,
        )
        .await?;
        tx.commit().await?;
        Ok(updated)
    }

    /// Reconstructs an approved application's KYC certificate and re-verifies its
    /// ML-DSA signature against the stored public key.
    ///
    /// This is the tamper-evidence check: the canonical statement is rebuilt from
    /// the *current* stored row (applicant, sector, document count, decision,
    /// issue time), then verified against the stored signature. If any of those
    /// fields were altered after approval, `valid` comes back `false`.
    ///
    /// Returns `Ok(None)` when the application carries no certificate (still
    /// pending, or rejected).
    ///
    /// # Errors
    /// [`NewsroomError::NotFound`] if the application id is unknown; database failures.
    pub async fn certificate(
        &self,
        application_id: Uuid,
    ) -> Result<Option<CertificateView>, NewsroomError> {
        let app = sqlx::query_as::<_, SectorApplication>(&format!(
            "SELECT {APPLICATION_COLUMNS}, NULL::text AS applicant_name, NULL::text AS sector_name, \
             NULL::text AS sector_slug FROM meridian.sector_applications WHERE id = $1"
        ))
        .bind(application_id)
        .fetch_optional(self.pool())
        .await?
        .ok_or(NewsroomError::NotFound("Application"))?;

        let (Some(signature), Some(algorithm), Some(public_key)) = (
            app.verification_signature.clone(),
            app.verification_alg.clone(),
            app.verification_pubkey.clone(),
        ) else {
            return Ok(None);
        };

        let issued_str = app
            .reviewed_at
            .and_then(|at| at.format(&time::format_description::well_known::Rfc3339).ok())
            .unwrap_or_default();
        let document_count = app.kyc_documents.as_array().map_or(0, Vec::len);
        let message = crate::kyc_signing::certificate_message(
            &application_id.to_string(),
            &app.user_id.to_string(),
            &app.desk_id.to_string(),
            document_count,
            "approved",
            &issued_str,
        );
        let valid =
            crate::kyc_signing::verify_with_public_key(&public_key, &message, &signature);

        Ok(Some(CertificateView {
            application_id,
            user_id: app.user_id,
            desk_id: app.desk_id,
            decision: "approved".to_owned(),
            issued_at: app.reviewed_at,
            document_count,
            statement: String::from_utf8_lossy(&message).into_owned(),
            signature,
            algorithm,
            public_key,
            valid,
        }))
    }

    /// Every invitation for a desk; requires invite rights.
    ///
    /// # Errors
    /// [`NewsroomError::Forbidden`]; database failures.
    pub async fn invitations_for_desk(
        &self,
        actor: &Actor,
        desk: &Desk,
    ) -> Result<Vec<DeskInvitation>, NewsroomError> {
        self.require_can_invite(actor, desk).await?;
        Ok(sqlx::query_as::<_, DeskInvitation>(
            "SELECT * FROM meridian.desk_invitations WHERE desk_id = $1 ORDER BY created_at DESC",
        )
        .bind(desk.id)
        .fetch_all(self.pool())
        .await?)
    }

    /// Whether `actor` may view a desk: a global-dashboard holder sees all,
    /// otherwise only desks they belong to.
    ///
    /// # Errors
    /// Propagates database failures.
    pub async fn can_view_desk(&self, actor: &Actor, desk: &Desk) -> Result<bool, NewsroomError> {
        if authz::has(self.pool(), actor, "dashboard.view_global", None).await? {
            return Ok(true);
        }
        let member: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM meridian.desk_memberships WHERE desk_id = $1 AND user_id = $2)",
        )
        .bind(desk.id)
        .bind(actor.id)
        .fetch_one(self.pool())
        .await?;
        Ok(member)
    }

    /// Authorize a dashboard / search read that accepts a client-supplied `desk_id`,
    /// under the desk-private model — so a caller can't read another desk's editorial
    /// data by passing its id.
    ///
    /// - `dashboard.view_global` (or admin): always allowed — a `None` request is the
    ///   true newsroom-wide roll-up.
    /// - Otherwise a **specific** `desk_id` is allowed only if the viewer belongs to
    ///   it; the newsroom-wide (`None`) request is refused (it would expose every
    ///   desk) — the viewer must scope to a desk they're a member of, or hold
    ///   `dashboard.view_global`.
    ///
    /// The read may then run its queries unchanged: a permitted specific desk is
    /// already scoped by the query's own `desk_id` filter, and the only unscoped
    /// (`None`) path left open belongs to global viewers.
    ///
    /// # Errors
    /// [`NewsroomError::Forbidden`] for a non-member desk, or an org-wide request
    /// without `dashboard.view_global`; database failures.
    pub(crate) async fn require_desk_read(
        &self,
        actor: &Actor,
        desk_id: Option<Uuid>,
    ) -> Result<(), NewsroomError> {
        if authz::has(self.pool(), actor, "dashboard.view_global", None).await? {
            return Ok(());
        }
        let Some(requested) = desk_id else {
            return Err(NewsroomError::Forbidden {
                capability: "dashboard.view_global".to_owned(),
                reason: "The newsroom-wide view requires dashboard.view_global; scope to a desk you belong to".to_owned(),
            });
        };
        let member: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM meridian.desk_memberships WHERE desk_id = $1 AND user_id = $2)",
        )
        .bind(requested)
        .bind(actor.id)
        .fetch_one(self.pool())
        .await?;
        if !member {
            return Err(NewsroomError::Forbidden {
                capability: "dashboard.view_global".to_owned(),
                reason: "You do not belong to that desk".to_owned(),
            });
        }
        Ok(())
    }

    /// Aggregate analytics for a desk, optionally rolling up its sub-desks.
    /// A workflow state with no SLA row cannot breach: silence over an invented
    /// threshold.
    ///
    /// # Errors
    /// [`NewsroomError::NotFound`] for an unknown desk; database failures.
    pub async fn desk_analytics(
        &self,
        actor: &Actor,
        desk_id: Uuid,
        include_sub_desks: bool,
    ) -> Result<DeskAnalytics, NewsroomError> {
        // Validate the desk exists (404 otherwise) before aggregating, and confine
        // the read to members / `dashboard.view_global` — a desk's analytics are its
        // own, not every authenticated user's to read.
        let desk = self.get_desk(desk_id).await?;
        if !self.can_view_desk(actor, &desk).await? {
            return Err(NewsroomError::Forbidden {
                capability: "dashboard.view_global".to_owned(),
                reason: "You do not belong to that desk".to_owned(),
            });
        }
        let desk_ids = if include_sub_desks {
            self.descendant_ids(desk_id, true).await?
        } else {
            vec![desk_id]
        };

        let counts: Vec<(String, i64)> = sqlx::query_as(
            "SELECT workflow_state, count(*) FROM meridian.stories \
             WHERE desk_id = ANY($1) GROUP BY workflow_state",
        )
        .bind(&desk_ids)
        .fetch_all(self.pool())
        .await?;
        let mut stories_by_state = std::collections::BTreeMap::new();
        let mut total_stories = 0_i64;
        for (state, count) in counts {
            total_stories += count;
            stories_by_state.insert(state, count);
        }

        let open_tasks: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM meridian.tasks WHERE desk_id = ANY($1) AND status <> 'done'",
        )
        .bind(&desk_ids)
        .fetch_one(self.pool())
        .await?;
        let overdue_tasks: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM meridian.tasks \
             WHERE desk_id = ANY($1) AND status <> 'done' AND due_at < now()",
        )
        .bind(&desk_ids)
        .fetch_one(self.pool())
        .await?;
        let member_count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM meridian.desk_memberships WHERE desk_id = ANY($1)",
        )
        .bind(&desk_ids)
        .fetch_one(self.pool())
        .await?;

        let sla_breaches = self.sla_breaches(desk_id, &desk_ids).await?;

        Ok(DeskAnalytics {
            desk_id,
            included_desk_ids: desk_ids,
            stories_by_state,
            total_stories,
            open_tasks,
            overdue_tasks,
            member_count,
            sla_breaches,
            generated_at: OffsetDateTime::now_utc(),
        })
    }

    /// The states where the median current dwell has reached the desk's target.
    /// A state with no active SLA yields nothing.
    async fn sla_breaches(
        &self,
        desk_id: Uuid,
        desk_ids: &[Uuid],
    ) -> Result<Vec<SlaBreach>, NewsroomError> {
        let mut breaches = Vec::new();
        for sla in self.get_slas(desk_id).await? {
            if !sla.is_active {
                continue;
            }
            // Median hours stories currently sit in this state, from the open
            // dwell rows, so it reflects how long work has been waiting now.
            let median: Option<f64> = sqlx::query_scalar(
                "SELECT percentile_cont(0.5) WITHIN GROUP ( \
                   ORDER BY EXTRACT(EPOCH FROM (now() - wsh.entered_at)) / 3600.0) \
                 FROM meridian.workflow_state_history wsh \
                 JOIN meridian.stories s ON s.id = wsh.story_id \
                 WHERE wsh.exited_at IS NULL AND wsh.to_state = $1 AND s.desk_id = ANY($2)",
            )
            .bind(&sla.workflow_state)
            .bind(desk_ids)
            .fetch_one(self.pool())
            .await?;
            let Some(median) = median else { continue };
            let warn_at = sla.target_hours * (f64::from(sla.warn_at_percent) / 100.0);
            let status = if median >= sla.target_hours {
                "breached"
            } else if median >= warn_at {
                "at_risk"
            } else {
                continue;
            };
            breaches.push(SlaBreach {
                workflow_state: sla.workflow_state.clone(),
                target_hours: sla.target_hours,
                median_hours: (median * 100.0).round() / 100.0,
                status: status.to_owned(),
            });
        }
        Ok(breaches)
    }
}

/// Maps a unique-violation on desk name/slug to a domain conflict.
fn duplicate_desk(error: sqlx::Error) -> NewsroomError {
    if let sqlx::Error::Database(ref db_error) = error
        && db_error.is_unique_violation()
    {
        return NewsroomError::Conflict("A desk with that name or slug already exists".to_owned());
    }
    NewsroomError::Database(error)
}

/// Builds the desk subtree rooted at `root` from a flat descendant list,
/// children sorted by name.
fn build_tree(root: &Desk, all: &[Desk]) -> DeskTreeNode {
    let children: Vec<DeskTreeNode> = {
        let mut kids: Vec<&Desk> = all
            .iter()
            .filter(|desk| desk.parent_id == Some(root.id))
            .collect();
        kids.sort_by(|a, b| a.name.cmp(&b.name));
        kids.into_iter().map(|desk| build_tree(desk, all)).collect()
    };
    DeskTreeNode {
        id: root.id,
        name: root.name.clone(),
        slug: root.slug.clone(),
        is_archived: root.is_archived,
        children,
    }
}
