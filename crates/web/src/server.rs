use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

use axum::{
    Extension, Form, Json, Router,
    body::Body,
    extract::{FromRequest, Request, rejection::JsonRejection},
    http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use meridian_contracts::{
    ApiError, BuildInfo, HealthResponse, RefreshRequest, TokenResponse,
};
use meridian_identity::{IdentityError, IdentityService};
use meridian_newsroom::NewsroomService;
use meridian_platform::{MediaStore, Platform, PlatformConfig};
use tower::ServiceBuilder;
use tower_http::{
    catch_panic::CatchPanicLayer,
    request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer},
    trace::TraceLayer,
};
use tracing_subscriber::EnvFilter;

use super::App;

#[derive(Clone, Default)]
struct AppMetrics {
    requests: Arc<AtomicU64>,
    server_errors: Arc<AtomicU64>,
    readiness_failures: Arc<AtomicU64>,
}

/// In-memory per-IP throttle for the sign-in endpoint. There was no brute-force
/// protection at all; ten failed attempts from one address inside the window earn
/// a `429` until it lapses. Only *failures* count and a success clears the counter,
/// so a legitimate user who fumbles a password is not locked out. Keyed on the
/// forwarded client IP (see `client_ip`), so proxy-less local traffic — including
/// the test suites — is never throttled.
#[derive(Clone, Default)]
struct LoginThrottle {
    inner: Arc<std::sync::Mutex<std::collections::HashMap<std::net::IpAddr, (std::time::Instant, u32)>>>,
}

impl LoginThrottle {
    const LIMIT: u32 = 10;
    const WINDOW_SECS: u64 = 300;

    fn guard(
        &self,
    ) -> std::sync::MutexGuard<'_, std::collections::HashMap<std::net::IpAddr, (std::time::Instant, u32)>>
    {
        self.inner.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn is_blocked(&self, ip: std::net::IpAddr) -> bool {
        matches!(self.guard().get(&ip), Some((start, count)) if start.elapsed().as_secs() < Self::WINDOW_SECS && *count >= Self::LIMIT)
    }

    fn record_failure(&self, ip: std::net::IpAddr) {
        let mut guard = self.guard();
        let entry = guard.entry(ip).or_insert((std::time::Instant::now(), 0));
        if entry.0.elapsed().as_secs() >= Self::WINDOW_SECS {
            *entry = (std::time::Instant::now(), 0);
        }
        entry.1 += 1;
    }

    fn clear(&self, ip: std::net::IpAddr) {
        self.guard().remove(&ip);
    }
}

pub fn start() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .json()
        .init();

