//! Module 3 compatibility REST routes: desks and feature flags.
//!
//! Paths and status codes mirror the legacy `FastAPI` router under `/api/v1`.
//! These operations remain legacy-owned in the contract ledger until
//! differential HTTP tests prove status/schema/audit parity; wiring them here is
//! the step that makes that comparison possible.
//!
//! Not wired (deferred with their dependencies): `GET /desks/{id}/analytics`
//! (Story/Task aggregation — Modules 4 & 6) and the composite `GET /navigation`
//! (badges + favourites + recents — Module 7 / Module 14).

use axum::{
    Extension, Json,
    extract::{FromRequestParts, Path, Query},
    http::{HeaderMap, StatusCode, header, request::Parts},
    response::IntoResponse,
    routing::{get, patch, post, put},
    Router,
};
use meridian_identity::IdentityService;
use meridian_newsroom::{
    Actor, NewsroomError, NewsroomService, Role,
    branding::BrandingInput,
    desks::{ScheduleInput, SlaEntry},
    nav::FlagInput,
    sub_sectors::{SubSectorDraft, SubSectorPatch},
};
use serde::Deserialize;
use serde_json::{Map, Value};
use uuid::Uuid;

/// Error responses carry a JSON body. Most errors use `{"detail": "..."}`;
/// capability denials use the legacy `{"detail": {"message", "reason"}}` shape.
pub(crate) type ApiResult<T> = Result<T, (StatusCode, Json<Value>)>;

pub fn router() -> Router {
    Router::new()
        .route("/api/v1/desks", get(list_desks).post(create_desk))
        .route(
            "/api/v1/desks/{desk_id}",
            axum::routing::patch(update_desk).delete(delete_desk),
        )
        .route(
            "/api/v1/feature-flags/{key}",
            axum::routing::patch(update_flag).delete(delete_flag),
        )
        .route("/api/v1/desks/{desk_id}/tree", get(desk_tree))
        .route("/api/v1/desks/{desk_id}/archive", post(archive_desk))
        .route("/api/v1/desks/{desk_id}/unarchive", post(unarchive_desk))
        .route(
            "/api/v1/desks/{desk_id}/slas",
            get(list_slas).put(replace_slas),
        )
        .route(
            "/api/v1/desks/{desk_id}/schedule",
            get(get_schedule).put(set_schedule),
        )
        .route(
            "/api/v1/desks/{desk_id}/invitations",
            get(list_desk_invitations).post(create_invitation),
        )
        .route("/api/v1/invitations", get(my_invitations))
        .route("/api/v1/invitations/{invitation_id}/accept", post(accept_invitation))
        .route("/api/v1/invitations/{invitation_id}/decline", post(decline_invitation))
        // Sector applications (Phase 3): apply, see your own, admin queue + review.
        .route("/api/v1/sectors/directory", get(sectors_directory))
        .route("/api/v1/sectors/{desk_id}/apply", post(apply_to_sector))
        .route("/api/v1/sector-applications/mine", get(my_applications))
        .route("/api/v1/sector-applications/pending", get(pending_applications))
        .route("/api/v1/sector-applications/{application_id}/review", post(review_application))
        .route("/api/v1/sector-applications/{application_id}/certificate", get(application_certificate))
        // Sub-sectors (industries) within a sector — the two-level taxonomy.
        .route(
            "/api/v1/sectors/{desk_id}/sub-sectors",
            get(list_sub_sectors).post(create_sub_sector),
        )
        .route(
            "/api/v1/sub-sectors/{sub_sector_id}",
            patch(update_sub_sector).delete(delete_sub_sector),
        )
        // A single sector's application queue (delegated managers see only their own).
        .route("/api/v1/sectors/{desk_id}/applications", get(sector_applications))
        // The taxonomy path (root → node) for one industry, for breadcrumbs/SEO.
        .route("/api/v1/sub-sectors/{sub_sector_id}/path", get(sub_sector_path))
        .route("/api/v1/feature-flags", get(evaluate_flags).post(create_flag))
        .route("/api/v1/feature-flags/admin", get(list_flags))
        .route(
            "/api/v1/feature-flags/{key}/emergency-disable",
            put(emergency_disable_flag).post(emergency_disable_flag),
        )
        // Per-workspace branding (multi-tenant): read is open (the masthead needs it
        // before auth); writes are capability-gated in the service.
        .route("/api/v1/branding", get(get_branding).put(set_org_branding))
        .route(
            "/api/v1/desks/{desk_id}/branding",
            get(get_desk_branding).put(set_desk_branding),
        )
        // Per-sector SMTP — a desk lead configures its own outbound mail server.
        .route(
            "/api/v1/desks/{desk_id}/smtp",
            get(get_desk_smtp).put(set_desk_smtp),
        )
        .route("/api/v1/desks/{desk_id}/smtp/test", post(test_desk_smtp))
}

