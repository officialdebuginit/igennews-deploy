//! Module 8 — dashboard aggregations. Ported from the legacy `dashboard`
//! package: the editorial `pipeline` (per-state counts, oldest waiting story,
//! historical median dwell) and per-person `workload`. Computed over existing
//! tables, so no new schema.
//!
//! Deferred within Module 8 (heavier, standalone pieces): the computed attention
//! queue + acknowledgement, bottleneck severity, metric snapshots, release
//! timeline (needs Module 9), drilldowns, and saved dashboard views.

use serde::Serialize;
use uuid::Uuid;

use crate::{Actor, NewsroomError, NewsroomService, authz};

/// Ordered pipeline states shown on the board (intake → ready).
const PIPELINE_STATES: [&str; 9] = [
    "intake",
    "proposed",
    "assigned",
    "reporting",
    "drafting",
    "desk_review",
    "verification",
    "copy_standards",
    "ready",
];

/// Story states that count toward a person's active load.
const ACTIVE_WORKFLOW_STATES: [&str; 7] = [
    "assigned",
    "reporting",
    "drafting",
    "desk_review",
    "verification",
    "copy_standards",
    "ready",
];

/// Historical-throughput lookback for the median dwell, in days.
const MEDIAN_LOOKBACK_DAYS: i32 = 30;

#[derive(Debug, Serialize)]
pub struct PipelineStage {
    pub state: String,
    pub count: i64,
    pub oldest_story_id: Option<Uuid>,
    pub oldest_story_title: Option<String>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub oldest_since: Option<time::OffsetDateTime>,
    pub median_hours_in_state: Option<f64>,
    // SLA / bottleneck telemetry. All computed except `available_reviewers`, which
    // needs on-call semantics `desk_schedules` does not yet express (see the field
    // comment below) and stays null rather than being invented.
    pub current_wait_median_hours: Option<f64>,
    pub sla_target_hours: Option<f64>,
    pub sla_status: Option<String>,
    pub overdue_count: i64,
    pub overdue_share: f64,
    pub backward_transitions: i64,
    /// Reviewers free to pick work up in this state. `desk_schedules` stores a
    /// weekly-hours blob and an on-call flag, but nothing that says who is
    /// *unoccupied right now*; deriving it from that would be a guess dressed as a
    /// measurement, so it stays null until the schedule model can answer it.
    pub available_reviewers: Option<i64>,
    pub baseline_queue_size: Option<i64>,
    pub baseline_samples: i64,
    pub is_bottleneck: bool,
    pub bottleneck_reasons: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct Pipeline {
    pub total: i64,
    pub sla_scoped: bool,
    pub stages: Vec<PipelineStage>,
    #[serde(with = "time::serde::rfc3339")]
    pub generated_at: time::OffsetDateTime,
}

/// One row behind a metric, for a drill-down list.
#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct DrilldownRow {
    pub entity_type: String,
    pub entity_id: String,
    pub title: String,
    pub subtitle: String,
    pub href: String,
}

/// One observed point in a metric series.
#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct MetricPoint {
    #[serde(with = "time::serde::rfc3339")]
    pub bucket_start: time::OffsetDateTime,
    pub value: i64,
}


/// Rounds to two decimals, so an hours figure reads as a measurement rather than
/// a float artefact.
fn round2(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}

/// Grades a live wait against a desk's SLA target, warning at the desk's own
/// configured percentage rather than a fixed threshold.
///
/// `None` when the target is not a usable number: a state with no meaningful SLA
/// cannot breach, and silence is better than an invented threshold.
fn sla_status(wait_hours: f64, target_hours: f64, warn_at_percent: i32) -> Option<&'static str> {
    if !target_hours.is_finite() || target_hours <= 0.0 || !wait_hours.is_finite() {
        return None;
    }
    let ratio = wait_hours / target_hours;
    let warn_at = f64::from(warn_at_percent) / 100.0;
    Some(if ratio >= 1.0 {
        "breach"
    } else if ratio >= warn_at {
        "warn"
    } else {
        "ok"
    })
}

/// The overdue fraction of a queue, 0.0 when the queue is empty. Precision loss
/// on the cast is irrelevant at newsroom queue sizes.
#[allow(clippy::cast_precision_loss)]
fn share(part: i64, whole: i64) -> f64 {
    if whole <= 0 {
        return 0.0;
    }
    round2(part as f64 / whole as f64)
}

/// Rounds an averaged queue depth back to a whole number of stories. Saturating,
/// because a baseline is a count and a count cannot overflow into nonsense.
#[allow(clippy::cast_possible_truncation)]
fn round_to_i64(value: f64) -> i64 {
    /// 2^63 as an exact `f64`; anything at or above it is unrepresentable as `i64`.
    const I64_MAX_AS_F64: f64 = 9_223_372_036_854_775_808.0;
    let rounded = value.round();
    if !rounded.is_finite() || rounded < 0.0 {
        return 0;
    }
    if rounded >= I64_MAX_AS_F64 { i64::MAX } else { rounded as i64 }
}

