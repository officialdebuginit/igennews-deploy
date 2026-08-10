//! Module 10 (people) — user directory, self-service profile, admin user
//! updates, and desk membership. Ported from the legacy `api.py` people
//! handlers.
//!
//! Deferred: account creation (`POST /users`, password hashing — an identity
//! concern), plus `preferences` and `profile_visibility`, which are not yet
//! columns in the Rust users schema. Contact-detail redaction here approximates
//! the legacy rule: editors/admins/self see the email, others do not.

use serde::Serialize;
use sqlx::FromRow;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::{Actor, NewsroomError, NewsroomService, authz};

/// Maps a unique-violation on email/handle to a domain conflict.
fn duplicate_user(error: sqlx::Error) -> NewsroomError {
    if let sqlx::Error::Database(ref db_error) = error
        && db_error.is_unique_violation()
    {
        return NewsroomError::Conflict("A user with that email or handle already exists".to_owned());
    }
    NewsroomError::Database(error)
}

/// A user row as stored.
#[derive(Clone, Debug, FromRow)]
struct UserRow {
    id: Uuid,
    email: String,
    handle: String,
    display_name: String,
    role: String,
    department: Option<String>,
    is_active: bool,
    is_admin: bool,
    created_at: OffsetDateTime,
    /// The curated profile blob; avatar/cover asset ids are pulled from it so the
    /// directory can render photos without a second query per user.
    profile: serde_json::Value,
}

/// A user's curated professional profile document with its owner's identity and
/// the viewer's edit permission. The `profile` blob is UI-shaped.
#[derive(Debug, Serialize)]
pub struct RichProfile {
    pub id: Uuid,
    pub display_name: String,
    pub handle: String,
    pub role: String,
    pub department: Option<String>,
    pub profile: serde_json::Value,
    pub can_edit: bool,
}

/// Aggregate counts for a single user's newsroom footprint, shown on the admin
/// analytics panel of a profile.
#[derive(Debug, Default, Serialize, FromRow)]
pub struct AdminStats {
    pub stories: i64,
    pub pitches: i64,
    pub feed_posts: i64,
    pub comments: i64,
    pub following: i64,
    pub followers: i64,
    pub active_sessions: i64,
}

