//! Module 7 (activity) — the activity centre (approvals, reminders, mentions),
//! the navigation badge counts those feed, and the escalation sweep. Ported from
//! the legacy `awareness/activity.py` and `notifications.process_escalations`.

use serde::Serialize;
use sqlx::FromRow;
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

#[derive(FromRow)]
struct EscalationCandidate {
    id: Uuid,
    user_id: Uuid,
    priority: String,
    title: String,
    entity_type: Option<String>,
    entity_id: Option<String>,
    created_at: OffsetDateTime,
}

use crate::awareness::{NotifyInput, notify};
use crate::{Actor, NewsroomError, NewsroomService};

/// One actionable item in the activity centre.
#[derive(Clone, Debug, Serialize)]
pub struct ActivityItem {
    pub kind: String,
    pub entity_type: String,
    pub entity_id: String,
    pub title: String,
    pub subtitle: String,
    pub href: String,
    /// When the item happened — escalated, last changed, assigned.
    ///
    /// Distinct from `due_at`, which is a *deadline*. Legacy carries both and the
    /// Rust port omitted `at`, which stayed invisible only because every feed
    /// that would have populated it returned an empty list. Populating those
    /// feeds exposed the gap, and exposed that this port had been putting event
    /// times into `due_at` for want of anywhere else to put them.
    #[serde(with = "time::serde::rfc3339::option")]
    pub at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub due_at: Option<OffsetDateTime>,
}

/// The activity centre: three actionable lists for one person.
#[derive(Debug, Serialize)]
pub struct ActivityCenter {
    pub approvals: Vec<ActivityItem>,
    pub reminders: Vec<ActivityItem>,
    pub mentions: Vec<ActivityItem>,
    /// Open work assigned to the caller: tasks not yet done, and stories they
    /// author or edit that have not been released.
    pub assignments: Vec<ActivityItem>,
    pub escalations: Vec<ActivityItem>,
    pub following: Vec<ActivityItem>,
    #[serde(with = "time::serde::rfc3339")]
    pub generated_at: OffsetDateTime,
}

/// The unread/actionable counts shown as navigation badges.
#[derive(Debug, Serialize)]
pub struct BadgeCounts {
    pub notifications: i64,
    pub approvals: i64,
    pub reminders: i64,
    pub mentions: i64,
}

impl NewsroomService {
    /// Reviews assigned to the caller and still pending.
    ///
    /// # Errors
    /// Propagates database failures.
    async fn approval_items(&self, actor: &Actor) -> Result<Vec<ActivityItem>, NewsroomError> {
        let rows: Vec<(Uuid, String, Option<String>)> = sqlx::query_as(
            "SELECT r.story_id, r.kind, s.title \
             FROM meridian.reviews r LEFT JOIN meridian.stories s ON s.id = r.story_id \
             WHERE r.assigned_to_id = $1 AND r.decision = 'pending'",
        )
        .bind(actor.id)
        .fetch_all(self.pool())
        .await?;
        Ok(rows
            .into_iter()
            .map(|(story_id, kind, title)| ActivityItem {
                kind: "review".to_owned(),
                entity_type: "story".to_owned(),
                entity_id: story_id.to_string(),
                title: title.unwrap_or_else(|| "Story".to_owned()),
                subtitle: format!("{} review", kind.replace('_', " ")),
                href: format!("/editor?story={story_id}&panel=reviews"),
                at: None,
                due_at: None,
            })
            .collect())
    }