impl NewsroomService {
    /// The editorial pipeline: for each stage, how many stories sit there, the
    /// one waiting longest, and the historical median dwell for that state.
    ///
    /// # Errors
    /// Propagates database failures.
    // The body gathers six independent aggregates and then grades each stage
    // against them; splitting the gathering would scatter the scope filters that
    // must stay identical across all six.
    #[allow(clippy::too_many_lines)]
    pub async fn dashboard_pipeline(
        &self,
        actor: &Actor,
        desk_id: Option<Uuid>,
        owner_id: Option<Uuid>,
    ) -> Result<Pipeline, NewsroomError> {
        self.require_desk_read(actor, desk_id).await?;
        let counts: Vec<(String, i64)> = sqlx::query_as(
            "SELECT workflow_state, count(*) FROM meridian.stories \
             WHERE ($1::uuid IS NULL OR desk_id = $1) AND ($2::uuid IS NULL OR author_id = $2) \
             GROUP BY workflow_state",
        )
        .bind(desk_id)
        .bind(owner_id)
        .fetch_all(self.pool())
        .await?;

        // One row per state: the story with the earliest updated_at.
        let oldest: Vec<(String, Uuid, String, time::OffsetDateTime)> = sqlx::query_as(
            "SELECT DISTINCT ON (workflow_state) workflow_state, id, title, updated_at \
             FROM meridian.stories \
             WHERE ($1::uuid IS NULL OR desk_id = $1) AND ($2::uuid IS NULL OR author_id = $2) \
             ORDER BY workflow_state, updated_at ASC",
        )
        .bind(desk_id)
        .bind(owner_id)
        .fetch_all(self.pool())
        .await?;

        // Median hours in each state over closed dwell rows in the lookback
        // window (global historical throughput, not desk-scoped, as in legacy).
        let medians: Vec<(String, Option<f64>)> = sqlx::query_as(
            "SELECT to_state, \
               percentile_cont(0.5) WITHIN GROUP ( \
                 ORDER BY EXTRACT(EPOCH FROM (exited_at - entered_at)) / 3600.0) \
             FROM meridian.workflow_state_history \
             WHERE exited_at IS NOT NULL AND entered_at >= now() - make_interval(days => $1::int) \
             GROUP BY to_state",
        )
        .bind(MEDIAN_LOOKBACK_DAYS)
        .fetch_all(self.pool())
        .await?;

        // Live wait: how long the stories *currently* sitting in each state have
        // been there. Distinct from `median_hours_in_state`, which is historical
        // throughput over closed dwell rows — a stage can have a healthy history
        // and a stalled queue right now, and the bottleneck signal needs both.
        let live_waits: Vec<(String, Option<f64>, i64)> = sqlx::query_as(
            "SELECT h.to_state, \
               percentile_cont(0.5) WITHIN GROUP ( \
                 ORDER BY EXTRACT(EPOCH FROM (now() - h.entered_at)) / 3600.0), \
               count(*) \
             FROM meridian.workflow_state_history h \
             JOIN meridian.stories s ON s.id = h.story_id \
             WHERE h.exited_at IS NULL \
               AND ($1::uuid IS NULL OR s.desk_id = $1) \
               AND ($2::uuid IS NULL OR s.author_id = $2) \
             GROUP BY h.to_state",
        )
        .bind(desk_id)
        .bind(owner_id)
        .fetch_all(self.pool())
        .await?;

        // Per-state SLA targets, only when scoped to one desk: an SLA belongs to a
        // desk, so an unscoped pipeline has no single target to report against.
        let slas: Vec<(String, f64, i32)> = if let Some(desk) = desk_id {
            sqlx::query_as(
                "SELECT workflow_state, target_hours, warn_at_percent \
                 FROM meridian.desk_slas WHERE desk_id = $1 AND is_active",
            )
            .bind(desk)
            .fetch_all(self.pool())
            .await?
        } else {
            Vec::new()
        };

        // Stories whose current wait already exceeds the desk's target.
        let overdue: Vec<(String, i64)> = if let Some(desk) = desk_id {
            sqlx::query_as(
                "SELECT h.to_state, count(*) \
                 FROM meridian.workflow_state_history h \
                 JOIN meridian.stories s ON s.id = h.story_id \
                 JOIN meridian.desk_slas sla \
                   ON sla.desk_id = s.desk_id AND sla.workflow_state = h.to_state AND sla.is_active \
                 WHERE h.exited_at IS NULL AND s.desk_id = $1 \
                   AND EXTRACT(EPOCH FROM (now() - h.entered_at)) / 3600.0 > sla.target_hours \
                 GROUP BY h.to_state",
            )
            .bind(desk)
            .fetch_all(self.pool())
            .await?
        } else {
            Vec::new()
        };

        // Kickbacks: a transition that moved a story *earlier* in the pipeline.
        // Rework is the clearest bottleneck signal a stage can emit, because it
        // means work is arriving here that is not fit to leave.
        let backwards: Vec<(String, i64)> = sqlx::query_as(
            "SELECT h.from_state, count(*) \
             FROM meridian.workflow_state_history h \
             JOIN meridian.stories s ON s.id = h.story_id \
             WHERE h.from_state IS NOT NULL \
               AND array_position($3::text[], h.from_state) > array_position($3::text[], h.to_state) \
               AND h.entered_at >= now() - make_interval(days => $4::int) \
               AND ($1::uuid IS NULL OR s.desk_id = $1) \
               AND ($2::uuid IS NULL OR s.author_id = $2) \
             GROUP BY h.from_state",
        )
        .bind(desk_id)
        .bind(owner_id)
        .bind(PIPELINE_STATES.as_slice())
        .bind(MEDIAN_LOOKBACK_DAYS)
        .fetch_all(self.pool())
        .await?;

        // Historical queue depth per state, from the captured metric series, so
        // "unusually deep" is measured against this newsroom rather than a guess.
        let baselines: Vec<(String, Option<f64>, i64)> = sqlx::query_as(
            "SELECT replace(metric_key, 'pipeline.queue.', ''), avg(value), count(*) \
             FROM meridian.metric_snapshots \
             WHERE metric_key LIKE 'pipeline.queue.%' \
               AND bucket_start >= now() - make_interval(days => $1::int) \
             GROUP BY 1",
        )
        .bind(MEDIAN_LOOKBACK_DAYS)
        .fetch_all(self.pool())
        .await?;

        let stages: Vec<PipelineStage> = PIPELINE_STATES
            .iter()
            .map(|state| {
                let count = counts
                    .iter()
                    .find(|(s, _)| s == state)
                    .map_or(0, |(_, c)| *c);
                let oldest_row = oldest.iter().find(|(s, ..)| s == state);
                let median = medians
                    .iter()
                    .find(|(s, _)| s == state)
                    .and_then(|(_, m)| *m)
                    .map(round2);

                let live_wait = live_waits
                    .iter()
                    .find(|(s, ..)| s == state)
                    .and_then(|(_, wait, _)| *wait)
                    .map(round2);
                let sla = slas
                    .iter()
                    .find(|(s, ..)| s == state)
                    .map(|(_, target, warn)| (*target, *warn));
                let overdue_count = overdue
                    .iter()
                    .find(|(s, _)| s == state)
                    .map_or(0, |(_, n)| *n);
                let backward_transitions = backwards
                    .iter()
                    .find(|(s, _)| s == state)
                    .map_or(0, |(_, n)| *n);
                let baseline_row = baselines.iter().find(|(s, ..)| s == state);
                let baseline = baseline_row.and_then(|(_, avg, _)| *avg).map(round_to_i64);
                let baseline_samples = baseline_row.map_or(0, |(.., n)| *n);

                // SLA status compares the *live* wait against the desk's target,
                // warning at the desk's own configured percentage rather than a
                // fixed threshold. A state with no SLA row cannot breach — silence
                // over an invented threshold, matching `desk_analytics`.
                let sla_status = sla
                    .zip(live_wait)
                    .and_then(|((target, warn), wait)| sla_status(wait, target, warn))
                    .map(str::to_owned);

                let overdue_share = share(overdue_count, count);

                // A stage is a bottleneck for stated reasons, never a bare boolean:
                // the dashboard has to be able to say *why* it flagged something.
                let mut bottleneck_reasons = Vec::new();
                if sla_status.as_deref() == Some("breach") {
                    bottleneck_reasons.push("Live wait exceeds the desk SLA".to_owned());
                }
                if overdue_share >= 0.5 && overdue_count > 0 {
                    bottleneck_reasons.push("Half or more of this queue is overdue".to_owned());
                }
                if baseline.is_some_and(|base| {
                    baseline_samples >= 3 && base > 0 && count > base * 2
                }) {
                    bottleneck_reasons
                        .push("Queue is more than twice its recent average".to_owned());
                }
                if backward_transitions >= 3 {
                    bottleneck_reasons.push(format!(
                        "{backward_transitions} stories sent back from this stage recently"
                    ));
                }
                PipelineStage {
                    state: (*state).to_owned(),
                    count,
                    oldest_story_id: oldest_row.map(|(_, id, ..)| *id),
                    oldest_story_title: oldest_row.map(|(.., title, _)| title.clone()),
                    oldest_since: oldest_row.map(|(.., since)| *since),
                    median_hours_in_state: median,
                    current_wait_median_hours: live_wait,
                    sla_target_hours: sla.map(|(target, _)| target),
                    sla_status,
                    overdue_count,
                    overdue_share,
                    backward_transitions,
                    // Deliberately null — see the field comment.
                    available_reviewers: None,
                    baseline_queue_size: baseline,
                    baseline_samples,
                    is_bottleneck: !bottleneck_reasons.is_empty(),
                    bottleneck_reasons,
                }
            })
            .collect();

        Ok(Pipeline {
            total: stages.iter().map(|stage| stage.count).sum(),
            sla_scoped: desk_id.is_some(),
            stages,
            generated_at: time::OffsetDateTime::now_utc(),
        })
    }

