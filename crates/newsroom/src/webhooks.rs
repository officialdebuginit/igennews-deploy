//! Outbound webhooks & the event fabric (design: docs/MARKET-RESEARCH-AND-GAPS.md §6).
//!
//! Domain mutations enqueue an event into the `webhook_events` outbox in their own
//! transaction. A background dispatcher fans each event out to the subscriptions
//! whose type-patterns and desk-filter match, then delivers over HTTP with the Svix
//! retry schedule and Standard-Webhooks HMAC signing. Delivery is at-least-once and
//! unordered by design — consumers dedupe on the `webhook-id` header.

use base64::Engine as _;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::FromRow;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::{Actor, NewsroomError, NewsroomService, authz};

/// Seconds to wait before each retry, after the immediate first attempt — Svix's
/// schedule: 5s, 5m, 30m, 2h, 5h, 10h, 10h (8 attempts total with the first).
const RETRY_BACKOFF_SECS: [i64; 7] = [5, 300, 1_800, 7_200, 18_000, 36_000, 36_000];
/// Per-delivery HTTP timeout (Svix uses 15s).
const DELIVERY_TIMEOUT_SECS: u64 = 15;
/// Replay-protection window a receiver should enforce (documented, not enforced here).
pub const SIGNATURE_TOLERANCE_SECS: i64 = 300;
/// Auto-disable an endpoint after this many days of unbroken failure (Svix: 5).
const AUTO_DISABLE_DAYS: i64 = 5;

// ---------------------------------------------------------------------------------
// Signing (Standard Webhooks / Stripe scheme): HMAC-SHA256 over `id.timestamp.body`.
// ---------------------------------------------------------------------------------

/// HMAC-SHA256 over `sha2`, so we add no dependency for one small primitive.
fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; 32] {
    const BLOCK: usize = 64;
    let mut block_key = [0u8; BLOCK];
    if key.len() > BLOCK {
        let digest = Sha256::digest(key);
        block_key[..32].copy_from_slice(&digest);
    } else {
        block_key[..key.len()].copy_from_slice(key);
    }
    let mut ipad = [0x36u8; BLOCK];
    let mut opad = [0x5cu8; BLOCK];
    for i in 0..BLOCK {
        ipad[i] ^= block_key[i];
        opad[i] ^= block_key[i];
    }
    let mut inner = Sha256::new();
    inner.update(ipad);
    inner.update(message);
    let inner = inner.finalize();
    let mut outer = Sha256::new();
    outer.update(opad);
    outer.update(inner);
    outer.finalize().into()
}

/// The `webhook-signature` value for a payload: `v1,<base64(HMAC)>`. `secret` is the
/// stored `whsec_<base64>` form; the HMAC key is its decoded bytes.
#[must_use]
pub fn sign(secret: &str, msg_id: &str, timestamp: i64, body: &str) -> String {
    let raw = secret.strip_prefix("whsec_").unwrap_or(secret);
    let key = base64::engine::general_purpose::STANDARD
        .decode(raw)
        .unwrap_or_else(|_| raw.as_bytes().to_vec());
    let signed = format!("{msg_id}.{timestamp}.{body}");
    let mac = hmac_sha256(&key, signed.as_bytes());
    format!("v1,{}", base64::engine::general_purpose::STANDARD.encode(mac))
}

/// A fresh signing secret in `whsec_<base64(32 bytes)>` form. Entropy comes from two
/// `UUIDv7`s (CSPRNG-random component) folded through SHA-256.
fn new_secret() -> String {
    let mut material = Vec::with_capacity(32);
    material.extend_from_slice(Uuid::now_v7().as_bytes());
    material.extend_from_slice(Uuid::now_v7().as_bytes());
    let digest = Sha256::digest(&material);
    format!(
        "whsec_{}",
        base64::engine::general_purpose::STANDARD.encode(digest)
    )
}

/// Transport/signing headers an operator may not override via custom headers.
const RESERVED_HEADERS: [&str; 6] = [
    "content-type",
    "webhook-id",
    "webhook-timestamp",
    "webhook-signature",
    "user-agent",
    "host",
];

