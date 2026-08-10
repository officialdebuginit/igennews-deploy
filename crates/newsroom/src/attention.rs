//! Module 8 (attention) — the computed attention queue and its per-user
//! acknowledge/snooze state. Ported from the legacy `dashboard/alerts.py`
//! detectors and `dashboard/attention.py`.
//!
//! Items are computed from current state, never stored, so the queue cannot go
//! stale; only the response (acknowledge/snooze) is persisted. A ported subset of
//! detectors: pending/overdue reviews, overdue tasks, open corrections, failed
//! releases. Snooze is time-bounded — there is no permanent dismissal.

use serde::Serialize;
use sqlx::FromRow;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::{
    Actor, NewsroomError, NewsroomService, audit, authz,
    awareness::{NotifyInput, notify},
};

/// One computed attention item, annotated with the viewer's response state.
#[derive(Clone, Debug, FromRow, Serialize)]
pub struct AttentionItem {
    // The item identity is the fingerprint (`{entity_type}:{id}`); legacy names it
    // `id` and the item type `type`.
    #[serde(rename = "id")]
    pub fingerprint: String,
    #[serde(rename = "type")]
    pub item_type: String,
    pub severity: String,
    pub title: String,
    pub description: String,
    pub entity_type: String,
    pub entity_id: String,
    pub entity_title: Option<String>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub due_at: Option<OffsetDateTime>,
    pub acknowledged: bool,
    #[serde(with = "time::serde::rfc3339::option")]
    pub acknowledged_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub snoozed_until: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub first_detected_at: Option<OffsetDateTime>,
    // `escalated_at` comes from the persisted attention state; `owner` and `desk`
    // are built in SQL from the row each detector already joins. `action` is the
    // computed open-entity link, filled in Rust after the query.
    #[sqlx(default)]
    #[serde(with = "time::serde::rfc3339::option")]
    pub escalated_at: Option<OffsetDateTime>,
    #[sqlx(default)]
    pub owner: Option<serde_json::Value>,
    #[sqlx(default)]
    pub desk: Option<serde_json::Value>,
    #[sqlx(default)]
    pub action: serde_json::Value,
}

/// The persisted acknowledge/snooze state for one item and user.
#[derive(Clone, Debug, FromRow, Serialize)]
pub struct AttentionState {
    pub fingerprint: String,
    // Read for FromRow but not part of the legacy attention-state contract.
    #[serde(skip_serializing)]
    pub user_id: Uuid,
    #[serde(with = "time::serde::rfc3339::option")]
    pub acknowledged_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub snoozed_until: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339")]
    pub first_detected_at: OffsetDateTime,
    #[serde(skip_serializing)]
    pub last_detected_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339::option")]
    pub escalated_at: Option<OffsetDateTime>,
}