    /// Headline dashboard counts for the caller's scope.
    ///
    /// # Errors
    /// Propagates database failures.
    pub async fn dashboard_summary(
        &self,
        actor: &Actor,
        desk_id: Option<Uuid>,
        owner_id: Option<Uuid>,
    ) -> Result<serde_json::Value, NewsroomError> {
        self.require_desk_read(actor, desk_id).await?;
        let row = sqlx::query_as::<_, SummaryRow>(SUMMARY_SQL)
        .bind(desk_id)
        .bind(owner_id)
        .fetch_one(self.pool())
        .await?;
        // Legacy shape: one card per metric, each `{value, overdue, at_risk,
        // critical, change, change_since, trend}` inside `metrics`, plus a `scope`
        // window.
        //
        // `change`/`trend` are measured against the day's first `metric_snapshots`
        // reading (see `capture_metrics`, which records all five keys hourly,
        // first-write-wins). With no snapshot for the day there is no baseline, so
        // the change is reported as 0 / "flat" — the honest reading of "nothing to
        // compare against", not a guess from a partial series.
        //
        // The breakdowns are computed only where the schema supports them:
        //   overdue  — tasks are the metric itself; stories past due and not
        //              ready/archived; scheduled releases past their slot. Reviews
        //              and corrections carry no due date of their own, so they stay
        //              null rather than borrowing their story's.
        //   at_risk  — tasks due inside 24 hours. Nothing else has a comparable
        //              lead-time signal.
        //   critical — urgent-priority tasks and stories; retraction and
        //              legal/privacy removals among corrections. Reviews and
        //              releases have no severity column, so they stay 0.
        let now = time::OffsetDateTime::now_utc();
        let day_start = now.replace_time(time::Time::MIDNIGHT);
        let day_end = day_start + time::Duration::days(1);

        let baselines: Vec<MetricBaseline> = sqlx::query_as(
            "SELECT DISTINCT ON (metric_key) metric_key, value \
             FROM meridian.metric_snapshots \
             WHERE scope_type = 'global' AND scope_id = '' AND bucket_start >= $1 \
             ORDER BY metric_key, bucket_start ASC",
        )
        .bind(day_start)
        .fetch_all(self.pool())
        .await?;

        let baseline_for = |key: &str| -> Option<i64> {
            baselines.iter().find(|row| row.metric_key == key).map(|row| row.value)
        };

        let metric = |key: &str,
                      value: i64,
                      overdue: Option<i64>,
                      at_risk: Option<i64>,
                      critical: i64| {
            let change = baseline_for(key).map_or(0, |baseline| value - baseline);
            let trend = match change {
                0 => "flat",
                change if change > 0 => "up",
                _ => "down",
            };
            serde_json::json!({
                "value": value,
                "overdue": overdue,
                "at_risk": at_risk,
                "critical": critical,
                "change": change,
                "change_since": crate::rfc3339(day_start),
                "trend": trend,
            })
        };
        Ok(serde_json::json!({
            "metrics": {
                "open_corrections": metric(
                    "open_corrections", row.open_corrections, None, None, row.corrections_severe,
                ),
                "overdue_tasks": metric(
                    "overdue_tasks",
                    row.overdue_tasks,
                    Some(row.overdue_tasks),
                    Some(row.tasks_at_risk),
                    row.tasks_urgent,
                ),
                "pending_reviews": metric("pending_reviews", row.pending_reviews, None, None, 0),
                "scheduled_releases": metric(
                    "scheduled_releases",
                    row.scheduled_releases,
                    Some(row.releases_overdue),
                    None,
                    0,
                ),
                "stories_due_today": metric(
                    "stories_due_today",
                    row.stories_due_today,
                    Some(row.stories_overdue),
                    None,
                    row.stories_urgent,
                ),
            },
            "scope": {
                "desk_id": desk_id,
                "owner_id": owner_id,
                "from": crate::rfc3339(day_start),
                "to": crate::rfc3339(day_end),
            },
            "generated_at": crate::rfc3339(now),
        }))
    }

