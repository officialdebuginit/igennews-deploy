//! Module 7 — awareness: in-app notifications (with preference, digest,
//! grouping, and critical-override rules), plus follows, favourites, and
//! recents. Ported from the legacy `services.notify` and `awareness` package.
//!
//! Deferred within Module 7 (separate, larger pieces): full-text search, the
//! feed, the activity centre, and the escalation sweep.

use serde::Serialize;
use sqlx::{FromRow, PgConnection};
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

use crate::{Actor, NewsroomError, NewsroomService};

/// How long after its last heartbeat a viewer still counts as present.
///
/// Comfortably more than the client's heartbeat interval, so one dropped beat on a
/// slow connection does not make someone flicker out of the room and back.
const PRESENCE_WINDOW_SECONDS: i32 = 60;

/// One person currently viewing an entity.
#[derive(Clone, Debug, FromRow, Serialize)]
pub struct PresenceView {
    pub user_id: Uuid,
    pub display_name: String,
    #[serde(with = "time::serde::rfc3339")]
    pub last_seen_at: OffsetDateTime,
}

/// The most recent items kept per user; older recents are trimmed on write.
pub const RECENTS_LIMIT: i64 = 20;

/// Kinds that are always critical regardless of the caller's requested priority.
const CRITICAL_KINDS: [&str; 3] = ["release.failed", "correction.retraction", "legal.hold"];

/// Rank of a priority for min-priority comparisons.
fn priority_rank(priority: &str) -> i32 {
    match priority {
        "low" => 0,
        "high" => 2,
        "critical" => 3,
        // "normal" and any unknown value rank as normal.
        _ => 1,
    }
}

/// The digest hold for a preference, or `None` when the mode is unknown.
fn digest_delay(mode: &str) -> Option<Duration> {
    match mode {
        "immediate" => Some(Duration::ZERO),
        "hourly" => Some(Duration::hours(1)),
        "daily" => Some(Duration::days(1)),
        _ => None,
    }
}

#[derive(Clone, Debug, FromRow, Serialize)]
pub struct Notification {
    pub id: Uuid,
    // The list contract (`GET /notifications`) carries delivery metadata but not
    // `user_id`; the single-notification read contract is the mirror image (see
    // `ReadNotification`).
    #[serde(skip_serializing)]
    pub user_id: Uuid,
    pub kind: String,
    pub title: String,
    pub body: Option<String>,
    pub entity_type: Option<String>,
    pub entity_id: Option<String>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub read_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    pub priority: String,
    pub group_key: Option<String>,
    pub count: i32,
    #[serde(skip_serializing)]
    pub deliver_after: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub escalated_at: Option<OffsetDateTime>,
    #[serde(skip_serializing)]
    pub escalated_to_id: Option<Uuid>,
}

