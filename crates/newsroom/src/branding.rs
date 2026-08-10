//! Per-workspace branding for the multi-tenant platform.
//!
//! A desk (workspace) may carry its own brand; a single org-default row
//! (`desk_id IS NULL`) is the platform fallback. Resolution at render time is
//! desk brand → org default → the built-in wire-service tokens (the client falls
//! back when this returns `None`). Colours are opaque CSS strings the client
//! injects as `--accent-brand` / `--brand-ink` overrides — no rebuild per brand.

use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::{Actor, NewsroomError, NewsroomService, audit, authz};

const COLS: &str = "id, desk_id, brand_name, logo_asset_id, logo_url, accent_color, \
                    ink_color, font_preset, updated_at";

/// A brand for a workspace context, serialized for the UI.
#[derive(Clone, Debug, FromRow, Serialize)]
pub struct WorkspaceBranding {
    pub id: Uuid,
    pub desk_id: Option<Uuid>,
    pub brand_name: Option<String>,
    pub logo_asset_id: Option<Uuid>,
    pub logo_url: Option<String>,
    pub accent_color: Option<String>,
    pub ink_color: Option<String>,
    pub font_preset: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
}

/// The mutable fields accepted when setting a brand.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct BrandingInput {
    pub brand_name: Option<String>,
    pub logo_url: Option<String>,
    pub accent_color: Option<String>,
    pub ink_color: Option<String>,
    pub font_preset: Option<String>,
}

impl NewsroomService {
    /// The org-default brand, or `None` when unset.
    ///
    /// # Errors
    /// Propagates database failures.
    pub async fn org_branding(&self) -> Result<Option<WorkspaceBranding>, NewsroomError> {
        Ok(sqlx::query_as::<_, WorkspaceBranding>(&format!(
            "SELECT {COLS} FROM meridian.workspace_branding WHERE desk_id IS NULL"
        ))
        .fetch_optional(self.pool())
        .await?)
    }

    /// The brand for a specific desk, or `None`.
    ///
    /// # Errors
    /// Propagates database failures.
    pub async fn desk_branding(
        &self,
        desk_id: Uuid,
    ) -> Result<Option<WorkspaceBranding>, NewsroomError> {
        Ok(sqlx::query_as::<_, WorkspaceBranding>(&format!(
            "SELECT {COLS} FROM meridian.workspace_branding WHERE desk_id = $1"
        ))
        .bind(desk_id)
        .fetch_optional(self.pool())
        .await?)
    }

    /// The brand to render for a context: the desk's own brand if set, else the org
    /// default, else `None` (the client uses the built-in tokens).
    ///
    /// # Errors
    /// Propagates database failures.
    pub async fn resolve_branding(
        &self,
        desk_id: Option<Uuid>,
    ) -> Result<Option<WorkspaceBranding>, NewsroomError> {
        if let Some(id) = desk_id
            && let Some(brand) = self.desk_branding(id).await?
        {
            return Ok(Some(brand));
        }
        self.org_branding().await
    }

    /// Sets the org-default brand. Org-wide, so gated on `permissions.manage`.
    ///
    /// # Errors
    /// [`NewsroomError::Forbidden`]; database failures.
    pub async fn set_org_branding(
        &self,
        actor: &Actor,
        input: &BrandingInput,
    ) -> Result<WorkspaceBranding, NewsroomError> {
        authz::require(self.pool(), actor, "permissions.manage", None).await?;
        self.upsert_branding(actor, None, input).await
    }

    /// Sets a desk's brand. Requires `desks.manage` in that desk.
    ///
    /// # Errors
    /// [`NewsroomError::Forbidden`]; database failures.
    pub async fn set_desk_branding(
        &self,
        actor: &Actor,
        desk_id: Uuid,
        input: &BrandingInput,
    ) -> Result<WorkspaceBranding, NewsroomError> {
        authz::require(self.pool(), actor, "desks.manage", Some(desk_id)).await?;
        self.upsert_branding(actor, Some(desk_id), input).await
    }

    async fn upsert_branding(
        &self,
        actor: &Actor,
        desk_id: Option<Uuid>,
        input: &BrandingInput,
    ) -> Result<WorkspaceBranding, NewsroomError> {
        let mut tx = self.pool().begin().await?;
        let existing: Option<Uuid> = match desk_id {
            Some(id) => {
                sqlx::query_scalar("SELECT id FROM meridian.workspace_branding WHERE desk_id = $1")
                    .bind(id)
                    .fetch_optional(&mut *tx)
                    .await?
            }
            None => {
                sqlx::query_scalar(
                    "SELECT id FROM meridian.workspace_branding WHERE desk_id IS NULL",
                )
                .fetch_optional(&mut *tx)
                .await?
            }
        };
        let brand = if let Some(bid) = existing {
            sqlx::query_as::<_, WorkspaceBranding>(&format!(
                "UPDATE meridian.workspace_branding \
                   SET brand_name = $2, logo_url = $3, accent_color = $4, ink_color = $5, \
                       font_preset = $6, updated_by = $7, updated_at = now() \
                 WHERE id = $1 RETURNING {COLS}"
            ))
            .bind(bid)
            .bind(&input.brand_name)
            .bind(&input.logo_url)
            .bind(&input.accent_color)
            .bind(&input.ink_color)
            .bind(&input.font_preset)
            .bind(actor.id)
            .fetch_one(&mut *tx)
            .await?
        } else {
            sqlx::query_as::<_, WorkspaceBranding>(&format!(
                "INSERT INTO meridian.workspace_branding \
                   (id, desk_id, brand_name, logo_url, accent_color, ink_color, font_preset, updated_by) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8) RETURNING {COLS}"
            ))
            .bind(Uuid::now_v7())
            .bind(desk_id)
            .bind(&input.brand_name)
            .bind(&input.logo_url)
            .bind(&input.accent_color)
            .bind(&input.ink_color)
            .bind(&input.font_preset)
            .bind(actor.id)
            .fetch_one(&mut *tx)
            .await?
        };
        audit(
            &mut *tx,
            actor.id,
            "branding.updated",
            "workspace_branding",
            &brand.id.to_string(),
            None,
            Some(serde_json::json!({ "desk_id": desk_id, "accent_color": input.accent_color })),
        )
        .await?;
        tx.commit().await?;
        Ok(brand)
    }
}
