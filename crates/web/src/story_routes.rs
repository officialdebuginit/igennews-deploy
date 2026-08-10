//! Module 4 compatibility REST routes: stories, immutable versions, workflow
//! transitions, publish-readiness, pitches, sources, evidence, and claims.
//!
//! Paths and status codes mirror the legacy `/api/v1` router. These operations
//! stay legacy-owned in the contract ledger until differential HTTP tests prove
//! parity; wiring them here is what makes that comparison possible.

use std::time::Duration;

use axum::{
    Extension, Json, Router,
    extract::{Multipart, Path, Query},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{get, post},
};
use meridian_identity::IdentityService;
use meridian_platform::MediaStore;
use sha2::{Digest, Sha256};
use meridian_newsroom::{
    NewsroomService,
    pitches::{PitchDecision, PitchDraft, PitchFilter},
    media::{AssetDraft, NinjsStory, PublicArticle, ReleaseDraft, StoredAsset, SyndicationItem},
    reviews::{CommentDraft, ReviewFilter, ReviewRequest},
    stories::{StoryDraft, StoryFilter, StoryPatch, WorkflowState},
    tasks::{CoverageDraft, CoveragePatch, TaskDraft, TaskFilter, TaskPatch},
    verification::{ClaimDraft, EvidenceDraft, SourceDraft},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::newsroom_routes::{ApiResult, AuthActor, actor_from_request, newsroom_error};
use crate::realtime::RealtimeHub;

pub fn router() -> Router {
    Router::new()
        // Public syndication surfaces (unauthenticated — only published stories).
        .route("/feed.xml", get(rss_feed))
        .route("/feed.ninjs", get(ninjs_feed))
        .route("/feed.newsml", get(newsml_feed))
        .route("/sitemap.xml", get(sitemap_xml))
        .route("/news-sitemap.xml", get(news_sitemap_xml))
        .route("/robots.txt", get(robots_txt))
        .route("/trust.txt", get(trust_txt))
        // Public reader: one published article by slug, no authentication. This is
        // what makes a shared /read/{slug} link work for a logged-out visitor.
        .route("/api/v1/public/articles/{slug}", get(public_article_route))
        // Headless JSON content API: the list of published articles for downstream
        // sites/apps — the JSON counterpart to the RSS/ninjs/NewsML feeds.
        .route("/api/v1/public/articles", get(public_articles_route))
        // Interchange ingest: turn an IPTC ninjs (News-in-JSON) or NewsML-G2 wire
        // item into a draft story. The inverse of the /feed.ninjs and /feed.newsml
        // exports.
        .route("/api/v1/ingest/ninjs", post(ingest_ninjs))
        .route("/api/v1/ingest/newsml", post(ingest_newsml))
        .route("/api/v1/ingest/nitf", post(ingest_nitf))
        .route("/api/v1/ingest/poll", post(ingest_poll))
        .route("/api/v1/stories", get(list_stories).post(create_story))
        .route(
            "/api/v1/stories/{story_id}",
            get(get_story).patch(update_story).delete(delete_story),
        )
        .route(
            "/api/v1/stories/{story_id}/versions",
            get(list_versions).post(save_version),
        )
        .route("/api/v1/stories/{story_id}/transition", post(transition))
        .route("/api/v1/stories/{story_id}/publish-readiness", get(readiness))
        .route("/api/v1/pitches", get(list_pitches).post(create_pitch))
        .route("/api/v1/pitches/{pitch_id}/decision", post(decide_pitch))
        .route(
            "/api/v1/stories/{story_id}/sources",
            get(list_sources).post(create_source),
        )
        .route("/api/v1/sources/{source_id}/approve", post(approve_source))
        .route("/api/v1/sources/{source_id}/grants", post(grant_source_access))
        .route(
            "/api/v1/stories/{story_id}/evidence",
            get(list_evidence).post(create_evidence),
        )
        .route(
            "/api/v1/stories/{story_id}/claims",
            get(list_claims).post(create_claim),
        )
        .route("/api/v1/claims/{claim_id}/decision", post(decide_claim))
        // Beyond the legacy surface: evidence had no transition path, so its
        // verification status could never move off the default.
        .route("/api/v1/evidence/{evidence_id}/status", post(set_evidence_status))
        .route("/api/v1/stories/{story_id}/reviews", post(request_review))
        .route("/api/v1/reviews", get(list_reviews))
        .route("/api/v1/reviews/{review_id}/decision", post(decide_review))
        .route("/api/v1/stories/{story_id}/approvals", get(list_approvals))
        .route(
            "/api/v1/stories/{story_id}/comments",
            get(list_comments).post(create_comment),
        )
        .route("/api/v1/comments/{comment_id}/resolve", post(resolve_comment))
        .route("/api/v1/comments/{comment_id}", axum::routing::delete(delete_comment))
        .route("/api/v1/tasks", get(list_tasks).post(create_task))
        .route(
            "/api/v1/tasks/{task_id}",
            axum::routing::patch(update_task).delete(delete_task),
        )
        .route(
            "/api/v1/coverage-events",
            get(list_coverage_events).post(create_coverage_event),
        )
        .route(
            "/api/v1/coverage-events/{event_id}",
            axum::routing::patch(update_coverage_event).delete(delete_coverage_event),
        )
        .route("/api/v1/desks/{desk_id}/analytics", get(desk_analytics))
        .route("/api/v1/assets", get(list_assets).post(create_asset))
        .route("/api/v1/assets/upload", post(upload_asset))
        .route("/api/v1/assets/cleanup-orphans", post(cleanup_orphans))
        .route(
            "/api/v1/assets/{asset_id}",
            axum::routing::delete(delete_asset).patch(update_asset),
        )
        .route("/api/v1/assets/{asset_id}/verification", post(set_asset_verification))
        .route("/api/v1/assets/{asset_id}/download", get(download_asset))
        .route("/api/v1/assets/{asset_id}/raw", get(raw_asset))
        .route("/api/v1/assets/{asset_id}/public", get(public_asset))
        .route("/api/v1/assets/{asset_id}/share-url", get(asset_share_url))
        .route("/api/v1/assets/{asset_id}/folder", axum::routing::put(move_asset))
        .route("/api/v1/assets/{asset_id}/rename", axum::routing::put(rename_asset))
        .route("/api/v1/folders", get(list_folders).post(create_folder))
        .route(
            "/api/v1/folders/{folder_id}",
            axum::routing::patch(rename_folder).delete(delete_folder),
        )
        .route("/api/v1/legal/sign", post(legal_sign))
        .route("/api/v1/legal/verify", post(legal_verify))
        .route(
            "/api/v1/legal/documents",
            get(list_legal_documents).post(create_legal_document),
        )
        .route(
            "/api/v1/legal/documents/{doc_id}",
            get(get_legal_document)
                .patch(update_legal_document)
                .delete(delete_legal_document),
        )
        .route("/api/v1/legal/documents/{doc_id}/parties", post(add_legal_party))
        .route(
            "/api/v1/legal/documents/{doc_id}/parties/{party_id}",
            axum::routing::delete(remove_legal_party),
        )
        .route("/api/v1/legal/documents/{doc_id}/sign", post(sign_legal_document))
        .route("/api/v1/legal/documents/{doc_id}/decline", post(decline_legal_document))
        .route("/api/v1/legal/documents/{doc_id}/void", post(void_legal_document))
        .route("/api/v1/legal/documents/{doc_id}/verify", get(verify_legal_document))
        .route("/api/v1/legal/documents/{doc_id}/activity", get(legal_document_activity))
        .route("/api/v1/legal/documents/{doc_id}/export", get(export_legal_document))
        .route(
            "/api/v1/stories/{story_id}/releases",
            get(list_releases).post(create_release),
        )
        .route("/api/v1/stories/{story_id}/corrections", post(create_correction))
        // Beyond the legacy surface: the channel registry. `releases.channels` was
        // free text with nothing defining what a channel is.
        .route("/api/v1/channels", get(list_channels).post(upsert_channel))
        // Beyond the legacy surface: front-page curation had no model at all.
        .route("/api/v1/front-page", get(front_page))
        .route("/api/v1/front-page/{slot_id}", axum::routing::put(set_front_page_slot))
        .route("/api/v1/corrections", get(list_corrections))
        .route("/api/v1/corrections/{correction_id}/resolve", post(resolve_correction))
        .route("/api/v1/releases/process-due", post(process_due_releases))
        .route("/api/v1/releases/{release_id}", axum::routing::patch(reschedule_release))
        .route("/api/v1/releases/{release_id}/cancel", post(cancel_release))
        .route("/api/v1/releases/{release_id}/publish-now", post(publish_release_now))
        .route("/api/v1/releases/{release_id}/channel-schedule", axum::routing::put(set_channel_schedule))
}

// --- Defaults + request bodies ----------------------------------------------

fn untitled() -> String {
    "Untitled Story".to_owned()
}
fn general() -> String {
    "General".to_owned()
}
fn article() -> String {
    "article".to_owned()
}
fn medium() -> String {
    "medium".to_owned()
}
fn empty_array() -> Value {
    Value::Array(Vec::new())
}
fn on_record() -> String {
    "on_record".to_owned()
}
fn story_team() -> String {
    "story_team".to_owned()
}

#[derive(Deserialize)]
struct StoryCreateBody {
    slug: String,
    #[serde(default = "untitled")]
    title: String,
    #[serde(default)]
    dek: String,
    #[serde(default = "empty_array")]
    body: Value,
    #[serde(default = "general")]
    category: String,
    #[serde(default = "empty_array")]
    tags: Value,
    #[serde(default = "article")]
    story_type: String,
    #[serde(default = "medium")]
    priority: String,
    #[serde(default)]
    desk_id: Option<Uuid>,
    #[serde(default)]
    sub_sector_id: Option<Uuid>,
    #[serde(default)]
    event_id: Option<Uuid>,
    #[serde(default)]
    author_id: Option<Uuid>,
    #[serde(default)]
    editor_id: Option<Uuid>,
    #[serde(default)]
    fact_checker_id: Option<Uuid>,
    #[serde(default)]
    copy_editor_id: Option<Uuid>,
}

#[derive(Deserialize)]
struct StoryPatchBody {
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    dek: Option<String>,
    #[serde(default)]
    body: Option<Value>,
    #[serde(default)]
    category: Option<String>,
    #[serde(default)]
    tags: Option<Value>,
    #[serde(default)]
    priority: Option<String>,
    #[serde(default)]
    desk_id: Option<Uuid>,
    #[serde(default)]
    sub_sector_id: Option<Uuid>,
    #[serde(default)]
    author_id: Option<Uuid>,
    #[serde(default)]
    editor_id: Option<Uuid>,
    #[serde(default)]
    fact_checker_id: Option<Uuid>,
    #[serde(default)]
    copy_editor_id: Option<Uuid>,
    #[serde(default, with = "time::serde::rfc3339::option")]
    due_at: Option<OffsetDateTime>,
    // Per-story rights overrides. A missing field is left unchanged; an explicit
    // empty string clears the override back to the outlet default.
    #[serde(default)]
    rights_holder: Option<String>,
    #[serde(default)]
    rights_notice: Option<String>,
    #[serde(default)]
    usage_terms: Option<String>,
    #[serde(default)]
    license_url: Option<String>,
    // Reuse constraints (ODRL). Same unchanged/clear rules as the rights fields.
    #[serde(default)]
    reuse_embargo_until: Option<String>,
    #[serde(default)]
    territory_mode: Option<String>,
    #[serde(default)]
    territories: Option<String>,
    // AI-disclosure: an IPTC Digital Source Type code.
    #[serde(default)]
    digital_source_type: Option<String>,
}

#[derive(Deserialize)]
struct StoryFilterQuery {
    workflow: Option<String>,
    publication: Option<String>,
    desk_id: Option<Uuid>,
    author_id: Option<Uuid>,
}

#[derive(Deserialize)]
struct VersionBody {
    #[serde(default)]
    change_summary: Option<String>,
    #[serde(default)]
    filed: bool,
}

#[derive(Deserialize)]
struct TransitionBody {
    state: String,
    #[serde(default)]
    reason: Option<String>,
    #[serde(default)]
    fast_path: bool,
}

#[derive(Deserialize)]
struct PitchCreateBody {
    headline: String,
    summary: String,
    #[serde(default)]
    angle: Option<String>,
    #[serde(default)]
    desk_id: Option<Uuid>,
    #[serde(default)]
    event_id: Option<Uuid>,
    #[serde(default)]
    assignee_id: Option<Uuid>,
    #[serde(default)]
    editor_id: Option<Uuid>,
    #[serde(default = "medium")]
    priority: String,
    #[serde(default)]
    target_audience: Option<String>,
    #[serde(default)]
    expected_format: Option<String>,
    #[serde(default = "empty_array")]
    key_questions: Value,
    #[serde(default = "empty_array")]
    likely_sources: Value,
    #[serde(default = "empty_array")]
    risks: Value,
    #[serde(default, with = "time::serde::rfc3339::option")]
    target_at: Option<OffsetDateTime>,
}

#[derive(Deserialize)]
struct PitchFilterQuery {
    status: Option<String>,
    desk_id: Option<Uuid>,
}

#[derive(Deserialize)]
struct PitchDecisionBody {
    status: String,
    #[serde(default)]
    note: Option<String>,
    #[serde(default)]
    assignee_id: Option<Uuid>,
    #[serde(default)]
    editor_id: Option<Uuid>,
}

#[derive(Deserialize)]
struct SourceCreateBody {
    identity: String,
    publishable_attribution: String,
    #[serde(default = "on_record")]
    ground_rule: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    motive: Option<String>,
    #[serde(default)]
    reliability_notes: Option<String>,
    #[serde(default = "story_team")]
    access_level: String,
}

#[derive(Deserialize)]
struct SourceApprovalBody {
    #[serde(default)]
    rationale: Option<String>,
}

#[derive(Deserialize)]
struct SourceGrantBody {
    user_id: Uuid,
}

#[derive(Deserialize)]
struct EvidenceCreateBody {
    #[serde(default)]
    source_id: Option<Uuid>,
    kind: String,
    title: String,
    #[serde(default)]
    uri: Option<String>,
    #[serde(default)]
    notes: Option<String>,
    #[serde(default)]
    checksum: Option<String>,
    #[serde(default, with = "time::serde::rfc3339::option")]
    acquired_at: Option<OffsetDateTime>,
}

#[derive(Deserialize)]
struct ClaimCreateBody {
    text: String,
    #[serde(default)]
    locator: Option<String>,
    #[serde(default)]
    version_id: Option<Uuid>,
    #[serde(default = "empty_array")]
    evidence_ids: Value,
}

#[derive(Deserialize)]
struct ClaimDecisionBody {
    status: String,
    #[serde(default)]
    note: Option<String>,
}

#[derive(Deserialize)]
struct ReviewCreateBody {
    version_id: Uuid,
    kind: String,
    assigned_to_id: Uuid,
}

#[derive(Deserialize)]
struct ReviewFilterQuery {
    decision: Option<String>,
    story_id: Option<Uuid>,
}

#[derive(Deserialize)]
struct ReviewDecisionBody {
    decision: String,
    #[serde(default)]
    notes: Option<String>,
}

#[derive(Deserialize)]
struct CommentCreateBody {
    body: String,
    #[serde(default)]
    locator: Option<String>,
    #[serde(default)]
    version_id: Option<Uuid>,
}

#[derive(Deserialize)]
struct TaskCreateBody {
    title: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default = "todo")]
    status: String,
    #[serde(default = "medium")]
    priority: String,
    #[serde(default)]
    story_id: Option<Uuid>,
    #[serde(default)]
    desk_id: Option<Uuid>,
    #[serde(default)]
    pitch_id: Option<Uuid>,
    #[serde(default)]
    event_id: Option<Uuid>,
    #[serde(default)]
    assigned_to_id: Option<Uuid>,
    #[serde(default, with = "time::serde::rfc3339::option")]
    due_at: Option<OffsetDateTime>,
}

#[derive(Deserialize)]
struct TaskPatchBody {
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    priority: Option<String>,
    #[serde(default)]
    assigned_to_id: Option<Uuid>,
    #[serde(default, with = "time::serde::rfc3339::option")]
    due_at: Option<OffsetDateTime>,
    #[serde(default)]
    blocker: Option<String>,
}

#[derive(Deserialize)]
struct TaskFilterQuery {
    assigned_to_id: Option<Uuid>,
    status: Option<String>,
    story_id: Option<Uuid>,
    /// Narrows the board to one sector; also scopes the `tasks.view_all` check.
    desk_id: Option<Uuid>,
}

fn todo() -> String {
    "todo".to_owned()
}
fn planned() -> String {
    "planned".to_owned()
}

#[derive(Deserialize)]
struct CoverageCreateBody {
    title: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    desk_id: Option<Uuid>,
    #[serde(default, with = "time::serde::rfc3339::option")]
    starts_at: Option<OffsetDateTime>,
    #[serde(default, with = "time::serde::rfc3339::option")]
    ends_at: Option<OffsetDateTime>,
    #[serde(default = "planned")]
    status: String,
    #[serde(default)]
    location: Option<String>,
}

#[derive(Deserialize)]
struct CoveragePatchBody {
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    desk_id: Option<Uuid>,
    #[serde(default, with = "time::serde::rfc3339::option")]
    starts_at: Option<OffsetDateTime>,
    #[serde(default, with = "time::serde::rfc3339::option")]
    ends_at: Option<OffsetDateTime>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    location: Option<String>,
}

#[derive(Deserialize)]
struct AnalyticsQuery {
    #[serde(default)]
    include_sub_desks: bool,
}

#[derive(Deserialize)]
struct AssetQuery {
    story_id: Option<Uuid>,
}