async fn get_desk_smtp(
    AuthActor(actor): AuthActor,
    Extension(newsroom): Extension<NewsroomService>,
    Path(desk_id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let settings = newsroom.get_desk_smtp(&actor, desk_id).await.map_err(newsroom_error)?;
    Ok(Json(settings))
}

async fn set_desk_smtp(
    AuthActor(actor): AuthActor,
    Extension(newsroom): Extension<NewsroomService>,
    Path(desk_id): Path<Uuid>,
    Json(input): Json<meridian_newsroom::desk_email::SmtpInput>,
) -> ApiResult<impl IntoResponse> {
    let settings = newsroom.set_desk_smtp(&actor, desk_id, &input).await.map_err(newsroom_error)?;
    Ok(Json(settings))
}

#[derive(serde::Deserialize)]
struct SmtpTestBody {
    to: String,
}

async fn test_desk_smtp(
    AuthActor(actor): AuthActor,
    Extension(newsroom): Extension<NewsroomService>,
    Path(desk_id): Path<Uuid>,
    Json(body): Json<SmtpTestBody>,
) -> ApiResult<impl IntoResponse> {
    newsroom
        .send_desk_test_email(&actor, desk_id, &body.to)
        .await
        .map_err(newsroom_error)?;
    Ok(Json(serde_json::json!({ "sent": true })))
}

// --- Authentication + error mapping -----------------------------------------

/// An authenticated [`Actor`], resolved as a request-parts extractor. Because
/// `FromRequestParts` runs before the body (`FromRequest`) extractor, using this
/// makes the 401 auth check precede body validation — so an unauthenticated
/// request with a malformed body returns 401, not 422, matching legacy.
pub(crate) struct AuthActor(pub Actor);

impl<S: Send + Sync> FromRequestParts<S> for AuthActor {
    type Rejection = (StatusCode, Json<Value>);

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let Extension(identity) = Extension::<IdentityService>::from_request_parts(parts, state)
            .await
            .map_err(|_| unauthorized("Could not validate credentials"))?;
        let actor = actor_from_request(&identity, &parts.headers).await?;
        Ok(AuthActor(actor))
    }
}

/// Resolves the bearer token to the acting user and a capability [`Actor`].
pub(crate) async fn actor_from_request(
    identity: &IdentityService,
    headers: &HeaderMap,
) -> ApiResult<Actor> {
    let token = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .ok_or_else(|| unauthorized("Could not validate credentials"))?;
    let (user, _session) = identity
        .resolve_access_token(token)
        .await
        .map_err(|_| unauthorized("Could not validate credentials"))?;
    let role = Role::from_wire(&user.role)
        .ok_or_else(|| unauthorized("Could not validate credentials"))?;
    Ok(Actor {
        id: user.id,
        role,
        is_admin: user.is_admin,
    })
}

fn unauthorized(detail: &str) -> (StatusCode, Json<Value>) {
    (
        StatusCode::UNAUTHORIZED,
        Json(serde_json::json!({ "detail": detail })),
    )
}

// --- Branding (multi-tenant) ------------------------------------------------

#[derive(Deserialize)]
struct BrandQuery {
    desk: Option<Uuid>,
}

