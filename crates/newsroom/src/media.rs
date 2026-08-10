//! Module 9 — media and publishing: asset metadata, releases (with the publish
//! gate), and corrections. Ported from the legacy `api.py` media/publishing
//! handlers and `services.publish_release`.
//!
//! Deferred (the worker/object-store pieces): `RustFS` byte upload + presigned
//! URLs, the scheduled-publish pass with attempt/retry rows, and orphan cleanup.
//! Externally-referenced assets (register-by-URI) are fully supported here.

use serde::Serialize;
use sqlx::FromRow;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::{Actor, NewsroomError, NewsroomService, authz};

const CORRECTION_CLASSES: [&str; 5] = [
    "presentation_fix",
    "clarification",
    "correction",
    "retraction",
    "legal_privacy_removal",
];

#[derive(Clone, Debug, FromRow, Serialize)]
pub struct Asset {
    pub id: Uuid,
    pub story_id: Option<Uuid>,
    pub kind: String,
    pub uri: String,
    pub bucket: Option<String>,
    pub object_key: Option<String>,
    pub filename: Option<String>,
    pub byte_size: Option<i64>,
    pub width: Option<i32>,
    pub height: Option<i32>,
    pub checksum: Option<String>,
    pub creator: Option<String>,
    pub source: Option<String>,
    pub rights: Option<String>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub rights_expires_at: Option<OffsetDateTime>,
    pub credit: Option<String>,
    pub caption: Option<String>,
    pub alt_text: Option<String>,
    pub verification_status: String,
    pub c2pa_data: serde_json::Value,
    /// The Drive folder this asset is filed under, or `None` for the root
    /// ("Unfiled"). `ON DELETE SET NULL`, so deleting a folder re-files here.
    #[sqlx(default)]
    pub folder_id: Option<Uuid>,
    pub created_by_id: Uuid,
    /// Display name of the uploader, joined from `users` by the list query — the
    /// "Added by" column in the file manager. `#[sqlx(default)]` so the plain
    /// `SELECT *` paths (get_asset, RETURNING *) leave it `None`.
    #[sqlx(default)]
    pub created_by_name: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    // Read for internal use but not part of the legacy asset contract, so not
    // serialized.
    #[serde(skip_serializing)]
    pub updated_at: OffsetDateTime,
    // Legacy exposes `download_url` on every asset — null in the list view (a
    // presigned URL is minted on demand by `GET /assets/{id}/download`). Not a
    // stored column, so `FromRow` defaults it to `None`.
    #[sqlx(default)]
    pub download_url: Option<String>,
}

/// A media object that has been uploaded to the object store; its bytes live at
/// `bucket`/`object_key`.
#[derive(Clone, Debug)]
pub struct StoredAsset {
    pub story_id: Option<Uuid>,
    pub kind: String,
    pub uri: String,
    pub bucket: String,
    pub object_key: String,
    pub filename: Option<String>,
    pub byte_size: i64,
    pub checksum: String,
    pub alt_text: Option<String>,
}

/// New externally-referenced asset (`bucket`/`object_key` stay null).
#[derive(Clone, Debug)]
pub struct AssetDraft {
    pub story_id: Option<Uuid>,
    pub kind: String,
    pub uri: String,
    pub filename: Option<String>,
    pub creator: Option<String>,
    pub source: Option<String>,
    pub rights: Option<String>,
    pub credit: Option<String>,
    pub caption: Option<String>,
    pub alt_text: Option<String>,
}

/// A Drive folder — a lightweight named container for organising assets. Carries
/// a live `asset_count` (assets filed directly under it) so the sidebar can show
/// counts without a second round trip.
#[derive(Clone, Debug, FromRow, Serialize)]
pub struct DriveFolder {
    pub id: Uuid,
    pub name: String,
    pub parent_id: Option<Uuid>,
    #[sqlx(default)]
    pub asset_count: i64,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

#[derive(Clone, Debug, FromRow, Serialize)]
pub struct Release {
    pub id: Uuid,
    pub story_id: Uuid,
    pub version_id: Uuid,
    pub release_type: String,
    pub status: String,
    pub channels: serde_json::Value,
    pub receipts: serde_json::Value,
    /// Per-channel send times: `{ "newsletter": "2026-08-05T12:00:00Z" }`. A
    /// channel absent here uses `scheduled_at`; an empty map is the atomic default.
    #[serde(default)]
    pub channel_schedule: serde_json::Value,
    #[serde(with = "time::serde::rfc3339::option")]
    pub scheduled_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub published_at: Option<OffsetDateTime>,
    /// Hold-until: published and distributed, but not showable before this.
    /// Distinct from `scheduled_at`, which is when it goes out at all.
    #[serde(with = "time::serde::rfc3339::option")]
    pub embargo_until: Option<OffsetDateTime>,
    /// Take-down: `process_due_releases` unpublishes the story once this passes.
    #[serde(with = "time::serde::rfc3339::option")]
    pub expires_at: Option<OffsetDateTime>,
    pub correction_of_id: Option<Uuid>,
    pub created_by_id: Uuid,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

/// A published story reduced to what a syndication feed / sitemap needs.
#[derive(Clone, Debug, FromRow, Serialize)]
pub struct SyndicationItem {
    pub slug: String,
    pub title: String,
    pub dek: String,
    pub published_at: Option<OffsetDateTime>,
}

/// A published-article summary for the headless list API — `SyndicationItem` plus the
/// section, so a consumer can filter and display without a second fetch.
#[derive(Clone, Debug, FromRow, Serialize)]
pub struct PublishedSummary {
    pub slug: String,
    pub title: String,
    pub dek: String,
    pub category: String,
    pub published_at: Option<OffsetDateTime>,
}

/// A published story with the fields an IPTC **ninjs** (News-in-JSON) item needs.
/// `author_name` is joined from `users`. Rights are per-story overrides (each
/// `None` when the story keeps the outlet default), resolved against the outlet
/// baseline by the serializer.
#[derive(Clone, Debug, FromRow)]
pub struct NinjsStory {
    pub slug: String,
    pub title: String,
    pub dek: String,
    pub body: serde_json::Value,
    pub category: String,
    /// Free-form tag array; Media Topic qcodes are stored here and lifted into
    /// `subject` entries on export.
    pub tags: serde_json::Value,
    #[sqlx(default)]
    pub author_name: Option<String>,
    pub rights_holder: Option<String>,
    pub rights_notice: Option<String>,
    pub usage_terms: Option<String>,
    pub license_url: Option<String>,
    pub reuse_embargo_until: Option<OffsetDateTime>,
    pub territory_mode: String,
    pub territories: Option<String>,
    /// IPTC Digital Source Type code (AI-disclosure); lifted into the feed as
    /// `digitalsourcetype` (ninjs) / `<digitalSourceType>` (NewsML-G2).
    pub digital_source_type: Option<String>,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
    pub published_at: Option<OffsetDateTime>,
}

/// One published story, with everything the public reader needs, fetched with no
/// authentication. `author_name` is joined from `users`; `id` lets the caller pull
/// the story's public corrections. Only `publication_state = 'published'` rows are
/// ever returned, so a draft can never leak through this surface.
#[derive(Clone, Debug, FromRow, Serialize)]
pub struct PublicArticle {
    pub id: Uuid,
    pub slug: String,
    pub title: String,
    pub dek: String,
    pub category: String,
    /// The sector (desk) and industry (sub-sector) the story is filed under — the
    /// taxonomy path, resolved into breadcrumbs + structured data on the public page.
    pub desk_id: Option<Uuid>,
    pub sub_sector_id: Option<Uuid>,
    pub body: serde_json::Value,
    pub tags: serde_json::Value,
    #[sqlx(default)]
    pub author_name: Option<String>,
    pub rights_holder: Option<String>,
    pub rights_notice: Option<String>,
    pub usage_terms: Option<String>,
    pub license_url: Option<String>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub reuse_embargo_until: Option<OffsetDateTime>,
    pub territory_mode: String,
    pub territories: Option<String>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub published_at: Option<OffsetDateTime>,
}

/// New-release input.
#[derive(Clone, Debug)]
pub struct ReleaseDraft {
    pub version_id: Uuid,
    pub channels: serde_json::Value,
    pub scheduled_at: Option<OffsetDateTime>,
    pub embargo_until: Option<OffsetDateTime>,
    pub expires_at: Option<OffsetDateTime>,
}

#[derive(Clone, Debug, FromRow, Serialize)]
pub struct Correction {
    pub id: Uuid,
    pub story_id: Uuid,
    pub release_id: Uuid,
    pub reported_by_id: Uuid,
    pub owner_id: Option<Uuid>,
    pub classification: String,
    pub status: String,
    pub description: String,
    pub public_note: Option<String>,
    pub resolved_release_id: Option<Uuid>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub resolved_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(skip_serializing)]
    pub updated_at: OffsetDateTime,
}

/// A distribution channel the newsroom can target.
#[derive(Clone, Debug, FromRow, Serialize)]
pub struct Channel {
    pub id: Uuid,
    pub key: String,
    pub name: String,
    pub kind: String,
    pub description: Option<String>,
    pub is_active: bool,
    pub config: serde_json::Value,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(skip_serializing)]
    pub updated_at: OffsetDateTime,
}

/// New or updated channel.
#[derive(Clone, Debug)]
pub struct ChannelInput {
    pub key: String,
    pub name: String,
    pub kind: String,
    pub description: Option<String>,
    pub is_active: bool,
    /// Per-kind delivery configuration (e.g. `{"webhook_url": "https://…"}`). A
    /// channel carrying a `webhook_url` is delivered to on publish.
    pub config: serde_json::Value,
}

/// The channel kinds the registry accepts, matching the `channels_kind_check`
/// constraint in migration `0020`. A kind decides how delivery would work, so an
/// unknown one is not deliverable by anything.
pub const CHANNEL_KINDS: [&str; 5] =
    ["web", "app_push", "newsletter", "social", "syndication"];

/// One front-page position and whatever occupies it.
///
/// `story_title` is joined rather than stored: the slot references a story, and
/// duplicating its headline here would let the page show a title the story no
/// longer has.
#[derive(Clone, Debug, FromRow, Serialize)]
pub struct FrontPageSlot {
    pub id: Uuid,
    pub position: i32,
    pub label: String,
    pub story_id: Option<Uuid>,
    pub story_title: Option<String>,
    pub story_slug: Option<String>,
    pub is_pinned: bool,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
}

/// New-correction input.
#[derive(Clone, Debug)]
pub struct CorrectionDraft {
    pub release_id: Uuid,
    pub classification: String,
    pub description: String,
    pub owner_id: Option<Uuid>,
    pub public_note: Option<String>,
}

impl NewsroomService {
    /// Every Drive folder, alphabetically, each with a live count of the assets
    /// filed directly under it.
    ///
    /// # Errors
    /// Propagates database failures.
    pub async fn list_folders(&self) -> Result<Vec<DriveFolder>, NewsroomError> {
        Ok(sqlx::query_as::<_, DriveFolder>(
            "SELECT f.id, f.name, f.parent_id, \
                    COUNT(a.id)::bigint AS asset_count, f.created_at \
             FROM meridian.drive_folders f \
             LEFT JOIN meridian.assets a ON a.folder_id = f.id \
             GROUP BY f.id \
             ORDER BY lower(f.name)",
        )
        .fetch_all(self.pool())
        .await?)
    }

