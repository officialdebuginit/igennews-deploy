//! Per-sector SMTP (design: docs/MARKET-RESEARCH-AND-GAPS.md §7 follow-up).
//!
//! Each desk can carry its own outbound mail server, so a sector sends from and manages
//! its own email. Managed by a desk lead (`desks.manage`). Sending goes over async SMTP
//! (lettre + rustls). The password is never serialized back to a client.

use lettre::message::Mailbox;
use lettre::transport::smtp::authentication::Credentials;
use lettre::{AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::{Actor, NewsroomError, NewsroomService, authz};

/// A desk's SMTP configuration. `password` is write-only (never serialized out).
#[derive(Clone, Debug, Serialize, FromRow)]
pub struct DeskSmtpSettings {
    pub desk_id: Uuid,
    pub host: String,
    pub port: i32,
    pub username: Option<String>,
    #[serde(skip_serializing)]
    pub password: Option<String>,
    pub from_address: String,
    pub from_name: Option<String>,
    pub use_starttls: bool,
    pub active: bool,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
    /// True in the serialized view when a password is stored, without revealing it.
    #[sqlx(default)]
    #[serde(default)]
    pub has_password: bool,
}

/// Create/update input. An omitted `password` keeps the stored one.
#[derive(Clone, Debug, Deserialize)]
pub struct SmtpInput {
    pub host: String,
    #[serde(default = "default_port")]
    pub port: i32,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub password: Option<String>,
    pub from_address: String,
    #[serde(default)]
    pub from_name: Option<String>,
    #[serde(default = "yes")]
    pub use_starttls: bool,
}

const fn default_port() -> i32 {
    587
}
const fn yes() -> bool {
    true
}

const SMTP_COLUMNS: &str = "desk_id, host, port, username, password, from_address, from_name, \
    use_starttls, active, updated_at, (password IS NOT NULL) AS has_password";

impl NewsroomService {
    /// A desk's SMTP settings (password redacted), or `None` if unconfigured.
    ///
    /// # Errors
    /// [`NewsroomError::Forbidden`] without `desks.manage` on the desk; database failures.
    pub async fn get_desk_smtp(
        &self,
        actor: &Actor,
        desk_id: Uuid,
    ) -> Result<Option<DeskSmtpSettings>, NewsroomError> {
        authz::require(self.pool(), actor, "desks.manage", Some(desk_id)).await?;
        Ok(sqlx::query_as::<_, DeskSmtpSettings>(&format!(
            "SELECT {SMTP_COLUMNS} FROM meridian.desk_smtp_settings WHERE desk_id = $1"
        ))
        .bind(desk_id)
        .fetch_optional(self.pool())
        .await?)
    }

    /// Upserts a desk's SMTP settings. An omitted password keeps the stored one.
    ///
    /// # Errors
    /// [`NewsroomError::Forbidden`] without `desks.manage` on the desk; database failures.
    pub async fn set_desk_smtp(
        &self,
        actor: &Actor,
        desk_id: Uuid,
        input: &SmtpInput,
    ) -> Result<DeskSmtpSettings, NewsroomError> {
        authz::require(self.pool(), actor, "desks.manage", Some(desk_id)).await?;
        Ok(sqlx::query_as::<_, DeskSmtpSettings>(&format!(
            "INSERT INTO meridian.desk_smtp_settings \
               (desk_id, host, port, username, password, from_address, from_name, use_starttls, updated_by_id) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) \
             ON CONFLICT (desk_id) DO UPDATE SET \
               host = $2, port = $3, username = $4, \
               password = COALESCE($5, meridian.desk_smtp_settings.password), \
               from_address = $6, from_name = $7, use_starttls = $8, \
               updated_by_id = $9, updated_at = now() \
             RETURNING {SMTP_COLUMNS}"
        ))
        .bind(desk_id)
        .bind(&input.host)
        .bind(input.port)
        .bind(&input.username)
        .bind(input.password.as_deref().filter(|p| !p.is_empty()))
        .bind(&input.from_address)
        .bind(&input.from_name)
        .bind(input.use_starttls)
        .bind(actor.id)
        .fetch_one(self.pool())
        .await?)
    }

    /// Sends a test email through the desk's SMTP server. Proves the configuration
    /// works end to end.
    ///
    /// # Errors
    /// [`NewsroomError::Forbidden`] without `desks.manage`; [`NewsroomError::NotFound`]
    /// if unconfigured; [`NewsroomError::Unprocessable`] on an SMTP/build failure.
    pub async fn send_desk_test_email(
        &self,
        actor: &Actor,
        desk_id: Uuid,
        to: &str,
    ) -> Result<(), NewsroomError> {
        authz::require(self.pool(), actor, "desks.manage", Some(desk_id)).await?;
        let settings = self
            .get_desk_smtp(actor, desk_id)
            .await?
            .ok_or(NewsroomError::NotFound("SMTP settings"))?;
        // Re-read the password (redacted on the settings struct above).
        let password: Option<String> =
            sqlx::query_scalar("SELECT password FROM meridian.desk_smtp_settings WHERE desk_id = $1")
                .bind(desk_id)
                .fetch_one(self.pool())
                .await?;
        send_email(
            &settings,
            password.as_deref(),
            to,
            "Meridian SMTP test",
            "This is a test message confirming your sector's SMTP settings work.",
        )
        .await
    }
}

/// Sends one message through the given desk SMTP settings.
async fn send_email(
    settings: &DeskSmtpSettings,
    password: Option<&str>,
    to: &str,
    subject: &str,
    body: &str,
) -> Result<(), NewsroomError> {
    let from = match &settings.from_name {
        Some(name) if !name.trim().is_empty() => format!("{name} <{}>", settings.from_address),
        _ => settings.from_address.clone(),
    };
    let from: Mailbox = from
        .parse()
        .map_err(|e| NewsroomError::Unprocessable(format!("invalid from address: {e}")))?;
    let to: Mailbox = to
        .parse()
        .map_err(|e| NewsroomError::Unprocessable(format!("invalid recipient: {e}")))?;
    let message = Message::builder()
        .from(from)
        .to(to)
        .subject(subject)
        .body(body.to_owned())
        .map_err(|e| NewsroomError::Unprocessable(format!("could not build message: {e}")))?;

    let host = settings.host.trim();
    let mut builder = if settings.use_starttls {
        AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(host)
    } else {
        AsyncSmtpTransport::<Tokio1Executor>::relay(host)
    }
    .map_err(|e| NewsroomError::Unprocessable(format!("SMTP connect: {e}")))?;
    builder = builder.port(u16::try_from(settings.port).unwrap_or(587));
    if let (Some(user), Some(pass)) = (settings.username.as_deref(), password) {
        builder = builder.credentials(Credentials::new(user.to_owned(), pass.to_owned()));
    }
    builder
        .build()
        .send(message)
        .await
        .map_err(|e| NewsroomError::Unprocessable(format!("SMTP send failed: {e}")))?;
    Ok(())
}