/// Validates and canonicalizes operator-supplied custom headers into a flat JSON
/// object of `string → string`. Accepts `None`/`null` (→ `{}`), rejects non-object
/// shapes, non-string values, and reserved header names.
fn normalize_headers(
    value: Option<&serde_json::Value>,
) -> Result<serde_json::Value, NewsroomError> {
    let Some(value) = value else {
        return Ok(serde_json::json!({}));
    };
    if value.is_null() {
        return Ok(serde_json::json!({}));
    }
    let map = value.as_object().ok_or_else(|| {
        NewsroomError::Unprocessable("headers must be a JSON object of string values".to_owned())
    })?;
    let mut out = serde_json::Map::with_capacity(map.len());
    for (name, val) in map {
        let name_trimmed = name.trim();
        if name_trimmed.is_empty() {
            return Err(NewsroomError::Unprocessable("header name is empty".to_owned()));
        }
        if RESERVED_HEADERS.contains(&name_trimmed.to_ascii_lowercase().as_str()) {
            return Err(NewsroomError::Unprocessable(format!(
                "header '{name_trimmed}' is reserved and cannot be overridden"
            )));
        }
        let text = val.as_str().ok_or_else(|| {
            NewsroomError::Unprocessable(format!("header '{name_trimmed}' must be a string"))
        })?;
        out.insert(name_trimmed.to_owned(), serde_json::Value::String(text.to_owned()));
    }
    Ok(serde_json::Value::Object(out))
}

/// Whether an event-type pattern matches a concrete event type. Supports exact,
/// `*` (all), `prefix.*`, and `*.suffix`.
fn event_matches(pattern: &str, event_type: &str) -> bool {
    if pattern == "*" || pattern == "*.*" || pattern == event_type {
        return true;
    }
    if let Some(prefix) = pattern.strip_suffix(".*") {
        return event_type
            .strip_prefix(prefix)
            .is_some_and(|rest| rest.starts_with('.'));
    }
    if let Some(suffix) = pattern.strip_prefix("*.") {
        return event_type
            .strip_suffix(suffix)
            .is_some_and(|rest| rest.ends_with('.'));
    }
    false
}

// ---------------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------------

/// A subscription endpoint. `secret` is never serialized back out.
#[derive(Clone, Debug, Serialize, FromRow)]
pub struct WebhookEndpoint {
    pub id: Uuid,
    pub url: String,
    #[serde(skip_serializing)]
    pub secret: String,
    pub description: String,
    pub event_types: Vec<String>,
    pub desk_filter: Option<String>,
    /// The OWNING sector; `None` = org-level. Admins manage every endpoint; a desk
    /// lead manages their own desk's.
    pub desk_id: Option<Uuid>,
    /// Custom headers sent with every delivery (a JSON object of string→string).
    pub headers: serde_json::Value,
    pub active: bool,
    #[serde(with = "time::serde::rfc3339::option")]
    pub disabled_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

/// Create/replace input for an endpoint.
#[derive(Clone, Debug, Deserialize)]
pub struct EndpointInput {
    pub url: String,
    #[serde(default)]
    pub description: String,
    #[serde(default = "all_events")]
    pub event_types: Vec<String>,
    #[serde(default)]
    pub desk_filter: Option<String>,
    /// The owning sector (by id). `None` creates an org-level endpoint (admin only).
    #[serde(default)]
    pub desk_id: Option<Uuid>,
    /// Custom headers (JSON object) sent with every delivery.
    #[serde(default)]
    pub headers: Option<serde_json::Value>,
}

/// Partial update for an endpoint's configuration.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct EndpointPatch {
    #[serde(default)]
    pub active: Option<bool>,
    #[serde(default)]
    pub event_types: Option<Vec<String>>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub desk_filter: Option<String>,
    #[serde(default)]
    pub headers: Option<serde_json::Value>,
}

fn all_events() -> Vec<String> {
    vec!["*".to_owned()]
}

/// The endpoint plus its one-time signing secret, returned only at creation.
#[derive(Clone, Debug, Serialize)]
pub struct EndpointCreated {
    #[serde(flatten)]
    pub endpoint: WebhookEndpoint,
    /// The signing secret — shown once; store it to verify signatures.
    pub secret: String,
}