    /// Creates a Drive folder. `parent_id`, when given, must be an existing folder.
    ///
    /// # Errors
    /// [`NewsroomError::Unprocessable`] for an empty name or unknown parent;
    /// database failures.
    pub async fn create_folder(
        &self,
        actor: &Actor,
        name: &str,
        parent_id: Option<Uuid>,
    ) -> Result<DriveFolder, NewsroomError> {
        let name = name.trim();
        if name.is_empty() {
            return Err(NewsroomError::Unprocessable("A folder needs a name".to_owned()));
        }
        if let Some(parent) = parent_id {
            let exists: Option<Uuid> =
                sqlx::query_scalar("SELECT id FROM meridian.drive_folders WHERE id = $1")
                    .bind(parent)
                    .fetch_optional(self.pool())
                    .await?;
            if exists.is_none() {
                return Err(NewsroomError::Unprocessable("Parent folder not found".to_owned()));
            }
        }
        let id = Uuid::now_v7();
        let mut tx = self.pool().begin().await?;
        sqlx::query(
            "INSERT INTO meridian.drive_folders (id, name, parent_id, created_by) \
             VALUES ($1, $2, $3, $4)",
        )
        .bind(id)
        .bind(name)
        .bind(parent_id)
        .bind(actor.id)
        .execute(&mut *tx)
        .await?;
        crate::audit(
            &mut *tx,
            actor.id,
            "folder.created",
            "drive_folder",
            &id.to_string(),
            None,
            Some(serde_json::json!({ "name": name, "parent_id": parent_id })),
        )
        .await?;
        Self::enqueue_event(
            &mut *tx,
            "folder.created",
            &id.to_string(),
            None,
            serde_json::json!({ "folder_id": id, "name": name, "parent_id": parent_id }),
            Some(actor.id),
        )
        .await?;
        tx.commit().await?;
        // Fresh folder, so the count is zero — read it back for the created_at.
        let folder = sqlx::query_as::<_, DriveFolder>(
            "SELECT id, name, parent_id, 0::bigint AS asset_count, created_at \
             FROM meridian.drive_folders WHERE id = $1",
        )
        .bind(id)
        .fetch_one(self.pool())
        .await?;
        Ok(folder)
    }

    /// Renames a Drive folder.
    ///
    /// # Errors
    /// [`NewsroomError::Unprocessable`] for an empty name;
    /// [`NewsroomError::NotFound`]; database failures.
    pub async fn rename_folder(
        &self,
        actor: &Actor,
        folder_id: Uuid,
        name: &str,
    ) -> Result<DriveFolder, NewsroomError> {
        let name = name.trim();
        if name.is_empty() {
            return Err(NewsroomError::Unprocessable("A folder needs a name".to_owned()));
        }
        let mut tx = self.pool().begin().await?;
        let updated = sqlx::query(
            "UPDATE meridian.drive_folders SET name = $2 WHERE id = $1",
        )
        .bind(folder_id)
        .bind(name)
        .execute(&mut *tx)
        .await?;
        if updated.rows_affected() == 0 {
            return Err(NewsroomError::NotFound("Folder"));
        }
        crate::audit(
            &mut *tx,
            actor.id,
            "folder.renamed",
            "drive_folder",
            &folder_id.to_string(),
            None,
            Some(serde_json::json!({ "name": name })),
        )
        .await?;
        Self::enqueue_event(
            &mut *tx,
            "folder.renamed",
            &folder_id.to_string(),
            None,
            serde_json::json!({ "folder_id": folder_id, "name": name }),
            Some(actor.id),
        )
        .await?;
        tx.commit().await?;
        let folder = sqlx::query_as::<_, DriveFolder>(
            "SELECT f.id, f.name, f.parent_id, \
                    COUNT(a.id)::bigint AS asset_count, f.created_at \
             FROM meridian.drive_folders f \
             LEFT JOIN meridian.assets a ON a.folder_id = f.id \
             WHERE f.id = $1 GROUP BY f.id",
        )
        .bind(folder_id)
        .fetch_one(self.pool())
        .await?;
        Ok(folder)
    }