/// `GET /api/v1/branding[?desk=<uuid>]` — the resolved brand for a context
/// (desk brand → org default → null). Open, because the masthead is rendered
/// before authentication.
async fn get_branding(
    Extension(newsroom): Extension<NewsroomService>,
    Query(query): Query<BrandQuery>,
) -> ApiResult<impl IntoResponse> {
    let brand = newsroom
        .resolve_branding(query.desk)
        .await
        .map_err(newsroom_error)?;
    Ok(Json(brand))
}

/// `PUT /api/v1/branding` — set the org-default brand. Gated on `permissions.manage`.
async fn set_org_branding(
    AuthActor(actor): AuthActor,
    Extension(newsroom): Extension<NewsroomService>,
    Json(input): Json<BrandingInput>,
) -> ApiResult<impl IntoResponse> {
    let brand = newsroom
        .set_org_branding(&actor, &input)
        .await
        .map_err(newsroom_error)?;
    Ok(Json(brand))
}

/// `GET /api/v1/desks/{desk_id}/branding` — a desk's own brand (or null).
async fn get_desk_branding(
    Extension(newsroom): Extension<NewsroomService>,
    Path(desk_id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let brand = newsroom
        .desk_branding(desk_id)
        .await
        .map_err(newsroom_error)?;
    Ok(Json(brand))
}

/// `PUT /api/v1/desks/{desk_id}/branding` — set a desk's brand. Requires `desks.manage`.
async fn set_desk_branding(
    AuthActor(actor): AuthActor,
    Extension(newsroom): Extension<NewsroomService>,
    Path(desk_id): Path<Uuid>,
    Json(input): Json<BrandingInput>,
) -> ApiResult<impl IntoResponse> {
    let brand = newsroom
        .set_desk_branding(&actor, desk_id, &input)
        .await
        .map_err(newsroom_error)?;
    Ok(Json(brand))
}

pub(crate) fn newsroom_error(error: NewsroomError) -> (StatusCode, Json<Value>) {
    match error {
        NewsroomError::NotFound(entity) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "detail": format!("{entity} not found") })),
        ),
        // The legacy capability denial is a structured detail: message + reason.
        NewsroomError::Forbidden { capability, reason } => (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({
                "detail": {
                    "message": format!("Missing capability '{capability}'"),
                    "reason": reason,
                }
            })),
        ),
        NewsroomError::Conflict(message) => (
            StatusCode::CONFLICT,
            Json(serde_json::json!({ "detail": message })),
        ),
        NewsroomError::Unprocessable(message) => (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(serde_json::json!({ "detail": message })),
        ),
        NewsroomError::Database(source) => {
            tracing::error!(error = %source, "newsroom database operation failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "detail": "Newsroom service unavailable" })),
            )
        }
    }
}

// --- Request bodies ---------------------------------------------------------

fn default_warn_at() -> i32 {
    80
}
fn default_true() -> bool {
    true
}
fn default_timezone() -> String {
    "UTC".to_owned()
}
/// The default workspace role for an invitation that names none.
///
/// Migration `0016` narrowed `desk_memberships.role` to eight workspace roles and
/// set the **column** default to `viewer` — the safe choice, since a membership
/// granted without a stated role should not carry edit rights. This serde default
/// was left saying `member`, which `0016` had just made illegal, so every
/// invitation that omitted a role was rejected by the constraint. The two defaults
/// must agree; `viewer` is the one `0016` chose deliberately.
fn default_member() -> String {
    "viewer".to_owned()
}
fn default_object() -> Value {
    Value::Object(Map::new())
}

#[derive(Deserialize)]
struct SlaEntryRequest {
    workflow_state: String,
    target_hours: f64,
    #[serde(default = "default_warn_at")]
    warn_at_percent: i32,
    #[serde(default = "default_true")]
    is_active: bool,
}

#[derive(Deserialize)]
struct SlaReplaceRequest {
    #[serde(default)]
    entries: Vec<SlaEntryRequest>,
}