    /// Overdue tasks and overdue unpublished stories the caller owns.
    ///
    /// # Errors
    /// Propagates database failures.
    async fn reminder_items(&self, actor: &Actor) -> Result<Vec<ActivityItem>, NewsroomError> {
        let mut items = Vec::new();
        let tasks: Vec<(Uuid, String, Option<OffsetDateTime>)> = sqlx::query_as(
            "SELECT id, title, due_at FROM meridian.tasks \
             WHERE assigned_to_id = $1 AND status <> 'done' AND due_at < now()",
        )
        .bind(actor.id)
        .fetch_all(self.pool())
        .await?;
        for (id, title, due_at) in tasks {
            items.push(ActivityItem {
                kind: "overdue_task".to_owned(),
                entity_type: "task".to_owned(),
                entity_id: id.to_string(),
                title,
                subtitle: "Overdue".to_owned(),
                href: format!("/tasks/{id}"),
                at: None,
                due_at,
            });
        }
        let stories: Vec<(Uuid, String, Option<OffsetDateTime>)> = sqlx::query_as(
            "SELECT id, title, due_at FROM meridian.stories \
             WHERE (author_id = $1 OR editor_id = $1) AND due_at < now() \
               AND publication_state = 'not_live'",
        )
        .bind(actor.id)
        .fetch_all(self.pool())
        .await?;
        for (id, title, due_at) in stories {
            items.push(ActivityItem {
                kind: "overdue_story".to_owned(),
                entity_type: "story".to_owned(),
                entity_id: id.to_string(),
                title,
                subtitle: "Past due".to_owned(),
                href: format!("/editor?story={id}"),
                at: None,
                due_at,
            });
        }
        Ok(items)
    }

    /// The caller's unread mention notifications.
    ///
    /// # Errors
    /// Propagates database failures.
    async fn mention_items(&self, actor: &Actor) -> Result<Vec<ActivityItem>, NewsroomError> {
        let rows: Vec<(Option<String>, Option<String>, String)> = sqlx::query_as(
            "SELECT entity_type, entity_id, title FROM meridian.notifications \
             WHERE user_id = $1 AND kind = 'mention' AND read_at IS NULL \
             ORDER BY created_at DESC LIMIT 20",
        )
        .bind(actor.id)
        .fetch_all(self.pool())
        .await?;
        Ok(rows
            .into_iter()
            .map(|(entity_type, entity_id, title)| {
                let entity_id = entity_id.unwrap_or_default();
                ActivityItem {
                    kind: "mention".to_owned(),
                    entity_type: entity_type.unwrap_or_else(|| "story".to_owned()),
                    entity_id: entity_id.clone(),
                    title,
                    subtitle: "Mention".to_owned(),
                    href: format!("/editor?story={entity_id}"),
                    at: None,
                    due_at: None,
                }
            })
            .collect())
    }

    /// Open work assigned to the caller — tasks and stories alike.
    ///
    /// Distinct from `reminders`, which is the *overdue* subset: this is
    /// everything on their plate, so an assignment appears here before it slips.
    ///
    /// # Errors
    /// Propagates database failures.
    async fn assignment_items(&self, actor: &Actor) -> Result<Vec<ActivityItem>, NewsroomError> {
        let rows: Vec<(String, Uuid, String, String, Option<OffsetDateTime>)> = sqlx::query_as(
            "SELECT 'task', t.id, t.title, t.status, t.due_at \
             FROM meridian.tasks t \
             WHERE t.assigned_to_id = $1 AND t.status NOT IN ('done', 'cancelled') \
             UNION ALL \
             SELECT 'story', s.id, s.title, s.workflow_state, s.due_at \
             FROM meridian.stories s \
             WHERE (s.author_id = $1 OR s.editor_id = $1) \
               AND s.workflow_state NOT IN ('archived', 'parked') \
               AND s.publication_state = 'not_live' \
             ORDER BY 5 ASC NULLS LAST",
        )
        .bind(actor.id)
        .fetch_all(self.pool())
        .await?;
        Ok(rows
            .into_iter()
            .map(|(entity_type, id, title, state, due_at)| ActivityItem {
                kind: "assignment".to_owned(),
                entity_type: entity_type.clone(),
                entity_id: id.to_string(),
                title,
                subtitle: state.replace('_', " "),
                href: if entity_type == "task" {
                    format!("/tasks/{id}")
                } else {
                    format!("/story/{id}")
                },
                at: None,
                due_at,
            })
            .collect())
    }