/// One audit-log line about a user, trimmed for the activity timeline.
#[derive(Debug, Serialize, FromRow)]
pub struct AdminActivity {
    pub action: String,
    pub entity_type: String,
    pub entity_id: String,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

/// A sign-in session (device) for the login-history table.
#[derive(Debug, Serialize, FromRow)]
pub struct AdminSession {
    pub ip_address: Option<String>,
    pub user_agent: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339::option")]
    pub last_seen_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub revoked_at: Option<OffsetDateTime>,
}

/// A story authored by the user, trimmed for the admin editorial list.
#[derive(Debug, Serialize, FromRow)]
pub struct AdminStory {
    pub id: Uuid,
    pub title: String,
    pub slug: String,
    pub workflow_state: String,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
}

/// A pitch created by or assigned to the user, for the admin editorial list.
#[derive(Debug, Serialize, FromRow)]
pub struct AdminPitch {
    pub id: Uuid,
    pub headline: String,
    pub status: String,
    pub priority: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

/// Every editorial item under one user — the stories and pitches an admin manages
/// on their behalf. Assembled by [`NewsroomService::admin_editorial_items`].
#[derive(Debug, Serialize)]
pub struct AdminEditorial {
    pub stories: Vec<AdminStory>,
    pub pitches: Vec<AdminPitch>,
    pub active_stories: i64,
}

/// An admin-only analytical overview of one user: identity, aggregate counts,
/// recent audit activity, and session history. Assembled by [`NewsroomService::admin_overview`].
#[derive(Debug, Serialize)]
pub struct AdminOverview {
    pub id: Uuid,
    pub display_name: String,
    pub handle: String,
    pub email: String,
    pub role: String,
    pub department: Option<String>,
    pub is_active: bool,
    pub is_admin: bool,
    /// Avatar asset id (or external URL) pulled from the profile blob, so the
    /// console can render the user's photo.
    pub avatar: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339::option")]
    pub updated_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub last_active: Option<OffsetDateTime>,
    pub stats: AdminStats,
    pub activity: Vec<AdminActivity>,
    pub sessions: Vec<AdminSession>,
}

/// A user profile as returned; `email` is redacted for non-privileged viewers.
#[derive(Clone, Debug, Serialize)]
pub struct UserProfile {
    pub id: Uuid,
    pub email: Option<String>,
    pub handle: String,
    pub display_name: String,
    pub role: String,
    pub department: Option<String>,
    pub is_active: bool,
    pub is_admin: bool,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    // The self-service profile block, matching the legacy user contract. The Rust
    // users table does not carry these columns yet, so the optionals serialize as
    // `null` and the string/collection fields take the legacy defaults; populate
    // them here once the columns exist.
    pub avatar_url: Option<String>,
    /// Cover/banner asset id (or external URL), pulled from the profile blob.
    pub cover: Option<String>,
    pub bio: Option<String>,
    pub pronouns: Option<String>,
    pub title: Option<String>,
    pub location: Option<String>,
    pub phone: Option<String>,
    pub timezone: String,
    pub availability_status: String,
    pub availability_until: Option<String>,
    pub profile_visibility: String,
    pub languages: Vec<String>,
    pub skills: Vec<String>,
    pub preferences: serde_json::Value,
    pub working_hours: serde_json::Value,
}

impl UserProfile {
    fn from_row(row: UserRow, reveal_contact: bool) -> Self {
        Self {
            id: row.id,
            email: reveal_contact.then_some(row.email),
            handle: row.handle,
            display_name: row.display_name,
            role: row.role,
            department: row.department,
            is_active: row.is_active,
            is_admin: row.is_admin,
            created_at: row.created_at,
            avatar_url: row
                .profile
                .get("avatar")
                .and_then(serde_json::Value::as_str)
                .filter(|s| !s.is_empty())
                .map(ToOwned::to_owned),
            cover: row
                .profile
                .get("cover")
                .and_then(serde_json::Value::as_str)
                .filter(|s| !s.is_empty())
                .map(ToOwned::to_owned),
            bio: None,
            pronouns: None,
            title: None,
            location: None,
            phone: None,
            timezone: "UTC".to_owned(),
            availability_status: "available".to_owned(),
            availability_until: None,
            profile_visibility: "newsroom".to_owned(),
            languages: Vec::new(),
            skills: Vec::new(),
            preferences: serde_json::json!({}),
            working_hours: serde_json::json!({}),
        }
    }
}

/// Self-service profile edits.
#[derive(Clone, Debug, Default)]
pub struct ProfileUpdate {
    pub display_name: Option<String>,
    pub department: Option<String>,
}

/// A new user account. `password_hash` is produced by the identity crate at the
/// web boundary, so this crate never handles plaintext passwords.
#[derive(Clone, Debug)]
pub struct NewUser {
    pub email: String,
    pub handle: String,
    pub display_name: String,
    pub role: String,
    pub department: Option<String>,
    pub password_hash: String,
}

/// Admin user edits.
#[derive(Clone, Debug, Default)]
pub struct UserUpdate {
    pub display_name: Option<String>,
    pub role: Option<String>,
    pub department: Option<String>,
    pub is_active: Option<bool>,
}

const USER_COLUMNS: &str =
    "id, email, handle, display_name, role, department, is_active, is_admin, created_at, profile";

const NEWSROOM_ROLES: [&str; 12] = [
    "admin",
    "editor_in_chief",
    "managing_editor",
    "section_editor",
    "reporter",
    "copy_editor",
    "fact_checker",
    "producer",
    "standards_legal",
    "audience_editor",
    "contributor",
    "viewer",
];

impl NewsroomService {
    /// Whether the viewer holds the org-wide `people.view_contact` capability.
    ///
    /// Resolved once per listing rather than per row: the capability does not
    /// depend on the subject, only the "it is my own profile" arm does.
    async fn may_view_contacts(&self, viewer: &Actor) -> Result<bool, NewsroomError> {
        authz::has(self.pool(), viewer, "people.view_contact", None).await
    }

