use std::convert::Infallible;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Path, Query, State};
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::response::{IntoResponse, sse::{Event, Sse}};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use futures::stream::Stream;
use serde::{Deserialize, Serialize};

use crate::app::AppState;
use crate::state::{ChunkRow, ChunkType, CollectionChange, ConnectionRow, PromptTurnRow, PromptTurnState, TerminalRow, TerminalState};

/// Config for the WebSocket ACP endpoint — spawns a new conductor per connection.
#[derive(Clone)]
pub struct AcpEndpointConfig {
    pub agent_command: Vec<String>,
    pub durable_streams: crate::durable_streams::EmbeddedDurableStreams,
}

pub fn router(app: Arc<AppState>, acp_config: Option<AcpEndpointConfig>) -> Router {
    let api = Router::new()
        .route("/api/v1/connections", get(list_connections))
        .route("/api/v1/connections/{id}/queue", get(get_queue).delete(clear_queue).put(reorder_queue))
        .route("/api/v1/connections/{id}/queue/{turn_id}", delete(cancel_queued_turn))
        .route("/api/v1/connections/{id}/queue/pause", post(pause_queue))
        .route("/api/v1/connections/{id}/queue/resume", post(resume_queue))
        .route("/api/v1/connections/{id}/prompt-turns", get(list_prompt_turns))
        .route("/api/v1/prompt-turns/{id}/stream", get(stream_prompt_turn))
        .route("/api/v1/prompt-turns/{id}/chunks", get(get_chunks))
        .route("/api/v1/registry", get(get_registry))
        .route("/api/v1/connections/{id}/files", get(get_file))
        .route("/api/v1/connections/{id}/fs/tree", get(get_tree))
        .route("/api/v1/connections/{id}/terminals", get(list_terminals).post(create_terminal))
        .route("/api/v1/connections/{id}/terminals/{tid}", get(get_terminal).delete(kill_terminal))
        .route("/api/v1/connections/{id}/terminals/{tid}/output", get(stream_terminal_output))
        .with_state(app);

    if let Some(config) = acp_config {
        // /acp has its own state — use .with_state() to erase the type before nesting
        let acp: Router = Router::new()
            .route("/acp", get(ws_acp_handler))
            .with_state(Arc::new(config));
        Router::new().merge(api).merge(acp)
    } else {
        api
    }
}

/// WebSocket ACP endpoint — each connection gets its own conductor + agent.
/// Pure SDK: ConductorImpl::new_agent + ProxiesAndAgent + ByteStreams.
async fn ws_acp_handler(
    ws: WebSocketUpgrade,
    State(config): State<Arc<AcpEndpointConfig>>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_acp_session(socket, config))
}

/// Spawn a conductor for this WebSocket client session.
async fn handle_acp_session(socket: WebSocket, config: Arc<AcpEndpointConfig>) {
    use futures::StreamExt;
    use sacp_conductor::{ConductorImpl, McpBridgeMode, ProxiesAndAgent};
    use sacp_tokio::AcpAgent;

    // Spawn agent subprocess
    let agent = match AcpAgent::from_args(config.agent_command.clone()) {
        Ok(a) => a,
        Err(e) => {
            tracing::error!("Failed to parse agent command: {}", e);
            return;
        }
    };

    // Create per-session app state (shares durable streams server)
    let app = match AppState::with_shared_streams(config.durable_streams.clone()).await {
        Ok(a) => Arc::new(a),
        Err(e) => {
            tracing::error!("Failed to create app state: {}", e);
            return;
        }
    };

    // Bridge WebSocket ↔ sacp::Channel — SDK's message-level transport.
    // Each WS text frame = one JSON-RPC message (no byte-level pipes).
    let (channel_conductor, channel_ws) = sacp::Channel::duplex();
    let sacp::Channel { tx: to_conductor_tx, rx: mut from_conductor_rx } = channel_ws;
    let (mut ws_write, mut ws_read) = socket.split();

    // WS → conductor (incoming: parse JSON-RPC from text frames)
    tokio::spawn(async move {
        while let Some(Ok(msg)) = ws_read.next().await {
            if let Message::Text(text) = msg {
                match serde_json::from_str::<sacp::jsonrpcmsg::Message>(&text) {
                    Ok(parsed) => {
                        if to_conductor_tx.unbounded_send(Ok(parsed)).is_err() { break; }
                    }
                    Err(e) => {
                        tracing::warn!("WS parse error: {}", e);
                    }
                }
            }
        }
    });

    // Conductor → WS (outgoing: serialize JSON-RPC to text frames)
    tokio::spawn(async move {
        use futures::SinkExt;
        while let Some(msg) = from_conductor_rx.next().await {
            match msg {
                Ok(message) => {
                    if let Ok(json) = serde_json::to_string(&message) {
                        if ws_write.send(Message::Text(json.into())).await.is_err() { break; }
                    }
                }
                Err(_) => break,
            }
        }
    });

    // Run conductor on the Channel — SDK's zero-copy message transport
    let conductor = ConductorImpl::new_agent(
        "durable-acp".to_string(),
        ProxiesAndAgent::new(agent)
            .proxy(crate::durable_state_proxy::DurableStateProxy { app })
            .proxy(crate::peer_mcp::PeerMcpProxy),
        McpBridgeMode::default(),
    );

    if let Err(e) = conductor.run(channel_conductor).await {
        tracing::warn!("ACP WebSocket session ended: {}", e);
    }
}