// The detector union: each SELECT yields the AttentionItem shape. Filters are
// bound once ($1 desk, $2 owner) and applied per detector against its own story
// join. Titles/descriptions are composed in SQL to match the legacy wording.
//
// Each detector also yields the item's owner and desk. Both were previously
// filled as null "to match the shape"; every detector already joins a row that
// carries them, so they are read rather than invented. The desk in particular is
// what lets `assign_attention` / `escalate_attention` resolve their capability in
// the sector that owns the work instead of globally.
const DETECTORS: &str = "\
    SELECT 'review:' || r.id AS fingerprint, \
           CASE WHEN r.created_at <= now() - interval '48 hours' THEN 'review_overdue' ELSE 'review_pending' END AS item_type, \
           CASE WHEN r.created_at <= now() - interval '48 hours' THEN 'high' ELSE 'medium' END AS severity, \
           initcap(replace(r.kind, '_', ' ')) || ' review ' || \
             CASE WHEN r.created_at <= now() - interval '48 hours' THEN 'overdue' ELSE 'waiting' END AS title, \
           'Requested ' || to_char(r.created_at, 'DD Mon HH24:MI') AS description, \
           'review' AS entity_type, r.id::text AS entity_id, s.title AS entity_title, s.due_at AS due_at, \
           r.assigned_to_id AS owner_id, s.desk_id AS desk_id \
    FROM meridian.reviews r JOIN meridian.stories s ON s.id = r.story_id \
    WHERE r.decision = 'pending' \
      AND ($1::uuid IS NULL OR s.desk_id = $1) AND ($2::uuid IS NULL OR s.author_id = $2) \
    UNION ALL \
    SELECT 'task:' || t.id, 'task_overdue', \
           CASE WHEN t.priority = 'urgent' THEN 'high' ELSE 'medium' END, \
           'Task overdue', t.title, 'task', t.id::text, t.title, t.due_at, \
           t.assigned_to_id, t.desk_id \
    FROM meridian.tasks t \
    WHERE t.due_at < now() AND t.status NOT IN ('done', 'cancelled') \
      AND ($1::uuid IS NULL OR t.desk_id = $1) AND ($2::uuid IS NULL OR t.assigned_to_id = $2) \
    UNION ALL \
    SELECT 'correction:' || c.id, 'correction_open', \
           CASE WHEN c.classification IN ('retraction', 'legal_privacy_removal') THEN 'critical' ELSE 'high' END, \
           'Open ' || replace(c.classification, '_', ' '), c.description, 'correction', c.id::text, s.title, NULL::timestamptz, \
           c.owner_id, s.desk_id \
    FROM meridian.corrections c JOIN meridian.stories s ON s.id = c.story_id \
    WHERE c.status = 'open' \
      AND ($1::uuid IS NULL OR s.desk_id = $1) AND ($2::uuid IS NULL OR s.author_id = $2) \
    UNION ALL \
    SELECT 'release-failed:' || rel.id, 'release_failed', 'critical', \
           'Release failed', 'Release ' || rel.release_type || ' is ' || rel.status, \
           'release', rel.id::text, s.title, NULL::timestamptz, \
           rel.created_by_id, s.desk_id \
    FROM meridian.releases rel JOIN meridian.stories s ON s.id = rel.story_id \
    WHERE rel.status IN ('failed', 'partial_failure') \
      AND ($1::uuid IS NULL OR s.desk_id = $1) AND ($2::uuid IS NULL OR s.author_id = $2)";

impl NewsroomService {
    /// The attention queue for the viewer, most severe first. Snoozed items are
    /// hidden unless `include_snoozed`.
    ///
    /// # Errors
    /// Propagates database failures.
    pub async fn attention_queue(
        &self,
        actor: &Actor,
        desk_id: Option<Uuid>,
        owner_id: Option<Uuid>,
        include_snoozed: bool,
    ) -> Result<Vec<AttentionItem>, NewsroomError> {
        // Desk-private: a specific desk needs membership, the newsroom-wide queue
        // needs dashboard.view_global — otherwise the queue leaks other desks' story
        // titles, correction classifications, owners and due dates.
        self.require_desk_read(actor, desk_id).await?;
        // `escalated_at` is selected from the persisted state rather than left
        // null: the column has existed since migration 0013 and `escalate_attention`
        // has always written it — the queue simply never read it back, so an
        // escalated item looked identical to one nobody had touched.
        let query = format!(
            "WITH items AS ({DETECTORS}) \
             SELECT items.fingerprint, items.item_type, items.severity, items.title, \
                    items.description, items.entity_type, items.entity_id, items.entity_title, \
                    items.due_at, \
                    (a.acknowledged_at IS NOT NULL) AS acknowledged, a.acknowledged_at, \
                    a.snoozed_until, a.first_detected_at, a.escalated_at, \
                    CASE WHEN owner.id IS NULL THEN NULL ELSE \
                      jsonb_build_object('id', owner.id, 'display_name', owner.display_name) END AS owner, \
                    CASE WHEN d.id IS NULL THEN NULL ELSE \
                      jsonb_build_object('id', d.id, 'name', d.name, 'slug', d.slug) END AS desk \
             FROM items \
             LEFT JOIN meridian.attention_states a \
               ON a.fingerprint = items.fingerprint AND a.user_id = $3 \
             LEFT JOIN meridian.users owner ON owner.id = items.owner_id \
             LEFT JOIN meridian.desks d ON d.id = items.desk_id \
             WHERE $4 OR a.snoozed_until IS NULL OR a.snoozed_until <= now() \
             ORDER BY CASE items.severity \
                        WHEN 'critical' THEN 0 WHEN 'high' THEN 1 WHEN 'medium' THEN 2 ELSE 3 END, \
                      items.due_at ASC NULLS LAST"
        );
        let items = sqlx::query_as::<_, AttentionItem>(&query)
            .bind(desk_id)
            .bind(owner_id)
            .bind(actor.id)
            .bind(include_snoozed)
            .fetch_all(self.pool())
            .await?;
        // The open-entity link legacy attaches to each item.
        Ok(items
            .into_iter()
            .map(|mut item| {
                item.action = serde_json::json!({
                    "label": format!("Open {}", item.entity_type),
                    "href": format!("/{}s/{}", item.entity_type, item.entity_id),
                });
                item
            })
            .collect())
    }