#[derive(Deserialize)]
struct AssetCreateBody {
    #[serde(default)]
    story_id: Option<Uuid>,
    kind: String,
    uri: String,
    #[serde(default)]
    filename: Option<String>,
    #[serde(default)]
    creator: Option<String>,
    #[serde(default)]
    source: Option<String>,
    #[serde(default)]
    rights: Option<String>,
    #[serde(default)]
    credit: Option<String>,
    #[serde(default)]
    caption: Option<String>,
    #[serde(default)]
    alt_text: Option<String>,
}

#[derive(Deserialize)]
struct ReleaseCreateBody {
    version_id: Uuid,
    #[serde(default = "empty_array")]
    channels: Value,
    #[serde(default, with = "time::serde::rfc3339::option")]
    scheduled_at: Option<OffsetDateTime>,
    /// Beyond the legacy contract: hold-until and take-down, separate from
    /// `scheduled_at`. Both optional, so a legacy-shaped body still works.
    #[serde(default, with = "time::serde::rfc3339::option")]
    embargo_until: Option<OffsetDateTime>,
    #[serde(default, with = "time::serde::rfc3339::option")]
    expires_at: Option<OffsetDateTime>,
}

#[derive(Deserialize)]
struct CorrectionQuery {
    story_id: Option<Uuid>,
}

// --- Story handlers ---------------------------------------------------------

async fn list_stories(
    Extension(identity): Extension<IdentityService>,
    Extension(newsroom): Extension<NewsroomService>,
    Query(query): Query<StoryFilterQuery>,
    headers: HeaderMap,
) -> ApiResult<impl IntoResponse> {
    let actor = actor_from_request(&identity, &headers).await?;
    let filter = StoryFilter {
        workflow: query.workflow,
        publication: query.publication,
        desk_id: query.desk_id,
        author_id: query.author_id,
    };
    let stories = newsroom.list_stories(&actor, &filter).await.map_err(newsroom_error)?;
    Ok(Json(stories))
}

async fn create_story(
    AuthActor(actor): AuthActor,
    Extension(newsroom): Extension<NewsroomService>,
    Extension(hub): Extension<RealtimeHub>,
    Json(body): Json<StoryCreateBody>,
) -> ApiResult<impl IntoResponse> {
    let draft = StoryDraft {
        slug: body.slug,
        title: body.title,
        dek: body.dek,
        body: body.body,
        category: body.category,
        tags: body.tags,
        story_type: body.story_type,
        priority: body.priority,
        desk_id: body.desk_id,
        sub_sector_id: body.sub_sector_id,
        event_id: body.event_id,
        author_id: body.author_id,
        editor_id: body.editor_id,
        fact_checker_id: body.fact_checker_id,
        copy_editor_id: body.copy_editor_id,
    };
    let story = newsroom.create_story(&actor, &draft).await.map_err(newsroom_error)?;
    hub.emit("stories");
    Ok((StatusCode::CREATED, Json(story)))
}

