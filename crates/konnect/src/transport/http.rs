//! Streamable HTTP transport (MCP spec 2025-03-26 / 2025-06-18).
//!
//! A single MCP endpoint at `/mcp` supports:
//!   POST /mcp    — client sends one JSON-RPC message.
//!                  Requests get an `application/json` response body;
//!                  notifications/responses get `202 Accepted` with no body.
//!   GET  /mcp    — opens an SSE stream for server-initiated messages
//!                  (e.g. `notifications/tools/list_changed`).
//!   DELETE /mcp  — 405 (this server is stateless; no session termination).
//!
//! `GET /mcp/sse` is kept as an alias for older configs. `GET /health`
//! returns "ok" for probes.
//!
//! Security: the bind is validated by [`HttpPolicy`] before this runs. Every
//! request's `Host` and `Origin` (when present) must name loopback, or 403,
//! which defeats DNS rebinding. With a token configured, every route but
//! `/health` also needs `Authorization: Bearer <token>`, or 401.

use super::http_policy::{origin_allowed, HttpPolicy};
use anyhow::Result;
use axum::{
    extract::{Request, State},
    http::{header, HeaderMap, StatusCode},
    middleware::{self, Next},
    response::{
        sse::{Event, Sse},
        IntoResponse, Response,
    },
    routing::get,
    Json, Router,
};
use konnect_core::mcp::handler::McpHandler;
use serde_json::Value;
use std::convert::Infallible;
use std::sync::Arc;
use tokio_stream::wrappers::ReceiverStream;
use tracing::{info, warn};

#[derive(Clone)]
struct AppState {
    handler: Arc<McpHandler>,
}

/// Run the MCP server over Streamable HTTP.
pub async fn run_http(handler: McpHandler, policy: HttpPolicy) -> Result<()> {
    let addr = policy.addr();
    if policy.is_remote() {
        warn!(
            "HTTP transport is bound to non-loopback address {addr} (http_allow_remote): \
             every tool, including ones that write files, is reachable from the network, \
             guarded only by the bearer token. Use TLS or a trusted network."
        );
    } else if !policy.has_token() {
        info!("HTTP transport has no http_token: any local process can call its tools");
    }

    let state = AppState {
        handler: Arc::new(handler),
    };

    let app = Router::new()
        .route(
            "/mcp",
            axum::routing::post(handle_post)
                .get(handle_sse)
                .delete(|| async { StatusCode::METHOD_NOT_ALLOWED }),
        )
        .route("/mcp/sse", get(handle_sse)) // legacy alias
        .route("/health", get(handle_health))
        .layer(middleware::from_fn_with_state(Arc::new(policy), guard))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    info!("Streamable HTTP transport listening on http://{addr}/mcp");

    axum::serve(listener, app).await?;
    Ok(())
}

async fn guard(State(policy): State<Arc<HttpPolicy>>, request: Request, next: Next) -> Response {
    match refusal(&policy, request.uri().path(), request.headers()) {
        Some(response) => response,
        None => next.run(request).await,
    }
}

/// Absent `Host` and `Origin` pass: native MCP clients and curl may omit
/// them, and a browser always sends both. A header that is present but not
/// valid UTF-8 is refused.
fn refusal(policy: &HttpPolicy, path: &str, headers: &HeaderMap) -> Option<Response> {
    let value = |name| headers.get(name).map(|v| v.to_str().unwrap_or(""));

    if value(header::HOST).is_some_and(|host| !policy.host_allowed(host)) {
        return Some((StatusCode::FORBIDDEN, "Host not allowed").into_response());
    }
    if value(header::ORIGIN).is_some_and(|origin| !origin_allowed(origin)) {
        return Some((StatusCode::FORBIDDEN, "Origin not allowed").into_response());
    }
    if path != "/health" && !policy.authorized(value(header::AUTHORIZATION)) {
        return Some(
            (
                StatusCode::UNAUTHORIZED,
                [(header::WWW_AUTHENTICATE, "Bearer")],
                "Missing or invalid bearer token",
            )
                .into_response(),
        );
    }
    None
}

async fn handle_post(State(state): State<AppState>, Json(body): Json<Value>) -> Response {
    match state.handler.handle_message(body).await {
        // JSON-RPC request → single JSON response object.
        Some(resp) => Json(resp).into_response(),
        // JSON-RPC notification or response → 202 Accepted, no body.
        None => StatusCode::ACCEPTED.into_response(),
    }
}

/// GET on the MCP endpoint: open an SSE stream for server-initiated messages.
async fn handle_sse(
    State(state): State<AppState>,
) -> Sse<impl tokio_stream::Stream<Item = Result<Event, Infallible>>> {
    let (tx, rx) = tokio::sync::mpsc::channel::<Event>(32);

    // Register SSE sender with handler so it can push notifications
    // (e.g. tools/list_changed after load_toolset / unload_toolset).
    state.handler.register_sse_sender(tx).await;

    use tokio_stream::StreamExt;
    let stream = ReceiverStream::new(rx).map(Ok);
    Sse::new(stream)
}

async fn handle_health() -> &'static str {
    "ok"
}