    /// The legacy `GET /dashboard` home composite: headline counts, the story
    /// count per workflow state, and the recent audit trail.
    ///
    /// # Errors
    /// Propagates database failures.
    pub async fn dashboard_home(
        &self,
        actor: &Actor,
        desk_id: Option<Uuid>,
        owner_id: Option<Uuid>,
    ) -> Result<serde_json::Value, NewsroomError> {
        self.require_desk_read(actor, desk_id).await?;
        let row = sqlx::query_as::<_, SummaryRow>(SUMMARY_SQL)
        .bind(desk_id)
        .bind(owner_id)
        .fetch_one(self.pool())
        .await?;
        let workflow: Vec<(String, i64)> = sqlx::query_as(
            "SELECT workflow_state, count(*) FROM meridian.stories \
             WHERE ($1::uuid IS NULL OR desk_id = $1) AND ($2::uuid IS NULL OR author_id = $2) \
             GROUP BY workflow_state",
        )
        .bind(desk_id)
        .bind(owner_id)
        .fetch_all(self.pool())
        .await?;
        let count_of = |state: &str| {
            workflow
                .iter()
                .find(|(w, _)| w == state)
                .map_or(0, |(_, c)| *c)
        };
        let recent = sqlx::query_as::<_, crate::admin::AuditRecord>(
            "SELECT id, actor_id, action, entity_type, entity_id, before, after, context, created_at \
             FROM meridian.audit_events ORDER BY created_at DESC LIMIT 10",
        )
        .fetch_all(self.pool())
        .await?;
        Ok(serde_json::json!({
            "open_corrections": row.open_corrections,
            "overdue_tasks": row.overdue_tasks,
            "pending_reviews": row.pending_reviews,
            "scheduled_releases": row.scheduled_releases,
            "stories_due_today": row.stories_due_today,
            "stories_by_workflow": {
                "intake": count_of("intake"),
                "proposed": count_of("proposed"),
                "assigned": count_of("assigned"),
                "reporting": count_of("reporting"),
                "drafting": count_of("drafting"),
                "desk_review": count_of("desk_review"),
                "copy_standards": count_of("copy_standards"),
                "verification": count_of("verification"),
                "ready": count_of("ready"),
                "parked": count_of("parked"),
                "archived": count_of("archived"),
            },
            "recent_activity": recent,
        }))
    }

    /// Recent audit activity, newest first, optionally filtered.
    ///
    /// # Errors
    /// Propagates database failures.
    pub async fn dashboard_activity(
        &self,
        entity_type: Option<&str>,
        actor_id: Option<Uuid>,
        limit: i64,
    ) -> Result<Vec<ActivityRow>, NewsroomError> {
        Ok(sqlx::query_as::<_, ActivityRow>(
            "SELECT a.id, a.actor_id, u.display_name AS actor_name, a.action, a.entity_type, \
                    a.entity_id, a.created_at, a.context \
             FROM meridian.audit_events a LEFT JOIN meridian.users u ON u.id = a.actor_id \
             WHERE ($1::text IS NULL OR a.entity_type = $1) AND ($2::uuid IS NULL OR a.actor_id = $2) \
             ORDER BY a.created_at DESC LIMIT $3",
        )
        .bind(entity_type)
        .bind(actor_id)
        .bind(limit.clamp(1, 200))
        .fetch_all(self.pool())
        .await?)
    }

