//! Sub-sectors (industries) — the second level of the sector taxonomy.
//!
//! A sub-sector is a *classification within a sector* (desk); see
//! `docs/SECTOR-TAXONOMY-AND-GOVERNANCE.md`. The desk stays the authorization
//! boundary, so managing a sector's sub-sectors requires desk-scoped `desks.manage`
//! (or being the desk lead) on the parent sector — exactly like its SLAs or
//! schedule. Listing is open (an industry directory); mutations are gated.

use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::{Actor, NewsroomError, NewsroomService, audit};

/// An industry within a sector.
#[derive(Debug, Clone, Serialize, FromRow)]
pub struct SubSector {
    pub id: Uuid,
    pub desk_id: Uuid,
    /// The parent node within the same sector, or `None` at the top level. Industries
    /// nest to arbitrary depth (Sector › Industry › Sub-industry › …).
    pub parent_id: Option<Uuid>,
    pub name: String,
    pub slug: String,
    pub description: Option<String>,
    pub position: i32,
    pub is_archived: bool,
    pub settings: serde_json::Value,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(skip_serializing)]
    pub updated_at: OffsetDateTime,
}

/// New sub-sector fields.
#[derive(Debug, Clone, Deserialize)]
pub struct SubSectorDraft {
    pub name: String,
    pub slug: String,
    pub description: Option<String>,
    pub position: Option<i32>,
    /// Optional parent node (must be in the same sector) — omit for a top-level
    /// industry, set to nest deeper.
    #[serde(default)]
    pub parent_id: Option<Uuid>,
}

/// A partial update; `None` leaves a field unchanged.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct SubSectorPatch {
    pub name: Option<String>,
    pub slug: Option<String>,
    pub description: Option<String>,
    pub position: Option<i32>,
    pub is_archived: Option<bool>,
}

impl NewsroomService {
    /// Lists a sector's sub-sectors, ordered by display position then name. Readable
    /// by any authenticated caller — an industry directory, like the sector one.
    ///
    /// # Errors
    /// Propagates database failures.
    pub async fn list_sub_sectors(
        &self,
        desk_id: Uuid,
        include_archived: bool,
    ) -> Result<Vec<SubSector>, NewsroomError> {
        let rows = sqlx::query_as::<_, SubSector>(
            "SELECT * FROM meridian.sub_sectors \
             WHERE desk_id = $1 AND ($2 OR NOT is_archived) \
             ORDER BY position, name",
        )
        .bind(desk_id)
        .bind(include_archived)
        .fetch_all(self.pool())
        .await?;
        Ok(rows)
    }

    /// Loads one sub-sector by id.
    ///
    /// # Errors
    /// [`NewsroomError::NotFound`] if absent; propagates database failures.
    pub async fn get_sub_sector(&self, id: Uuid) -> Result<SubSector, NewsroomError> {
        sqlx::query_as::<_, SubSector>("SELECT * FROM meridian.sub_sectors WHERE id = $1")
            .bind(id)
            .fetch_optional(self.pool())
            .await?
            .ok_or(NewsroomError::NotFound("Sub-sector"))
    }

    /// The taxonomy path for an industry: its ancestor chain ordered **root → node**
    /// (the node itself is last). Drives breadcrumbs and SEO structured data. Empty if
    /// the id is unknown. A depth cap keeps a malformed cycle from looping forever.
    ///
    /// # Errors
    /// Propagates database failures.
    pub async fn sub_sector_path(&self, id: Uuid) -> Result<Vec<SubSector>, NewsroomError> {
        let rows = sqlx::query_as::<_, SubSector>(
            "WITH RECURSIVE anc(id, desk_id, parent_id, name, slug, description, position, \
                               is_archived, settings, created_at, updated_at, depth) AS ( \
                 SELECT s.*, 0 FROM meridian.sub_sectors s WHERE s.id = $1 \
                 UNION ALL \
                 SELECT s.*, anc.depth + 1 FROM meridian.sub_sectors s \
                   JOIN anc ON s.id = anc.parent_id WHERE anc.depth < 32 \
             ) \
             SELECT id, desk_id, parent_id, name, slug, description, position, is_archived, \
                    settings, created_at, updated_at \
             FROM anc ORDER BY depth DESC",
        )
        .bind(id)
        .fetch_all(self.pool())
        .await?;
        Ok(rows)
    }

    /// Creates an industry under a sector. Requires desk-scoped `desks.manage` (or
    /// desk lead) on the parent sector.
    ///
    /// # Errors
    /// [`NewsroomError::Forbidden`] without the grant, [`NewsroomError::Conflict`] on
    /// a duplicate name/slug within the sector, [`NewsroomError::NotFound`] for an
    /// unknown sector; propagates database failures.
    pub async fn create_sub_sector(
        &self,
        actor: &Actor,
        desk_id: Uuid,
        draft: &SubSectorDraft,
    ) -> Result<SubSector, NewsroomError> {
        let desk = self.get_desk(desk_id).await?;
        self.require_desk_admin(actor, &desk).await?;
        // A parent node must be another industry in the same sector.
        if let Some(parent_id) = draft.parent_id {
            let parent = self.get_sub_sector(parent_id).await?;
            if parent.desk_id != desk_id {
                return Err(NewsroomError::Unprocessable(
                    "The parent industry must be in the same sector".to_owned(),
                ));
            }
        }
        let mut tx = self.pool().begin().await?;
        let sub = sqlx::query_as::<_, SubSector>(
            "INSERT INTO meridian.sub_sectors (id, desk_id, parent_id, name, slug, description, position) \
             VALUES ($1,$2,$3,$4,$5,$6,$7) RETURNING *",
        )
        .bind(Uuid::now_v7())
        .bind(desk_id)
        .bind(draft.parent_id)
        .bind(&draft.name)
        .bind(&draft.slug)
        .bind(draft.description.as_deref())
        .bind(draft.position.unwrap_or(0))
        .fetch_one(&mut *tx)
        .await
        .map_err(duplicate_sub_sector)?;
        audit(
            &mut *tx,
            actor.id,
            "subsector.created",
            "sub_sector",
            &sub.id.to_string(),
            None,
            Some(serde_json::json!({ "name": sub.name, "desk_id": desk_id })),
        )
        .await?;
        Self::enqueue_event(
            &mut *tx,
            "subsector.created",
            &sub.id.to_string(),
            Some(&desk.slug),
            serde_json::json!({ "id": sub.id, "desk_id": desk_id, "name": sub.name, "slug": sub.slug }),
            Some(actor.id),
        )
        .await?;
        tx.commit().await?;
        Ok(sub)
    }

