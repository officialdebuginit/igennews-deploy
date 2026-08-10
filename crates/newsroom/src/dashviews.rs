//! Module 8 (views) — saved dashboard views. Ported from the legacy
//! `dashboard/views.py`. A view is private to its owner unless shared to a desk;
//! sharing requires the `dashboard.share_view` capability.

use serde::Serialize;
use sqlx::FromRow;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::{Actor, NewsroomError, NewsroomService, authz};

#[derive(Clone, Debug, FromRow, Serialize)]
pub struct DashboardView {
    pub id: Uuid,
    pub user_id: Uuid,
    pub desk_id: Option<Uuid>,
    pub name: String,
    pub description: Option<String>,
    pub is_default: bool,
    pub is_shared: bool,
    pub layout_json: serde_json::Value,
    pub filters_json: serde_json::Value,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
}

/// The layout version, matching the legacy `layouts.LAYOUT_VERSION`.
const LAYOUT_VERSION: i32 = 1;

/// The dashboard layout a caller should open onto (legacy `LayoutRead`): their
/// saved default view if they have one, otherwise the role baseline preset.
#[derive(Clone, Debug, Serialize)]
pub struct ResolvedLayout {
    pub layout_version: i32,
    /// `"saved_view"` when the caller has set a default, `"role_default"` otherwise.
    pub source: String,
    pub view_id: Option<Uuid>,
    pub view_name: Option<String>,
    pub role: String,
    pub widgets: serde_json::Value,
    pub filters: serde_json::Value,
}

/// Every widget the dashboard can render, and whether it is on by default.
///
/// This is the catalogue a saved view chooses from. It lists **only widgets the
/// frontend actually implements** — a catalogue that offers something unrenderable
/// is worse than a short one, because a saved view referencing it would silently
/// show nothing.
///
/// `drilldowns`, `saved_views` and `personal_shelf` were added on 2026-07-31 when
/// those widgets were built; the four before them predate that. The per-role
/// presets of legacy `dashboard/layouts.py` (imp1.md §19 — fact-checker
/// "high-risk claims", copy-editor "style issues", admin system health) still
/// depend on widgets that do not exist, and stay deferred rather than faked.
pub const WIDGET_CATALOGUE: [(&str, &str, bool); 7] = [
    ("summary_metrics", "Headline counts", true),
    ("attention_queue", "Attention queue", true),
    ("drilldowns", "What is behind the numbers", true),
    ("saved_views", "Saved views", true),
    ("personal_shelf", "Pinned and recently opened", true),
    ("pipeline", "Editorial pipeline", false),
    ("activity", "Recent activity", false),
];

/// One widget placement, in the 12-column grid shape the layout contract uses.
fn widget(kind: &str, y: i32, h: i32) -> serde_json::Value {
    serde_json::json!({
        "id": format!("{kind}-{y}"),
        "type": kind,
        "position": { "x": 0, "y": y, "w": 12, "h": h },
        "settings": {},
    })
}

/// A layout from a chosen set of widget types, in catalogue order.
///
/// Unknown types are dropped rather than passed through: the dashboard can only
/// render what it implements, and a placement it ignores would leave a gap the
/// person deliberately configured.
#[must_use]
pub fn layout_from_types(types: &[String]) -> serde_json::Value {
    let mut y = 0;
    let placements: Vec<serde_json::Value> = WIDGET_CATALOGUE
        .iter()
        .filter(|(kind, _, _)| types.iter().any(|chosen| chosen == kind))
        .map(|(kind, _, _)| {
            let placement = widget(kind, y, 4);
            y += 4;
            placement
        })
        .collect();
    serde_json::Value::Array(placements)
}

/// The universally-available widget baseline, in a 12-column grid — the
/// catalogue's default-on entries.
fn baseline_widgets() -> serde_json::Value {
    let defaults: Vec<String> = WIDGET_CATALOGUE
        .iter()
        .filter(|(_, _, on)| *on)
        .map(|(kind, _, _)| (*kind).to_owned())
        .collect();
    layout_from_types(&defaults)
}