    /// The release timeline: scheduled and recently published releases.
    ///
    /// # Errors
    /// Propagates database failures.
    pub async fn dashboard_releases(
        &self,
        actor: &Actor,
        desk_id: Option<Uuid>,
        status: Option<&str>,
    ) -> Result<Vec<ReleaseTimelineRow>, NewsroomError> {
        self.require_desk_read(actor, desk_id).await?;
        Ok(sqlx::query_as::<_, ReleaseTimelineRow>(
            "SELECT rel.id, rel.story_id, s.title AS story_title, rel.release_type, rel.status, \
                    rel.channels, rel.channel_schedule, rel.receipts, rel.scheduled_at, rel.published_at, \
                    s.desk_id AS desk_id, d.name AS desk_name \
             FROM meridian.releases rel \
             LEFT JOIN meridian.stories s ON s.id = rel.story_id \
             LEFT JOIN meridian.desks d ON d.id = s.desk_id \
             WHERE ($1::uuid IS NULL OR s.desk_id = $1) AND ($2::text IS NULL OR rel.status = $2) \
             ORDER BY COALESCE(rel.scheduled_at, rel.published_at, rel.created_at) DESC LIMIT 100",
        )
        .bind(desk_id)
        .bind(status)
        .fetch_all(self.pool())
        .await?)
    }

    /// The entities behind a dashboard metric, so a number can be opened. `key`
    /// is a fixed metric name or `pipeline:{state}`; unknown keys are rejected.
    ///
    /// # Errors
    /// [`NewsroomError::Unprocessable`] for an unknown key; database failures.
    pub async fn drilldown(
        &self,
        actor: &Actor,
        key: &str,
        desk_id: Option<Uuid>,
        owner_id: Option<Uuid>,
    ) -> Result<Vec<DrilldownRow>, NewsroomError> {
        self.require_desk_read(actor, desk_id).await?;
        // Fixed SQL per key; filters bound uniformly as ($1 desk, $2 owner).
        let sql: String = match key {
            "pending_reviews" =>
                "SELECT 'review' AS entity_type, r.id::text AS entity_id, \
                        COALESCE(s.title, 'Unknown story') AS title, \
                        replace(r.kind, '_', ' ') || ' review' AS subtitle, \
                        '/editor?story=' || r.story_id || '&panel=reviews' AS href \
                 FROM meridian.reviews r LEFT JOIN meridian.stories s ON s.id = r.story_id \
                 WHERE r.decision = 'pending' \
                   AND ($1::uuid IS NULL OR s.desk_id = $1) AND ($2::uuid IS NULL OR s.author_id = $2) \
                 LIMIT 100".to_owned(),
            "overdue_tasks" =>
                "SELECT 'task' AS entity_type, t.id::text AS entity_id, t.title, \
                        'Overdue' AS subtitle, '/tasks/' || t.id AS href \
                 FROM meridian.tasks t \
                 WHERE t.due_at < now() AND t.status NOT IN ('done', 'cancelled') \
                   AND ($1::uuid IS NULL OR t.desk_id = $1) AND ($2::uuid IS NULL OR t.assigned_to_id = $2) \
                 LIMIT 100".to_owned(),
            "open_corrections" =>
                "SELECT 'correction' AS entity_type, c.id::text AS entity_id, \
                        COALESCE(s.title, 'Unknown story') AS title, \
                        replace(c.classification, '_', ' ') AS subtitle, \
                        '/editor?story=' || c.story_id || '&panel=publish' AS href \
                 FROM meridian.corrections c LEFT JOIN meridian.stories s ON s.id = c.story_id \
                 WHERE c.status = 'open' \
                   AND ($1::uuid IS NULL OR s.desk_id = $1) AND ($2::uuid IS NULL OR s.author_id = $2) \
                 LIMIT 100".to_owned(),
            "failed_releases" =>
                "SELECT 'release' AS entity_type, rel.id::text AS entity_id, \
                        COALESCE(s.title, 'Unknown story') AS title, \
                        'Release ' || rel.status AS subtitle, \
                        '/editor?story=' || rel.story_id || '&panel=publish' AS href \
                 FROM meridian.releases rel LEFT JOIN meridian.stories s ON s.id = rel.story_id \
                 WHERE rel.status IN ('failed', 'partial_failure') \
                   AND ($1::uuid IS NULL OR s.desk_id = $1) AND ($2::uuid IS NULL OR s.author_id = $2) \
                 LIMIT 100".to_owned(),
            "scheduled_releases" =>
                "SELECT 'release' AS entity_type, rel.id::text AS entity_id, \
                        COALESCE(s.title, 'Unknown story') AS title, \
                        'Scheduled ' || to_char(rel.scheduled_at, 'DD Mon HH24:MI') AS subtitle, \
                        '/editor?story=' || rel.story_id || '&panel=publish' AS href \
                 FROM meridian.releases rel LEFT JOIN meridian.stories s ON s.id = rel.story_id \
                 WHERE rel.status = 'scheduled' \
                   AND ($1::uuid IS NULL OR s.desk_id = $1) AND ($2::uuid IS NULL OR s.author_id = $2) \
                 ORDER BY rel.scheduled_at LIMIT 100".to_owned(),
            "stories_due_today" =>
                "SELECT 'story' AS entity_type, s.id::text AS entity_id, s.title, \
                        'Due today' AS subtitle, '/editor?story=' || s.id AS href \
                 FROM meridian.stories s \
                 WHERE s.due_at >= now() AND s.due_at < now() + interval '24 hours' \
                   AND ($1::uuid IS NULL OR s.desk_id = $1) AND ($2::uuid IS NULL OR s.author_id = $2) \
                 ORDER BY s.due_at LIMIT 100".to_owned(),
            other => {
                let Some(state) = other.strip_prefix("pipeline:") else {
                    return Err(NewsroomError::Unprocessable(format!(
                        "'{key}' is not a drill-down key"
                    )));
                };
                if !PIPELINE_STATES.contains(&state) && !["parked", "archived"].contains(&state) {
                    return Err(NewsroomError::Unprocessable(format!(
                        "'{state}' is not a workflow state"
                    )));
                }
                // The state is validated against a fixed set, so embedding it is safe.
                format!(
                    "SELECT 'story' AS entity_type, s.id::text AS entity_id, s.title AS title, \
                            '{state}' AS subtitle, '/editor?story=' || s.id AS href \
                     FROM meridian.stories s WHERE s.workflow_state = '{state}' \
                       AND ($1::uuid IS NULL OR s.desk_id = $1) AND ($2::uuid IS NULL OR s.author_id = $2) \
                     ORDER BY s.updated_at LIMIT 100"
                )
            }
        };
        Ok(sqlx::query_as::<_, DrilldownRow>(&sql)
            .bind(desk_id)
            .bind(owner_id)
            .fetch_all(self.pool())
            .await?)
    }