#[derive(Deserialize)]
struct ScheduleRequest {
    #[serde(default = "default_timezone")]
    timezone: String,
    #[serde(default = "default_object")]
    hours: Value,
    #[serde(default)]
    on_call_user_id: Option<Uuid>,
    #[serde(default)]
    notes: Option<String>,
}

#[derive(Deserialize)]
struct InvitationRequest {
    user_id: Uuid,
    #[serde(default = "default_member")]
    desk_role: String,
    #[serde(default)]
    message: Option<String>,
}

#[derive(Deserialize)]
struct FlagCreateRequest {
    key: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    enabled: bool,
    #[serde(default)]
    rollout_percent: i32,
    #[serde(default)]
    target_user_ids: Vec<String>,
    #[serde(default)]
    target_desk_ids: Vec<String>,
    #[serde(default)]
    emergency_disabled: bool,
}

/// PATCH body for a feature flag — like the create request but with no `key`
/// (the key comes from the path), matching legacy's partial `FlagUpdate`.
#[derive(Deserialize)]
struct FlagUpdateRequest {
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    enabled: bool,
    #[serde(default)]
    rollout_percent: i32,
    #[serde(default)]
    target_user_ids: Vec<String>,
    #[serde(default)]
    target_desk_ids: Vec<String>,
    #[serde(default)]
    emergency_disabled: bool,
}

// --- Desk CRUD --------------------------------------------------------------

#[derive(Deserialize)]
struct DeskListQuery {
    #[serde(default)]
    include_archived: bool,
}

#[derive(Deserialize)]
struct DeskCreateRequest {
    name: String,
    slug: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    lead_user_id: Option<Uuid>,
    #[serde(default)]
    parent_id: Option<Uuid>,
}

#[derive(Deserialize)]
struct DeskPatchRequest {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    slug: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    lead_user_id: Option<Uuid>,
    #[serde(default)]
    parent_id: Option<Uuid>,
    #[serde(default)]
    settings: Option<serde_json::Value>,
}

async fn list_desks(
    AuthActor(actor): AuthActor,
    Extension(newsroom): Extension<NewsroomService>,
    axum::extract::Query(query): axum::extract::Query<DeskListQuery>,
) -> ApiResult<impl IntoResponse> {
    let desks = newsroom
        .list_desks(&actor, query.include_archived)
        .await
        .map_err(newsroom_error)?;
    Ok(Json(desks))
}

async fn create_desk(
    AuthActor(actor): AuthActor,
    Extension(newsroom): Extension<NewsroomService>,
    Json(body): Json<DeskCreateRequest>,
) -> ApiResult<impl IntoResponse> {
    let desk = newsroom
        .create_desk(
            &actor,
            &body.name,
            &body.slug,
            body.description.as_deref(),
            body.lead_user_id,
            body.parent_id,
        )
        .await
        .map_err(newsroom_error)?;
    Ok((StatusCode::CREATED, Json(desk)))
}

async fn update_desk(
    AuthActor(actor): AuthActor,
    Extension(newsroom): Extension<NewsroomService>,
    Path(desk_id): Path<Uuid>,
    Json(body): Json<DeskPatchRequest>,
) -> ApiResult<impl IntoResponse> {
    let patch = meridian_newsroom::desks::DeskPatch {
        name: body.name,
        slug: body.slug,
        description: body.description,
        lead_user_id: body.lead_user_id,
        parent_id: body.parent_id,
        settings: body.settings,
    };
    let desk = newsroom
        .update_desk(&actor, desk_id, &patch)
        .await
        .map_err(newsroom_error)?;
    Ok(Json(desk))
}