    dioxus::serve(|| async move {
        let config = PlatformConfig::from_env()?;
        let platform = Platform::initialize(config).await?;
        let auth_secret = std::env::var("MERIDIAN_AUTH_SECRET")
            .map_err(|_| anyhow::anyhow!("MERIDIAN_AUTH_SECRET is required"))?;
        if auth_secret.len() < 64 {
            return Err(anyhow::anyhow!(
                "MERIDIAN_AUTH_SECRET must contain at least 64 characters"
            ));
        }
        let identity = IdentityService::new(platform.pooled_database(), auth_secret, 15, 30);
        let newsroom = NewsroomService::new(platform.pooled_database());
        // Project the capability registry into `role_definitions` so the stored
        // system roles match the code. Idempotent — it only writes rows whose
        // capabilities or scope actually changed — and it is what makes resolver
        // step 5 (role assignments, which join `role_definitions`) able to match
        // at all. Startup fails loudly rather than serving a stale registry.
        let synced = newsroom.sync_system_roles().await?;
        if synced > 0 {
            tracing::info!(roles = synced, "synchronised system role definitions");
        }

        // Background release scheduler: without this, a scheduled release never
        // publishes on its own and an expired one never comes down — the sweep only
        // ran when something POSTed `/releases/process-due`. This fires it on an
        // interval as a system actor. Disable with `MERIDIAN_DISABLE_SCHEDULER=1`;
        // tune with `MERIDIAN_SCHEDULER_INTERVAL_SECS` (default 30).
        if std::env::var("MERIDIAN_DISABLE_SCHEDULER").is_err() {
            let sched = newsroom.clone();
            let interval_secs: u64 = std::env::var("MERIDIAN_SCHEDULER_INTERVAL_SECS")
                .ok()
                .and_then(|v| v.parse().ok())
                .filter(|&s| s >= 5)
                .unwrap_or(30);
            tokio::spawn(async move {
                let actor = match sched.system_actor().await {
                    Ok(Some(actor)) => actor,
                    Ok(None) => {
                        tracing::warn!("scheduler: no admin user; scheduled releases will not auto-fire");
                        return;
                    }
                    Err(error) => {
                        tracing::error!(%error, "scheduler: could not resolve a system actor");
                        return;
                    }
                };
                let mut ticker =
                    tokio::time::interval(std::time::Duration::from_secs(interval_secs));
                ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                tracing::info!(interval_secs, "release scheduler started");
                loop {
                    ticker.tick().await;
                    // `worker` is the only trigger the release_attempts check
                    // constraint accepts for an automated background pass.
                    match sched.process_due_releases(&actor, "worker").await {
                        Ok(published) if !published.is_empty() => {
                            tracing::info!(count = published.len(), "scheduler: published due releases");
                        }
                        Ok(_) => {}
                        Err(error) => tracing::warn!(%error, "scheduler: pass failed"),
                    }
                }
            });
        }

        // Background webhook dispatcher: fans newly-enqueued domain events out to the
        // subscriptions that match, then delivers them over HTTP with the Svix retry
        // schedule. Disable with `MERIDIAN_DISABLE_WEBHOOKS=1`.
        if std::env::var("MERIDIAN_DISABLE_WEBHOOKS").is_err() {
            let hooks = newsroom.clone();
            tokio::spawn(async move {
                let client = reqwest::Client::new();
                let mut ticker = tokio::time::interval(std::time::Duration::from_secs(5));
                ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                tracing::info!("webhook dispatcher started");
                loop {
                    ticker.tick().await;
                    if let Err(error) = hooks.fan_out_events().await {
                        tracing::warn!(%error, "webhooks: fan-out failed");
                    }
                    if let Err(error) = hooks.deliver_due_webhooks(&client).await {
                        tracing::warn!(%error, "webhooks: delivery pass failed");
                    }
                }
            });
        }

        let media = MediaStore::from_env().await?;
        let realtime = crate::realtime::RealtimeHub::new();
        let metrics = AppMetrics::default();
        let operations = operations_router(&platform, &metrics);
        let request_id = HeaderName::from_static("x-request-id");
        let middleware = ServiceBuilder::new()
            .layer(SetRequestIdLayer::new(request_id.clone(), MakeRequestUuid))
            .layer(PropagateRequestIdLayer::new(request_id))
            .layer(TraceLayer::new_for_http())
            .layer(Extension(metrics))
            .layer(Extension(LoginThrottle::default()))
            .layer(Extension(identity))
            .layer(Extension(newsroom))
            .layer(Extension(media))
            .layer(Extension(realtime))
            .layer(middleware::from_fn(add_security_headers))
            .layer(middleware::from_fn(record_request));

        Ok(dioxus::server::router(App)
            .merge(operations)
            .merge(crate::newsroom_routes::router())
            .merge(crate::story_routes::router())
            .merge(crate::awareness_routes::router())
            .merge(crate::admin_routes::router())
            .merge(crate::webhook_routes::router())
            .merge(crate::subscription_routes::router())
            .layer(CatchPanicLayer::new())
            .layer(middleware))
    });
}