    /// Deletes a Drive folder. Its assets are re-filed to the root (the DB
    /// `ON DELETE SET NULL`), and any subfolders cascade away.
    ///
    /// # Errors
    /// [`NewsroomError::NotFound`]; database failures.
    pub async fn delete_folder(&self, actor: &Actor, folder_id: Uuid) -> Result<(), NewsroomError> {
        let mut tx = self.pool().begin().await?;
        let deleted = sqlx::query("DELETE FROM meridian.drive_folders WHERE id = $1")
            .bind(folder_id)
            .execute(&mut *tx)
            .await?;
        if deleted.rows_affected() == 0 {
            return Err(NewsroomError::NotFound("Folder"));
        }
        crate::audit(
            &mut *tx,
            actor.id,
            "folder.deleted",
            "drive_folder",
            &folder_id.to_string(),
            None,
            None,
        )
        .await?;
        Self::enqueue_event(
            &mut *tx,
            "folder.deleted",
            &folder_id.to_string(),
            None,
            serde_json::json!({ "folder_id": folder_id }),
            Some(actor.id),
        )
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Moves an asset into a folder, or to the root when `folder_id` is `None`.
    ///
    /// # Errors
    /// [`NewsroomError::Unprocessable`] for an unknown target folder;
    /// [`NewsroomError::NotFound`]; database failures.
    pub async fn set_asset_folder(
        &self,
        actor: &Actor,
        asset_id: Uuid,
        folder_id: Option<Uuid>,
    ) -> Result<Asset, NewsroomError> {
        if let Some(target) = folder_id {
            let exists: Option<Uuid> =
                sqlx::query_scalar("SELECT id FROM meridian.drive_folders WHERE id = $1")
                    .bind(target)
                    .fetch_optional(self.pool())
                    .await?;
            if exists.is_none() {
                return Err(NewsroomError::Unprocessable("Target folder not found".to_owned()));
            }
        }
        let mut tx = self.pool().begin().await?;
        let updated = sqlx::query_as::<_, Asset>(
            "UPDATE meridian.assets SET folder_id = $2, updated_at = now() \
             WHERE id = $1 RETURNING *",
        )
        .bind(asset_id)
        .bind(folder_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(NewsroomError::NotFound("Asset"))?;
        crate::audit(
            &mut *tx,
            actor.id,
            "asset.moved",
            "asset",
            &asset_id.to_string(),
            None,
            Some(serde_json::json!({ "folder_id": folder_id })),
        )
        .await?;
        Self::enqueue_event(
            &mut *tx,
            "asset.moved",
            &asset_id.to_string(),
            None,
            serde_json::json!({ "asset_id": asset_id, "folder_id": folder_id }),
            Some(actor.id),
        )
        .await?;
        tx.commit().await?;
        Ok(updated)
    }

    /// Lists assets, optionally scoped to a story, newest first.
    ///
    /// # Errors
    /// Propagates database failures.
    pub async fn list_assets(&self, story_id: Option<Uuid>) -> Result<Vec<Asset>, NewsroomError> {
        // `a.*` plus the joined uploader name; FromRow maps the extra column into
        // `created_by_name` (the file-manager "Added by").
        Ok(sqlx::query_as::<_, Asset>(
            "SELECT a.*, u.display_name AS created_by_name \
             FROM meridian.assets a \
             LEFT JOIN meridian.users u ON u.id = a.created_by_id \
             WHERE ($1::uuid IS NULL OR a.story_id = $1) \
             ORDER BY a.created_at DESC",
        )
        .bind(story_id)
        .fetch_all(self.pool())
        .await?)
    }

    /// Renames an asset — sets a new display filename. The stored object key is
    /// untouched (it is an opaque content address); only the human-facing name
    /// changes, exactly as a file manager renames a file without moving its bytes.
    ///
    /// # Errors
    /// [`NewsroomError::Unprocessable`] for an empty name; [`NewsroomError::NotFound`];
    /// database failures.
    pub async fn rename_asset(
        &self,
        actor: &Actor,
        asset_id: Uuid,
        filename: &str,
    ) -> Result<Asset, NewsroomError> {
        let filename = filename.trim();
        if filename.is_empty() {
            return Err(NewsroomError::Unprocessable("A file needs a name".to_owned()));
        }
        let mut tx = self.pool().begin().await?;
        let updated = sqlx::query_as::<_, Asset>(
            "UPDATE meridian.assets SET filename = $2, updated_at = now() \
             WHERE id = $1 RETURNING *",
        )
        .bind(asset_id)
        .bind(filename)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(NewsroomError::NotFound("Asset"))?;
        crate::audit(
            &mut *tx,
            actor.id,
            "asset.renamed",
            "asset",
            &asset_id.to_string(),
            None,
            Some(serde_json::json!({ "filename": filename })),
        )
        .await?;
        Self::enqueue_event(
            &mut *tx,
            "asset.renamed",
            &asset_id.to_string(),
            None,
            serde_json::json!({ "asset_id": asset_id, "filename": filename }),
            Some(actor.id),
        )
        .await?;
        tx.commit().await?;
        Ok(updated)
    }

    /// Registers an externally-referenced asset by URI.
    ///
    /// # Errors
    /// Propagates database failures.
    pub async fn create_asset(
        &self,
        actor: &Actor,
        draft: &AssetDraft,
    ) -> Result<Asset, NewsroomError> {
        let mut tx = self.pool().begin().await?;
        let asset = sqlx::query_as::<_, Asset>(
            "INSERT INTO meridian.assets \
             (id, story_id, kind, uri, filename, creator, source, rights, credit, caption, alt_text, created_by_id) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12) RETURNING *",
        )
        .bind(Uuid::now_v7())
        .bind(draft.story_id)
        .bind(&draft.kind)
        .bind(&draft.uri)
        .bind(&draft.filename)
        .bind(&draft.creator)
        .bind(&draft.source)
        .bind(&draft.rights)
        .bind(&draft.credit)
        .bind(&draft.caption)
        .bind(&draft.alt_text)
        .bind(actor.id)
        .fetch_one(&mut *tx)
        .await?;
        crate::audit(
            &mut *tx,
            actor.id,
            "asset.created",
            "asset",
            &asset.id.to_string(),
            None,
            Some(serde_json::json!({ "kind": asset.kind, "story_id": asset.story_id })),
        )
        .await?;
        Self::enqueue_event(
            &mut *tx,
            "asset.created",
            &asset.id.to_string(),
            None,
            serde_json::json!({ "asset_id": asset.id, "kind": asset.kind, "story_id": asset.story_id }),
            Some(actor.id),
        )
        .await?;
        tx.commit().await?;
        Ok(asset)
    }

    /// Records an asset whose bytes were just uploaded to the object store. The
    /// object metadata is only committed after the object exists (gate #6).
    ///
    /// # Errors
    /// Propagates database failures.
    pub async fn create_uploaded_asset(
        &self,
        actor: &Actor,
        stored: &StoredAsset,
    ) -> Result<Asset, NewsroomError> {
        let mut tx = self.pool().begin().await?;
        let asset = sqlx::query_as::<_, Asset>(
            "INSERT INTO meridian.assets \
             (id, story_id, kind, uri, bucket, object_key, filename, byte_size, checksum, alt_text, created_by_id) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11) RETURNING *",
        )
        .bind(Uuid::now_v7())
        .bind(stored.story_id)
        .bind(&stored.kind)
        .bind(&stored.uri)
        .bind(&stored.bucket)
        .bind(&stored.object_key)
        .bind(&stored.filename)
        .bind(stored.byte_size)
        .bind(&stored.checksum)
        .bind(&stored.alt_text)
        .bind(actor.id)
        .fetch_one(&mut *tx)
        .await?;
        crate::audit(
            &mut *tx,
            actor.id,
            "asset.uploaded",
            "asset",
            &asset.id.to_string(),
            None,
            Some(serde_json::json!({ "object_key": stored.object_key, "byte_size": stored.byte_size })),
        )
        .await?;
        Self::enqueue_event(
            &mut *tx,
            "asset.uploaded",
            &asset.id.to_string(),
            None,
            serde_json::json!({ "asset_id": asset.id, "byte_size": stored.byte_size, "story_id": asset.story_id }),
            Some(actor.id),
        )
        .await?;
        tx.commit().await?;
        Ok(asset)
    }

    /// Loads an asset by id.
    ///
    /// # Errors
    /// [`NewsroomError::NotFound`]; database failures.
    pub async fn get_asset(&self, asset_id: Uuid) -> Result<Asset, NewsroomError> {
        sqlx::query_as::<_, Asset>("SELECT * FROM meridian.assets WHERE id = $1")
            .bind(asset_id)
            .fetch_optional(self.pool())
            .await?
            .ok_or(NewsroomError::NotFound("Asset"))
    }

    /// The published stories a public feed or sitemap should list, newest first.
    /// Unauthenticated — only `published` stories are ever returned.
    ///
    /// # Errors
    /// Propagates database failures.
    pub async fn published_for_syndication(
        &self,
    ) -> Result<Vec<SyndicationItem>, NewsroomError> {
        let rows = sqlx::query_as::<_, SyndicationItem>(
            "SELECT slug, title, dek, published_at FROM meridian.stories \
             WHERE publication_state = 'published' \
             ORDER BY published_at DESC NULLS LAST LIMIT 200",
        )
        .fetch_all(self.pool())
        .await?;
        Ok(rows)
    }

    /// A paginated, optionally filtered page of published article summaries, newest
    /// first — the query behind the headless `GET /api/v1/public/articles` list.
    /// `category` filters to an exact section; `since` returns only items published
    /// strictly after that instant (incremental sync).
    ///
    /// # Errors
    /// Propagates database failures.
    pub async fn published_summaries(
        &self,
        limit: i64,
        offset: i64,
        category: Option<&str>,
        since: Option<OffsetDateTime>,
    ) -> Result<Vec<PublishedSummary>, NewsroomError> {
        let rows = sqlx::query_as::<_, PublishedSummary>(
            // Embargo gate: a release may be published (and syndicated to partners)
            // while still not showable to readers until its embargo lifts. Hide the
            // story from this reader-facing list until the current publication's
            // embargo has passed. NULL embargo (or no release) means show.
            "SELECT slug, title, dek, category, published_at FROM meridian.stories \
             WHERE publication_state = 'published' \
               AND ($3::text IS NULL OR category = $3) \
               AND ($4::timestamptz IS NULL OR published_at > $4) \
               AND COALESCE(( \
                 SELECT r.embargo_until FROM meridian.releases r \
                 WHERE r.story_id = stories.id AND r.status = 'published' \
                 ORDER BY r.published_at DESC NULLS LAST LIMIT 1 \
               ), now()) <= now() \
             ORDER BY published_at DESC NULLS LAST LIMIT $1 OFFSET $2",
        )
        .bind(limit)
        .bind(offset)
        .bind(category)
        .bind(since)
        .fetch_all(self.pool())
        .await?;
        Ok(rows)
    }

    /// Published stories with the body + byline + subject an IPTC ninjs feed needs.
    ///
    /// # Errors
    /// Propagates database failures.
    pub async fn published_ninjs(&self) -> Result<Vec<NinjsStory>, NewsroomError> {
        let rows = sqlx::query_as::<_, NinjsStory>(
            "SELECT s.slug, s.title, s.dek, s.body, s.category, s.tags, \
                    u.display_name AS author_name, \
                    s.rights_holder, s.rights_notice, s.usage_terms, s.license_url, \
                    s.reuse_embargo_until, s.territory_mode, s.territories, s.digital_source_type, \
                    s.created_at, s.updated_at, s.published_at \
             FROM meridian.stories s \
             LEFT JOIN meridian.users u ON u.id = s.author_id \
             WHERE s.publication_state = 'published' \
             ORDER BY s.published_at DESC NULLS LAST LIMIT 200",
        )
        .fetch_all(self.pool())
        .await?;
        Ok(rows)
    }

    /// One published story in the ninjs shape, by id — used to build the IPTC ninjs
    /// payload delivered to syndication channels.
    ///
    /// # Errors
    /// Propagates database failures.
    pub async fn published_ninjs_one(
        &self,
        story_id: Uuid,
    ) -> Result<Option<NinjsStory>, NewsroomError> {
        let row = sqlx::query_as::<_, NinjsStory>(
            "SELECT s.slug, s.title, s.dek, s.body, s.category, s.tags, \
                    u.display_name AS author_name, \
                    s.rights_holder, s.rights_notice, s.usage_terms, s.license_url, \
                    s.reuse_embargo_until, s.territory_mode, s.territories, s.digital_source_type, \
                    s.created_at, s.updated_at, s.published_at \
             FROM meridian.stories s \
             LEFT JOIN meridian.users u ON u.id = s.author_id \
             WHERE s.id = $1 AND s.publication_state = 'published' LIMIT 1",
        )
        .bind(story_id)
        .fetch_optional(self.pool())
        .await?;
        Ok(row)
    }

    /// One published story by slug, for the public reader. Unauthenticated by
    /// design — the `publication_state = 'published'` filter is the only gate, so a
    /// draft or scheduled story resolves to `None` rather than leaking.
    ///
    /// # Errors
    /// Propagates database failures.
    pub async fn public_article(
        &self,
        slug: &str,
    ) -> Result<Option<PublicArticle>, NewsroomError> {
        let row = sqlx::query_as::<_, PublicArticle>(
            "SELECT s.id, s.slug, s.title, s.dek, s.category, s.desk_id, s.sub_sector_id, s.body, s.tags, \
                    u.display_name AS author_name, \
                    s.rights_holder, s.rights_notice, s.usage_terms, s.license_url, \
                    s.reuse_embargo_until, s.territory_mode, s.territories, \
                    s.published_at \
             FROM meridian.stories s \
             LEFT JOIN meridian.users u ON u.id = s.author_id \
             WHERE s.slug = $1 AND s.publication_state = 'published' \
               AND COALESCE(( \
                 SELECT r.embargo_until FROM meridian.releases r \
                 WHERE r.story_id = s.id AND r.status = 'published' \
                 ORDER BY r.published_at DESC NULLS LAST LIMIT 1 \
               ), now()) <= now() \
             LIMIT 1",
        )
        .bind(slug)
        .fetch_optional(self.pool())
        .await?;
        Ok(row)
    }

    /// Moves an asset's verification status. Publish-gate blocker #8 requires
    /// every attached asset to be `verified`; before this there was no path to
    /// satisfy it, so any story carrying an asset was unpublishable. An asset
    /// attached to a story is gated by that story's edit team; an org-level asset
    /// (no story) is editable by any authenticated actor.
    ///
    /// # Errors
    /// [`NewsroomError::Unprocessable`] for an unknown status;
    /// [`NewsroomError::Forbidden`] off the story team; [`NewsroomError::NotFound`];
    /// database failures.
    pub async fn set_asset_verification(
        &self,
        actor: &Actor,
        asset_id: Uuid,
        status: &str,
    ) -> Result<Asset, NewsroomError> {
        // Matches the `assets.verification_status` CHECK constraint (migration 0009).
        const ASSET_VERIFICATION_STATUSES: [&str; 5] =
            ["unverified", "in_progress", "verified", "disputed", "rejected"];
        if !ASSET_VERIFICATION_STATUSES.contains(&status) {
            return Err(NewsroomError::Unprocessable(format!(
                "'{status}' is not an asset verification status"
            )));
        }
        let asset = self.get_asset(asset_id).await?;
        if let Some(story_id) = asset.story_id {
            let story = self.get_story(story_id).await?;
            if !self.may_edit_story(actor, &story).await? {
                return Err(NewsroomError::Forbidden {
                    capability: "stories.team".to_owned(),
                    reason: "You are not on this story team".to_owned(),
                });
            }
        }
        let mut tx = self.pool().begin().await?;
        let updated = sqlx::query_as::<_, Asset>(
            "UPDATE meridian.assets SET verification_status = $2, updated_at = now() \
             WHERE id = $1 RETURNING *",
        )
        .bind(asset_id)
        .bind(status)
        .fetch_one(&mut *tx)
        .await?;
        crate::audit(
            &mut *tx,
            actor.id,
            "asset.verification_changed",
            "asset",
            &asset_id.to_string(),
            None,
            Some(serde_json::json!({ "status": status })),
        )
        .await?;
        Self::enqueue_event(
            &mut *tx,
            "asset.verification_changed",
            &asset_id.to_string(),
            None,
            serde_json::json!({ "asset_id": asset_id, "status": status }),
            Some(actor.id),
        )
        .await?;
        tx.commit().await?;
        Ok(updated)
    }

    /// Edits an asset's rights and alt text after creation. Two publish-gate
    /// blockers depend on these (rights present + unexpired, alt text on image /
    /// graphic), and before this they could only be set at create time. Gated on
    /// the owning story's edit team, like verification.
    ///
    /// # Errors
    /// [`NewsroomError::Forbidden`] off the story team; [`NewsroomError::NotFound`];
    /// database failures.
    pub async fn update_asset_metadata(
        &self,
        actor: &Actor,
        asset_id: Uuid,
        rights: Option<String>,
        alt_text: Option<String>,
    ) -> Result<Asset, NewsroomError> {
        let asset = self.get_asset(asset_id).await?;
        if let Some(story_id) = asset.story_id {
            let story = self.get_story(story_id).await?;
            if !self.may_edit_story(actor, &story).await? {
                return Err(NewsroomError::Forbidden {
                    capability: "stories.team".to_owned(),
                    reason: "You are not on this story team".to_owned(),
                });
            }
        }
        let mut tx = self.pool().begin().await?;
        let updated = sqlx::query_as::<_, Asset>(
            "UPDATE meridian.assets SET rights = $2, alt_text = $3, updated_at = now() \
             WHERE id = $1 RETURNING *",
        )
        .bind(asset_id)
        .bind(rights)
        .bind(alt_text)
        .fetch_one(&mut *tx)
        .await?;
        crate::audit(
            &mut *tx,
            actor.id,
            "asset.updated",
            "asset",
            &asset_id.to_string(),
            None,
            None,
        )
        .await?;
        Self::enqueue_event(
            &mut *tx,
            "asset.updated",
            &asset_id.to_string(),
            None,
            serde_json::json!({ "asset_id": asset_id }),
            Some(actor.id),
        )
        .await?;
        tx.commit().await?;
        Ok(updated)
    }

    /// Deletes an asset. Gated like its editing siblings
    /// ([`Self::set_asset_verification`], [`Self::update_asset_metadata`]): an asset
    /// attached to a story requires that story's edit team; an org-level asset (no
    /// story — the shared Drive) is deletable by any authenticated actor. Deletion is
    /// irreversible and removes the stored bytes, so this check must run before the
    /// caller touches the object store.
    ///
    /// # Errors
    /// [`NewsroomError::Forbidden`] off the story team; [`NewsroomError::NotFound`];
    /// database failures.
    pub async fn delete_asset(&self, actor: &Actor, asset_id: Uuid) -> Result<(), NewsroomError> {
        let asset = self.get_asset(asset_id).await?;
        if let Some(story_id) = asset.story_id {
            let story = self.get_story(story_id).await?;
            if !self.may_edit_story(actor, &story).await? {
                return Err(NewsroomError::Forbidden {
                    capability: "stories.team".to_owned(),
                    reason: "You are not on this story team".to_owned(),
                });
            }
        }
        let mut tx = self.pool().begin().await?;
        let deleted = sqlx::query("DELETE FROM meridian.assets WHERE id = $1")
            .bind(asset_id)
            .execute(&mut *tx)
            .await?;
        if deleted.rows_affected() == 0 {
            return Err(NewsroomError::NotFound("Asset"));
        }
        crate::audit(
            &mut *tx,
            actor.id,
            "asset.deleted",
            "asset",
            &asset_id.to_string(),
            None,
            None,
        )
        .await?;
        Self::enqueue_event(
            &mut *tx,
            "asset.deleted",
            &asset_id.to_string(),
            None,
            serde_json::json!({ "asset_id": asset_id }),
            Some(actor.id),
        )
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Of the given object keys, those that still have an asset metadata row.
    /// The complement are orphaned objects safe to reclaim.
    ///
    /// # Errors
    /// Propagates database failures.
    pub async fn existing_object_keys(
        &self,
        keys: &[String],
    ) -> Result<Vec<String>, NewsroomError> {
        Ok(sqlx::query_scalar::<_, String>(
            "SELECT object_key FROM meridian.assets \
             WHERE object_key = ANY($1) AND object_key IS NOT NULL",
        )
        .bind(keys)
        .fetch_all(self.pool())
        .await?)
    }

    /// Total stored bytes across owned (RustFS-backed) assets.
    ///
    /// # Errors
    /// Propagates database failures.
    pub async fn asset_storage_total(&self) -> Result<i64, NewsroomError> {
        Ok(sqlx::query_scalar(
            "SELECT COALESCE(sum(byte_size), 0)::bigint FROM meridian.assets \
             WHERE object_key IS NOT NULL",
        )
        .fetch_one(self.pool())
        .await?)
    }

    /// A story's releases, newest first.
    ///
    /// # Errors
    /// Propagates database failures.
    pub async fn list_releases(&self, story_id: Uuid) -> Result<Vec<Release>, NewsroomError> {
        Ok(sqlx::query_as::<_, Release>(
            "SELECT * FROM meridian.releases WHERE story_id = $1 ORDER BY created_at DESC",
        )
        .bind(story_id)
        .fetch_all(self.pool())
        .await?)
    }

    /// Creates a release, enforcing the publish gate. A future `scheduled_at`
    /// schedules it; otherwise it publishes immediately. Requires the
    /// `reviews.approve` capability and a publisher role.
    ///
    /// # Errors
    /// [`NewsroomError::Forbidden`]; [`NewsroomError::NotFound`];
    /// [`NewsroomError::Unprocessable`] for a cross-story version;
    /// [`NewsroomError::Conflict`] for a non-latest version or an unmet gate;
    /// database failures.
    // The gate, the insert, and the schedule-or-publish branch form one
    // transaction; splitting it would scatter that atomicity.
    #[allow(clippy::too_many_lines)]
    pub async fn create_release(
        &self,
        actor: &Actor,
        story_id: Uuid,
        draft: &ReleaseDraft,
    ) -> Result<Release, NewsroomError> {
        // Input validation first. Terms that can never be satisfied are a
        // malformed request, not a workflow state — reporting the publish gate
        // instead would answer a question the caller did not ask, and send them
        // chasing approvals for a release that was never expressible.
        let now = OffsetDateTime::now_utc();
        if draft.expires_at.is_some_and(|at| at <= now) {
            return Err(NewsroomError::Unprocessable(
                "Expiry is already in the past; this release would be taken down immediately"
                    .to_owned(),
            ));
        }
        if let (Some(embargo), Some(expiry)) = (draft.embargo_until, draft.expires_at)
            && embargo >= expiry
        {
            return Err(NewsroomError::Unprocessable(
                "Embargo ends after expiry, so the story would never be publicly visible"
                    .to_owned(),
            ));
        }

        // The story is loaded before the gate so publish authority resolves in the
        // sector that owns the story, not globally.
        let story = self.get_story(story_id).await?;
        authz::require(self.pool(), actor, "reviews.approve", story.desk_id).await?;
        authz::require(self.pool(), actor, "releases.publish", story.desk_id).await?;
        let version_story: Option<Uuid> =
            sqlx::query_scalar("SELECT story_id FROM meridian.story_versions WHERE id = $1")
                .bind(draft.version_id)
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
        let latest: Option<Uuid> = sqlx::query_scalar(
            "SELECT id FROM meridian.story_versions WHERE story_id = $1 ORDER BY number DESC LIMIT 1",
        )
        .bind(story_id)
        .fetch_optional(self.pool())
        .await?;
        if latest != Some(draft.version_id) {
            return Err(NewsroomError::Conflict(
                "Only the latest version can be released".to_owned(),
            ));
        }
        // A persisted fast-path flag alone must not disarm the gate at release: the
        // caller must still hold the capability and a reason must stand.
        if story.fast_path {
            authz::require(self.pool(), actor, "workflow.fast_path", story.desk_id).await?;
            if story.fast_path_reason.as_deref().unwrap_or_default().trim().is_empty() {
                return Err(NewsroomError::Conflict(
                    "Fast-path release requires a recorded reason".to_owned(),
                ));
            }
        }
        let blockers = self.readiness_blockers(&story, story.fast_path).await?;
        if !blockers.is_empty() {
            return Err(NewsroomError::Conflict(format!(
                "Publish gate failed: {}",
                blockers.join("; ")
            )));
        }

        let scheduled = draft.scheduled_at.is_some_and(|at| at > now);
        let mut tx = self.pool().begin().await?;
        let release = sqlx::query_as::<_, Release>(
            "INSERT INTO meridian.releases \
             (id, story_id, version_id, channels, scheduled_at, embargo_until, expires_at, created_by_id) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8) RETURNING *",
        )
        .bind(Uuid::now_v7())
        .bind(story_id)
        .bind(draft.version_id)
        .bind(&draft.channels)
        .bind(draft.scheduled_at)
        .bind(draft.embargo_until)
        .bind(draft.expires_at)
        .bind(actor.id)
        .fetch_one(&mut *tx)
        .await?;

        let release = if scheduled {
            let updated = sqlx::query_as::<_, Release>(
                "UPDATE meridian.releases SET status = 'scheduled' WHERE id = $1 RETURNING *",
            )
            .bind(release.id)
            .fetch_one(&mut *tx)
            .await?;
            sqlx::query(
                "UPDATE meridian.stories SET publication_state = 'scheduled', scheduled_at = $2, \
                   updated_at = now() WHERE id = $1",
            )
            .bind(story_id)
            .bind(draft.scheduled_at)
            .execute(&mut *tx)
            .await?;
            crate::audit(
                &mut *tx,
                actor.id,
                "release.scheduled",
                "release",
                &updated.id.to_string(),
                None,
                Some(serde_json::json!({ "scheduled_at": draft.scheduled_at })),
            )
            .await?;
            Self::enqueue_event(
                &mut *tx,
                "release.scheduled",
                &updated.id.to_string(),
                None,
                serde_json::json!({ "release_id": updated.id, "story_id": story_id, "scheduled_at": draft.scheduled_at }),
                Some(actor.id),
            )
            .await?;
            updated
        } else {
            // Immediate publish: story must be READY, then synthesise a receipt
            // per channel.
            if story.workflow_state != "ready" {
                return Err(NewsroomError::Conflict(
                    "Story must be ready before publication".to_owned(),
                ));
            }
            let receipts = synthesize_receipts(&draft.channels, now);
            let updated = sqlx::query_as::<_, Release>(
                "UPDATE meridian.releases SET status = 'published', published_at = $2, receipts = $3 \
                 WHERE id = $1 RETURNING *",
            )
            .bind(release.id)
            .bind(now)
            .bind(&receipts)
            .fetch_one(&mut *tx)
            .await?;
            sqlx::query(
                "UPDATE meridian.stories SET publication_state = 'published', \
                   published_at = COALESCE(published_at, $2), scheduled_at = NULL, updated_at = now() \
                 WHERE id = $1",
            )
            .bind(story_id)
            .bind(now)
            .execute(&mut *tx)
            .await?;
            crate::audit(
                &mut *tx,
                actor.id,
                "release.published",
                "release",
                &updated.id.to_string(),
                None,
                Some(serde_json::json!({ "story_id": story_id, "version_id": draft.version_id })),
            )
            .await?;
            // Immediate publish-now also fires the outbound `article.published` event.
            Self::enqueue_event(
                &mut *tx,
                "article.published",
                &story_id.to_string(),
                None,
                serde_json::json!({
                    "uri": story_id,
                    "type": "text",
                    "profile": "ninjs",
                    "pubstatus": "usable",
                    "headlines": [{ "value": story.title }],
                    "slugline": story.slug,
                    "subjects": [{ "name": story.category }],
                    "digitalsourcetype": story.digital_source_type,
                    "trigger": "publish_now",
                }),
                Some(actor.id),
            )
            .await?;
            updated
        };
        tx.commit().await?;
        // Immediate publishes deliver now, outside the transaction — network calls
        // must not hold a DB lock. Scheduled releases deliver when they fire, in
        // `process_due_releases`. Delivery never fails the publish: the story is
        // already live, and a failed receipt records the problem for a retry.
        if release.status == "published" {
            let receipts = self.deliver_receipts(&story, &draft.channels, now).await;
            let delivered = sqlx::query_as::<_, Release>(
                "UPDATE meridian.releases SET receipts = $2 WHERE id = $1 RETURNING *",
            )
            .bind(release.id)
            .bind(&receipts)
            .fetch_one(self.pool())
            .await?;
            return Ok(delivered);
        }
        Ok(release)
    }

    /// POSTs the published article to every targeted channel that carries a
    /// `webhook_url` in its config, returning one receipt per channel. Channels
    /// without a webhook stay `not_delivered`; transport errors and non-2xx
    /// responses become `failed` receipts. Only `http`/`https` webhooks are called.
    async fn deliver_receipts(
        &self,
        story: &crate::stories::Story,
        channels: &serde_json::Value,
        now: OffsetDateTime,
    ) -> serde_json::Value {
        let at = now
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap_or_default();
        let registry = self.list_channels(false).await.unwrap_or_default();
        // The default payload: a generic `article.published` envelope. Syndication
        // channels instead receive a proper IPTC ninjs item, built lazily on first use.
        let envelope = serde_json::json!({
            "event": "article.published",
            "id": story.id,
            "slug": story.slug,
            "title": story.title,
            "dek": story.dek,
            "category": story.category,
            "body": story.body,
            "published_at": at,
        });
        let mut ninjs: Option<serde_json::Value> = None;
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .ok();
        let targets = channels.as_array().cloned().unwrap_or_default();
        let mut receipts = Vec::with_capacity(targets.len());
        for target in &targets {
            let name = target
                .as_str()
                .or_else(|| target.get("name").and_then(serde_json::Value::as_str))
                .unwrap_or("unknown");
            let record = registry.iter().find(|c| c.key == name || c.name == name);
            let webhook = record
                .and_then(|c| c.config.get("webhook_url"))
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|url| url.starts_with("http://") || url.starts_with("https://"));
            let is_syndication = record.is_some_and(|c| c.kind == "syndication");
            let receipt = match (webhook, client.as_ref()) {
                (Some(url), Some(client)) => {
                    let (payload, format) = if is_syndication {
                        if ninjs.is_none() {
                            ninjs = self
                                .published_ninjs_one(story.id)
                                .await
                                .ok()
                                .flatten()
                                .map(|item| ninjs_delivery_value(&item));
                        }
                        match &ninjs {
                            Some(doc) => (doc.clone(), "ninjs"),
                            None => (envelope.clone(), "envelope"),
                        }
                    } else {
                        (envelope.clone(), "envelope")
                    };
                    match client.post(url).json(&payload).send().await {
                    Ok(response) => serde_json::json!({
                        "channel": name,
                        "status": if response.status().is_success() { "delivered" } else { "failed" },
                        "http": response.status().as_u16(),
                        "detail": format!("POST {url}"),
                        "format": format,
                        "at": at,
                    }),
                    Err(error) => serde_json::json!({
                        "channel": name,
                        "status": "failed",
                        "detail": error.to_string(),
                        "at": at,
                    }),
                    }
                }
                _ => serde_json::json!({
                    "channel": name,
                    "status": UNDELIVERED_STATUS,
                    "detail": "No webhook configured for this channel",
                    "at": at,
                }),
            };
            receipts.push(receipt);
        }
        serde_json::Value::Array(receipts)
    }

    /// Publishes every scheduled release whose time has passed, re-checking the
    /// gate and recording a succeeded or failed attempt for each. Returns the
    /// releases that published. Requires `reviews.approve` + a publisher role.
    ///
    /// `trigger` is one of `worker` / `queue` / `endpoint`.
    ///
    /// # Errors
    /// [`NewsroomError::Forbidden`]; database failures.
    pub async fn process_due_releases(
        &self,
        actor: &Actor,
        trigger: &str,
    ) -> Result<Vec<Release>, NewsroomError> {
        // Org-wide sweep authority: this pass spans every desk, so the outer gate
        // is unscoped and each release is re-checked against its own sector below.
        authz::require(self.pool(), actor, "reviews.approve", None).await?;
        authz::require(self.pool(), actor, "releases.publish", None).await?;
        // Due when the base slot has passed, OR any per-channel time has — the
        // latter catches a channel scheduled earlier than the release's own slot.
        let due = sqlx::query_as::<_, Release>(
            "SELECT * FROM meridian.releases \
             WHERE status = 'scheduled' AND scheduled_at IS NOT NULL AND ( \
                 scheduled_at <= now() \
                 OR EXISTS ( \
                     SELECT 1 FROM jsonb_each_text(channel_schedule) e \
                     WHERE e.value <> '' AND e.value::timestamptz <= now() \
                 ) \
             ) \
             ORDER BY scheduled_at",
        )
        .fetch_all(self.pool())
        .await?;

        let now = OffsetDateTime::now_utc();
        let mut published = Vec::new();
        for release in due {
            let story = self.get_story(release.story_id).await?;
            // Publish authority is per-sector: a desk lead running the sweep
            // publishes only the desks they hold it in. Releases they cannot
            // publish are left queued for someone who can, not failed.
            if !authz::has(self.pool(), actor, "reviews.approve", story.desk_id).await? {
                continue;
            }
            let blockers = self.readiness_blockers(&story, story.fast_path).await?;
            let attempt_number: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM meridian.release_attempts WHERE release_id = $1",
            )
            .bind(release.id)
            .fetch_one(self.pool())
            .await?;
            if !blockers.is_empty() {
                // A recurring sweep re-checks the same still-blocked release every
                // pass. Record the gate failure once, not a fresh identical row on
                // every tick — only append a new attempt when the last one wasn't
                // already the same gate failure (so a *changed* blocker set is still
                // captured, and a later success still reads a clean history).
                let last: Option<(String, Option<String>, Option<String>)> = sqlx::query_as(
                    "SELECT status, error_code, error_message FROM meridian.release_attempts \
                     WHERE release_id = $1 ORDER BY started_at DESC, attempt_number DESC LIMIT 1",
                )
                .bind(release.id)
                .fetch_optional(self.pool())
                .await?;
                let joined = blockers.join("; ");
                let unchanged = matches!(
                    last,
                    Some((ref s, ref code, ref msg))
                        if s == "failed"
                            && code.as_deref() == Some("gate_failed")
                            && msg.as_deref() == Some(joined.as_str())
                );
                if unchanged {
                    continue;
                }
                let mut tx = self.pool().begin().await?;
                sqlx::query(
                    "INSERT INTO meridian.release_attempts \
                     (id, release_id, attempt_number, status, trigger, error_code, error_message) \
                     VALUES ($1,$2,$3,'failed',$4,'gate_failed',$5)",
                )
                .bind(Uuid::now_v7())
                .bind(release.id)
                .bind(i32::try_from(attempt_number + 1).unwrap_or(1))
                .bind(trigger)
                .bind(joined)
                .execute(&mut *tx)
                .await?;
                tx.commit().await?;
                continue;
            }

            // Per-channel staggered publish: when the release carries a
            // `channel_schedule`, send only the channels whose own time has
            // arrived and that haven't gone out yet, and mark the release
            // published only once every channel has. An empty schedule (the
            // common case) skips this and takes the atomic path below unchanged.
            let has_channel_schedule = release
                .channel_schedule
                .as_object()
                .is_some_and(|m| !m.is_empty());
            if has_channel_schedule {
                let all_channels: Vec<String> = release
                    .channels
                    .as_array()
                    .map(|a| a.iter().filter_map(|c| c.as_str().map(str::to_owned)).collect())
                    .unwrap_or_default();
                let already = delivered_channel_names(&release.receipts);
                let due_names: Vec<String> = all_channels
                    .iter()
                    .filter(|ch| !already.contains(*ch))
                    .filter(|ch| {
                        effective_channel_time(ch, &release.channel_schedule, release.scheduled_at)
                            .is_some_and(|t| t <= now)
                    })
                    .cloned()
                    .collect();
                if due_names.is_empty() {
                    continue;
                }
                let due_value = serde_json::Value::Array(
                    due_names.iter().map(|c| serde_json::Value::String(c.clone())).collect(),
                );
                // Deliver the newly-due channels, then merge with prior receipts.
                let fresh = self.deliver_receipts(&story, &due_value, now).await;
                let mut merged = release.receipts.as_array().cloned().unwrap_or_default();
                if let Some(arr) = fresh.as_array() {
                    merged.extend(arr.iter().cloned());
                }
                let merged_value = serde_json::Value::Array(merged);
                let done = delivered_channel_names(&merged_value);
                let all_done = all_channels.iter().all(|ch| done.contains(ch));

                let mut tx = self.pool().begin().await?;
                // Same `status = 'scheduled'` guard as the atomic path: if the
                // release was cancelled mid-stagger, stop — don't keep delivering
                // channels or flip a cancelled release to published.
                let updated = if all_done {
                    let Some(r) = sqlx::query_as::<_, Release>(
                        "UPDATE meridian.releases SET status = 'published', \
                           published_at = COALESCE(published_at, $2), receipts = $3 \
                         WHERE id = $1 AND status = 'scheduled' RETURNING *",
                    )
                    .bind(release.id)
                    .bind(now)
                    .bind(&merged_value)
                    .fetch_optional(&mut *tx)
                    .await?
                    else {
                        continue;
                    };
                    sqlx::query(
                        "UPDATE meridian.stories SET publication_state = 'published', \
                           published_at = COALESCE(published_at, $2), scheduled_at = NULL, updated_at = now() \
                         WHERE id = $1",
                    )
                    .bind(release.story_id)
                    .bind(now)
                    .execute(&mut *tx)
                    .await?;
                    r
                } else {
                    // Partial: record what went out, keep the release scheduled.
                    let Some(r) = sqlx::query_as::<_, Release>(
                        "UPDATE meridian.releases SET receipts = $2 \
                         WHERE id = $1 AND status = 'scheduled' RETURNING *",
                    )
                    .bind(release.id)
                    .bind(&merged_value)
                    .fetch_optional(&mut *tx)
                    .await?
                    else {
                        continue;
                    };
                    r
                };
                sqlx::query(
                    "INSERT INTO meridian.release_attempts \
                     (id, release_id, attempt_number, status, trigger) VALUES ($1,$2,$3,'succeeded',$4)",
                )
                .bind(Uuid::now_v7())
                .bind(release.id)
                .bind(i32::try_from(attempt_number + 1).unwrap_or(1))
                .bind(trigger)
                .execute(&mut *tx)
                .await?;
                crate::audit(
                    &mut *tx,
                    actor.id,
                    if all_done { "release.published" } else { "release.channel_published" },
                    "release",
                    &release.id.to_string(),
                    None,
                    Some(serde_json::json!({
                        "trigger": trigger,
                        "channels": due_names,
                        "partial": !all_done,
                    })),
                )
                .await?;
                tx.commit().await?;
                if all_done {
                    published.push(updated);
                }
                continue;
            }

            let mut tx = self.pool().begin().await?;
            let receipts = synthesize_receipts(&release.channels, now);
            // Guard on `status = 'scheduled'` so a cancel (or another sweep / a
            // concurrent publish-now) that landed between the due-scan and now wins:
            // no row matches, we skip, and the release is neither double-published
            // nor un-cancelled. Without it, `WHERE id = $1` would overwrite a
            // cancellation and deliver twice.
            let Some(updated) = sqlx::query_as::<_, Release>(
                "UPDATE meridian.releases SET status = 'published', published_at = $2, receipts = $3 \
                 WHERE id = $1 AND status = 'scheduled' RETURNING *",
            )
            .bind(release.id)
            .bind(now)
            .bind(&receipts)
            .fetch_optional(&mut *tx)
            .await?
            else {
                continue;
            };
            sqlx::query(
                "UPDATE meridian.stories SET publication_state = 'published', \
                   published_at = COALESCE(published_at, $2), scheduled_at = NULL, updated_at = now() \
                 WHERE id = $1",
            )
            .bind(release.story_id)
            .bind(now)
            .execute(&mut *tx)
            .await?;
            sqlx::query(
                "INSERT INTO meridian.release_attempts \
                 (id, release_id, attempt_number, status, trigger) VALUES ($1,$2,$3,'succeeded',$4)",
            )
            .bind(Uuid::now_v7())
            .bind(release.id)
            .bind(i32::try_from(attempt_number + 1).unwrap_or(1))
            .bind(trigger)
            .execute(&mut *tx)
            .await?;
            crate::audit(
                &mut *tx,
                actor.id,
                "release.published",
                "release",
                &updated.id.to_string(),
                None,
                Some(serde_json::json!({ "trigger": trigger, "scheduled": true })),
            )
            .await?;
            // Fire the outbound `article.published` event (ninjs-shaped) so downstream
            // notification/syndication/analytics consumers hear about it.
            Self::enqueue_event(
                &mut *tx,
                "article.published",
                &updated.story_id.to_string(),
                None,
                serde_json::json!({
                    "uri": updated.story_id,
                    "type": "text",
                    "profile": "ninjs",
                    "pubstatus": "usable",
                    "headlines": [{ "value": story.title }],
                    "slugline": story.slug,
                    "subjects": [{ "name": story.category }],
                    "digitalsourcetype": story.digital_source_type,
                    "trigger": trigger,
                }),
                Some(actor.id),
            )
            .await?;
            tx.commit().await?;
            // Deliver the now-published release outside the transaction, then record
            // the real receipts — the same path as an immediate publish.
            let receipts = self.deliver_receipts(&story, &release.channels, now).await;
            let delivered = sqlx::query_as::<_, Release>(
                "UPDATE meridian.releases SET receipts = $2 WHERE id = $1 RETURNING *",
            )
            .bind(release.id)
            .bind(&receipts)
            .fetch_one(self.pool())
            .await?;
            published.push(delivered);
        }

        self.expire_due_releases(actor, trigger).await?;
        Ok(published)
    }

    /// Takes down releases whose `expires_at` has passed.
    ///
    /// Rides the same pass as scheduled publishing rather than needing a second
    /// worker: both are "the clock reached a datetime on a release", and running
    /// them in one loop means a deployment cannot end up with publishing live and
    /// expiry not.
    ///
    /// The story's `publication_state` is what readers key off, so it goes back to
    /// `not_live`. The release is kept and marked `expired` rather than deleted —
    /// the fact that something *was* published and then came down is exactly what a
    /// transparency record has to preserve.
    ///
    /// # Errors
    /// Propagates database failures.
    async fn expire_due_releases(
        &self,
        actor: &Actor,
        trigger: &str,
    ) -> Result<Vec<Release>, NewsroomError> {
        let due = sqlx::query_as::<_, Release>(
            "SELECT * FROM meridian.releases \
             WHERE status = 'published' AND expires_at IS NOT NULL AND expires_at <= now() \
             ORDER BY expires_at",
        )
        .fetch_all(self.pool())
        .await?;

        let mut expired = Vec::new();
        for release in due {
            let story = self.get_story(release.story_id).await?;
            // Same per-sector authority as publishing: taking a story down is a
            // publishing act, so a sweep run by a desk lead expires only their
            // desks. Anything they cannot publish, they cannot unpublish.
            if !authz::has(self.pool(), actor, "releases.publish", story.desk_id).await? {
                continue;
            }
            let mut tx = self.pool().begin().await?;
            let updated = sqlx::query_as::<_, Release>(
                "UPDATE meridian.releases SET status = 'expired' WHERE id = $1 RETURNING *",
            )
            .bind(release.id)
            .fetch_one(&mut *tx)
            .await?;
            // Only take the story down if this was its last standing publication. A
            // newer release (e.g. a correction re-release) may have republished the
            // same story after this one's slot; that publication is still live and
            // must not be pulled by an older release's expiry. This release is already
            // 'expired' above, so it is not counted among the remaining published ones.
            sqlx::query(
                "UPDATE meridian.stories \
                 SET publication_state = 'not_live', updated_at = now() \
                 WHERE id = $1 AND NOT EXISTS ( \
                     SELECT 1 FROM meridian.releases r \
                     WHERE r.story_id = $1 AND r.status = 'published' \
                 )",
            )
            .bind(release.story_id)
            .execute(&mut *tx)
            .await?;
            crate::audit(
                &mut *tx,
                actor.id,
                "release.expired",
                "release",
                &updated.id.to_string(),
                None,
                Some(serde_json::json!({ "trigger": trigger, "expires_at": release.expires_at })),
            )
            .await?;
            Self::enqueue_event(
                &mut *tx,
                "release.expired",
                &updated.id.to_string(),
                None,
                serde_json::json!({ "release_id": updated.id, "story_id": release.story_id, "expires_at": release.expires_at }),
                Some(actor.id),
            )
            .await?;
            tx.commit().await?;
            expired.push(updated);
        }
        Ok(expired)
    }

    /// Loads one release by id.
    ///
    /// # Errors
    /// [`NewsroomError::NotFound`]; database failures.
    pub async fn get_release(&self, release_id: Uuid) -> Result<Release, NewsroomError> {
        sqlx::query_as::<_, Release>("SELECT * FROM meridian.releases WHERE id = $1")
            .bind(release_id)
            .fetch_optional(self.pool())
            .await?
            .ok_or(NewsroomError::NotFound("Release"))
    }

    /// Moves a scheduled release to a new time. Only a `scheduled` release can be
    /// rescheduled; publish authority is checked in the release's own sector.
    ///
    /// # Errors
    /// [`NewsroomError::Forbidden`]; [`NewsroomError::NotFound`];
    /// [`NewsroomError::Conflict`] if not scheduled; database failures.
    pub async fn reschedule_release(
        &self,
        actor: &Actor,
        release_id: Uuid,
        scheduled_at: OffsetDateTime,
    ) -> Result<Release, NewsroomError> {
        let release = self.get_release(release_id).await?;
        let story = self.get_story(release.story_id).await?;
        authz::require(self.pool(), actor, "releases.publish", story.desk_id).await?;
        if release.status != "scheduled" {
            return Err(NewsroomError::Conflict(
                "Only a scheduled release can be rescheduled".to_owned(),
            ));
        }
        // A slot at or after the release's own expiry would publish and then be taken
        // down in the same pass — reject it as unexpressible, the way create_release
        // rejects a past expiry, rather than accepting a release that flaps live then
        // not_live on its first sweep.
        if release.expires_at.is_some_and(|expiry| scheduled_at >= expiry) {
            return Err(NewsroomError::Unprocessable(
                "The new slot is at or after the release's expiry, so it would publish then immediately expire"
                    .to_owned(),
            ));
        }
        let mut tx = self.pool().begin().await?;
        let updated = sqlx::query_as::<_, Release>(
            "UPDATE meridian.releases SET scheduled_at = $2 WHERE id = $1 RETURNING *",
        )
        .bind(release_id)
        .bind(scheduled_at)
        .fetch_one(&mut *tx)
        .await?;
        sqlx::query(
            "UPDATE meridian.stories SET scheduled_at = $2, updated_at = now() WHERE id = $1",
        )
        .bind(release.story_id)
        .bind(scheduled_at)
        .execute(&mut *tx)
        .await?;
        crate::audit(
            &mut *tx,
            actor.id,
            "release.rescheduled",
            "release",
            &release_id.to_string(),
            None,
            Some(serde_json::json!({ "scheduled_at": scheduled_at })),
        )
        .await?;
        Self::enqueue_event(
            &mut *tx,
            "release.rescheduled",
            &release_id.to_string(),
            None,
            serde_json::json!({ "release_id": release_id, "story_id": release.story_id, "scheduled_at": scheduled_at }),
            Some(actor.id),
        )
        .await?;
        tx.commit().await?;
        Ok(updated)
    }

    /// Sets per-channel send times on a scheduled release. `schedule` is a map of
    /// channel key -> RFC-3339 time; an empty value clears that channel back to the
    /// base slot, and keys that aren't among the release's own channels are dropped.
    /// Only a `scheduled` release can be retimed.
    ///
    /// # Errors
    /// [`NewsroomError::Forbidden`]; [`NewsroomError::NotFound`];
    /// [`NewsroomError::Conflict`] if not scheduled; [`NewsroomError::Unprocessable`]
    /// for a non-RFC-3339 time; database failures.
    pub async fn set_channel_schedule(
        &self,
        actor: &Actor,
        release_id: Uuid,
        schedule: &serde_json::Value,
    ) -> Result<Release, NewsroomError> {
        let release = self.get_release(release_id).await?;
        let story = self.get_story(release.story_id).await?;
        authz::require(self.pool(), actor, "releases.publish", story.desk_id).await?;
        if release.status != "scheduled" {
            return Err(NewsroomError::Conflict(
                "Only a scheduled release can be retimed per channel".to_owned(),
            ));
        }
        let own_channels: std::collections::HashSet<String> = release
            .channels
            .as_array()
            .map(|a| a.iter().filter_map(|c| c.as_str().map(str::to_owned)).collect())
            .unwrap_or_default();
        let incoming = schedule.as_object().ok_or_else(|| {
            NewsroomError::Unprocessable("channel_schedule must be an object".to_owned())
        })?;
        // Keep only known channels with a valid, non-empty RFC-3339 time.
        let mut cleaned = serde_json::Map::new();
        for (channel, value) in incoming {
            if !own_channels.contains(channel) {
                continue;
            }
            let Some(text) = value.as_str() else { continue };
            if text.is_empty() {
                continue;
            }
            OffsetDateTime::parse(text, &time::format_description::well_known::Rfc3339).map_err(|_| {
                NewsroomError::Unprocessable(format!("'{channel}' time is not a valid RFC-3339 instant"))
            })?;
            cleaned.insert(channel.clone(), serde_json::Value::String(text.to_owned()));
        }
        let cleaned_value = serde_json::Value::Object(cleaned);
        let mut tx = self.pool().begin().await?;
        let updated = sqlx::query_as::<_, Release>(
            "UPDATE meridian.releases SET channel_schedule = $2 WHERE id = $1 RETURNING *",
        )
        .bind(release_id)
        .bind(&cleaned_value)
        .fetch_one(&mut *tx)
        .await?;
        crate::audit(
            &mut *tx,
            actor.id,
            "release.channel_scheduled",
            "release",
            &release_id.to_string(),
            None,
            Some(cleaned_value),
        )
        .await?;
        Self::enqueue_event(
            &mut *tx,
            "release.channel_scheduled",
            &release_id.to_string(),
            None,
            serde_json::json!({ "release_id": release_id }),
            Some(actor.id),
        )
        .await?;
        tx.commit().await?;
        Ok(updated)
    }

    /// Cancels a scheduled release: the release is marked `cancelled` and the story
    /// returns to `not_live`. Only a `scheduled` release can be cancelled.
    ///
    /// # Errors
    /// [`NewsroomError::Forbidden`]; [`NewsroomError::NotFound`];
    /// [`NewsroomError::Conflict`] if not scheduled; database failures.
    pub async fn cancel_release(
        &self,
        actor: &Actor,
        release_id: Uuid,
    ) -> Result<Release, NewsroomError> {
        let release = self.get_release(release_id).await?;
        let story = self.get_story(release.story_id).await?;
        authz::require(self.pool(), actor, "releases.publish", story.desk_id).await?;
        if release.status != "scheduled" {
            return Err(NewsroomError::Conflict(
                "Only a scheduled release can be cancelled".to_owned(),
            ));
        }
        let mut tx = self.pool().begin().await?;
        let updated = sqlx::query_as::<_, Release>(
            "UPDATE meridian.releases SET status = 'cancelled', scheduled_at = NULL WHERE id = $1 RETURNING *",
        )
        .bind(release_id)
        .fetch_one(&mut *tx)
        .await?;
        sqlx::query(
            "UPDATE meridian.stories SET publication_state = 'not_live', scheduled_at = NULL, \
               updated_at = now() WHERE id = $1",
        )
        .bind(release.story_id)
        .execute(&mut *tx)
        .await?;
        crate::audit(
            &mut *tx,
            actor.id,
            "release.cancelled",
            "release",
            &release_id.to_string(),
            None,
            None,
        )
        .await?;
        Self::enqueue_event(
            &mut *tx,
            "release.cancelled",
            &release_id.to_string(),
            None,
            serde_json::json!({ "release_id": release_id, "story_id": release.story_id }),
            Some(actor.id),
        )
        .await?;
        tx.commit().await?;
        Ok(updated)
    }

    /// Publishes a scheduled release immediately, re-checking the gate the same way
    /// the scheduled sweep does. Only a `scheduled` release can be published now.
    ///
    /// # Errors
    /// [`NewsroomError::Forbidden`]; [`NewsroomError::NotFound`];
    /// [`NewsroomError::Conflict`] if not scheduled or the gate fails; database failures.
    pub async fn publish_release_now(
        &self,
        actor: &Actor,
        release_id: Uuid,
    ) -> Result<Release, NewsroomError> {
        let release = self.get_release(release_id).await?;
        let story = self.get_story(release.story_id).await?;
        authz::require(self.pool(), actor, "reviews.approve", story.desk_id).await?;
        authz::require(self.pool(), actor, "releases.publish", story.desk_id).await?;
        if release.status != "scheduled" {
            return Err(NewsroomError::Conflict(
                "Only a scheduled release can be published now".to_owned(),
            ));
        }
        if story.fast_path {
            authz::require(self.pool(), actor, "workflow.fast_path", story.desk_id).await?;
            if story.fast_path_reason.as_deref().unwrap_or_default().trim().is_empty() {
                return Err(NewsroomError::Conflict(
                    "Fast-path release requires a recorded reason".to_owned(),
                ));
            }
        }
        let blockers = self.readiness_blockers(&story, story.fast_path).await?;
        if !blockers.is_empty() {
            return Err(NewsroomError::Conflict(format!(
                "Publish gate failed: {}",
                blockers.join("; ")
            )));
        }
        let now = OffsetDateTime::now_utc();
        let receipts = synthesize_receipts(&release.channels, now);
        let mut tx = self.pool().begin().await?;
        // The `status != "scheduled"` check above is read-then-act; guard the write
        // too so a concurrent scheduled sweep or cancel that lands in between can't
        // be overwritten (which would double-publish and double-deliver).
        let Some(_updated) = sqlx::query_as::<_, Release>(
            "UPDATE meridian.releases SET status = 'published', published_at = $2, \
               scheduled_at = NULL, receipts = $3 WHERE id = $1 AND status = 'scheduled' RETURNING *",
        )
        .bind(release_id)
        .bind(now)
        .bind(&receipts)
        .fetch_optional(&mut *tx)
        .await?
        else {
            return Err(NewsroomError::Conflict(
                "Release is no longer scheduled".to_owned(),
            ));
        };
        sqlx::query(
            "UPDATE meridian.stories SET publication_state = 'published', \
               published_at = COALESCE(published_at, $2), scheduled_at = NULL, updated_at = now() \
             WHERE id = $1",
        )
        .bind(release.story_id)
        .bind(now)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "INSERT INTO meridian.release_attempts \
             (id, release_id, attempt_number, status, trigger) \
             VALUES ($1, $2, (SELECT count(*)+1 FROM meridian.release_attempts WHERE release_id = $2), 'succeeded', 'endpoint')",
        )
        .bind(Uuid::now_v7())
        .bind(release_id)
        .execute(&mut *tx)
        .await?;
        crate::audit(
            &mut *tx,
            actor.id,
            "release.published",
            "release",
            &release_id.to_string(),
            None,
            Some(serde_json::json!({ "manual": true })),
        )
        .await?;
        tx.commit().await?;
        // Deliver receipts outside the transaction, then record the real receipts.
        let receipts = self.deliver_receipts(&story, &release.channels, now).await;
        let delivered = sqlx::query_as::<_, Release>(
            "UPDATE meridian.releases SET receipts = $2 WHERE id = $1 RETURNING *",
        )
        .bind(release_id)
        .bind(&receipts)
        .fetch_one(self.pool())
        .await?;
        Ok(delivered)
    }

