//! Module 3 — newsroom structure (desks, memberships, schedules, SLAs,
//! invitations) and the authorization engine its access checks depend on.
//!
//! The web crate owns HTTP; this crate owns the domain rules and repositories so
//! they can be exercised without a server. Ported from the legacy `desks` and
//! `permissions` packages, whose behaviour remains the oracle until differential
//! tests move ownership.

pub mod activity;
pub mod admin;
pub mod attention;
pub mod authz;
pub mod awareness;
pub mod branding;
pub mod capabilities;
pub mod dashboard;
pub mod dashviews;
pub mod desk_email;
pub mod desks;
pub mod kyc_signing;
pub mod legal_docs;
pub mod legal_signing;
pub mod feed;
pub mod media;
pub mod nav;
pub mod people;
pub mod pitches;
pub mod reviews;
pub mod search;
pub mod stories;
pub mod sub_sectors;
pub mod subscriptions;
pub mod tasks;
pub mod verification;
pub mod webhooks;

use serde::Serialize;
use sqlx::PgPool;
use thiserror::Error;
use uuid::Uuid;

pub use authz::Actor;
pub use capabilities::Role;

/// An instant as RFC 3339, for embedding in a `serde_json::json!` literal.
///
/// Struct fields get there with `#[serde(with = "time::serde::rfc3339")]`, but a
/// timestamp written straight into a `json!` bypasses serde's field attributes and
/// falls back to `time`'s `Display` — which is *not* RFC 3339 (`2026-07-29
/// 02:05:23.350678 +00:00:00`: a space for the `T`, and a three-part offset). The
/// legacy `OpenAPI` declares these fields `format: date-time`, so that output breaks
/// the published contract, and the shape-based parity harness cannot see it
/// because a string is a string either way.
///
/// Nine call sites produced exactly that. Use this at every one.
#[must_use]
pub fn rfc3339(at: time::OffsetDateTime) -> String {
    at.format(&time::format_description::well_known::Rfc3339).unwrap_or_default()
}

/// Every way a Module 3 domain operation can fail, mapped to HTTP at the edge.
#[derive(Debug, Error)]
pub enum NewsroomError {
    #[error("{0} not found")]
    NotFound(&'static str),
    #[error("missing capability '{capability}'")]
    Forbidden { capability: String, reason: String },
    #[error("{0}")]
    Conflict(String),
    #[error("{0}")]
    Unprocessable(String),
    #[error("database operation failed: {0}")]
    Database(#[from] sqlx::Error),
}

/// One row appended to the shared audit trail. Ported from `services.audit`.
///
/// # Errors
///
/// Returns [`NewsroomError::Database`] if the row cannot be written.
pub async fn audit<'a, E>(
    executor: E,
    actor_id: Uuid,
    action: &str,
    entity_type: &str,
    entity_id: &str,
    before: Option<serde_json::Value>,
    after: Option<serde_json::Value>,
) -> Result<(), NewsroomError>
where
    E: sqlx::PgExecutor<'a>,
{
    sqlx::query(
        "INSERT INTO meridian.audit_events \
         (id, actor_id, action, entity_type, entity_id, before, after, context) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, '{}'::jsonb)",
    )
    .bind(Uuid::now_v7())
    .bind(actor_id)
    .bind(action)
    .bind(entity_type)
    .bind(entity_id)
    .bind(before)
    .bind(after)
    .execute(executor)
    .await?;
    Ok(())
}

/// A newsroom-structure service bound to the pooled database.
#[derive(Clone)]
pub struct NewsroomService {
    pool: PgPool,
}

impl NewsroomService {
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    #[must_use]
    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// A system [`Actor`] (the earliest admin user) for background jobs such as the
    /// release scheduler. `None` if no admin user exists yet.
    ///
    /// # Errors
    /// Propagates database failures.
    pub async fn system_actor(&self) -> Result<Option<Actor>, NewsroomError> {
        let row: Option<(Uuid, String, bool)> = sqlx::query_as(
            "SELECT id, role, is_admin FROM meridian.users \
             WHERE is_admin = true ORDER BY created_at LIMIT 1",
        )
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.and_then(|(id, role, is_admin)| {
            capabilities::Role::from_wire(&role).map(|role| Actor { id, role, is_admin })
        }))
    }
}

/// One line of a decision's audit trail — the legacy `trace` entry shape.
#[derive(Debug, Serialize)]
pub struct TraceEntry {
    pub rule: &'static str,
    pub outcome: &'static str,
    pub detail: String,
}

/// A capability decision serialized for the effective-permission viewer and the
/// permission simulator. Legacy carries a `trace` array (the rules that fired),
/// not a flat `reason`.
#[derive(Debug, Serialize)]
pub struct DecisionView {
    pub capability: String,
    pub allowed: bool,
    pub source: &'static str,
    pub trace: Vec<TraceEntry>,
}

impl From<authz::Decision> for DecisionView {
    fn from(decision: authz::Decision) -> Self {
        let outcome = if decision.allowed { "allow" } else { "deny" };
        Self {
            capability: decision.capability,
            allowed: decision.allowed,
            source: decision.source,
            trace: vec![TraceEntry {
                rule: decision.source,
                outcome,
                detail: decision.reason,
            }],
        }
    }
}