    /// Captures this hour's newsroom-wide metric snapshot (0-filled pipeline
    /// states + open tasks + pending reviews), first-write-wins per bucket.
    /// Returns the number of series written (0 once the bucket is captured).
    ///
    /// Writes a global snapshot, so it is administrator-only — a scheduler drives it
    /// with admin credentials; without the gate any authenticated user could write
    /// the hourly metric row.
    ///
    /// # Errors
    /// [`NewsroomError::Forbidden`] for a non-administrator; database failures.
    pub async fn capture_metrics(&self, actor: &Actor) -> Result<u64, NewsroomError> {
        if !actor.is_admin {
            return Err(NewsroomError::Forbidden {
                capability: "admin".to_owned(),
                reason: "Metric capture is administrator-only".to_owned(),
            });
        }
        let result = sqlx::query(
            "INSERT INTO meridian.metric_snapshots \
               (id, metric_key, scope_type, scope_id, bucket_start, bucket_end, value) \
             SELECT gen_random_uuid(), 'pipeline:' || st.state, 'global', '', \
                    date_trunc('hour', now()), date_trunc('hour', now()) + interval '1 hour', \
                    (SELECT count(*) FROM meridian.stories s WHERE s.workflow_state = st.state) \
             FROM (VALUES ('intake'),('proposed'),('assigned'),('reporting'),('drafting'), \
                          ('desk_review'),('verification'),('copy_standards'),('ready')) AS st(state) \
             UNION ALL \
             SELECT gen_random_uuid(), 'open_tasks', 'global', '', \
                    date_trunc('hour', now()), date_trunc('hour', now()) + interval '1 hour', \
                    (SELECT count(*) FROM meridian.tasks WHERE status NOT IN ('done', 'cancelled')) \
             UNION ALL \
             SELECT gen_random_uuid(), 'pending_reviews', 'global', '', \
                    date_trunc('hour', now()), date_trunc('hour', now()) + interval '1 hour', \
                    (SELECT count(*) FROM meridian.reviews WHERE decision = 'pending') \
             UNION ALL \
             SELECT gen_random_uuid(), 'overdue_tasks', 'global', '', \
                    date_trunc('hour', now()), date_trunc('hour', now()) + interval '1 hour', \
                    (SELECT count(*) FROM meridian.tasks \
                       WHERE status NOT IN ('done','cancelled') AND due_at < now()) \
             UNION ALL \
             SELECT gen_random_uuid(), 'open_corrections', 'global', '', \
                    date_trunc('hour', now()), date_trunc('hour', now()) + interval '1 hour', \
                    (SELECT count(*) FROM meridian.corrections WHERE status = 'open') \
             UNION ALL \
             SELECT gen_random_uuid(), 'scheduled_releases', 'global', '', \
                    date_trunc('hour', now()), date_trunc('hour', now()) + interval '1 hour', \
                    (SELECT count(*) FROM meridian.releases WHERE status = 'scheduled') \
             UNION ALL \
             SELECT gen_random_uuid(), 'stories_due_today', 'global', '', \
                    date_trunc('hour', now()), date_trunc('hour', now()) + interval '1 hour', \
                    (SELECT count(*) FROM meridian.stories \
                       WHERE due_at::date = (now() AT TIME ZONE 'UTC')::date) \
             ON CONFLICT (metric_key, scope_type, scope_id, bucket_start) DO NOTHING",
        )
        .execute(self.pool())
        .await?;
        Ok(result.rows_affected())
    }

    /// The observed values of one metric series since `since`, oldest first.
    ///
    /// # Errors
    /// Propagates database failures.
    pub async fn metric_series(
        &self,
        metric_key: &str,
        scope_type: &str,
        scope_id: &str,
        since: time::OffsetDateTime,
    ) -> Result<Vec<MetricPoint>, NewsroomError> {
        Ok(sqlx::query_as::<_, MetricPoint>(
            "SELECT bucket_start, value FROM meridian.metric_snapshots \
             WHERE metric_key = $1 AND scope_type = $2 AND scope_id = $3 AND bucket_start >= $4 \
             ORDER BY bucket_start",
        )
        .bind(metric_key)
        .bind(scope_type)
        .bind(scope_id)
        .bind(since)
        .fetch_all(self.pool())
        .await?)
    }

