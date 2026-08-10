//! Module 6 — tasks (kanban work items) and coverage events (planning/calendar).
//! Ported from the legacy `api.py` task and coverage-event handlers. `notify()`
//! side effects are written here alongside the audit trail.

use serde::Serialize;
use sqlx::FromRow;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::{Actor, NewsroomError, NewsroomService, audit, authz};

const WORK_STATUSES: [&str; 6] = [
    "todo",
    "in_progress",
    "blocked",
    "in_review",
    "done",
    "cancelled",
];

#[derive(Clone, Debug, FromRow, Serialize)]
pub struct Task {
    pub id: Uuid,
    pub story_id: Option<Uuid>,
    pub desk_id: Option<Uuid>,
    pub pitch_id: Option<Uuid>,
    pub event_id: Option<Uuid>,
    pub title: String,
    pub description: Option<String>,
    pub status: String,
    pub priority: String,
    pub assigned_to_id: Option<Uuid>,
    pub created_by_id: Uuid,
    #[serde(with = "time::serde::rfc3339::option")]
    pub due_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub completed_at: Option<OffsetDateTime>,
    pub blocker: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(skip_serializing)]
    pub updated_at: OffsetDateTime,
}

#[derive(Clone, Debug, FromRow, Serialize)]
pub struct CoverageEvent {
    pub id: Uuid,
    pub title: String,
    pub description: Option<String>,
    pub desk_id: Option<Uuid>,
    pub owner_id: Uuid,
    #[serde(with = "time::serde::rfc3339::option")]
    pub starts_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub ends_at: Option<OffsetDateTime>,
    pub status: String,
    pub location: Option<String>,
    pub metadata_json: serde_json::Value,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(skip_serializing)]
    pub updated_at: OffsetDateTime,
}

/// New-task input.
#[derive(Clone, Debug)]
pub struct TaskDraft {
    pub title: String,
    pub description: Option<String>,
    pub status: String,
    pub priority: String,
    pub story_id: Option<Uuid>,
    pub desk_id: Option<Uuid>,
    pub pitch_id: Option<Uuid>,
    pub event_id: Option<Uuid>,
    pub assigned_to_id: Option<Uuid>,
    pub due_at: Option<OffsetDateTime>,
}

/// A partial task update; a `None` field is left unchanged.
#[derive(Clone, Debug, Default)]
pub struct TaskPatch {
    pub title: Option<String>,
    pub description: Option<String>,
    pub status: Option<String>,
    pub priority: Option<String>,
    pub assigned_to_id: Option<Uuid>,
    pub due_at: Option<OffsetDateTime>,
    pub blocker: Option<String>,
}

/// Filters for the task list.
#[derive(Clone, Debug, Default)]
pub struct TaskFilter {
    pub assigned_to_id: Option<Uuid>,
    pub status: Option<String>,
    pub story_id: Option<Uuid>,
    /// Narrows the list to one sector, and scopes the `tasks.view_all` check.
    pub desk_id: Option<Uuid>,
}

/// New coverage-event input.
#[derive(Clone, Debug)]
pub struct CoverageDraft {
    pub title: String,
    pub description: Option<String>,
    pub desk_id: Option<Uuid>,
    pub starts_at: Option<OffsetDateTime>,
    pub ends_at: Option<OffsetDateTime>,
    pub status: String,
    pub location: Option<String>,
}

/// A partial coverage-event update.
#[derive(Clone, Debug, Default)]
pub struct CoveragePatch {
    pub title: Option<String>,
    pub description: Option<String>,
    pub desk_id: Option<Uuid>,
    pub starts_at: Option<OffsetDateTime>,
    pub ends_at: Option<OffsetDateTime>,
    pub status: Option<String>,
    pub location: Option<String>,
}

impl NewsroomService {
    /// Lists tasks ordered by due date. An explicit `assigned_to_id` filter wins;
    /// otherwise a non-editor sees only their own assignments.
    ///
    /// # Errors
    /// Propagates database failures.
    pub async fn list_tasks(
        &self,
        actor: &Actor,
        filter: &TaskFilter,
    ) -> Result<Vec<Task>, NewsroomError> {
        let sees_all = authz::has(self.pool(), actor, "tasks.view_all", filter.desk_id).await?;
        let assignee = filter.assigned_to_id.or_else(|| (!sees_all).then_some(actor.id));
        // Sector visibility on top of the assignee narrowing: even a holder of
        // `tasks.view_all` only sees the desks they belong to unless they hold
        // the global dashboard. Deskless tasks stay org-visible.
        let global = authz::has(self.pool(), actor, "dashboard.view_global", None).await?;
        Ok(sqlx::query_as::<_, Task>(
            "SELECT * FROM meridian.tasks \
             WHERE ($1::uuid IS NULL OR assigned_to_id = $1) \
               AND ($2::text IS NULL OR status = $2) \
               AND ($3::uuid IS NULL OR story_id = $3) \
               AND ($4::uuid IS NULL OR desk_id = $4) \
               AND ($5::boolean \
                    OR desk_id IS NULL \
                    OR desk_id IN (SELECT desk_id FROM meridian.desk_memberships \
                                    WHERE user_id = $6) \
                    OR assigned_to_id = $6 OR created_by_id = $6) \
             ORDER BY due_at NULLS LAST, created_at DESC",
        )
        .bind(assignee)
        .bind(&filter.status)
        .bind(filter.story_id)
        .bind(filter.desk_id)
        .bind(global)
        .bind(actor.id)
        .fetch_all(self.pool())
        .await?)
    }