async fn list_connections(State(app): State<Arc<AppState>>) -> Json<Vec<ConnectionRow>> {
    let snapshot = app.durable_streams.stream_db.snapshot().await;
    Json(snapshot.connections.into_values().collect())
}

async fn get_queue(
    Path(id): Path<String>,
    State(app): State<Arc<AppState>>,
) -> Json<Vec<PromptTurnRow>> {
    let snapshot = app.durable_streams.stream_db.snapshot().await;
    Json(
        snapshot.prompt_turns.into_values()
            .filter(|row| row.logical_connection_id == id && row.state == PromptTurnState::Queued)
            .collect(),
    )
}

async fn list_prompt_turns(
    Path(id): Path<String>,
    State(app): State<Arc<AppState>>,
) -> Json<Vec<PromptTurnRow>> {
    let snapshot = app.durable_streams.stream_db.snapshot().await;
    let mut turns: Vec<PromptTurnRow> = snapshot.prompt_turns.into_values()
        .filter(|row| row.logical_connection_id == id)
        .collect();
    turns.sort_by_key(|t| t.started_at);
    Json(turns)
}

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

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StreamQuery {
    after_seq: Option<i64>,
}

async fn get_chunks(
    Path(id): Path<String>,
    State(app): State<Arc<AppState>>,
) -> Json<Vec<ChunkRow>> {
    let snapshot = app.durable_streams.stream_db.snapshot().await;
    let mut chunks: Vec<ChunkRow> = snapshot.chunks.into_values()
        .filter(|c| c.prompt_turn_id == id).collect();
    chunks.sort_by_key(|c| c.seq);
    Json(chunks)
}

async fn stream_prompt_turn(
    Path(prompt_turn_id): Path<String>,
    Query(query): Query<StreamQuery>,
    State(app): State<Arc<AppState>>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let after_seq = query.after_seq.unwrap_or(-1);
    let mut rx = app.durable_streams.stream_db.subscribe_changes();

    let stream = async_stream::stream! {
        let snapshot = app.durable_streams.stream_db.snapshot().await;
        let mut last_seq = after_seq;
        let mut existing: Vec<&ChunkRow> = snapshot.chunks.values()
            .filter(|c| c.prompt_turn_id == prompt_turn_id && c.seq > after_seq).collect();
        existing.sort_by_key(|c| c.seq);

        for chunk in &existing {
            last_seq = chunk.seq;
            if let Ok(json) = serde_json::to_string(chunk) {
                yield Ok(Event::default().data(json));
            }
            if is_terminal_chunk(chunk) { return; }
        }

        if let Some(turn) = snapshot.prompt_turns.get(&prompt_turn_id) {
            if matches!(turn.state, PromptTurnState::Completed | PromptTurnState::Cancelled | PromptTurnState::Broken) {
                return;
            }
        }
        drop(snapshot);

        loop {
            match tokio::time::timeout(Duration::from_secs(120), rx.recv()).await {
                Ok(Ok(CollectionChange::Chunks)) => {
                    let snapshot = app.durable_streams.stream_db.snapshot().await;
                    let mut new: Vec<&ChunkRow> = snapshot.chunks.values()
                        .filter(|c| c.prompt_turn_id == prompt_turn_id && c.seq > last_seq).collect();
                    new.sort_by_key(|c| c.seq);
                    for chunk in &new {
                        last_seq = chunk.seq;
                        if let Ok(json) = serde_json::to_string(chunk) {
                            yield Ok(Event::default().data(json));
                        }
                        if is_terminal_chunk(chunk) { return; }
                    }
                }
                Ok(Ok(_)) => {
                    let snapshot = app.durable_streams.stream_db.snapshot().await;
                    if let Some(turn) = snapshot.prompt_turns.get(&prompt_turn_id) {
                        if matches!(turn.state, PromptTurnState::Completed | PromptTurnState::Cancelled | PromptTurnState::Broken) {
                            return;
                        }
                    }
                }
                Ok(Err(_)) | Err(_) => return,
            }
        }
    };

    Sse::new(stream).keep_alive(
        axum::response::sse::KeepAlive::new().interval(Duration::from_secs(15)).text(""),
    )
}

async fn get_registry(
    State(_app): State<Arc<AppState>>,
) -> Result<Json<crate::registry::Registry>, axum::http::StatusCode> {
    crate::registry::read_registry().map(Json).map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)
}

fn is_terminal_chunk(chunk: &ChunkRow) -> bool {
    matches!(chunk.chunk_type, ChunkType::Stop | ChunkType::Error)
}

// --- Queue management ---

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

// --- File system access ---

#[derive(Debug, Deserialize)]
struct FileQuery {
    path: Option<String>,
}