    /// Per-person workload: active stories authored, pending reviews assigned,
    /// and overdue / urgent tasks. Scoped to a desk's members when given.
    ///
    /// # Errors
    /// Propagates database failures.
    #[allow(clippy::cast_precision_loss)]
    pub async fn dashboard_workload(
        &self,
        actor: &Actor,
        desk_id: Option<Uuid>,
    ) -> Result<serde_json::Value, NewsroomError> {
        // Staff capacity scores are sensitive; `dashboard.view_workload` exists to
        // gate them and was never enforced. Require it, scoped to the requested desk:
        // a global holder sees the newsroom-wide view (desk_id = None), a desk-scoped
        // holder only their desk.
        authz::require(self.pool(), actor, "dashboard.view_workload", desk_id).await?;
        let active: Vec<&str> = ACTIVE_WORKFLOW_STATES.to_vec();
        let rows = sqlx::query_as::<_, WorkloadRow>(
            "SELECT u.id AS user_id, u.display_name, u.role, \
               COALESCE(s.active_stories, 0) AS active_stories, \
               COALESCE(r.pending_reviews, 0) AS pending_reviews, \
               COALESCE(ot.overdue_tasks, 0) AS overdue_tasks, \
               COALESCE(ut.urgent_tasks, 0) AS urgent_tasks \
             FROM meridian.users u \
             LEFT JOIN ( \
               SELECT author_id, count(*) AS active_stories FROM meridian.stories \
               WHERE workflow_state = ANY($2) GROUP BY author_id) s ON s.author_id = u.id \
             LEFT JOIN ( \
               SELECT assigned_to_id, count(*) AS pending_reviews FROM meridian.reviews \
               WHERE decision = 'pending' GROUP BY assigned_to_id) r ON r.assigned_to_id = u.id \
             LEFT JOIN ( \
               SELECT assigned_to_id, count(*) AS overdue_tasks FROM meridian.tasks \
               WHERE due_at < now() AND status NOT IN ('done', 'cancelled') \
               GROUP BY assigned_to_id) ot ON ot.assigned_to_id = u.id \
             LEFT JOIN ( \
               SELECT assigned_to_id, count(*) AS urgent_tasks FROM meridian.tasks \
               WHERE priority = 'urgent' AND status NOT IN ('done', 'cancelled') \
               GROUP BY assigned_to_id) ut ON ut.assigned_to_id = u.id \
             WHERE u.is_active \
               AND ($1::uuid IS NULL OR u.id IN ( \
                 SELECT user_id FROM meridian.desk_memberships WHERE desk_id = $1)) \
             ORDER BY u.display_name",
        )
        .bind(desk_id)
        .bind(&active)
        .fetch_all(self.pool())
        .await?;

        // Weighted workload score per staffer, matching the legacy workload model.
        let (w_story, w_review, w_overdue, w_urgent, capacity_threshold) =
            (1.0_f64, 2.0, 3.0, 2.0, 10.0);
        let entries: Vec<serde_json::Value> = rows
            .iter()
            .map(|row| {
                let score = row.active_stories as f64 * w_story
                    + row.pending_reviews as f64 * w_review
                    + row.overdue_tasks as f64 * w_overdue
                    + row.urgent_tasks as f64 * w_urgent;
                serde_json::json!({
                    "user_id": row.user_id,
                    "display_name": row.display_name,
                    "role": row.role,
                    "active_stories": row.active_stories,
                    "pending_reviews": row.pending_reviews,
                    "overdue_tasks": row.overdue_tasks,
                    "urgent_tasks": row.urgent_tasks,
                    "workload_score": score,
                    "availability_status": "available",
                    "is_available": true,
                    "over_capacity": score > capacity_threshold,
                })
            })
            .collect();

        let unassigned_stories: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM meridian.stories WHERE author_id IS NULL AND workflow_state = ANY($1)",
        )
        .bind(&active)
        .fetch_one(self.pool())
        .await?;
        let unassigned_events: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM meridian.coverage_events WHERE desk_id IS NULL",
        )
        .fetch_one(self.pool())
        .await
        .unwrap_or(0);
        let active_staff = i64::try_from(entries.len()).unwrap_or(i64::MAX);

        Ok(serde_json::json!({
            "entries": entries,
            "active_staff": active_staff,
            "unavailable_staff": 0,
            "unassigned_stories": unassigned_stories,
            "unassigned_events": unassigned_events,
            "weights": {
                "story": w_story,
                "review": w_review,
                "overdue": w_overdue,
                "urgent": w_urgent,
                "capacity_threshold": capacity_threshold,
            },
            "generated_at": crate::rfc3339(time::OffsetDateTime::now_utc()),
        }))
    }
}

#[derive(sqlx::FromRow)]
struct SummaryRow {
    pending_reviews: i64,
    overdue_tasks: i64,
    open_corrections: i64,
    scheduled_releases: i64,
    stories_due_today: i64,
    // Breakdowns, computed only where the schema genuinely supports them. See
    // `dashboard_summary` for which metric gets which and why the rest stay at
    // their empty-state values rather than being approximated.
    tasks_urgent: i64,
    tasks_at_risk: i64,
    stories_overdue: i64,
    stories_urgent: i64,
    releases_overdue: i64,
    corrections_severe: i64,
}

/// A metric's baseline for today: the earliest snapshot recorded at or after
/// midnight UTC. An absent row means no snapshot exists for the day, in which
/// case the change is reported as zero rather than guessed from a partial series.
#[derive(Debug, sqlx::FromRow)]
struct MetricBaseline {
    metric_key: String,
    value: i64,
}

