//! Module 10 compatibility REST routes: permissions/roles management and the
//! audit log. Ported from the legacy `permissions` router and `/audit`.

use axum::{
    Extension, Json, Router,
    extract::{Path, Query},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{get, post},
};
use meridian_identity::IdentityService;
use meridian_newsroom::{
    NewsroomService,
    admin::{AssignmentInput, AuditFilter, DelegationInput, PolicyInput, RoleInput},
    people::{NewUser, ProfileUpdate, UserUpdate},
};
use serde::Deserialize;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::newsroom_routes::{ApiResult, AuthActor, actor_from_request, newsroom_error};

pub fn router() -> Router {
    Router::new()
        .route("/api/v1/permissions/capabilities", get(list_capabilities))
        .route("/api/v1/permissions/simulate", post(simulate))
        .route(
            "/api/v1/users/{user_id}/effective-permissions",
            get(effective_permissions),
        )
        .route(
            "/api/v1/permission-policies",
            get(list_policies).post(upsert_policy),
        )
        .route(
            "/api/v1/permission-policies/{policy_id}",
            axum::routing::delete(delete_policy),
        )
        .route("/api/v1/roles", get(list_roles).post(create_role))
        .route(
            "/api/v1/roles/{key}",
            get(get_role).patch(update_role).delete(delete_role),
        )
        .route(
            "/api/v1/users/{user_id}/role-assignments",
            get(list_assignments).post(assign_role),
        )
        .route(
            "/api/v1/role-assignments/{assignment_id}",
            axum::routing::delete(revoke_assignment),
        )
        .route("/api/v1/delegations", get(list_delegations).post(create_delegation))
        .route(
            "/api/v1/delegations/{delegation_id}",
            axum::routing::delete(revoke_delegation),
        )
        .route("/api/v1/audit", get(list_audit))
        .route("/api/v1/users", get(list_users).post(create_user))
        .route("/api/v1/users/me", axum::routing::patch(update_own_profile))
        .route("/api/v1/users/{user_id}/profile", get(get_user_profile).patch(set_user_profile))
        .route("/api/v1/users/{user_id}/admin-overview", get(admin_overview))
        .route("/api/v1/users/{user_id}/account-action", post(account_action))
        .route("/api/v1/users/{user_id}/set-password", post(set_password))
        .route("/api/v1/users/{user_id}/editorial", get(user_editorial))
        .route(
            "/api/v1/users/{user_id}",
            get(get_user).patch(admin_update_user),
        )
        .route(
            "/api/v1/desks/{desk_id}/members",
            get(list_desk_members).post(add_desk_member),
        )
        .route(
            "/api/v1/desks/{desk_id}/members/{user_id}",
            axum::routing::delete(remove_desk_member),
        )
}

#[derive(Deserialize)]
struct UserListQuery {
    role: Option<String>,
}

async fn list_users(
    Extension(identity): Extension<IdentityService>,
    Extension(newsroom): Extension<NewsroomService>,
    Query(query): Query<UserListQuery>,
    headers: HeaderMap,
) -> ApiResult<impl IntoResponse> {
    let actor = actor_from_request(&identity, &headers).await?;
    let users = newsroom.list_users(&actor, query.role.as_deref()).await.map_err(newsroom_error)?;
    Ok(Json(users))
}

async fn get_user(
    Extension(identity): Extension<IdentityService>,
    Extension(newsroom): Extension<NewsroomService>,
    Path(user_id): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<impl IntoResponse> {
    let actor = actor_from_request(&identity, &headers).await?;
    let user = newsroom.get_user(&actor, user_id).await.map_err(newsroom_error)?;
    Ok(Json(user))
}

#[derive(Deserialize)]
struct UserCreateBody {
    email: String,
    handle: String,
    display_name: String,
    role: String,
    #[serde(default)]
    department: Option<String>,
    password: String,
}

async fn create_user(
    AuthActor(actor): AuthActor,
    Extension(newsroom): Extension<NewsroomService>,
    Json(body): Json<UserCreateBody>,
) -> ApiResult<impl IntoResponse> {
    if body.password.len() < 8 {
        return Err((
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(serde_json::json!({ "detail": "password must be at least 8 characters" })),
        ));
    }
    // Hash at the boundary; the newsroom crate never sees plaintext.
    let password_hash = meridian_identity::hash_password(&body.password).map_err(|_| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "detail": "Could not create account" })),
        )
    })?;
    let new_user = NewUser {
        email: body.email,
        handle: body.handle,
        display_name: body.display_name,
        role: body.role,
        department: body.department,
        password_hash,
    };
    let user = newsroom.create_user(&actor, &new_user).await.map_err(newsroom_error)?;
    Ok((StatusCode::CREATED, Json(user)))
}