    /// Loads a task or returns [`NewsroomError::NotFound`].
    ///
    /// # Errors
    /// Propagates database failures.
    pub async fn get_task(&self, task_id: Uuid) -> Result<Task, NewsroomError> {
        sqlx::query_as::<_, Task>("SELECT * FROM meridian.tasks WHERE id = $1")
            .bind(task_id)
            .fetch_optional(self.pool())
            .await?
            .ok_or(NewsroomError::NotFound("Task"))
    }

    /// Creates a task; requires the `tasks.assign` capability.
    ///
    /// # Errors
    /// [`NewsroomError::Forbidden`]; [`NewsroomError::Unprocessable`] for a bad
    /// status; database failures.
    pub async fn create_task(
        &self,
        actor: &Actor,
        draft: &TaskDraft,
    ) -> Result<Task, NewsroomError> {
        authz::require(self.pool(), actor, "tasks.assign", draft.desk_id).await?;
        if !WORK_STATUSES.contains(&draft.status.as_str()) {
            return Err(NewsroomError::Unprocessable(format!(
                "'{}' is not a work status",
                draft.status
            )));
        }
        let mut tx = self.pool().begin().await?;
        let task = sqlx::query_as::<_, Task>(
            "INSERT INTO meridian.tasks \
             (id, story_id, desk_id, pitch_id, event_id, title, description, status, priority, \
              assigned_to_id, created_by_id, due_at) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12) RETURNING *",
        )
        .bind(Uuid::now_v7())
        .bind(draft.story_id)
        .bind(draft.desk_id)
        .bind(draft.pitch_id)
        .bind(draft.event_id)
        .bind(&draft.title)
        .bind(&draft.description)
        .bind(&draft.status)
        .bind(&draft.priority)
        .bind(draft.assigned_to_id)
        .bind(actor.id)
        .bind(draft.due_at)
        .fetch_one(&mut *tx)
        .await?;
        if let Some(assignee) = task.assigned_to_id {
            crate::awareness::notify(
                &mut tx,
                &crate::awareness::NotifyInput {
                    user_id: assignee,
                    kind: "task.assigned",
                    title: "Task assigned",
                    body: Some(&task.title),
                    entity_type: "task",
                    entity_id: &task.id.to_string(),
                    priority: "normal",
                    group_key: None,
                },
            )
            .await?;
        }
        audit(
            &mut *tx,
            actor.id,
            "task.created",
            "task",
            &task.id.to_string(),
            None,
            Some(serde_json::json!({ "title": task.title })),
        )
        .await?;
        Self::enqueue_event(
            &mut *tx,
            "task.created",
            &task.id.to_string(),
            None,
            serde_json::json!({ "task_id": task.id, "title": task.title, "status": task.status }),
            Some(actor.id),
        )
        .await?;
        tx.commit().await?;
        Ok(task)
    }