/// The headline counts plus their breakdowns, bound as (`$1` desk, `$2` owner).
///
/// Shared by `dashboard_summary` and `dashboard_home` because both project the
/// same [`SummaryRow`] — when they were two copies, adding a column to one left
/// the other failing to decode at runtime rather than at compile time.
const SUMMARY_SQL: &str = "SELECT \
   (SELECT count(*) FROM meridian.reviews r WHERE r.decision = 'pending') AS pending_reviews, \
   (SELECT count(*) FROM meridian.tasks t WHERE t.status NOT IN ('done','cancelled') \
        AND t.due_at < now() AND ($1::uuid IS NULL OR t.desk_id = $1)) AS overdue_tasks, \
   (SELECT count(*) FROM meridian.corrections c WHERE c.status = 'open') AS open_corrections, \
   (SELECT count(*) FROM meridian.releases rel LEFT JOIN meridian.stories rs ON rs.id = rel.story_id \
      WHERE rel.status = 'scheduled' AND ($1::uuid IS NULL OR rs.desk_id = $1)) AS scheduled_releases, \
   (SELECT count(*) FROM meridian.stories s \
      WHERE s.due_at::date = (now() AT TIME ZONE 'UTC')::date \
        AND ($1::uuid IS NULL OR s.desk_id = $1) AND ($2::uuid IS NULL OR s.author_id = $2)) AS stories_due_today, \
   (SELECT count(*) FROM meridian.tasks t WHERE t.status NOT IN ('done','cancelled') \
        AND t.due_at < now() AND t.priority = 'urgent' \
        AND ($1::uuid IS NULL OR t.desk_id = $1)) AS tasks_urgent, \
   (SELECT count(*) FROM meridian.tasks t WHERE t.status NOT IN ('done','cancelled') \
        AND t.due_at >= now() AND t.due_at < now() + interval '24 hours' \
        AND ($1::uuid IS NULL OR t.desk_id = $1)) AS tasks_at_risk, \
   (SELECT count(*) FROM meridian.stories s \
      WHERE s.due_at < now() AND s.workflow_state NOT IN ('ready','archived') \
        AND ($1::uuid IS NULL OR s.desk_id = $1) AND ($2::uuid IS NULL OR s.author_id = $2)) AS stories_overdue, \
   (SELECT count(*) FROM meridian.stories s \
      WHERE s.due_at::date = (now() AT TIME ZONE 'UTC')::date AND s.priority = 'urgent' \
        AND ($1::uuid IS NULL OR s.desk_id = $1) AND ($2::uuid IS NULL OR s.author_id = $2)) AS stories_urgent, \
   (SELECT count(*) FROM meridian.releases rel LEFT JOIN meridian.stories rs ON rs.id = rel.story_id \
      WHERE rel.status = 'scheduled' AND rel.scheduled_at < now() \
        AND ($1::uuid IS NULL OR rs.desk_id = $1)) AS releases_overdue, \
   (SELECT count(*) FROM meridian.corrections c WHERE c.status = 'open' \
        AND c.classification IN ('retraction','legal_privacy_removal')) AS corrections_severe";

/// One recent audit entry for the activity feed.
#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct ActivityRow {
    pub id: Uuid,
    pub actor_id: Option<Uuid>,
    pub actor_name: Option<String>,
    pub action: String,
    pub entity_type: String,
    pub entity_id: String,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: time::OffsetDateTime,
    pub context: serde_json::Value,
}

/// One release on the publishing timeline.
#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct ReleaseTimelineRow {
    pub id: Uuid,
    pub story_id: Uuid,
    pub story_title: Option<String>,
    pub release_type: String,
    pub status: String,
    pub channels: serde_json::Value,
    pub channel_schedule: serde_json::Value,
    pub receipts: serde_json::Value,
    #[serde(with = "time::serde::rfc3339::option")]
    pub scheduled_at: Option<time::OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub published_at: Option<time::OffsetDateTime>,
    pub desk_id: Option<Uuid>,
    pub desk_name: Option<String>,
}

#[derive(sqlx::FromRow)]
struct WorkloadRow {
    user_id: Uuid,
    display_name: String,
    role: String,
    active_stories: i64,
    pending_reviews: i64,
    overdue_tasks: i64,
    urgent_tasks: i64,
}

#[cfg(test)]
mod tests {
    use super::{round_to_i64, share, sla_status};

    #[test]
    fn sla_status_warns_at_the_desks_own_percentage() {
        // A desk that warns at 80% of a 10-hour target.
        assert_eq!(sla_status(2.0, 10.0, 80), Some("ok"));
        assert_eq!(sla_status(7.9, 10.0, 80), Some("ok"));
        assert_eq!(sla_status(8.0, 10.0, 80), Some("warn"));
        assert_eq!(sla_status(9.9, 10.0, 80), Some("warn"));
        assert_eq!(sla_status(10.0, 10.0, 80), Some("breach"));
        assert_eq!(sla_status(40.0, 10.0, 80), Some("breach"));
        // A stricter desk warns earlier on the same wait.
        assert_eq!(sla_status(5.0, 10.0, 50), Some("warn"));
        assert_eq!(sla_status(5.0, 10.0, 80), Some("ok"));
    }

    #[test]
    fn a_state_without_a_usable_target_cannot_breach() {
        assert_eq!(sla_status(100.0, 0.0, 80), None);
        assert_eq!(sla_status(100.0, -5.0, 80), None);
        assert_eq!(sla_status(100.0, f64::NAN, 80), None);
        assert_eq!(sla_status(f64::INFINITY, 10.0, 80), None);
    }

    #[test]
    fn share_is_zero_for_an_empty_queue_rather_than_undefined() {
        assert!((share(0, 0) - 0.0).abs() < f64::EPSILON);
        assert!((share(5, 0) - 0.0).abs() < f64::EPSILON);
        assert!((share(1, 2) - 0.5).abs() < f64::EPSILON);
        assert!((share(1, 3) - 0.33).abs() < f64::EPSILON);
    }

    #[test]
    fn a_baseline_rounds_to_a_whole_number_of_stories_and_never_goes_negative() {
        assert_eq!(round_to_i64(2.4), 2);
        assert_eq!(round_to_i64(2.5), 3);
        assert_eq!(round_to_i64(-1.0), 0);
        assert_eq!(round_to_i64(f64::MAX), i64::MAX);
    }
}