/// The single-notification read contract (`POST /notifications/{id}/read`) — the
/// core fields plus `user_id`, without the list's delivery/escalation metadata.
#[derive(Debug, Serialize)]
pub struct ReadNotification {
    pub id: Uuid,
    pub user_id: Uuid,
    pub kind: String,
    pub title: String,
    pub body: Option<String>,
    pub entity_type: Option<String>,
    pub entity_id: Option<String>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub read_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

impl From<Notification> for ReadNotification {
    fn from(n: Notification) -> Self {
        Self {
            id: n.id,
            user_id: n.user_id,
            kind: n.kind,
            title: n.title,
            body: n.body,
            entity_type: n.entity_type,
            entity_id: n.entity_id,
            read_at: n.read_at,
            created_at: n.created_at,
        }
    }
}

#[derive(Clone, Debug, FromRow, Serialize)]
pub struct NotificationPreference {
    pub user_id: Uuid,
    pub kind: String,
    pub in_app: bool,
    pub digest: String,
    pub min_priority: String,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
}

#[derive(Clone, Debug, FromRow, Serialize)]
pub struct Follow {
    #[serde(skip_serializing)]
    pub user_id: Uuid,
    pub entity_type: String,
    pub entity_id: String,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

#[derive(Clone, Debug, FromRow, Serialize)]
pub struct Favorite {
    #[serde(skip_serializing)]
    pub user_id: Uuid,
    pub entity_type: String,
    pub entity_id: String,
    pub label: String,
    pub position: i32,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

#[derive(Clone, Debug, FromRow, Serialize)]
pub struct RecentItem {
    #[serde(skip_serializing)]
    pub user_id: Uuid,
    pub entity_type: String,
    pub entity_id: String,
    pub title: String,
    #[serde(with = "time::serde::rfc3339")]
    pub visited_at: OffsetDateTime,
}

/// A preference entry to upsert.
#[derive(Clone, Debug)]
pub struct PreferenceInput {
    pub kind: String,
    pub in_app: bool,
    pub digest: String,
    pub min_priority: String,
}

/// The fields of a notification to deliver.
#[derive(Clone, Debug)]
pub struct NotifyInput<'a> {
    pub user_id: Uuid,
    pub kind: &'a str,
    pub title: &'a str,
    pub body: Option<&'a str>,
    pub entity_type: &'a str,
    pub entity_id: &'a str,
    pub priority: &'a str,
    pub group_key: Option<&'a str>,
}

/// Delivers an in-app notification, honouring the recipient's preferences,
/// digest hold, and grouping. Takes a connection so it can run inside the same
/// transaction as the change that triggered it. Returns the new row, or `None`
/// when the notification was suppressed or folded into an existing group.
///
/// # Errors
/// Returns [`NewsroomError::Database`] on query failure.
pub async fn notify(
    conn: &mut PgConnection,
    input: &NotifyInput<'_>,
) -> Result<Option<Notification>, NewsroomError> {
    // Critical kinds ignore the requested priority and every preference.
    let priority = if CRITICAL_KINDS.contains(&input.kind) {
        "critical"
    } else {
        input.priority
    };
    let is_critical = priority == "critical";

    let preference = sqlx::query_as::<_, NotificationPreference>(
        "SELECT * FROM meridian.notification_preferences WHERE user_id = $1 AND kind = $2",
    )
    .bind(input.user_id)
    .bind(input.kind)
    .fetch_optional(&mut *conn)
    .await?;

    let mut deliver_after: Option<OffsetDateTime> = None;
    if let Some(pref) = &preference
        && !is_critical
    {
        if !pref.in_app || pref.digest == "off" {
            return Ok(None);
        }
        if priority_rank(priority) < priority_rank(&pref.min_priority) {
            return Ok(None);
        }
        if let Some(delay) = digest_delay(&pref.digest)
            && delay > Duration::ZERO
        {
            deliver_after = Some(OffsetDateTime::now_utc() + delay);
        }
    }

    if let Some(group_key) = input.group_key {
        // Fold into a live unread group rather than adding another row.
        let existing = sqlx::query_as::<_, (Uuid, String)>(
            "SELECT id, priority FROM meridian.notifications \
             WHERE user_id = $1 AND group_key = $2 AND read_at IS NULL \
             ORDER BY created_at DESC LIMIT 1",
        )
        .bind(input.user_id)
        .bind(group_key)
        .fetch_optional(&mut *conn)
        .await?;
        if let Some((existing_id, existing_priority)) = existing {
            let raised = if priority_rank(priority) > priority_rank(&existing_priority) {
                priority
            } else {
                existing_priority.as_str()
            };
            sqlx::query(
                "UPDATE meridian.notifications \
                 SET count = count + 1, title = $2, created_at = now(), priority = $3 WHERE id = $1",
            )
            .bind(existing_id)
            .bind(input.title)
            .bind(raised)
            .execute(&mut *conn)
            .await?;
            return Ok(None);
        }
    }

    let notification = sqlx::query_as::<_, Notification>(
        "INSERT INTO meridian.notifications \
         (id, user_id, kind, title, body, entity_type, entity_id, priority, group_key, deliver_after) \
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10) RETURNING *",
    )
    .bind(Uuid::now_v7())
    .bind(input.user_id)
    .bind(input.kind)
    .bind(input.title)
    .bind(input.body)
    .bind(input.entity_type)
    .bind(input.entity_id)
    .bind(priority)
    .bind(input.group_key)
    .bind(deliver_after)
    .fetch_one(&mut *conn)
    .await?;
    Ok(Some(notification))
}

impl NewsroomService {
    /// The caller's visible notifications: newest first, excluding digest-held
    /// rows not yet due.
    ///
    /// # Errors
    /// Propagates database failures.
    pub async fn list_notifications(
        &self,
        actor: &Actor,
        unread_only: bool,
        limit: i64,
    ) -> Result<Vec<Notification>, NewsroomError> {
        Ok(sqlx::query_as::<_, Notification>(
            "SELECT * FROM meridian.notifications \
             WHERE user_id = $1 \
               AND (NOT $2 OR read_at IS NULL) \
               AND (deliver_after IS NULL OR deliver_after <= now()) \
             ORDER BY created_at DESC LIMIT $3",
        )
        .bind(actor.id)
        .bind(unread_only)
        .bind(limit.clamp(1, 200))
        .fetch_all(self.pool())
        .await?)
    }

