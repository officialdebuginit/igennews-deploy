//! Outbound-webhook subscription management (design: docs/MARKET-RESEARCH-AND-GAPS.md §6).
//! Register endpoints, list them, delete them, and read the delivery log. All gated
//! on `webhooks.manage` inside the service methods.

use axum::{
    Extension, Json, Router,
    extract::{Path, Query},
    http::StatusCode,
    response::IntoResponse,
    routing::get,
};
use meridian_newsroom::{
    NewsroomService,
    webhooks::{EndpointInput, EndpointPatch},
};
use serde::Deserialize;
use uuid::Uuid;

use crate::newsroom_routes::{ApiResult, AuthActor, newsroom_error};

pub fn router() -> Router {
    Router::new()
        .route(
            "/api/v1/webhooks/endpoints",
            get(list_endpoints).post(create_endpoint),
        )
        .route(
            "/api/v1/webhooks/endpoints/{endpoint_id}",
            axum::routing::patch(update_endpoint).delete(delete_endpoint),
        )
        .route(
            "/api/v1/webhooks/endpoints/{endpoint_id}/rotate-secret",
            axum::routing::post(rotate_secret),
        )
        .route(
            "/api/v1/webhooks/endpoints/{endpoint_id}/test",
            axum::routing::post(test_endpoint),
        )
        .route("/api/v1/webhooks/deliveries", get(list_deliveries))
}

async fn create_endpoint(
    AuthActor(actor): AuthActor,
    Extension(newsroom): Extension<NewsroomService>,
    Json(input): Json<EndpointInput>,
) -> ApiResult<impl IntoResponse> {
    let created = newsroom
        .create_webhook_endpoint(&actor, &input)
        .await
        .map_err(newsroom_error)?;
    Ok((StatusCode::CREATED, Json(created)))
}

async fn list_endpoints(
    AuthActor(actor): AuthActor,
    Extension(newsroom): Extension<NewsroomService>,
) -> ApiResult<impl IntoResponse> {
    let endpoints = newsroom
        .list_webhook_endpoints(&actor)
        .await
        .map_err(newsroom_error)?;
    Ok(Json(endpoints))
}

async fn delete_endpoint(
    AuthActor(actor): AuthActor,
    Extension(newsroom): Extension<NewsroomService>,
    Path(endpoint_id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    newsroom
        .delete_webhook_endpoint(&actor, endpoint_id)
        .await
        .map_err(newsroom_error)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn update_endpoint(
    AuthActor(actor): AuthActor,
    Extension(newsroom): Extension<NewsroomService>,
    Path(endpoint_id): Path<Uuid>,
    Json(patch): Json<EndpointPatch>,
) -> ApiResult<impl IntoResponse> {
    let endpoint = newsroom
        .update_webhook_endpoint(&actor, endpoint_id, &patch)
        .await
        .map_err(newsroom_error)?;
    Ok(Json(endpoint))
}

async fn rotate_secret(
    AuthActor(actor): AuthActor,
    Extension(newsroom): Extension<NewsroomService>,
    Path(endpoint_id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let secret = newsroom
        .rotate_webhook_secret(&actor, endpoint_id)
        .await
        .map_err(newsroom_error)?;
    Ok(Json(serde_json::json!({ "secret": secret })))
}

async fn test_endpoint(
    AuthActor(actor): AuthActor,
    Extension(newsroom): Extension<NewsroomService>,
    Path(endpoint_id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    newsroom
        .send_webhook_test(&actor, endpoint_id)
        .await
        .map_err(newsroom_error)?;
    Ok(StatusCode::ACCEPTED)
}

#[derive(Deserialize)]
struct DeliveryQuery {
    #[serde(default = "default_limit")]
    limit: i64,
}
fn default_limit() -> i64 {
    100
}

async fn list_deliveries(
    AuthActor(actor): AuthActor,
    Extension(newsroom): Extension<NewsroomService>,
    Query(query): Query<DeliveryQuery>,
) -> ApiResult<impl IntoResponse> {
    let rows = newsroom
        .webhook_deliveries(&actor, query.limit)
        .await
        .map_err(newsroom_error)?;
    Ok(Json(rows))
}