async fn add_security_headers(request: axum::http::Request<Body>, next: Next) -> Response {
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        HeaderName::from_static("referrer-policy"),
        HeaderValue::from_static("strict-origin-when-cross-origin"),
    );
    headers.insert(
        HeaderName::from_static("permissions-policy"),
        HeaderValue::from_static("camera=(), microphone=(), geolocation=()"),
    );
    headers.insert(
        HeaderName::from_static("content-security-policy"),
        HeaderValue::from_static(
            "default-src 'self'; script-src 'self' 'unsafe-inline' 'unsafe-eval'; \
             style-src 'self' 'unsafe-inline' https://fonts.googleapis.com; \
             font-src 'self' https://fonts.gstatic.com data:; \
             connect-src 'self' ws: wss:; img-src 'self' data:; object-src 'none'; \
             base-uri 'self'; frame-ancestors 'none'; form-action 'self'",
        ),
    );
    response
}

fn operations_router(platform: &Platform, metrics: &AppMetrics) -> Router {
    let readiness_platform = platform.clone();
    let readiness_metrics = metrics.clone();

    Router::new()
        .route(
            "/health",
            get(|| async {
                Json(HealthResponse {
                    status: "ok",
                    service: "meridian-newsroom-api",
                })
            }),
        )
        .route(
            "/health/live",
            get(|| async {
                Json(HealthResponse {
                    status: "ok",
                    service: "meridian-rust",
                })
            }),
        )
        .route(
            "/health/ready",
            get(move || {
                let platform = readiness_platform.clone();
                let metrics = readiness_metrics.clone();
                async move {
                    let response = platform.readiness().await;
                    if response.status != "ready" {
                        metrics.readiness_failures.fetch_add(1, Ordering::Relaxed);
                    }
                    let status = if response.status == "ready" {
                        StatusCode::OK
                    } else {
                        StatusCode::SERVICE_UNAVAILABLE
                    };
                    (status, Json(response))
                }
            }),
        )
        .route(
            "/build-info",
            get(|| async {
                Json(BuildInfo {
                    service: "meridian-rust",
                    version: env!("CARGO_PKG_VERSION"),
                    git_sha: option_env!("GIT_SHA").unwrap_or("development"),
                })
            }),
        )
        .route("/metrics", get(render_metrics))
        .route("/api/v1/events/stream", get(crate::realtime::events_stream))
        .route("/api/v1/auth/token", post(login))
        .route("/api/v1/auth/refresh", post(refresh))
        .route("/api/v1/auth/logout", post(logout))
        .route("/api/v1/auth/logout-all", post(logout_all))
        .route("/api/v1/auth/sessions", get(list_sessions))
        .route(
            "/api/v1/auth/sessions/{session_id}",
            axum::routing::delete(revoke_session),
        )
        .route("/api/v1/auth/password/change", post(change_password))
        .route(
            "/api/v1/auth/password-reset/request",
            post(request_password_reset),
        )
        .route(
            "/api/v1/auth/password-reset/confirm",
            post(confirm_password_reset),
        )
        .route("/api/v1/users/me", get(current_user))
}

/// A JSON body extractor that answers a malformed or invalid body with
/// `FastAPI`'s **422** shape instead of `Axum`'s default **400** plain-text
/// rejection, so the error contract matches the legacy oracle.
///
/// Parity policy (chosen 2026-07-29): *match the malformed-JSON status only*. The
/// deliberate 401-before-body-parse auth-ordering on `AuthActor` routes is kept as
/// an intentional divergence; this extractor is applied to the body-carrying
/// routes that have no auth extractor (password change, password-reset), where the
/// only difference from legacy was 400-vs-422.
///
/// The body reproduces `FastAPI`'s `Pydantic` error envelope closely enough to
/// match under the differential-parity skeleton: a `detail` array of one object
/// with `type`/`loc`/`msg`/`input`, plus `ctx` for a JSON syntax error (the
/// `json_invalid` case), matching legacy's own malformed-vs-missing distinction.
struct ValidatedJson<T>(T);

