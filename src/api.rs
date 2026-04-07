use std::sync::Arc;

use anyhow::Result;
use axum::extract::{Path, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use agent_client_protocol::{CancelNotification, ContentBlock, PromptRequest};
use sacp::Agent;
use serde::{Deserialize, Serialize};

use crate::app::AppState;
use crate::state::{ConnectionRow, PromptTurnRow, PromptTurnState};

pub fn router(app: Arc<AppState>) -> Router {
    Router::new()
        .route("/api/v1/connections", get(list_connections))
        .route("/api/v1/connections/{id}/queue", get(get_queue))
        .route("/api/v1/connections/{id}/queue/pause", post(pause_queue))
        .route("/api/v1/connections/{id}/queue/resume", post(resume_queue))
        .route("/api/v1/connections/{id}/prompt", post(submit_prompt))
        .route("/api/v1/connections/{id}/cancel", post(cancel_turn))
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
}

async fn submit_prompt(
    Path(_id): Path<String>,
    State(app): State<Arc<AppState>>,
    Json(body): Json<PromptBody>,
) -> Result<Json<PromptAccepted>, axum::http::StatusCode> {
    let Some(cx) = app.proxy_connection.lock().await.clone() else {
        return Err(axum::http::StatusCode::SERVICE_UNAVAILABLE);
    };
    let request = PromptRequest::new(body.session_id, vec![ContentBlock::from(body.text)]);
    tokio::spawn(async move {
        let _ = cx.send_request_to(Agent, request).block_task().await;
    });
    Ok(Json(PromptAccepted { queued: false }))
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
