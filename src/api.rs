//! REST API — queue management + filesystem access + ACP WebSocket endpoint.
//!
//! State observation (connections, chunks, prompt turns, terminals, permissions)
//! is handled by the durable stream at :port/streams/durable-acp-state.
//! Clients subscribe via SSE using @durable-acp/state StreamDB.
//!
//! This API only exposes:
//! - /acp (WebSocket) — ACP client transport, spawns conductor per connection
//! - /api/v1/*/queue/* — queue management (pause, resume, cancel, clear, reorder)
//! - /api/v1/*/files, /api/v1/*/fs/tree — filesystem access (not in the stream)
//! - /api/v1/registry — peer discovery

use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::extract::ws::{WebSocket, WebSocketUpgrade};
use axum::response::IntoResponse;
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use crate::app::AppState;
use crate::state::TerminalState;

/// Config for the WebSocket ACP endpoint — spawns a new conductor per connection.
#[derive(Clone)]
pub struct AcpEndpointConfig {
    pub agent_command: Vec<String>,
    pub durable_streams: crate::durable_streams::EmbeddedDurableStreams,
}

pub fn router(app: Arc<AppState>, acp_config: Option<AcpEndpointConfig>) -> Router {
    let api = Router::new()
        // Queue management
        .route("/api/v1/connections/{id}/queue/pause", post(pause_queue))
        .route("/api/v1/connections/{id}/queue/resume", post(resume_queue))
        .route("/api/v1/connections/{id}/queue/{turn_id}", delete(cancel_queued_turn))
        .route("/api/v1/connections/{id}/queue", delete(clear_queue).put(reorder_queue))
        // Filesystem access (not in the durable stream)
        .route("/api/v1/connections/{id}/files", get(get_file))
        .route("/api/v1/connections/{id}/fs/tree", get(get_tree))
        // Peer discovery
        .route("/api/v1/registry", get(get_registry))
        // Terminal state mutation
        .route("/api/v1/connections/{id}/terminals/{tid}", delete(kill_terminal))
        .with_state(app);

    if let Some(config) = acp_config {
        let acp: Router = Router::new()
            .route("/acp", get(ws_acp_handler))
            .with_state(Arc::new(config));
        Router::new().merge(api).merge(acp)
    } else {
        api
    }
}

// ---------------------------------------------------------------------------
// WebSocket ACP transport — spawns a conductor per connection
// ---------------------------------------------------------------------------

async fn ws_acp_handler(
    ws: WebSocketUpgrade,
    State(config): State<Arc<AcpEndpointConfig>>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_acp_session(socket, config))
}

async fn handle_acp_session(socket: WebSocket, config: Arc<AcpEndpointConfig>) {
    use sacp_tokio::AcpAgent;

    let agent = match AcpAgent::from_args(config.agent_command.clone()) {
        Ok(a) => a,
        Err(e) => {
            tracing::error!("Failed to parse agent command: {}", e);
            return;
        }
    };

    let app = match AppState::with_shared_streams(config.durable_streams.clone()).await {
        Ok(a) => Arc::new(a),
        Err(e) => {
            tracing::error!("Failed to create app state: {}", e);
            return;
        }
    };

    let conductor = crate::conductor::build_conductor(app, agent);
    let transport = crate::transport::AxumWsTransport { socket };

    if let Err(e) = conductor.run(transport).await {
        tracing::warn!("ACP WebSocket session ended: {}", e);
    }
}

// ---------------------------------------------------------------------------
// Queue management
// ---------------------------------------------------------------------------

async fn pause_queue(
    Path(_id): Path<String>,
    State(app): State<Arc<AppState>>,
) -> Result<Json<serde_json::Value>, axum::http::StatusCode> {
    app.set_paused(true).await.map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(serde_json::json!({ "paused": true })))
}

async fn resume_queue(
    Path(_id): Path<String>,
    State(app): State<Arc<AppState>>,
) -> Result<Json<serde_json::Value>, axum::http::StatusCode> {
    app.set_paused(false).await.map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(serde_json::json!({ "paused": false })))
}

async fn cancel_queued_turn(
    Path((_id, turn_id)): Path<(String, String)>,
    State(app): State<Arc<AppState>>,
) -> Result<Json<serde_json::Value>, axum::http::StatusCode> {
    let removed = app.cancel_queued_turn(&turn_id).await
        .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;
    if removed {
        Ok(Json(serde_json::json!({ "cancelled": turn_id })))
    } else {
        Err(axum::http::StatusCode::NOT_FOUND)
    }
}

