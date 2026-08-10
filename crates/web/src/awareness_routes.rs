//! Module 7 compatibility REST routes: notifications, preferences, follows,
//! favourites, recents, and the composite navigation.
//!
//! The composite `GET /navigation` now returns items, actions, favourites,
//! recents, and the unread-notification badge. The approvals/reminders/mentions
//! badges come from the activity centre (still deferred within Module 7) and are
//! intentionally omitted rather than faked.

use axum::{
    Extension, Json, Router,
    extract::{Path, Query},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{get, post},
};
use meridian_identity::IdentityService;
use meridian_newsroom::{
    NewsroomService, awareness::PreferenceInput, dashviews::DashboardViewInput, feed::FeedPostDraft,
    search::{SavedSearchInput, SearchFilters},
};
use uuid::Uuid;
use serde::Deserialize;
use serde_json::json;

use crate::newsroom_routes::{ApiResult, AuthActor, actor_from_request, newsroom_error};
use crate::realtime::RealtimeHub;

pub fn router() -> Router {
    Router::new()
        .route("/api/v1/notifications", get(list_notifications))
        .route("/api/v1/notifications/{notification_id}/read", post(mark_read))
        .route("/api/v1/notifications/read-all", post(mark_all_read))
        .route(
            "/api/v1/notifications/preferences",
            get(get_preferences).put(set_preferences),
        )
        .route("/api/v1/follows", get(list_follows).post(add_follow))
        .route(
            "/api/v1/follows/{entity_type}/{entity_id}",
            axum::routing::delete(remove_follow),
        )
        .route("/api/v1/favorites", get(list_favorites).post(add_favorite))
        .route(
            "/api/v1/favorites/{entity_type}/{entity_id}",
            axum::routing::delete(remove_favorite),
        )
        .route("/api/v1/recents", get(list_recents).post(record_recent))
        .route("/api/v1/navigation", get(navigation))
        .route("/api/v1/activity-center", get(activity_center))
        .route("/api/v1/notifications/process-escalations", post(process_escalations))
        .route("/api/v1/dashboard/pipeline", get(dashboard_pipeline))
        .route("/api/v1/dashboard/workload", get(dashboard_workload))
        .route("/api/v1/dashboard", get(dashboard_home))
        .route("/api/v1/dashboard/summary", get(dashboard_summary))
        .route("/api/v1/dashboard/activity", get(dashboard_activity))
        .route("/api/v1/dashboard/releases", get(dashboard_releases))
        .route(
            "/api/v1/dashboard/layout",
            get(dashboard_layout),
        )
        .route("/api/v1/dashboard/layout/reset", post(reset_dashboard_layout))
        .route("/api/v1/dashboard/attention", get(dashboard_attention))
        .route("/api/v1/dashboard/metrics/capture", post(capture_metrics))
        .route("/api/v1/dashboard/metrics/series", get(metric_series))
        .route("/api/v1/dashboard/drilldown/{key}", get(drilldown))
        .route(
            "/api/v1/dashboard/views",
            get(list_dashboard_views).post(create_dashboard_view),
        )
        .route(
            "/api/v1/dashboard/views/{view_id}",
            get(get_dashboard_view)
                .patch(update_dashboard_view)
                .delete(delete_dashboard_view),
        )
        .route(
            "/api/v1/dashboard/views/{view_id}/set-default",
            post(set_default_dashboard_view),
        )
        .route(
            "/api/v1/attention/{fingerprint}/acknowledge",
            post(acknowledge_attention).delete(unacknowledge_attention),
        )
        .route(
            "/api/v1/attention/{fingerprint}/snooze",
            post(snooze_attention).delete(unsnooze_attention),
        )
        .route("/api/v1/attention/{fingerprint}/assign", post(assign_attention))
        .route("/api/v1/attention/{fingerprint}/escalate", post(escalate_attention))
        .route(
            "/api/v1/dashboard/views/{view_id}/duplicate",
            post(duplicate_dashboard_view),
        )
        // Beyond the legacy surface: presence had no store and no endpoint.
        .route("/api/v1/presence", get(list_presence).post(touch_presence))
        .route("/api/v1/search", get(search))
        .route("/api/v1/search/corpus", get(search_corpus))
        .route("/api/v1/search/facets", get(search_facets))
        .route("/api/v1/search/reindex", post(search_reindex))
        .route(
            "/api/v1/saved-searches",
            get(list_saved_searches).post(create_saved_search),
        )
        .route(
            "/api/v1/saved-searches/{search_id}",
            axum::routing::delete(delete_saved_search),
        )
        .route("/api/v1/feed", get(list_feed).post(create_feed_post))
        .route("/api/v1/feed/replies/{reply_id}", axum::routing::delete(delete_feed_reply))
        .route("/api/v1/feed/{post_id}", axum::routing::delete(delete_feed_post))
        .route("/api/v1/feed/{post_id}/like", post(like_feed_post))
        .route("/api/v1/feed/{post_id}/repost", post(repost_feed_post))
        .route(
            "/api/v1/feed/{post_id}/replies",
            get(list_feed_replies).post(create_feed_reply),
        )
}

#[derive(Deserialize)]
struct FeedQuery {
    kind: Option<String>,
    #[serde(default = "feed_limit")]
    limit: i64,
}
fn feed_limit() -> i64 {
    100
}