/// `GET /users/{id}/profile` — a person's curated professional profile. Any
/// signed-in user may read it; a private profile hides its body from all but the
/// owner and admins.
async fn get_user_profile(
    Extension(identity): Extension<IdentityService>,
    Extension(newsroom): Extension<NewsroomService>,
    Path(user_id): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<impl IntoResponse> {
    let actor = actor_from_request(&identity, &headers).await?;
    let profile = newsroom.rich_profile(&actor, user_id).await.map_err(newsroom_error)?;
    Ok(Json(profile))
}

/// `GET /users/{id}/admin-overview` — analytics, activity, and session history
/// for one user. Admin only (enforced in the service layer).
async fn admin_overview(
    Extension(identity): Extension<IdentityService>,
    Extension(newsroom): Extension<NewsroomService>,
    Path(user_id): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<impl IntoResponse> {
    let actor = actor_from_request(&identity, &headers).await?;
    let overview = newsroom.admin_overview(&actor, user_id).await.map_err(newsroom_error)?;
    Ok(Json(overview))
}

#[derive(Deserialize)]
struct AccountActionBody {
    /// `"reset"` (password-reset email) or `"recovery"` (account-recovery email).
    action: String,
}

/// `POST /users/{id}/account-action` — admin triggers a password-reset or
/// recovery email for the user. The action is audited (so it lands on the user's
/// activity timeline) and, in local development with `MERIDIAN_EXPOSE_RESET_TOKENS=true`,
/// the reset token is echoed back for testing.
async fn account_action(
    Extension(identity): Extension<IdentityService>,
    Extension(newsroom): Extension<NewsroomService>,
    Path(user_id): Path<Uuid>,
    headers: HeaderMap,
    Json(body): Json<AccountActionBody>,
) -> ApiResult<impl IntoResponse> {
    let actor = actor_from_request(&identity, &headers).await?;
    let email = newsroom
        .admin_account_action(&actor, user_id, &body.action)
        .await
        .map_err(newsroom_error)?;
    let token = identity.request_password_reset(&email).await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "detail": e.to_string() })),
        )
    })?;
    let expose = std::env::var("MERIDIAN_EXPOSE_RESET_TOKENS").as_deref() == Ok("true");
    Ok(Json(serde_json::json!({
        "sent": true,
        "action": body.action,
        "email": email,
        "reset_token": if expose { token } else { None },
    })))
}

/// `GET /users/{id}/editorial` — stories and pitches under a user. Admin only.
async fn user_editorial(
    Extension(identity): Extension<IdentityService>,
    Extension(newsroom): Extension<NewsroomService>,
    Path(user_id): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<impl IntoResponse> {
    let actor = actor_from_request(&identity, &headers).await?;
    let items = newsroom.admin_editorial_items(&actor, user_id).await.map_err(newsroom_error)?;
    Ok(Json(items))
}

#[derive(Deserialize)]
struct SetPasswordBody {
    new_password: String,
}

/// `POST /users/{id}/set-password` — an admin directly sets a user's password.
/// Authorized and audited via the newsroom (admin only), then applied through the
/// identity service, which revokes the user's live sessions.
async fn set_password(
    Extension(identity): Extension<IdentityService>,
    Extension(newsroom): Extension<NewsroomService>,
    Path(user_id): Path<Uuid>,
    headers: HeaderMap,
    Json(body): Json<SetPasswordBody>,
) -> ApiResult<impl IntoResponse> {
    let actor = actor_from_request(&identity, &headers).await?;
    if body.new_password.chars().count() < 8 {
        return Err((
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(serde_json::json!({ "detail": "Password must be at least 8 characters." })),
        ));
    }
    // Authorize (admin) and audit; also confirms the user exists.
    let email = newsroom
        .admin_account_action(&actor, user_id, "password")
        .await
        .map_err(newsroom_error)?;
    identity.admin_set_password(user_id, &body.new_password).await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "detail": e.to_string() })),
        )
    })?;
    Ok(Json(serde_json::json!({ "updated": true, "email": email })))
}

/// `PATCH /users/{id}/profile` — replace the profile document. Owner or admin only.
async fn set_user_profile(
    Extension(identity): Extension<IdentityService>,
    Extension(newsroom): Extension<NewsroomService>,
    Path(user_id): Path<Uuid>,
    headers: HeaderMap,
    Json(body): Json<serde_json::Value>,
) -> ApiResult<impl IntoResponse> {
    let actor = actor_from_request(&identity, &headers).await?;
    let profile = newsroom.set_rich_profile(&actor, user_id, body).await.map_err(newsroom_error)?;
    Ok(Json(profile))
}