/// New/updated dashboard-view input.
#[derive(Clone, Debug)]
pub struct DashboardViewInput {
    pub name: String,
    pub description: Option<String>,
    pub desk_id: Option<Uuid>,
    pub is_shared: bool,
    pub layout: serde_json::Value,
    pub filters: serde_json::Value,
}

impl NewsroomService {
    /// Views the caller may see: their own, plus views shared to a desk they
    /// belong to (or shared newsroom-wide).
    ///
    /// # Errors
    /// Propagates database failures.
    pub async fn list_dashboard_views(
        &self,
        actor: &Actor,
    ) -> Result<Vec<DashboardView>, NewsroomError> {
        Ok(sqlx::query_as::<_, DashboardView>(
            "SELECT * FROM meridian.dashboard_views \
             WHERE user_id = $1 \
                OR (is_shared AND (desk_id IS NULL OR desk_id IN ( \
                     SELECT desk_id FROM meridian.desk_memberships WHERE user_id = $1))) \
             ORDER BY name",
        )
        .bind(actor.id)
        .fetch_all(self.pool())
        .await?)
    }

    /// One view the caller may read, or not-found.
    ///
    /// # Errors
    /// [`NewsroomError::NotFound`]; database failures.
    pub async fn get_dashboard_view(
        &self,
        actor: &Actor,
        view_id: Uuid,
    ) -> Result<DashboardView, NewsroomError> {
        sqlx::query_as::<_, DashboardView>(
            "SELECT * FROM meridian.dashboard_views \
             WHERE id = $2 AND (user_id = $1 \
                OR (is_shared AND (desk_id IS NULL OR desk_id IN ( \
                     SELECT desk_id FROM meridian.desk_memberships WHERE user_id = $1))))",
        )
        .bind(actor.id)
        .bind(view_id)
        .fetch_optional(self.pool())
        .await?
        .ok_or(NewsroomError::NotFound("Dashboard view"))
    }

    /// Creates a view for the caller. Sharing requires `dashboard.share_view`.
    ///
    /// # Errors
    /// [`NewsroomError::Forbidden`]; database failures.
    pub async fn create_dashboard_view(
        &self,
        actor: &Actor,
        input: &DashboardViewInput,
    ) -> Result<DashboardView, NewsroomError> {
        if input.is_shared {
            authz::require(self.pool(), actor, "dashboard.share_view", input.desk_id).await?;
        }
        Ok(sqlx::query_as::<_, DashboardView>(
            "INSERT INTO meridian.dashboard_views \
             (id, user_id, desk_id, name, description, is_shared, layout_json, filters_json) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8) RETURNING *",
        )
        .bind(Uuid::now_v7())
        .bind(actor.id)
        .bind(input.desk_id)
        .bind(&input.name)
        .bind(&input.description)
        .bind(input.is_shared)
        .bind(&input.layout)
        .bind(&input.filters)
        .fetch_one(self.pool())
        .await?)
    }

    /// Updates a view the caller owns. Sharing requires `dashboard.share_view`.
    ///
    /// # Errors
    /// [`NewsroomError::Forbidden`]; [`NewsroomError::NotFound`]; database failures.
    pub async fn update_dashboard_view(
        &self,
        actor: &Actor,
        view_id: Uuid,
        input: &DashboardViewInput,
    ) -> Result<DashboardView, NewsroomError> {
        if input.is_shared {
            authz::require(self.pool(), actor, "dashboard.share_view", input.desk_id).await?;
        }
        sqlx::query_as::<_, DashboardView>(
            "UPDATE meridian.dashboard_views SET \
               name = $3, description = $4, desk_id = $5, is_shared = $6, \
               layout_json = $7, filters_json = $8, updated_at = now() \
             WHERE id = $2 AND user_id = $1 RETURNING *",
        )
        .bind(actor.id)
        .bind(view_id)
        .bind(&input.name)
        .bind(&input.description)
        .bind(input.desk_id)
        .bind(input.is_shared)
        .bind(&input.layout)
        .bind(&input.filters)
        .fetch_optional(self.pool())
        .await?
        .ok_or(NewsroomError::NotFound("Dashboard view"))
    }