async fn list_feed(
    Extension(identity): Extension<IdentityService>,
    Extension(newsroom): Extension<NewsroomService>,
    Query(query): Query<FeedQuery>,
    headers: HeaderMap,
) -> ApiResult<impl IntoResponse> {
    let actor = actor_from_request(&identity, &headers).await?;
    let posts = newsroom
        .list_feed(&actor, query.kind.as_deref(), query.limit)
        .await
        .map_err(newsroom_error)?;
    Ok(Json(posts))
}

#[derive(Deserialize)]
struct FeedPostBody {
    #[serde(default = "note")]
    kind: String,
    content: String,
    #[serde(default)]
    story_id: Option<uuid::Uuid>,
    #[serde(default)]
    asset_id: Option<uuid::Uuid>,
}
fn note() -> String {
    "note".to_owned()
}

async fn create_feed_post(
    AuthActor(actor): AuthActor,
    Extension(newsroom): Extension<NewsroomService>,
    Extension(hub): Extension<RealtimeHub>,
    Json(body): Json<FeedPostBody>,
) -> ApiResult<impl IntoResponse> {
    let draft = FeedPostDraft {
        kind: body.kind,
        content: body.content,
        story_id: body.story_id,
        asset_id: body.asset_id,
    };
    let post = newsroom.create_feed_post(&actor, &draft).await.map_err(newsroom_error)?;
    hub.emit("feed");
    Ok((StatusCode::CREATED, Json(post)))
}

async fn like_feed_post(
    Extension(identity): Extension<IdentityService>,
    Extension(newsroom): Extension<NewsroomService>,
    Extension(hub): Extension<RealtimeHub>,
    Path(post_id): Path<uuid::Uuid>,
    headers: HeaderMap,
) -> ApiResult<impl IntoResponse> {
    let actor = actor_from_request(&identity, &headers).await?;
    let post = newsroom.toggle_feed_like(&actor, post_id).await.map_err(newsroom_error)?;
    hub.emit("feed");
    Ok(Json(post))
}

async fn repost_feed_post(
    Extension(identity): Extension<IdentityService>,
    Extension(newsroom): Extension<NewsroomService>,
    Extension(hub): Extension<RealtimeHub>,
    Path(post_id): Path<uuid::Uuid>,
    headers: HeaderMap,
) -> ApiResult<impl IntoResponse> {
    let actor = actor_from_request(&identity, &headers).await?;
    let post = newsroom.toggle_feed_repost(&actor, post_id).await.map_err(newsroom_error)?;
    hub.emit("feed");
    Ok(Json(post))
}

async fn list_feed_replies(
    Extension(identity): Extension<IdentityService>,
    Extension(newsroom): Extension<NewsroomService>,
    Path(post_id): Path<uuid::Uuid>,
    headers: HeaderMap,
) -> ApiResult<impl IntoResponse> {
    actor_from_request(&identity, &headers).await?;
    let replies = newsroom.list_feed_replies(post_id).await.map_err(newsroom_error)?;
    Ok(Json(replies))
}

#[derive(Deserialize)]
struct FeedReplyBody {
    body: String,
}

async fn create_feed_reply(
    AuthActor(actor): AuthActor,
    Extension(newsroom): Extension<NewsroomService>,
    Extension(hub): Extension<RealtimeHub>,
    Path(post_id): Path<uuid::Uuid>,
    Json(body): Json<FeedReplyBody>,
) -> ApiResult<impl IntoResponse> {
    let reply = newsroom
        .create_feed_reply(&actor, post_id, &body.body)
        .await
        .map_err(newsroom_error)?;
    hub.emit("feed");
    Ok((StatusCode::CREATED, Json(reply)))
}

async fn delete_feed_reply(
    Extension(identity): Extension<IdentityService>,
    Extension(newsroom): Extension<NewsroomService>,
    Extension(hub): Extension<RealtimeHub>,
    Path(reply_id): Path<uuid::Uuid>,
    headers: HeaderMap,
) -> ApiResult<impl IntoResponse> {
    let actor = actor_from_request(&identity, &headers).await?;
    newsroom.delete_feed_reply(&actor, reply_id).await.map_err(newsroom_error)?;
    hub.emit("feed");
    Ok(StatusCode::NO_CONTENT)
}

