use std::{env, str::FromStr, sync::Arc, time::Duration};

use aws_config::BehaviorVersion;
use aws_credential_types::Credentials;
use aws_sdk_s3::{Client as S3Client, presigning::PresigningConfig, primitives::ByteStream};
use aws_types::region::Region;
use meridian_contracts::{DependencyHealth, ReadinessResponse};
use sqlx::{
    PgPool,
    postgres::{PgConnectOptions, PgListener, PgPoolOptions},
};
use thiserror::Error;

#[derive(Clone, Debug)]
pub struct PlatformConfig {
    pub database_url: String,
    pub database_direct_url: String,
    pub s3_endpoint_url: String,
    pub s3_bucket: String,
    pub s3_access_key_id: String,
    pub s3_secret_access_key: String,
    pub s3_region: String,
}

#[derive(Debug, Error)]
pub enum PlatformError {
    #[error("required environment variable {0} is missing")]
    MissingVariable(&'static str),
    #[error("{name} must use PostgreSQL; SQLite and other database engines are unsupported")]
    InvalidDatabaseScheme { name: &'static str },
    #[error("DATABASE_URL and DATABASE_DIRECT_URL must be different endpoints")]
    DatabaseEndpointsNotSeparated,
    #[error("database initialization failed: {0}")]
    Database(#[from] sqlx::Error),
    #[error("database migration failed: {0}")]
    Migration(#[from] sqlx::migrate::MigrateError),
    #[error("object storage probe failed: {0}")]
    Storage(String),
    #[error("direct PostgreSQL notification probe timed out")]
    NotificationTimeout,
}

impl PlatformConfig {
    /// Loads the platform contract from process environment variables.
    ///
    /// # Errors
    ///
    /// Returns [`PlatformError`] when a required variable is missing, a database
    /// URL is not `PostgreSQL`, or the pooled and direct endpoints are identical.
    pub fn from_env() -> Result<Self, PlatformError> {
        let _ = dotenvy::dotenv();
        let config = Self {
            database_url: required("DATABASE_URL")?,
            database_direct_url: required("DATABASE_DIRECT_URL")?,
            s3_endpoint_url: required("S3_ENDPOINT_URL")?,
            s3_bucket: required("S3_BUCKET")?,
            s3_access_key_id: required("S3_ACCESS_KEY_ID")?,
            s3_secret_access_key: required("S3_SECRET_ACCESS_KEY")?,
            s3_region: env::var("S3_REGION").unwrap_or_else(|_| "us-east-1".to_owned()),
        };
        config.validate()?;
        Ok(config)
    }

    /// Validates the PostgreSQL-only and split-connection invariants.
    ///
    /// # Errors
    ///
    /// Returns [`PlatformError`] when either URL is not `PostgreSQL` or when both
    /// roles point at the same endpoint authority.
    pub fn validate(&self) -> Result<(), PlatformError> {
        validate_postgres_url("DATABASE_URL", &self.database_url)?;
        validate_postgres_url("DATABASE_DIRECT_URL", &self.database_direct_url)?;
        if endpoint_authority(&self.database_url) == endpoint_authority(&self.database_direct_url) {
            return Err(PlatformError::DatabaseEndpointsNotSeparated);
        }
        Ok(())
    }
}

fn required(name: &'static str) -> Result<String, PlatformError> {
    env::var(name).map_err(|_| PlatformError::MissingVariable(name))
}

fn validate_postgres_url(name: &'static str, value: &str) -> Result<(), PlatformError> {
    if value.starts_with("postgres://") || value.starts_with("postgresql://") {
        Ok(())
    } else {
        Err(PlatformError::InvalidDatabaseScheme { name })
    }
}

fn endpoint_authority(url: &str) -> &str {
    url.split_once("//")
        .map_or(url, |(_, rest)| rest)
        .split('/')
        .next()
        .unwrap_or(url)
}

#[derive(Clone)]
pub struct Platform {
    inner: Arc<PlatformInner>,
}

struct PlatformInner {
    pooled_database: PgPool,
    direct_database: PgPool,
    s3: S3Client,
    bucket: String,
}

impl Platform {
    #[must_use]
    pub fn pooled_database(&self) -> PgPool {
        self.inner.pooled_database.clone()
    }

    /// Connects to `PgBouncer` and direct `PostgreSQL` and constructs the
    /// `RustFS` client. Database migrations are intentionally not run by the web
    /// process; use the dedicated `meridian-migrate` binary.
    ///
    /// # Errors
    ///
    /// Returns [`PlatformError`] if a database URL cannot be parsed, a database
    /// connection cannot be established, or object-storage configuration fails.
    pub async fn initialize(config: PlatformConfig) -> Result<Self, PlatformError> {
        // PgBouncer runs in transaction mode. A client-side prepared-statement
        // cache assumes session affinity, so the pooled connection disables it.
        let pooled_options =
            PgConnectOptions::from_str(&config.database_url)?.statement_cache_capacity(0);
        let pooled_database = PgPoolOptions::new()
            .max_connections(12)
            .acquire_timeout(Duration::from_secs(5))
            .connect_with(pooled_options)
            .await?;
        let direct_options = PgConnectOptions::from_str(&config.database_direct_url)?;
        let direct_database = PgPoolOptions::new()
            .max_connections(2)
            .acquire_timeout(Duration::from_secs(5))
            .connect_with(direct_options)
            .await?;

        let credentials = Credentials::new(
            config.s3_access_key_id,
            config.s3_secret_access_key,
            None,
            None,
            "meridian-environment",
        );
        let shared_config = aws_config::defaults(BehaviorVersion::latest())
            .credentials_provider(credentials)
            .region(Region::new(config.s3_region))
            .endpoint_url(config.s3_endpoint_url)
            .load()
            .await;
        let s3_config = aws_sdk_s3::config::Builder::from(&shared_config)
            .force_path_style(true)
            .build();

        Ok(Self {
            inner: Arc::new(PlatformInner {
                pooled_database,
                direct_database,
                s3: S3Client::from_conf(s3_config),
                bucket: config.s3_bucket,
            }),
        })
    }

    pub async fn readiness(&self) -> ReadinessResponse {
        let pooled_database = sqlx::query_scalar::<_, i32>("SELECT 1")
            .fetch_one(&self.inner.pooled_database)
            .await
            .is_ok();
        let direct_database = sqlx::query_scalar::<_, i32>("SELECT 1")
            .fetch_one(&self.inner.direct_database)
            .await
            .is_ok();
        let object_storage = self
            .inner
            .s3
            .head_bucket()
            .bucket(&self.inner.bucket)
            .send()
            .await
            .is_ok();
        let ready = pooled_database && direct_database && object_storage;

        ReadinessResponse {
            status: if ready { "ready" } else { "not_ready" },
            dependencies: DependencyHealth {
                pooled_database,
                direct_database,
                object_storage,
            },
        }
    }

    /// Writes, reads, presigns, and deletes a uniquely named object in `RustFS`.
    /// Cleanup is attempted even when a read or presign assertion fails.
    ///
    /// # Errors
    ///
    /// Returns [`PlatformError::Storage`] when any S3 operation fails or the
    /// downloaded payload differs from the uploaded payload.
    pub async fn storage_round_trip(&self) -> Result<(), PlatformError> {
        let payload = b"meridian-module-1-storage-probe";
        let key = format!("module-1-probes/{}.txt", uuid::Uuid::now_v7());

        self.inner
            .s3
            .put_object()
            .bucket(&self.inner.bucket)
            .key(&key)
            .content_type("text/plain")
            .body(ByteStream::from_static(payload))
            .send()
            .await
            .map_err(storage_error)?;

        let probe_result = async {
            self.inner
                .s3
                .head_object()
                .bucket(&self.inner.bucket)
                .key(&key)
                .send()
                .await
                .map_err(storage_error)?;

            let object = self
                .inner
                .s3
                .get_object()
                .bucket(&self.inner.bucket)
                .key(&key)
                .send()
                .await
                .map_err(storage_error)?;
            let downloaded = object.body.collect().await.map_err(storage_error)?;
            if downloaded.into_bytes().as_ref() != payload {
                return Err(PlatformError::Storage(
                    "downloaded object did not match uploaded payload".to_owned(),
                ));
            }

            let presign =
                PresigningConfig::expires_in(Duration::from_mins(1)).map_err(storage_error)?;
            let request = self
                .inner
                .s3
                .get_object()
                .bucket(&self.inner.bucket)
                .key(&key)
                .presigned(presign)
                .await
                .map_err(storage_error)?;
            if !request.uri().contains("X-Amz-Signature=") {
                return Err(PlatformError::Storage(
                    "presigned request did not include a signature query".to_owned(),
                ));
            }
            Ok(())
        }
        .await;

        let cleanup_result = self
            .inner
            .s3
            .delete_object()
            .bucket(&self.inner.bucket)
            .key(&key)
            .send()
            .await
            .map_err(storage_error);

        probe_result.and(cleanup_result.map(|_| ()))
    }

    /// Proves that session-scoped notifications work over the direct connection.
    ///
    /// # Errors
    ///
    /// Returns a database error when `LISTEN` or `NOTIFY` fails and
    /// [`PlatformError::NotificationTimeout`] when no matching event arrives.
    pub async fn notification_round_trip(&self) -> Result<(), PlatformError> {
        let channel = "meridian_module_1_probe";
        let payload = uuid::Uuid::now_v7().to_string();
        let mut listener = PgListener::connect_with(&self.inner.direct_database).await?;
        listener.listen(channel).await?;

        sqlx::query("SELECT pg_notify($1, $2)")
            .bind(channel)
            .bind(&payload)
            .execute(&self.inner.direct_database)
            .await?;

        let notification = tokio::time::timeout(Duration::from_secs(5), listener.recv())
            .await
            .map_err(|_| PlatformError::NotificationTimeout)??;
        if notification.payload() != payload {
            return Err(PlatformError::Database(sqlx::Error::Protocol(
                "notification payload mismatch".to_owned(),
            )));
        }
        Ok(())
    }
}

/// Runs `SQLx` migrations through a dedicated direct `PostgreSQL` credential.
/// The single connection is pinned to the `meridian` schema so `SQLx`'s ledger
/// and all unqualified migration objects stay outside `public`.
///
/// # Errors
///
/// Returns [`PlatformError`] when the URL is not `PostgreSQL`, the connection
/// fails, the required schema is unavailable, or a migration fails.
pub async fn run_migrations(database_url: &str) -> Result<(), PlatformError> {
    validate_postgres_url("MIGRATION_DATABASE_URL", database_url)?;
    let options = PgConnectOptions::from_str(database_url)?;
    let database = PgPoolOptions::new()
        .max_connections(1)
        .acquire_timeout(Duration::from_secs(5))
        .connect_with(options)
        .await?;

    sqlx::query("SET search_path TO meridian")
        .execute(&database)
        .await?;
    sqlx::migrate!("../../migrations").run(&database).await?;
    sqlx::query(
        "REVOKE ALL ON TABLE meridian._sqlx_migrations \
         FROM meridian_app, meridian_worker, meridian_readonly",
    )
    .execute(&database)
    .await?;
    database.close().await;
    Ok(())
}

fn storage_error(error: impl std::fmt::Display) -> PlatformError {
    PlatformError::Storage(error.to_string())
}

/// The owned-media object store, backed by a `RustFS`/S3 bucket configured
/// through the `MEDIA_S3_*` environment variables. Separate from the platform
/// probe client so media has its own bucket and credentials.
#[derive(Clone)]
pub struct MediaStore {
    s3: S3Client,
    bucket: String,
}

impl MediaStore {
    /// Builds the media store from `MEDIA_S3_ENDPOINT`, `MEDIA_S3_BUCKET`,
    /// `MEDIA_S3_ACCESS_KEY`, `MEDIA_S3_SECRET_KEY`, and `MEDIA_S3_REGION`.
    ///
    /// # Errors
    ///
    /// Returns [`PlatformError::MissingVariable`] when a required variable is
    /// unset.
    pub async fn from_env() -> Result<Self, PlatformError> {
        let endpoint = required("MEDIA_S3_ENDPOINT")?;
        let bucket = required("MEDIA_S3_BUCKET")?;
        let access_key = required("MEDIA_S3_ACCESS_KEY")?;
        let secret_key = required("MEDIA_S3_SECRET_KEY")?;
        let region = env::var("MEDIA_S3_REGION").unwrap_or_else(|_| "us-east-1".to_owned());

        let credentials = Credentials::new(access_key, secret_key, None, None, "meridian-media");
        let shared_config = aws_config::defaults(BehaviorVersion::latest())
            .credentials_provider(credentials)
            .region(Region::new(region))
            .endpoint_url(endpoint)
            .load()
            .await;
        let s3_config = aws_sdk_s3::config::Builder::from(&shared_config)
            .force_path_style(true)
            .build();
        Ok(Self {
            s3: S3Client::from_conf(s3_config),
            bucket,
        })
    }

    #[must_use]
    pub fn bucket(&self) -> &str {
        &self.bucket
    }

    /// Whether the media bucket is reachable.
    pub async fn is_ready(&self) -> bool {
        self.s3
            .head_bucket()
            .bucket(&self.bucket)
            .send()
            .await
            .is_ok()
    }

    /// Uploads bytes under `key` with the given content type.
    ///
    /// # Errors
    ///
    /// Returns [`PlatformError::Storage`] if the put fails.
    pub async fn upload(
        &self,
        key: &str,
        content_type: &str,
        bytes: Vec<u8>,
    ) -> Result<(), PlatformError> {
        self.s3
            .put_object()
            .bucket(&self.bucket)
            .key(key)
            .content_type(content_type)
            .body(ByteStream::from(bytes))
            .send()
            .await
            .map_err(storage_error)?;
        Ok(())
    }

    /// Returns a presigned GET URL for `key`, valid for `expires`.
    ///
    /// # Errors
    ///
    /// Returns [`PlatformError::Storage`] if presigning fails.
    pub async fn presign_get(
        &self,
        key: &str,
        expires: Duration,
    ) -> Result<String, PlatformError> {
        let config = PresigningConfig::expires_in(expires).map_err(storage_error)?;
        let request = self
            .s3
            .get_object()
            .bucket(&self.bucket)
            .key(key)
            .presigned(config)
            .await
            .map_err(storage_error)?;
        Ok(request.uri().to_string())
    }

    /// Fetches an object's raw bytes and content type. Used to stream media back
    /// through the application's own origin, so a strict `img-src 'self'` CSP can
    /// display it without whitelisting the object store's host.
    ///
    /// # Errors
    ///
    /// Returns [`PlatformError::Storage`] if the object is missing or the read fails.
    pub async fn get_object(
        &self,
        key: &str,
    ) -> Result<(Vec<u8>, Option<String>), PlatformError> {
        let output = self
            .s3
            .get_object()
            .bucket(&self.bucket)
            .key(key)
            .send()
            .await
            .map_err(storage_error)?;
        let content_type = output.content_type().map(ToOwned::to_owned);
        let bytes = output
            .body
            .collect()
            .await
            .map_err(storage_error)?
            .into_bytes()
            .to_vec();
        Ok((bytes, content_type))
    }

    /// Fetches a byte range of an object (S3 `Range` GET), for HTTP 206 media
    /// streaming so a `<video>`/`<audio>` element can seek. `range` is a raw HTTP
    /// `Range` value such as `bytes=0-1048575`. Returns the partial bytes, the
    /// content type, the `Content-Range` value the store computed, and the
    /// object's total size (parsed from that header).
    ///
    /// # Errors
    ///
    /// Returns [`PlatformError::Storage`] if the object is missing or the read fails.
    pub async fn get_object_range(
        &self,
        key: &str,
        range: &str,
    ) -> Result<(Vec<u8>, Option<String>, Option<String>, Option<i64>), PlatformError> {
        let output = self
            .s3
            .get_object()
            .bucket(&self.bucket)
            .key(key)
            .range(range)
            .send()
            .await
            .map_err(storage_error)?;
        let content_type = output.content_type().map(ToOwned::to_owned);
        let content_range = output.content_range().map(ToOwned::to_owned);
        let total = content_range
            .as_ref()
            .and_then(|cr| cr.rsplit('/').next())
            .and_then(|total| total.trim().parse::<i64>().ok());
        let bytes = output
            .body
            .collect()
            .await
            .map_err(storage_error)?
            .into_bytes()
            .to_vec();
        Ok((bytes, content_type, content_range, total))
    }

    /// Lists every object key under `prefix`, following pagination.
    ///
    /// # Errors
    ///
    /// Returns [`PlatformError::Storage`] if any list page fails.
    pub async fn list_objects(&self, prefix: &str) -> Result<Vec<String>, PlatformError> {
        let mut keys = Vec::new();
        let mut continuation: Option<String> = None;
        loop {
            let mut request = self
                .s3
                .list_objects_v2()
                .bucket(&self.bucket)
                .prefix(prefix);
            if let Some(token) = &continuation {
                request = request.continuation_token(token);
            }
            let page = request.send().await.map_err(storage_error)?;
            for object in page.contents() {
                if let Some(key) = object.key() {
                    keys.push(key.to_owned());
                }
            }
            if page.is_truncated().unwrap_or(false) {
                continuation = page.next_continuation_token().map(ToOwned::to_owned);
                if continuation.is_none() {
                    break;
                }
            } else {
                break;
            }
        }
        Ok(keys)
    }

    /// Deletes the object at `key`.
    ///
    /// # Errors
    ///
    /// Returns [`PlatformError::Storage`] if the delete fails.
    pub async fn delete(&self, key: &str) -> Result<(), PlatformError> {
        self.s3
            .delete_object()
            .bucket(&self.bucket)
            .key(key)
            .send()
            .await
            .map_err(storage_error)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{PlatformConfig, PlatformError, run_migrations};

    fn valid_config() -> PlatformConfig {
        PlatformConfig {
            database_url: "postgresql://app:secret@localhost:6433/meridian".to_owned(),
            database_direct_url: "postgresql://app:secret@localhost:5433/meridian".to_owned(),
            s3_endpoint_url: "http://localhost:9000".to_owned(),
            s3_bucket: "meridian-media".to_owned(),
            s3_access_key_id: "access".to_owned(),
            s3_secret_access_key: "secret".to_owned(),
            s3_region: "us-east-1".to_owned(),
        }
    }

    #[test]
    fn rejects_sqlite() {
        let mut config = valid_config();
        config.database_url = "sqlite:///meridian.db".to_owned();
        assert!(matches!(
            config.validate(),
            Err(PlatformError::InvalidDatabaseScheme { .. })
        ));
    }

    #[test]
    fn requires_separate_pooler_and_direct_endpoints() {
        let mut config = valid_config();
        config.database_direct_url = config.database_url.clone();
        assert!(matches!(
            config.validate(),
            Err(PlatformError::DatabaseEndpointsNotSeparated)
        ));
    }

    #[tokio::test]
    async fn migration_runner_rejects_sqlite_before_connecting() {
        assert!(matches!(
            run_migrations("sqlite:///meridian.db").await,
            Err(PlatformError::InvalidDatabaseScheme {
                name: "MIGRATION_DATABASE_URL"
            })
        ));
    }
}