    /// Acknowledges an item for the viewer.
    ///
    /// # Errors
    /// Propagates database failures.
    pub async fn acknowledge_attention(
        &self,
        actor: &Actor,
        fingerprint: &str,
    ) -> Result<AttentionState, NewsroomError> {
        Ok(sqlx::query_as::<_, AttentionState>(
            "INSERT INTO meridian.attention_states (fingerprint, user_id, acknowledged_at) \
             VALUES ($1, $2, now()) \
             ON CONFLICT (fingerprint, user_id) DO UPDATE SET \
               acknowledged_at = now(), last_detected_at = now() \
             RETURNING fingerprint, user_id, acknowledged_at, snoozed_until, first_detected_at, last_detected_at, escalated_at",
        )
        .bind(fingerprint)
        .bind(actor.id)
        .fetch_one(self.pool())
        .await?)
    }

    /// Clears the viewer's acknowledgement of an item.
    ///
    /// # Errors
    /// Propagates database failures.
    pub async fn unacknowledge_attention(
        &self,
        actor: &Actor,
        fingerprint: &str,
    ) -> Result<AttentionState, NewsroomError> {
        Ok(sqlx::query_as::<_, AttentionState>(
            "INSERT INTO meridian.attention_states (fingerprint, user_id, acknowledged_at) \
             VALUES ($1, $2, NULL) \
             ON CONFLICT (fingerprint, user_id) DO UPDATE SET \
               acknowledged_at = NULL, last_detected_at = now() \
             RETURNING fingerprint, user_id, acknowledged_at, snoozed_until, first_detected_at, last_detected_at, escalated_at",
        )
        .bind(fingerprint)
        .bind(actor.id)
        .fetch_one(self.pool())
        .await?)
    }

    /// Snoozes an item for the viewer until `until`.
    ///
    /// # Errors
    /// Propagates database failures.
    pub async fn snooze_attention(
        &self,
        actor: &Actor,
        fingerprint: &str,
        until: OffsetDateTime,
    ) -> Result<AttentionState, NewsroomError> {
        Ok(sqlx::query_as::<_, AttentionState>(
            "INSERT INTO meridian.attention_states (fingerprint, user_id, snoozed_until) \
             VALUES ($1, $2, $3) \
             ON CONFLICT (fingerprint, user_id) DO UPDATE SET \
               snoozed_until = $3, last_detected_at = now() \
             RETURNING fingerprint, user_id, acknowledged_at, snoozed_until, first_detected_at, last_detected_at, escalated_at",
        )
        .bind(fingerprint)
        .bind(actor.id)
        .bind(until)
        .fetch_one(self.pool())
        .await?)
    }