/// Resolve the cwd for a connection, validating that the requested path
/// doesn't escape it.
fn resolve_path(
    app: &AppState,
    snapshot: &crate::state::Collections,
    id: &str,
    rel: &str,
) -> Result<PathBuf, axum::http::StatusCode> {
    let conn = snapshot.connections.get(id)
        .or_else(|| {
            // Fall back to matching by logical_connection_id prefix or any connection
            snapshot.connections.values().find(|c| c.logical_connection_id == app.logical_connection_id)
        })
        .ok_or(axum::http::StatusCode::NOT_FOUND)?;
    let cwd = conn.cwd.as_deref().ok_or(axum::http::StatusCode::NOT_FOUND)?;
    let cwd = PathBuf::from(cwd);
    let full = cwd.join(rel);
    // Canonicalize to resolve ../ traversals, then check prefix
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

// --- Terminal management ---

async fn list_terminals(
    Path(id): Path<String>,
    State(app): State<Arc<AppState>>,
) -> Json<Vec<TerminalRow>> {
    let snapshot = app.durable_streams.stream_db.snapshot().await;
    Json(snapshot.terminals.into_values()
        .filter(|t| t.logical_connection_id == id || id == app.logical_connection_id)
        .collect())
}

async fn get_terminal(
    Path((_id, tid)): Path<(String, String)>,
    State(app): State<Arc<AppState>>,
) -> Result<Json<TerminalRow>, axum::http::StatusCode> {
    let snapshot = app.durable_streams.stream_db.snapshot().await;
    snapshot.terminals.get(&tid).cloned()
        .map(Json)
        .ok_or(axum::http::StatusCode::NOT_FOUND)
}

async fn create_terminal(
    Path(_id): Path<String>,
    State(_app): State<Arc<AppState>>,
) -> Result<Json<serde_json::Value>, axum::http::StatusCode> {
    // Terminal creation goes through the ACP client connection (not REST).
    // The DurableStateProxy records terminal state when the agent creates one.
    Err(axum::http::StatusCode::NOT_IMPLEMENTED)
}

async fn kill_terminal(
    Path((_id, tid)): Path<(String, String)>,
    State(app): State<Arc<AppState>>,
) -> Result<Json<serde_json::Value>, axum::http::StatusCode> {
    // Update terminal state to Released in the state stream
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

async fn stream_terminal_output(
    Path((_id, tid)): Path<(String, String)>,
    State(app): State<Arc<AppState>>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let mut rx = app.durable_streams.stream_db.subscribe_changes();

    let stream = async_stream::stream! {
        // Send current terminal state
        let snapshot = app.durable_streams.stream_db.snapshot().await;
        if let Some(term) = snapshot.terminals.get(&tid) {
            if let Ok(json) = serde_json::to_string(term) {
                yield Ok(Event::default().event("terminal").data(json));
            }
        }

        // Stream chunks associated with this terminal's prompt turn
        let prompt_turn_id = snapshot.terminals.get(&tid)
            .and_then(|t| t.prompt_turn_id.clone());
        let mut last_seq: i64 = -1;

        if let Some(ref pt_id) = prompt_turn_id {
            let mut existing: Vec<&ChunkRow> = snapshot.chunks.values()
                .filter(|c| c.prompt_turn_id == *pt_id).collect();
            existing.sort_by_key(|c| c.seq);
            for chunk in &existing {
                last_seq = chunk.seq;
                if let Ok(json) = serde_json::to_string(chunk) {
                    yield Ok(Event::default().event("chunk").data(json));
                }
            }
        }
        drop(snapshot);

        // Stream new chunks as they arrive
        loop {
            match tokio::time::timeout(Duration::from_secs(120), rx.recv()).await {
                Ok(Ok(CollectionChange::Chunks)) => {
                    if let Some(ref pt_id) = prompt_turn_id {
                        let snapshot = app.durable_streams.stream_db.snapshot().await;
                        let mut new: Vec<&ChunkRow> = snapshot.chunks.values()
                            .filter(|c| c.prompt_turn_id == *pt_id && c.seq > last_seq).collect();
                        new.sort_by_key(|c| c.seq);
                        for chunk in &new {
                            last_seq = chunk.seq;
                            if let Ok(json) = serde_json::to_string(chunk) {
                                yield Ok(Event::default().event("chunk").data(json));
                            }
                        }
                    }
                }
                Ok(Ok(CollectionChange::Terminals)) => {
                    let snapshot = app.durable_streams.stream_db.snapshot().await;
                    if let Some(term) = snapshot.terminals.get(&tid) {
                        if let Ok(json) = serde_json::to_string(term) {
                            yield Ok(Event::default().event("terminal").data(json));
                        }
                        if matches!(term.state, TerminalState::Exited | TerminalState::Released | TerminalState::Broken) {
                            return;
                        }
                    }
                }
                Ok(Ok(_)) => {}
                Ok(Err(_)) | Err(_) => return,
            }
        }
    };

    Sse::new(stream).keep_alive(
        axum::response::sse::KeepAlive::new().interval(Duration::from_secs(15)).text(""),
    )
}