    /// The user directory, ordered by display name, optionally filtered by role.
    ///
    /// # Errors
    /// Propagates database failures.
    pub async fn list_users(
        &self,
        viewer: &Actor,
        role: Option<&str>,
    ) -> Result<Vec<UserProfile>, NewsroomError> {
        let rows = sqlx::query_as::<_, UserRow>(&format!(
            "SELECT {USER_COLUMNS} FROM meridian.users \
             WHERE ($1::text IS NULL OR role = $1) ORDER BY display_name"
        ))
        .bind(role)
        .fetch_all(self.pool())
        .await?;
        let may_view = self.may_view_contacts(viewer).await?;
        Ok(rows
            .into_iter()
            .map(|row| {
                let reveal = may_view || viewer.id == row.id;
                UserProfile::from_row(row, reveal)
            })
            .collect())
    }

    /// One user profile.
    ///
    /// # Errors
    /// [`NewsroomError::NotFound`]; database failures.
    pub async fn get_user(
        &self,
        viewer: &Actor,
        user_id: Uuid,
    ) -> Result<UserProfile, NewsroomError> {
        let row = sqlx::query_as::<_, UserRow>(&format!(
            "SELECT {USER_COLUMNS} FROM meridian.users WHERE id = $1"
        ))
        .bind(user_id)
        .fetch_optional(self.pool())
        .await?
        .ok_or(NewsroomError::NotFound("User"))?;
        let reveal = viewer.id == user_id || self.may_view_contacts(viewer).await?;
        Ok(UserProfile::from_row(row, reveal))
    }

    /// Updates the caller's own profile (display name, department).
    ///
    /// # Errors
    /// Propagates database failures.
    pub async fn update_own_profile(
        &self,
        actor: &Actor,
        update: &ProfileUpdate,
    ) -> Result<UserProfile, NewsroomError> {
        let mut tx = self.pool().begin().await?;
        let row = sqlx::query_as::<_, UserRow>(&format!(
            "UPDATE meridian.users SET \
               display_name = COALESCE($2, display_name), \
               department = COALESCE($3, department), \
               updated_at = now() \
             WHERE id = $1 RETURNING {USER_COLUMNS}"
        ))
        .bind(actor.id)
        .bind(&update.display_name)
        .bind(&update.department)
        .fetch_one(&mut *tx)
        .await?;
        crate::audit(
            &mut *tx,
            actor.id,
            "user.profile.updated",
            "user",
            &actor.id.to_string(),
            None,
            None,
        )
        .await?;
        tx.commit().await?;
        Ok(UserProfile::from_row(row, true))
    }

    /// A person's curated professional profile (headline, bio, experience,
    /// education, editorials, social links), plus whether the viewer may edit it.
    /// A profile marked `private` shows its body only to the owner and admins.
    ///
    /// # Errors
    /// [`NewsroomError::NotFound`] for an unknown user; database failures.
    pub async fn rich_profile(
        &self,
        viewer: &Actor,
        user_id: Uuid,
    ) -> Result<RichProfile, NewsroomError> {
        #[derive(FromRow)]
        struct Row {
            id: Uuid,
            display_name: String,
            handle: String,
            role: String,
            department: Option<String>,
            profile: serde_json::Value,
        }
        let row = sqlx::query_as::<_, Row>(
            "SELECT id, display_name, handle, role, department, profile \
             FROM meridian.users WHERE id = $1",
        )
        .bind(user_id)
        .fetch_optional(self.pool())
        .await?
        .ok_or(NewsroomError::NotFound("User"))?;
        let can_edit = viewer.id == user_id || viewer.is_admin;
        let private = row.profile.get("visibility").and_then(serde_json::Value::as_str) == Some("private");
        let profile = if private && !can_edit {
            serde_json::json!({ "visibility": "private" })
        } else {
            row.profile
        };
        Ok(RichProfile {
            id: row.id,
            display_name: row.display_name,
            handle: row.handle,
            role: row.role,
            department: row.department,
            profile,
            can_edit,
        })
    }