impl<T, S> FromRequest<S> for ValidatedJson<T>
where
    T: serde::de::DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = (StatusCode, Json<serde_json::Value>);

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        match Json::<T>::from_request(req, state).await {
            Ok(Json(value)) => Ok(Self(value)),
            Err(rejection) => {
                let mut item = serde_json::json!({
                    "loc": ["body"],
                    "msg": rejection.body_text(),
                    "input": {},
                });
                if matches!(rejection, JsonRejection::JsonSyntaxError(_)) {
                    item["type"] = serde_json::Value::String("json_invalid".to_owned());
                    item["ctx"] = serde_json::json!({ "error": "JSON decode error" });
                } else {
                    item["type"] = serde_json::Value::String("value_error".to_owned());
                }
                Err((
                    StatusCode::UNPROCESSABLE_ENTITY,
                    Json(serde_json::json!({ "detail": [item] })),
                ))
            }
        }
    }
}

#[derive(serde::Deserialize)]
struct LoginForm {
    username: String,
    password: String,
}

/// The client IP for the audit record on a session, read from the proxy that
/// terminates TLS in front of the app. `dioxus::serve` owns the Axum service, so
/// `ConnectInfo` is not available to wire a socket peer; behind a load balancer
/// the forwarded headers are the real source anyway. `X-Forwarded-For` may hold a
/// comma-separated chain — the first entry is the original client. Falls back to
/// `X-Real-IP`. A direct (proxy-less) local request yields `None`, which the
/// sessions view renders as "—" rather than a fabricated address.
fn client_ip(headers: &HeaderMap) -> Option<std::net::IpAddr> {
    headers
        .get("x-forwarded-for")
        .and_then(|value| value.to_str().ok())
        .and_then(|chain| chain.split(',').next())
        .or_else(|| headers.get("x-real-ip").and_then(|value| value.to_str().ok()))
        .and_then(|candidate| candidate.trim().parse().ok())
}

async fn login(
    Extension(identity): Extension<IdentityService>,
    Extension(throttle): Extension<LoginThrottle>,
    headers: HeaderMap,
    Form(form): Form<LoginForm>,
) -> Result<Json<TokenResponse>, (StatusCode, Json<ApiError>)> {
    let ip = client_ip(&headers);
    if let Some(ip) = ip
        && throttle.is_blocked(ip)
    {
        return Err((
            StatusCode::TOO_MANY_REQUESTS,
            Json(ApiError {
                detail: "Too many failed sign-in attempts. Try again later.".to_owned(),
            }),
        ));
    }
    let user = match identity.authenticate(&form.username, &form.password).await {
        Ok(user) => {
            if let Some(ip) = ip {
                throttle.clear(ip);
            }
            user
        }
        Err(error) => {
            if let Some(ip) = ip {
                throttle.record_failure(ip);
            }
            return Err(auth_error(error));
        }
    };
    let user_agent = headers
        .get(header::USER_AGENT)
        .and_then(|value| value.to_str().ok());
    let ip_address = client_ip(&headers);
    let (session, refresh_token) = identity
        .create_session(&user, user_agent, ip_address, None)
        .await
        .map_err(auth_error)?;
    let access_token = identity
        .issue_access_token(&user, session.id)
        .map_err(auth_error)?;
    Ok(Json(TokenResponse {
        access_token,
        token_type: "bearer",
        refresh_token: Some(refresh_token),
        expires_in: Some(900),
    }))
}

async fn refresh(
    Extension(identity): Extension<IdentityService>,
    Json(payload): Json<RefreshRequest>,
) -> Result<Json<TokenResponse>, (StatusCode, Json<ApiError>)> {
    let (session, refresh_token, user) = identity
        .rotate_session(&payload.refresh_token)
        .await
        .map_err(auth_error)?;
    let access_token = identity
        .issue_access_token(&user, session.id)
        .map_err(auth_error)?;
    Ok(Json(TokenResponse {
        access_token,
        token_type: "bearer",
        refresh_token: Some(refresh_token),
        expires_in: Some(900),
    }))
}

