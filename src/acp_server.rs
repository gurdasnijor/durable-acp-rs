//! ACP hosting surface — exposes the conductor over WebSocket.
//!
//! This is the ACP data plane: protocol-compliant ACP transport hosting.
//! Separate from the product HTTP API in `api.rs`.

use std::sync::Arc;

use axum::extract::ws::{WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::Router;

use crate::stream_server::StreamServer;

/// Configuration for the hosted ACP endpoint.
#[derive(Clone)]
pub struct AcpEndpointConfig {
    pub agent_command: Vec<String>,
    pub stream_server: StreamServer,
    pub connection_id: String,
}

/// Create an axum Router that serves the `/acp` WebSocket endpoint.
pub fn router(config: AcpEndpointConfig) -> Router {
    Router::new()
        .route("/acp", get(ws_acp_handler))
        .with_state(Arc::new(config))
}

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