    /// Replaces a user's professional profile document. Only the account holder or
    /// an admin may write it.
    ///
    /// # Errors
    /// [`NewsroomError::Forbidden`] for anyone else; database failures.
    pub async fn set_rich_profile(
        &self,
        actor: &Actor,
        user_id: Uuid,
        profile: serde_json::Value,
    ) -> Result<RichProfile, NewsroomError> {
        if actor.id != user_id && !actor.is_admin {
            return Err(NewsroomError::Forbidden {
                capability: "users.manage".to_owned(),
                reason: "Only the account holder or an admin can edit this profile".to_owned(),
            });
        }
        let mut tx = self.pool().begin().await?;
        sqlx::query("UPDATE meridian.users SET profile = $2, updated_at = now() WHERE id = $1")
            .bind(user_id)
            .bind(&profile)
            .execute(&mut *tx)
            .await?;
        crate::audit(&mut *tx, actor.id, "user.profile.content_updated", "user", &user_id.to_string(), None, None).await?;
        tx.commit().await?;
        self.rich_profile(actor, user_id).await
    }

    /// Builds the admin analytics overview for one user: identity, footprint
    /// counts, recent audit activity, and session history. Admin only.
    ///
    /// # Errors
    /// [`NewsroomError::Forbidden`] for non-admins; [`NewsroomError::NotFound`]
    /// for an unknown user; database failures.
    pub async fn admin_overview(
        &self,
        actor: &Actor,
        user_id: Uuid,
    ) -> Result<AdminOverview, NewsroomError> {
        if !actor.is_admin {
            return Err(NewsroomError::Forbidden {
                capability: "users.manage".to_owned(),
                reason: "Admin access is required to view account analytics".to_owned(),
            });
        }
        #[derive(FromRow)]
        struct Account {
            id: Uuid,
            display_name: String,
            handle: String,
            email: String,
            role: String,
            department: Option<String>,
            is_active: bool,
            is_admin: bool,
            created_at: OffsetDateTime,
            updated_at: Option<OffsetDateTime>,
            profile: serde_json::Value,
        }
        let account = sqlx::query_as::<_, Account>(
            "SELECT id, display_name, handle, email, role, department, is_active, is_admin, \
                    created_at, updated_at, profile \
             FROM meridian.users WHERE id = $1",
        )
        .bind(user_id)
        .fetch_optional(self.pool())
        .await?
        .ok_or(NewsroomError::NotFound("User"))?;

        let stats = sqlx::query_as::<_, AdminStats>(
            "SELECT \
               (SELECT count(*) FROM meridian.stories     WHERE author_id = $1) AS stories, \
               (SELECT count(*) FROM meridian.pitches      WHERE assignee_id = $1) AS pitches, \
               (SELECT count(*) FROM meridian.feed_posts   WHERE author_id = $1) AS feed_posts, \
               (SELECT count(*) FROM meridian.comments     WHERE author_id = $1) AS comments, \
               (SELECT count(*) FROM meridian.follows      WHERE user_id = $1) AS following, \
               (SELECT count(*) FROM meridian.follows      WHERE entity_type = 'user' AND entity_id::text = $1::text) AS followers, \
               (SELECT count(*) FROM meridian.sessions     WHERE user_id = $1 AND revoked_at IS NULL AND expires_at > now()) AS active_sessions",
        )
        .bind(user_id)
        .fetch_one(self.pool())
        .await?;

        let activity = sqlx::query_as::<_, AdminActivity>(
            "SELECT action, entity_type, entity_id, created_at \
             FROM meridian.audit_events \
             WHERE actor_id = $1 OR (entity_type = 'user' AND entity_id = $1::text) \
             ORDER BY created_at DESC LIMIT 25",
        )
        .bind(user_id)
        .fetch_all(self.pool())
        .await?;

        let sessions = sqlx::query_as::<_, AdminSession>(
            "SELECT ip_address::text AS ip_address, user_agent, created_at, last_seen_at, revoked_at \
             FROM meridian.sessions WHERE user_id = $1 \
             ORDER BY created_at DESC LIMIT 12",
        )
        .bind(user_id)
        .fetch_all(self.pool())
        .await?;

        let last_active = sessions
            .iter()
            .filter_map(|s| s.last_seen_at.or(Some(s.created_at)))
            .max();

        Ok(AdminOverview {
            id: account.id,
            display_name: account.display_name,
            handle: account.handle,
            email: account.email,
            role: account.role,
            department: account.department,
            is_active: account.is_active,
            is_admin: account.is_admin,
            avatar: account
                .profile
                .get("avatar")
                .and_then(serde_json::Value::as_str)
                .filter(|s| !s.is_empty())
                .map(ToOwned::to_owned),
            created_at: account.created_at,
            updated_at: account.updated_at,
            last_active,
            stats,
            activity,
            sessions,
        })
    }