async fn delete_desk(
    AuthActor(actor): AuthActor,
    Extension(newsroom): Extension<NewsroomService>,
    Path(desk_id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    newsroom.delete_desk(&actor, desk_id).await.map_err(newsroom_error)?;
    Ok(StatusCode::NO_CONTENT)
}

// -- Sub-sectors (industries) -------------------------------------------------

/// `GET /api/v1/sectors/{desk_id}/sub-sectors` — an industry directory for a sector.
/// Readable by any authenticated caller (drives the story editor's industry picker).
async fn list_sub_sectors(
    AuthActor(_actor): AuthActor,
    Extension(newsroom): Extension<NewsroomService>,
    Path(desk_id): Path<Uuid>,
    Query(query): Query<DeskListQuery>,
) -> ApiResult<impl IntoResponse> {
    let subs = newsroom
        .list_sub_sectors(desk_id, query.include_archived)
        .await
        .map_err(newsroom_error)?;
    Ok(Json(subs))
}

/// `POST /api/v1/sectors/{desk_id}/sub-sectors` — add an industry to a sector.
/// Requires desk-scoped `desks.manage` (or desk lead) on the sector.
async fn create_sub_sector(
    AuthActor(actor): AuthActor,
    Extension(newsroom): Extension<NewsroomService>,
    Path(desk_id): Path<Uuid>,
    Json(draft): Json<SubSectorDraft>,
) -> ApiResult<impl IntoResponse> {
    let sub = newsroom
        .create_sub_sector(&actor, desk_id, &draft)
        .await
        .map_err(newsroom_error)?;
    Ok((StatusCode::CREATED, Json(sub)))
}

/// `PATCH /api/v1/sub-sectors/{id}` — edit or archive an industry.
async fn update_sub_sector(
    AuthActor(actor): AuthActor,
    Extension(newsroom): Extension<NewsroomService>,
    Path(sub_sector_id): Path<Uuid>,
    Json(patch): Json<SubSectorPatch>,
) -> ApiResult<impl IntoResponse> {
    let sub = newsroom
        .update_sub_sector(&actor, sub_sector_id, &patch)
        .await
        .map_err(newsroom_error)?;
    Ok(Json(sub))
}

/// `GET /api/v1/sectors/{desk_id}/applications` — one sector's application queue,
/// scoped to a delegated manager's own sector (desk-scoped `desks.manage`).
async fn sector_applications(
    AuthActor(actor): AuthActor,
    Extension(newsroom): Extension<NewsroomService>,
    Path(desk_id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let apps = newsroom
        .sector_applications(&actor, desk_id)
        .await
        .map_err(newsroom_error)?;
    Ok(Json(apps))
}

/// `GET /api/v1/sub-sectors/{id}/path` — the industry's taxonomy path (root → node).
async fn sub_sector_path(
    AuthActor(_actor): AuthActor,
    Extension(newsroom): Extension<NewsroomService>,
    Path(sub_sector_id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let path = newsroom
        .sub_sector_path(sub_sector_id)
        .await
        .map_err(newsroom_error)?;
    Ok(Json(path))
}

/// `DELETE /api/v1/sub-sectors/{id}` — remove an industry (stories keep their desk).
async fn delete_sub_sector(
    AuthActor(actor): AuthActor,
    Extension(newsroom): Extension<NewsroomService>,
    Path(sub_sector_id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    newsroom
        .delete_sub_sector(&actor, sub_sector_id)
        .await
        .map_err(newsroom_error)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn update_flag(
    AuthActor(actor): AuthActor,
    Extension(newsroom): Extension<NewsroomService>,
    Path(key): Path<String>,
    Json(payload): Json<FlagUpdateRequest>,
) -> ApiResult<impl IntoResponse> {
    let input = FlagInput {
        description: payload.description,
        enabled: payload.enabled,
        rollout_percent: payload.rollout_percent,
        target_user_ids: payload.target_user_ids,
        target_desk_ids: payload.target_desk_ids,
        emergency_disabled: payload.emergency_disabled,
    };
    let flag = newsroom.update_flag(&actor, &key, &input).await.map_err(newsroom_error)?;
    Ok(Json(flag))
}

async fn delete_flag(
    AuthActor(actor): AuthActor,
    Extension(newsroom): Extension<NewsroomService>,
    Path(key): Path<String>,
) -> ApiResult<impl IntoResponse> {
    newsroom.delete_flag(&actor, &key).await.map_err(newsroom_error)?;
    Ok(StatusCode::NO_CONTENT)
}

// --- Desk handlers ----------------------------------------------------------

async fn desk_tree(
    Extension(identity): Extension<IdentityService>,
    Extension(newsroom): Extension<NewsroomService>,
    Path(desk_id): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<impl IntoResponse> {
    actor_from_request(&identity, &headers).await?;
    let tree = newsroom.desk_tree(desk_id).await.map_err(newsroom_error)?;
    Ok(Json(tree))
}

async fn archive_desk(
    Extension(identity): Extension<IdentityService>,
    Extension(newsroom): Extension<NewsroomService>,
    Path(desk_id): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<impl IntoResponse> {
    let actor = actor_from_request(&identity, &headers).await?;
    let desk = newsroom.get_desk(desk_id).await.map_err(newsroom_error)?;
    newsroom.archive(&actor, &desk).await.map_err(newsroom_error)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn unarchive_desk(
    Extension(identity): Extension<IdentityService>,
    Extension(newsroom): Extension<NewsroomService>,
    Path(desk_id): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<impl IntoResponse> {
    let actor = actor_from_request(&identity, &headers).await?;
    let desk = newsroom.get_desk(desk_id).await.map_err(newsroom_error)?;
    newsroom
        .unarchive(&actor, &desk)
        .await
        .map_err(newsroom_error)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn list_slas(
    Extension(identity): Extension<IdentityService>,
    Extension(newsroom): Extension<NewsroomService>,
    Path(desk_id): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<impl IntoResponse> {
    actor_from_request(&identity, &headers).await?;
    // Preserve the legacy 404-before-list behaviour.
    newsroom.get_desk(desk_id).await.map_err(newsroom_error)?;
    let slas = newsroom.get_slas(desk_id).await.map_err(newsroom_error)?;
    Ok(Json(slas))
}

async fn replace_slas(
    AuthActor(actor): AuthActor,
    Extension(newsroom): Extension<NewsroomService>,
    Path(desk_id): Path<Uuid>,
    Json(payload): Json<SlaReplaceRequest>,
) -> ApiResult<impl IntoResponse> {
    let desk = newsroom.get_desk(desk_id).await.map_err(newsroom_error)?;
    let entries: Vec<SlaEntry> = payload
        .entries
        .into_iter()
        .map(|entry| SlaEntry {
            workflow_state: entry.workflow_state,
            target_hours: entry.target_hours,
            warn_at_percent: entry.warn_at_percent,
            is_active: entry.is_active,
        })
        .collect();
    let slas = newsroom
        .replace_slas(&actor, &desk, &entries)
        .await
        .map_err(newsroom_error)?;
    Ok(Json(slas))
}

async fn get_schedule(
    Extension(identity): Extension<IdentityService>,
    Extension(newsroom): Extension<NewsroomService>,
    Path(desk_id): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<impl IntoResponse> {
    actor_from_request(&identity, &headers).await?;
    newsroom.get_desk(desk_id).await.map_err(newsroom_error)?;
    let schedule = newsroom
        .get_schedule(desk_id)
        .await
        .map_err(newsroom_error)?;
    Ok(Json(schedule))
}

async fn set_schedule(
    AuthActor(actor): AuthActor,
    Extension(newsroom): Extension<NewsroomService>,
    Path(desk_id): Path<Uuid>,
    Json(payload): Json<ScheduleRequest>,
) -> ApiResult<impl IntoResponse> {
    let desk = newsroom.get_desk(desk_id).await.map_err(newsroom_error)?;
    let input = ScheduleInput {
        timezone: payload.timezone,
        hours: payload.hours,
        on_call_user_id: payload.on_call_user_id,
        notes: payload.notes,
    };
    let schedule = newsroom
        .set_schedule(&actor, &desk, &input)
        .await
        .map_err(newsroom_error)?;
    Ok(Json(schedule))
}

async fn list_desk_invitations(
    Extension(identity): Extension<IdentityService>,
    Extension(newsroom): Extension<NewsroomService>,
    Path(desk_id): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<impl IntoResponse> {
    let actor = actor_from_request(&identity, &headers).await?;
    let desk = newsroom.get_desk(desk_id).await.map_err(newsroom_error)?;
    let invitations = newsroom
        .invitations_for_desk(&actor, &desk)
        .await
        .map_err(newsroom_error)?;
    Ok(Json(invitations))
}

async fn create_invitation(
    AuthActor(actor): AuthActor,
    Extension(newsroom): Extension<NewsroomService>,
    Path(desk_id): Path<Uuid>,
    Json(payload): Json<InvitationRequest>,
) -> ApiResult<impl IntoResponse> {
    let desk = newsroom.get_desk(desk_id).await.map_err(newsroom_error)?;
    let invitation = newsroom
        .invite(
            &actor,
            &desk,
            payload.user_id,
            &payload.desk_role,
            payload.message.as_deref(),
        )
        .await
        .map_err(newsroom_error)?;
    Ok((StatusCode::CREATED, Json(invitation)))
}

async fn my_invitations(
    Extension(identity): Extension<IdentityService>,
    Extension(newsroom): Extension<NewsroomService>,
    headers: HeaderMap,
) -> ApiResult<impl IntoResponse> {
    let actor = actor_from_request(&identity, &headers).await?;
    let invitations = newsroom
        .pending_invitations_for(&actor)
        .await
        .map_err(newsroom_error)?;
    Ok(Json(invitations))
}

async fn accept_invitation(
    Extension(identity): Extension<IdentityService>,
    Extension(newsroom): Extension<NewsroomService>,
    Path(invitation_id): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<impl IntoResponse> {
    let actor = actor_from_request(&identity, &headers).await?;
    let invitation = newsroom
        .respond_to_invitation(&actor, invitation_id, true)
        .await
        .map_err(newsroom_error)?;
    Ok(Json(invitation))
}

async fn decline_invitation(
    Extension(identity): Extension<IdentityService>,
    Extension(newsroom): Extension<NewsroomService>,
    Path(invitation_id): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<impl IntoResponse> {
    let actor = actor_from_request(&identity, &headers).await?;
    let invitation = newsroom
        .respond_to_invitation(&actor, invitation_id, false)
        .await
        .map_err(newsroom_error)?;
    Ok(Json(invitation))
}

// --- Sector-application handlers --------------------------------------------

#[derive(serde::Deserialize)]
struct ApplyBody {
    #[serde(default)]
    role: Option<String>,
    #[serde(default)]
    message: Option<String>,
    #[serde(default)]
    kyc_documents: serde_json::Value,
}

#[derive(serde::Deserialize)]
struct ReviewBody {
    approve: bool,
    #[serde(default)]
    decision_note: Option<String>,
}

async fn sectors_directory(
    Extension(identity): Extension<IdentityService>,
    Extension(newsroom): Extension<NewsroomService>,
    headers: HeaderMap,
) -> ApiResult<impl IntoResponse> {
    // Any signed-in user may browse the directory to find sectors to apply to.
    actor_from_request(&identity, &headers).await?;
    let sectors = newsroom.sectors_directory().await.map_err(newsroom_error)?;
    Ok(Json(sectors))
}

async fn apply_to_sector(
    Extension(identity): Extension<IdentityService>,
    Extension(newsroom): Extension<NewsroomService>,
    Path(desk_id): Path<Uuid>,
    headers: HeaderMap,
    Json(body): Json<ApplyBody>,
) -> ApiResult<impl IntoResponse> {
    let actor = actor_from_request(&identity, &headers).await?;
    let role = body.role.as_deref().unwrap_or("reporter");
    let docs = if body.kyc_documents.is_null() {
        serde_json::json!([])
    } else {
        body.kyc_documents
    };
    let application = newsroom
        .apply_to_sector(&actor, desk_id, role, body.message.as_deref(), &docs)
        .await
        .map_err(newsroom_error)?;
    Ok((StatusCode::CREATED, Json(application)))
}

async fn my_applications(
    Extension(identity): Extension<IdentityService>,
    Extension(newsroom): Extension<NewsroomService>,
    headers: HeaderMap,
) -> ApiResult<impl IntoResponse> {
    let actor = actor_from_request(&identity, &headers).await?;
    let apps = newsroom.my_applications(&actor).await.map_err(newsroom_error)?;
    Ok(Json(apps))
}

async fn pending_applications(
    Extension(identity): Extension<IdentityService>,
    Extension(newsroom): Extension<NewsroomService>,
    headers: HeaderMap,
) -> ApiResult<impl IntoResponse> {
    let actor = actor_from_request(&identity, &headers).await?;
    let apps = newsroom.pending_applications(&actor).await.map_err(newsroom_error)?;
    Ok(Json(apps))
}

async fn review_application(
    Extension(identity): Extension<IdentityService>,
    Extension(newsroom): Extension<NewsroomService>,
    Path(application_id): Path<Uuid>,
    headers: HeaderMap,
    Json(body): Json<ReviewBody>,
) -> ApiResult<impl IntoResponse> {
    let actor = actor_from_request(&identity, &headers).await?;
    let application = newsroom
        .review_application(&actor, application_id, body.approve, body.decision_note.as_deref())
        .await
        .map_err(newsroom_error)?;
    Ok(Json(application))
}

/// `GET /sector-applications/{id}/certificate` — the post-quantum KYC
/// certificate, re-verified on the fly. Any signed-in user may check it; the
/// point is public verifiability, not a fresh secret. Returns 404 when the
/// application has no certificate yet (pending or rejected).
async fn application_certificate(
    Extension(identity): Extension<IdentityService>,
    Extension(newsroom): Extension<NewsroomService>,
    Path(application_id): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<impl IntoResponse> {
    actor_from_request(&identity, &headers).await?;
    let certificate = newsroom
        .certificate(application_id)
        .await
        .map_err(newsroom_error)?
        .ok_or((
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "detail": "This application has no verification certificate." })),
        ))?;
    Ok(Json(certificate))
}

// --- Feature-flag handlers --------------------------------------------------

async fn evaluate_flags(
    Extension(identity): Extension<IdentityService>,
    Extension(newsroom): Extension<NewsroomService>,
    headers: HeaderMap,
) -> ApiResult<impl IntoResponse> {
    let actor = actor_from_request(&identity, &headers).await?;
    let resolved = newsroom
        .evaluate_all_flags(&actor)
        .await
        .map_err(newsroom_error)?;
    let mut map = Map::new();
    for (key, value) in resolved {
        map.insert(key, Value::Bool(value));
    }
    Ok(Json(Value::Object(map)))
}

async fn list_flags(
    Extension(identity): Extension<IdentityService>,
    Extension(newsroom): Extension<NewsroomService>,
    headers: HeaderMap,
) -> ApiResult<impl IntoResponse> {
    let actor = actor_from_request(&identity, &headers).await?;
    let flags = newsroom
        .list_flag_definitions(&actor)
        .await
        .map_err(newsroom_error)?;
    Ok(Json(flags))
}

async fn create_flag(
    AuthActor(actor): AuthActor,
    Extension(newsroom): Extension<NewsroomService>,
    Json(payload): Json<FlagCreateRequest>,
) -> ApiResult<impl IntoResponse> {
    let input = FlagInput {
        description: payload.description,
        enabled: payload.enabled,
        rollout_percent: payload.rollout_percent,
        target_user_ids: payload.target_user_ids,
        target_desk_ids: payload.target_desk_ids,
        emergency_disabled: payload.emergency_disabled,
    };
    let flag = newsroom
        .create_flag(&actor, &payload.key, &input)
        .await
        .map_err(newsroom_error)?;
    Ok((StatusCode::CREATED, Json(flag)))
}

async fn emergency_disable_flag(
    Extension(identity): Extension<IdentityService>,
    Extension(newsroom): Extension<NewsroomService>,
    Path(key): Path<String>,
    headers: HeaderMap,
) -> ApiResult<impl IntoResponse> {
    let actor = actor_from_request(&identity, &headers).await?;
    let flag = newsroom
        .emergency_disable_flag(&actor, &key)
        .await
        .map_err(newsroom_error)?;
    Ok(Json(flag))
}