    /// Deletes a view the caller owns.
    ///
    /// # Errors
    /// [`NewsroomError::NotFound`]; database failures.
    pub async fn delete_dashboard_view(
        &self,
        actor: &Actor,
        view_id: Uuid,
    ) -> Result<(), NewsroomError> {
        let deleted = sqlx::query(
            "DELETE FROM meridian.dashboard_views WHERE id = $1 AND user_id = $2",
        )
        .bind(view_id)
        .bind(actor.id)
        .execute(self.pool())
        .await?;
        if deleted.rows_affected() == 0 {
            return Err(NewsroomError::NotFound("Dashboard view"));
        }
        Ok(())
    }

    /// Marks one of the caller's views as their default, clearing any other.
    ///
    /// # Errors
    /// [`NewsroomError::NotFound`]; database failures.
    pub async fn set_default_dashboard_view(
        &self,
        actor: &Actor,
        view_id: Uuid,
    ) -> Result<DashboardView, NewsroomError> {
        let mut tx = self.pool().begin().await?;
        sqlx::query(
            "UPDATE meridian.dashboard_views SET is_default = false, updated_at = now() \
             WHERE user_id = $1 AND is_default",
        )
        .bind(actor.id)
        .execute(&mut *tx)
        .await?;
        let view = sqlx::query_as::<_, DashboardView>(
            "UPDATE meridian.dashboard_views SET is_default = true, updated_at = now() \
             WHERE id = $2 AND user_id = $1 RETURNING *",
        )
        .bind(actor.id)
        .bind(view_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(NewsroomError::NotFound("Dashboard view"))?;
        tx.commit().await?;
        Ok(view)
    }

    /// The layout the caller should open onto (legacy `/dashboard/layout`): their
    /// saved default view if set, otherwise their role's baseline preset.
    ///
    /// # Errors
    /// Propagates database failures.
    pub async fn resolve_layout(&self, actor: &Actor) -> Result<ResolvedLayout, NewsroomError> {
        let default = sqlx::query_as::<_, DashboardView>(
            "SELECT * FROM meridian.dashboard_views \
             WHERE user_id = $1 AND is_default LIMIT 1",
        )
        .bind(actor.id)
        .fetch_optional(self.pool())
        .await?;
        let role = actor.role.as_str().to_owned();
        Ok(match default {
            Some(view) => ResolvedLayout {
                layout_version: LAYOUT_VERSION,
                source: "saved_view".to_owned(),
                view_id: Some(view.id),
                view_name: Some(view.name),
                role,
                widgets: view.layout_json,
                filters: view.filters_json,
            },
            None => ResolvedLayout {
                layout_version: LAYOUT_VERSION,
                source: "role_default".to_owned(),
                view_id: None,
                view_name: None,
                role,
                widgets: baseline_widgets(),
                filters: serde_json::json!({}),
            },
        })
    }

    /// Clears the caller's default-view flag (legacy `/dashboard/layout/reset`).
    /// The saved view itself survives — "go back to the standard layout" must not
    /// destroy the one someone arranged.
    ///
    /// # Errors
    /// Propagates database failures.
    pub async fn clear_default_dashboard_view(
        &self,
        actor: &Actor,
    ) -> Result<(), NewsroomError> {
        sqlx::query(
            "UPDATE meridian.dashboard_views SET is_default = false, updated_at = now() \
             WHERE user_id = $1 AND is_default",
        )
        .bind(actor.id)
        .execute(self.pool())
        .await?;
        Ok(())
    }
}
