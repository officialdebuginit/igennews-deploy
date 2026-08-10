//! Module 7 — in-process real-time broker for the Server-Sent Events stream.
//!
//! Ported faithfully from the legacy `realtime.py` and `GET /events/stream`
//! (`api.py`). Domain mutations publish a change *type* — never row data ("M4 D3:
//! invalidate, never patch") — and the stream forwards each type to a browser,
//! which invalidates the matching queries and refetches. The server's next answer
//! stays the authority on what changed.
//!
//! A `tokio::sync::broadcast` channel replaces the legacy per-subscriber asyncio
//! queues. `broadcast` already drops the *oldest* message for a receiver that
//! falls behind, which is exactly the legacy `_MAX_QUEUE` drop-oldest policy: an
//! older invalidation signal is disposable because a later one (or the client's
//! poll fallback) reconverges the same screen. As in legacy, an emit with no
//! subscribers is a silent no-op — a UI hint layered on an already-committed
//! mutation must never break its caller.

use std::{convert::Infallible, time::Duration};

use axum::{
    Extension, Json,
    extract::Query,
    http::StatusCode,
    response::{
        IntoResponse,
        sse::{Event, KeepAlive, Sse},
    },
};
use meridian_contracts::ApiError;
use meridian_identity::IdentityService;
use serde::Deserialize;
use tokio::sync::broadcast;
use tokio_stream::{StreamExt, wrappers::BroadcastStream};

/// Per-connection backlog bound. Bounded on purpose (mirrors legacy `_MAX_QUEUE`):
/// one browser that stops reading must not let its backlog grow without limit.
const MAX_BACKLOG: usize = 100;

/// Heartbeat interval, matching legacy `_SSE_HEARTBEAT_SECONDS`. Keeps the
/// connection alive through proxies that would close an idle stream.
const HEARTBEAT_SECONDS: u64 = 25;

/// In-process fan-out handle. Cheap to clone (it is a broadcast `Sender`), so it
/// rides in an axum `Extension` alongside the other services and route handlers
/// call [`RealtimeHub::emit`] after a successful mutation.
#[derive(Clone)]
pub struct RealtimeHub {
    tx: broadcast::Sender<String>,
}

impl Default for RealtimeHub {
    fn default() -> Self {
        Self::new()
    }
}

impl RealtimeHub {
    #[must_use]
    pub fn new() -> Self {
        let (tx, _rx) = broadcast::channel(MAX_BACKLOG);
        Self { tx }
    }

    /// Announce a change *type* to every open stream. Deliberately total and
    /// silent: `send` returns `Err` only when there are no subscribers, which is
    /// the legacy no-op path, so the result is dropped.
    pub fn emit(&self, event_type: &str) {
        let _ = self.tx.send(event_type.to_owned());
    }

    fn subscribe(&self) -> broadcast::Receiver<String> {
        self.tx.subscribe()
    }
}

#[derive(Deserialize)]
pub(crate) struct StreamQuery {
    access_token: String,
}

/// `GET /api/v1/events/stream?access_token=…`
///
/// `EventSource` cannot set an `Authorization` header, so the access token arrives
/// as a query parameter and is validated up front through the same path as every
/// other route. An invalid or expired token is a 401 here and now, never a hung
/// connection.
pub(crate) async fn events_stream(
    Extension(identity): Extension<IdentityService>,
    Extension(hub): Extension<RealtimeHub>,
    Query(query): Query<StreamQuery>,
) -> Result<impl IntoResponse, (StatusCode, Json<ApiError>)> {
    identity
        .resolve_access_token(&query.access_token)
        .await
        .map_err(|_| {
            (
                StatusCode::UNAUTHORIZED,
                Json(ApiError {
                    detail: "Could not validate credentials".to_owned(),
                }),
            )
        })?;

    // Announce liveness immediately (a `:` line is an SSE comment, ignored by the
    // browser's onmessage) so the client's "Live" indicator turns green without
    // waiting out the first heartbeat — matching legacy's leading `: connected`.
    let connected = tokio_stream::once(Event::default().comment("connected"));
    let changes = BroadcastStream::new(hub.subscribe()).map(|message| match message {
        // Only a type crosses the wire, never row data (M4 D3).
        Ok(event_type) => Event::default().data(
            serde_json::json!({ "type": event_type }).to_string(),
        ),
        // A lagging receiver dropped older signals; a later event (or the poll
        // fallback) reconverges the screen, so surface a harmless comment.
        Err(_lagged) => Event::default().comment("lagged"),
    });
    let stream = connected.chain(changes).map(Ok::<Event, Infallible>);

    Ok(Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(HEARTBEAT_SECONDS))
            .text("ping"),
    ))
}