/// A row of the delivery log, for the admin UI.
#[derive(Clone, Debug, Serialize, FromRow)]
pub struct DeliveryLogRow {
    pub id: Uuid,
    pub event_id: String,
    pub endpoint_id: Uuid,
    pub event_type: String,
    pub attempt: i32,
    pub status: String,
    pub status_code: Option<i32>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

const ENDPOINT_COLUMNS: &str =
    "id, url, secret, description, event_types, desk_filter, desk_id, headers, active, disabled_at, created_at";

impl NewsroomService {
    // -- Subscription management (sector-scoped) -----------------------------------

    /// Authorizes managing an endpoint owned by `desk_id`. An admin / org-level
    /// `webhooks.manage` holder manages **every** endpoint; otherwise a desk-scoped
    /// endpoint requires `desks.manage` on that desk, and an org-level (`None`)
    /// endpoint requires org-level `webhooks.manage` (admins only).
    async fn require_webhook_manage(
        &self,
        actor: &Actor,
        desk_id: Option<Uuid>,
    ) -> Result<(), NewsroomError> {
        if authz::has(self.pool(), actor, "webhooks.manage", None).await? {
            return Ok(());
        }
        match desk_id {
            Some(id) => authz::require(self.pool(), actor, "desks.manage", Some(id)).await,
            None => authz::require(self.pool(), actor, "webhooks.manage", None).await,
        }
    }

    /// The desks whose webhooks `actor` may manage — the desks they belong to and
    /// hold `desks.manage` on. Empty for a plain member; used to scope listings.
    async fn manageable_webhook_desks(&self, actor: &Actor) -> Result<Vec<Uuid>, NewsroomError> {
        let member: Vec<Uuid> =
            sqlx::query_scalar("SELECT desk_id FROM meridian.desk_memberships WHERE user_id = $1")
                .bind(actor.id)
                .fetch_all(self.pool())
                .await?;
        let mut out = Vec::new();
        for desk in member {
            if authz::has(self.pool(), actor, "desks.manage", Some(desk)).await? {
                out.push(desk);
            }
        }
        Ok(out)
    }

    /// Registers a new endpoint and returns it with its one-time secret. A desk-scoped
    /// endpoint (`input.desk_id`) is authorized against that desk; an org-level one
    /// requires admin.
    ///
    /// # Errors
    /// [`NewsroomError::Forbidden`] without the right scope; database failures.
    pub async fn create_webhook_endpoint(
        &self,
        actor: &Actor,
        input: &EndpointInput,
    ) -> Result<EndpointCreated, NewsroomError> {
        self.require_webhook_manage(actor, input.desk_id).await?;
        let secret = new_secret();
        let headers = normalize_headers(input.headers.as_ref())?;
        let endpoint = sqlx::query_as::<_, WebhookEndpoint>(&format!(
            "INSERT INTO meridian.webhook_endpoints \
               (url, secret, description, event_types, desk_filter, desk_id, headers, created_by_id) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8) RETURNING {ENDPOINT_COLUMNS}"
        ))
        .bind(&input.url)
        .bind(&secret)
        .bind(&input.description)
        .bind(&input.event_types)
        .bind(&input.desk_filter)
        .bind(input.desk_id)
        .bind(&headers)
        .bind(actor.id)
        .fetch_one(self.pool())
        .await?;
        Ok(EndpointCreated { endpoint, secret })
    }