    /// Updates a task. The assignee, the creator, or an editor may update it;
    /// moving to `done` stamps `completed_at` if unset.
    ///
    /// # Errors
    /// [`NewsroomError::Forbidden`]; [`NewsroomError::NotFound`];
    /// [`NewsroomError::Unprocessable`] for a bad status; database failures.
    pub async fn update_task(
        &self,
        actor: &Actor,
        task_id: Uuid,
        patch: &TaskPatch,
    ) -> Result<Task, NewsroomError> {
        let task = self.get_task(task_id).await?;
        if !owns_task(&task, actor)
            && !authz::has(self.pool(), actor, "tasks.edit_any", task.desk_id).await?
        {
            return Err(NewsroomError::Forbidden {
                capability: "tasks.update".to_owned(),
                reason: "Cannot update this task".to_owned(),
            });
        }
        if let Some(status) = &patch.status
            && !WORK_STATUSES.contains(&status.as_str())
        {
            return Err(NewsroomError::Unprocessable(format!(
                "'{status}' is not a work status"
            )));
        }
        let mut tx = self.pool().begin().await?;
        let updated = sqlx::query_as::<_, Task>(
            "UPDATE meridian.tasks SET \
               title = COALESCE($2, title), \
               description = COALESCE($3, description), \
               status = COALESCE($4, status), \
               priority = COALESCE($5, priority), \
               assigned_to_id = COALESCE($6, assigned_to_id), \
               due_at = COALESCE($7, due_at), \
               blocker = COALESCE($8, blocker), \
               completed_at = CASE \
                 WHEN COALESCE($4, status) = 'done' AND completed_at IS NULL THEN now() \
                 ELSE completed_at END, \
               updated_at = now() \
             WHERE id = $1 RETURNING *",
        )
        .bind(task_id)
        .bind(&patch.title)
        .bind(&patch.description)
        .bind(&patch.status)
        .bind(&patch.priority)
        .bind(patch.assigned_to_id)
        .bind(patch.due_at)
        .bind(&patch.blocker)
        .fetch_one(&mut *tx)
        .await?;
        audit(
            &mut *tx,
            actor.id,
            "task.updated",
            "task",
            &task_id.to_string(),
            None,
            None,
        )
        .await?;
        let task_event = if patch.status.as_deref() == Some("done") {
            "task.completed"
        } else {
            "task.updated"
        };
        Self::enqueue_event(
            &mut *tx,
            task_event,
            &task_id.to_string(),
            None,
            serde_json::json!({ "task_id": task_id, "title": updated.title, "status": updated.status }),
            Some(actor.id),
        )
        .await?;
        tx.commit().await?;
        Ok(updated)
    }

    /// Deletes a task. The assignee, the creator, or an editor may delete it.
    ///
    /// # Errors
    /// [`NewsroomError::Forbidden`]; [`NewsroomError::NotFound`]; database failures.
    pub async fn delete_task(&self, actor: &Actor, task_id: Uuid) -> Result<(), NewsroomError> {
        let task = self.get_task(task_id).await?;
        if !owns_task(&task, actor)
            && !authz::has(self.pool(), actor, "tasks.edit_any", task.desk_id).await?
        {
            return Err(NewsroomError::Forbidden {
                capability: "tasks.delete".to_owned(),
                reason: "Not authorized to delete this task".to_owned(),
            });
        }
        let mut tx = self.pool().begin().await?;
        sqlx::query("DELETE FROM meridian.tasks WHERE id = $1")
            .bind(task_id)
            .execute(&mut *tx)
            .await?;
        audit(
            &mut *tx,
            actor.id,
            "task.deleted",
            "task",
            &task_id.to_string(),
            Some(serde_json::json!({ "title": task.title })),
            None,
        )
        .await?;
        Self::enqueue_event(
            &mut *tx,
            "task.deleted",
            &task_id.to_string(),
            None,
            serde_json::json!({ "task_id": task_id, "title": task.title }),
            Some(actor.id),
        )
        .await?;
        tx.commit().await?;
        Ok(())
    }

    // --- Coverage events ----------------------------------------------------

    /// Lists coverage events ordered by start time.
    ///
    /// # Errors
    /// Propagates database failures.
    pub async fn list_coverage_events(&self) -> Result<Vec<CoverageEvent>, NewsroomError> {
        Ok(sqlx::query_as::<_, CoverageEvent>(
            "SELECT * FROM meridian.coverage_events ORDER BY starts_at NULLS LAST",
        )
        .fetch_all(self.pool())
        .await?)
    }

    /// Creates a coverage event owned by the actor.
    ///
    /// # Errors
    /// Propagates database failures.
    pub async fn create_coverage_event(
        &self,
        actor: &Actor,
        draft: &CoverageDraft,
    ) -> Result<CoverageEvent, NewsroomError> {
        let mut tx = self.pool().begin().await?;
        let event = sqlx::query_as::<_, CoverageEvent>(
            "INSERT INTO meridian.coverage_events \
             (id, title, description, desk_id, owner_id, starts_at, ends_at, status, location) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9) RETURNING *",
        )
        .bind(Uuid::now_v7())
        .bind(&draft.title)
        .bind(&draft.description)
        .bind(draft.desk_id)
        .bind(actor.id)
        .bind(draft.starts_at)
        .bind(draft.ends_at)
        .bind(&draft.status)
        .bind(&draft.location)
        .fetch_one(&mut *tx)
        .await?;
        audit(
            &mut *tx,
            actor.id,
            "coverage_event.created",
            "coverage_event",
            &event.id.to_string(),
            None,
            Some(serde_json::json!({ "title": event.title })),
        )
        .await?;
        Self::enqueue_event(
            &mut *tx,
            "coverage_event.created",
            &event.id.to_string(),
            None,
            serde_json::json!({ "coverage_event_id": event.id, "title": event.title, "status": event.status }),
            Some(actor.id),
        )
        .await?;
        tx.commit().await?;
        Ok(event)
    }

