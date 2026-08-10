use std::fmt::Write as _;

use argon2::password_hash::SaltString;
use argon2::{Argon2, PasswordHash, PasswordHasher, PasswordVerifier};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header, Validation, decode, encode};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::{FromRow, PgPool};
use thiserror::Error;
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

#[derive(Clone, Debug, Deserialize, FromRow, PartialEq, Eq, Serialize)]
pub struct User {
    pub id: Uuid,
    pub email: String,
    pub handle: String,
    pub display_name: String,
    #[serde(skip_serializing)]
    pub password_hash: String,
    pub role: String,
    pub department: Option<String>,
    pub is_active: bool,
    pub is_admin: bool,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

#[derive(Clone, Debug, Deserialize, FromRow, PartialEq, Eq, Serialize)]
pub struct Session {
    pub id: Uuid,
    pub user_id: Uuid,
    pub family_id: Uuid,
    #[serde(skip_serializing)]
    pub refresh_token_hash: String,
    pub user_agent: Option<String>,
    pub ip_address: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub last_seen_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub expires_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339::option")]
    pub revoked_at: Option<OffsetDateTime>,
    pub revoked_reason: Option<String>,
}

impl Session {
    #[must_use]
    pub fn is_active(&self, now: OffsetDateTime) -> bool {
        self.revoked_at.is_none() && self.expires_at > now
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AccessClaims {
    pub sub: Uuid,
    pub role: String,
    pub sid: Uuid,
    pub jti: String,
    pub exp: i64,
}

#[derive(Debug, Error)]
pub enum IdentityError {
    #[error("invalid credentials")]
    InvalidCredentials,
    #[error("invalid or expired refresh token")]
    InvalidRefreshToken,
    #[error("refresh token reuse detected; session family revoked")]
    RefreshTokenReuse,
    #[error("invalid access token")]
    InvalidAccessToken,
    #[error("password hashing failed: {0}")]
    PasswordHash(String),
    #[error("database operation failed: {0}")]
    Database(#[from] sqlx::Error),
    #[error("token operation failed: {0}")]
    Jwt(#[from] jsonwebtoken::errors::Error),
}

#[derive(Clone)]
pub struct IdentityService {
    pool: PgPool,
    jwt_secret: String,
    access_minutes: i64,
    refresh_days: i64,
}

impl IdentityService {
    #[must_use]
    pub fn new(pool: PgPool, jwt_secret: String, access_minutes: i64, refresh_days: i64) -> Self {
        Self {
            pool,
            jwt_secret,
            access_minutes,
            refresh_days,
        }
    }

    /// Verifies an email or handle and password against an active user.
    ///
    /// # Errors
    ///
    /// Returns [`IdentityError::InvalidCredentials`] for unknown, inactive, or
    /// mismatched credentials and [`IdentityError::Database`] on query failure.
    pub async fn authenticate(&self, login: &str, password: &str) -> Result<User, IdentityError> {
        let user = sqlx::query_as::<_, User>(
            "SELECT id, email, handle, display_name, password_hash, role, department, is_active, is_admin, created_at \
             FROM meridian.users WHERE lower(email) = lower($1) OR lower(handle) = lower($1) LIMIT 1",
        )
        .bind(login)
        .fetch_optional(&self.pool)
        .await?;
        let Some(user) = user else {
            return Err(IdentityError::InvalidCredentials);
        };
        if !user.is_active || !verify_password(password, &user.password_hash) {
            return Err(IdentityError::InvalidCredentials);
        }
        Ok(user)
    }

    /// Creates a refresh-token session while persisting only the token hash.
    ///
    /// # Errors
    ///
    /// Returns [`IdentityError::Database`] if the session cannot be persisted.
    pub async fn create_session(
        &self,
        user: &User,
        user_agent: Option<&str>,
        ip_address: Option<std::net::IpAddr>,
        family_id: Option<Uuid>,
    ) -> Result<(Session, String), IdentityError> {
        let plaintext = opaque_token();
        let token_hash = hash_opaque_token(&plaintext);
        let now = OffsetDateTime::now_utc();
        let session = sqlx::query_as::<_, Session>(
            "INSERT INTO meridian.sessions \
             (id, user_id, family_id, refresh_token_hash, user_agent, ip_address, created_at, last_seen_at, expires_at) \
             VALUES ($1,$2,$3,$4,$5,$6::text::inet,$7,$7,$8) \
             RETURNING id,user_id,family_id,refresh_token_hash::text,user_agent,host(ip_address) AS ip_address,created_at,last_seen_at,expires_at,revoked_at,revoked_reason",
        )
        .bind(Uuid::now_v7())
        .bind(user.id)
        .bind(family_id.unwrap_or_else(Uuid::now_v7))
        .bind(token_hash)
        .bind(user_agent.map(|value| value.chars().take(400).collect::<String>()))
        .bind(ip_address.map(|value| value.to_string()))
        .bind(now)
        .bind(now + Duration::days(self.refresh_days))
        .fetch_one(&self.pool)
        .await?;
        Ok((session, plaintext))
    }

    /// Rotates a refresh token once and revokes its full family on replay.
    ///
    /// # Errors
    ///
    /// Returns a refresh-token error for invalid, expired, or replayed tokens,
    /// or [`IdentityError::Database`] if the transaction fails.
    pub async fn rotate_session(
        &self,
        refresh_token: &str,
    ) -> Result<(Session, String, User), IdentityError> {
        let mut transaction = self.pool.begin().await?;
        let token_hash = hash_opaque_token(refresh_token);
        let session = sqlx::query_as::<_, Session>(
            "SELECT id,user_id,family_id,refresh_token_hash::text,user_agent,host(ip_address) AS ip_address,created_at,last_seen_at,expires_at,revoked_at,revoked_reason \
             FROM meridian.sessions WHERE refresh_token_hash = $1 FOR UPDATE",
        )
        .bind(token_hash)
        .fetch_optional(&mut *transaction)
        .await?
        .ok_or(IdentityError::InvalidRefreshToken)?;
        if session.revoked_at.is_some() {
            sqlx::query("UPDATE meridian.sessions SET revoked_at=COALESCE(revoked_at,now()), revoked_reason=COALESCE(revoked_reason,'refresh token reuse detected') WHERE family_id=$1")
                .bind(session.family_id).execute(&mut *transaction).await?;
            transaction.commit().await?;
            return Err(IdentityError::RefreshTokenReuse);
        }
        if !session.is_active(OffsetDateTime::now_utc()) {
            sqlx::query("UPDATE meridian.sessions SET revoked_at=now(), revoked_reason='expired' WHERE id=$1")
                .bind(session.id).execute(&mut *transaction).await?;
            transaction.commit().await?;
            return Err(IdentityError::InvalidRefreshToken);
        }
        let user = sqlx::query_as::<_, User>(
            "SELECT id,email,handle,display_name,password_hash,role,department,is_active,is_admin,created_at FROM meridian.users WHERE id=$1",
        ).bind(session.user_id).fetch_optional(&mut *transaction).await?
            .filter(|user| user.is_active).ok_or(IdentityError::InvalidRefreshToken)?;
        sqlx::query(
            "UPDATE meridian.sessions SET revoked_at=now(), revoked_reason='rotated' WHERE id=$1",
        )
        .bind(session.id)
        .execute(&mut *transaction)
        .await?;
        let plaintext = opaque_token();
        let now = OffsetDateTime::now_utc();
        let replacement = sqlx::query_as::<_, Session>(
            "INSERT INTO meridian.sessions (id,user_id,family_id,refresh_token_hash,user_agent,ip_address,created_at,last_seen_at,expires_at) \
             VALUES ($1,$2,$3,$4,$5,$6::text::inet,$7,$7,$8) RETURNING id,user_id,family_id,refresh_token_hash::text,user_agent,host(ip_address) AS ip_address,created_at,last_seen_at,expires_at,revoked_at,revoked_reason",
        ).bind(Uuid::now_v7()).bind(user.id).bind(session.family_id).bind(hash_opaque_token(&plaintext))
            .bind(session.user_agent).bind(session.ip_address).bind(now).bind(now + Duration::days(self.refresh_days))
            .fetch_one(&mut *transaction).await?;
        transaction.commit().await?;
        Ok((replacement, plaintext, user))
    }

    /// Revokes a single session by id — the caller's own session on sign-out,
    /// identified from its access token. Idempotent: an already-revoked or
    /// unknown session is a no-op. Matches the legacy oracle, whose `POST
    /// /auth/logout` is bearer-authenticated, takes no body, and ends the
    /// session the access token belongs to.
    ///
    /// # Errors
    /// Returns [`IdentityError::Database`] on query failure.
    pub async fn logout(&self, user_id: Uuid, session_id: Uuid) -> Result<(), IdentityError> {
        sqlx::query(
            "UPDATE meridian.sessions SET revoked_at = COALESCE(revoked_at, now()), \
               revoked_reason = COALESCE(revoked_reason, 'logout') \
               WHERE id = $1 AND user_id = $2",
        )
        .bind(session_id)
        .bind(user_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Revokes every live session for a user (sign out everywhere). Returns how
    /// many were revoked.
    ///
    /// # Errors
    /// Returns [`IdentityError::Database`] on query failure.
    pub async fn logout_all(&self, user_id: Uuid) -> Result<u64, IdentityError> {
        let result = sqlx::query(
            "UPDATE meridian.sessions SET revoked_at = now(), revoked_reason = 'logout_all' \
             WHERE user_id = $1 AND revoked_at IS NULL",
        )
        .bind(user_id)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected())
    }

    /// The user's live sessions, most recently seen first.
    ///
    /// # Errors
    /// Returns [`IdentityError::Database`] on query failure.
    pub async fn list_sessions(&self, user_id: Uuid) -> Result<Vec<Session>, IdentityError> {
        Ok(sqlx::query_as::<_, Session>(
            "SELECT id,user_id,family_id,refresh_token_hash::text,user_agent,host(ip_address) AS ip_address,\
             created_at,last_seen_at,expires_at,revoked_at,revoked_reason \
             FROM meridian.sessions WHERE user_id = $1 AND revoked_at IS NULL AND expires_at > now() \
             ORDER BY last_seen_at DESC",
        )
        .bind(user_id)
        .fetch_all(&self.pool)
        .await?)
    }

    /// Revokes one of the user's sessions by id. Returns whether it changed.
    ///
    /// # Errors
    /// Returns [`IdentityError::Database`] on query failure.
    pub async fn revoke_session(
        &self,
        user_id: Uuid,
        session_id: Uuid,
    ) -> Result<bool, IdentityError> {
        let result = sqlx::query(
            "UPDATE meridian.sessions SET revoked_at = now(), revoked_reason = 'revoked' \
             WHERE id = $1 AND user_id = $2 AND revoked_at IS NULL",
        )
        .bind(session_id)
        .bind(user_id)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() > 0)
    }

    /// Changes a user's password after verifying the current one, then revokes
    /// every other session — a password change must not leave old logins alive.
    ///
    /// # Errors
    /// [`IdentityError::InvalidCredentials`] if the current password is wrong;
    /// database or hashing errors otherwise.
    pub async fn change_password(
        &self,
        user_id: Uuid,
        current: &str,
        new_password: &str,
        keep_session_id: Option<Uuid>,
    ) -> Result<(), IdentityError> {
        let hash: Option<String> =
            sqlx::query_scalar("SELECT password_hash FROM meridian.users WHERE id = $1")
                .bind(user_id)
                .fetch_optional(&self.pool)
                .await?;
        let hash = hash.ok_or(IdentityError::InvalidCredentials)?;
        if !verify_password(current, &hash) {
            return Err(IdentityError::InvalidCredentials);
        }
        let encoded = hash_password(new_password)?;
        let mut tx = self.pool.begin().await?;
        sqlx::query("UPDATE meridian.users SET password_hash = $2, updated_at = now() WHERE id = $1")
            .bind(user_id)
            .bind(&encoded)
            .execute(&mut *tx)
            .await?;
        // Revoke every *other* session, as the contract has always said. Revoking
        // the caller's own would sign them out of the change they just made — the
        // change_password self-logout bug.
        sqlx::query(
            "UPDATE meridian.sessions SET revoked_at = now(), revoked_reason = 'password_changed' \
             WHERE user_id = $1 AND revoked_at IS NULL AND ($2::uuid IS NULL OR id <> $2)",
        )
        .bind(user_id)
        .bind(keep_session_id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Directly sets a user's password without knowing the current one, for
    /// administrative account recovery. The caller **must** have authorized the
    /// actor as an administrator before calling. Every live session for the user
    /// is revoked, forcing a fresh sign-in with the new password.
    ///
    /// # Errors
    /// [`IdentityError::InvalidCredentials`] for an unknown user;
    /// [`IdentityError::PasswordHash`] if hashing fails; database failures.
    pub async fn admin_set_password(
        &self,
        user_id: Uuid,
        new_password: &str,
    ) -> Result<(), IdentityError> {
        let encoded = hash_password(new_password)?;
        let mut tx = self.pool.begin().await?;
        let updated =
            sqlx::query("UPDATE meridian.users SET password_hash = $2, updated_at = now() WHERE id = $1")
                .bind(user_id)
                .bind(&encoded)
                .execute(&mut *tx)
                .await?;
        if updated.rows_affected() == 0 {
            return Err(IdentityError::InvalidCredentials);
        }
        sqlx::query(
            "UPDATE meridian.sessions SET revoked_at = now(), revoked_reason = 'admin_password_reset' \
             WHERE user_id = $1 AND revoked_at IS NULL",
        )
        .bind(user_id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Issues a single-use password-reset token for an email, if it belongs to an
    /// active user. Returns the plaintext token for delivery; only its SHA-256
    /// hash is stored. Returns `None` for an unknown email so the caller cannot
    /// use this endpoint to enumerate accounts.
    ///
    /// # Errors
    /// Returns [`IdentityError::Database`] on query failure.
    pub async fn request_password_reset(
        &self,
        email: &str,
    ) -> Result<Option<String>, IdentityError> {
        let user_id: Option<Uuid> = sqlx::query_scalar(
            "SELECT id FROM meridian.users WHERE lower(email) = lower($1) AND is_active",
        )
        .bind(email)
        .fetch_optional(&self.pool)
        .await?;
        let Some(user_id) = user_id else {
            return Ok(None);
        };
        let plaintext = opaque_token();
        sqlx::query(
            "INSERT INTO meridian.password_reset_tokens (id, user_id, token_hash, expires_at) \
             VALUES ($1, $2, $3, now() + interval '1 hour')",
        )
        .bind(Uuid::now_v7())
        .bind(user_id)
        .bind(hash_opaque_token(&plaintext))
        .execute(&self.pool)
        .await?;
        Ok(Some(plaintext))
    }

    /// Consumes a password-reset token, sets the new password, and revokes every
    /// live session. The token is single-use: it is marked used in the same
    /// transaction.
    ///
    /// # Errors
    /// [`IdentityError::InvalidCredentials`] for an unknown, expired, or already
    /// used token; database or hashing errors otherwise.
    pub async fn confirm_password_reset(
        &self,
        token: &str,
        new_password: &str,
    ) -> Result<(), IdentityError> {
        let mut tx = self.pool.begin().await?;
        let user_id: Option<Uuid> = sqlx::query_scalar(
            "UPDATE meridian.password_reset_tokens SET used_at = now() \
             WHERE token_hash = $1 AND used_at IS NULL AND expires_at > now() \
             RETURNING user_id",
        )
        .bind(hash_opaque_token(token))
        .fetch_optional(&mut *tx)
        .await?;
        let user_id = user_id.ok_or(IdentityError::InvalidCredentials)?;
        let encoded = hash_password(new_password)?;
        sqlx::query("UPDATE meridian.users SET password_hash = $2, updated_at = now() WHERE id = $1")
            .bind(user_id)
            .bind(&encoded)
            .execute(&mut *tx)
            .await?;
        sqlx::query(
            "UPDATE meridian.sessions SET revoked_at = now(), revoked_reason = 'password_reset' \
             WHERE user_id = $1 AND revoked_at IS NULL",
        )
        .bind(user_id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Issues a legacy-compatible HS256 access token for a live session.
    ///
    /// # Errors
    ///
    /// Returns [`IdentityError::Jwt`] if claims cannot be encoded.
    pub fn issue_access_token(
        &self,
        user: &User,
        session_id: Uuid,
    ) -> Result<String, IdentityError> {
        let claims = AccessClaims {
            sub: user.id,
            role: user.role.clone(),
            sid: session_id,
            jti: opaque_token()[..22].to_owned(),
            exp: (OffsetDateTime::now_utc() + Duration::minutes(self.access_minutes))
                .unix_timestamp(),
        };
        Ok(encode(
            &Header::new(Algorithm::HS256),
            &claims,
            &EncodingKey::from_secret(self.jwt_secret.as_bytes()),
        )?)
    }

    /// Resolves a bearer token to an active user and live database session.
    ///
    /// # Errors
    ///
    /// Returns [`IdentityError::InvalidAccessToken`] when the claims or session
    /// are invalid, and a database or JWT error for infrastructure failures.
    pub async fn resolve_access_token(
        &self,
        token: &str,
    ) -> Result<(User, Session), IdentityError> {
        let claims = decode::<AccessClaims>(
            token,
            &DecodingKey::from_secret(self.jwt_secret.as_bytes()),
            &Validation::new(Algorithm::HS256),
        )?
        .claims;
        let mut transaction = self.pool.begin().await?;
        let session = sqlx::query_as::<_, Session>(
            "UPDATE meridian.sessions SET last_seen_at=now() WHERE id=$1 AND user_id=$2 AND revoked_at IS NULL AND expires_at>now() \
             RETURNING id,user_id,family_id,refresh_token_hash::text,user_agent,host(ip_address) AS ip_address,created_at,last_seen_at,expires_at,revoked_at,revoked_reason",
        ).bind(claims.sid).bind(claims.sub).fetch_optional(&mut *transaction).await?
            .ok_or(IdentityError::InvalidAccessToken)?;
        let user = sqlx::query_as::<_, User>(
            "SELECT id,email,handle,display_name,password_hash,role,department,is_active,is_admin,created_at FROM meridian.users WHERE id=$1 AND is_active",
        ).bind(claims.sub).fetch_optional(&mut *transaction).await?
            .ok_or(IdentityError::InvalidAccessToken)?;
        transaction.commit().await?;
        Ok((user, session))
    }
}

/// Hashes a password with Argon2id and a fresh random salt, returning a PHC
/// string compatible with [`verify_password`] and the legacy verifier.
///
/// # Errors
///
/// Returns [`IdentityError::PasswordHash`] if hashing fails.
pub fn hash_password(password: &str) -> Result<String, IdentityError> {
    let mut salt_bytes = [0_u8; 16];
    rand::rng().fill_bytes(&mut salt_bytes);
    let salt = SaltString::encode_b64(&salt_bytes)
        .map_err(|error| IdentityError::PasswordHash(error.to_string()))?;
    let hash = Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map_err(|error| IdentityError::PasswordHash(error.to_string()))?;
    Ok(hash.to_string())
}

#[must_use]
pub fn verify_password(password: &str, encoded: &str) -> bool {
    PasswordHash::new(encoded).is_ok_and(|hash| {
        Argon2::default()
            .verify_password(password.as_bytes(), &hash)
            .is_ok()
    })
}

#[must_use]
pub fn opaque_token() -> String {
    let mut bytes = [0_u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

#[must_use]
pub fn hash_opaque_token(token: &str) -> String {
    Sha256::digest(token.as_bytes())
        .iter()
        .fold(String::with_capacity(64), |mut output, byte| {
            write!(output, "{byte:02x}").expect("writing to a String cannot fail");
            output
        })
}

#[cfg(test)]
mod tests {
    use super::{hash_opaque_token, opaque_token, verify_password};

    #[test]
    fn opaque_tokens_have_256_bits_and_stable_sha256_hashes() {
        let token = opaque_token();
        assert_eq!(token.len(), 43);
        assert_eq!(hash_opaque_token(&token).len(), 64);
        assert_ne!(token, opaque_token());
    }

    #[test]
    fn hashed_password_round_trips_and_salts_differ() {
        let first = super::hash_password("correct horse battery staple").expect("hash");
        let second = super::hash_password("correct horse battery staple").expect("hash");
        assert_ne!(first, second, "each hash uses a fresh salt");
        assert!(verify_password("correct horse battery staple", &first));
        assert!(!verify_password("wrong", &first));
    }

    #[test]
    fn verifies_legacy_argon2id_phc_hashes() {
        let encoded = "$argon2id$v=19$m=65536,t=3,p=4$27YwJ9RITShevHhOYMh7Jg$oQSVNiU9aW0dEsAdXyT5Zih+0tIU3sPSNu/bWDARMzQ";
        assert!(verify_password("legacy-contract-password", encoded));
        assert!(!verify_password("wrong", encoded));
    }
}