    /// Attention items escalated *to* the caller.
    ///
    /// `escalate_attention` writes `escalated_at` and reassigns the underlying
    /// row, so the caller's own escalated items are the join of those two facts.
    ///
    /// # Errors
    /// Propagates database failures.
    async fn escalation_items(&self, actor: &Actor) -> Result<Vec<ActivityItem>, NewsroomError> {
        let rows: Vec<(String, String, Option<String>, Option<OffsetDateTime>)> = sqlx::query_as(
            "SELECT a.fingerprint, COALESCE(a.attention_type, 'escalation'), \
                    COALESCE(s.title, t.title), a.escalated_at \
             FROM meridian.attention_states a \
             LEFT JOIN meridian.reviews r \
               ON a.fingerprint = 'review:' || r.id AND r.assigned_to_id = $1 \
             LEFT JOIN meridian.stories s ON s.id = r.story_id \
             LEFT JOIN meridian.tasks t \
               ON a.fingerprint = 'task:' || t.id AND t.assigned_to_id = $1 \
             WHERE a.escalated_at IS NOT NULL \
               AND (r.id IS NOT NULL OR t.id IS NOT NULL) \
             ORDER BY a.escalated_at DESC",
        )
        .bind(actor.id)
        .fetch_all(self.pool())
        .await?;
        Ok(rows
            .into_iter()
            .map(|(fingerprint, attention_type, title, escalated_at)| {
                let entity_type =
                    fingerprint.split_once(':').map_or("attention", |(kind, _)| kind).to_owned();
                let entity_id =
                    fingerprint.split_once(':').map_or(String::new(), |(_, id)| id.to_owned());
                ActivityItem {
                    kind: "escalation".to_owned(),
                    entity_type,
                    entity_id,
                    title: title.unwrap_or_else(|| "Escalated item".to_owned()),
                    subtitle: attention_type.replace('_', " "),
                    href: "/".to_owned(),
                    at: escalated_at,
                    due_at: None,
                }
            })
            .collect())
    }

    /// Stories the caller follows that have moved on since.
    ///
    /// Follows are the point of the feature — "tell me when this changes" — so the
    /// list is ordered by the story's own `updated_at`, most recently changed
    /// first.
    ///
    /// # Errors
    /// Propagates database failures.
    async fn following_items(&self, actor: &Actor) -> Result<Vec<ActivityItem>, NewsroomError> {
        let rows: Vec<(Uuid, String, String, Option<OffsetDateTime>)> = sqlx::query_as(
            "SELECT s.id, s.title, s.workflow_state, s.updated_at \
             FROM meridian.follows f \
             JOIN meridian.stories s ON s.id::text = f.entity_id \
             WHERE f.user_id = $1 AND f.entity_type = 'story' \
             ORDER BY s.updated_at DESC",
        )
        .bind(actor.id)
        .fetch_all(self.pool())
        .await?;
        Ok(rows
            .into_iter()
            .map(|(id, title, state, updated_at)| ActivityItem {
                kind: "following".to_owned(),
                entity_type: "story".to_owned(),
                entity_id: id.to_string(),
                title,
                subtitle: state.replace('_', " "),
                href: format!("/story/{id}"),
                at: updated_at,
                due_at: None,
            })
            .collect())
    }

    /// The full activity centre for the caller.
    ///
    /// # Errors
    /// Propagates database failures.
    pub async fn activity_center(&self, actor: &Actor) -> Result<ActivityCenter, NewsroomError> {
        Ok(ActivityCenter {
            approvals: self.approval_items(actor).await?,
            reminders: self.reminder_items(actor).await?,
            mentions: self.mention_items(actor).await?,
            assignments: self.assignment_items(actor).await?,
            escalations: self.escalation_items(actor).await?,
            following: self.following_items(actor).await?,
            generated_at: OffsetDateTime::now_utc(),
        })
    }