    /// Lists endpoints the viewer may manage: every one for an admin / org-level
    /// `webhooks.manage` holder, otherwise those owned by desks they lead.
    ///
    /// # Errors
    /// [`NewsroomError::Forbidden`] for a viewer who manages no desks; database failures.
    pub async fn list_webhook_endpoints(
        &self,
        actor: &Actor,
    ) -> Result<Vec<WebhookEndpoint>, NewsroomError> {
        if authz::has(self.pool(), actor, "webhooks.manage", None).await? {
            return Ok(sqlx::query_as::<_, WebhookEndpoint>(&format!(
                "SELECT {ENDPOINT_COLUMNS} FROM meridian.webhook_endpoints ORDER BY created_at DESC"
            ))
            .fetch_all(self.pool())
            .await?);
        }
        let desks = self.manageable_webhook_desks(actor).await?;
        if desks.is_empty() {
            return Err(NewsroomError::Forbidden {
                capability: "webhooks.manage".to_owned(),
                reason: "You manage no webhooks".to_owned(),
            });
        }
        Ok(sqlx::query_as::<_, WebhookEndpoint>(&format!(
            "SELECT {ENDPOINT_COLUMNS} FROM meridian.webhook_endpoints \
             WHERE desk_id = ANY($1) ORDER BY created_at DESC"
        ))
        .bind(&desks)
        .fetch_all(self.pool())
        .await?)
    }

    /// Deletes an endpoint (cascades its deliveries). Authorized against the
    /// endpoint's owning desk — admins delete any, a desk lead only their desk's.
    ///
    /// # Errors
    /// [`NewsroomError::Forbidden`] out of scope; [`NewsroomError::NotFound`];
    /// database failures.
    pub async fn delete_webhook_endpoint(
        &self,
        actor: &Actor,
        endpoint_id: Uuid,
    ) -> Result<(), NewsroomError> {
        let owner: Option<Option<Uuid>> =
            sqlx::query_scalar("SELECT desk_id FROM meridian.webhook_endpoints WHERE id = $1")
                .bind(endpoint_id)
                .fetch_optional(self.pool())
                .await?;
        let desk_id = owner.ok_or(NewsroomError::NotFound("Webhook endpoint"))?;
        self.require_webhook_manage(actor, desk_id).await?;
        sqlx::query("DELETE FROM meridian.webhook_endpoints WHERE id = $1")
            .bind(endpoint_id)
            .execute(self.pool())
            .await?;
        Ok(())
    }

    /// Resolves an endpoint's owning desk and authorizes `actor` to manage it,
    /// returning the loaded endpoint. Shared by the per-endpoint control methods.
    async fn authorize_endpoint(
        &self,
        actor: &Actor,
        endpoint_id: Uuid,
    ) -> Result<WebhookEndpoint, NewsroomError> {
        let endpoint = sqlx::query_as::<_, WebhookEndpoint>(&format!(
            "SELECT {ENDPOINT_COLUMNS} FROM meridian.webhook_endpoints WHERE id = $1"
        ))
        .bind(endpoint_id)
        .fetch_optional(self.pool())
        .await?
        .ok_or(NewsroomError::NotFound("Webhook endpoint"))?;
        self.require_webhook_manage(actor, endpoint.desk_id).await?;
        Ok(endpoint)
    }

    /// Rotates an endpoint's signing secret and returns the new value **once**.
    /// Receivers must update before the next delivery — there is no overlap window.
    ///
    /// # Errors
    /// [`NewsroomError::Forbidden`] out of scope; [`NewsroomError::NotFound`];
    /// database failures.
    pub async fn rotate_webhook_secret(
        &self,
        actor: &Actor,
        endpoint_id: Uuid,
    ) -> Result<String, NewsroomError> {
        self.authorize_endpoint(actor, endpoint_id).await?;
        let secret = new_secret();
        sqlx::query("UPDATE meridian.webhook_endpoints SET secret = $2 WHERE id = $1")
            .bind(endpoint_id)
            .bind(&secret)
            .execute(self.pool())
            .await?;
        Ok(secret)
    }

