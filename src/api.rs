//! REST API — filesystem access + peer discovery + ACP WebSocket endpoint.
//!
//! State observation (connections, chunks, prompt turns, terminals, permissions)
//! is handled by the durable stream at :port/streams/durable-acp-state.
//! Clients subscribe via SSE using @durable-acp/state StreamDB.
//!
//! This API only exposes:
//! - /acp (WebSocket) — ACP client transport, spawns conductor per connection
//! - /api/v1/*/files, /api/v1/*/fs/tree — filesystem access (not in the stream)
//! - /api/v1/registry — peer discovery
//! - /api/v1/*/terminals/{tid} — terminal state mutation

use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::extract::ws::{WebSocket, WebSocketUpgrade};
use axum::response::IntoResponse;
use axum::routing::{delete, get};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use tower_http::cors::{Any, CorsLayer};

use crate::state::TerminalState;
use crate::stream_server::StreamServer;

/// Focused API state — only what the HTTP routes need.
#[derive(Clone)]
pub struct ApiState {
    pub stream_server: StreamServer,
    pub connection_id: String,
}

/// Config for the WebSocket ACP endpoint (separate from API state).
#[derive(Clone)]
pub struct AcpEndpointConfig {
    pub agent_command: Vec<String>,
    pub stream_server: StreamServer,
    pub connection_id: String,
}

pub fn router(api_state: ApiState, acp_config: Option<AcpEndpointConfig>) -> Router {
    let api = Router::new()
        // Filesystem access (not in the durable stream)
        .route("/api/v1/connections/{id}/files", get(get_file))
        .route("/api/v1/connections/{id}/fs/tree", get(get_tree))
        // Agent templates (from agents.toml)
        .route("/api/v1/agent-templates", get(get_agent_templates))
        // Peer discovery
        .route("/api/v1/registry", get(get_registry))
        // Terminal state mutation
        .route("/api/v1/connections/{id}/terminals/{tid}", delete(kill_terminal))
        .with_state(api_state);

    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    if let Some(config) = acp_config {
        let acp: Router = Router::new()
            .route("/acp", get(ws_acp_handler))
            .with_state(Arc::new(config));
        Router::new().merge(api).merge(acp).layer(cors)
    } else {
        api.layer(cors)
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

    use sacp_conductor::{ConductorImpl, McpBridgeMode, ProxiesAndAgent};
    use crate::durable_stream_tracer::DurableStreamTracer;
    use crate::peer_mcp::PeerMcpProxy;

    // Set connection ID on StreamDb so trace event materialization works
    config.stream_server.stream_db.set_connection_id(config.connection_id.clone()).await;

    let tracer = DurableStreamTracer::start(
        config.stream_server.clone(),
        config.stream_server.state_stream.clone(),
    );
    let conductor = ConductorImpl::new_agent(
        "durable-acp".to_string(),
        ProxiesAndAgent::new(agent)
            .proxy(PeerMcpProxy),
        McpBridgeMode::default(),
    )
    .trace_to(tracer);
    let transport = crate::transport::AxumWsTransport { socket };

    if let Err(e) = conductor.run(transport).await {
        tracing::warn!("ACP WebSocket session ended: {}", e);
    }
}

// ---------------------------------------------------------------------------
// Filesystem access (not in the durable stream)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct FileQuery {
    path: Option<String>,
}

fn resolve_path(
    api: &ApiState,
    snapshot: &crate::state::Collections,
    id: &str,
    rel: &str,
) -> Result<PathBuf, axum::http::StatusCode> {
    let conn = snapshot.connections.get(id)
        .or_else(|| snapshot.connections.values().find(|c| c.logical_connection_id == api.connection_id))
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
    State(api): State<ApiState>,
) -> Result<String, axum::http::StatusCode> {
    let rel = query.path.as_deref().unwrap_or(".");
    let snapshot = api.stream_server.stream_db.snapshot().await;
    let path = resolve_path(&api, &snapshot, &id, rel)?;
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
    State(api): State<ApiState>,
) -> Result<Json<Vec<TreeEntry>>, axum::http::StatusCode> {
    let rel = query.path.as_deref().unwrap_or(".");
    let snapshot = api.stream_server.stream_db.snapshot().await;
    let path = resolve_path(&api, &snapshot, &id, rel)?;
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
    State(_api): State<ApiState>,
) -> Result<Json<crate::registry::Registry>, axum::http::StatusCode> {
    crate::registry::read_registry().map(Json).map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)
}

async fn kill_terminal(
    Path((_id, tid)): Path<(String, String)>,
    State(api): State<ApiState>,
) -> Result<Json<serde_json::Value>, axum::http::StatusCode> {
    let mut snapshot = api.stream_server.stream_db.snapshot().await;
    let row = snapshot.terminals.get_mut(&tid)
        .ok_or(axum::http::StatusCode::NOT_FOUND)?;
    row.state = TerminalState::Released;
    row.updated_at = crate::state::now_ms();
    let updated = row.clone();
    drop(snapshot);
    let envelope = crate::state::StateEnvelope {
        entity_type: "terminal".to_string(),
        key: tid.clone(),
        headers: crate::state::StateHeaders { operation: "update".to_string() },
        value: Some(&updated),
    };
    api.stream_server
        .append_json(&api.stream_server.state_stream, &envelope)
        .await
        .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(serde_json::json!({ "killed": tid })))
}

// ---------------------------------------------------------------------------
// Agent templates (from agents.toml)
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize)]
struct AgentTemplateConfig {
    name: String,
    #[serde(default)]
    port: u16,
    agent: Option<String>,
    command: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
struct AgentsToml {
    #[serde(default)]
    agent: Vec<AgentTemplateConfig>,
}

async fn get_agent_templates(
    State(_api): State<ApiState>,
) -> Result<Json<Vec<AgentTemplateConfig>>, axum::http::StatusCode> {
    let path = std::path::Path::new("agents.toml");
    if !path.exists() {
        return Ok(Json(vec![]));
    }
    let content = std::fs::read_to_string(path)
        .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;
    let config: AgentsToml = toml::from_str(&content)
        .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(config.agent))
}