    /// Marks one of the caller's notifications read.
    ///
    /// # Errors
    /// [`NewsroomError::NotFound`]; database failures.
    pub async fn mark_notification_read(
        &self,
        actor: &Actor,
        notification_id: Uuid,
    ) -> Result<Notification, NewsroomError> {
        sqlx::query_as::<_, Notification>(
            "UPDATE meridian.notifications SET read_at = COALESCE(read_at, now()) \
             WHERE id = $1 AND user_id = $2 RETURNING *",
        )
        .bind(notification_id)
        .bind(actor.id)
        .fetch_optional(self.pool())
        .await?
        .ok_or(NewsroomError::NotFound("Notification"))
    }

    /// Marks every unread notification read; returns how many changed.
    ///
    /// # Errors
    /// Propagates database failures.
    pub async fn mark_all_notifications_read(&self, actor: &Actor) -> Result<u64, NewsroomError> {
        let result = sqlx::query(
            "UPDATE meridian.notifications SET read_at = now() \
             WHERE user_id = $1 AND read_at IS NULL",
        )
        .bind(actor.id)
        .execute(self.pool())
        .await?;
        Ok(result.rows_affected())
    }

    /// The caller's notification preferences.
    ///
    /// # Errors
    /// Propagates database failures.
    pub async fn get_preferences(
        &self,
        actor: &Actor,
    ) -> Result<Vec<NotificationPreference>, NewsroomError> {
        Ok(sqlx::query_as::<_, NotificationPreference>(
            "SELECT * FROM meridian.notification_preferences WHERE user_id = $1 ORDER BY kind",
        )
        .bind(actor.id)
        .fetch_all(self.pool())
        .await?)
    }

    /// Replaces the caller's preference set with `entries`, removing any omitted.
    ///
    /// # Errors
    /// Propagates database failures.
    pub async fn set_preferences(
        &self,
        actor: &Actor,
        entries: &[PreferenceInput],
    ) -> Result<Vec<NotificationPreference>, NewsroomError> {
        let mut tx = self.pool().begin().await?;
        let mut keep: Vec<String> = Vec::with_capacity(entries.len());
        for entry in entries {
            keep.push(entry.kind.clone());
            sqlx::query(
                "INSERT INTO meridian.notification_preferences \
                 (user_id, kind, in_app, digest, min_priority, updated_at) \
                 VALUES ($1,$2,$3,$4,$5, now()) \
                 ON CONFLICT (user_id, kind) DO UPDATE SET \
                   in_app = EXCLUDED.in_app, digest = EXCLUDED.digest, \
                   min_priority = EXCLUDED.min_priority, updated_at = now()",
            )
            .bind(actor.id)
            .bind(&entry.kind)
            .bind(entry.in_app)
            .bind(&entry.digest)
            .bind(&entry.min_priority)
            .execute(&mut *tx)
            .await?;
        }
        sqlx::query(
            "DELETE FROM meridian.notification_preferences \
             WHERE user_id = $1 AND NOT (kind = ANY($2))",
        )
        .bind(actor.id)
        .bind(&keep)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        self.get_preferences(actor).await
    }

    /// The caller's follows.
    ///
    /// # Errors
    /// Propagates database failures.
    pub async fn list_follows(&self, actor: &Actor) -> Result<Vec<Follow>, NewsroomError> {
        Ok(sqlx::query_as::<_, Follow>(
            "SELECT * FROM meridian.follows WHERE user_id = $1 ORDER BY created_at DESC",
        )
        .bind(actor.id)
        .fetch_all(self.pool())
        .await?)
    }