    /// Applies a partial configuration change (enable/disable, event patterns,
    /// description, desk filter, custom headers). Fields left `None` are untouched.
    /// Re-enabling (`active = Some(true)`) clears the auto-disable `disabled_at` stamp
    /// so the failure back-off starts fresh.
    ///
    /// # Errors
    /// [`NewsroomError::Forbidden`] out of scope; [`NewsroomError::NotFound`];
    /// [`NewsroomError::Unprocessable`] for malformed headers; database failures.
    pub async fn update_webhook_endpoint(
        &self,
        actor: &Actor,
        endpoint_id: Uuid,
        patch: &EndpointPatch,
    ) -> Result<WebhookEndpoint, NewsroomError> {
        self.authorize_endpoint(actor, endpoint_id).await?;
        let headers = match patch.headers.as_ref() {
            Some(h) => Some(normalize_headers(Some(h))?),
            None => None,
        };
        Ok(sqlx::query_as::<_, WebhookEndpoint>(&format!(
            "UPDATE meridian.webhook_endpoints SET \
               active = COALESCE($7, active), \
               disabled_at = CASE WHEN $7 IS TRUE THEN NULL ELSE disabled_at END, \
               event_types = COALESCE($2, event_types), \
               description = COALESCE($3, description), \
               desk_filter = CASE WHEN $4 THEN $5 ELSE desk_filter END, \
               headers = COALESCE($6, headers) \
             WHERE id = $1 RETURNING {ENDPOINT_COLUMNS}"
        ))
        .bind(endpoint_id)
        .bind(patch.event_types.as_ref())
        .bind(patch.description.as_ref())
        .bind(patch.desk_filter.is_some())
        .bind(patch.desk_filter.as_ref())
        .bind(headers.as_ref())
        .bind(patch.active)
        .fetch_one(self.pool())
        .await?)
    }