    /// Lists every editorial item under one user — stories they authored and
    /// pitches they created or were assigned — for the admin management console.
    /// Admin only.
    ///
    /// # Errors
    /// [`NewsroomError::Forbidden`] for non-admins; database failures.
    pub async fn admin_editorial_items(
        &self,
        actor: &Actor,
        user_id: Uuid,
    ) -> Result<AdminEditorial, NewsroomError> {
        if !actor.is_admin {
            return Err(NewsroomError::Forbidden {
                capability: "users.manage".to_owned(),
                reason: "Admin access is required to view a user's editorial items".to_owned(),
            });
        }
        let stories = sqlx::query_as::<_, AdminStory>(
            "SELECT id, title, slug, workflow_state, updated_at \
             FROM meridian.stories WHERE author_id = $1 \
             ORDER BY updated_at DESC LIMIT 50",
        )
        .bind(user_id)
        .fetch_all(self.pool())
        .await?;
        let pitches = sqlx::query_as::<_, AdminPitch>(
            "SELECT id, headline, status::text AS status, priority::text AS priority, created_at \
             FROM meridian.pitches WHERE assignee_id = $1 OR created_by_id = $1 \
             ORDER BY created_at DESC LIMIT 50",
        )
        .bind(user_id)
        .fetch_all(self.pool())
        .await?;
        let active_stories = stories
            .iter()
            .filter(|s| !matches!(s.workflow_state.as_str(), "published" | "archived"))
            .count() as i64;
        Ok(AdminEditorial {
            stories,
            pitches,
            active_stories,
        })
    }

    /// Authorizes an admin-initiated account action (password reset or recovery
    /// email), records it in the audit log so it appears on the user's activity
    /// timeline, and returns the target's email address for out-of-band delivery.
    ///
    /// # Errors
    /// [`NewsroomError::Forbidden`] for non-admins; [`NewsroomError::NotFound`]
    /// for an unknown user; database failures.
    pub async fn admin_account_action(
        &self,
        actor: &Actor,
        user_id: Uuid,
        action: &str,
    ) -> Result<String, NewsroomError> {
        if !actor.is_admin {
            return Err(NewsroomError::Forbidden {
                capability: "users.manage".to_owned(),
                reason: "Admin access is required for account recovery actions".to_owned(),
            });
        }
        let audit_action = match action {
            "reset" => "user.password_reset.sent_by_admin",
            "recovery" => "user.recovery_email.sent_by_admin",
            "password" => "user.password.set_by_admin",
            other => {
                return Err(NewsroomError::Unprocessable(format!(
                    "'{other}' is not a supported account action"
                )));
            }
        };
        let email: Option<String> =
            sqlx::query_scalar("SELECT email FROM meridian.users WHERE id = $1")
                .bind(user_id)
                .fetch_optional(self.pool())
                .await?;
        let email = email.ok_or(NewsroomError::NotFound("User"))?;
        let mut tx = self.pool().begin().await?;
        crate::audit(
            &mut *tx,
            actor.id,
            audit_action,
            "user",
            &user_id.to_string(),
            None,
            None,
        )
        .await?;
        tx.commit().await?;
        Ok(email)
    }

