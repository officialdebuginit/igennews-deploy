//! Reader-subscription management (design: docs/MARKET-RESEARCH-AND-GAPS.md §7).
//! Admin CRUD gated on `subscriptions.manage`; a public paywall entitlement check.

use axum::{
    Extension, Json, Router,
    extract::{Path, Query},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
};
use meridian_newsroom::{NewsroomService, subscriptions::SubscriptionInput};
use serde::Deserialize;
use uuid::Uuid;

use crate::newsroom_routes::{ApiResult, AuthActor, newsroom_error};

pub fn router() -> Router {
    Router::new()
        .route(
            "/api/v1/subscriptions",
            get(list_subscriptions).post(create_subscription),
        )
        .route("/api/v1/subscriptions/{subscription_id}/cancel", post(cancel_subscription))
        // Public: a reader proves entitlement by their email (no auth).
        .route("/api/v1/paywall/entitlement", get(entitlement))
}

async fn create_subscription(
    AuthActor(actor): AuthActor,
    Extension(newsroom): Extension<NewsroomService>,
    Json(input): Json<SubscriptionInput>,
) -> ApiResult<impl IntoResponse> {
    let subscription = newsroom
        .create_subscription(&actor, &input)
        .await
        .map_err(newsroom_error)?;
    Ok((StatusCode::CREATED, Json(subscription)))
}

async fn list_subscriptions(
    AuthActor(actor): AuthActor,
    Extension(newsroom): Extension<NewsroomService>,
) -> ApiResult<impl IntoResponse> {
    let rows = newsroom.list_subscriptions(&actor).await.map_err(newsroom_error)?;
    Ok(Json(rows))
}

async fn cancel_subscription(
    AuthActor(actor): AuthActor,
    Extension(newsroom): Extension<NewsroomService>,
    Path(subscription_id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let subscription = newsroom
        .cancel_subscription(&actor, subscription_id)
        .await
        .map_err(newsroom_error)?;
    Ok(Json(subscription))
}

#[derive(Deserialize)]
struct EntitlementQuery {
    #[serde(default)]
    email: String,
}

async fn entitlement(
    Extension(newsroom): Extension<NewsroomService>,
    Query(query): Query<EntitlementQuery>,
) -> ApiResult<impl IntoResponse> {
    let entitled = newsroom.is_entitled(&query.email).await.map_err(newsroom_error)?;
    Ok(Json(serde_json::json!({ "entitled": entitled })))
}