    /// Navigation badge counts: unread notifications, pending approvals, overdue
    /// reminders, and unread mentions.
    ///
    /// # Errors
    /// Propagates database failures.
    pub async fn badge_counts(&self, actor: &Actor) -> Result<BadgeCounts, NewsroomError> {
        let notifications = self.unread_notification_count(actor).await?;
        let approvals: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM meridian.reviews WHERE assigned_to_id = $1 AND decision = 'pending'",
        )
        .bind(actor.id)
        .fetch_one(self.pool())
        .await?;
        let reminders: i64 = sqlx::query_scalar(
            "SELECT (SELECT count(*) FROM meridian.tasks \
                       WHERE assigned_to_id = $1 AND status <> 'done' AND due_at < now()) \
                  + (SELECT count(*) FROM meridian.stories \
                       WHERE (author_id = $1 OR editor_id = $1) AND due_at < now() \
                         AND publication_state = 'not_live')",
        )
        .bind(actor.id)
        .fetch_one(self.pool())
        .await?;
        let mentions: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM meridian.notifications \
             WHERE user_id = $1 AND kind = 'mention' AND read_at IS NULL",
        )
        .bind(actor.id)
        .fetch_one(self.pool())
        .await?;
        Ok(BadgeCounts {
            notifications,
            approvals,
            reminders,
            mentions,
        })
    }

    /// Escalates unread high/critical notifications that have gone stale to the
    /// owner's desk lead, once each. Pull-based, mirroring the legacy manual
    /// trigger. Returns how many were escalated.
    ///
    /// This is a newsroom-wide maintenance sweep (it notifies every stalled owner's
    /// desk lead), so it is administrator-only — a scheduler drives it with admin
    /// credentials. Without the gate any authenticated user could fire it.
    ///
    /// # Errors
    /// [`NewsroomError::Forbidden`] for a non-administrator; database failures.
    pub async fn process_escalations(&self, actor: &Actor) -> Result<u64, NewsroomError> {
        if !actor.is_admin {
            return Err(NewsroomError::Forbidden {
                capability: "admin".to_owned(),
                reason: "Escalation sweeps are administrator-only".to_owned(),
            });
        }
        // Candidates: unread, not-yet-escalated, high or critical.
        let candidates = sqlx::query_as::<_, EscalationCandidate>(
            "SELECT id, user_id, priority, title, entity_type, entity_id, created_at \
             FROM meridian.notifications \
             WHERE read_at IS NULL AND escalated_at IS NULL AND priority IN ('high', 'critical')",
        )
        .fetch_all(self.pool())
        .await?;
        let now = OffsetDateTime::now_utc();
        let mut escalated = 0_u64;
        for candidate in candidates {
            let EscalationCandidate {
                id,
                user_id: owner_id,
                priority,
                title,
                entity_type,
                entity_id,
                created_at,
            } = candidate;
            let threshold = match priority.as_str() {
                "critical" => Duration::hours(1),
                "high" => Duration::hours(8),
                _ => continue,
            };
            if created_at + threshold > now {
                continue;
            }
            // Escalation goes up the desk, not the role hierarchy: the desk lead
            // owns the stalled work.
            let lead: Option<Uuid> = sqlx::query_scalar(
                "SELECT d.lead_user_id FROM meridian.desks d \
                 JOIN meridian.desk_memberships m ON m.desk_id = d.id \
                 WHERE m.user_id = $1 AND d.lead_user_id IS NOT NULL AND d.lead_user_id <> $1 \
                 LIMIT 1",
            )
            .bind(owner_id)
            .fetch_optional(self.pool())
            .await?
            .flatten();
            let Some(lead_id) = lead else { continue };

            let mut tx = self.pool().begin().await?;
            sqlx::query(
                "UPDATE meridian.notifications SET escalated_at = now(), escalated_to_id = $2 WHERE id = $1",
            )
            .bind(id)
            .bind(lead_id)
            .execute(&mut *tx)
            .await?;
            // Through notify() so the recipient's preferences and grouping still
            // apply; a critical escalation reaches them regardless.
            notify(
                &mut tx,
                &NotifyInput {
                    user_id: lead_id,
                    kind: "escalation",
                    title: &format!("Unactioned: {title}"),
                    body: None,
                    entity_type: entity_type.as_deref().unwrap_or("notification"),
                    entity_id: entity_id.as_deref().unwrap_or(""),
                    priority: &priority,
                    group_key: None,
                },
            )
            .await?;
            tx.commit().await?;
            escalated += 1;
        }
        Ok(escalated)
    }
}