async fn clear_queue(
    Path(_id): Path<String>,
    State(app): State<Arc<AppState>>,
) -> Result<Json<serde_json::Value>, axum::http::StatusCode> {
    let count = app.cancel_all_queued().await
        .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(serde_json::json!({ "cancelled": count })))
}

#[derive(Debug, Deserialize)]
struct ReorderBody {
    order: Vec<String>,
}

async fn reorder_queue(
    Path(_id): Path<String>,
    State(app): State<Arc<AppState>>,
    Json(body): Json<ReorderBody>,
) -> Result<Json<serde_json::Value>, axum::http::StatusCode> {
    app.reorder_queue(&body.order).await
        .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(serde_json::json!({ "reordered": true })))
}

// ---------------------------------------------------------------------------
// Filesystem access (not in the durable stream)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct FileQuery {
    path: Option<String>,
}

fn resolve_path(
    app: &AppState,
    snapshot: &crate::state::Collections,
    id: &str,
    rel: &str,
) -> Result<PathBuf, axum::http::StatusCode> {
    let conn = snapshot.connections.get(id)
        .or_else(|| snapshot.connections.values().find(|c| c.logical_connection_id == app.logical_connection_id))
        .ok_or(axum::http::StatusCode::NOT_FOUND)?;
    let cwd = conn.cwd.as_deref().ok_or(axum::http::StatusCode::NOT_FOUND)?;
    let cwd = PathBuf::from(cwd);
    let full = cwd.join(rel);
    let canonical = full.canonicalize().map_err(|_| axum::http::StatusCode::NOT_FOUND)?;
    let cwd_canonical = cwd.canonicalize().map_err(|_| axum::http::StatusCode::NOT_FOUND)?;
    if !canonical.starts_with(&cwd_canonical) {
        return Err(axum::http::StatusCode::FORBIDDEN);
    }
    Ok(canonical)
}

async fn get_file(
    Path(id): Path<String>,
    Query(query): Query<FileQuery>,
    State(app): State<Arc<AppState>>,
) -> Result<String, axum::http::StatusCode> {
    let rel = query.path.as_deref().unwrap_or(".");
    let snapshot = app.durable_streams.stream_db.snapshot().await;
    let path = resolve_path(&app, &snapshot, &id, rel)?;
    std::fs::read_to_string(path).map_err(|_| axum::http::StatusCode::NOT_FOUND)
}

#[derive(Debug, Serialize)]
struct TreeEntry {
    name: String,
    #[serde(rename = "type")]
    entry_type: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    size: Option<u64>,
}

async fn get_tree(
    Path(id): Path<String>,
    Query(query): Query<FileQuery>,
    State(app): State<Arc<AppState>>,
) -> Result<Json<Vec<TreeEntry>>, axum::http::StatusCode> {
    let rel = query.path.as_deref().unwrap_or(".");
    let snapshot = app.durable_streams.stream_db.snapshot().await;
    let path = resolve_path(&app, &snapshot, &id, rel)?;
    let entries = std::fs::read_dir(&path).map_err(|_| axum::http::StatusCode::NOT_FOUND)?;
    let mut result = Vec::new();
    for entry in entries.flatten() {
        let meta = entry.metadata().ok();
        let is_dir = meta.as_ref().map(|m| m.is_dir()).unwrap_or(false);
        result.push(TreeEntry {
            name: entry.file_name().to_string_lossy().to_string(),
            entry_type: if is_dir { "directory" } else { "file" },
            size: if is_dir { None } else { meta.map(|m| m.len()) },
        });
    }
    result.sort_by(|a, b| a.entry_type.cmp(&b.entry_type).then(a.name.cmp(&b.name)));
    Ok(Json(result))
}

// ---------------------------------------------------------------------------
// Peer discovery + terminal state mutation
// ---------------------------------------------------------------------------

async fn get_registry(
    State(_app): State<Arc<AppState>>,
) -> Result<Json<crate::registry::Registry>, axum::http::StatusCode> {
    crate::registry::read_registry().map(Json).map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)
}

async fn kill_terminal(
    Path((_id, tid)): Path<(String, String)>,
    State(app): State<Arc<AppState>>,
) -> Result<Json<serde_json::Value>, axum::http::StatusCode> {
    let mut snapshot = app.durable_streams.stream_db.snapshot().await;
    let row = snapshot.terminals.get_mut(&tid)
        .ok_or(axum::http::StatusCode::NOT_FOUND)?;
    row.state = TerminalState::Released;
    row.updated_at = crate::app::now_ms();
    let updated = row.clone();
    drop(snapshot);
    app.write_state_event("terminal", "update", &tid, Some(&updated))
        .await.map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(serde_json::json!({ "killed": tid })))
}