async fn get_story(
    Extension(identity): Extension<IdentityService>,
    Extension(newsroom): Extension<NewsroomService>,
    Path(story_id): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<impl IntoResponse> {
    let actor = actor_from_request(&identity, &headers).await?;
    let story = newsroom.require_story_read(&actor, story_id).await.map_err(newsroom_error)?;
    Ok(Json(story))
}

/// `PATCH /api/v1/stories/{id}` — a partial story update. Carries the editorial
/// fields plus per-story rights (holder / notice / usage terms / licence) and reuse
/// constraints (embargo / territory); an omitted field is unchanged, an empty string
/// clears a clearable one back to the outlet default.
async fn update_story(
    AuthActor(actor): AuthActor,
    Extension(newsroom): Extension<NewsroomService>,
    Extension(hub): Extension<RealtimeHub>,
    Path(story_id): Path<Uuid>,
    Json(body): Json<StoryPatchBody>,
) -> ApiResult<impl IntoResponse> {
    let patch = StoryPatch {
        title: body.title,
        dek: body.dek,
        body: body.body,
        category: body.category,
        tags: body.tags,
        priority: body.priority,
        desk_id: body.desk_id,
        sub_sector_id: body.sub_sector_id,
        author_id: body.author_id,
        editor_id: body.editor_id,
        fact_checker_id: body.fact_checker_id,
        copy_editor_id: body.copy_editor_id,
        due_at: body.due_at,
        rights_holder: body.rights_holder,
        rights_notice: body.rights_notice,
        usage_terms: body.usage_terms,
        license_url: body.license_url,
        reuse_embargo_until: body.reuse_embargo_until,
        territory_mode: body.territory_mode,
        digital_source_type: body.digital_source_type,
        territories: body.territories,
    };
    let story = newsroom
        .update_story(&actor, story_id, &patch)
        .await
        .map_err(newsroom_error)?;
    hub.emit("stories");
    Ok(Json(story))
}

async fn delete_story(
    Extension(identity): Extension<IdentityService>,
    Extension(newsroom): Extension<NewsroomService>,
    Path(story_id): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<impl IntoResponse> {
    let actor = actor_from_request(&identity, &headers).await?;
    newsroom.delete_story(&actor, story_id).await.map_err(newsroom_error)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn list_versions(
    Extension(identity): Extension<IdentityService>,
    Extension(newsroom): Extension<NewsroomService>,
    Path(story_id): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<impl IntoResponse> {
    let actor = actor_from_request(&identity, &headers).await?;
    newsroom.require_story_read(&actor, story_id).await.map_err(newsroom_error)?;
    let versions = newsroom.list_versions(story_id).await.map_err(newsroom_error)?;
    Ok(Json(versions))
}

async fn save_version(
    AuthActor(actor): AuthActor,
    Extension(newsroom): Extension<NewsroomService>,
    Path(story_id): Path<Uuid>,
    Json(body): Json<VersionBody>,
) -> ApiResult<impl IntoResponse> {
    let story = newsroom.get_story(story_id).await.map_err(newsroom_error)?;
    let version = newsroom
        .save_version(&actor, &story, body.change_summary.as_deref(), body.filed)
        .await
        .map_err(newsroom_error)?;
    Ok((StatusCode::CREATED, Json(version)))
}

async fn transition(
    AuthActor(actor): AuthActor,
    Extension(newsroom): Extension<NewsroomService>,
    Extension(hub): Extension<RealtimeHub>,
    Path(story_id): Path<Uuid>,
    Json(body): Json<TransitionBody>,
) -> ApiResult<impl IntoResponse> {
    let story = newsroom.get_story(story_id).await.map_err(newsroom_error)?;
    let target = WorkflowState::from_wire(&body.state).ok_or_else(|| {
        newsroom_error(meridian_newsroom::NewsroomError::Unprocessable(format!(
            "'{}' is not a workflow state",
            body.state
        )))
    })?;
    let updated = newsroom
        .transition_story(&actor, &story, target, body.reason.as_deref(), body.fast_path)
        .await
        .map_err(newsroom_error)?;
    hub.emit("stories");
    Ok(Json(updated))
}

async fn readiness(
    Extension(identity): Extension<IdentityService>,
    Extension(newsroom): Extension<NewsroomService>,
    Path(story_id): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<impl IntoResponse> {
    actor_from_request(&identity, &headers).await?;
    let story = newsroom.get_story(story_id).await.map_err(newsroom_error)?;
    let readiness = newsroom.publish_readiness(&story).await.map_err(newsroom_error)?;
    Ok(Json(readiness))
}

// --- Pitch handlers ---------------------------------------------------------

async fn list_pitches(
    Extension(identity): Extension<IdentityService>,
    Extension(newsroom): Extension<NewsroomService>,
    Query(query): Query<PitchFilterQuery>,
    headers: HeaderMap,
) -> ApiResult<impl IntoResponse> {
    actor_from_request(&identity, &headers).await?;
    let filter = PitchFilter {
        status: query.status,
        desk_id: query.desk_id,
    };
    let pitches = newsroom.list_pitches(&filter).await.map_err(newsroom_error)?;
    Ok(Json(pitches))
}

async fn create_pitch(
    AuthActor(actor): AuthActor,
    Extension(newsroom): Extension<NewsroomService>,
    Json(body): Json<PitchCreateBody>,
) -> ApiResult<impl IntoResponse> {
    let draft = PitchDraft {
        headline: body.headline,
        summary: body.summary,
        angle: body.angle,
        desk_id: body.desk_id,
        event_id: body.event_id,
        assignee_id: body.assignee_id,
        editor_id: body.editor_id,
        priority: body.priority,
        target_audience: body.target_audience,
        expected_format: body.expected_format,
        key_questions: body.key_questions,
        likely_sources: body.likely_sources,
        risks: body.risks,
        target_at: body.target_at,
    };
    let pitch = newsroom.create_pitch(&actor, &draft).await.map_err(newsroom_error)?;
    Ok((StatusCode::CREATED, Json(pitch)))
}

async fn decide_pitch(
    AuthActor(actor): AuthActor,
    Extension(newsroom): Extension<NewsroomService>,
    Extension(hub): Extension<RealtimeHub>,
    Path(pitch_id): Path<Uuid>,
    Json(body): Json<PitchDecisionBody>,
) -> ApiResult<impl IntoResponse> {
    let decision = PitchDecision {
        status: body.status,
        note: body.note,
        assignee_id: body.assignee_id,
        editor_id: body.editor_id,
    };
    let outcome = newsroom
        .decide_pitch(&actor, pitch_id, &decision)
        .await
        .map_err(newsroom_error)?;
    // A commission decision can create a story, mirroring legacy's emit.
    hub.emit("stories");
    Ok(Json(outcome))
}

// --- Source handlers --------------------------------------------------------

async fn list_sources(
    Extension(identity): Extension<IdentityService>,
    Extension(newsroom): Extension<NewsroomService>,
    Path(story_id): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<impl IntoResponse> {
    let actor = actor_from_request(&identity, &headers).await?;
    let story = newsroom.get_story(story_id).await.map_err(newsroom_error)?;
    let sources = newsroom.list_sources(&actor, &story).await.map_err(newsroom_error)?;
    Ok(Json(sources))
}

async fn create_source(
    AuthActor(actor): AuthActor,
    Extension(newsroom): Extension<NewsroomService>,
    Path(story_id): Path<Uuid>,
    Json(body): Json<SourceCreateBody>,
) -> ApiResult<impl IntoResponse> {
    let story = newsroom.get_story(story_id).await.map_err(newsroom_error)?;
    let draft = SourceDraft {
        identity: body.identity,
        publishable_attribution: body.publishable_attribution,
        ground_rule: body.ground_rule,
        description: body.description,
        motive: body.motive,
        reliability_notes: body.reliability_notes,
        access_level: body.access_level,
    };
    let source = newsroom
        .create_source(&actor, &story, &draft)
        .await
        .map_err(newsroom_error)?;
    Ok((StatusCode::CREATED, Json(source)))
}

async fn approve_source(
    AuthActor(actor): AuthActor,
    Extension(newsroom): Extension<NewsroomService>,
    Path(source_id): Path<Uuid>,
    Json(body): Json<SourceApprovalBody>,
) -> ApiResult<impl IntoResponse> {
    let source = newsroom
        .approve_source(&actor, source_id, body.rationale.as_deref())
        .await
        .map_err(newsroom_error)?;
    Ok(Json(source))
}

async fn grant_source_access(
    AuthActor(actor): AuthActor,
    Extension(newsroom): Extension<NewsroomService>,
    Path(source_id): Path<Uuid>,
    Json(body): Json<SourceGrantBody>,
) -> ApiResult<impl IntoResponse> {
    newsroom
        .grant_source_access(&actor, source_id, body.user_id)
        .await
        .map_err(newsroom_error)?;
    Ok(StatusCode::NO_CONTENT)
}

// --- Evidence handlers ------------------------------------------------------

async fn list_evidence(
    Extension(identity): Extension<IdentityService>,
    Extension(newsroom): Extension<NewsroomService>,
    Path(story_id): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<impl IntoResponse> {
    let actor = actor_from_request(&identity, &headers).await?;
    newsroom.require_story_read(&actor, story_id).await.map_err(newsroom_error)?;
    let evidence = newsroom.list_evidence(story_id).await.map_err(newsroom_error)?;
    Ok(Json(evidence))
}

async fn create_evidence(
    AuthActor(actor): AuthActor,
    Extension(newsroom): Extension<NewsroomService>,
    Path(story_id): Path<Uuid>,
    Json(body): Json<EvidenceCreateBody>,
) -> ApiResult<impl IntoResponse> {
    let story = newsroom.get_story(story_id).await.map_err(newsroom_error)?;
    let draft = EvidenceDraft {
        source_id: body.source_id,
        kind: body.kind,
        title: body.title,
        uri: body.uri,
        notes: body.notes,
        checksum: body.checksum,
        acquired_at: body.acquired_at,
    };
    let evidence = newsroom
        .create_evidence(&actor, &story, &draft)
        .await
        .map_err(newsroom_error)?;
    Ok((StatusCode::CREATED, Json(evidence)))
}

// --- Claim handlers ---------------------------------------------------------

async fn list_claims(
    Extension(identity): Extension<IdentityService>,
    Extension(newsroom): Extension<NewsroomService>,
    Path(story_id): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<impl IntoResponse> {
    let actor = actor_from_request(&identity, &headers).await?;
    newsroom.require_story_read(&actor, story_id).await.map_err(newsroom_error)?;
    let claims = newsroom.list_claims(story_id).await.map_err(newsroom_error)?;
    Ok(Json(claims))
}

async fn create_claim(
    AuthActor(actor): AuthActor,
    Extension(newsroom): Extension<NewsroomService>,
    Path(story_id): Path<Uuid>,
    Json(body): Json<ClaimCreateBody>,
) -> ApiResult<impl IntoResponse> {
    let story = newsroom.get_story(story_id).await.map_err(newsroom_error)?;
    let draft = ClaimDraft {
        text: body.text,
        locator: body.locator,
        version_id: body.version_id,
        evidence_ids: body.evidence_ids,
    };
    let claim = newsroom.create_claim(&actor, &story, &draft).await.map_err(newsroom_error)?;
    Ok((StatusCode::CREATED, Json(claim)))
}

async fn decide_claim(
    AuthActor(actor): AuthActor,
    Extension(newsroom): Extension<NewsroomService>,
    Path(claim_id): Path<Uuid>,
    Json(body): Json<ClaimDecisionBody>,
) -> ApiResult<impl IntoResponse> {
    let claim = newsroom
        .decide_claim(&actor, claim_id, &body.status, body.note.as_deref())
        .await
        .map_err(newsroom_error)?;
    Ok(Json(claim))
}

/// `POST /api/v1/evidence/{id}/status` — move an evidence item's verification
/// status, appending the change to its chain of custody.
async fn set_evidence_status(
    AuthActor(actor): AuthActor,
    Extension(newsroom): Extension<NewsroomService>,
    Path(evidence_id): Path<Uuid>,
    Json(body): Json<ClaimDecisionBody>,
) -> ApiResult<impl IntoResponse> {
    let evidence = newsroom
        .set_evidence_status(&actor, evidence_id, &body.status, body.note.as_deref())
        .await
        .map_err(newsroom_error)?;
    Ok(Json(evidence))
}

/// `POST /api/v1/assets/{id}/verification` — move an asset's verification status.
/// This is the resolution path for publish-gate blocker #8 ("every asset verified").
async fn set_asset_verification(
    AuthActor(actor): AuthActor,
    Extension(newsroom): Extension<NewsroomService>,
    Path(asset_id): Path<Uuid>,
    Json(body): Json<ClaimDecisionBody>,
) -> ApiResult<impl IntoResponse> {
    let asset = newsroom
        .set_asset_verification(&actor, asset_id, &body.status)
        .await
        .map_err(newsroom_error)?;
    Ok(Json(asset))
}

#[derive(Deserialize)]
struct AssetPatchBody {
    #[serde(default)]
    rights: Option<String>,
    #[serde(default)]
    alt_text: Option<String>,
}

/// `PATCH /api/v1/assets/{id}` — edit rights / alt text after creation, so the
/// rights and alt-text publish-gate blockers can be cleared without re-uploading.
async fn update_asset(
    AuthActor(actor): AuthActor,
    Extension(newsroom): Extension<NewsroomService>,
    Path(asset_id): Path<Uuid>,
    Json(body): Json<AssetPatchBody>,
) -> ApiResult<impl IntoResponse> {
    let asset = newsroom
        .update_asset_metadata(&actor, asset_id, body.rights, body.alt_text)
        .await
        .map_err(newsroom_error)?;
    Ok(Json(asset))
}

// --- Drive folders ----------------------------------------------------------

#[derive(Deserialize)]
struct FolderCreateBody {
    name: String,
    #[serde(default)]
    parent_id: Option<Uuid>,
}

#[derive(Deserialize)]
struct FolderRenameBody {
    name: String,
}

#[derive(Deserialize)]
struct MoveAssetBody {
    /// Target folder, or `null` to move the asset back to the root (Unfiled).
    #[serde(default)]
    folder_id: Option<Uuid>,
}

/// `GET /api/v1/folders` — every Drive folder with its asset count.
async fn list_folders(
    Extension(identity): Extension<IdentityService>,
    Extension(newsroom): Extension<NewsroomService>,
    headers: HeaderMap,
) -> ApiResult<impl IntoResponse> {
    actor_from_request(&identity, &headers).await?;
    let folders = newsroom.list_folders().await.map_err(newsroom_error)?;
    Ok(Json(folders))
}

/// `POST /api/v1/folders` — create a Drive folder.
async fn create_folder(
    AuthActor(actor): AuthActor,
    Extension(newsroom): Extension<NewsroomService>,
    Json(body): Json<FolderCreateBody>,
) -> ApiResult<impl IntoResponse> {
    let folder = newsroom
        .create_folder(&actor, &body.name, body.parent_id)
        .await
        .map_err(newsroom_error)?;
    Ok((StatusCode::CREATED, Json(folder)))
}

/// `PATCH /api/v1/folders/{id}` — rename a Drive folder.
async fn rename_folder(
    AuthActor(actor): AuthActor,
    Extension(newsroom): Extension<NewsroomService>,
    Path(folder_id): Path<Uuid>,
    Json(body): Json<FolderRenameBody>,
) -> ApiResult<impl IntoResponse> {
    let folder = newsroom
        .rename_folder(&actor, folder_id, &body.name)
        .await
        .map_err(newsroom_error)?;
    Ok(Json(folder))
}

/// `DELETE /api/v1/folders/{id}` — delete a folder; its assets re-file to root.
async fn delete_folder(
    AuthActor(actor): AuthActor,
    Extension(newsroom): Extension<NewsroomService>,
    Path(folder_id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    newsroom.delete_folder(&actor, folder_id).await.map_err(newsroom_error)?;
    Ok(StatusCode::NO_CONTENT)
}

/// `PUT /api/v1/assets/{id}/folder` — move an asset into a folder (or to root).
async fn move_asset(
    AuthActor(actor): AuthActor,
    Extension(newsroom): Extension<NewsroomService>,
    Path(asset_id): Path<Uuid>,
    Json(body): Json<MoveAssetBody>,
) -> ApiResult<impl IntoResponse> {
    let asset = newsroom
        .set_asset_folder(&actor, asset_id, body.folder_id)
        .await
        .map_err(newsroom_error)?;
    Ok(Json(asset))
}

#[derive(Deserialize)]
struct RenameAssetBody {
    filename: String,
}

/// `PUT /api/v1/assets/{id}/rename` — give a file a new display name.
async fn rename_asset(
    AuthActor(actor): AuthActor,
    Extension(newsroom): Extension<NewsroomService>,
    Path(asset_id): Path<Uuid>,
    Json(body): Json<RenameAssetBody>,
) -> ApiResult<impl IntoResponse> {
    let asset = newsroom
        .rename_asset(&actor, asset_id, &body.filename)
        .await
        .map_err(newsroom_error)?;
    Ok(Json(asset))
}

// --- Review / approval / comment handlers -----------------------------------

async fn request_review(
    AuthActor(actor): AuthActor,
    Extension(newsroom): Extension<NewsroomService>,
    Path(story_id): Path<Uuid>,
    Json(body): Json<ReviewCreateBody>,
) -> ApiResult<impl IntoResponse> {
    let request = ReviewRequest {
        version_id: body.version_id,
        kind: body.kind,
        assigned_to_id: body.assigned_to_id,
    };
    let review = newsroom
        .request_review(&actor, story_id, &request)
        .await
        .map_err(newsroom_error)?;
    Ok((StatusCode::CREATED, Json(review)))
}

async fn list_reviews(
    Extension(identity): Extension<IdentityService>,
    Extension(newsroom): Extension<NewsroomService>,
    Query(query): Query<ReviewFilterQuery>,
    headers: HeaderMap,
) -> ApiResult<impl IntoResponse> {
    let actor = actor_from_request(&identity, &headers).await?;
    let filter = ReviewFilter {
        decision: query.decision,
        story_id: query.story_id,
    };
    let reviews = newsroom.list_reviews(&actor, &filter).await.map_err(newsroom_error)?;
    Ok(Json(reviews))
}

async fn decide_review(
    AuthActor(actor): AuthActor,
    Extension(newsroom): Extension<NewsroomService>,
    Extension(hub): Extension<RealtimeHub>,
    Path(review_id): Path<Uuid>,
    Json(body): Json<ReviewDecisionBody>,
) -> ApiResult<impl IntoResponse> {
    let review = newsroom
        .decide_review(&actor, review_id, &body.decision, body.notes.as_deref())
        .await
        .map_err(newsroom_error)?;
    hub.emit("reviews");
    hub.emit("stories");
    Ok(Json(review))
}

async fn list_approvals(
    Extension(identity): Extension<IdentityService>,
    Extension(newsroom): Extension<NewsroomService>,
    Path(story_id): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<impl IntoResponse> {
    let actor = actor_from_request(&identity, &headers).await?;
    newsroom.require_story_read(&actor, story_id).await.map_err(newsroom_error)?;
    let approvals = newsroom.list_approvals(story_id).await.map_err(newsroom_error)?;
    Ok(Json(approvals))
}

async fn create_comment(
    AuthActor(actor): AuthActor,
    Extension(newsroom): Extension<NewsroomService>,
    Path(story_id): Path<Uuid>,
    Json(body): Json<CommentCreateBody>,
) -> ApiResult<impl IntoResponse> {
    let draft = CommentDraft {
        body: body.body,
        locator: body.locator,
        version_id: body.version_id,
    };
    let comment = newsroom
        .create_comment(&actor, story_id, &draft)
        .await
        .map_err(newsroom_error)?;
    Ok((StatusCode::CREATED, Json(comment)))
}

async fn list_comments(
    Extension(identity): Extension<IdentityService>,
    Extension(newsroom): Extension<NewsroomService>,
    Path(story_id): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<impl IntoResponse> {
    let actor = actor_from_request(&identity, &headers).await?;
    newsroom.require_story_read(&actor, story_id).await.map_err(newsroom_error)?;
    let comments = newsroom.list_comments(story_id).await.map_err(newsroom_error)?;
    Ok(Json(comments))
}

async fn resolve_comment(
    Extension(identity): Extension<IdentityService>,
    Extension(newsroom): Extension<NewsroomService>,
    Path(comment_id): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<impl IntoResponse> {
    let actor = actor_from_request(&identity, &headers).await?;
    let comment = newsroom.resolve_comment(&actor, comment_id).await.map_err(newsroom_error)?;
    Ok(Json(comment))
}

async fn delete_comment(
    Extension(identity): Extension<IdentityService>,
    Extension(newsroom): Extension<NewsroomService>,
    Path(comment_id): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<impl IntoResponse> {
    let actor = actor_from_request(&identity, &headers).await?;
    newsroom.delete_comment(&actor, comment_id).await.map_err(newsroom_error)?;
    Ok(StatusCode::NO_CONTENT)
}

// --- Task handlers ----------------------------------------------------------

async fn list_tasks(
    Extension(identity): Extension<IdentityService>,
    Extension(newsroom): Extension<NewsroomService>,
    Query(query): Query<TaskFilterQuery>,
    headers: HeaderMap,
) -> ApiResult<impl IntoResponse> {
    let actor = actor_from_request(&identity, &headers).await?;
    let filter = TaskFilter {
        assigned_to_id: query.assigned_to_id,
        status: query.status,
        story_id: query.story_id,
        desk_id: query.desk_id,
    };
    let tasks = newsroom.list_tasks(&actor, &filter).await.map_err(newsroom_error)?;
    Ok(Json(tasks))
}

async fn create_task(
    AuthActor(actor): AuthActor,
    Extension(newsroom): Extension<NewsroomService>,
    Extension(hub): Extension<RealtimeHub>,
    Json(body): Json<TaskCreateBody>,
) -> ApiResult<impl IntoResponse> {
    let draft = TaskDraft {
        title: body.title,
        description: body.description,
        status: body.status,
        priority: body.priority,
        story_id: body.story_id,
        desk_id: body.desk_id,
        pitch_id: body.pitch_id,
        event_id: body.event_id,
        assigned_to_id: body.assigned_to_id,
        due_at: body.due_at,
    };
    let task = newsroom.create_task(&actor, &draft).await.map_err(newsroom_error)?;
    hub.emit("tasks");
    Ok((StatusCode::CREATED, Json(task)))
}

async fn update_task(
    AuthActor(actor): AuthActor,
    Extension(newsroom): Extension<NewsroomService>,
    Extension(hub): Extension<RealtimeHub>,
    Path(task_id): Path<Uuid>,
    Json(body): Json<TaskPatchBody>,
) -> ApiResult<impl IntoResponse> {
    let patch = TaskPatch {
        title: body.title,
        description: body.description,
        status: body.status,
        priority: body.priority,
        assigned_to_id: body.assigned_to_id,
        due_at: body.due_at,
        blocker: body.blocker,
    };
    let task = newsroom.update_task(&actor, task_id, &patch).await.map_err(newsroom_error)?;
    hub.emit("tasks");
    Ok(Json(task))
}

async fn delete_task(
    Extension(identity): Extension<IdentityService>,
    Extension(newsroom): Extension<NewsroomService>,
    Extension(hub): Extension<RealtimeHub>,
    Path(task_id): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<impl IntoResponse> {
    let actor = actor_from_request(&identity, &headers).await?;
    newsroom.delete_task(&actor, task_id).await.map_err(newsroom_error)?;
    hub.emit("tasks");
    Ok(StatusCode::NO_CONTENT)
}

// --- Coverage-event handlers ------------------------------------------------

async fn list_coverage_events(
    Extension(identity): Extension<IdentityService>,
    Extension(newsroom): Extension<NewsroomService>,
    headers: HeaderMap,
) -> ApiResult<impl IntoResponse> {
    actor_from_request(&identity, &headers).await?;
    let events = newsroom.list_coverage_events().await.map_err(newsroom_error)?;
    Ok(Json(events))
}

async fn create_coverage_event(
    AuthActor(actor): AuthActor,
    Extension(newsroom): Extension<NewsroomService>,
    Json(body): Json<CoverageCreateBody>,
) -> ApiResult<impl IntoResponse> {
    let draft = CoverageDraft {
        title: body.title,
        description: body.description,
        desk_id: body.desk_id,
        starts_at: body.starts_at,
        ends_at: body.ends_at,
        status: body.status,
        location: body.location,
    };
    let event = newsroom
        .create_coverage_event(&actor, &draft)
        .await
        .map_err(newsroom_error)?;
    Ok((StatusCode::CREATED, Json(event)))
}

async fn update_coverage_event(
    AuthActor(actor): AuthActor,
    Extension(newsroom): Extension<NewsroomService>,
    Path(event_id): Path<Uuid>,
    Json(body): Json<CoveragePatchBody>,
) -> ApiResult<impl IntoResponse> {
    let patch = CoveragePatch {
        title: body.title,
        description: body.description,
        desk_id: body.desk_id,
        starts_at: body.starts_at,
        ends_at: body.ends_at,
        status: body.status,
        location: body.location,
    };
    let event = newsroom
        .update_coverage_event(&actor, event_id, &patch)
        .await
        .map_err(newsroom_error)?;
    Ok(Json(event))
}

async fn delete_coverage_event(
    Extension(identity): Extension<IdentityService>,
    Extension(newsroom): Extension<NewsroomService>,
    Path(event_id): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<impl IntoResponse> {
    let actor = actor_from_request(&identity, &headers).await?;
    newsroom
        .delete_coverage_event(&actor, event_id)
        .await
        .map_err(newsroom_error)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn desk_analytics(
    Extension(identity): Extension<IdentityService>,
    Extension(newsroom): Extension<NewsroomService>,
    Path(desk_id): Path<Uuid>,
    Query(query): Query<AnalyticsQuery>,
    headers: HeaderMap,
) -> ApiResult<impl IntoResponse> {
    let actor = actor_from_request(&identity, &headers).await?;
    let analytics = newsroom
        .desk_analytics(&actor, desk_id, query.include_sub_desks)
        .await
        .map_err(newsroom_error)?;
    Ok(Json(analytics))
}

// --- Media / publishing handlers --------------------------------------------

async fn list_assets(
    Extension(identity): Extension<IdentityService>,
    Extension(newsroom): Extension<NewsroomService>,
    Query(query): Query<AssetQuery>,
    headers: HeaderMap,
) -> ApiResult<impl IntoResponse> {
    actor_from_request(&identity, &headers).await?;
    let assets = newsroom.list_assets(query.story_id).await.map_err(newsroom_error)?;
    // Attach a short-lived signed public URL to every stored asset so the Drive
    // can render real previews (images, video first-frame, PDF first page) with a
    // plain `<img>`/`<video>`/`<embed>` — no auth header, CSP-safe, no thumbnail
    // files to generate or store.
    let secret = std::env::var("MERIDIAN_AUTH_SECRET").unwrap_or_default();
    let exp = time::OffsetDateTime::now_utc().unix_timestamp() + 24 * 60 * 60;
    let enriched: Vec<serde_json::Value> = assets
        .into_iter()
        .map(|asset| {
            let mut value = serde_json::to_value(&asset).unwrap_or_else(|_| serde_json::json!({}));
            if asset.object_key.is_some() {
                let sig = asset_signature(secret.as_bytes(), asset.id, exp);
                value["preview_url"] = serde_json::json!(format!(
                    "/api/v1/assets/{}/public?exp={exp}&sig={sig}",
                    asset.id
                ));
            }
            value
        })
        .collect();
    Ok(Json(enriched))
}

async fn create_asset(
    AuthActor(actor): AuthActor,
    Extension(newsroom): Extension<NewsroomService>,
    Json(body): Json<AssetCreateBody>,
) -> ApiResult<impl IntoResponse> {
    let draft = AssetDraft {
        story_id: body.story_id,
        kind: body.kind,
        uri: body.uri,
        filename: body.filename,
        creator: body.creator,
        source: body.source,
        rights: body.rights,
        credit: body.credit,
        caption: body.caption,
        alt_text: body.alt_text,
    };
    let asset = newsroom.create_asset(&actor, &draft).await.map_err(newsroom_error)?;
    Ok((StatusCode::CREATED, Json(asset)))
}

async fn delete_asset(
    Extension(identity): Extension<IdentityService>,
    Extension(newsroom): Extension<NewsroomService>,
    Extension(media): Extension<MediaStore>,
    Path(asset_id): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<impl IntoResponse> {
    let actor = actor_from_request(&identity, &headers).await?;
    // Note the bytes before deleting, but delete the metadata row FIRST — that call
    // is the authorization boundary, so an unauthorized caller is rejected before any
    // object is touched. Only once the row is gone (proving the delete was allowed)
    // do we reclaim the stored bytes.
    let object_key = newsroom
        .get_asset(asset_id)
        .await
        .ok()
        .and_then(|asset| asset.object_key);
    newsroom.delete_asset(&actor, asset_id).await.map_err(newsroom_error)?;
    if let Some(object_key) = object_key {
        let _ = media.delete(&object_key).await;
    }
    Ok(StatusCode::NO_CONTENT)
}

fn upload_error(detail: &str) -> (StatusCode, Json<Value>) {
    (
        StatusCode::UNPROCESSABLE_ENTITY,
        Json(serde_json::json!({ "detail": detail })),
    )
}

/// Infers a coarse asset kind from a content type, as the legacy uploader does.
fn kind_for(content_type: &str) -> &'static str {
    if content_type.starts_with("image/") {
        "image"
    } else if content_type.starts_with("video/") {
        "video"
    } else if content_type.starts_with("audio/") {
        "audio"
    } else {
        "document"
    }
}

/// Compresses an uploaded image: dimensions above `MAX_DIM` are downscaled and
/// the pixels re-encoded (JPEG q82 / PNG). Returns the possibly-smaller bytes and
/// the content type. Non-images, undecodable data, or results that aren't smaller
/// keep the original bytes — compression is a best-effort optimisation, never a
/// gate on the upload succeeding.
#[cfg(feature = "server")]
fn compress_image(bytes: Vec<u8>, content_type: &str) -> (Vec<u8>, String) {
    const MAX_DIM: u32 = 2400;
    let ct = content_type.to_ascii_lowercase();
    let is_jpeg = ct == "image/jpeg" || ct == "image/jpg";
    let is_png = ct == "image/png";
    if !is_jpeg && !is_png {
        return (bytes, content_type.to_owned());
    }
    let Ok(image) = image::load_from_memory(&bytes) else {
        return (bytes, content_type.to_owned());
    };
    let resized = if image.width() > MAX_DIM || image.height() > MAX_DIM {
        image.resize(MAX_DIM, MAX_DIM, image::imageops::FilterType::Lanczos3)
    } else {
        image
    };
    let mut out = std::io::Cursor::new(Vec::new());
    let encoded = if is_jpeg {
        let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 82);
        resized.write_with_encoder(encoder)
    } else {
        resized.write_to(&mut out, image::ImageFormat::Png)
    };
    match encoded {
        Ok(()) => {
            let compressed = out.into_inner();
            if !compressed.is_empty() && compressed.len() < bytes.len() {
                (compressed, content_type.to_owned())
            } else {
                (bytes, content_type.to_owned())
            }
        }
        Err(_) => (bytes, content_type.to_owned()),
    }
}

async fn upload_asset(
    AuthActor(actor): AuthActor,
    Extension(newsroom): Extension<NewsroomService>,
    Extension(media): Extension<MediaStore>,
    mut multipart: Multipart,
) -> ApiResult<impl IntoResponse> {

    let mut bytes: Option<Vec<u8>> = None;
    let mut filename: Option<String> = None;
    let mut content_type = "application/octet-stream".to_owned();
    let mut story_id: Option<Uuid> = None;
    let mut alt_text: Option<String> = None;

    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|error| upload_error(&format!("Malformed upload: {error}")))?
    {
        match field.name() {
            Some("file") => {
                filename = field.file_name().map(ToOwned::to_owned);
                if let Some(ct) = field.content_type() {
                    content_type = ct.to_owned();
                }
                bytes = Some(
                    field
                        .bytes()
                        .await
                        .map_err(|error| upload_error(&format!("Could not read file: {error}")))?
                        .to_vec(),
                );
            }
            Some("story_id") => {
                let value = field.text().await.unwrap_or_default();
                if !value.is_empty() {
                    story_id = Some(
                        value
                            .parse()
                            .map_err(|_| upload_error("story_id is not a valid id"))?,
                    );
                }
            }
            Some("alt_text") => {
                alt_text = field.text().await.ok().filter(|value| !value.is_empty());
            }
            _ => {}
        }
    }

    let bytes = bytes.ok_or_else(|| upload_error("A 'file' part is required"))?;
    if bytes.is_empty() {
        return Err(upload_error("The uploaded file is empty"));
    }
    // Downscale oversized dimensions and recompress uploaded images before they
    // are stored, so the object store (and every reader) carries lighter files.
    let (bytes, content_type) = compress_image(bytes, &content_type);
    let byte_size = i64::try_from(bytes.len()).unwrap_or(i64::MAX);
    let checksum = {
        let digest = Sha256::digest(&bytes);
        let mut hex = String::with_capacity(64);
        for byte in digest {
            use std::fmt::Write as _;
            let _ = write!(hex, "{byte:02x}");
        }
        hex
    };
    let kind = kind_for(&content_type).to_owned();
    let safe_name = filename.clone().unwrap_or_else(|| "upload".to_owned());
    let object_key = format!("assets/{}/{}", Uuid::now_v7(), safe_name);

    // Object first, then metadata — no row before its bytes exist (gate #6).
    media
        .upload(&object_key, &content_type, bytes)
        .await
        .map_err(|error| {
            (
                StatusCode::BAD_GATEWAY,
                Json(serde_json::json!({ "detail": format!("Object store upload failed: {error}") })),
            )
        })?;

    let stored = StoredAsset {
        story_id,
        kind,
        uri: format!("s3://{}/{}", media.bucket(), object_key),
        bucket: media.bucket().to_owned(),
        object_key,
        filename,
        byte_size,
        checksum,
        alt_text,
    };
    let asset = newsroom
        .create_uploaded_asset(&actor, &stored)
        .await
        .map_err(newsroom_error)?;
    Ok((StatusCode::CREATED, Json(asset)))
}

async fn cleanup_orphans(
    AuthActor(actor): AuthActor,
    Extension(newsroom): Extension<NewsroomService>,
    Extension(media): Extension<MediaStore>,
) -> ApiResult<impl IntoResponse> {
    // Reclaiming stored bytes is an administrative operation.
    if !actor.is_admin {
        return Err((
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({ "detail": "An admin role is required" })),
        ));
    }
    let bad_gateway = |error: String| {
        (
            StatusCode::BAD_GATEWAY,
            Json(serde_json::json!({ "detail": format!("Object store error: {error}") })),
        )
    };
    // Every object we store, and the subset still referenced by a metadata row;
    // the difference is orphaned bytes (gate #6).
    let stored = media
        .list_objects("assets/")
        .await
        .map_err(|error| bad_gateway(error.to_string()))?;
    let referenced = newsroom
        .existing_object_keys(&stored)
        .await
        .map_err(newsroom_error)?;
    let mut deleted = 0_u64;
    for key in &stored {
        if !referenced.contains(key) && media.delete(key).await.is_ok() {
            deleted += 1;
        }
    }
    Ok(Json(serde_json::json!({
        "scanned": stored.len(),
        "referenced": referenced.len(),
        "orphans_deleted": deleted,
    })))
}

async fn download_asset(
    Extension(identity): Extension<IdentityService>,
    Extension(newsroom): Extension<NewsroomService>,
    Extension(media): Extension<MediaStore>,
    Path(asset_id): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<impl IntoResponse> {
    actor_from_request(&identity, &headers).await?;
    let asset = newsroom.get_asset(asset_id).await.map_err(newsroom_error)?;
    let Some(object_key) = asset.object_key else {
        // Externally-referenced assets are not stored by us; hand back their URI.
        return Ok(Json(serde_json::json!({ "url": asset.uri, "external": true })));
    };
    let url = media
        .presign_get(&object_key, Duration::from_mins(5))
        .await
        .map_err(|error| {
            (
                StatusCode::BAD_GATEWAY,
                Json(serde_json::json!({ "detail": format!("Presign failed: {error}") })),
            )
        })?;
    Ok(Json(serde_json::json!({ "url": url, "expires_in": 300 })))
}

/// `GET /api/v1/assets/{id}/raw` — streams the object's bytes through the app's
/// own origin. Presigned object-store URLs live on a different host, which the
/// strict `img-src 'self'` CSP blocks; serving the bytes here keeps images on
/// the same origin. Authenticated, like the presign endpoint.
async fn raw_asset(
    Extension(identity): Extension<IdentityService>,
    Extension(newsroom): Extension<NewsroomService>,
    Extension(media): Extension<MediaStore>,
    Path(asset_id): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<impl IntoResponse> {
    actor_from_request(&identity, &headers).await?;
    let asset = newsroom.get_asset(asset_id).await.map_err(newsroom_error)?;
    let Some(object_key) = asset.object_key else {
        return Err((
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "detail": "Asset has no stored object" })),
        ));
    };
    serve_object(&media, &object_key, &headers).await
}

/// Streams an object's bytes, honouring a `Range:` header with 206 Partial
/// Content (video/audio seeking) and always advertising `Accept-Ranges: bytes`.
/// Shared by the authed raw endpoint and the signed public preview.
#[cfg(feature = "server")]
async fn serve_object(
    media: &MediaStore,
    object_key: &str,
    headers: &HeaderMap,
) -> ApiResult<axum::response::Response> {
    if let Some(range) = headers
        .get(axum::http::header::RANGE)
        .and_then(|value| value.to_str().ok())
        .filter(|value| value.starts_with("bytes="))
    {
        let (bytes, content_type, content_range, _total) = media
            .get_object_range(object_key, range)
            .await
            .map_err(|error| {
                (
                    StatusCode::BAD_GATEWAY,
                    Json(serde_json::json!({ "detail": format!("Range fetch failed: {error}") })),
                )
            })?;
        let content_type = content_type.unwrap_or_else(|| "application/octet-stream".to_owned());
        let mut builder = axum::http::Response::builder()
            .status(StatusCode::PARTIAL_CONTENT)
            .header(axum::http::header::CONTENT_TYPE, content_type)
            .header(axum::http::header::ACCEPT_RANGES, "bytes")
            .header(axum::http::header::CACHE_CONTROL, "private, max-age=300");
        if let Some(range_header) = content_range {
            builder = builder.header(axum::http::header::CONTENT_RANGE, range_header);
        }
        let response = builder
            .body(axum::body::Body::from(bytes))
            .map_err(|error| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({ "detail": format!("Response build failed: {error}") })),
                )
            })?;
        return Ok(response.into_response());
    }
    let (bytes, content_type) = media.get_object(object_key).await.map_err(|error| {
        (
            StatusCode::BAD_GATEWAY,
            Json(serde_json::json!({ "detail": format!("Fetch failed: {error}") })),
        )
    })?;
    let content_type = content_type.unwrap_or_else(|| "application/octet-stream".to_owned());
    Ok((
        [
            (axum::http::header::CONTENT_TYPE, content_type),
            (axum::http::header::ACCEPT_RANGES, "bytes".to_owned()),
            (
                axum::http::header::CACHE_CONTROL,
                "private, max-age=300".to_owned(),
            ),
        ],
        bytes,
    )
        .into_response())
}

/// `HMAC-SHA256(secret, "{asset_id}.{exp}")` as hex — signs a public preview
/// link. Built directly over `sha2` to avoid a dedicated hmac dependency.
#[cfg(feature = "server")]
fn asset_signature(secret: &[u8], asset_id: Uuid, exp: i64) -> String {
    use sha2::{Digest, Sha256};
    let mut key = [0u8; 64];
    if secret.len() > 64 {
        key[..32].copy_from_slice(&Sha256::digest(secret));
    } else {
        key[..secret.len()].copy_from_slice(secret);
    }
    let mut ipad = [0x36u8; 64];
    let mut opad = [0x5cu8; 64];
    for index in 0..64 {
        ipad[index] ^= key[index];
        opad[index] ^= key[index];
    }
    let message = format!("{asset_id}.{exp}");
    let inner = {
        let mut hasher = Sha256::new();
        hasher.update(ipad);
        hasher.update(message.as_bytes());
        hasher.finalize()
    };
    let outer = {
        let mut hasher = Sha256::new();
        hasher.update(opad);
        hasher.update(inner);
        hasher.finalize()
    };
    let mut hex = String::with_capacity(64);
    for byte in outer {
        use std::fmt::Write as _;
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

#[cfg(feature = "server")]
#[derive(serde::Deserialize)]
struct PublicAssetQuery {
    exp: i64,
    sig: String,
}

/// `GET /api/v1/assets/{id}/public?exp&sig` — a signed, expiring, **no-auth**
/// preview of an asset, for shareable links. Verifies the HMAC and expiry, then
/// streams the object (range-aware) exactly like the authed endpoint.
#[cfg(feature = "server")]
async fn public_asset(
    Extension(newsroom): Extension<NewsroomService>,
    Extension(media): Extension<MediaStore>,
    Path(asset_id): Path<Uuid>,
    Query(query): Query<PublicAssetQuery>,
    headers: HeaderMap,
) -> ApiResult<impl IntoResponse> {
    let now = time::OffsetDateTime::now_utc().unix_timestamp();
    if query.exp < now {
        return Err((
            StatusCode::GONE,
            Json(serde_json::json!({ "detail": "This link has expired" })),
        ));
    }
    let secret = std::env::var("MERIDIAN_AUTH_SECRET").unwrap_or_default();
    let expected = asset_signature(secret.as_bytes(), asset_id, query.exp);
    // Length-checked, constant-fold byte compare — reject forgeries without
    // leaking timing on the first differing byte.
    let valid = expected.len() == query.sig.len()
        && expected
            .bytes()
            .zip(query.sig.bytes())
            .fold(0u8, |acc, (a, b)| acc | (a ^ b))
            == 0;
    if !valid {
        return Err((
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({ "detail": "Invalid or missing signature" })),
        ));
    }
    let asset = newsroom.get_asset(asset_id).await.map_err(newsroom_error)?;
    let Some(object_key) = asset.object_key else {
        return Err((
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "detail": "Asset has no stored object" })),
        ));
    };
    serve_object(&media, &object_key, &headers).await
}

/// `GET /api/v1/assets/{id}/share-url` — authed; mints a 7-day signed public
/// preview link the caller can share externally.
#[cfg(feature = "server")]
async fn asset_share_url(
    AuthActor(_actor): AuthActor,
    Path(asset_id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let secret = std::env::var("MERIDIAN_AUTH_SECRET").unwrap_or_default();
    let exp = time::OffsetDateTime::now_utc().unix_timestamp() + 7 * 24 * 60 * 60;
    let sig = asset_signature(secret.as_bytes(), asset_id, exp);
    Ok(Json(serde_json::json!({
        "url": format!("/api/v1/assets/{asset_id}/public?exp={exp}&sig={sig}"),
        "expires_at": exp,
    })))
}

#[cfg(feature = "server")]
#[derive(serde::Deserialize)]
struct LegalSignBody {
    document_id: Option<String>,
    title: String,
    /// The document's Markdown content.
    content: String,
    party_id: String,
    party_name: String,
    #[serde(default)]
    party_role: String,
}

/// `POST /api/v1/legal/sign` — authed; executes one party's ML-DSA signature over
/// a Markdown legal document (via its SHA-256 digest) and returns the detached
/// party signature plus the published public key, digest and algorithm. The caller
/// stores the document + accumulating signatures (e.g. as a Drive asset).
#[cfg(feature = "server")]
async fn legal_sign(
    AuthActor(_actor): AuthActor,
    Json(body): Json<LegalSignBody>,
) -> ApiResult<impl IntoResponse> {
    use meridian_newsroom::legal_signing::{content_digest, LegalSigner, LEGAL_SIGNING_ALG};
    let signer = LegalSigner::from_env();
    let document_id = body.document_id.clone().unwrap_or_else(|| Uuid::now_v7().to_string());
    let digest = content_digest(&body.content);
    let signed_at = time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default();
    let signature = signer
        .sign_party(
            &document_id,
            &body.title,
            &digest,
            &body.party_id,
            &body.party_name,
            &body.party_role,
            &signed_at,
        )
        .ok_or_else(|| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "detail": "Signing failed" })),
            )
        })?;
    Ok(Json(serde_json::json!({
        "document_id": document_id,
        "title": body.title,
        "content_sha256": digest,
        "algorithm": LEGAL_SIGNING_ALG,
        "public_key": signer.public_key_b64(),
        "signature": signature,
    })))
}

#[cfg(feature = "server")]
#[derive(serde::Deserialize)]
struct LegalVerifyBody {
    document_id: String,
    title: String,
    content: String,
    signatures: Vec<meridian_newsroom::legal_signing::PartySignature>,
}

/// `POST /api/v1/legal/verify` — **public** (no auth); re-derives the content
/// digest and checks every party signature against the published key. Returns the
/// per-party validity and whether the document is fully executed (all parties
/// signed the current content). Editing the Markdown invalidates every signature.
#[cfg(feature = "server")]
async fn legal_verify(
    Json(body): Json<LegalVerifyBody>,
) -> ApiResult<impl IntoResponse> {
    use meridian_newsroom::legal_signing::{content_digest, LegalSigner};
    let signer = LegalSigner::from_env();
    let digest = content_digest(&body.content);
    let results: Vec<serde_json::Value> = body
        .signatures
        .iter()
        .map(|party| {
            let valid = signer.verify_party(&body.document_id, &body.title, &digest, party);
            serde_json::json!({
                "party_id": party.party_id,
                "party_name": party.party_name,
                "role": party.party_role,
                "signed_at": party.signed_at,
                "valid": valid,
            })
        })
        .collect();
    let fully_executed = !body.signatures.is_empty()
        && body
            .signatures
            .iter()
            .all(|party| signer.verify_party(&body.document_id, &body.title, &digest, party));
    Ok(Json(serde_json::json!({
        "content_sha256": digest,
        "public_key": signer.public_key_b64(),
        "party_count": body.signatures.len(),
        "fully_executed": fully_executed,
        "results": results,
    })))
}

// --- Legal documents: persisted, multi-party, in-platform signing ----------

#[derive(Deserialize)]
struct LegalDocCreateBody {
    title: String,
    body_md: String,
}

#[derive(Deserialize)]
struct LegalPartyBody {
    /// Email or handle of a registered platform user.
    identifier: String,
    #[serde(default)]
    role: Option<String>,
    /// "signatory" (default) or "cc" (viewer).
    #[serde(default)]
    kind: Option<String>,
    /// Optional 1-based signing order for sequential signing.
    #[serde(default)]
    sign_order: Option<i32>,
}

/// `POST /api/v1/legal/documents` — create a legal document from markdown.
async fn create_legal_document(
    AuthActor(actor): AuthActor,
    Extension(newsroom): Extension<NewsroomService>,
    Json(body): Json<LegalDocCreateBody>,
) -> ApiResult<impl IntoResponse> {
    let doc = newsroom
        .create_legal_document(&actor, &body.title, &body.body_md)
        .await
        .map_err(newsroom_error)?;
    Ok((StatusCode::CREATED, Json(doc)))
}

/// `GET /api/v1/legal/documents` — documents the caller owns or is a party to.
async fn list_legal_documents(
    AuthActor(actor): AuthActor,
    Extension(newsroom): Extension<NewsroomService>,
) -> ApiResult<impl IntoResponse> {
    let docs = newsroom.list_legal_documents(&actor).await.map_err(newsroom_error)?;
    Ok(Json(docs))
}

/// `GET /api/v1/legal/documents/{id}` — a document plus its parties.
async fn get_legal_document(
    AuthActor(actor): AuthActor,
    Extension(newsroom): Extension<NewsroomService>,
    Path(doc_id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let (document, parties) = newsroom
        .get_legal_document(&actor, doc_id)
        .await
        .map_err(newsroom_error)?;
    Ok(Json(serde_json::json!({ "document": document, "parties": parties })))
}

/// `PATCH /api/v1/legal/documents/{id}` — edit title/body before any signature.
async fn update_legal_document(
    AuthActor(actor): AuthActor,
    Extension(newsroom): Extension<NewsroomService>,
    Path(doc_id): Path<Uuid>,
    Json(body): Json<LegalDocCreateBody>,
) -> ApiResult<impl IntoResponse> {
    let doc = newsroom
        .update_legal_document(&actor, doc_id, &body.title, &body.body_md)
        .await
        .map_err(newsroom_error)?;
    Ok(Json(doc))
}

/// `DELETE /api/v1/legal/documents/{id}` — owner deletes a non-executed document.
async fn delete_legal_document(
    AuthActor(actor): AuthActor,
    Extension(newsroom): Extension<NewsroomService>,
    Path(doc_id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    newsroom.delete_legal_document(&actor, doc_id).await.map_err(newsroom_error)?;
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /api/v1/legal/documents/{id}/parties` — add a registered-user party.
async fn add_legal_party(
    AuthActor(actor): AuthActor,
    Extension(newsroom): Extension<NewsroomService>,
    Path(doc_id): Path<Uuid>,
    Json(body): Json<LegalPartyBody>,
) -> ApiResult<impl IntoResponse> {
    let party = newsroom
        .add_legal_party(
            &actor,
            doc_id,
            &body.identifier,
            body.role.as_deref().unwrap_or(""),
            body.kind.as_deref().unwrap_or("signatory"),
            body.sign_order,
        )
        .await
        .map_err(newsroom_error)?;
    Ok((StatusCode::CREATED, Json(party)))
}

/// `POST /api/v1/legal/documents/{id}/sign` — the caller signs the document.
async fn sign_legal_document(
    AuthActor(actor): AuthActor,
    Extension(newsroom): Extension<NewsroomService>,
    Path(doc_id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let party = newsroom.sign_legal_document(&actor, doc_id).await.map_err(newsroom_error)?;
    Ok(Json(party))
}

#[derive(Deserialize)]
struct DeclineBody {
    #[serde(default)]
    reason: Option<String>,
}

/// `POST /api/v1/legal/documents/{id}/decline` — the caller declines to sign.
async fn decline_legal_document(
    AuthActor(actor): AuthActor,
    Extension(newsroom): Extension<NewsroomService>,
    Path(doc_id): Path<Uuid>,
    Json(body): Json<DeclineBody>,
) -> ApiResult<impl IntoResponse> {
    let doc = newsroom
        .decline_legal_document(&actor, doc_id, body.reason.as_deref().unwrap_or(""))
        .await
        .map_err(newsroom_error)?;
    Ok(Json(doc))
}

/// `POST /api/v1/legal/documents/{id}/void` — the owner cancels the document.
async fn void_legal_document(
    AuthActor(actor): AuthActor,
    Extension(newsroom): Extension<NewsroomService>,
    Path(doc_id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let doc = newsroom.void_legal_document(&actor, doc_id).await.map_err(newsroom_error)?;
    Ok(Json(doc))
}

/// `DELETE /api/v1/legal/documents/{id}/parties/{party_id}` — owner removes a party.
async fn remove_legal_party(
    AuthActor(actor): AuthActor,
    Extension(newsroom): Extension<NewsroomService>,
    Path((doc_id, party_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<impl IntoResponse> {
    newsroom.remove_legal_party(&actor, doc_id, party_id).await.map_err(newsroom_error)?;
    Ok(StatusCode::NO_CONTENT)
}

/// `GET /api/v1/legal/documents/{id}/activity` — the document's audit trail.
async fn legal_document_activity(
    AuthActor(actor): AuthActor,
    Extension(newsroom): Extension<NewsroomService>,
    Path(doc_id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let events = newsroom.legal_document_activity(&actor, doc_id).await.map_err(newsroom_error)?;
    Ok(Json(events))
}

/// `GET /api/v1/legal/documents/{id}/verify` — verify every party signature
/// against the current body.
async fn verify_legal_document(
    AuthActor(actor): AuthActor,
    Extension(newsroom): Extension<NewsroomService>,
    Path(doc_id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let result = newsroom
        .verify_legal_document(&actor, doc_id)
        .await
        .map_err(newsroom_error)?;
    Ok(Json(result))
}

/// `GET /api/v1/legal/documents/{id}/export` — a self-contained, watermarked
/// HTML copy of the document. Carries a diagonal SIGNED-COPY watermark, a
/// signature block, and an embedded JSON manifest (body + signatures) so the
/// downloaded file can be re-uploaded and verified. Access is limited to the
/// document's creator and its parties.
async fn export_legal_document(
    AuthActor(actor): AuthActor,
    Extension(newsroom): Extension<NewsroomService>,
    Path(doc_id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let (document, parties) = newsroom
        .get_legal_document(&actor, doc_id)
        .await
        .map_err(newsroom_error)?;
    let html = legal_export_html(&document, &parties);
    Ok(axum::response::Html(html))
}

/// Minimal HTML escaping for text interpolated into the export.
fn html_escape(input: &str) -> String {
    input
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Builds the watermarked, self-verifying HTML export for a legal document.
fn legal_export_html(
    doc: &meridian_newsroom::legal_docs::LegalDocument,
    parties: &[meridian_newsroom::legal_docs::LegalParty],
) -> String {
    let signed: Vec<serde_json::Value> = parties
        .iter()
        .filter(|p| p.signed_at.is_some() && p.signature.is_some())
        .map(|p| {
            serde_json::json!({
                "party_id": p.user_id.to_string(),
                "party_name": p.signed_name.clone().or_else(|| p.display_name.clone()).unwrap_or_default(),
                "party_role": p.party_role,
                "signed_at": p.signed_at.map(meridian_newsroom::rfc3339).unwrap_or_default(),
                "signature": p.signature.clone().unwrap_or_default(),
                "algorithm": p.algorithm.clone().unwrap_or_default(),
            })
        })
        .collect();
    // The manifest a re-uploaded copy is verified from.
    let manifest = serde_json::json!({
        "kind": "meridian-legaldoc",
        "version": 1,
        "document_id": doc.id.to_string(),
        "title": doc.title,
        "content_sha256": doc.content_sha256,
        "status": doc.status,
        "content": doc.body_md,
        "signatures": signed,
    });
    let manifest_json = serde_json::to_string(&manifest).unwrap_or_default();

    let rows: String = parties
        .iter()
        .map(|p| {
            let name = html_escape(p.display_name.as_deref().unwrap_or("—"));
            let is_cc = p.party_kind == "cc";
            let role = html_escape(if is_cc { "viewer (cc)" } else { p.party_role.as_str() });
            let (when, state) = if is_cc {
                ("shared for review".to_owned(), "pending")
            } else if let Some(at) = p.signed_at {
                (meridian_newsroom::rfc3339(at), "signed")
            } else if p.declined_at.is_some() {
                ("declined".to_owned(), "pending")
            } else {
                ("awaiting signature".to_owned(), "pending")
            };
            format!(
                "<tr><td>{name}</td><td>{role}</td><td class=\"s-{state}\">{}</td></tr>",
                html_escape(&when)
            )
        })
        .collect();

    let title = html_escape(&doc.title);
    let body = html_escape(&doc.body_md);
    let digest = html_escape(&doc.content_sha256);
    let executed = doc.status == "executed";
    let stamp = if executed { "FULLY EXECUTED" } else { "SIGNED COPY" };

    format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
<title>{title} — iGEN News legal document</title>\
<style>\
:root{{color-scheme:light}}\
body{{font-family:Georgia,'Times New Roman',serif;max-width:820px;margin:0 auto;padding:3rem 1.5rem;color:#1a1a1a;position:relative}}\
.wm{{position:fixed;inset:0;z-index:0;pointer-events:none;overflow:hidden;opacity:.06;\
background-image:repeating-linear-gradient(-30deg,transparent 0 120px,#000 120px 121px)}}\
.wm span{{position:absolute;top:40%;left:-10%;width:120%;text-align:center;\
font:700 3.2rem/1 Arial,sans-serif;letter-spacing:.3em;transform:rotate(-30deg);color:#000}}\
main{{position:relative;z-index:1}}\
.eyebrow{{font:600 .7rem/1 Arial,sans-serif;letter-spacing:.18em;text-transform:uppercase;color:#7a2e12}}\
h1{{font-size:1.9rem;margin:.2rem 0 1rem}}\
.doc{{white-space:pre-wrap;line-height:1.7;font-size:1.02rem;border-top:1px solid #ddd;padding-top:1.4rem}}\
.sigs{{margin-top:2.5rem;border-top:2px solid #1a1a1a;padding-top:1rem}}\
table{{width:100%;border-collapse:collapse;font:.9rem/1.5 Arial,sans-serif}}\
th,td{{text-align:left;padding:.5rem .4rem;border-bottom:1px solid #e2e2e2}}\
.s-signed{{color:#14713b;font-weight:600}}.s-pending{{color:#9a6a00}}\
.meta{{margin-top:1.5rem;font:.72rem/1.5 monospace;color:#555;word-break:break-all}}\
.badge{{display:inline-block;font:700 .7rem/1 Arial,sans-serif;letter-spacing:.1em;\
padding:.35rem .6rem;border-radius:5px;background:{};color:#fff}}\
</style></head><body>\
<div class=\"wm\"><span>MERIDIAN · {stamp}</span></div>\
<main>\
<p class=\"eyebrow\">iGEN News · In-platform legal document</p>\
<h1>{title}</h1>\
<p><span class=\"badge\">{stamp}</span></p>\
<div class=\"doc\">{body}</div>\
<div class=\"sigs\"><h2 style=\"font:600 1.1rem/1 Arial,sans-serif\">Signatures</h2>\
<table><thead><tr><th>Party</th><th>Role</th><th>Signed</th></tr></thead><tbody>{rows}</tbody></table>\
<p class=\"meta\">Algorithm: ML-DSA-65 (FIPS 204) · Content SHA-256: {digest}<br>\
This copy is self-verifying: re-upload it to iGEN News → Legal docs → Verify to confirm \
who created it, who signed, and that nothing was altered.</p>\
</div></main>\
<script type=\"application/json\" id=\"meridian-legaldoc\">{}</script>\
</body></html>",
        if executed { "#14713b" } else { "#7a2e12" },
        manifest_json.replace('<', "\\u003c"),
    )
}

async fn list_releases(
    Extension(identity): Extension<IdentityService>,
    Extension(newsroom): Extension<NewsroomService>,
    Path(story_id): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<impl IntoResponse> {
    let actor = actor_from_request(&identity, &headers).await?;
    newsroom.require_story_read(&actor, story_id).await.map_err(newsroom_error)?;
    let releases = newsroom.list_releases(story_id).await.map_err(newsroom_error)?;
    Ok(Json(releases))
}

async fn create_release(
    AuthActor(actor): AuthActor,
    Extension(newsroom): Extension<NewsroomService>,
    Extension(hub): Extension<RealtimeHub>,
    Path(story_id): Path<Uuid>,
    Json(body): Json<ReleaseCreateBody>,
) -> ApiResult<impl IntoResponse> {
    let draft = ReleaseDraft {
        version_id: body.version_id,
        channels: body.channels,
        scheduled_at: body.scheduled_at,
        embargo_until: body.embargo_until,
        expires_at: body.expires_at,
    };
    let release = newsroom
        .create_release(&actor, story_id, &draft)
        .await
        .map_err(newsroom_error)?;
    hub.emit("releases");
    Ok((StatusCode::CREATED, Json(release)))
}

#[derive(Deserialize)]
struct CorrectionCreateBody {
    release_id: Uuid,
    classification: String,
    description: String,
    #[serde(default)]
    owner_id: Option<Uuid>,
    #[serde(default)]
    public_note: Option<String>,
}

async fn create_correction(
    AuthActor(actor): AuthActor,
    Extension(newsroom): Extension<NewsroomService>,
    Path(story_id): Path<Uuid>,
    Json(body): Json<CorrectionCreateBody>,
) -> ApiResult<impl IntoResponse> {
    let draft = meridian_newsroom::media::CorrectionDraft {
        release_id: body.release_id,
        classification: body.classification,
        description: body.description,
        owner_id: body.owner_id,
        public_note: body.public_note,
    };
    let correction = newsroom
        .create_correction(&actor, story_id, &draft)
        .await
        .map_err(newsroom_error)?;
    Ok((StatusCode::CREATED, Json(correction)))
}

async fn front_page(
    Extension(identity): Extension<IdentityService>,
    Extension(newsroom): Extension<NewsroomService>,
    headers: HeaderMap,
) -> ApiResult<impl IntoResponse> {
    actor_from_request(&identity, &headers).await?;
    let slots = newsroom.front_page().await.map_err(newsroom_error)?;
    Ok(Json(slots))
}

#[derive(Deserialize)]
struct SlotBody {
    /// Absent or null empties the slot — a real curation act, not a no-op.
    #[serde(default)]
    story_id: Option<Uuid>,
    #[serde(default)]
    is_pinned: bool,
}

async fn set_front_page_slot(
    AuthActor(actor): AuthActor,
    Extension(newsroom): Extension<NewsroomService>,
    Path(slot_id): Path<Uuid>,
    Json(body): Json<SlotBody>,
) -> ApiResult<impl IntoResponse> {
    let slot = newsroom
        .set_front_page_slot(&actor, slot_id, body.story_id, body.is_pinned)
        .await
        .map_err(newsroom_error)?;
    Ok(Json(slot))
}

#[derive(Deserialize)]
struct ChannelQuery {
    /// Defaults to false so the admin screen sees retired channels; the publish
    /// form asks for `?active_only=true`.
    #[serde(default)]
    active_only: bool,
}

async fn list_channels(
    Extension(identity): Extension<IdentityService>,
    Extension(newsroom): Extension<NewsroomService>,
    Query(query): Query<ChannelQuery>,
    headers: HeaderMap,
) -> ApiResult<impl IntoResponse> {
    actor_from_request(&identity, &headers).await?;
    let channels = newsroom.list_channels(query.active_only).await.map_err(newsroom_error)?;
    Ok(Json(channels))
}

#[derive(Deserialize)]
struct ChannelBody {
    key: String,
    name: String,
    kind: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default = "yes")]
    is_active: bool,
    /// Optional delivery endpoint. When set, the channel is POSTed the article on
    /// publish; stored under `config.webhook_url`.
    #[serde(default)]
    webhook_url: Option<String>,
}
fn yes() -> bool {
    true
}

async fn upsert_channel(
    AuthActor(actor): AuthActor,
    Extension(newsroom): Extension<NewsroomService>,
    Json(body): Json<ChannelBody>,
) -> ApiResult<impl IntoResponse> {
    let config = match body.webhook_url.as_deref().map(str::trim) {
        Some(url) if !url.is_empty() => serde_json::json!({ "webhook_url": url }),
        _ => serde_json::json!({}),
    };
    let input = meridian_newsroom::media::ChannelInput {
        key: body.key,
        name: body.name,
        kind: body.kind,
        description: body.description,
        is_active: body.is_active,
        config,
    };
    let channel = newsroom.upsert_channel(&actor, &input).await.map_err(newsroom_error)?;
    Ok(Json(channel))
}

async fn list_corrections(
    Extension(identity): Extension<IdentityService>,
    Extension(newsroom): Extension<NewsroomService>,
    Query(query): Query<CorrectionQuery>,
    headers: HeaderMap,
) -> ApiResult<impl IntoResponse> {
    actor_from_request(&identity, &headers).await?;
    let corrections = newsroom.list_corrections(query.story_id).await.map_err(newsroom_error)?;
    Ok(Json(corrections))
}

async fn resolve_correction(
    Extension(identity): Extension<IdentityService>,
    Extension(newsroom): Extension<NewsroomService>,
    Path(correction_id): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<impl IntoResponse> {
    let actor = actor_from_request(&identity, &headers).await?;
    let correction = newsroom
        .resolve_correction(&actor, correction_id)
        .await
        .map_err(newsroom_error)?;
    Ok(Json(correction))
}

async fn process_due_releases(
    Extension(identity): Extension<IdentityService>,
    Extension(newsroom): Extension<NewsroomService>,
    Extension(hub): Extension<RealtimeHub>,
    headers: HeaderMap,
) -> ApiResult<impl IntoResponse> {
    let actor = actor_from_request(&identity, &headers).await?;
    let published = newsroom
        .process_due_releases(&actor, "endpoint")
        .await
        .map_err(newsroom_error)?;
    if !published.is_empty() {
        hub.emit("releases");
    }
    Ok(Json(published))
}

#[derive(Deserialize)]
struct RescheduleBody {
    #[serde(with = "time::serde::rfc3339")]
    scheduled_at: OffsetDateTime,
}

/// `PATCH /api/v1/releases/{id}` — move a scheduled release to a new time.
async fn reschedule_release(
    AuthActor(actor): AuthActor,
    Extension(newsroom): Extension<NewsroomService>,
    Extension(hub): Extension<RealtimeHub>,
    Path(release_id): Path<Uuid>,
    Json(body): Json<RescheduleBody>,
) -> ApiResult<impl IntoResponse> {
    let release = newsroom
        .reschedule_release(&actor, release_id, body.scheduled_at)
        .await
        .map_err(newsroom_error)?;
    hub.emit("releases");
    Ok(Json(release))
}

/// `POST /api/v1/releases/{id}/cancel` — cancel a scheduled release.
async fn cancel_release(
    AuthActor(actor): AuthActor,
    Extension(newsroom): Extension<NewsroomService>,
    Extension(hub): Extension<RealtimeHub>,
    Path(release_id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let release = newsroom.cancel_release(&actor, release_id).await.map_err(newsroom_error)?;
    hub.emit("releases");
    Ok(Json(release))
}

/// `POST /api/v1/releases/{id}/publish-now` — publish a scheduled release immediately.
async fn publish_release_now(
    AuthActor(actor): AuthActor,
    Extension(newsroom): Extension<NewsroomService>,
    Extension(hub): Extension<RealtimeHub>,
    Path(release_id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let release =
        newsroom.publish_release_now(&actor, release_id).await.map_err(newsroom_error)?;
    hub.emit("releases");
    Ok(Json(release))
}

/// `PUT /api/v1/releases/{id}/channel-schedule` — set per-channel send times.
/// Body is a JSON object of `{ channel: rfc3339 }`; empty/unknown entries drop out.
async fn set_channel_schedule(
    AuthActor(actor): AuthActor,
    Extension(newsroom): Extension<NewsroomService>,
    Extension(hub): Extension<RealtimeHub>,
    Path(release_id): Path<Uuid>,
    Json(body): Json<serde_json::Value>,
) -> ApiResult<impl IntoResponse> {
    let release = newsroom
        .set_channel_schedule(&actor, release_id, &body)
        .await
        .map_err(newsroom_error)?;
    hub.emit("releases");
    Ok(Json(release))
}

// --- Public syndication: RSS, sitemap, robots ------------------------------
//
// Unauthenticated, published-only. These give crawlers and feed readers a real
// surface even though the reader page itself is a client-rendered SPA — the feed
// and sitemap are the discoverability primitives that do not need SSR.

/// Escapes the five XML predefined entities so titles/deks can't break the doc.
fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// The public origin for absolute links. Prefers `MERIDIAN_PUBLIC_URL`; otherwise
/// reconstructs it from the forwarded proto + `Host` header.
fn public_base(headers: &HeaderMap) -> String {
    if let Ok(configured) = std::env::var("MERIDIAN_PUBLIC_URL") {
        return configured.trim_end_matches('/').to_owned();
    }
    let host = headers
        .get("host")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("localhost");
    let proto = headers
        .get("x-forwarded-proto")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("http");
    format!("{proto}://{host}")
}

async fn syndication_items(newsroom: &NewsroomService) -> Vec<SyndicationItem> {
    newsroom.published_for_syndication().await.unwrap_or_default()
}

/// A resolved correction as the public reader sees it — the note and when it was
/// made, plus its classification for the badge. Internal ids and the reporter are
/// deliberately omitted from this public surface.
#[derive(Serialize)]
struct PublicCorrection {
    public_note: String,
    classification: String,
    #[serde(with = "time::serde::rfc3339::option")]
    resolved_at: Option<OffsetDateTime>,
}

#[derive(Serialize)]
struct PublicArticleResponse {
    article: PublicArticle,
    corrections: Vec<PublicCorrection>,
}

/// `GET /api/v1/public/articles/{slug}` — one published article plus its resolved,
/// publicly-noted corrections. Unauthenticated; a non-published slug is a 404.
#[derive(Serialize)]
struct PublicArticleSummary {
    slug: String,
    title: String,
    dek: String,
    category: String,
    url: String,
    #[serde(with = "time::serde::rfc3339::option")]
    published_at: Option<OffsetDateTime>,
}

#[derive(Serialize)]
struct PublicArticleList {
    count: usize,
    limit: i64,
    offset: i64,
    articles: Vec<PublicArticleSummary>,
}

#[derive(Deserialize)]
struct ArticleListQuery {
    #[serde(default)]
    limit: Option<i64>,
    #[serde(default)]
    offset: Option<i64>,
    #[serde(default)]
    category: Option<String>,
    /// RFC 3339 instant; only articles published strictly after it are returned.
    #[serde(default)]
    since: Option<String>,
}

/// `GET /api/v1/public/articles?limit=&offset=&category=&since=` — the published
/// article list as JSON, newest first, paginated and filterable, with absolute reader
/// URLs. Unauthenticated; the headless-API counterpart to the RSS / ninjs / NewsML
/// feeds. `limit` defaults to 50, clamped 1..=200; `since` (RFC 3339) enables
/// incremental sync; an unparseable `since` is ignored.
async fn public_articles_route(
    Extension(newsroom): Extension<NewsroomService>,
    Query(query): Query<ArticleListQuery>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let base = public_base(&headers);
    let limit = query.limit.unwrap_or(50).clamp(1, 200);
    let offset = query.offset.unwrap_or(0).max(0);
    let category = query.category.as_deref().filter(|c| !c.is_empty());
    let since = query.since.as_deref().and_then(|value| {
        OffsetDateTime::parse(value, &time::format_description::well_known::Rfc3339).ok()
    });
    let articles: Vec<PublicArticleSummary> = newsroom
        .published_summaries(limit, offset, category, since)
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|item| PublicArticleSummary {
            url: format!("{base}/read/{}", item.slug),
            slug: item.slug,
            title: item.title,
            dek: item.dek,
            category: item.category,
            published_at: item.published_at,
        })
        .collect();
    Json(PublicArticleList { count: articles.len(), limit, offset, articles })
}

async fn public_article_route(
    Extension(newsroom): Extension<NewsroomService>,
    Path(slug): Path<String>,
) -> ApiResult<impl IntoResponse> {
    let article = newsroom
        .public_article(&slug)
        .await
        .map_err(newsroom_error)?
        .ok_or_else(|| newsroom_error(meridian_newsroom::NewsroomError::NotFound("Article")))?;
    let corrections = newsroom
        .list_corrections(Some(article.id))
        .await
        .map_err(newsroom_error)?
        .into_iter()
        .filter(|c| c.status == "resolved" && c.public_note.is_some())
        .map(|c| PublicCorrection {
            public_note: c.public_note.unwrap_or_default(),
            classification: c.classification,
            resolved_at: c.resolved_at,
        })
        .collect();
    Ok(Json(PublicArticleResponse { article, corrections }))
}

/// `GET /feed.xml` — an RSS 2.0 feed of published stories.
async fn rss_feed(
    Extension(newsroom): Extension<NewsroomService>,
    headers: HeaderMap,
) -> impl IntoResponse {
    use std::fmt::Write as _;
    let base = public_base(&headers);
    let mut items_xml = String::new();
    for item in &syndication_items(&newsroom).await {
        let url = format!("{base}/read/{}", item.slug);
        let _ = write!(
            items_xml,
            "<item><title>{}</title><link>{url}</link><guid isPermaLink=\"true\">{url}</guid>",
            xml_escape(&item.title)
        );
        if !item.dek.is_empty() {
            let _ = write!(items_xml, "<description>{}</description>", xml_escape(&item.dek));
        }
        if let Some(date) = item
            .published_at
            .and_then(|at| at.format(&time::format_description::well_known::Rfc2822).ok())
        {
            let _ = write!(items_xml, "<pubDate>{date}</pubDate>");
        }
        items_xml.push_str("</item>");
    }
    let body = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<rss version=\"2.0\"><channel><title>iGEN News</title><link>{base}</link><description>Published stories</description>{items_xml}</channel></rss>"
    );
    ([(axum::http::header::CONTENT_TYPE, "application/rss+xml; charset=utf-8")], body)
}

/// `GET /sitemap.xml` — a sitemap of the public reader URLs.
async fn sitemap_xml(
    Extension(newsroom): Extension<NewsroomService>,
    headers: HeaderMap,
) -> impl IntoResponse {
    use std::fmt::Write as _;
    let base = public_base(&headers);
    let mut urls = String::new();
    // The editorial-standards page is a stable public URL and the target of the
    // reader's trust-policy metadata, so it belongs in the sitemap.
    let _ = write!(urls, "<url><loc>{base}/standards</loc></url>");
    for item in &syndication_items(&newsroom).await {
        let _ = write!(urls, "<url><loc>{base}/read/{}</loc>", xml_escape(&item.slug));
        if let Some(date) = item
            .published_at
            .and_then(|at| at.format(&time::format_description::well_known::Rfc3339).ok())
        {
            let _ = write!(urls, "<lastmod>{date}</lastmod>");
        }
        urls.push_str("</url>");
    }
    let body = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<urlset xmlns=\"http://www.sitemaps.org/schemas/sitemap/0.9\">{urls}</urlset>"
    );
    ([(axum::http::header::CONTENT_TYPE, "application/xml; charset=utf-8")], body)
}

/// `GET /robots.txt` — points crawlers at the sitemap and the reader.
async fn robots_txt(headers: HeaderMap) -> impl IntoResponse {
    let base = public_base(&headers);
    let body = format!(
        "User-agent: *\nAllow: /read/\nSitemap: {base}/sitemap.xml\nSitemap: {base}/news-sitemap.xml\n"
    );
    ([(axum::http::header::CONTENT_TYPE, "text/plain; charset=utf-8")], body)
}

/// `GET /trust.txt` — the JournalList **trust.txt** for the outlet
/// (<https://journallist.net>), the machine-readable declaration news organisations
/// publish so crawlers and trust tools can find who controls the site and where its
/// disclosures live. Points `disclosure` / `contact` at the public standards page.
async fn trust_txt(headers: HeaderMap) -> impl IntoResponse {
    let base = public_base(&headers);
    let body = format!(
        "# trust.txt for iGEN News — https://journallist.net/specification-of-trust-txt\n\
         control={base}/\n\
         disclosure={base}/standards\n\
         disclosure={base}/standards#ownership\n\
         contact={base}/standards#feedback\n"
    );
    ([(axum::http::header::CONTENT_TYPE, "text/plain; charset=utf-8")], body)
}

#[derive(Deserialize)]
struct IngestQuery {
    /// Optional target desk/sector for the ingested draft.
    #[serde(default)]
    desk_id: Option<Uuid>,
}

/// `POST /api/v1/ingest/ninjs` — turn an IPTC ninjs (News-in-JSON) item into a draft
/// story. The inverse of `to_ninjs`: headline → title, description → dek, body_html /
/// body_text → block body, `subject` → category + Media Topic tags. Authenticated and
/// capability-gated (`stories.create`, scoped to the target desk); the caller becomes
/// the author. The story lands as a normal draft at the head of the lifecycle.
async fn ingest_ninjs(
    AuthActor(actor): AuthActor,
    Extension(newsroom): Extension<NewsroomService>,
    Extension(hub): Extension<RealtimeHub>,
    Query(query): Query<IngestQuery>,
    Json(ninjs): Json<Value>,
) -> ApiResult<impl IntoResponse> {
    let draft = ninjs_to_draft(&ninjs, query.desk_id).ok_or_else(|| {
        newsroom_error(meridian_newsroom::NewsroomError::Unprocessable(
            "ninjs document has no headline".to_owned(),
        ))
    })?;
    let story = newsroom.create_story(&actor, &draft).await.map_err(newsroom_error)?;
    hub.emit("stories");
    Ok((StatusCode::CREATED, Json(story)))
}

/// Builds a draft from one ninjs item — `None` if it has no headline. Shared by the
/// single-item ingest and the feed poller.
fn ninjs_to_draft(ninjs: &Value, desk_id: Option<Uuid>) -> Option<StoryDraft> {
    let title = ninjs.get("headline").and_then(Value::as_str).unwrap_or_default().trim().to_owned();
    if title.is_empty() {
        return None;
    }
    let dek = ninjs
        .get("description_text")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or_else(|| ninjs.get("description_html").and_then(Value::as_str).map(html_to_text))
        .unwrap_or_default()
        .trim()
        .to_owned();
    let (category, tags) = ninjs_category_and_tags(ninjs);
    Some(StoryDraft {
        slug: slugify(&title),
        title,
        dek,
        body: ninjs_body_blocks(ninjs),
        category,
        tags,
        story_type: "text".to_owned(),
        priority: "medium".to_owned(),
        desk_id,
        sub_sector_id: None,
        event_id: None,
        author_id: None,
        editor_id: None,
        fact_checker_id: None,
        copy_editor_id: None,
    })
}

#[derive(Deserialize)]
struct IngestPollBody {
    /// The partner ninjs feed to pull (`{items:[…]}`, a bare array, or a single item).
    feed_url: String,
    /// Optional target desk/sector for the ingested drafts.
    #[serde(default)]
    desk_id: Option<Uuid>,
}

#[derive(Serialize)]
struct IngestPollResult {
    total: usize,
    ingested: usize,
    skipped: usize,
    errors: usize,
}

/// `POST /api/v1/ingest/poll` — pull a partner ninjs feed and ingest new items as
/// draft stories. Idempotent: an item whose slug already exists is skipped, so
/// re-polling the same feed does not duplicate. This is the automated counterpart to
/// the single-item `/ingest/ninjs`; a scheduler can call it on an interval.
async fn ingest_poll(
    AuthActor(actor): AuthActor,
    Extension(newsroom): Extension<NewsroomService>,
    Extension(hub): Extension<RealtimeHub>,
    Json(body): Json<IngestPollBody>,
) -> ApiResult<impl IntoResponse> {
    if !(body.feed_url.starts_with("http://") || body.feed_url.starts_with("https://")) {
        return Err(newsroom_error(meridian_newsroom::NewsroomError::Unprocessable(
            "feed_url must be an http(s) URL".to_owned(),
        )));
    }
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|error| {
            newsroom_error(meridian_newsroom::NewsroomError::Unprocessable(error.to_string()))
        })?;
    let feed: Value = client
        .get(&body.feed_url)
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(|error| {
            newsroom_error(meridian_newsroom::NewsroomError::Unprocessable(format!(
                "could not fetch feed: {error}"
            )))
        })?
        .json()
        .await
        .map_err(|error| {
            newsroom_error(meridian_newsroom::NewsroomError::Unprocessable(format!(
                "feed is not JSON: {error}"
            )))
        })?;
    // A ninjs feed is `{items:[…]}`; also accept a bare array or a single item.
    let items: Vec<Value> = feed
        .get("items")
        .and_then(Value::as_array)
        .cloned()
        .or_else(|| feed.as_array().cloned())
        .unwrap_or_else(|| vec![feed.clone()]);
    let mut result = IngestPollResult { total: items.len(), ingested: 0, skipped: 0, errors: 0 };
    for item in &items {
        let Some(draft) = ninjs_to_draft(item, body.desk_id) else {
            result.errors += 1;
            continue;
        };
        match newsroom.slug_exists(&draft.slug).await {
            Ok(true) => result.skipped += 1,
            Ok(false) => match newsroom.create_story(&actor, &draft).await {
                Ok(_) => result.ingested += 1,
                Err(_) => result.errors += 1,
            },
            Err(_) => result.errors += 1,
        }
    }
    if result.ingested > 0 {
        hub.emit("stories");
    }
    Ok(Json(result))
}

/// A URL-safe slug from a headline: lowercase alphanumerics, single dashes between
/// words. Slug uniqueness is enforced by the database, so a duplicate headline
/// surfaces as a conflict rather than silently overwriting.
fn slugify(text: &str) -> String {
    let mut slug = String::new();
    let mut pending_dash = false;
    for ch in text.chars() {
        if ch.is_ascii_alphanumeric() {
            if pending_dash && !slug.is_empty() {
                slug.push('-');
            }
            pending_dash = false;
            slug.push(ch.to_ascii_lowercase());
        } else {
            pending_dash = true;
        }
    }
    if slug.is_empty() {
        "imported-story".to_owned()
    } else {
        slug.chars().take(120).collect()
    }
}

/// Best-effort HTML → plain text for ninjs `*_html` fields: block-closing tags become
/// line breaks, remaining tags are dropped, and the common entities are decoded.
fn html_to_text(html: &str) -> String {
    let mut s = html.to_owned();
    for boundary in ["</p>", "<br>", "<br/>", "<br />", "</h1>", "</h2>", "</h3>", "</li>", "</blockquote>"] {
        s = s.replace(boundary, "\n");
    }
    let mut out = String::new();
    let mut in_tag = false;
    for ch in s.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(ch),
            _ => {}
        }
    }
    out.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

/// Split plain text into `{type,text}` paragraph blocks, dropping blank lines.
fn text_blocks(text: &str) -> Value {
    let blocks: Vec<Value> = text
        .split('\n')
        .map(str::trim)
        .filter(|para| !para.is_empty())
        .map(|para| serde_json::json!({ "type": "paragraph", "text": para }))
        .collect();
    Value::Array(blocks)
}

/// A `{type,text}` paragraph block body from a ninjs item — `body_text` if present,
/// otherwise `body_html` flattened. Blank lines are dropped.
fn ninjs_body_blocks(ninjs: &Value) -> Value {
    let text = ninjs
        .get("body_text")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or_else(|| ninjs.get("body_html").and_then(Value::as_str).map(html_to_text))
        .unwrap_or_default();
    text_blocks(&text)
}

/// Split a ninjs `subject` array into the story's free-text category (the first entry
/// with a name but no code) and its tag list (every entry carrying a `code`, e.g.
/// Media Topic qcodes).
fn ninjs_category_and_tags(ninjs: &Value) -> (String, Value) {
    let mut category = String::new();
    let mut tags: Vec<Value> = Vec::new();
    if let Some(subjects) = ninjs.get("subject").and_then(Value::as_array) {
        for subject in subjects {
            match subject.get("code").and_then(Value::as_str) {
                Some(code) => tags.push(Value::String(code.to_owned())),
                None => {
                    if category.is_empty() {
                        if let Some(name) = subject.get("name").and_then(Value::as_str) {
                            category = name.to_owned();
                        }
                    }
                }
            }
        }
    }
    if category.is_empty() {
        category = "General".to_owned();
    }
    (category, Value::Array(tags))
}

/// `POST /api/v1/ingest/newsml` — turn a NewsML-G2 `newsItem` into a draft story,
/// the inverse of `to_newsml`. Best-effort targeted extraction of the fields we
/// emit (headline, description, the xhtml body, Media Topic subjects) rather than a
/// full XML parse — enough to round-trip our own export and typical G2 items.
async fn ingest_newsml(
    AuthActor(actor): AuthActor,
    Extension(newsroom): Extension<NewsroomService>,
    Extension(hub): Extension<RealtimeHub>,
    Query(query): Query<IngestQuery>,
    xml: String,
) -> ApiResult<impl IntoResponse> {
    let title = newsml_extract(&xml, "<headline>", "</headline>")
        .map(|value| xml_unescape(&value))
        .unwrap_or_default()
        .trim()
        .to_owned();
    if title.is_empty() {
        return Err(newsroom_error(meridian_newsroom::NewsroomError::Unprocessable(
            "NewsML-G2 document has no <headline>".to_owned(),
        )));
    }
    let dek = newsml_extract(&xml, "<description>", "</description>")
        .map(|value| xml_unescape(&value))
        .unwrap_or_default()
        .trim()
        .to_owned();
    let (category, tags) = newsml_category_and_tags(&xml);
    let body_html = newsml_extract(&xml, "<body>", "</body>").unwrap_or_default();
    let draft = StoryDraft {
        slug: slugify(&title),
        title,
        dek,
        body: text_blocks(&html_to_text(&body_html)),
        category,
        tags,
        story_type: "text".to_owned(),
        priority: "medium".to_owned(),
        desk_id: query.desk_id,
        sub_sector_id: None,
        event_id: None,
        author_id: None,
        editor_id: None,
        fact_checker_id: None,
        copy_editor_id: None,
    };
    let story = newsroom.create_story(&actor, &draft).await.map_err(newsroom_error)?;
    hub.emit("stories");
    Ok((StatusCode::CREATED, Json(story)))
}

/// Builds a draft from an IPTC **NITF** document — `None` if it has no headline.
/// Best-effort extraction: `<hl1>` (or `<title>`) → headline, `<abstract>` → dek,
/// `<body.content>` → block body.
fn nitf_to_draft(xml: &str, desk_id: Option<Uuid>) -> Option<StoryDraft> {
    let title = newsml_extract(xml, "<hl1>", "</hl1>")
        .or_else(|| newsml_extract(xml, "<title>", "</title>"))
        .map(|value| xml_unescape(&html_to_text(&value)))
        .unwrap_or_default()
        .trim()
        .to_owned();
    if title.is_empty() {
        return None;
    }
    let dek = newsml_extract(xml, "<abstract>", "</abstract>")
        .map(|value| html_to_text(&value))
        .unwrap_or_default()
        .trim()
        .to_owned();
    let body_src = newsml_extract(xml, "<body.content>", "</body.content>")
        .or_else(|| newsml_extract(xml, "<body>", "</body>"))
        .unwrap_or_default();
    Some(StoryDraft {
        slug: slugify(&title),
        title,
        dek,
        body: text_blocks(&html_to_text(&body_src)),
        category: "General".to_owned(),
        tags: serde_json::json!([]),
        story_type: "text".to_owned(),
        priority: "medium".to_owned(),
        desk_id,
        sub_sector_id: None,
        event_id: None,
        author_id: None,
        editor_id: None,
        fact_checker_id: None,
        copy_editor_id: None,
    })
}

/// `POST /api/v1/ingest/nitf` — turn an IPTC **NITF** document into a draft story,
/// a third importable interchange format alongside ninjs and NewsML-G2.
async fn ingest_nitf(
    AuthActor(actor): AuthActor,
    Extension(newsroom): Extension<NewsroomService>,
    Extension(hub): Extension<RealtimeHub>,
    Query(query): Query<IngestQuery>,
    xml: String,
) -> ApiResult<impl IntoResponse> {
    let draft = nitf_to_draft(&xml, query.desk_id).ok_or_else(|| {
        newsroom_error(meridian_newsroom::NewsroomError::Unprocessable(
            "NITF document has no headline".to_owned(),
        ))
    })?;
    let story = newsroom.create_story(&actor, &draft).await.map_err(newsroom_error)?;
    hub.emit("stories");
    Ok((StatusCode::CREATED, Json(story)))
}

/// Inverse of `xml_escape`. `&amp;` is decoded last so an escaped entity in the
/// source (e.g. `&amp;lt;`) survives as literal text rather than being re-decoded.
fn xml_unescape(value: &str) -> String {
    value
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

/// The inner text between the first `open` tag and the next following `close` tag.
fn newsml_extract(xml: &str, open: &str, close: &str) -> Option<String> {
    let start = xml.find(open)? + open.len();
    let end = xml[start..].find(close)? + start;
    Some(xml[start..end].to_owned())
}

/// The story's free category (the attribute-less `<subject><name>…</name>`) and its
/// Media Topic tags (every `qcode="medtop:XXXXXXXX"` in the document).
fn newsml_category_and_tags(xml: &str) -> (String, Value) {
    let category = newsml_extract(xml, "<subject><name>", "</name>")
        .map(|value| xml_unescape(&value))
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "General".to_owned());
    let mut tags: Vec<Value> = Vec::new();
    let marker = "qcode=\"medtop:";
    let mut rest = xml;
    while let Some(pos) = rest.find(marker) {
        let after = &rest[pos + marker.len()..];
        let code: String = after.chars().take_while(char::is_ascii_alphanumeric).collect();
        if !code.is_empty() {
            tags.push(Value::String(code));
        }
        rest = after;
    }
    (category, Value::Array(tags))
}

/// Renders a `{type,text}` block body to minimal HTML for the ninjs `body_html`.
fn blocks_to_html(body: &serde_json::Value) -> String {
    use std::fmt::Write as _;
    let Some(blocks) = body.as_array() else {
        return String::new();
    };
    let mut html = String::new();
    for block in blocks {
        let text = block
            .get("text")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");
        if text.trim().is_empty() {
            continue;
        }
        // A rich body persists as one `html` block holding the editor's full HTML
        // (images, inline marks, lists, code) — emit it verbatim for the reader.
        if block.get("type").and_then(serde_json::Value::as_str) == Some("html") {
            html.push_str(text);
            continue;
        }
        let tag = match block.get("type").and_then(serde_json::Value::as_str) {
            Some("heading") => "h2",
            Some("subheading") => "h3",
            Some("quote") => "blockquote",
            _ => "p",
        };
        let _ = write!(html, "<{tag}>{}</{tag}>", xml_escape(text));
    }
    html
}

/// Maps a published story to an IPTC **ninjs** (News-in-JSON) item — the modern JSON
/// news-interchange format (the schema Superdesk and IPTC-aligned agencies consume).
fn to_ninjs(base: &str, item: &NinjsStory) -> serde_json::Value {
    let fmt = |dt: OffsetDateTime| {
        dt.format(&time::format_description::well_known::Rfc3339).ok()
    };
    let version_created = item.published_at.and_then(fmt).or_else(|| fmt(item.updated_at));
    let rights = item_rights(item);
    let mut rightsinfo = serde_json::json!({
        "copyrightholder": rights.holder,
        "copyrightnotice": rights.notice,
        "usageterms": rights.usage,
    });
    // Machine-readable licence link (a CC deed, an ODRL/RightsML policy), when the
    // story sets one. Emitted as a ninjs `link` with rel="license" so a consumer
    // can resolve the actual terms, not just read the prose.
    if let Some(url) = rights.license {
        rightsinfo["link"] = serde_json::json!([{ "rel": "license", "href": url }]);
    }
    let uri = format!("{base}/read/{}", item.slug);
    let mut ninjs = serde_json::json!({
        "uri": uri,
        "type": "text",
        "profile": "text",
        "pubstatus": "usable",
        "language": "en",
        "versioncreated": version_created,
        "firstcreated": fmt(item.created_at),
        "headline": item.title,
        "description_text": item.dek,
        "body_html": blocks_to_html(&item.body),
        "byline": item.author_name.clone().unwrap_or_else(|| "iGEN News".to_owned()),
        "copyrightholder": rights.holder,
        "copyrightnotice": rights.notice,
        "usageterms": rights.usage,
        // Structured ninjs rightsinfo — the machine-readable rights statement, richer
        // than the single usageterms string.
        "rightsinfo": [rightsinfo],
        "subject": ninjs_subjects(item),
    });
    // Machine-readable reuse constraints as an ODRL policy (RightsML is an ODRL
    // profile). Only present when the story sets an embargo and/or a territory
    // restriction, so an unconstrained item stays byte-for-byte what it was.
    if let Some(policy) = odrl_policy(item, &uri) {
        ninjs["odrl:hasPolicy"] = policy;
    }
    // AI-disclosure: IPTC Digital Source Type as a scheme URI (EU AI Act Art. 50).
    if let Some(code) = item
        .digital_source_type
        .as_deref()
        .map(str::trim)
        .filter(|c| !c.is_empty())
    {
        ninjs["digitalsourcetype"] = serde_json::json!(format!(
            "{}{code}",
            meridian_contracts::DIGITAL_SOURCE_TYPE_SCHEME
        ));
    }
    ninjs
}

/// The rights statement to emit for one story: each per-story override or, where
/// the story sets none, the outlet default. This is the single place the
/// outlet-vs-story fallback lives, so ninjs and NewsML-G2 agree.
struct ResolvedRights<'a> {
    holder: &'a str,
    notice: &'a str,
    usage: &'a str,
    license: Option<&'a str>,
}

fn item_rights(item: &NinjsStory) -> ResolvedRights<'_> {
    ResolvedRights {
        holder: rights_override(&item.rights_holder).unwrap_or("iGEN News"),
        notice: rights_override(&item.rights_notice).unwrap_or(COPYRIGHT_NOTICE),
        usage: rights_override(&item.usage_terms).unwrap_or(USAGE_TERMS),
        license: rights_override(&item.license_url),
    }
}

/// A per-story rights override, trimmed, or `None` when the story leaves the
/// field blank (so the caller falls back to the outlet default).
fn rights_override(value: &Option<String>) -> Option<&str> {
    value
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
}

/// The story's territory list, upper-cased ISO codes with blanks dropped. Empty
/// when the story sets no territories (or `territory_mode` is `any`).
fn territory_list(item: &NinjsStory) -> Vec<String> {
    if item.territory_mode == "any" {
        return Vec::new();
    }
    item.territories
        .as_deref()
        .unwrap_or("")
        .split(',')
        .map(str::trim)
        .filter(|code| !code.is_empty())
        .map(str::to_ascii_uppercase)
        .collect()
}

/// The ODRL spatial operator for the story's `territory_mode`, or `None` when
/// there is no effective territory restriction.
fn territory_operator(item: &NinjsStory, territories: &[String]) -> Option<&'static str> {
    if territories.is_empty() {
        return None;
    }
    match item.territory_mode.as_str() {
        // "allow" → reuse only in these areas; "deny" → everywhere but these.
        "allow" => Some("isAnyOf"),
        "deny" => Some("isNoneOf"),
        _ => None,
    }
}

/// Builds an ODRL policy for the story's reuse constraints, or `None` when it has
/// neither a reuse embargo nor a territory restriction. The policy is a `Set` with
/// one `reproduce` permission carrying a temporal (`dateTime`) and/or spatial
/// (`spatial`) constraint — the RightsML/ODRL shape a rights-aware consumer reads.
fn odrl_policy(item: &NinjsStory, target_url: &str) -> Option<serde_json::Value> {
    let embargo = item
        .reuse_embargo_until
        .and_then(|dt| dt.format(&time::format_description::well_known::Rfc3339).ok());
    let territories = territory_list(item);
    let territory_op = territory_operator(item, &territories);
    if embargo.is_none() && territory_op.is_none() {
        return None;
    }
    let mut constraints = Vec::new();
    if let Some(ref instant) = embargo {
        constraints.push(serde_json::json!({
            "leftOperand": "dateTime",
            "operator": "gteq",
            "rightOperand": instant,
        }));
    }
    if let Some(op) = territory_op {
        constraints.push(serde_json::json!({
            "leftOperand": "spatial",
            "operator": op,
            "rightOperand": territories,
        }));
    }
    Some(serde_json::json!({
        "@context": "http://www.w3.org/ns/odrl.jsonld",
        "@type": "Set",
        "uid": format!("{target_url}#reuse-policy"),
        "permission": [{
            "target": target_url,
            "action": "reproduce",
            "constraint": constraints,
        }],
    }))
}

/// A human-readable one-line summary of the story's reuse constraints, or `None`
/// when it has none. Used where a structured policy does not fit — the NewsML-G2
/// `<usageTerms>` prose a non-ODRL consumer still reads.
fn reuse_constraint_prose(item: &NinjsStory) -> Option<String> {
    let mut parts = Vec::new();
    if let Some(instant) = item
        .reuse_embargo_until
        .and_then(|dt| dt.format(&time::format_description::well_known::Rfc3339).ok())
    {
        parts.push(format!("Reuse embargoed until {instant}."));
    }
    let territories = territory_list(item);
    match territory_operator(item, &territories) {
        Some("isAnyOf") => parts.push(format!("Reuse permitted only in: {}.", territories.join(", "))),
        Some("isNoneOf") => parts.push(format!("Reuse not permitted in: {}.", territories.join(", "))),
        _ => {}
    }
    (!parts.is_empty()).then(|| parts.join(" "))
}

/// The outlet-level rights statement carried on every syndicated item. A structured
/// per-item RightsML/ODRL model would supersede this; a blanket usage-terms string is
/// the conventional agency baseline.
const USAGE_TERMS: &str =
    "© iGEN News. For editorial reuse by licensed partners; other use requires permission.";

/// The copyright notice carried alongside the usage terms in every rights statement.
const COPYRIGHT_NOTICE: &str = "© iGEN News. All rights reserved.";

/// The `subject` array for a ninjs item: the free category plus any IPTC Media Topic
/// qcodes stored on the story's tags, expanded to `{code, name, scheme}`.
fn ninjs_subjects(item: &NinjsStory) -> Vec<serde_json::Value> {
    let mut subjects = vec![serde_json::json!({ "name": item.category })];
    if let Some(tags) = item.tags.as_array() {
        for qcode in tags.iter().filter_map(serde_json::Value::as_str) {
            if let Some(label) = meridian_contracts::media_topic_label(qcode) {
                subjects.push(serde_json::json!({
                    "code": qcode,
                    "name": label,
                    "scheme": "http://cv.iptc.org/newscodes/mediatopic/",
                    "rel": "subject",
                }));
            }
        }
    }
    subjects
}

/// `GET /feed.ninjs` — a ninjs package of published stories for agency/interchange
/// consumers. Public, published-only, no auth.
async fn ninjs_feed(
    Extension(newsroom): Extension<NewsroomService>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let base = public_base(&headers);
    let items: Vec<serde_json::Value> = newsroom
        .published_ninjs()
        .await
        .unwrap_or_default()
        .iter()
        .map(|story| to_ninjs(&base, story))
        .collect();
    Json(serde_json::json!({ "version": "3.0", "items": items }))
}

/// Maps a published story to an IPTC **NewsML-G2** `newsItem` (power conformance).
/// The richer XML interchange standard, alongside the JSON `ninjs` variant.
fn to_newsml(base: &str, item: &NinjsStory) -> String {
    let rfc3339 = |dt: OffsetDateTime| {
        dt.format(&time::format_description::well_known::Rfc3339)
            .unwrap_or_default()
    };
    let version_created = item
        .published_at
        .map_or_else(|| rfc3339(item.updated_at), rfc3339);
    let first_created = rfc3339(item.created_at);
    let guid = format!("{base}/read/{}", item.slug);
    let rights = item_rights(item);
    // NewsML-G2 rightsInfo permits a <link> child; when the story carries a
    // machine-readable licence we emit it with the xhtml `license` relationship
    // so a consumer can resolve the actual policy, not just the prose usageTerms.
    let license_link = rights.license.map_or_else(String::new, |url| {
        format!(
            "<link rel=\"http://www.w3.org/1999/xhtml/vocab#license\" href=\"{}\"/>",
            xml_escape(url)
        )
    });
    // Reuse constraints (embargo / territory) as an extra <usageTerms> — the ODRL
    // policy travels in the ninjs variant; here a non-ODRL XML consumer still reads
    // the restriction as prose. Absent when the story sets no constraints.
    let reuse_terms = reuse_constraint_prose(item).map_or_else(String::new, |prose| {
        format!("<usageTerms>{}</usageTerms>", xml_escape(&prose))
    });
    let author = xml_escape(&item.author_name.clone().unwrap_or_else(|| "iGEN News".to_owned()));
    // Free category subject + one qcoded Media Topic subject per stored qcode.
    let mut subjects = format!("<subject><name>{}</name></subject>", xml_escape(&item.category));
    if let Some(tags) = item.tags.as_array() {
        use std::fmt::Write as _;
        for qcode in tags.iter().filter_map(serde_json::Value::as_str) {
            if let Some(label) = meridian_contracts::media_topic_label(qcode) {
                let _ = write!(
                    subjects,
                    "<subject type=\"cpnat:abstract\" qcode=\"medtop:{qcode}\"><name>{}</name></subject>",
                    xml_escape(label)
                );
            }
        }
    }
    // AI-disclosure: IPTC Digital Source Type (EU AI Act Art. 50). `digsrctype` is
    // the scheme alias in the referenced IPTC G2 catalog.
    let digsrctype = item
        .digital_source_type
        .as_deref()
        .map(str::trim)
        .filter(|c| !c.is_empty())
        .map_or_else(String::new, |code| {
            format!("<digitalSourceType qcode=\"digsrctype:{}\"/>", xml_escape(code))
        });
    format!(
        "<newsItem xmlns=\"http://iptc.org/std/nar/2006-10-01/\" guid=\"{guid}\" version=\"1\" \
standard=\"NewsML-G2\" standardversion=\"2.29\" conformance=\"power\" xml:lang=\"en\">\
<catalogRef href=\"http://www.iptc.org/std/catalog/catalog.IPTC-G2-Standards_38.xml\"/>\
<itemMeta><itemClass qcode=\"ninat:text\"/><provider literal=\"iGEN News\"/>\
<versionCreated>{version_created}</versionCreated><firstCreated>{first_created}</firstCreated>\
<pubStatus qcode=\"stat:usable\"/></itemMeta>\
<rightsInfo><copyrightHolder literal=\"{holder}\"><name>{holder}</name></copyrightHolder>\
<copyrightNotice>{copyright}</copyrightNotice><usageTerms>{usage}</usageTerms>{reuse_terms}{license_link}</rightsInfo>\
<contentMeta><contentCreated>{first_created}</contentCreated><by>{author}</by>\
<headline>{headline}</headline><description>{description}</description>{subjects}{digsrctype}</contentMeta>\
<contentSet><inlineXML contenttype=\"application/xhtml+xml\">\
<html xmlns=\"http://www.w3.org/1999/xhtml\"><body>{body}</body></html>\
</inlineXML></contentSet></newsItem>",
        headline = xml_escape(&item.title),
        description = xml_escape(&item.dek),
        holder = xml_escape(rights.holder),
        usage = xml_escape(rights.usage),
        copyright = xml_escape(rights.notice),
        body = blocks_to_html(&item.body),
    )
}

/// `GET /feed.newsml` — a NewsML-G2 `newsMessage` of published stories.
async fn newsml_feed(
    Extension(newsroom): Extension<NewsroomService>,
    headers: HeaderMap,
) -> impl IntoResponse {
    use std::fmt::Write as _;
    let base = public_base(&headers);
    let mut items = String::new();
    for story in &newsroom.published_ninjs().await.unwrap_or_default() {
        let _ = write!(items, "{}", to_newsml(&base, story));
    }
    let body = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
<newsMessage xmlns=\"http://iptc.org/std/nar/2006-10-01/\"><itemSet>{items}</itemSet></newsMessage>"
    );
    ([(axum::http::header::CONTENT_TYPE, "application/vnd.iptc.g2.newsitem+xml; charset=utf-8")], body)
}

/// `GET /news-sitemap.xml` — Google News sitemap: published stories from the last
/// 48 hours (Google News ignores older URLs), carrying `news:publication_date`.
async fn news_sitemap_xml(
    Extension(newsroom): Extension<NewsroomService>,
    headers: HeaderMap,
) -> impl IntoResponse {
    use std::fmt::Write as _;
    let base = public_base(&headers);
    let cutoff = OffsetDateTime::now_utc() - time::Duration::hours(48);
    let mut urls = String::new();
    for item in &syndication_items(&newsroom).await {
        let Some(published) = item.published_at else {
            continue;
        };
        if published < cutoff {
            continue;
        }
        let Ok(date) = published.format(&time::format_description::well_known::Rfc3339) else {
            continue;
        };
        let _ = write!(
            urls,
            "<url><loc>{base}/read/{slug}</loc><news:news><news:publication>\
<news:name>iGEN News</news:name><news:language>en</news:language></news:publication>\
<news:publication_date>{date}</news:publication_date><news:title>{title}</news:title>\
</news:news></url>",
            slug = xml_escape(&item.slug),
            title = xml_escape(&item.title),
        );
    }
    let body = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
<urlset xmlns=\"http://www.sitemaps.org/schemas/sitemap/0.9\" \
xmlns:news=\"http://www.google.com/schemas/sitemap-news/0.9\">{urls}</urlset>"
    );
    ([(axum::http::header::CONTENT_TYPE, "application/xml; charset=utf-8")], body)
}

#[cfg(test)]
mod syndication_tests {
    use super::{USAGE_TERMS, blocks_to_html, ninjs_subjects, to_newsml, to_ninjs, xml_escape};
    use meridian_newsroom::media::NinjsStory;
    use time::OffsetDateTime;

    fn sample(tags: serde_json::Value, body: serde_json::Value) -> NinjsStory {
        NinjsStory {
            slug: "test-slug".to_owned(),
            title: "A & B <headline>".to_owned(),
            dek: "The dek".to_owned(),
            body,
            category: "General".to_owned(),
            tags,
            author_name: Some("Jane Doe".to_owned()),
            rights_holder: None,
            rights_notice: None,
            usage_terms: None,
            license_url: None,
            reuse_embargo_until: None,
            territory_mode: "any".to_owned(),
            territories: None,
            digital_source_type: None,
            created_at: OffsetDateTime::UNIX_EPOCH,
            updated_at: OffsetDateTime::UNIX_EPOCH,
            published_at: Some(OffsetDateTime::UNIX_EPOCH),
        }
    }

    #[test]
    fn to_ninjs_emits_digital_source_type_as_iptc_uri() {
        let item = NinjsStory {
            digital_source_type: Some("trainedAlgorithmicMedia".to_owned()),
            ..sample(serde_json::json!([]), serde_json::json!([]))
        };
        let value = to_ninjs("https://example.com", &item);
        assert_eq!(
            value["digitalsourcetype"],
            "http://cv.iptc.org/newscodes/digitalsourcetype/trainedAlgorithmicMedia"
        );
        // Absent when the story leaves it unspecified.
        let bare = to_ninjs("https://example.com", &sample(serde_json::json!([]), serde_json::json!([])));
        assert!(bare.get("digitalsourcetype").is_none());
    }

    #[test]
    fn xml_escape_covers_all_five_entities() {
        assert_eq!(
            xml_escape("a & b < c > d \" e ' f"),
            "a &amp; b &lt; c &gt; d &quot; e &apos; f"
        );
    }

    #[test]
    fn blocks_to_html_maps_types_escapes_and_skips_blank() {
        let body = serde_json::json!([
            {"type": "heading", "text": "Head & line"},
            {"type": "paragraph", "text": "Body"},
            {"type": "quote", "text": "Quote"},
            {"type": "paragraph", "text": "   "},
        ]);
        assert_eq!(
            blocks_to_html(&body),
            "<h2>Head &amp; line</h2><p>Body</p><blockquote>Quote</blockquote>"
        );
    }

    #[test]
    fn ninjs_subjects_expand_valid_media_topics_only() {
        let item = sample(serde_json::json!(["11000000", "99999999"]), serde_json::json!([]));
        let subjects = ninjs_subjects(&item);
        assert_eq!(subjects.len(), 2, "category + one valid topic; unknown qcode dropped");
        assert_eq!(subjects[0]["name"], "General");
        assert_eq!(subjects[1]["code"], "11000000");
        assert_eq!(subjects[1]["name"], "politics");
        assert_eq!(subjects[1]["scheme"], "http://cv.iptc.org/newscodes/mediatopic/");
    }

    #[test]
    fn to_ninjs_carries_required_iptc_fields() {
        let item = sample(serde_json::json!([]), serde_json::json!([{"type": "paragraph", "text": "Hi"}]));
        let value = to_ninjs("https://example.com", &item);
        assert_eq!(value["type"], "text");
        assert_eq!(value["pubstatus"], "usable");
        assert_eq!(value["uri"], "https://example.com/read/test-slug");
        assert_eq!(value["byline"], "Jane Doe");
        assert_eq!(value["body_html"], "<p>Hi</p>");
        assert_eq!(value["usageterms"], USAGE_TERMS);
    }

    #[test]
    fn to_newsml_qcodes_topics_and_escapes_headline() {
        let item = sample(serde_json::json!(["15000000"]), serde_json::json!([{"type": "paragraph", "text": "Hi"}]));
        let xml = to_newsml("https://example.com", &item);
        assert!(xml.contains("standard=\"NewsML-G2\""));
        assert!(xml.contains("qcode=\"medtop:15000000\""));
        assert!(xml.contains("<rightsInfo>") && xml.contains("<usageTerms>"));
        assert!(xml.contains("A &amp; B &lt;headline&gt;"), "headline must be XML-escaped");
    }

    #[test]
    fn slugify_is_url_safe() {
        assert_eq!(super::slugify("Hello, World!"), "hello-world");
        assert_eq!(super::slugify("  Multiple   spaces "), "multiple-spaces");
        assert_eq!(super::slugify("!!!"), "imported-story");
    }

    #[test]
    fn html_to_text_flattens_blocks_and_decodes_entities() {
        let text = super::html_to_text("<p>First &amp; only.</p><p>Second.</p>");
        assert!(text.contains("First & only."));
        assert!(text.contains("Second."));
        assert!(!text.contains('<'));
    }

    #[test]
    fn ninjs_body_blocks_paragraphs_from_body_text() {
        let doc = serde_json::json!({ "body_text": "One.\n\nTwo." });
        let blocks = super::ninjs_body_blocks(&doc);
        let arr = blocks.as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["type"], "paragraph");
        assert_eq!(arr[0]["text"], "One.");
        assert_eq!(arr[1]["text"], "Two.");
    }

    #[test]
    fn ninjs_subject_splits_into_category_and_tags() {
        let doc = serde_json::json!({ "subject": [
            {"name": "Transport"},
            {"code": "11000000", "name": "politics"},
        ]});
        let (category, tags) = super::ninjs_category_and_tags(&doc);
        assert_eq!(category, "Transport");
        assert_eq!(tags, serde_json::json!(["11000000"]));
    }

    #[test]
    fn ninjs_export_then_import_preserves_topic_category_and_body() {
        // Export a story to ninjs, then parse it back with the import helpers — the
        // Media Topic code, the free category and the body text must all survive.
        let item = sample(
            serde_json::json!(["15000000"]),
            serde_json::json!([{"type": "paragraph", "text": "Body."}]),
        );
        let ninjs = to_ninjs("https://example.com", &item);
        let (category, tags) = super::ninjs_category_and_tags(&ninjs);
        assert_eq!(category, "General");
        assert_eq!(tags, serde_json::json!(["15000000"]));
        let blocks = super::ninjs_body_blocks(&ninjs);
        assert_eq!(blocks.as_array().unwrap()[0]["text"], "Body.");
    }

    #[test]
    fn xml_unescape_reverses_xml_escape() {
        let raw = "A & B <headline> \"q\" 'a'";
        assert_eq!(super::xml_unescape(&xml_escape(raw)), raw);
    }

    #[test]
    fn newsml_export_then_import_preserves_headline_category_topics_body() {
        // Export to NewsML-G2, then extract fields back — headline (XML-escaped in
        // the source), free category, Media Topic qcode and body must all survive.
        let item = sample(
            serde_json::json!(["15000000"]),
            serde_json::json!([{"type": "paragraph", "text": "Body text."}]),
        );
        let xml = to_newsml("https://example.com", &item);
        let title = super::newsml_extract(&xml, "<headline>", "</headline>")
            .map(|s| super::xml_unescape(&s))
            .unwrap();
        assert_eq!(title, "A & B <headline>");
        let (category, tags) = super::newsml_category_and_tags(&xml);
        assert_eq!(category, "General");
        assert_eq!(tags, serde_json::json!(["15000000"]));
        let body_html = super::newsml_extract(&xml, "<body>", "</body>").unwrap();
        let blocks = super::text_blocks(&super::html_to_text(&body_html));
        assert_eq!(blocks.as_array().unwrap()[0]["text"], "Body text.");
    }

    #[test]
    fn to_ninjs_carries_structured_rightsinfo() {
        let value = to_ninjs("https://example.com", &sample(serde_json::json!([]), serde_json::json!([])));
        assert_eq!(value["copyrightholder"], "iGEN News");
        assert!(value["copyrightnotice"].as_str().unwrap().contains("rights reserved"));
        let rights = value["rightsinfo"].as_array().unwrap();
        assert_eq!(rights.len(), 1);
        assert_eq!(rights[0]["copyrightholder"], "iGEN News");
        assert_eq!(rights[0]["usageterms"], USAGE_TERMS);
        assert!(rights[0]["copyrightnotice"].as_str().unwrap().contains("rights reserved"));
    }

    #[test]
    fn to_newsml_carries_copyright_holder_and_notice() {
        let xml = to_newsml(
            "https://example.com",
            &sample(serde_json::json!([]), serde_json::json!([{"type": "paragraph", "text": "Hi"}])),
        );
        assert!(xml.contains("<copyrightHolder"));
        assert!(xml.contains("<copyrightNotice>"));
        assert!(xml.contains("<usageTerms>"));
    }

    /// A `NinjsStory` carrying every per-story rights override, for the
    /// override-vs-default tests below.
    fn sample_with_rights() -> NinjsStory {
        NinjsStory {
            rights_holder: Some("Guest Author".to_owned()),
            rights_notice: Some("© 2026 Guest Author.".to_owned()),
            usage_terms: Some("CC BY 4.0 — attribution required.".to_owned()),
            license_url: Some("https://creativecommons.org/licenses/by/4.0/".to_owned()),
            ..sample(serde_json::json!([]), serde_json::json!([{"type": "paragraph", "text": "Hi"}]))
        }
    }

    #[test]
    fn to_ninjs_uses_per_story_rights_when_set() {
        let value = to_ninjs("https://example.com", &sample_with_rights());
        assert_eq!(value["copyrightholder"], "Guest Author");
        assert_eq!(value["copyrightnotice"], "© 2026 Guest Author.");
        assert_eq!(value["usageterms"], "CC BY 4.0 — attribution required.");
        let rights = value["rightsinfo"].as_array().unwrap();
        assert_eq!(rights[0]["copyrightholder"], "Guest Author");
        // The machine-readable licence link is emitted with rel="license".
        assert_eq!(rights[0]["link"][0]["rel"], "license");
        assert_eq!(
            rights[0]["link"][0]["href"],
            "https://creativecommons.org/licenses/by/4.0/"
        );
    }

    #[test]
    fn to_ninjs_falls_back_to_outlet_default_when_rights_blank() {
        // A whitespace-only override is treated as unset, so the outlet default wins.
        let mut item = sample(serde_json::json!([]), serde_json::json!([]));
        item.rights_holder = Some("   ".to_owned());
        let value = to_ninjs("https://example.com", &item);
        assert_eq!(value["copyrightholder"], "iGEN News");
        assert_eq!(value["usageterms"], USAGE_TERMS);
        // No licence set -> no link on the rightsinfo entry.
        assert!(value["rightsinfo"][0].get("link").is_none());
    }

    #[test]
    fn to_newsml_uses_per_story_rights_and_license_link() {
        let xml = to_newsml("https://example.com", &sample_with_rights());
        assert!(xml.contains("<copyrightHolder literal=\"Guest Author\">"));
        assert!(xml.contains("<copyrightNotice>© 2026 Guest Author.</copyrightNotice>"));
        assert!(xml.contains("CC BY 4.0"));
        assert!(xml.contains(
            "<link rel=\"http://www.w3.org/1999/xhtml/vocab#license\" \
             href=\"https://creativecommons.org/licenses/by/4.0/\"/>"
        ));
    }

    #[test]
    fn to_newsml_omits_license_link_without_a_license() {
        let xml = to_newsml(
            "https://example.com",
            &sample(serde_json::json!([]), serde_json::json!([])),
        );
        assert!(!xml.contains("vocab#license"));
    }

    // --- Reuse constraints (ODRL) ---

    /// 2001-09-09T01:46:40Z — a fixed instant for embargo assertions.
    fn embargo_instant() -> OffsetDateTime {
        OffsetDateTime::from_unix_timestamp(1_000_000_000).unwrap()
    }

    #[test]
    fn odrl_policy_is_none_without_constraints() {
        let item = sample(serde_json::json!([]), serde_json::json!([]));
        assert!(super::odrl_policy(&item, "https://example.com/read/x").is_none());
        // …and the ninjs item then carries no policy key at all.
        let value = to_ninjs("https://example.com", &item);
        assert!(value.get("odrl:hasPolicy").is_none());
    }

    #[test]
    fn odrl_policy_carries_embargo_and_territory_constraints() {
        let mut item = sample(serde_json::json!([]), serde_json::json!([]));
        item.reuse_embargo_until = Some(embargo_instant());
        item.territory_mode = "deny".to_owned();
        item.territories = Some("us, ca".to_owned());
        let policy = super::odrl_policy(&item, "https://example.com/read/x").unwrap();
        assert_eq!(policy["@type"], "Set");
        let permission = &policy["permission"][0];
        assert_eq!(permission["action"], "reproduce");
        let constraints = permission["constraint"].as_array().unwrap();
        // Temporal constraint: reproduce only at/after the embargo instant.
        let temporal = constraints.iter().find(|c| c["leftOperand"] == "dateTime").unwrap();
        assert_eq!(temporal["operator"], "gteq");
        assert_eq!(temporal["rightOperand"], "2001-09-09T01:46:40Z");
        // Spatial constraint: deny → isNoneOf, codes upper-cased and split.
        let spatial = constraints.iter().find(|c| c["leftOperand"] == "spatial").unwrap();
        assert_eq!(spatial["operator"], "isNoneOf");
        assert_eq!(spatial["rightOperand"], serde_json::json!(["US", "CA"]));
    }

    #[test]
    fn allow_mode_maps_to_is_any_of() {
        let mut item = sample(serde_json::json!([]), serde_json::json!([]));
        item.territory_mode = "allow".to_owned();
        item.territories = Some("GB".to_owned());
        let policy = super::odrl_policy(&item, "https://example.com/read/x").unwrap();
        let spatial = &policy["permission"][0]["constraint"][0];
        assert_eq!(spatial["operator"], "isAnyOf");
        assert_eq!(spatial["rightOperand"], serde_json::json!(["GB"]));
    }

    #[test]
    fn any_mode_ignores_territories() {
        // Codes present but mode 'any' → no restriction, so no policy.
        let mut item = sample(serde_json::json!([]), serde_json::json!([]));
        item.territory_mode = "any".to_owned();
        item.territories = Some("US".to_owned());
        assert!(super::odrl_policy(&item, "https://example.com/read/x").is_none());
        assert!(super::reuse_constraint_prose(&item).is_none());
    }

    #[test]
    fn to_ninjs_attaches_policy_and_newsml_prose() {
        let mut item = sample(serde_json::json!([]), serde_json::json!([]));
        item.reuse_embargo_until = Some(embargo_instant());
        item.territory_mode = "allow".to_owned();
        item.territories = Some("GB, IE".to_owned());
        // ninjs gets the structured ODRL policy targeting the article URI.
        let value = to_ninjs("https://example.com", &item);
        assert_eq!(value["odrl:hasPolicy"]["permission"][0]["target"], "https://example.com/read/test-slug");
        // NewsML-G2 gets the human-readable prose as an extra <usageTerms>.
        let xml = to_newsml("https://example.com", &item);
        assert!(xml.contains("Reuse embargoed until 2001-09-09T01:46:40Z."));
        assert!(xml.contains("Reuse permitted only in: GB, IE."));
    }

    #[test]
    fn ninjs_to_draft_maps_a_wire_item() {
        let item = serde_json::json!({
            "headline": "Hello, Wire!",
            "description_text": "A dek from the wire.",
            "body_text": "One.\n\nTwo.",
            "subject": [{"name": "Transport"}, {"code": "15000000", "name": "sport"}],
        });
        let draft = super::ninjs_to_draft(&item, None).expect("has a headline");
        assert_eq!(draft.title, "Hello, Wire!");
        assert_eq!(draft.slug, "hello-wire");
        assert_eq!(draft.dek, "A dek from the wire.");
        assert_eq!(draft.category, "Transport");
        assert_eq!(draft.tags, serde_json::json!(["15000000"]));
        assert_eq!(draft.body.as_array().unwrap().len(), 2);
        // A headline-less item is not ingestable.
        assert!(super::ninjs_to_draft(&serde_json::json!({ "body_text": "x" }), None).is_none());
    }

    #[test]
    fn nitf_to_draft_extracts_headline_abstract_and_body() {
        let xml = "<nitf><head><title>ignored</title></head><body><body.head>\
            <hedline><hl1>NITF Wire Headline</hl1></hedline>\
            <abstract><p>The NITF abstract.</p></abstract></body.head>\
            <body.content><p>First para.</p><p>Second para.</p></body.content>\
            </body></nitf>";
        let draft = super::nitf_to_draft(xml, None).expect("has a headline");
        assert_eq!(draft.title, "NITF Wire Headline");
        assert_eq!(draft.slug, "nitf-wire-headline");
        assert_eq!(draft.dek, "The NITF abstract.");
        let paras = draft.body.as_array().unwrap();
        assert_eq!(paras.len(), 2);
        assert_eq!(paras[0]["text"], "First para.");
        // No <hl1>/<title> → not ingestable.
        assert!(super::nitf_to_draft("<nitf><body></body></nitf>", None).is_none());
    }
}
