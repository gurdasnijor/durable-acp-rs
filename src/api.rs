use std::convert::Infallible;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Path, Query, State};
use axum::response::sse::{Event, Sse};
use axum::routing::{delete, get, post, put};
use axum::{Json, Router};
use agent_client_protocol::{CancelNotification, ContentBlock, PromptRequest};
use futures::stream::Stream;
use sacp::Agent;
use serde::{Deserialize, Serialize};

use crate::app::AppState;
use crate::state::{ChunkRow, ChunkType, CollectionChange, ConnectionRow, PromptTurnRow, PromptTurnState};

pub fn router(app: Arc<AppState>) -> Router {
    Router::new()
        .route("/api/v1/connections", get(list_connections))
        .route("/api/v1/connections/{id}/queue", get(get_queue).delete(clear_queue).put(reorder_queue))
        .route("/api/v1/connections/{id}/queue/{turn_id}", delete(cancel_queued_turn))
        .route("/api/v1/connections/{id}/queue/pause", post(pause_queue))
        .route("/api/v1/connections/{id}/queue/resume", post(resume_queue))
        .route("/api/v1/connections/{id}/prompt", post(submit_prompt))
        .route("/api/v1/connections/{id}/cancel", post(cancel_turn))
        .route("/api/v1/prompt-turns/{id}/stream", get(stream_prompt_turn))
        .route("/api/v1/prompt-turns/{id}/chunks", get(get_chunks))
        .route("/api/v1/registry", get(get_registry))
        .route("/api/v1/connections/{id}/files", get(get_file))
        .route("/api/v1/connections/{id}/fs/tree", get(get_tree))
        .with_state(app)
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
struct PromptBody {
    session_id: String,
    text: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PromptAccepted {
    queued: bool,
    prompt_turn_id: String,
}

/// Submit a prompt via REST API. Records the turn, sends to agent through the
/// proxy connection, and tracks completion asynchronously.
async fn submit_prompt(
    Path(_id): Path<String>,
    State(app): State<Arc<AppState>>,
    Json(body): Json<PromptBody>,
) -> Result<Json<PromptAccepted>, axum::http::StatusCode> {
    let cx = app.proxy_connection.lock().await.clone()
        .ok_or(axum::http::StatusCode::SERVICE_UNAVAILABLE)?;

    let prompt_turn_id = uuid::Uuid::new_v4().to_string();
    let request = PromptRequest::new(body.session_id.clone(), vec![ContentBlock::from(body.text)]);

    // Record the prompt turn
    let turn = PromptTurnRow {
        prompt_turn_id: prompt_turn_id.clone(),
        logical_connection_id: app.logical_connection_id.clone(),
        session_id: body.session_id.clone(),
        request_id: prompt_turn_id.clone(),
        text: request.prompt.first().and_then(|b| {
            if let ContentBlock::Text(t) = b { Some(t.text.clone()) } else { None }
        }),
        state: PromptTurnState::Active,
        position: None,
        stop_reason: None,
        started_at: crate::app::now_ms(),
        completed_at: None,
    };
    app.write_state_event("prompt_turn", "insert", &prompt_turn_id, Some(&turn))
        .await.map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;

    app.session_to_prompt_turn.write().await
        .insert(body.session_id, prompt_turn_id.clone());

    // Send to agent; completion tracked asynchronously
    let app2 = app.clone();
    let turn_id = prompt_turn_id.clone();
    cx.clone().spawn(async move {
        let result = cx.send_request_to(Agent, request).block_task().await;
        match &result {
            Ok(response) => {
                let _ = app2.record_chunk(&turn_id, ChunkType::Stop, serde_json::json!({ "stopReason": response.stop_reason }).to_string()).await;
                let _ = app2.finish_prompt_turn(&turn_id, format!("{:?}", response.stop_reason).to_lowercase(), PromptTurnState::Completed).await;
            }
            Err(error) => {
                let _ = app2.record_chunk(&turn_id, ChunkType::Error, error.to_string()).await;
                let _ = app2.finish_prompt_turn(&turn_id, error.to_string(), PromptTurnState::Broken).await;
            }
        }
        Ok(())
    }).map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(Json(PromptAccepted { queued: true, prompt_turn_id }))
}

async fn cancel_turn(
    Path(_id): Path<String>,
    State(app): State<Arc<AppState>>,
    Json(body): Json<CancelBody>,
) -> Result<Json<serde_json::Value>, axum::http::StatusCode> {
    let cx = app.proxy_connection.lock().await.clone()
        .ok_or(axum::http::StatusCode::SERVICE_UNAVAILABLE)?;
    cx.send_notification_to(Agent, CancelNotification::new(body.session_id))
        .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(serde_json::json!({ "cancelled": true })))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CancelBody {
    session_id: String,
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
