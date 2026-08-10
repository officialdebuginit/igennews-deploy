//! Reader subscriptions / entitlements (design: docs/MARKET-RESEARCH-AND-GAPS.md §7).
//!
//! The entitlement record behind the paywall, decoupled from any payment provider: a
//! manual grant now, or a row a Stripe integration would upsert from Stripe's webhooks.
//! Lifecycle changes emit `subscription.started` / `subscription.canceled` on the
//! outbound webhook fabric, so downstream (email, CRM, analytics) stays in sync.

use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::{Actor, NewsroomError, NewsroomService, authz};

/// A reader subscription. `subscriber_email` is the identity key; `status` gates
/// entitlement.
#[derive(Clone, Debug, Serialize, FromRow)]
pub struct Subscription {
    pub id: Uuid,
    pub subscriber_email: String,
    pub subscriber_name: Option<String>,
    pub plan: String,
    pub status: String,
    pub source: String,
    pub external_ref: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub started_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339::option")]
    pub current_period_end: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub canceled_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

/// Create input for a subscription.
#[derive(Clone, Debug, Deserialize)]
pub struct SubscriptionInput {
    pub subscriber_email: String,
    #[serde(default)]
    pub subscriber_name: Option<String>,
    #[serde(default = "standard_plan")]
    pub plan: String,
    #[serde(default = "manual_source")]
    pub source: String,
    #[serde(default)]
    pub external_ref: Option<String>,
    #[serde(default)]
    pub current_period_end: Option<String>,
}

fn standard_plan() -> String {
    "standard".to_owned()
}
fn manual_source() -> String {
    "manual".to_owned()
}

const SUBSCRIPTION_COLUMNS: &str = "id, subscriber_email, subscriber_name, plan, status, source, \
    external_ref, started_at, current_period_end, canceled_at, created_at";

impl NewsroomService {
    /// Grants a subscription (gated on `subscriptions.manage`) and fires
    /// `subscription.started`. A live subscription for the same email is returned
    /// unchanged rather than duplicated.
    ///
    /// # Errors
    /// [`NewsroomError::Forbidden`] without the capability; database failures.
    pub async fn create_subscription(
        &self,
        actor: &Actor,
        input: &SubscriptionInput,
    ) -> Result<Subscription, NewsroomError> {
        authz::require(self.pool(), actor, "subscriptions.manage", None).await?;
        let email = input.subscriber_email.trim().to_lowercase();
        if email.is_empty() {
            return Err(NewsroomError::Unprocessable("A subscriber email is required".to_owned()));
        }
        // Idempotent: if a live subscription already exists for this email, return it.
        if let Some(existing) = sqlx::query_as::<_, Subscription>(&format!(
            "SELECT {SUBSCRIPTION_COLUMNS} FROM meridian.subscriptions \
             WHERE lower(subscriber_email) = $1 AND status IN ('active', 'trialing') LIMIT 1"
        ))
        .bind(&email)
        .fetch_optional(self.pool())
        .await?
        {
            return Ok(existing);
        }

        let period_end = input
            .current_period_end
            .as_deref()
            .and_then(|value| OffsetDateTime::parse(value, &time::format_description::well_known::Rfc3339).ok());

        let mut tx = self.pool().begin().await?;
        let subscription = sqlx::query_as::<_, Subscription>(&format!(
            "INSERT INTO meridian.subscriptions \
               (subscriber_email, subscriber_name, plan, source, external_ref, current_period_end, created_by_id) \
             VALUES ($1, $2, $3, $4, $5, $6, $7) RETURNING {SUBSCRIPTION_COLUMNS}"
        ))
        .bind(&email)
        .bind(&input.subscriber_name)
        .bind(&input.plan)
        .bind(&input.source)
        .bind(&input.external_ref)
        .bind(period_end)
        .bind(actor.id)
        .fetch_one(&mut *tx)
        .await?;
        crate::audit(
            &mut *tx,
            actor.id,
            "subscription.started",
            "subscription",
            &subscription.id.to_string(),
            None,
            Some(serde_json::json!({ "plan": subscription.plan, "source": subscription.source })),
        )
        .await?;
        Self::enqueue_event(
            &mut *tx,
            "subscription.started",
            &subscription.id.to_string(),
            None,
            serde_json::json!({
                "id": subscription.id,
                "subscriber_email": subscription.subscriber_email,
                "plan": subscription.plan,
                "status": subscription.status,
                "source": subscription.source,
            }),
            Some(actor.id),
        )
        .await?;
        tx.commit().await?;
        Ok(subscription)
    }

    /// Cancels a subscription and fires `subscription.canceled`.
    ///
    /// # Errors
    /// [`NewsroomError::Forbidden`] without the capability; [`NewsroomError::NotFound`];
    /// database failures.
    pub async fn cancel_subscription(
        &self,
        actor: &Actor,
        subscription_id: Uuid,
    ) -> Result<Subscription, NewsroomError> {
        authz::require(self.pool(), actor, "subscriptions.manage", None).await?;
        let mut tx = self.pool().begin().await?;
        let subscription = sqlx::query_as::<_, Subscription>(&format!(
            "UPDATE meridian.subscriptions SET status = 'canceled', canceled_at = now(), updated_at = now() \
             WHERE id = $1 RETURNING {SUBSCRIPTION_COLUMNS}"
        ))
        .bind(subscription_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(NewsroomError::NotFound("Subscription"))?;
        crate::audit(
            &mut *tx,
            actor.id,
            "subscription.canceled",
            "subscription",
            &subscription.id.to_string(),
            None,
            None,
        )
        .await?;
        Self::enqueue_event(
            &mut *tx,
            "subscription.canceled",
            &subscription.id.to_string(),
            None,
            serde_json::json!({
                "id": subscription.id,
                "subscriber_email": subscription.subscriber_email,
                "plan": subscription.plan,
            }),
            Some(actor.id),
        )
        .await?;
        tx.commit().await?;
        Ok(subscription)
    }

    /// All subscriptions, newest first (gated on `subscriptions.manage`).
    ///
    /// # Errors
    /// [`NewsroomError::Forbidden`] without the capability; database failures.
    pub async fn list_subscriptions(
        &self,
        actor: &Actor,
    ) -> Result<Vec<Subscription>, NewsroomError> {
        authz::require(self.pool(), actor, "subscriptions.manage", None).await?;
        Ok(sqlx::query_as::<_, Subscription>(&format!(
            "SELECT {SUBSCRIPTION_COLUMNS} FROM meridian.subscriptions ORDER BY created_at DESC LIMIT 500"
        ))
        .fetch_all(self.pool())
        .await?)
    }

    /// Whether `email` currently holds a live subscription — the paywall entitlement
    /// check. Public (no auth): a reader proves entitlement by their email. An expired
    /// `current_period_end` does not count.
    ///
    /// # Errors
    /// Database failures.
    pub async fn is_entitled(&self, email: &str) -> Result<bool, NewsroomError> {
        let email = email.trim().to_lowercase();
        if email.is_empty() {
            return Ok(false);
        }
        Ok(sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS ( \
               SELECT 1 FROM meridian.subscriptions \
               WHERE lower(subscriber_email) = $1 AND status IN ('active', 'trialing') \
                 AND (current_period_end IS NULL OR current_period_end > now()))",
        )
        .bind(&email)
        .fetch_one(self.pool())
        .await?)
    }
}