async fn logout(
    Extension(identity): Extension<IdentityService>,
    headers: HeaderMap,
) -> Result<StatusCode, (StatusCode, Json<ApiError>)> {
    let token = bearer_token(&headers)?;
    let (user, session) = identity
        .resolve_access_token(token)
        .await
        .map_err(auth_error)?;
    identity
        .logout(user.id, session.id)
        .await
        .map_err(auth_error)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn logout_all(
    Extension(identity): Extension<IdentityService>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<ApiError>)> {
    let token = bearer_token(&headers)?;
    let (user, _) = identity
        .resolve_access_token(token)
        .await
        .map_err(auth_error)?;
    let revoked = identity.logout_all(user.id).await.map_err(auth_error)?;
    Ok(Json(serde_json::json!({ "revoked": revoked })))
}

/// A session as the legacy `GET /auth/sessions` contract returns it: the display
/// fields plus `is_current`, without the internal family/token/revocation columns.
#[derive(serde::Serialize)]
struct SessionView {
    id: String,
    user_agent: Option<String>,
    ip_address: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    created_at: time::OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    last_seen_at: time::OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    expires_at: time::OffsetDateTime,
    is_current: bool,
}

async fn list_sessions(
    Extension(identity): Extension<IdentityService>,
    headers: HeaderMap,
) -> Result<Json<Vec<SessionView>>, (StatusCode, Json<ApiError>)> {
    let token = bearer_token(&headers)?;
    let (user, current) = identity
        .resolve_access_token(token)
        .await
        .map_err(auth_error)?;
    let sessions = identity.list_sessions(user.id).await.map_err(auth_error)?;
    let views = sessions
        .into_iter()
        .map(|s| SessionView {
            is_current: s.id == current.id,
            id: s.id.to_string(),
            user_agent: s.user_agent,
            ip_address: s.ip_address,
            created_at: s.created_at,
            last_seen_at: s.last_seen_at,
            expires_at: s.expires_at,
        })
        .collect();
    Ok(Json(views))
}

async fn revoke_session(
    Extension(identity): Extension<IdentityService>,
    axum::extract::Path(session_id): axum::extract::Path<uuid::Uuid>,
    headers: HeaderMap,
) -> Result<StatusCode, (StatusCode, Json<ApiError>)> {
    let token = bearer_token(&headers)?;
    let (user, _) = identity
        .resolve_access_token(token)
        .await
        .map_err(auth_error)?;
    let removed = identity
        .revoke_session(user.id, session_id)
        .await
        .map_err(auth_error)?;
    if removed {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err((
            StatusCode::NOT_FOUND,
            Json(ApiError {
                detail: "Session not found".to_owned(),
            }),
        ))
    }
}

#[derive(serde::Deserialize)]
struct PasswordChange {
    current_password: String,
    new_password: String,
}

async fn change_password(
    Extension(identity): Extension<IdentityService>,
    headers: HeaderMap,
    ValidatedJson(payload): ValidatedJson<PasswordChange>,
) -> Result<StatusCode, (StatusCode, Json<ApiError>)> {
    let token = bearer_token(&headers)?;
    let (user, session) = identity
        .resolve_access_token(token)
        .await
        .map_err(auth_error)?;
    if payload.new_password.len() < 8 {
        return Err((
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(ApiError {
                detail: "password must be at least 8 characters".to_owned(),
            }),
        ));
    }
    identity
        .change_password(
            user.id,
            &payload.current_password,
            &payload.new_password,
            Some(session.id),
        )
        .await
        .map_err(auth_error)?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(serde::Deserialize)]
struct PasswordResetRequestBody {
    email: String,
}

/// `POST /api/v1/auth/password-reset/request` — issue a single-use reset token.
///
/// The response is identical whether or not the email is registered (202 with a
/// null token), so an unauthenticated caller cannot enumerate accounts. The token
/// is echoed in the body only when `MERIDIAN_EXPOSE_RESET_TOKENS=true` (local
/// development), mirroring the legacy `expose_reset_tokens` setting; in any other
/// configuration real delivery is out of band.
async fn request_password_reset(
    Extension(identity): Extension<IdentityService>,
    ValidatedJson(payload): ValidatedJson<PasswordResetRequestBody>,
) -> Result<(StatusCode, Json<serde_json::Value>), (StatusCode, Json<ApiError>)> {
    let token = identity
        .request_password_reset(&payload.email)
        .await
        .map_err(auth_error)?;
    let expose = std::env::var("MERIDIAN_EXPOSE_RESET_TOKENS").as_deref() == Ok("true");
    let reset_token = if expose { token } else { None };
    Ok((
        StatusCode::ACCEPTED,
        Json(serde_json::json!({
            "detail": "If the account exists, a reset link has been issued.",
            "reset_token": reset_token,
        })),
    ))
}

#[derive(serde::Deserialize)]
struct PasswordResetConfirmBody {
    token: String,
    new_password: String,
}

/// `POST /api/v1/auth/password-reset/confirm` — consume a token and set the
/// password, revoking every live session. 204 on success.
async fn confirm_password_reset(
    Extension(identity): Extension<IdentityService>,
    ValidatedJson(payload): ValidatedJson<PasswordResetConfirmBody>,
) -> Result<StatusCode, (StatusCode, Json<ApiError>)> {
    // Legacy enforces a 10-character minimum on the new password (422).
    if payload.new_password.len() < 10 {
        return Err((
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(ApiError {
                detail: "new_password must be at least 10 characters".to_owned(),
            }),
        ));
    }
    identity
        .confirm_password_reset(&payload.token, &payload.new_password)
        .await
        .map_err(|error| match error {
            // Legacy answers 400 for an unknown/expired/used reset token, not the
            // 401 that `InvalidCredentials` maps to on the login path.
            IdentityError::InvalidCredentials => (
                StatusCode::BAD_REQUEST,
                Json(ApiError {
                    detail: "Invalid or expired reset token".to_owned(),
                }),
            ),
            other => auth_error(other),
        })?;
    Ok(StatusCode::NO_CONTENT)
}

/// The `GET /users/me` profile contract. Mirrors the legacy oracle's shape: the
/// core identity fields plus the self-service profile block. The Rust `users`
/// table does not carry the profile columns yet, so the optional fields serialize
/// as `null` and the string/collection fields take the legacy defaults; when those
/// columns are added, populate them here.
#[derive(serde::Serialize)]
struct MeProfile {
    id: String,
    email: String,
    handle: String,
    display_name: String,
    role: String,
    department: Option<String>,
    is_active: bool,
    is_admin: bool,
    #[serde(with = "time::serde::rfc3339")]
    created_at: time::OffsetDateTime,
    avatar_url: Option<String>,
    bio: Option<String>,
    pronouns: Option<String>,
    title: Option<String>,
    location: Option<String>,
    phone: Option<String>,
    timezone: String,
    availability_status: String,
    availability_until: Option<String>,
    profile_visibility: String,
    languages: Vec<String>,
    skills: Vec<String>,
    preferences: serde_json::Value,
    working_hours: serde_json::Value,
}

async fn current_user(
    Extension(identity): Extension<IdentityService>,
    headers: HeaderMap,
) -> Result<Json<MeProfile>, (StatusCode, Json<ApiError>)> {
    let token = bearer_token(&headers)?;
    let (user, _) = identity
        .resolve_access_token(token)
        .await
        .map_err(auth_error)?;
    Ok(Json(MeProfile {
        id: user.id.to_string(),
        email: user.email.clone(),
        handle: user.handle.clone(),
        display_name: user.display_name.clone(),
        role: user.role.clone(),
        department: user.department.clone(),
        is_active: user.is_active,
        is_admin: user.is_admin,
        created_at: user.created_at,
        avatar_url: None,
        bio: None,
        pronouns: None,
        title: None,
        location: None,
        phone: None,
        timezone: "UTC".to_owned(),
        availability_status: "available".to_owned(),
        availability_until: None,
        profile_visibility: "newsroom".to_owned(),
        languages: Vec::new(),
        skills: Vec::new(),
        preferences: serde_json::json!({}),
        working_hours: serde_json::json!({}),
    }))
}

fn bearer_token(headers: &HeaderMap) -> Result<&str, (StatusCode, Json<ApiError>)> {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .ok_or_else(|| auth_error(IdentityError::InvalidAccessToken))
}

fn auth_error(error: IdentityError) -> (StatusCode, Json<ApiError>) {
    let (status, detail) = match error {
        IdentityError::InvalidCredentials => {
            (StatusCode::UNAUTHORIZED, "Incorrect username or password")
        }
        IdentityError::RefreshTokenReuse => (
            StatusCode::UNAUTHORIZED,
            "Refresh token was already used; session family revoked. Sign in again.",
        ),
        IdentityError::InvalidRefreshToken => (StatusCode::UNAUTHORIZED, "Invalid refresh token"),
        IdentityError::InvalidAccessToken => {
            (StatusCode::UNAUTHORIZED, "Could not validate credentials")
        }
        IdentityError::Jwt(source) => {
            tracing::warn!(error = %source, "access token validation failed");
            (StatusCode::UNAUTHORIZED, "Could not validate credentials")
        }
        IdentityError::Database(source) => {
            tracing::error!(error = %source, "identity database operation failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Authentication service unavailable",
            )
        }
        IdentityError::PasswordHash(source) => {
            tracing::error!(error = %source, "password hashing failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Authentication service unavailable",
            )
        }
    };
    (
        status,
        Json(ApiError {
            detail: detail.to_owned(),
        }),
    )
}

async fn record_request(
    Extension(metrics): Extension<AppMetrics>,
    request: axum::http::Request<Body>,
    next: Next,
) -> Response {
    let response = next.run(request).await;
    metrics.requests.fetch_add(1, Ordering::Relaxed);
    if response.status().is_server_error() {
        metrics.server_errors.fetch_add(1, Ordering::Relaxed);
    }
    response
}

async fn render_metrics(Extension(metrics): Extension<AppMetrics>) -> impl IntoResponse {
    let body = format!(
        concat!(
            "# HELP meridian_http_requests_total HTTP requests served.\n",
            "# TYPE meridian_http_requests_total counter\n",
            "meridian_http_requests_total {}\n",
            "# HELP meridian_http_server_errors_total HTTP 5xx responses.\n",
            "# TYPE meridian_http_server_errors_total counter\n",
            "meridian_http_server_errors_total {}\n",
            "# HELP meridian_readiness_failures_total Failed readiness evaluations.\n",
            "# TYPE meridian_readiness_failures_total counter\n",
            "meridian_readiness_failures_total {}\n"
        ),
        metrics.requests.load(Ordering::Relaxed),
        metrics.server_errors.load(Ordering::Relaxed),
        metrics.readiness_failures.load(Ordering::Relaxed),
    );
    ([(header::CONTENT_TYPE, "text/plain; version=0.0.4")], body)
}

#[cfg(test)]
mod tests {
    use axum::{Router, body::Body, http::Request, middleware, routing::get};
    use tower::ServiceExt;

    use super::add_security_headers;

    #[tokio::test]
    async fn security_headers_cover_every_response() {
        let app = Router::new()
            .route("/", get(|| async { "ok" }))
            .layer(middleware::from_fn(add_security_headers));
        let response = app
            .oneshot(Request::new(Body::empty()))
            .await
            .expect("security header response");
        let headers = response.headers();

        assert_eq!(headers["x-content-type-options"], "nosniff");
        assert_eq!(
            headers["referrer-policy"],
            "strict-origin-when-cross-origin"
        );
        assert_eq!(
            headers["permissions-policy"],
            "camera=(), microphone=(), geolocation=()"
        );
        assert!(
            headers["content-security-policy"]
                .to_str()
                .expect("valid CSP")
                .contains("frame-ancestors 'none'")
        );
    }
}