    /// Reassigns the entity behind an attention item (legacy `attention.assign`).
    /// Only review and task items carry an assignee; corrections and releases are
    /// workflow-owned and cannot be reassigned. Notifies the assignee and audits.
    ///
    /// # Errors
    /// [`NewsroomError::Forbidden`] without `attention.assign`;
    /// [`NewsroomError::NotFound`] for an unknown assignee;
    /// [`NewsroomError::Conflict`] for an item type with no owner; database failures.
    pub async fn assign_attention(
        &self,
        actor: &Actor,
        fingerprint: &str,
        assignee_id: Uuid,
    ) -> Result<AttentionState, NewsroomError> {
        // Scoped to the desk that owns the item, now that the detectors carry it.
        // Reassigning work inside Politics is a Politics decision.
        let desk = self.desk_for_attention(fingerprint).await?;
        authz::require(self.pool(), actor, "attention.assign", desk).await?;

        let assignee_exists: Option<Uuid> =
            sqlx::query_scalar("SELECT id FROM meridian.users WHERE id = $1")
                .bind(assignee_id)
                .fetch_optional(self.pool())
                .await?;
        if assignee_exists.is_none() {
            return Err(NewsroomError::NotFound("Assignee"));
        }

        // Move the underlying record; only review/task carry an assignee field.
        // Corrections and releases are owned by a workflow, so there is nothing
        // honest to write and the reassignment is refused (mirrors legacy 409).
        let (kind, raw) = fingerprint.split_once(':').unwrap_or((fingerprint, ""));
        let entity = Uuid::parse_str(raw).ok();
        let moved = match (kind, entity) {
            ("review", Some(id)) => {
                sqlx::query("UPDATE meridian.reviews SET assigned_to_id = $2 WHERE id = $1")
                    .bind(id)
                    .bind(assignee_id)
                    .execute(self.pool())
                    .await?
                    .rows_affected()
            }
            ("task", Some(id)) => sqlx::query(
                "UPDATE meridian.tasks SET assigned_to_id = $2, updated_at = now() WHERE id = $1",
            )
            .bind(id)
            .bind(assignee_id)
            .execute(self.pool())
            .await?
            .rows_affected(),
            _ => 0,
        };
        if moved == 0 {
            return Err(NewsroomError::Conflict(format!(
                "An item of type '{kind}' cannot be reassigned; open it and act on it directly."
            )));
        }

        let mut tx = self.pool().begin().await?;
        let state = sqlx::query_as::<_, AttentionState>(
            "INSERT INTO meridian.attention_states (fingerprint, user_id) VALUES ($1, $2) \
             ON CONFLICT (fingerprint, user_id) DO UPDATE SET last_detected_at = now() \
             RETURNING fingerprint, user_id, acknowledged_at, snoozed_until, first_detected_at, last_detected_at, escalated_at",
        )
        .bind(fingerprint)
        .bind(actor.id)
        .fetch_one(&mut *tx)
        .await?;
        notify(
            &mut tx,
            &NotifyInput {
                user_id: assignee_id,
                kind: "attention.assigned",
                title: &format!("You were assigned: {kind}"),
                body: None,
                entity_type: kind,
                entity_id: raw,
                priority: "high",
                group_key: None,
            },
        )
        .await?;
        audit(
            &mut *tx,
            actor.id,
            "attention.assigned",
            kind,
            raw,
            None,
            Some(serde_json::json!({ "fingerprint": fingerprint, "assignee_id": assignee_id })),
        )
        .await?;
        tx.commit().await?;
        Ok(state)
    }