    /// Follows an entity (idempotent).
    ///
    /// # Errors
    /// Propagates database failures.
    pub async fn add_follow(
        &self,
        actor: &Actor,
        entity_type: &str,
        entity_id: &str,
    ) -> Result<Follow, NewsroomError> {
        Ok(sqlx::query_as::<_, Follow>(
            "INSERT INTO meridian.follows (user_id, entity_type, entity_id) VALUES ($1,$2,$3) \
             ON CONFLICT (user_id, entity_type, entity_id) DO UPDATE SET entity_id = EXCLUDED.entity_id \
             RETURNING *",
        )
        .bind(actor.id)
        .bind(entity_type)
        .bind(entity_id)
        .fetch_one(self.pool())
        .await?)
    }

    /// Unfollows an entity; returns whether a row was removed.
    ///
    /// # Errors
    /// Propagates database failures.
    pub async fn remove_follow(
        &self,
        actor: &Actor,
        entity_type: &str,
        entity_id: &str,
    ) -> Result<bool, NewsroomError> {
        let result = sqlx::query(
            "DELETE FROM meridian.follows WHERE user_id = $1 AND entity_type = $2 AND entity_id = $3",
        )
        .bind(actor.id)
        .bind(entity_type)
        .bind(entity_id)
        .execute(self.pool())
        .await?;
        Ok(result.rows_affected() > 0)
    }

    /// The caller's favourites, in display order.
    ///
    /// # Errors
    /// Propagates database failures.
    pub async fn list_favorites(&self, actor: &Actor) -> Result<Vec<Favorite>, NewsroomError> {
        Ok(sqlx::query_as::<_, Favorite>(
            "SELECT * FROM meridian.favorites WHERE user_id = $1 ORDER BY position, created_at",
        )
        .bind(actor.id)
        .fetch_all(self.pool())
        .await?)
    }

    /// Pins or updates a favourite.
    ///
    /// # Errors
    /// Propagates database failures.
    pub async fn add_favorite(
        &self,
        actor: &Actor,
        entity_type: &str,
        entity_id: &str,
        label: &str,
        position: i32,
    ) -> Result<Favorite, NewsroomError> {
        Ok(sqlx::query_as::<_, Favorite>(
            "INSERT INTO meridian.favorites (user_id, entity_type, entity_id, label, position) \
             VALUES ($1,$2,$3,$4,$5) \
             ON CONFLICT (user_id, entity_type, entity_id) DO UPDATE SET \
               label = EXCLUDED.label, position = EXCLUDED.position RETURNING *",
        )
        .bind(actor.id)
        .bind(entity_type)
        .bind(entity_id)
        .bind(label)
        .bind(position)
        .fetch_one(self.pool())
        .await?)
    }

    /// Removes a favourite; returns whether a row was removed.
    ///
    /// # Errors
    /// Propagates database failures.
    pub async fn remove_favorite(
        &self,
        actor: &Actor,
        entity_type: &str,
        entity_id: &str,
    ) -> Result<bool, NewsroomError> {
        let result = sqlx::query(
            "DELETE FROM meridian.favorites WHERE user_id = $1 AND entity_type = $2 AND entity_id = $3",
        )
        .bind(actor.id)
        .bind(entity_type)
        .bind(entity_id)
        .execute(self.pool())
        .await?;
        Ok(result.rows_affected() > 0)
    }

    /// The caller's most recent items, newest first.
    ///
    /// # Errors
    /// Propagates database failures.
    pub async fn list_recents(&self, actor: &Actor) -> Result<Vec<RecentItem>, NewsroomError> {
        Ok(sqlx::query_as::<_, RecentItem>(
            "SELECT * FROM meridian.recent_items WHERE user_id = $1 \
             ORDER BY visited_at DESC LIMIT $2",
        )
        .bind(actor.id)
        .bind(RECENTS_LIMIT)
        .fetch_all(self.pool())
        .await?)
    }

