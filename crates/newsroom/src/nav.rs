//! Navigation menu and feature flags, ported from the legacy `navigation`
//! package (`service.py` menu filtering and `flags.py`).
//!
//! Menu filtering here is presentation, not security: every endpoint behind a
//! link is independently capability-gated. This exists so the UI stops offering
//! people doors that will 403 in their face.
//!
//! Deferred to their proper modules: `badges` (unread notifications + activity
//! centre — Module 7), and favourites/recents (their own Module 14 tables).
//! Only the capability-filtered menu and feature flags are ported here.

use serde::Serialize;
use sha2::{Digest, Sha256};
use sqlx::FromRow;
use sqlx::types::Json;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::{Actor, NewsroomError, NewsroomService, audit, authz};

/// One navigation entry and the capability it requires (`None` = everyone).
#[derive(Clone, Copy, Debug)]
struct NavEntry {
    key: &'static str,
    label: &'static str,
    href: &'static str,
    icon: Option<&'static str>,
    capability: Option<&'static str>,
}

/// A navigation entry permitted to the caller, serialized for the UI.
#[derive(Clone, Debug, Serialize)]
pub struct NavItem {
    pub key: &'static str,
    pub label: &'static str,
    pub href: &'static str,
    // Always serialized, `null` when absent — the legacy `navigation` payload
    // includes `"icon": null` on quick actions, and the differential-parity
    // skeleton treats a missing key and a null value as different shapes.
    pub icon: Option<&'static str>,
}

const NAV_ITEMS: &[NavEntry] = &[
    NavEntry { key: "dashboard", label: "Dashboard", href: "/", icon: Some("layout-dashboard"), capability: Some("dashboard.view") },
    NavEntry { key: "stories", label: "Stories", href: "/stories", icon: Some("newspaper"), capability: None },
    NavEntry { key: "tasks", label: "Tasks", href: "/tasks", icon: Some("check-square"), capability: None },
    NavEntry { key: "calendar", label: "Calendar", href: "/calendar", icon: Some("calendar"), capability: None },
    NavEntry { key: "workspace", label: "Workspaces", href: "/workspace", icon: Some("layers"), capability: None },
    NavEntry { key: "team", label: "Team", href: "/team", icon: Some("users"), capability: None },
    NavEntry { key: "feed", label: "Feed", href: "/feed", icon: Some("message-square"), capability: None },
    NavEntry { key: "assets", label: "Assets", href: "/assets", icon: Some("image"), capability: None },
    NavEntry { key: "analytics", label: "Analytics", href: "/analytics", icon: Some("bar-chart"), capability: Some("dashboard.view_audience") },
    NavEntry { key: "admin", label: "Admin", href: "/admin", icon: Some("shield"), capability: Some("permissions.manage") },
    NavEntry { key: "settings", label: "Settings", href: "/settings", icon: Some("settings"), capability: None },
];

const QUICK_ACTIONS: &[NavEntry] = &[
    NavEntry { key: "new_story", label: "New story", href: "/editor?new=1", icon: None, capability: Some("stories.create") },
    NavEntry { key: "new_pitch", label: "New pitch", href: "/stories?pitch=1", icon: None, capability: Some("stories.create") },
    NavEntry { key: "new_task", label: "New task", href: "/tasks?new=1", icon: None, capability: Some("tasks.assign") },
    NavEntry { key: "new_desk", label: "New workspace", href: "/workspace?new=1", icon: None, capability: Some("desks.manage") },
];

/// A feature flag row. Target lists hold entity-id strings, as in the legacy
/// JSON columns.
#[derive(Clone, Debug, FromRow, Serialize)]
pub struct FeatureFlag {
    pub key: String,
    pub description: Option<String>,
    pub enabled: bool,
    pub rollout_percent: i32,
    pub target_user_ids: Json<Vec<String>>,
    pub target_desk_ids: Json<Vec<String>>,
    pub emergency_disabled: bool,
    // Legacy's feature-flag contract exposes `updated_at` but not `created_at`.
    #[serde(skip_serializing)]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
}

/// The mutable fields accepted when creating or updating a flag.
#[derive(Clone, Debug)]
pub struct FlagInput {
    pub description: Option<String>,
    pub enabled: bool,
    pub rollout_percent: i32,
    pub target_user_ids: Vec<String>,
    pub target_desk_ids: Vec<String>,
    pub emergency_disabled: bool,
}

/// Stable 0-99 bucket for a user within a flag. A hash rather than a random
/// draw so the bucket does not change between requests; per-flag so two flags at
/// the same percentage do not hit exactly the same slice of the newsroom.
#[must_use]
pub fn bucket_of(key: &str, user_id: Uuid) -> u32 {
    let digest = Sha256::digest(format!("{key}:{user_id}").as_bytes());
    let prefix = u32::from_be_bytes([digest[0], digest[1], digest[2], digest[3]]);
    prefix % 100
}