    /// Creates a user account; admin only. `is_admin` follows the admin role.
    ///
    /// # Errors
    /// [`NewsroomError::Forbidden`] for a non-admin; [`NewsroomError::Unprocessable`]
    /// for an unknown role; [`NewsroomError::Conflict`] for a duplicate email or
    /// handle; database failures.
    pub async fn create_user(
        &self,
        actor: &Actor,
        new_user: &NewUser,
    ) -> Result<UserProfile, NewsroomError> {
        if !actor.is_admin {
            return Err(NewsroomError::Forbidden {
                capability: "users.manage".to_owned(),
                reason: "An admin role is required".to_owned(),
            });
        }
        if !NEWSROOM_ROLES.contains(&new_user.role.as_str()) {
            return Err(NewsroomError::Unprocessable(format!(
                "'{}' is not a newsroom role",
                new_user.role
            )));
        }
        let mut tx = self.pool().begin().await?;
        let row = sqlx::query_as::<_, UserRow>(&format!(
            "INSERT INTO meridian.users \
             (id, email, handle, display_name, password_hash, role, department, is_admin) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, ($6 = 'admin')) RETURNING {USER_COLUMNS}"
        ))
        .bind(Uuid::now_v7())
        .bind(&new_user.email)
        .bind(&new_user.handle)
        .bind(&new_user.display_name)
        .bind(&new_user.password_hash)
        .bind(&new_user.role)
        .bind(&new_user.department)
        .fetch_one(&mut *tx)
        .await
        .map_err(duplicate_user)?;
        crate::audit(
            &mut *tx,
            actor.id,
            "user.created",
            "user",
            &row.id.to_string(),
            None,
            Some(serde_json::json!({ "email": row.email, "role": row.role })),
        )
        .await?;
        tx.commit().await?;
        Ok(UserProfile::from_row(row, true))
    }

    /// Admin update of a user's display name, role, department, or active flag.
    /// `is_admin` is kept in lockstep with the admin role.
    ///
    /// # Errors
    /// [`NewsroomError::Forbidden`] for a non-admin; [`NewsroomError::Unprocessable`]
    /// for an unknown role; [`NewsroomError::NotFound`]; database failures.
    pub async fn admin_update_user(
        &self,
        actor: &Actor,
        user_id: Uuid,
        update: &UserUpdate,
    ) -> Result<UserProfile, NewsroomError> {
        if !actor.is_admin {
            return Err(NewsroomError::Forbidden {
                capability: "users.manage".to_owned(),
                reason: "An admin role is required".to_owned(),
            });
        }
        if let Some(role) = &update.role
            && !NEWSROOM_ROLES.contains(&role.as_str())
        {
            return Err(NewsroomError::Unprocessable(format!(
                "'{role}' is not a newsroom role"
            )));
        }
        let mut tx = self.pool().begin().await?;
        let row = sqlx::query_as::<_, UserRow>(&format!(
            "UPDATE meridian.users SET \
               display_name = COALESCE($2, display_name), \
               role = COALESCE($3, role), \
               department = COALESCE($4, department), \
               is_active = COALESCE($5, is_active), \
               is_admin = (COALESCE($3, role) = 'admin'), \
               updated_at = now() \
             WHERE id = $1 RETURNING {USER_COLUMNS}"
        ))
        .bind(user_id)
        .bind(&update.display_name)
        .bind(&update.role)
        .bind(&update.department)
        .bind(update.is_active)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(NewsroomError::NotFound("User"))?;
        crate::audit(
            &mut *tx,
            actor.id,
            "user.updated",
            "user",
            &user_id.to_string(),
            None,
            Some(serde_json::json!({ "role": row.role, "is_active": row.is_active })),
        )
        .await?;
        tx.commit().await?;
        Ok(UserProfile::from_row(row, true))
    }