    /// The front page, in position order, with each slot's occupant resolved.
    ///
    /// # Errors
    /// Propagates database failures.
    pub async fn front_page(&self) -> Result<Vec<FrontPageSlot>, NewsroomError> {
        Ok(sqlx::query_as::<_, FrontPageSlot>(
            "SELECT f.id, f.position, f.label, f.story_id, \
                    s.title AS story_title, s.slug AS story_slug, \
                    f.is_pinned, f.updated_at \
             FROM meridian.front_page_slots f \
             LEFT JOIN meridian.stories s ON s.id = f.story_id \
             ORDER BY f.position",
        )
        .fetch_all(self.pool())
        .await?)
    }

    /// Places a story in a slot, clears it (`None`), or re-pins it.
    ///
    /// **Only a published story may be placed.** The check lives here rather than
    /// in a constraint because `publication_state` is on `stories` and a `CHECK`
    /// cannot reach across the foreign key — so this is the real enforcement, not
    /// a convenience.
    ///
    /// A story already in another slot is removed from it first: a front page that
    /// shows the same story twice is a curation mistake the model should make
    /// impossible rather than merely discourage.
    ///
    /// # Errors
    /// [`NewsroomError::Forbidden`] without `dashboard.view_audience`;
    /// [`NewsroomError::NotFound`] for an unknown slot or story;
    /// [`NewsroomError::Unprocessable`] when the story is not published;
    /// database failures.
    pub async fn set_front_page_slot(
        &self,
        actor: &Actor,
        slot_id: Uuid,
        story_id: Option<Uuid>,
        is_pinned: bool,
    ) -> Result<FrontPageSlot, NewsroomError> {
        // The front page is an audience decision, org-wide by definition — it is
        // one page, not one per sector — so it rides the global audience
        // capability rather than any desk's publish authority.
        authz::require(self.pool(), actor, "dashboard.view_audience", None).await?;

        if let Some(id) = story_id {
            let state: Option<String> =
                sqlx::query_scalar("SELECT publication_state FROM meridian.stories WHERE id = $1")
                    .bind(id)
                    .fetch_optional(self.pool())
                    .await?;
            match state.as_deref() {
                None => return Err(NewsroomError::NotFound("Story")),
                Some("published" | "partially_published" | "updating") => {}
                Some(other) => {
                    return Err(NewsroomError::Unprocessable(format!(
                        "Only a published story can go on the front page; this one is {other}"
                    )));
                }
            }
        }

        let mut tx = self.pool().begin().await?;
        if let Some(id) = story_id {
            sqlx::query(
                "UPDATE meridian.front_page_slots \
                 SET story_id = NULL, updated_by_id = $2, updated_at = now() \
                 WHERE story_id = $1 AND id <> $3",
            )
            .bind(id)
            .bind(actor.id)
            .bind(slot_id)
            .execute(&mut *tx)
            .await?;
        }
        let updated = sqlx::query_as::<_, FrontPageSlot>(
            "WITH upd AS ( \
               UPDATE meridian.front_page_slots \
               SET story_id = $2, is_pinned = $3, updated_by_id = $4, updated_at = now() \
               WHERE id = $1 RETURNING * ) \
             SELECT upd.id, upd.position, upd.label, upd.story_id, \
                    s.title AS story_title, s.slug AS story_slug, \
                    upd.is_pinned, upd.updated_at \
             FROM upd LEFT JOIN meridian.stories s ON s.id = upd.story_id",
        )
        .bind(slot_id)
        .bind(story_id)
        .bind(is_pinned)
        .bind(actor.id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(NewsroomError::NotFound("Front-page slot"))?;
        crate::audit(
            &mut *tx,
            actor.id,
            "frontpage.slot_set",
            "front_page_slot",
            &updated.id.to_string(),
            None,
            Some(serde_json::json!({ "position": updated.position, "story_id": story_id })),
        )
        .await?;
        tx.commit().await?;
        Ok(updated)
    }

    /// The channel registry. `active_only` is what the publish form offers;
    /// everything is what the admin screen manages.
    ///
    /// # Errors
    /// Propagates database failures.
    pub async fn list_channels(&self, active_only: bool) -> Result<Vec<Channel>, NewsroomError> {
        Ok(sqlx::query_as::<_, Channel>(
            "SELECT * FROM meridian.channels WHERE (NOT $1 OR is_active) ORDER BY name",
        )
        .bind(active_only)
        .fetch_all(self.pool())
        .await?)
    }

    /// Creates or updates a channel, keyed by `key`.
    ///
    /// Upsert rather than insert: `key` is what releases record, so re-adding an
    /// existing key must correct that channel rather than fail or duplicate it.
    ///
    /// # Errors
    /// [`NewsroomError::Forbidden`] without `flags.manage`;
    /// [`NewsroomError::Unprocessable`] for an unknown kind; database failures.
    pub async fn upsert_channel(
        &self,
        actor: &Actor,
        input: &ChannelInput,
    ) -> Result<Channel, NewsroomError> {
        // Channel configuration is org-wide distribution policy, not a sector
        // decision, so it rides the same global capability as feature flags.
        authz::require(self.pool(), actor, "flags.manage", None).await?;
        if !CHANNEL_KINDS.contains(&input.kind.as_str()) {
            return Err(NewsroomError::Unprocessable(format!(
                "'{}' is not a channel kind",
                input.kind
            )));
        }
        if input.key.trim().is_empty() {
            return Err(NewsroomError::Unprocessable("A channel needs a key".to_owned()));
        }
        let mut tx = self.pool().begin().await?;
        let channel = sqlx::query_as::<_, Channel>(
            "INSERT INTO meridian.channels (id, key, name, kind, description, is_active, config) \
             VALUES ($1,$2,$3,$4,$5,$6,$7) \
             ON CONFLICT (key) DO UPDATE SET \
               name = EXCLUDED.name, kind = EXCLUDED.kind, \
               description = EXCLUDED.description, is_active = EXCLUDED.is_active, \
               config = EXCLUDED.config, updated_at = now() \
             RETURNING *",
        )
        .bind(Uuid::now_v7())
        .bind(input.key.trim())
        .bind(&input.name)
        .bind(&input.kind)
        .bind(&input.description)
        .bind(input.is_active)
        .bind(&input.config)
        .fetch_one(&mut *tx)
        .await?;
        crate::audit(
            &mut *tx,
            actor.id,
            "channel.saved",
            "channel",
            &channel.id.to_string(),
            None,
            Some(serde_json::json!({ "key": channel.key, "is_active": channel.is_active })),
        )
        .await?;
        tx.commit().await?;
        Ok(channel)
    }

    /// Open corrections, optionally scoped to a story.
    ///
    /// # Errors
    /// Propagates database failures.
    pub async fn list_corrections(
        &self,
        story_id: Option<Uuid>,
    ) -> Result<Vec<Correction>, NewsroomError> {
        Ok(sqlx::query_as::<_, Correction>(
            "SELECT * FROM meridian.corrections WHERE ($1::uuid IS NULL OR story_id = $1) \
             ORDER BY created_at DESC",
        )
        .bind(story_id)
        .fetch_all(self.pool())
        .await?)
    }

    /// Files a correction against a published release. A correction publishes a
    /// public accountability note on the article, so it is gated to the story's edit
    /// team (which, via `may_edit_story`, also admits `stories.edit_any` editors in
    /// its desk) — not any authenticated user.
    ///
    /// # Errors
    /// [`NewsroomError::Forbidden`] off the story team;
    /// [`NewsroomError::Unprocessable`] for a bad classification;
    /// [`NewsroomError::NotFound`]; database failures.
    pub async fn create_correction(
        &self,
        actor: &Actor,
        story_id: Uuid,
        draft: &CorrectionDraft,
    ) -> Result<Correction, NewsroomError> {
        if !CORRECTION_CLASSES.contains(&draft.classification.as_str()) {
            return Err(NewsroomError::Unprocessable(format!(
                "'{}' is not a correction classification",
                draft.classification
            )));
        }
        let story = self.get_story(story_id).await?;
        if !self.may_edit_story(actor, &story).await? {
            return Err(NewsroomError::Forbidden {
                capability: "stories.team".to_owned(),
                reason: "You are not on this story team".to_owned(),
            });
        }
        let release_story: Option<Uuid> =
            sqlx::query_scalar("SELECT story_id FROM meridian.releases WHERE id = $1")
                .bind(draft.release_id)
                .fetch_optional(self.pool())
                .await?;
        if release_story != Some(story_id) {
            return Err(NewsroomError::Unprocessable(
                "Release belongs to another story".to_owned(),
            ));
        }
        let mut tx = self.pool().begin().await?;
        let correction = sqlx::query_as::<_, Correction>(
            "INSERT INTO meridian.corrections \
             (id, story_id, release_id, reported_by_id, owner_id, classification, description, public_note) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8) RETURNING *",
        )
        .bind(Uuid::now_v7())
        .bind(story_id)
        .bind(draft.release_id)
        .bind(actor.id)
        .bind(draft.owner_id)
        .bind(&draft.classification)
        .bind(&draft.description)
        .bind(&draft.public_note)
        .fetch_one(&mut *tx)
        .await?;
        crate::audit(
            &mut *tx,
            actor.id,
            "correction.created",
            "correction",
            &correction.id.to_string(),
            None,
            Some(serde_json::json!({ "classification": correction.classification })),
        )
        .await?;
        Self::enqueue_event(
            &mut *tx,
            "correction.created",
            &story_id.to_string(),
            None,
            serde_json::json!({
                "id": correction.id,
                "story_id": story_id,
                "classification": correction.classification,
                "description": correction.description,
                "public_note": correction.public_note,
            }),
            Some(actor.id),
        )
        .await?;
        tx.commit().await?;
        Ok(correction)
    }

    /// Marks a correction resolved. Gated like [`Self::create_correction`]: the
    /// story's edit team (or a `stories.edit_any` editor in its desk), so no
    /// authenticated user can clear another desk's accountability record.
    ///
    /// # Errors
    /// [`NewsroomError::Forbidden`] off the story team;
    /// [`NewsroomError::NotFound`]; database failures.
    pub async fn resolve_correction(
        &self,
        actor: &Actor,
        correction_id: Uuid,
    ) -> Result<Correction, NewsroomError> {
        let story_id: Option<Uuid> =
            sqlx::query_scalar("SELECT story_id FROM meridian.corrections WHERE id = $1")
                .bind(correction_id)
                .fetch_optional(self.pool())
                .await?;
        let story_id = story_id.ok_or(NewsroomError::NotFound("Correction"))?;
        let story = self.get_story(story_id).await?;
        if !self.may_edit_story(actor, &story).await? {
            return Err(NewsroomError::Forbidden {
                capability: "stories.team".to_owned(),
                reason: "You are not on this story team".to_owned(),
            });
        }
        let mut tx = self.pool().begin().await?;
        let correction = sqlx::query_as::<_, Correction>(
            "UPDATE meridian.corrections SET status = 'resolved', resolved_at = now(), \
               updated_at = now() WHERE id = $1 RETURNING *",
        )
        .bind(correction_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(NewsroomError::NotFound("Correction"))?;
        crate::audit(
            &mut *tx,
            actor.id,
            "correction.resolved",
            "correction",
            &correction.id.to_string(),
            None,
            None,
        )
        .await?;
        Self::enqueue_event(
            &mut *tx,
            "correction.resolved",
            &story_id.to_string(),
            None,
            serde_json::json!({ "id": correction.id, "story_id": story_id }),
            Some(actor.id),
        )
        .await?;
        tx.commit().await?;
        Ok(correction)
    }
}

/// What a receipt records when nothing was actually sent.
///
/// These receipts previously read `"published"` for every channel, which asserted
/// a delivery that never happened — there is no per-channel delivery layer, so no
/// newsletter was sent and no syndication feed was pushed. A persisted record
/// claiming otherwise is worse than a visibly empty one, because it looks
/// authoritative and would be believed in an audit.
///
/// The release itself is genuinely published — the story is live in Meridian — so
/// `releases.status` is unchanged. It is only the *per-channel delivery* claim
/// that was untrue, and this is the one field that carries it.
const UNDELIVERED_STATUS: &str = "not_delivered";

/// A valid IPTC **ninjs** item for a published story, for delivery to syndication
/// partners. Deliberately compact — `body_text` rather than `body_html`, the fields a
/// wire consumer needs — and self-contained so delivery does not depend on the web
/// feed serializer. (The `/feed.ninjs` export is the richer, canonical serializer; if
/// these ever need to be identical, unify them into one shared builder.)
fn ninjs_delivery_value(item: &NinjsStory) -> serde_json::Value {
    let body_text = item
        .body
        .as_array()
        .map(|blocks| {
            blocks
                .iter()
                .filter_map(|block| block.get("text").and_then(serde_json::Value::as_str))
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .collect::<Vec<_>>()
                .join("\n\n")
        })
        .unwrap_or_default();
    let mut subjects = vec![serde_json::json!({ "name": item.category })];
    if let Some(tags) = item.tags.as_array() {
        for qcode in tags.iter().filter_map(serde_json::Value::as_str) {
            if let Some(label) = meridian_contracts::media_topic_label(qcode) {
                subjects.push(serde_json::json!({
                    "code": qcode,
                    "name": label,
                    "scheme": "http://cv.iptc.org/newscodes/mediatopic/",
                }));
            }
        }
    }
    let version_created = item
        .published_at
        .or(Some(item.updated_at))
        .and_then(|dt| dt.format(&time::format_description::well_known::Rfc3339).ok());
    serde_json::json!({
        "type": "text",
        "pubstatus": "usable",
        "language": "en",
        "versioncreated": version_created,
        "headline": item.title,
        "description_text": item.dek,
        "body_text": body_text,
        "byline": item.author_name.clone().unwrap_or_else(|| "Meridian Newsroom".to_owned()),
        "copyrightholder": "Meridian Newsroom",
        "subject": subjects,
    })
}

/// Builds one receipt per channel for a publish.
///
/// Named `synthesize_*` because that is what it does: the receipts are composed
/// here rather than returned by a delivery system. See [`UNDELIVERED_STATUS`].
fn synthesize_receipts(channels: &serde_json::Value, now: OffsetDateTime) -> serde_json::Value {
    let at = now
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default();
    let receipts: Vec<serde_json::Value> = channels
        .as_array()
        .map(|list| {
            list.iter()
                .map(|channel| {
                    // A channel may be a bare name or an object carrying one.
                    let name = channel
                        .as_str()
                        .or_else(|| channel.get("name").and_then(serde_json::Value::as_str))
                        .unwrap_or("unknown");
                    serde_json::json!({
                        "channel": name,
                        "status": UNDELIVERED_STATUS,
                        "detail": "No delivery integration is configured for this channel",
                        "at": at,
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    serde_json::Value::Array(receipts)
}

/// The channel names a receipts array already covers, so a per-channel sweep never
/// re-delivers a channel that has already gone out.
fn delivered_channel_names(receipts: &serde_json::Value) -> std::collections::HashSet<String> {
    receipts
        .as_array()
        .map(|list| {
            list.iter()
                // A `failed` channel is a retryable delivery, not a completed one:
                // excluding it keeps the per-channel release scheduled so the next
                // sweep resends, instead of marking it published with a channel down.
                // `delivered` and `not_delivered` (no delivery layer) are terminal.
                .filter(|r| r.get("status").and_then(serde_json::Value::as_str) != Some("failed"))
                .filter_map(|r| r.get("channel").and_then(serde_json::Value::as_str).map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

/// When a channel is due to go out: its own time from `channel_schedule` if set and
/// parseable, otherwise the release's base `scheduled_at`.
fn effective_channel_time(
    channel: &str,
    channel_schedule: &serde_json::Value,
    scheduled_at: Option<OffsetDateTime>,
) -> Option<OffsetDateTime> {
    channel_schedule
        .get(channel)
        .and_then(serde_json::Value::as_str)
        .filter(|s| !s.is_empty())
        .and_then(|s| OffsetDateTime::parse(s, &time::format_description::well_known::Rfc3339).ok())
        .or(scheduled_at)
}

#[cfg(test)]
mod tests {
    use super::{delivered_channel_names, effective_channel_time};
    use serde_json::json;
    use time::OffsetDateTime;
    use time::format_description::well_known::Rfc3339;

    fn at(s: &str) -> OffsetDateTime {
        OffsetDateTime::parse(s, &Rfc3339).expect("valid RFC-3339")
    }

    #[test]
    fn delivered_names_counts_only_non_failed_channels() {
        let receipts = json!([
            {"channel": "web", "status": "delivered", "at": "2026-08-05T09:00:00Z"},
            {"channel": "newsletter", "status": "not_delivered", "at": "2026-08-05T12:00:00Z"},
            {"channel": "push", "status": "failed"}, // failed -> retryable, must NOT count
            {"status": "failed"}, // no channel -> skipped
        ]);
        let names = delivered_channel_names(&receipts);
        assert_eq!(names.len(), 2);
        assert!(names.contains("web"));
        assert!(names.contains("newsletter"));
        assert!(!names.contains("push"));
    }

    #[test]
    fn delivered_names_is_empty_for_non_arrays() {
        assert!(delivered_channel_names(&json!(null)).is_empty());
        assert!(delivered_channel_names(&json!({})).is_empty());
        assert!(delivered_channel_names(&json!([])).is_empty());
    }

    #[test]
    fn a_scheduled_channel_uses_its_own_time() {
        let schedule = json!({ "newsletter": "2026-08-05T12:00:00Z" });
        let base = Some(at("2026-08-05T09:00:00Z"));
        assert_eq!(
            effective_channel_time("newsletter", &schedule, base),
            Some(at("2026-08-05T12:00:00Z"))
        );
    }

    #[test]
    fn an_unscheduled_channel_falls_back_to_the_base_slot() {
        let schedule = json!({ "newsletter": "2026-08-05T12:00:00Z" });
        let base = Some(at("2026-08-05T09:00:00Z"));
        // "web" isn't in the map -> base slot.
        assert_eq!(effective_channel_time("web", &schedule, base), base);
    }

    #[test]
    fn an_empty_or_invalid_time_falls_back_to_the_base_slot() {
        let base = Some(at("2026-08-05T09:00:00Z"));
        assert_eq!(effective_channel_time("web", &json!({ "web": "" }), base), base);
        assert_eq!(effective_channel_time("web", &json!({ "web": "not-a-date" }), base), base);
        assert_eq!(effective_channel_time("web", &json!({ "web": 42 }), base), base);
    }

    #[test]
    fn no_time_anywhere_is_none() {
        assert_eq!(effective_channel_time("web", &json!({}), None), None);
        // A per-channel time still resolves even without a base slot.
        assert_eq!(
            effective_channel_time("web", &json!({ "web": "2026-08-05T09:00:00Z" }), None),
            Some(at("2026-08-05T09:00:00Z"))
        );
    }
}