    /// Escalates an attention item to a recipient, or the caller's desk lead when
    /// none is named (legacy `attention.escalate`). Notifies critically and audits.
    ///
    /// # Errors
    /// [`NewsroomError::Forbidden`] without `attention.escalate`;
    /// [`NewsroomError::Unprocessable`] when no recipient and no desk lead;
    /// [`NewsroomError::NotFound`] for an unknown recipient; database failures.
    pub async fn escalate_attention(
        &self,
        actor: &Actor,
        fingerprint: &str,
        to_user_id: Option<Uuid>,
    ) -> Result<AttentionState, NewsroomError> {
        let desk = self.desk_for_attention(fingerprint).await?;
        authz::require(self.pool(), actor, "attention.escalate", desk).await?;

        let target = if let Some(id) = to_user_id {
            id
        } else {
            // Escalation goes up the desk, not the role hierarchy: the desk lead
            // owns the stalled work.
            let lead: Option<Uuid> = sqlx::query_scalar(
                "SELECT d.lead_user_id FROM meridian.desks d \
                 JOIN meridian.desk_memberships m ON m.desk_id = d.id \
                 WHERE m.user_id = $1 AND d.lead_user_id IS NOT NULL AND d.lead_user_id <> $1 \
                 LIMIT 1",
            )
            .bind(actor.id)
            .fetch_optional(self.pool())
            .await?
            .flatten();
            lead.ok_or_else(|| {
                NewsroomError::Unprocessable(
                    "No desk lead to escalate to; name a recipient explicitly".to_owned(),
                )
            })?
        };

        let target_exists: Option<Uuid> =
            sqlx::query_scalar("SELECT id FROM meridian.users WHERE id = $1")
                .bind(target)
                .fetch_optional(self.pool())
                .await?;
        if target_exists.is_none() {
            return Err(NewsroomError::NotFound("Escalation recipient"));
        }

        let (kind, raw) = fingerprint.split_once(':').unwrap_or((fingerprint, ""));

        let mut tx = self.pool().begin().await?;
        let state = sqlx::query_as::<_, AttentionState>(
            "INSERT INTO meridian.attention_states (fingerprint, user_id, escalated_at) \
             VALUES ($1, $2, now()) \
             ON CONFLICT (fingerprint, user_id) DO UPDATE SET \
               escalated_at = now(), last_detected_at = now() \
             RETURNING fingerprint, user_id, acknowledged_at, snoozed_until, first_detected_at, last_detected_at, escalated_at",
        )
        .bind(fingerprint)
        .bind(actor.id)
        .fetch_one(&mut *tx)
        .await?;
        notify(
            &mut tx,
            &NotifyInput {
                user_id: target,
                kind: "escalation",
                title: &format!("Escalated to you: {kind}"),
                body: None,
                entity_type: kind,
                entity_id: raw,
                priority: "critical",
                group_key: None,
            },
        )
        .await?;
        audit(
            &mut *tx,
            actor.id,
            "attention.escalated",
            kind,
            raw,
            None,
            Some(serde_json::json!({ "fingerprint": fingerprint, "to": target })),
        )
        .await?;
        tx.commit().await?;
        Ok(state)
    }

    /// The desk owning the entity behind an attention fingerprint.
    ///
    /// Items are computed on read and never stored, so the fingerprint
    /// (`{kind}:{uuid}`) is the only handle a caller holds; this walks it back to a
    /// desk so the capability check can resolve in the right sector. `None` — an
    /// unparseable fingerprint, an unknown row, or a genuinely deskless one —
    /// falls back to an unscoped check, which is the stricter of the two outcomes
    /// because only a global grant can then answer.
    async fn desk_for_attention(&self, fingerprint: &str) -> Result<Option<Uuid>, NewsroomError> {
        let Some((kind, raw)) = fingerprint.split_once(':') else {
            return Ok(None);
        };
        let Ok(id) = raw.parse::<Uuid>() else {
            return Ok(None);
        };
        let sql = match kind {
            "review" => {
                "SELECT s.desk_id FROM meridian.stories s \
                 JOIN meridian.reviews r ON r.story_id = s.id WHERE r.id = $1"
            }
            "task" => "SELECT t.desk_id FROM meridian.tasks t WHERE t.id = $1",
            "correction" => {
                "SELECT s.desk_id FROM meridian.stories s \
                 JOIN meridian.corrections c ON c.story_id = s.id WHERE c.id = $1"
            }
            "release-failed" => {
                "SELECT s.desk_id FROM meridian.stories s \
                 JOIN meridian.releases rel ON rel.story_id = s.id WHERE rel.id = $1"
            }
            _ => return Ok(None),
        };
        Ok(sqlx::query_scalar(sql).bind(id).fetch_optional(self.pool()).await?.flatten())
    }
}