async fn delete_feed_post(
    Extension(identity): Extension<IdentityService>,
    Extension(newsroom): Extension<NewsroomService>,
    Extension(hub): Extension<RealtimeHub>,
    Path(post_id): Path<uuid::Uuid>,
    headers: HeaderMap,
) -> ApiResult<impl IntoResponse> {
    let actor = actor_from_request(&identity, &headers).await?;
    newsroom.delete_feed_post(&actor, post_id).await.map_err(newsroom_error)?;
    hub.emit("feed");
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct SearchQuery {
    #[serde(default, alias = "query")]
    q: String,
    #[serde(default = "search_limit")]
    limit: i64,
    #[serde(default)]
    offset: i64,
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    category: Option<String>,
    #[serde(default)]
    desk: Option<Uuid>,
    /// Inclusive `updated_at` lower bound, `YYYY-MM-DD` (operator's intent; UTC here).
    #[serde(default)]
    since: Option<String>,
    /// Exclusive `updated_at` upper bound, `YYYY-MM-DD` (the whole day is included).
    #[serde(default)]
    until: Option<String>,
}
fn search_limit() -> i64 {
    50
}

/// Parses a `YYYY-MM-DD` day to a UTC midnight instant; `end` advances one day so
/// an `until` bound covers the whole named day. `None` for anything unparseable.
fn parse_day(value: &str, end: bool) -> Option<time::OffsetDateTime> {
    let mut parts = value.split('-');
    let year: i32 = parts.next()?.parse().ok()?;
    let month: u8 = parts.next()?.parse().ok()?;
    let day: u8 = parts.next()?.parse().ok()?;
    let date = time::Date::from_calendar_date(year, time::Month::try_from(month).ok()?, day).ok()?;
    let instant = date.midnight().assume_utc();
    Some(if end { instant + time::Duration::days(1) } else { instant })
}

/// A blank querystring value (`?state=`) means "no filter", not "match empty".
fn blank_to_none(value: Option<String>) -> Option<String> {
    value.filter(|s| !s.trim().is_empty())
}

async fn search(
    Extension(identity): Extension<IdentityService>,
    Extension(newsroom): Extension<NewsroomService>,
    Query(query): Query<SearchQuery>,
    headers: HeaderMap,
) -> ApiResult<impl IntoResponse> {
    let actor = actor_from_request(&identity, &headers).await?;
    let filters = SearchFilters {
        workflow_state: blank_to_none(query.state),
        category: blank_to_none(query.category),
        desk_id: query.desk,
        since: query.since.as_deref().and_then(|d| parse_day(d, false)),
        until: query.until.as_deref().and_then(|d| parse_day(d, true)),
        limit: query.limit,
        offset: query.offset.max(0),
    };
    let page = newsroom
        .search_stories_filtered(&actor, &query.q, &filters)
        .await
        .map_err(newsroom_error)?;
    let returned = page.hits.len() as i64;
    Ok(Json(json!({
        "query": query.q,
        "results": page.hits,
        "total": page.total,
        "total_returned": returned,
        "offset": filters.offset,
        "limit": filters.limit.clamp(1, 100),
        "has_more": filters.offset + returned < page.total,
        "facets": page.facets,
    })))
}

#[derive(Deserialize)]
struct CorpusQuery {
    #[serde(default)]
    kind: String,
    #[serde(default, alias = "query")]
    q: String,
    #[serde(default = "search_limit")]
    limit: i64,
    #[serde(default)]
    offset: i64,
    #[serde(default)]
    desk: Option<Uuid>,
    #[serde(default)]
    since: Option<String>,
    #[serde(default)]
    until: Option<String>,
}

async fn search_corpus(
    Extension(identity): Extension<IdentityService>,
    Extension(newsroom): Extension<NewsroomService>,
    Query(query): Query<CorpusQuery>,
    headers: HeaderMap,
) -> ApiResult<impl IntoResponse> {
    let actor = actor_from_request(&identity, &headers).await?;
    let offset = query.offset.max(0);
    let (hits, total) = newsroom
        .search_corpus(
            &actor,
            &query.kind,
            &query.q,
            query.desk,
            query.since.as_deref().and_then(|d| parse_day(d, false)),
            query.until.as_deref().and_then(|d| parse_day(d, true)),
            query.limit,
            offset,
        )
        .await
        .map_err(newsroom_error)?;
    let returned = hits.len() as i64;
    Ok(Json(json!({
        "kind": query.kind,
        "results": hits,
        "total": total,
        "offset": offset,
        "has_more": offset + returned < total,
    })))
}

async fn search_facets(
    Extension(identity): Extension<IdentityService>,
    Extension(newsroom): Extension<NewsroomService>,
    headers: HeaderMap,
) -> ApiResult<impl IntoResponse> {
    actor_from_request(&identity, &headers).await?;
    let facets = newsroom.search_facets().await.map_err(newsroom_error)?;
    Ok(Json(facets))
}

async fn search_reindex(
    Extension(identity): Extension<IdentityService>,
    headers: HeaderMap,
) -> ApiResult<impl IntoResponse> {
    actor_from_request(&identity, &headers).await?;
    // The story search vector is a generated column kept current by PostgreSQL,
    // so there is nothing to rebuild — no rows are (re)indexed. Legacy returns
    // `{indexed: <count>}`; we match that shape with a zero-work count.
    Ok(Json(json!({ "indexed": 0 })))
}

#[derive(Deserialize)]
struct PresenceQuery {
    entity_type: String,
    entity_id: String,
}

/// `GET /api/v1/presence?entity_type=&entity_id=` — who else has it open.
async fn list_presence(
    Extension(identity): Extension<IdentityService>,
    Extension(newsroom): Extension<NewsroomService>,
    Query(query): Query<PresenceQuery>,
    headers: HeaderMap,
) -> ApiResult<impl IntoResponse> {
    let actor = actor_from_request(&identity, &headers).await?;
    let viewers = newsroom
        .presence_on(&actor, &query.entity_type, &query.entity_id)
        .await
        .map_err(newsroom_error)?;
    Ok(Json(viewers))
}

/// `POST /api/v1/presence` — the heartbeat. Returns the other viewers in the same
/// round trip, so a client needs one request per beat rather than two.
async fn touch_presence(
    AuthActor(actor): AuthActor,
    Extension(newsroom): Extension<NewsroomService>,
    Json(body): Json<PresenceQuery>,
) -> ApiResult<impl IntoResponse> {
    newsroom
        .touch_presence(&actor, &body.entity_type, &body.entity_id)
        .await
        .map_err(newsroom_error)?;
    let viewers = newsroom
        .presence_on(&actor, &body.entity_type, &body.entity_id)
        .await
        .map_err(newsroom_error)?;
    Ok(Json(viewers))
}

async fn list_saved_searches(
    Extension(identity): Extension<IdentityService>,
    Extension(newsroom): Extension<NewsroomService>,
    headers: HeaderMap,
) -> ApiResult<impl IntoResponse> {
    let actor = actor_from_request(&identity, &headers).await?;
    let searches = newsroom.list_saved_searches(&actor).await.map_err(newsroom_error)?;
    Ok(Json(searches))
}

#[derive(Deserialize)]
struct SavedSearchBody {
    name: String,
    #[serde(default)]
    query: String,
    #[serde(default = "empty_object")]
    filters: serde_json::Value,
    #[serde(default)]
    is_shared: bool,
}
fn empty_object() -> serde_json::Value {
    serde_json::Value::Object(serde_json::Map::new())
}

async fn create_saved_search(
    AuthActor(actor): AuthActor,
    Extension(newsroom): Extension<NewsroomService>,
    Json(body): Json<SavedSearchBody>,
) -> ApiResult<impl IntoResponse> {
    let input = SavedSearchInput {
        name: body.name,
        query: body.query,
        filters: body.filters,
        is_shared: body.is_shared,
    };
    let saved = newsroom.create_saved_search(&actor, &input).await.map_err(newsroom_error)?;
    Ok((StatusCode::CREATED, Json(saved)))
}

async fn delete_saved_search(
    Extension(identity): Extension<IdentityService>,
    Extension(newsroom): Extension<NewsroomService>,
    Path(search_id): Path<uuid::Uuid>,
    headers: HeaderMap,
) -> ApiResult<impl IntoResponse> {
    let actor = actor_from_request(&identity, &headers).await?;
    newsroom.delete_saved_search(&actor, search_id).await.map_err(newsroom_error)?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct DashboardScope {
    desk_id: Option<uuid::Uuid>,
    owner_id: Option<uuid::Uuid>,
}

async fn dashboard_pipeline(
    Extension(identity): Extension<IdentityService>,
    Extension(newsroom): Extension<NewsroomService>,
    Query(scope): Query<DashboardScope>,
    headers: HeaderMap,
) -> ApiResult<impl IntoResponse> {
    let actor = actor_from_request(&identity, &headers).await?;
    let pipeline = newsroom
        .dashboard_pipeline(&actor, scope.desk_id, scope.owner_id)
        .await
        .map_err(newsroom_error)?;
    Ok(Json(pipeline))
}

async fn dashboard_workload(
    Extension(identity): Extension<IdentityService>,
    Extension(newsroom): Extension<NewsroomService>,
    Query(scope): Query<DashboardScope>,
    headers: HeaderMap,
) -> ApiResult<impl IntoResponse> {
    let actor = actor_from_request(&identity, &headers).await?;
    let workload = newsroom.dashboard_workload(&actor, scope.desk_id).await.map_err(newsroom_error)?;
    Ok(Json(workload))
}

#[derive(Deserialize)]
struct AttentionQuery {
    desk_id: Option<uuid::Uuid>,
    owner_id: Option<uuid::Uuid>,
    #[serde(default)]
    include_snoozed: bool,
}

async fn dashboard_attention(
    Extension(identity): Extension<IdentityService>,
    Extension(newsroom): Extension<NewsroomService>,
    Query(query): Query<AttentionQuery>,
    headers: HeaderMap,
) -> ApiResult<impl IntoResponse> {
    let actor = actor_from_request(&identity, &headers).await?;
    let items = newsroom
        .attention_queue(&actor, query.desk_id, query.owner_id, query.include_snoozed)
        .await
        .map_err(newsroom_error)?;
    // Legacy wraps the queue with a count, a timestamp, and the caller's permitted
    // triage actions. The action grants approximate the legacy capability checks
    // via the admin flag pending the finer per-capability port.
    let can = actor.is_admin;
    Ok(Json(json!({
        "items": items,
        "total": items.len(),
        "generated_at": meridian_newsroom::rfc3339(time::OffsetDateTime::now_utc()),
        "permitted_actions": {
            "acknowledge": can, "assign": can, "escalate": can, "snooze": can,
        },
    })))
}

async fn dashboard_summary(
    Extension(identity): Extension<IdentityService>,
    Extension(newsroom): Extension<NewsroomService>,
    Query(scope): Query<DashboardScope>,
    headers: HeaderMap,
) -> ApiResult<impl IntoResponse> {
    let actor = actor_from_request(&identity, &headers).await?;
    let summary = newsroom
        .dashboard_summary(&actor, scope.desk_id, scope.owner_id)
        .await
        .map_err(newsroom_error)?;
    Ok(Json(summary))
}

async fn dashboard_home(
    Extension(identity): Extension<IdentityService>,
    Extension(newsroom): Extension<NewsroomService>,
    Query(scope): Query<DashboardScope>,
    headers: HeaderMap,
) -> ApiResult<impl IntoResponse> {
    let actor = actor_from_request(&identity, &headers).await?;
    let home = newsroom
        .dashboard_home(&actor, scope.desk_id, scope.owner_id)
        .await
        .map_err(newsroom_error)?;
    Ok(Json(home))
}

#[derive(Deserialize)]
struct ActivityQuery {
    entity_type: Option<String>,
    actor_id: Option<uuid::Uuid>,
    #[serde(default = "activity_limit")]
    limit: i64,
}
fn activity_limit() -> i64 {
    50
}

async fn dashboard_activity(
    Extension(identity): Extension<IdentityService>,
    Extension(newsroom): Extension<NewsroomService>,
    Query(query): Query<ActivityQuery>,
    headers: HeaderMap,
) -> ApiResult<impl IntoResponse> {
    let actor = actor_from_request(&identity, &headers).await?;
    // Legacy narrows non-broad-view callers to their own activity; only admins
    // may read another actor's stream here. (The finer capability-based broad
    // view of legacy `has_broad_view` is not yet ported — this is stricter, never
    // broader, so it can't leak.)
    let actor_id = if actor.is_admin {
        query.actor_id
    } else {
        Some(actor.id)
    };
    let items = newsroom
        .dashboard_activity(query.entity_type.as_deref(), actor_id, query.limit)
        .await
        .map_err(newsroom_error)?;
    Ok(Json(json!({
        "items": items,
        "generated_at": meridian_newsroom::rfc3339(time::OffsetDateTime::now_utc()),
    })))
}

#[derive(Deserialize)]
struct ReleaseTimelineQuery {
    desk_id: Option<uuid::Uuid>,
    status: Option<String>,
}

async fn dashboard_releases(
    Extension(identity): Extension<IdentityService>,
    Extension(newsroom): Extension<NewsroomService>,
    Query(query): Query<ReleaseTimelineQuery>,
    headers: HeaderMap,
) -> ApiResult<impl IntoResponse> {
    let actor = actor_from_request(&identity, &headers).await?;
    let items = newsroom
        .dashboard_releases(&actor, query.desk_id, query.status.as_deref())
        .await
        .map_err(newsroom_error)?;
    // `at_risk_count` (scheduled releases now past their slot) and the publisher
    // `retry_interval_seconds` mirror the legacy release-timeline envelope.
    let at_risk_count = items
        .iter()
        .filter(|r| r.status == "scheduled" && r.scheduled_at.is_some_and(|at| at < time::OffsetDateTime::now_utc()))
        .count();
    Ok(Json(json!({
        "items": items,
        "at_risk_count": at_risk_count,
        "retry_interval_seconds": 60,
        "generated_at": meridian_newsroom::rfc3339(time::OffsetDateTime::now_utc()),
    })))
}

async fn dashboard_layout(
    Extension(identity): Extension<IdentityService>,
    Extension(newsroom): Extension<NewsroomService>,
    headers: HeaderMap,
) -> ApiResult<impl IntoResponse> {
    let actor = actor_from_request(&identity, &headers).await?;
    let layout = newsroom.resolve_layout(&actor).await.map_err(newsroom_error)?;
    Ok(Json(layout))
}

async fn reset_dashboard_layout(
    Extension(identity): Extension<IdentityService>,
    Extension(newsroom): Extension<NewsroomService>,
    headers: HeaderMap,
) -> ApiResult<impl IntoResponse> {
    let actor = actor_from_request(&identity, &headers).await?;
    newsroom
        .clear_default_dashboard_view(&actor)
        .await
        .map_err(newsroom_error)?;
    let layout = newsroom.resolve_layout(&actor).await.map_err(newsroom_error)?;
    Ok(Json(layout))
}

async fn capture_metrics(
    Extension(identity): Extension<IdentityService>,
    Extension(newsroom): Extension<NewsroomService>,
    headers: HeaderMap,
) -> ApiResult<impl IntoResponse> {
    let actor = actor_from_request(&identity, &headers).await?;
    let written = newsroom.capture_metrics(&actor).await.map_err(newsroom_error)?;
    Ok(Json(json!({ "series_written": written })))
}

async fn drilldown(
    Extension(identity): Extension<IdentityService>,
    Extension(newsroom): Extension<NewsroomService>,
    Path(key): Path<String>,
    Query(scope): Query<DashboardScope>,
    headers: HeaderMap,
) -> ApiResult<impl IntoResponse> {
    let actor = actor_from_request(&identity, &headers).await?;
    let rows = newsroom
        .drilldown(&actor, &key, scope.desk_id, scope.owner_id)
        .await
        .map_err(newsroom_error)?;
    // Legacy envelope: the rows under `items`, a `total`, a `truncated` flag, and
    // a timestamp. The query is unbounded here, so nothing is truncated.
    let total = rows.len();
    Ok(Json(json!({
        "key": key,
        "items": rows,
        "total": total,
        "truncated": false,
        "generated_at": meridian_newsroom::rfc3339(time::OffsetDateTime::now_utc()),
    })))
}

#[derive(Deserialize)]
struct SeriesQuery {
    metric_key: String,
    #[serde(default = "global_scope")]
    scope_type: String,
    #[serde(default)]
    scope_id: String,
    #[serde(default = "default_since_days")]
    days: i64,
}
fn global_scope() -> String {
    "global".to_owned()
}
fn default_since_days() -> i64 {
    7
}

async fn metric_series(
    Extension(identity): Extension<IdentityService>,
    Extension(newsroom): Extension<NewsroomService>,
    Query(query): Query<SeriesQuery>,
    headers: HeaderMap,
) -> ApiResult<impl IntoResponse> {
    actor_from_request(&identity, &headers).await?;
    let since = time::OffsetDateTime::now_utc() - time::Duration::days(query.days.clamp(1, 90));
    let points = newsroom
        .metric_series(&query.metric_key, &query.scope_type, &query.scope_id, since)
        .await
        .map_err(newsroom_error)?;
    Ok(Json(json!({ "metric_key": query.metric_key, "points": points })))
}

async fn acknowledge_attention(
    Extension(identity): Extension<IdentityService>,
    Extension(newsroom): Extension<NewsroomService>,
    Path(fingerprint): Path<String>,
    headers: HeaderMap,
) -> ApiResult<impl IntoResponse> {
    let actor = actor_from_request(&identity, &headers).await?;
    let state = newsroom.acknowledge_attention(&actor, &fingerprint).await.map_err(newsroom_error)?;
    Ok(Json(state))
}

async fn unacknowledge_attention(
    Extension(identity): Extension<IdentityService>,
    Extension(newsroom): Extension<NewsroomService>,
    Path(fingerprint): Path<String>,
    headers: HeaderMap,
) -> ApiResult<impl IntoResponse> {
    let actor = actor_from_request(&identity, &headers).await?;
    let state = newsroom.unacknowledge_attention(&actor, &fingerprint).await.map_err(newsroom_error)?;
    Ok(Json(state))
}

#[derive(Deserialize)]
struct SnoozeBody {
    // Legacy names this `snoozed_until`.
    #[serde(rename = "snoozed_until", with = "time::serde::rfc3339")]
    until: time::OffsetDateTime,
}

async fn snooze_attention(
    AuthActor(actor): AuthActor,
    Extension(newsroom): Extension<NewsroomService>,
    Path(fingerprint): Path<String>,
    Json(body): Json<SnoozeBody>,
) -> ApiResult<impl IntoResponse> {
    let state = newsroom
        .snooze_attention(&actor, &fingerprint, body.until)
        .await
        .map_err(newsroom_error)?;
    Ok(Json(state))
}

#[derive(Deserialize)]
struct AssignBody {
    assignee_id: uuid::Uuid,
}

async fn assign_attention(
    AuthActor(actor): AuthActor,
    Extension(newsroom): Extension<NewsroomService>,
    Path(fingerprint): Path<String>,
    Json(body): Json<AssignBody>,
) -> ApiResult<impl IntoResponse> {
    let state = newsroom
        .assign_attention(&actor, &fingerprint, body.assignee_id)
        .await
        .map_err(newsroom_error)?;
    Ok(Json(state))
}

#[derive(Deserialize)]
struct EscalateBody {
    #[serde(default)]
    to_user_id: Option<uuid::Uuid>,
}

async fn escalate_attention(
    AuthActor(actor): AuthActor,
    Extension(newsroom): Extension<NewsroomService>,
    Path(fingerprint): Path<String>,
    Json(body): Json<EscalateBody>,
) -> ApiResult<impl IntoResponse> {
    let state = newsroom
        .escalate_attention(&actor, &fingerprint, body.to_user_id)
        .await
        .map_err(newsroom_error)?;
    Ok(Json(state))
}

#[derive(Deserialize)]
struct NotificationQuery {
    #[serde(default)]
    unread_only: bool,
    #[serde(default = "default_limit")]
    limit: i64,
}
fn default_limit() -> i64 {
    100
}

#[derive(Deserialize)]
struct PreferenceBody {
    kind: String,
    #[serde(default = "yes")]
    in_app: bool,
    #[serde(default = "immediate")]
    digest: String,
    #[serde(default = "low")]
    min_priority: String,
}
fn yes() -> bool {
    true
}
fn immediate() -> String {
    "immediate".to_owned()
}
fn low() -> String {
    "low".to_owned()
}

#[derive(Deserialize)]
struct PreferencesBody {
    #[serde(default)]
    entries: Vec<PreferenceBody>,
}

#[derive(Deserialize)]
struct FollowBody {
    entity_type: String,
    entity_id: String,
}

#[derive(Deserialize)]
struct FavoriteBody {
    entity_type: String,
    entity_id: String,
    label: String,
    #[serde(default)]
    position: i32,
}

#[derive(Deserialize)]
struct RecentBody {
    entity_type: String,
    entity_id: String,
    title: String,
}

async fn list_notifications(
    Extension(identity): Extension<IdentityService>,
    Extension(newsroom): Extension<NewsroomService>,
    Query(query): Query<NotificationQuery>,
    headers: HeaderMap,
) -> ApiResult<impl IntoResponse> {
    let actor = actor_from_request(&identity, &headers).await?;
    let items = newsroom
        .list_notifications(&actor, query.unread_only, query.limit)
        .await
        .map_err(newsroom_error)?;
    Ok(Json(items))
}

async fn mark_read(
    Extension(identity): Extension<IdentityService>,
    Extension(newsroom): Extension<NewsroomService>,
    Path(notification_id): Path<uuid::Uuid>,
    headers: HeaderMap,
) -> ApiResult<impl IntoResponse> {
    let actor = actor_from_request(&identity, &headers).await?;
    let item = newsroom
        .mark_notification_read(&actor, notification_id)
        .await
        .map_err(newsroom_error)?;
    // The single-notification read contract differs from the list element.
    Ok(Json(meridian_newsroom::awareness::ReadNotification::from(item)))
}

async fn mark_all_read(
    Extension(identity): Extension<IdentityService>,
    Extension(newsroom): Extension<NewsroomService>,
    headers: HeaderMap,
) -> ApiResult<impl IntoResponse> {
    let actor = actor_from_request(&identity, &headers).await?;
    let changed = newsroom.mark_all_notifications_read(&actor).await.map_err(newsroom_error)?;
    Ok(Json(json!({ "marked_read": changed })))
}

async fn get_preferences(
    Extension(identity): Extension<IdentityService>,
    Extension(newsroom): Extension<NewsroomService>,
    headers: HeaderMap,
) -> ApiResult<impl IntoResponse> {
    let actor = actor_from_request(&identity, &headers).await?;
    let prefs = newsroom.get_preferences(&actor).await.map_err(newsroom_error)?;
    Ok(Json(prefs))
}

async fn set_preferences(
    AuthActor(actor): AuthActor,
    Extension(newsroom): Extension<NewsroomService>,
    Json(body): Json<PreferencesBody>,
) -> ApiResult<impl IntoResponse> {
    let entries: Vec<PreferenceInput> = body
        .entries
        .into_iter()
        .map(|entry| PreferenceInput {
            kind: entry.kind,
            in_app: entry.in_app,
            digest: entry.digest,
            min_priority: entry.min_priority,
        })
        .collect();
    let prefs = newsroom.set_preferences(&actor, &entries).await.map_err(newsroom_error)?;
    Ok(Json(prefs))
}

async fn list_follows(
    Extension(identity): Extension<IdentityService>,
    Extension(newsroom): Extension<NewsroomService>,
    headers: HeaderMap,
) -> ApiResult<impl IntoResponse> {
    let actor = actor_from_request(&identity, &headers).await?;
    let follows = newsroom.list_follows(&actor).await.map_err(newsroom_error)?;
    Ok(Json(follows))
}

async fn add_follow(
    AuthActor(actor): AuthActor,
    Extension(newsroom): Extension<NewsroomService>,
    Json(body): Json<FollowBody>,
) -> ApiResult<impl IntoResponse> {
    let follow = newsroom
        .add_follow(&actor, &body.entity_type, &body.entity_id)
        .await
        .map_err(newsroom_error)?;
    Ok((StatusCode::CREATED, Json(follow)))
}

async fn remove_follow(
    Extension(identity): Extension<IdentityService>,
    Extension(newsroom): Extension<NewsroomService>,
    Path((entity_type, entity_id)): Path<(String, String)>,
    headers: HeaderMap,
) -> ApiResult<impl IntoResponse> {
    let actor = actor_from_request(&identity, &headers).await?;
    newsroom
        .remove_follow(&actor, &entity_type, &entity_id)
        .await
        .map_err(newsroom_error)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn list_favorites(
    Extension(identity): Extension<IdentityService>,
    Extension(newsroom): Extension<NewsroomService>,
    headers: HeaderMap,
) -> ApiResult<impl IntoResponse> {
    let actor = actor_from_request(&identity, &headers).await?;
    let favorites = newsroom.list_favorites(&actor).await.map_err(newsroom_error)?;
    Ok(Json(favorites))
}

async fn add_favorite(
    AuthActor(actor): AuthActor,
    Extension(newsroom): Extension<NewsroomService>,
    Json(body): Json<FavoriteBody>,
) -> ApiResult<impl IntoResponse> {
    let favorite = newsroom
        .add_favorite(&actor, &body.entity_type, &body.entity_id, &body.label, body.position)
        .await
        .map_err(newsroom_error)?;
    Ok((StatusCode::CREATED, Json(favorite)))
}

async fn remove_favorite(
    Extension(identity): Extension<IdentityService>,
    Extension(newsroom): Extension<NewsroomService>,
    Path((entity_type, entity_id)): Path<(String, String)>,
    headers: HeaderMap,
) -> ApiResult<impl IntoResponse> {
    let actor = actor_from_request(&identity, &headers).await?;
    newsroom
        .remove_favorite(&actor, &entity_type, &entity_id)
        .await
        .map_err(newsroom_error)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn list_recents(
    Extension(identity): Extension<IdentityService>,
    Extension(newsroom): Extension<NewsroomService>,
    headers: HeaderMap,
) -> ApiResult<impl IntoResponse> {
    let actor = actor_from_request(&identity, &headers).await?;
    let recents = newsroom.list_recents(&actor).await.map_err(newsroom_error)?;
    Ok(Json(recents))
}

async fn record_recent(
    AuthActor(actor): AuthActor,
    Extension(newsroom): Extension<NewsroomService>,
    Json(body): Json<RecentBody>,
) -> ApiResult<impl IntoResponse> {
    let item = newsroom
        .record_visit(&actor, &body.entity_type, &body.entity_id, &body.title)
        .await
        .map_err(newsroom_error)?;
    Ok((StatusCode::CREATED, Json(item)))
}

async fn navigation(
    Extension(identity): Extension<IdentityService>,
    Extension(newsroom): Extension<NewsroomService>,
    headers: HeaderMap,
) -> ApiResult<impl IntoResponse> {
    let actor = actor_from_request(&identity, &headers).await?;
    let (items, actions) = newsroom.navigation_menu(&actor).await.map_err(newsroom_error)?;
    let favorites = newsroom.list_favorites(&actor).await.map_err(newsroom_error)?;
    let recents = newsroom.list_recents(&actor).await.map_err(newsroom_error)?;
    let badges = newsroom.badge_counts(&actor).await.map_err(newsroom_error)?;
    let generated_at = time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default();
    Ok(Json(json!({
        "items": items,
        "actions": actions,
        "favorites": favorites,
        "recents": recents,
        "badges": badges,
        "generated_at": generated_at,
    })))
}

async fn activity_center(
    Extension(identity): Extension<IdentityService>,
    Extension(newsroom): Extension<NewsroomService>,
    headers: HeaderMap,
) -> ApiResult<impl IntoResponse> {
    let actor = actor_from_request(&identity, &headers).await?;
    let center = newsroom.activity_center(&actor).await.map_err(newsroom_error)?;
    Ok(Json(center))
}

async fn process_escalations(
    Extension(identity): Extension<IdentityService>,
    Extension(newsroom): Extension<NewsroomService>,
    headers: HeaderMap,
) -> ApiResult<impl IntoResponse> {
    let actor = actor_from_request(&identity, &headers).await?;
    let escalated = newsroom.process_escalations(&actor).await.map_err(newsroom_error)?;
    Ok(Json(json!({ "escalated": escalated })))
}

fn empty_layout() -> serde_json::Value {
    serde_json::Value::Array(Vec::new())
}

#[derive(Deserialize)]
struct ViewBody {
    name: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    desk_id: Option<uuid::Uuid>,
    #[serde(default)]
    is_shared: bool,
    #[serde(default = "empty_layout")]
    layout: serde_json::Value,
    /// Widget type names, as a chooser UI produces them. Takes precedence over
    /// `layout` when non-empty; see `view_input`.
    #[serde(default)]
    widgets: Vec<String>,
    #[serde(default = "empty_object")]
    filters: serde_json::Value,
}

fn view_input(body: ViewBody) -> DashboardViewInput {
    // A caller may send either a ready-made `layout` (the legacy contract, a full
    // placement array) or a `widgets` list of type names. The list is the shape a
    // chooser UI naturally produces, and turning it into placements here keeps the
    // grid arithmetic in one place rather than duplicated in every client.
    let layout = if body.widgets.is_empty() {
        body.layout
    } else {
        meridian_newsroom::dashviews::layout_from_types(&body.widgets)
    };
    DashboardViewInput {
        name: body.name,
        description: body.description,
        desk_id: body.desk_id,
        is_shared: body.is_shared,
        layout,
        filters: body.filters,
    }
}

async fn list_dashboard_views(
    Extension(identity): Extension<IdentityService>,
    Extension(newsroom): Extension<NewsroomService>,
    headers: HeaderMap,
) -> ApiResult<impl IntoResponse> {
    let actor = actor_from_request(&identity, &headers).await?;
    let views = newsroom.list_dashboard_views(&actor).await.map_err(newsroom_error)?;
    Ok(Json(views))
}

async fn get_dashboard_view(
    Extension(identity): Extension<IdentityService>,
    Extension(newsroom): Extension<NewsroomService>,
    Path(view_id): Path<uuid::Uuid>,
    headers: HeaderMap,
) -> ApiResult<impl IntoResponse> {
    let actor = actor_from_request(&identity, &headers).await?;
    let view = newsroom.get_dashboard_view(&actor, view_id).await.map_err(newsroom_error)?;
    Ok(Json(view))
}

async fn create_dashboard_view(
    AuthActor(actor): AuthActor,
    Extension(newsroom): Extension<NewsroomService>,
    Json(body): Json<ViewBody>,
) -> ApiResult<impl IntoResponse> {
    let view = newsroom.create_dashboard_view(&actor, &view_input(body)).await.map_err(newsroom_error)?;
    Ok((StatusCode::CREATED, Json(view)))
}

async fn update_dashboard_view(
    AuthActor(actor): AuthActor,
    Extension(newsroom): Extension<NewsroomService>,
    Path(view_id): Path<uuid::Uuid>,
    Json(body): Json<ViewBody>,
) -> ApiResult<impl IntoResponse> {
    let view = newsroom
        .update_dashboard_view(&actor, view_id, &view_input(body))
        .await
        .map_err(newsroom_error)?;
    Ok(Json(view))
}

async fn delete_dashboard_view(
    Extension(identity): Extension<IdentityService>,
    Extension(newsroom): Extension<NewsroomService>,
    Path(view_id): Path<uuid::Uuid>,
    headers: HeaderMap,
) -> ApiResult<impl IntoResponse> {
    let actor = actor_from_request(&identity, &headers).await?;
    newsroom.delete_dashboard_view(&actor, view_id).await.map_err(newsroom_error)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn unsnooze_attention(
    Extension(identity): Extension<IdentityService>,
    Extension(newsroom): Extension<NewsroomService>,
    Path(fingerprint): Path<String>,
    headers: HeaderMap,
) -> ApiResult<impl IntoResponse> {
    let actor = actor_from_request(&identity, &headers).await?;
    // Clearing a snooze is a snooze that has already elapsed.
    let state = newsroom
        .snooze_attention(&actor, &fingerprint, time::OffsetDateTime::now_utc())
        .await
        .map_err(newsroom_error)?;
    Ok(Json(state))
}

async fn duplicate_dashboard_view(
    Extension(identity): Extension<IdentityService>,
    Extension(newsroom): Extension<NewsroomService>,
    Path(view_id): Path<uuid::Uuid>,
    headers: HeaderMap,
) -> ApiResult<impl IntoResponse> {
    let actor = actor_from_request(&identity, &headers).await?;
    let source = newsroom.get_dashboard_view(&actor, view_id).await.map_err(newsroom_error)?;
    let copy = DashboardViewInput {
        name: format!("{} (copy)", source.name),
        description: source.description,
        desk_id: source.desk_id,
        // A duplicate starts private: sharing is a deliberate act, not inherited.
        is_shared: false,
        layout: source.layout_json,
        filters: source.filters_json,
    };
    let view = newsroom.create_dashboard_view(&actor, &copy).await.map_err(newsroom_error)?;
    Ok((StatusCode::CREATED, Json(view)))
}

async fn set_default_dashboard_view(
    Extension(identity): Extension<IdentityService>,
    Extension(newsroom): Extension<NewsroomService>,
    Path(view_id): Path<uuid::Uuid>,
    headers: HeaderMap,
) -> ApiResult<impl IntoResponse> {
    let actor = actor_from_request(&identity, &headers).await?;
    let view = newsroom.set_default_dashboard_view(&actor, view_id).await.map_err(newsroom_error)?;
    Ok(Json(view))
}