impl NewsroomService {
    /// The nav items and quick actions the caller may see.
    ///
    /// # Errors
    /// Propagates database failures from capability resolution.
    pub async fn navigation_menu(
        &self,
        actor: &Actor,
    ) -> Result<(Vec<NavItem>, Vec<NavItem>), NewsroomError> {
        let items = self.permitted(actor, NAV_ITEMS).await?;
        let actions = self.permitted(actor, QUICK_ACTIONS).await?;
        Ok((items, actions))
    }

    async fn permitted(
        &self,
        actor: &Actor,
        entries: &[NavEntry],
    ) -> Result<Vec<NavItem>, NewsroomError> {
        let mut permitted = Vec::new();
        for entry in entries {
            let allowed = match entry.capability {
                None => true,
                Some(capability) => authz::has(self.pool(), actor, capability, None).await?,
            };
            if allowed {
                permitted.push(NavItem {
                    key: entry.key,
                    label: entry.label,
                    href: entry.href,
                    icon: entry.icon,
                });
            }
        }
        Ok(permitted)
    }

    /// Resolves a single flag for `actor`, applying the kill switch, targeting,
    /// and percentage rollout in the legacy order.
    async fn evaluate(&self, flag: &FeatureFlag, actor: &Actor) -> Result<bool, NewsroomError> {
        // Kill switch first and separately from `enabled`.
        if flag.emergency_disabled || !flag.enabled {
            return Ok(false);
        }
        if flag.target_user_ids.0.iter().any(|id| id == &actor.id.to_string()) {
            return Ok(true);
        }
        if !flag.target_desk_ids.0.is_empty() {
            let desk_ids: Vec<Uuid> = sqlx::query_scalar(
                "SELECT desk_id FROM meridian.desk_memberships WHERE user_id = $1",
            )
            .bind(actor.id)
            .fetch_all(self.pool())
            .await?;
            if desk_ids
                .iter()
                .any(|id| flag.target_desk_ids.0.contains(&id.to_string()))
            {
                return Ok(true);
            }
        }
        if flag.rollout_percent >= 100 {
            return Ok(true);
        }
        if flag.rollout_percent <= 0 {
            return Ok(false);
        }
        Ok(bucket_of(&flag.key, actor.id) < u32::try_from(flag.rollout_percent).unwrap_or(0))
    }

    /// Every flag resolved to a plain boolean for this caller. Booleans only: a
    /// caller has no business knowing rollout percentages or who else is targeted.
    ///
    /// # Errors
    /// Propagates database failures.
    pub async fn evaluate_all_flags(
        &self,
        actor: &Actor,
    ) -> Result<Vec<(String, bool)>, NewsroomError> {
        let flags =
            sqlx::query_as::<_, FeatureFlag>("SELECT * FROM meridian.feature_flags ORDER BY key")
                .fetch_all(self.pool())
                .await?;
        let mut resolved = Vec::with_capacity(flags.len());
        for flag in flags {
            let value = self.evaluate(&flag, actor).await?;
            resolved.push((flag.key.clone(), value));
        }
        Ok(resolved)
    }

    /// Loads a flag or returns [`NewsroomError::NotFound`].
    ///
    /// # Errors
    /// Propagates database failures.
    pub async fn get_flag(&self, key: &str) -> Result<FeatureFlag, NewsroomError> {
        sqlx::query_as::<_, FeatureFlag>("SELECT * FROM meridian.feature_flags WHERE key = $1")
            .bind(key)
            .fetch_optional(self.pool())
            .await?
            .ok_or(NewsroomError::NotFound("Feature flag"))
    }

    /// Lists every flag definition; requires `flags.manage`.
    ///
    /// # Errors
    /// [`NewsroomError::Forbidden`]; database failures.
    pub async fn list_flag_definitions(
        &self,
        actor: &Actor,
    ) -> Result<Vec<FeatureFlag>, NewsroomError> {
        authz::require(self.pool(), actor, "flags.manage", None).await?;
        Ok(
            sqlx::query_as::<_, FeatureFlag>("SELECT * FROM meridian.feature_flags ORDER BY key")
                .fetch_all(self.pool())
                .await?,
        )
    }