#[derive(Deserialize)]
struct ProfileBody {
    #[serde(default)]
    display_name: Option<String>,
    #[serde(default)]
    department: Option<String>,
}

async fn update_own_profile(
    AuthActor(actor): AuthActor,
    Extension(newsroom): Extension<NewsroomService>,
    Json(body): Json<ProfileBody>,
) -> ApiResult<impl IntoResponse> {
    let update = ProfileUpdate {
        display_name: body.display_name,
        department: body.department,
    };
    let user = newsroom.update_own_profile(&actor, &update).await.map_err(newsroom_error)?;
    Ok(Json(user))
}

#[derive(Deserialize)]
struct UserUpdateBody {
    #[serde(default)]
    display_name: Option<String>,
    #[serde(default)]
    role: Option<String>,
    #[serde(default)]
    department: Option<String>,
    #[serde(default)]
    is_active: Option<bool>,
}

async fn admin_update_user(
    AuthActor(actor): AuthActor,
    Extension(newsroom): Extension<NewsroomService>,
    Path(user_id): Path<Uuid>,
    Json(body): Json<UserUpdateBody>,
) -> ApiResult<impl IntoResponse> {
    let update = UserUpdate {
        display_name: body.display_name,
        role: body.role,
        department: body.department,
        is_active: body.is_active,
    };
    let user = newsroom.admin_update_user(&actor, user_id, &update).await.map_err(newsroom_error)?;
    Ok(Json(user))
}