    /// Sends a synthetic `webhook.ping` to a single endpoint so the operator can
    /// confirm connectivity and signature verification. Bypasses fan-out matching —
    /// it targets this endpoint regardless of its event filter — and is delivered by
    /// the normal dispatcher on the next tick.
    ///
    /// # Errors
    /// [`NewsroomError::Forbidden`] out of scope; [`NewsroomError::NotFound`];
    /// database failures.
    pub async fn send_webhook_test(
        &self,
        actor: &Actor,
        endpoint_id: Uuid,
    ) -> Result<(), NewsroomError> {
        self.authorize_endpoint(actor, endpoint_id).await?;
        let payload = serde_json::json!({
            "message": "Test delivery from Meridian — your endpoint is reachable.",
            "endpoint_id": endpoint_id,
        });
        // A pre-fanned-out event (fanned_out = true) with a single hand-created
        // delivery row for this endpoint, so fan_out_events never re-matches it.
        let event_id = format!("evt_{}", Uuid::now_v7().simple());
        sqlx::query(
            "INSERT INTO meridian.webhook_events (id, event_type, subject, desk, payload, actor_id, fanned_out) \
             VALUES ($1, 'webhook.ping', $2, NULL, $3, $4, true)",
        )
        .bind(&event_id)
        .bind(endpoint_id.to_string())
        .bind(&payload)
        .bind(actor.id)
        .execute(self.pool())
        .await?;
        sqlx::query(
            "INSERT INTO meridian.webhook_deliveries \
               (event_id, endpoint_id, event_type, status, next_attempt_at) \
             VALUES ($1, $2, 'webhook.ping', 'pending', now())",
        )
        .bind(&event_id)
        .bind(endpoint_id)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    /// The recent delivery log (newest first) — every endpoint for an admin, or only
    /// the viewer's own desks' endpoints otherwise.
    ///
    /// # Errors
    /// [`NewsroomError::Forbidden`] for a viewer who manages no desks; database failures.
    pub async fn webhook_deliveries(
        &self,
        actor: &Actor,
        limit: i64,
    ) -> Result<Vec<DeliveryLogRow>, NewsroomError> {
        let limit = limit.clamp(1, 500);
        if authz::has(self.pool(), actor, "webhooks.manage", None).await? {
            return Ok(sqlx::query_as::<_, DeliveryLogRow>(
                "SELECT id, event_id, endpoint_id, event_type, attempt, status, status_code, created_at \
                 FROM meridian.webhook_deliveries ORDER BY created_at DESC LIMIT $1",
            )
            .bind(limit)
            .fetch_all(self.pool())
            .await?);
        }
        let desks = self.manageable_webhook_desks(actor).await?;
        if desks.is_empty() {
            return Err(NewsroomError::Forbidden {
                capability: "webhooks.manage".to_owned(),
                reason: "You manage no webhooks".to_owned(),
            });
        }
        Ok(sqlx::query_as::<_, DeliveryLogRow>(
            "SELECT d.id, d.event_id, d.endpoint_id, d.event_type, d.attempt, d.status, d.status_code, d.created_at \
             FROM meridian.webhook_deliveries d \
             JOIN meridian.webhook_endpoints e ON e.id = d.endpoint_id \
             WHERE e.desk_id = ANY($2) ORDER BY d.created_at DESC LIMIT $1",
        )
        .bind(limit)
        .bind(&desks)
        .fetch_all(self.pool())
        .await?)
    }

    // -- Emission (called from domain mutations) -----------------------------------

    /// Enqueues a domain event into the outbox. Runs on any executor, so callers pass
    /// their open transaction and the event is committed atomically with the mutation
    /// — never lost if the dispatcher is down, never sent if the mutation rolls back.
    ///
    /// # Errors
    /// Propagates database failures.
    pub async fn enqueue_event<'a, E>(
        executor: E,
        event_type: &str,
        subject: &str,
        desk: Option<&str>,
        payload: serde_json::Value,
        actor_id: Option<Uuid>,
    ) -> Result<String, NewsroomError>
    where
        E: sqlx::PgExecutor<'a>,
    {
        let id = format!("evt_{}", Uuid::now_v7().simple());
        sqlx::query(
            "INSERT INTO meridian.webhook_events (id, event_type, subject, desk, payload, actor_id) \
             VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(&id)
        .bind(event_type)
        .bind(subject)
        .bind(desk)
        .bind(payload)
        .bind(actor_id)
        .execute(executor)
        .await?;
        Ok(id)
    }

    // -- Dispatch (called from the background worker) ------------------------------

    /// Fans out newly-enqueued outbox events to matching active endpoints, creating a
    /// pending delivery per match. Idempotent via the `fanned_out` flag.
    ///
    /// # Errors
    /// Propagates database failures.
    pub async fn fan_out_events(&self) -> Result<u64, NewsroomError> {
        let events: Vec<(String, String, Option<String>)> = sqlx::query_as(
            "SELECT id, event_type, desk FROM meridian.webhook_events \
             WHERE NOT fanned_out ORDER BY created_at LIMIT 200",
        )
        .fetch_all(self.pool())
        .await?;
        if events.is_empty() {
            return Ok(0);
        }
        let endpoints: Vec<(Uuid, Vec<String>, Option<String>)> = sqlx::query_as(
            "SELECT id, event_types, desk_filter FROM meridian.webhook_endpoints WHERE active",
        )
        .fetch_all(self.pool())
        .await?;

        let mut created = 0u64;
        for (event_id, event_type, desk) in &events {
            for (endpoint_id, patterns, desk_filter) in &endpoints {
                if desk_filter.as_deref().is_some_and(|f| Some(f) != desk.as_deref()) {
                    continue;
                }
                if !patterns.iter().any(|p| event_matches(p, event_type)) {
                    continue;
                }
                sqlx::query(
                    "INSERT INTO meridian.webhook_deliveries (event_id, endpoint_id, event_type) \
                     VALUES ($1, $2, $3)",
                )
                .bind(event_id)
                .bind(endpoint_id)
                .bind(event_type)
                .execute(self.pool())
                .await?;
                created += 1;
            }
            sqlx::query("UPDATE meridian.webhook_events SET fanned_out = true WHERE id = $1")
                .bind(event_id)
                .execute(self.pool())
                .await?;
        }
        Ok(created)
    }

    /// Delivers due pending attempts: signs the payload, POSTs it, and records the
    /// result — advancing the retry schedule or marking `exhausted`, and
    /// auto-disabling an endpoint that has failed continuously for [`AUTO_DISABLE_DAYS`].
    /// Returns the number of attempts made.
    ///
    /// # Errors
    /// Propagates database failures (individual HTTP failures are recorded, not raised).
    pub async fn deliver_due_webhooks(
        &self,
        client: &reqwest::Client,
    ) -> Result<u64, NewsroomError> {
        let due: Vec<(Uuid, String, Uuid, i32)> = sqlx::query_as(
            "SELECT id, event_id, endpoint_id, attempt FROM meridian.webhook_deliveries \
             WHERE status = 'pending' AND next_attempt_at <= now() \
             ORDER BY next_attempt_at LIMIT 100",
        )
        .fetch_all(self.pool())
        .await?;

        let mut attempts = 0u64;
        for (delivery_id, event_id, endpoint_id, attempt) in due {
            let Some((url, secret, headers)) = self.endpoint_target(endpoint_id).await? else {
                // Endpoint gone/inactive: retire the delivery.
                self.mark_delivery(delivery_id, "failed", None, "endpoint inactive", None)
                    .await?;
                continue;
            };
            let Some((event_type, payload, subject, desk, created_at)) =
                self.event_row(&event_id).await?
            else {
                continue;
            };
            attempts += 1;

            let timestamp = OffsetDateTime::now_utc().unix_timestamp();
            let body = build_envelope(&event_id, &event_type, &subject, desk.as_deref(), created_at, &payload);
            let signature = sign(&secret, &event_id, timestamp, &body);

            let mut request = client
                .post(&url)
                .timeout(std::time::Duration::from_secs(DELIVERY_TIMEOUT_SECS))
                .header("content-type", "application/json")
                .header("webhook-id", &event_id)
                .header("webhook-timestamp", timestamp.to_string())
                .header("webhook-signature", signature)
                .header("user-agent", "Meridian-Webhooks/1.0");
            // Operator-defined custom headers (e.g. a receiver auth token). Reserved
            // signing/transport headers are protected from being overridden.
            if let Some(map) = headers.as_object() {
                for (name, value) in map {
                    let lower = name.to_ascii_lowercase();
                    if RESERVED_HEADERS.contains(&lower.as_str()) {
                        continue;
                    }
                    if let Some(v) = value.as_str() {
                        request = request.header(name, v);
                    }
                }
            }
            let result = request.body(body).send().await;

            let next = attempt + 1;
            match result {
                Ok(response) if response.status().is_success() => {
                    let code = i32::from(response.status().as_u16());
                    self.mark_delivery(delivery_id, "delivered", Some(code), "", Some(endpoint_id))
                        .await?;
                }
                Ok(response) => {
                    let code = i32::from(response.status().as_u16());
                    self.reschedule_or_exhaust(delivery_id, endpoint_id, next, Some(code), "non-2xx")
                        .await?;
                }
                Err(error) => {
                    self.reschedule_or_exhaust(
                        delivery_id,
                        endpoint_id,
                        next,
                        None,
                        &error.to_string(),
                    )
                    .await?;
                }
            }
        }
        Ok(attempts)
    }

    /// A live endpoint's URL + secret + custom headers, or `None` if inactive/deleted.
    async fn endpoint_target(
        &self,
        endpoint_id: Uuid,
    ) -> Result<Option<(String, String, serde_json::Value)>, NewsroomError> {
        Ok(sqlx::query_as(
            "SELECT url, secret, headers FROM meridian.webhook_endpoints WHERE id = $1 AND active",
        )
        .bind(endpoint_id)
        .fetch_optional(self.pool())
        .await?)
    }

    async fn event_row(
        &self,
        event_id: &str,
    ) -> Result<Option<(String, serde_json::Value, String, Option<String>, OffsetDateTime)>, NewsroomError>
    {
        Ok(sqlx::query_as(
            "SELECT event_type, payload, subject, desk, created_at \
             FROM meridian.webhook_events WHERE id = $1",
        )
        .bind(event_id)
        .fetch_optional(self.pool())
        .await?)
    }

    /// Records a terminal delivery outcome and, on success, clears the endpoint's
    /// failure streak.
    async fn mark_delivery(
        &self,
        delivery_id: Uuid,
        status: &str,
        code: Option<i32>,
        response: &str,
        clear_failure_for: Option<Uuid>,
    ) -> Result<(), NewsroomError> {
        sqlx::query(
            "UPDATE meridian.webhook_deliveries \
             SET status = $2, status_code = $3, response = $4, attempt = attempt + 1, \
                 delivered_at = CASE WHEN $2 = 'delivered' THEN now() ELSE delivered_at END, \
                 updated_at = now() WHERE id = $1",
        )
        .bind(delivery_id)
        .bind(status)
        .bind(code)
        .bind(response)
        .execute(self.pool())
        .await?;
        if let Some(endpoint_id) = clear_failure_for {
            sqlx::query("UPDATE meridian.webhook_endpoints SET failing_since = NULL WHERE id = $1")
                .bind(endpoint_id)
                .execute(self.pool())
                .await?;
        }
        Ok(())
    }

    /// Either schedules the next retry (advancing the Svix backoff) or, once the
    /// schedule is exhausted, marks the attempt `exhausted`; separately tracks the
    /// endpoint's failure streak and auto-disables after [`AUTO_DISABLE_DAYS`].
    async fn reschedule_or_exhaust(
        &self,
        delivery_id: Uuid,
        endpoint_id: Uuid,
        next_attempt: i32,
        code: Option<i32>,
        response: &str,
    ) -> Result<(), NewsroomError> {
        // Note the failure streak; auto-disable if it has run past the limit.
        sqlx::query(
            "UPDATE meridian.webhook_endpoints SET failing_since = COALESCE(failing_since, now()) \
             WHERE id = $1",
        )
        .bind(endpoint_id)
        .execute(self.pool())
        .await?;
        sqlx::query(
            "UPDATE meridian.webhook_endpoints SET active = false, disabled_at = now() \
             WHERE id = $1 AND failing_since < now() - make_interval(days => $2)",
        )
        .bind(endpoint_id)
        .bind(i32::try_from(AUTO_DISABLE_DAYS).unwrap_or(5))
        .execute(self.pool())
        .await?;

        let backoff = usize::try_from(next_attempt - 1)
            .ok()
            .and_then(|idx| RETRY_BACKOFF_SECS.get(idx).copied());
        match backoff {
            Some(secs) => {
                // Backoff values are all small (≤ 36 000s); an i32→f64 widening is exact.
                let secs = f64::from(i32::try_from(secs).unwrap_or(i32::MAX));
                sqlx::query(
                    "UPDATE meridian.webhook_deliveries \
                     SET attempt = $2, status_code = $3, response = $4, \
                         next_attempt_at = now() + make_interval(secs => $5::double precision), \
                         updated_at = now() WHERE id = $1",
                )
                .bind(delivery_id)
                .bind(next_attempt)
                .bind(code)
                .bind(response)
                .bind(secs)
                .execute(self.pool())
                .await?;
            }
            None => {
                self.mark_delivery(delivery_id, "exhausted", code, response, None)
                    .await?;
            }
        }
        Ok(())
    }
}

/// Builds the delivered JSON envelope (CloudEvents-named, Stripe-shaped `data`).
fn build_envelope(
    event_id: &str,
    event_type: &str,
    subject: &str,
    desk: Option<&str>,
    created_at: OffsetDateTime,
    payload: &serde_json::Value,
) -> String {
    let created = created_at
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default();
    serde_json::json!({
        "id": event_id,
        "type": event_type,
        "specversion": "1.0",
        "source": "/meridian",
        "subject": subject,
        "time": created,
        "desk": desk,
        "data": { "object": payload },
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::{event_matches, sign};

    #[test]
    fn wildcards_match_as_documented() {
        assert!(event_matches("*", "article.published"));
        assert!(event_matches("article.*", "article.published"));
        assert!(event_matches("article.*", "article.published.edited"));
        assert!(event_matches("*.published", "article.published"));
        assert!(event_matches("article.published", "article.published"));
        assert!(!event_matches("*.published", "release.channel_published"));
        assert!(!event_matches("article.*", "release.published"));
        assert!(!event_matches("task.created", "article.published"));
    }

    #[test]
    fn signature_is_stable_and_prefixed() {
        let s = sign("whsec_dGVzdHNlY3JldA==", "evt_1", 1_700_000_000, "{}");
        assert!(s.starts_with("v1,"));
        // Deterministic for the same inputs.
        assert_eq!(s, sign("whsec_dGVzdHNlY3JldA==", "evt_1", 1_700_000_000, "{}"));
    }
}