    /// A desk's members (as user profiles).
    ///
    /// # Errors
    /// [`NewsroomError::NotFound`] for an unknown desk; database failures.
    pub async fn list_desk_members(
        &self,
        viewer: &Actor,
        desk_id: Uuid,
    ) -> Result<Vec<UserProfile>, NewsroomError> {
        self.get_desk(desk_id).await?;
        let rows = sqlx::query_as::<_, UserRow>(
            // Columns are `u.`-qualified: `desk_memberships` also has a `role`, so
            // the bare `role` in USER_COLUMNS would be ambiguous across the join.
            "SELECT u.id, u.email, u.handle, u.display_name, u.role, u.department, \
                    u.is_active, u.is_admin, u.created_at FROM meridian.users u \
             JOIN meridian.desk_memberships m ON m.user_id = u.id \
             WHERE m.desk_id = $1 ORDER BY u.display_name",
        )
        .bind(desk_id)
        .fetch_all(self.pool())
        .await?;
        let may_view = self.may_view_contacts(viewer).await?;
        Ok(rows
            .into_iter()
            .map(|row| {
                let reveal = may_view || viewer.id == row.id;
                UserProfile::from_row(row, reveal)
            })
            .collect())
    }

    /// Adds a member to a desk; requires desk-admin rights.
    ///
    /// # Errors
    /// [`NewsroomError::Forbidden`]; [`NewsroomError::NotFound`]; database failures.
    pub async fn add_desk_member(
        &self,
        actor: &Actor,
        desk_id: Uuid,
        user_id: Uuid,
        role: &str,
    ) -> Result<(), NewsroomError> {
        crate::desks::validate_workspace_role(role)?;
        let desk = self.get_desk(desk_id).await?;
        self.require_desk_admin(actor, &desk).await?;
        let user_exists: bool =
            sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM meridian.users WHERE id = $1)")
                .bind(user_id)
                .fetch_one(self.pool())
                .await?;
        if !user_exists {
            return Err(NewsroomError::NotFound("User"));
        }
        let mut tx = self.pool().begin().await?;
        sqlx::query(
            "INSERT INTO meridian.desk_memberships (desk_id, user_id, role) VALUES ($1, $2, $3) \
             ON CONFLICT (desk_id, user_id) DO UPDATE SET role = EXCLUDED.role",
        )
        .bind(desk_id)
        .bind(user_id)
        .bind(role)
        .execute(&mut *tx)
        .await?;
        crate::audit(
            &mut *tx,
            actor.id,
            "desk.member.added",
            "desk",
            &desk_id.to_string(),
            None,
            Some(serde_json::json!({ "user_id": user_id, "role": role })),
        )
        .await?;
        Self::enqueue_event(
            &mut *tx,
            "desk.member_added",
            &desk_id.to_string(),
            None,
            serde_json::json!({ "desk_id": desk_id, "user_id": user_id, "role": role }),
            Some(actor.id),
        )
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Removes a member from a desk; requires desk-admin rights.
    ///
    /// # Errors
    /// [`NewsroomError::Forbidden`]; [`NewsroomError::NotFound`]; database failures.
    pub async fn remove_desk_member(
        &self,
        actor: &Actor,
        desk_id: Uuid,
        user_id: Uuid,
    ) -> Result<(), NewsroomError> {
        let desk = self.get_desk(desk_id).await?;
        self.require_desk_admin(actor, &desk).await?;
        let mut tx = self.pool().begin().await?;
        sqlx::query("DELETE FROM meridian.desk_memberships WHERE desk_id = $1 AND user_id = $2")
            .bind(desk_id)
            .bind(user_id)
            .execute(&mut *tx)
            .await?;
        crate::audit(
            &mut *tx,
            actor.id,
            "desk.member.removed",
            "desk",
            &desk_id.to_string(),
            None,
            Some(serde_json::json!({ "user_id": user_id })),
        )
        .await?;
        Self::enqueue_event(
            &mut *tx,
            "desk.member_removed",
            &desk_id.to_string(),
            None,
            serde_json::json!({ "desk_id": desk_id, "user_id": user_id }),
            Some(actor.id),
        )
        .await?;
        tx.commit().await?;
        Ok(())
    }
}