    /// Creates a flag; requires `flags.manage`.
    ///
    /// # Errors
    /// [`NewsroomError::Conflict`] if the key exists; [`NewsroomError::Forbidden`];
    /// database failures.
    pub async fn create_flag(
        &self,
        actor: &Actor,
        key: &str,
        input: &FlagInput,
    ) -> Result<FeatureFlag, NewsroomError> {
        authz::require(self.pool(), actor, "flags.manage", None).await?;
        let exists: bool =
            sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM meridian.feature_flags WHERE key = $1)")
                .bind(key)
                .fetch_one(self.pool())
                .await?;
        if exists {
            return Err(NewsroomError::Conflict(format!(
                "Feature flag '{key}' already exists"
            )));
        }
        let mut tx = self.pool().begin().await?;
        let flag = sqlx::query_as::<_, FeatureFlag>(
            "INSERT INTO meridian.feature_flags \
             (key, description, enabled, rollout_percent, target_user_ids, target_desk_ids, emergency_disabled) \
             VALUES ($1, $2, $3, $4, $5, $6, $7) RETURNING *",
        )
        .bind(key)
        .bind(&input.description)
        .bind(input.enabled)
        .bind(input.rollout_percent)
        .bind(Json(&input.target_user_ids))
        .bind(Json(&input.target_desk_ids))
        .bind(input.emergency_disabled)
        .fetch_one(&mut *tx)
        .await?;
        audit(
            &mut *tx,
            actor.id,
            "flag.created",
            "feature_flag",
            key,
            None,
            Some(serde_json::json!({ "enabled": input.enabled })),
        )
        .await?;
        tx.commit().await?;
        Ok(flag)
    }

    /// Updates a flag's configuration; requires `flags.manage`.
    ///
    /// # Errors
    /// [`NewsroomError::Forbidden`]; [`NewsroomError::NotFound`]; database failures.
    pub async fn update_flag(
        &self,
        actor: &Actor,
        key: &str,
        input: &FlagInput,
    ) -> Result<FeatureFlag, NewsroomError> {
        authz::require(self.pool(), actor, "flags.manage", None).await?;
        let mut tx = self.pool().begin().await?;
        let flag = sqlx::query_as::<_, FeatureFlag>(
            "UPDATE meridian.feature_flags SET description = $2, enabled = $3, \
               rollout_percent = $4, target_user_ids = $5, target_desk_ids = $6, \
               emergency_disabled = $7, updated_at = now() \
             WHERE key = $1 RETURNING *",
        )
        .bind(key)
        .bind(&input.description)
        .bind(input.enabled)
        .bind(input.rollout_percent)
        .bind(Json(&input.target_user_ids))
        .bind(Json(&input.target_desk_ids))
        .bind(input.emergency_disabled)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(NewsroomError::NotFound("Feature flag"))?;
        audit(
            &mut *tx,
            actor.id,
            "flag.updated",
            "feature_flag",
            key,
            None,
            Some(serde_json::json!({ "enabled": flag.enabled })),
        )
        .await?;
        tx.commit().await?;
        Ok(flag)
    }

    /// Deletes a flag; requires `flags.manage`.
    ///
    /// # Errors
    /// [`NewsroomError::Forbidden`]; [`NewsroomError::NotFound`]; database failures.
    pub async fn delete_flag(&self, actor: &Actor, key: &str) -> Result<(), NewsroomError> {
        authz::require(self.pool(), actor, "flags.manage", None).await?;
        let mut tx = self.pool().begin().await?;
        let deleted = sqlx::query("DELETE FROM meridian.feature_flags WHERE key = $1")
            .bind(key)
            .execute(&mut *tx)
            .await?;
        if deleted.rows_affected() == 0 {
            return Err(NewsroomError::NotFound("Feature flag"));
        }
        audit(&mut *tx, actor.id, "flag.deleted", "feature_flag", key, None, None).await?;
        tx.commit().await?;
        Ok(())
    }

    /// Kills a feature now, leaving its configuration intact for the post-mortem.
    ///
    /// # Errors
    /// [`NewsroomError::Forbidden`]; [`NewsroomError::NotFound`]; database failures.
    pub async fn emergency_disable_flag(
        &self,
        actor: &Actor,
        key: &str,
    ) -> Result<FeatureFlag, NewsroomError> {
        authz::require(self.pool(), actor, "flags.manage", None).await?;
        let mut tx = self.pool().begin().await?;
        let flag = sqlx::query_as::<_, FeatureFlag>(
            "UPDATE meridian.feature_flags SET emergency_disabled = true, updated_at = now() \
             WHERE key = $1 RETURNING *",
        )
        .bind(key)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(NewsroomError::NotFound("Feature flag"))?;
        audit(
            &mut *tx,
            actor.id,
            "flag.emergency_disabled",
            "feature_flag",
            key,
            None,
            Some(serde_json::json!({ "emergency_disabled": true })),
        )
        .await?;
        tx.commit().await?;
        Ok(flag)
    }
}

#[cfg(test)]
mod tests {
    use super::bucket_of;
    use uuid::Uuid;

    #[test]
    fn bucket_is_stable_and_in_range() {
        let user = Uuid::now_v7();
        let first = bucket_of("beta.editor", user);
        assert_eq!(first, bucket_of("beta.editor", user));
        assert!(first < 100);
    }

    #[test]
    fn bucket_differs_per_flag_key() {
        // Not a guarantee for every pair, but the digest must not ignore the key.
        let user = Uuid::from_u128(0x1234_5678_9abc_def0_1234_5678_9abc_def0);
        assert_ne!(bucket_of("flag.a", user), bucket_of("flag.z", user));
    }
}