    /// Records that the caller opened something, keeping the list capped.
    ///
    /// # Errors
    /// Propagates database failures.
    pub async fn record_visit(
        &self,
        actor: &Actor,
        entity_type: &str,
        entity_id: &str,
        title: &str,
    ) -> Result<RecentItem, NewsroomError> {
        let mut tx = self.pool().begin().await?;
        let item = sqlx::query_as::<_, RecentItem>(
            "INSERT INTO meridian.recent_items (user_id, entity_type, entity_id, title) \
             VALUES ($1,$2,$3,$4) \
             ON CONFLICT (user_id, entity_type, entity_id) DO UPDATE SET \
               visited_at = now(), title = EXCLUDED.title RETURNING *",
        )
        .bind(actor.id)
        .bind(entity_type)
        .bind(entity_id)
        .bind(title)
        .fetch_one(&mut *tx)
        .await?;
        // Trim everything past the cap.
        sqlx::query(
            "DELETE FROM meridian.recent_items \
             WHERE user_id = $1 AND (entity_type, entity_id) IN ( \
               SELECT entity_type, entity_id FROM meridian.recent_items \
               WHERE user_id = $1 ORDER BY visited_at DESC OFFSET $2)",
        )
        .bind(actor.id)
        .bind(RECENTS_LIMIT)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(item)
    }

    /// Records that the caller currently has an entity open.
    ///
    /// One row per (entity, person), refreshed in place — heartbeats update a row
    /// rather than appending, so the table grows with distinct pairs and not with
    /// time.
    ///
    /// # Errors
    /// Propagates database failures.
    pub async fn touch_presence(
        &self,
        actor: &Actor,
        entity_type: &str,
        entity_id: &str,
    ) -> Result<(), NewsroomError> {
        sqlx::query(
            "INSERT INTO meridian.presence (entity_type, entity_id, user_id, last_seen_at) \
             VALUES ($1,$2,$3, now()) \
             ON CONFLICT (entity_type, entity_id, user_id) DO UPDATE SET last_seen_at = now()",
        )
        .bind(entity_type)
        .bind(entity_id)
        .bind(actor.id)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    /// Who else currently has an entity open.
    ///
    /// Liveness is decided here, by the window, rather than by deleting rows — so
    /// a browser that crashed simply ages out and there is no sweeper to get
    /// stuck. The caller is excluded: the question the UI asks is "who *else* is
    /// here", and including yourself would make every story look occupied.
    ///
    /// # Errors
    /// Propagates database failures.
    pub async fn presence_on(
        &self,
        actor: &Actor,
        entity_type: &str,
        entity_id: &str,
    ) -> Result<Vec<PresenceView>, NewsroomError> {
        Ok(sqlx::query_as::<_, PresenceView>(
            "SELECT u.id AS user_id, u.display_name, p.last_seen_at \
             FROM meridian.presence p JOIN meridian.users u ON u.id = p.user_id \
             WHERE p.entity_type = $1 AND p.entity_id = $2 AND p.user_id <> $3 \
               AND p.last_seen_at > now() - make_interval(secs => $4::double precision) \
             ORDER BY u.display_name",
        )
        .bind(entity_type)
        .bind(entity_id)
        .bind(actor.id)
        .bind(f64::from(PRESENCE_WINDOW_SECONDS))
        .fetch_all(self.pool())
        .await?)
    }

    /// Unread notification count, for nav badges.
    ///
    /// # Errors
    /// Propagates database failures.
    pub async fn unread_notification_count(&self, actor: &Actor) -> Result<i64, NewsroomError> {
        Ok(sqlx::query_scalar(
            "SELECT count(*) FROM meridian.notifications \
             WHERE user_id = $1 AND read_at IS NULL \
               AND (deliver_after IS NULL OR deliver_after <= now())",
        )
        .bind(actor.id)
        .fetch_one(self.pool())
        .await?)
    }
}

#[cfg(test)]
mod tests {
    use super::{digest_delay, priority_rank};
    use time::Duration;

    #[test]
    fn priority_rank_orders_low_to_critical() {
        assert!(priority_rank("low") < priority_rank("normal"));
        assert!(priority_rank("normal") < priority_rank("high"));
        assert!(priority_rank("high") < priority_rank("critical"));
    }

    #[test]
    fn digest_delay_maps_modes() {
        assert_eq!(digest_delay("immediate"), Some(Duration::ZERO));
        assert_eq!(digest_delay("hourly"), Some(Duration::hours(1)));
        assert_eq!(digest_delay("daily"), Some(Duration::days(1)));
        assert_eq!(digest_delay("off"), None);
    }
}