async fn list_desk_members(
    Extension(identity): Extension<IdentityService>,
    Extension(newsroom): Extension<NewsroomService>,
    Path(desk_id): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<impl IntoResponse> {
    let actor = actor_from_request(&identity, &headers).await?;
    let members = newsroom.list_desk_members(&actor, desk_id).await.map_err(newsroom_error)?;
    Ok(Json(members))
}

#[derive(Deserialize)]
struct DeskMemberBody {
    user_id: Uuid,
    #[serde(default = "member")]
    role: String,
}
/// Default workspace role for a membership granted without one.
///
/// The second of two places that still said `member` after migration `0016`
/// narrowed the column to eight workspace roles and set its default to `viewer`.
/// Both are now `viewer` — the safe choice `0016` made deliberately, since a
/// membership granted without a stated role should not carry edit rights.
fn member() -> String {
    "viewer".to_owned()
}

async fn add_desk_member(
    AuthActor(actor): AuthActor,
    Extension(newsroom): Extension<NewsroomService>,
    Path(desk_id): Path<Uuid>,
    Json(body): Json<DeskMemberBody>,
) -> ApiResult<impl IntoResponse> {
    newsroom
        .add_desk_member(&actor, desk_id, body.user_id, &body.role)
        .await
        .map_err(newsroom_error)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn remove_desk_member(
    Extension(identity): Extension<IdentityService>,
    Extension(newsroom): Extension<NewsroomService>,
    Path((desk_id, user_id)): Path<(Uuid, Uuid)>,
    headers: HeaderMap,
) -> ApiResult<impl IntoResponse> {
    let actor = actor_from_request(&identity, &headers).await?;
    newsroom.remove_desk_member(&actor, desk_id, user_id).await.map_err(newsroom_error)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn list_capabilities(
    Extension(identity): Extension<IdentityService>,
    Extension(newsroom): Extension<NewsroomService>,
    headers: HeaderMap,
) -> ApiResult<impl IntoResponse> {
    actor_from_request(&identity, &headers).await?;
    Ok(Json(newsroom.list_capabilities()))
}

#[derive(Deserialize)]
struct SimulateBody {
    user_id: Uuid,
    capability: String,
    #[serde(default)]
    desk_id: Option<Uuid>,
}

async fn simulate(
    AuthActor(actor): AuthActor,
    Extension(newsroom): Extension<NewsroomService>,
    Json(body): Json<SimulateBody>,
) -> ApiResult<impl IntoResponse> {
    let decision = newsroom
        .simulate(&actor, body.user_id, &body.capability, body.desk_id)
        .await
        .map_err(newsroom_error)?;
    Ok(Json(decision))
}

/// `desk_id` scopes the answer to one sector. Absent, the viewer answers at org
/// level, which is the legacy behaviour and keeps the response shape unchanged.
#[derive(Deserialize)]
struct EffectivePermissionsQuery {
    desk_id: Option<Uuid>,
}

async fn effective_permissions(
    Extension(identity): Extension<IdentityService>,
    Extension(newsroom): Extension<NewsroomService>,
    Path(user_id): Path<Uuid>,
    Query(query): Query<EffectivePermissionsQuery>,
    headers: HeaderMap,
) -> ApiResult<impl IntoResponse> {
    let actor = actor_from_request(&identity, &headers).await?;
    // Resolve permissions first: this enforces self-or-`permissions.manage`, so a
    // forbidden probe never even returns the target's identity envelope below.
    let decisions = newsroom
        .effective_permissions(&actor, user_id, query.desk_id)
        .await
        .map_err(newsroom_error)?;
    let user = newsroom.get_user(&actor, user_id).await.map_err(newsroom_error)?;
    // Legacy wraps the capability decisions with the subject's identity.
    Ok(Json(serde_json::json!({
        "user_id": user.id,
        "display_name": user.display_name,
        "role": user.role,
        "is_admin": user.is_admin,
        "permissions": decisions,
    })))
}

async fn list_policies(
    Extension(identity): Extension<IdentityService>,
    Extension(newsroom): Extension<NewsroomService>,
    headers: HeaderMap,
) -> ApiResult<impl IntoResponse> {
    let actor = actor_from_request(&identity, &headers).await?;
    let policies = newsroom.list_policies(&actor).await.map_err(newsroom_error)?;
    Ok(Json(policies))
}

#[derive(Deserialize)]
struct PolicyBody {
    subject_type: String,
    subject_id: Uuid,
    capability: String,
    #[serde(default = "yes")]
    allow: bool,
    #[serde(default, with = "time::serde::rfc3339::option")]
    expires_at: Option<OffsetDateTime>,
    #[serde(default)]
    reason: Option<String>,
}
fn yes() -> bool {
    true
}

async fn upsert_policy(
    AuthActor(actor): AuthActor,
    Extension(newsroom): Extension<NewsroomService>,
    Json(body): Json<PolicyBody>,
) -> ApiResult<impl IntoResponse> {
    let input = PolicyInput {
        subject_type: body.subject_type,
        subject_id: body.subject_id,
        capability: body.capability,
        allow: body.allow,
        expires_at: body.expires_at,
        reason: body.reason,
    };
    let policy = newsroom.upsert_policy(&actor, &input).await.map_err(newsroom_error)?;
    Ok((StatusCode::CREATED, Json(policy)))
}

async fn delete_policy(
    Extension(identity): Extension<IdentityService>,
    Extension(newsroom): Extension<NewsroomService>,
    Path(policy_id): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<impl IntoResponse> {
    let actor = actor_from_request(&identity, &headers).await?;
    newsroom.delete_policy(&actor, policy_id).await.map_err(newsroom_error)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn list_roles(
    Extension(identity): Extension<IdentityService>,
    Extension(newsroom): Extension<NewsroomService>,
    headers: HeaderMap,
) -> ApiResult<impl IntoResponse> {
    actor_from_request(&identity, &headers).await?;
    let roles = newsroom.list_roles().await.map_err(newsroom_error)?;
    Ok(Json(roles))
}

#[derive(Deserialize)]
struct RoleBody {
    key: String,
    name: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    capabilities: Vec<String>,
}

async fn create_role(
    AuthActor(actor): AuthActor,
    Extension(newsroom): Extension<NewsroomService>,
    Json(body): Json<RoleBody>,
) -> ApiResult<impl IntoResponse> {
    let input = RoleInput {
        key: body.key,
        name: body.name,
        description: body.description,
        capabilities: body.capabilities,
    };
    let role = newsroom.create_role(&actor, &input).await.map_err(newsroom_error)?;
    Ok((StatusCode::CREATED, Json(role)))
}

async fn get_role(
    _: AuthActor,
    Extension(newsroom): Extension<NewsroomService>,
    Path(key): Path<String>,
) -> ApiResult<impl IntoResponse> {
    let role = newsroom.get_role(&key).await.map_err(newsroom_error)?;
    Ok(Json(role))
}

#[derive(Deserialize)]
struct RolePatchBody {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    capabilities: Option<Vec<String>>,
}

async fn update_role(
    AuthActor(actor): AuthActor,
    Extension(newsroom): Extension<NewsroomService>,
    Path(key): Path<String>,
    Json(body): Json<RolePatchBody>,
) -> ApiResult<impl IntoResponse> {
    let role = newsroom
        .update_role(
            &actor,
            &key,
            body.name.as_deref(),
            body.description.as_deref(),
            body.capabilities.as_deref(),
        )
        .await
        .map_err(newsroom_error)?;
    Ok(Json(role))
}

async fn delete_role(
    Extension(identity): Extension<IdentityService>,
    Extension(newsroom): Extension<NewsroomService>,
    Path(key): Path<String>,
    headers: HeaderMap,
) -> ApiResult<impl IntoResponse> {
    let actor = actor_from_request(&identity, &headers).await?;
    newsroom.delete_role(&actor, &key).await.map_err(newsroom_error)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn list_assignments(
    Extension(identity): Extension<IdentityService>,
    Extension(newsroom): Extension<NewsroomService>,
    Path(user_id): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<impl IntoResponse> {
    let actor = actor_from_request(&identity, &headers).await?;
    let assignments = newsroom.list_assignments(&actor, user_id).await.map_err(newsroom_error)?;
    Ok(Json(assignments))
}

#[derive(Deserialize)]
struct AssignmentBody {
    role_key: String,
    #[serde(default = "global")]
    scope_type: String,
    #[serde(default)]
    scope_id: Option<Uuid>,
    #[serde(default, with = "time::serde::rfc3339::option")]
    ends_at: Option<OffsetDateTime>,
    #[serde(default)]
    reason: Option<String>,
}
fn global() -> String {
    "global".to_owned()
}

async fn assign_role(
    AuthActor(actor): AuthActor,
    Extension(newsroom): Extension<NewsroomService>,
    Path(user_id): Path<Uuid>,
    Json(body): Json<AssignmentBody>,
) -> ApiResult<impl IntoResponse> {
    let input = AssignmentInput {
        role_key: body.role_key,
        scope_type: body.scope_type,
        scope_id: body.scope_id,
        ends_at: body.ends_at,
        reason: body.reason,
    };
    let assignment = newsroom.assign_role(&actor, user_id, &input).await.map_err(newsroom_error)?;
    Ok((StatusCode::CREATED, Json(assignment)))
}

async fn revoke_assignment(
    Extension(identity): Extension<IdentityService>,
    Extension(newsroom): Extension<NewsroomService>,
    Path(assignment_id): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<impl IntoResponse> {
    let actor = actor_from_request(&identity, &headers).await?;
    newsroom.revoke_assignment(&actor, assignment_id).await.map_err(newsroom_error)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn list_delegations(
    Extension(identity): Extension<IdentityService>,
    Extension(newsroom): Extension<NewsroomService>,
    headers: HeaderMap,
) -> ApiResult<impl IntoResponse> {
    let actor = actor_from_request(&identity, &headers).await?;
    let delegations = newsroom.list_delegations(&actor).await.map_err(newsroom_error)?;
    Ok(Json(delegations))
}

#[derive(Deserialize)]
struct DelegationBody {
    to_user_id: Uuid,
    #[serde(default)]
    capabilities: Vec<String>,
    #[serde(with = "time::serde::rfc3339")]
    ends_at: OffsetDateTime,
    #[serde(default)]
    reason: Option<String>,
}

async fn create_delegation(
    AuthActor(actor): AuthActor,
    Extension(newsroom): Extension<NewsroomService>,
    Json(body): Json<DelegationBody>,
) -> ApiResult<impl IntoResponse> {
    let input = DelegationInput {
        to_user_id: body.to_user_id,
        capabilities: body.capabilities,
        ends_at: body.ends_at,
        reason: body.reason,
    };
    let delegation = newsroom.create_delegation(&actor, &input).await.map_err(newsroom_error)?;
    Ok((StatusCode::CREATED, Json(delegation)))
}

async fn revoke_delegation(
    Extension(identity): Extension<IdentityService>,
    Extension(newsroom): Extension<NewsroomService>,
    Path(delegation_id): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<impl IntoResponse> {
    let actor = actor_from_request(&identity, &headers).await?;
    newsroom.revoke_delegation(&actor, delegation_id).await.map_err(newsroom_error)?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct AuditQuery {
    entity_type: Option<String>,
    action: Option<String>,
    actor_id: Option<Uuid>,
    #[serde(default = "audit_limit")]
    limit: i64,
}
fn audit_limit() -> i64 {
    50
}

async fn list_audit(
    Extension(identity): Extension<IdentityService>,
    Extension(newsroom): Extension<NewsroomService>,
    Query(query): Query<AuditQuery>,
    headers: HeaderMap,
) -> ApiResult<impl IntoResponse> {
    let actor = actor_from_request(&identity, &headers).await?;
    let filter = AuditFilter {
        entity_type: query.entity_type,
        action: query.action,
        actor_id: query.actor_id,
    };
    let records = newsroom.list_audit(&actor, &filter, query.limit).await.map_err(newsroom_error)?;
    // Legacy returns the audit records as a bare array.
    Ok(Json(records))
}