    /// Updates a coverage event. Gated like [`Self::update_task`]: its owner, or a
    /// holder of `tasks.edit_any` in its desk. Without this, any authenticated user
    /// could retitle, reschedule, or reassign any desk's coverage event.
    ///
    /// # Errors
    /// [`NewsroomError::Forbidden`] for a non-owner without `tasks.edit_any`;
    /// [`NewsroomError::NotFound`]; database failures.
    pub async fn update_coverage_event(
        &self,
        actor: &Actor,
        event_id: Uuid,
        patch: &CoveragePatch,
    ) -> Result<CoverageEvent, NewsroomError> {
        self.require_can_manage_coverage(actor, event_id).await?;
        let mut tx = self.pool().begin().await?;
        let event = sqlx::query_as::<_, CoverageEvent>(
            "UPDATE meridian.coverage_events SET \
               title = COALESCE($2, title), \
               description = COALESCE($3, description), \
               desk_id = COALESCE($4, desk_id), \
               starts_at = COALESCE($5, starts_at), \
               ends_at = COALESCE($6, ends_at), \
               status = COALESCE($7, status), \
               location = COALESCE($8, location), \
               updated_at = now() \
             WHERE id = $1 RETURNING *",
        )
        .bind(event_id)
        .bind(&patch.title)
        .bind(&patch.description)
        .bind(patch.desk_id)
        .bind(patch.starts_at)
        .bind(patch.ends_at)
        .bind(&patch.status)
        .bind(&patch.location)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(NewsroomError::NotFound("Event"))?;
        audit(
            &mut *tx,
            actor.id,
            "coverage_event.updated",
            "coverage_event",
            &event.id.to_string(),
            None,
            None,
        )
        .await?;
        Self::enqueue_event(
            &mut *tx,
            "coverage_event.updated",
            &event.id.to_string(),
            None,
            serde_json::json!({ "coverage_event_id": event.id, "title": event.title, "status": event.status }),
            Some(actor.id),
        )
        .await?;
        tx.commit().await?;
        Ok(event)
    }

    /// Deletes a coverage event. Gated like [`Self::update_coverage_event`]: its
    /// owner, or a holder of `tasks.edit_any` in its desk.
    ///
    /// # Errors
    /// [`NewsroomError::Forbidden`] for a non-owner without `tasks.edit_any`;
    /// [`NewsroomError::NotFound`]; database failures.
    pub async fn delete_coverage_event(
        &self,
        actor: &Actor,
        event_id: Uuid,
    ) -> Result<(), NewsroomError> {
        self.require_can_manage_coverage(actor, event_id).await?;
        let mut tx = self.pool().begin().await?;
        let deleted = sqlx::query("DELETE FROM meridian.coverage_events WHERE id = $1")
            .bind(event_id)
            .execute(&mut *tx)
            .await?;
        if deleted.rows_affected() == 0 {
            return Err(NewsroomError::NotFound("Event"));
        }
        audit(
            &mut *tx,
            actor.id,
            "coverage_event.deleted",
            "coverage_event",
            &event_id.to_string(),
            None,
            None,
        )
        .await?;
        Self::enqueue_event(
            &mut *tx,
            "coverage_event.deleted",
            &event_id.to_string(),
            None,
            serde_json::json!({ "coverage_event_id": event_id }),
            Some(actor.id),
        )
        .await?;
        tx.commit().await?;
        Ok(())
    }
}

/// Whether the actor owns the task outright — assignee or creator.
fn owns_task(task: &Task, actor: &Actor) -> bool {
    task.assigned_to_id == Some(actor.id) || task.created_by_id == actor.id
}

impl NewsroomService {
    /// The gate shared by coverage-event update and delete: the event's owner, or a
    /// holder of `tasks.edit_any` in its desk. Resolves the owner/desk from the
    /// stored row so a missing event is a clean not-found rather than a silent
    /// no-op.
    async fn require_can_manage_coverage(
        &self,
        actor: &Actor,
        event_id: Uuid,
    ) -> Result<(), NewsroomError> {
        let row: Option<(Uuid, Option<Uuid>)> =
            sqlx::query_as("SELECT owner_id, desk_id FROM meridian.coverage_events WHERE id = $1")
                .bind(event_id)
                .fetch_optional(self.pool())
                .await?;
        let (owner_id, desk_id) = row.ok_or(NewsroomError::NotFound("Event"))?;
        if owner_id != actor.id
            && !authz::has(self.pool(), actor, "tasks.edit_any", desk_id).await?
        {
            return Err(NewsroomError::Forbidden {
                capability: "tasks.edit_any".to_owned(),
                reason: "Cannot manage this coverage event".to_owned(),
            });
        }
        Ok(())
    }
}