    /// Updates an industry. Requires desk admin on its parent sector. Emits
    /// `subsector.archived` when the patch archives it, else `subsector.updated`.
    ///
    /// # Errors
    /// [`NewsroomError::Forbidden`] without the grant, [`NewsroomError::Conflict`] on
    /// a duplicate name/slug, [`NewsroomError::NotFound`] if absent; propagates
    /// database failures.
    pub async fn update_sub_sector(
        &self,
        actor: &Actor,
        id: Uuid,
        patch: &SubSectorPatch,
    ) -> Result<SubSector, NewsroomError> {
        let existing = self.get_sub_sector(id).await?;
        let desk = self.get_desk(existing.desk_id).await?;
        self.require_desk_admin(actor, &desk).await?;
        let mut tx = self.pool().begin().await?;
        let updated = sqlx::query_as::<_, SubSector>(
            "UPDATE meridian.sub_sectors SET \
               name = COALESCE($2, name), \
               slug = COALESCE($3, slug), \
               description = CASE WHEN $4 THEN $5 ELSE description END, \
               position = COALESCE($6, position), \
               is_archived = COALESCE($7, is_archived), \
               updated_at = now() \
             WHERE id = $1 RETURNING *",
        )
        .bind(id)
        .bind(patch.name.as_deref())
        .bind(patch.slug.as_deref())
        .bind(patch.description.is_some())
        .bind(patch.description.as_deref())
        .bind(patch.position)
        .bind(patch.is_archived)
        .fetch_one(&mut *tx)
        .await
        .map_err(duplicate_sub_sector)?;
        let event = if patch.is_archived == Some(true) {
            "subsector.archived"
        } else {
            "subsector.updated"
        };
        audit(
            &mut *tx,
            actor.id,
            event,
            "sub_sector",
            &id.to_string(),
            None,
            Some(serde_json::json!({ "name": updated.name })),
        )
        .await?;
        Self::enqueue_event(
            &mut *tx,
            event,
            &id.to_string(),
            Some(&desk.slug),
            serde_json::json!({ "id": id, "desk_id": desk.id, "name": updated.name }),
            Some(actor.id),
        )
        .await?;
        tx.commit().await?;
        Ok(updated)
    }

    /// Deletes an industry. Requires desk admin on its parent sector. Stories keep
    /// their `desk_id`; their `sub_sector_id` is nulled by the `ON DELETE SET NULL`.
    ///
    /// # Errors
    /// [`NewsroomError::Forbidden`] without the grant, [`NewsroomError::NotFound`] if
    /// absent; propagates database failures.
    pub async fn delete_sub_sector(&self, actor: &Actor, id: Uuid) -> Result<(), NewsroomError> {
        let existing = self.get_sub_sector(id).await?;
        let desk = self.get_desk(existing.desk_id).await?;
        self.require_desk_admin(actor, &desk).await?;
        let mut tx = self.pool().begin().await?;
        sqlx::query("DELETE FROM meridian.sub_sectors WHERE id = $1")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        audit(&mut *tx, actor.id, "subsector.deleted", "sub_sector", &id.to_string(), None, None).await?;
        Self::enqueue_event(
            &mut *tx,
            "subsector.deleted",
            &id.to_string(),
            Some(&desk.slug),
            serde_json::json!({ "id": id, "desk_id": desk.id }),
            Some(actor.id),
        )
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Validates that an industry belongs to a given sector (the story's desk),
    /// enforcing the "industry belongs to the story's sector" invariant. A `None`
    /// industry is always allowed (the industry is optional on a story).
    ///
    /// # Errors
    /// [`NewsroomError::Unprocessable`] if the industry is not in the sector,
    /// [`NewsroomError::NotFound`] for an unknown industry; propagates DB failures.
    pub async fn validate_sub_sector_for_desk(
        &self,
        sub_sector_id: Option<Uuid>,
        desk_id: Option<Uuid>,
    ) -> Result<Option<Uuid>, NewsroomError> {
        let Some(sub_id) = sub_sector_id else { return Ok(None) };
        let sub = self.get_sub_sector(sub_id).await?;
        if Some(sub.desk_id) != desk_id {
            return Err(NewsroomError::Unprocessable(
                "The industry must belong to the story's sector".to_owned(),
            ));
        }
        Ok(Some(sub_id))
    }
}

/// Maps a unique-violation on `(desk_id, name|slug)` to a domain conflict.
fn duplicate_sub_sector(error: sqlx::Error) -> NewsroomError {
    if let sqlx::Error::Database(ref db_error) = error
        && db_error.is_unique_violation()
    {
        return NewsroomError::Conflict(
            "An industry with that name or slug already exists in this sector".to_owned(),
        );
    }
    NewsroomError::Database(error)
}
