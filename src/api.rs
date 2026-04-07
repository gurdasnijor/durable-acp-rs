use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Path, Query, State};
use axum::response::sse::{Event, Sse};
use axum::routing::{get, post};
use axum::{Json, Router};
use agent_client_protocol::{CancelNotification, ContentBlock, PromptRequest};
use futures::stream::Stream;
use sacp::Agent;
use serde::{Deserialize, Serialize};

use crate::app::AppState;
use crate::state::{ChunkRow, CollectionChange, ConnectionRow, PromptTurnRow, PromptTurnState};

pub fn router(app: Arc<AppState>) -> Router {
    Router::new()
        .route("/api/v1/connections", get(list_connections))
        .route("/api/v1/connections/{id}/queue", get(get_queue))
        .route("/api/v1/connections/{id}/queue/pause", post(pause_queue))
        .route("/api/v1/connections/{id}/queue/resume", post(resume_queue))
        .route("/api/v1/connections/{id}/prompt", post(submit_prompt))
        .route("/api/v1/connections/{id}/cancel", post(cancel_turn))
        .route(
            "/api/v1/prompt-turns/{id}/stream",
            get(stream_prompt_turn),
        )
        .route("/api/v1/prompt-turns/{id}/chunks", get(get_chunks))
        .route("/api/v1/registry", get(get_registry))
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
        snapshot
            .prompt_turns
            .into_values()
            .filter(|row| row.logical_connection_id == id && row.state == PromptTurnState::Queued)
            .collect(),
    )
}

async fn pause_queue(
    Path(_id): Path<String>,
    State(app): State<Arc<AppState>>,
) -> Result<Json<serde_json::Value>, axum::http::StatusCode> {
    app.set_paused(true)
        .await
        .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(serde_json::json!({ "paused": true })))
}

async fn resume_queue(
    Path(_id): Path<String>,
    State(app): State<Arc<AppState>>,
) -> Result<Json<serde_json::Value>, axum::http::StatusCode> {
    app.set_paused(false)
        .await
        .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;
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

async fn submit_prompt(
    Path(_id): Path<String>,
    State(app): State<Arc<AppState>>,
    Json(body): Json<PromptBody>,
) -> Result<Json<PromptAccepted>, axum::http::StatusCode> {
    let Some(cx) = app.proxy_connection.lock().await.clone() else {
        return Err(axum::http::StatusCode::SERVICE_UNAVAILABLE);
    };

    // Pre-generate the prompt_turn_id so we can return it immediately
    let prompt_turn_id = uuid::Uuid::new_v4().to_string();
    app.set_next_prompt_turn_id(prompt_turn_id.clone()).await;

    let request = PromptRequest::new(body.session_id, vec![ContentBlock::from(body.text)]);
    tokio::spawn(async move {
        let _ = cx.send_request_to(Agent, request).block_task().await;
    });
    Ok(Json(PromptAccepted {
        queued: true,
        prompt_turn_id,
    }))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CancelBody {
    session_id: String,
}

async fn cancel_turn(
    Path(_id): Path<String>,
    State(app): State<Arc<AppState>>,
    Json(body): Json<CancelBody>,
) -> Result<Json<serde_json::Value>, axum::http::StatusCode> {
    let Some(cx) = app.proxy_connection.lock().await.clone() else {
        return Err(axum::http::StatusCode::SERVICE_UNAVAILABLE);
    };
    cx.send_notification_to(Agent, CancelNotification::new(body.session_id))
        .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(serde_json::json!({ "cancelled": true })))
}

// --- Chunk streaming ---

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
    let mut chunks: Vec<ChunkRow> = snapshot
        .chunks
        .into_values()
        .filter(|c| c.prompt_turn_id == id)
        .collect();
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
        // Send existing chunks first
        let snapshot = app.durable_streams.stream_db.snapshot().await;
        let mut last_seq = after_seq;
        let mut existing: Vec<&ChunkRow> = snapshot
            .chunks
            .values()
            .filter(|c| c.prompt_turn_id == prompt_turn_id && c.seq > after_seq)
            .collect();
        existing.sort_by_key(|c| c.seq);

        for chunk in &existing {
            last_seq = chunk.seq;
            if let Ok(json) = serde_json::to_string(chunk) {
                yield Ok(Event::default().data(json));
            }
            if is_terminal_chunk(chunk) {
                return;
            }
        }

        // Check if the turn is already finished
        if let Some(turn) = snapshot.prompt_turns.get(&prompt_turn_id) {
            if matches!(
                turn.state,
                PromptTurnState::Completed
                    | PromptTurnState::Cancelled
                    | PromptTurnState::Broken
            ) {
                return;
            }
        }

        drop(snapshot);

        // Stream new chunks as they arrive
        loop {
            match tokio::time::timeout(Duration::from_secs(120), rx.recv()).await {
                Ok(Ok(CollectionChange::Chunks)) => {
                    let snapshot = app.durable_streams.stream_db.snapshot().await;
                    let mut new_chunks: Vec<&ChunkRow> = snapshot
                        .chunks
                        .values()
                        .filter(|c| c.prompt_turn_id == prompt_turn_id && c.seq > last_seq)
                        .collect();
                    new_chunks.sort_by_key(|c| c.seq);

                    for chunk in &new_chunks {
                        last_seq = chunk.seq;
                        if let Ok(json) = serde_json::to_string(chunk) {
                            yield Ok(Event::default().data(json));
                        }
                        if is_terminal_chunk(chunk) {
                            return;
                        }
                    }
                }
                Ok(Ok(_)) => {
                    // Non-chunk change, check if turn completed
                    let snapshot = app.durable_streams.stream_db.snapshot().await;
                    if let Some(turn) = snapshot.prompt_turns.get(&prompt_turn_id) {
                        if matches!(
                            turn.state,
                            PromptTurnState::Completed
                                | PromptTurnState::Cancelled
                                | PromptTurnState::Broken
                        ) {
                            return;
                        }
                    }
                }
                Ok(Err(_)) => return, // channel closed
                Err(_) => return,     // timeout
            }
        }
    };

    Sse::new(stream).keep_alive(
        axum::response::sse::KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text(""),
    )
}

async fn get_registry(
    State(_app): State<Arc<AppState>>,
) -> Result<Json<crate::registry::Registry>, axum::http::StatusCode> {
    crate::registry::read_registry()
        .map(Json)
        .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)
}

fn is_terminal_chunk(chunk: &ChunkRow) -> bool {
    matches!(
        chunk.chunk_type,
        crate::state::ChunkType::Stop | crate::state::ChunkType::Error
    )
}
