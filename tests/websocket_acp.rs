//! End-to-end test: /acp WebSocket endpoint.
//!
//! Starts a real API server with /acp enabled, connects via WebSocket
//! as an ACP client using the SDK, sends a prompt, and verifies:
//! 1. The response comes back through the proxy chain
//! 2. State is persisted in the durable stream (StreamDb)

use std::sync::Arc;

use durable_acp_rs::acp_server::{self, AcpEndpointConfig};
use durable_acp_rs::api;
use durable_acp_rs::stream_server::StreamServer;
use durable_acp_rs::transport::WebSocketTransport;

mod common;
use common::TestApp;

async fn test_ds() -> StreamServer {
    common::test_ds().await
}

/// Start API server with /acp endpoint pointing at the testy binary.
async fn start_acp_server(ds: StreamServer) -> (String, Arc<TestApp>) {
    let app = Arc::new(TestApp::new(ds).await);

    // Find the testy binary (built alongside tests)
    let testy = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("testy");
    assert!(testy.exists(), "testy binary not found at {}", testy.display());

    let acp_config = AcpEndpointConfig {
        agent_command: vec![testy.to_string_lossy().to_string()],
        stream_server: app.stream_server.clone(),
        connection_id: app.connection_id.clone(),
    };

    let product_api = api::router(api::ApiState {
        stream_server: app.stream_server.clone(),
        connection_id: app.connection_id.clone(),
    });
    let acp_transport = acp_server::router(acp_config);
    let router = axum::Router::new().merge(product_api).merge(acp_transport);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });

    (format!("ws://127.0.0.1:{}", addr.port()), app)
}

// ---------------------------------------------------------------------------
// /acp WebSocket: connect as ACP client, send prompt, get response
// ---------------------------------------------------------------------------

#[tokio::test]
async fn acp_websocket_prompt_returns_response() {
    let ds = test_ds().await;
    let (ws_base, _app) = start_acp_server(ds).await;
    let ws_url = format!("{ws_base}/acp");

    let transport = sacp::DynConnectTo::new(WebSocketTransport { url: ws_url });

    let result = sacp::Client
        .builder()
        .name("ws-test-client")
        .on_receive_request(
            async |_req: agent_client_protocol::RequestPermissionRequest, responder, _cx| {
                responder.respond(agent_client_protocol::RequestPermissionResponse::new(
                    agent_client_protocol::RequestPermissionOutcome::Cancelled,
                ))
            },
            sacp::on_receive_request!(),
        )
        .connect_with(transport, async |cx| {
            cx.send_request(
                agent_client_protocol::InitializeRequest::new(
                    agent_client_protocol::ProtocolVersion::V1,
                ),
            )
            .block_task()
            .await?;

            cx.build_session_cwd()?
                .block_task()
                .run_until(async |mut session| {
                    session.send_prompt("hello via websocket")?;
                    let _response = session.read_to_string().await?;
                    Ok(())
                })
                .await
        })
        .await;

    assert!(result.is_ok(), "ACP WebSocket session failed: {:?}", result.err());
}

// ---------------------------------------------------------------------------
// /acp WebSocket: state persisted in durable stream after prompt
// ---------------------------------------------------------------------------

#[tokio::test]
async fn acp_websocket_persists_state_to_stream_db() {
    let ds = test_ds().await;
    let stream_db = ds.stream_db.clone();
    let (ws_base, _app) = start_acp_server(ds).await;
    let ws_url = format!("{ws_base}/acp");

    let transport = sacp::DynConnectTo::new(WebSocketTransport { url: ws_url });

    sacp::Client
        .builder()
        .name("ws-state-test")
        .on_receive_request(
            async |_req: agent_client_protocol::RequestPermissionRequest, responder, _cx| {
                responder.respond(agent_client_protocol::RequestPermissionResponse::new(
                    agent_client_protocol::RequestPermissionOutcome::Cancelled,
                ))
            },
            sacp::on_receive_request!(),
        )
        .connect_with(transport, async |cx| {
            cx.send_request(
                agent_client_protocol::InitializeRequest::new(
                    agent_client_protocol::ProtocolVersion::V1,
                ),
            )
            .block_task()
            .await?;

            cx.build_session_cwd()?
                .block_task()
                .run_until(async |mut session| {
                    session.send_prompt("state persistence test")?;
                    let _response = session.read_to_string().await?;
                    Ok(())
                })
                .await
        })
        .await
        .unwrap();

    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let snapshot = stream_db.snapshot().await;
    assert!(
        !snapshot.connections.is_empty(),
        "expected connection in StreamDb after WebSocket session"
    );
}

// ---------------------------------------------------------------------------
// /acp WebSocket: multiple sequential prompts in one session
// ---------------------------------------------------------------------------

#[tokio::test]
async fn acp_websocket_multiple_prompts_in_session() {
    let ds = test_ds().await;
    let stream_db = ds.stream_db.clone();
    let (ws_base, _app) = start_acp_server(ds).await;
    let ws_url = format!("{ws_base}/acp");

    let transport = sacp::DynConnectTo::new(WebSocketTransport { url: ws_url });

    sacp::Client
        .builder()
        .name("ws-multi-test")
        .on_receive_request(
            async |_req: agent_client_protocol::RequestPermissionRequest, responder, _cx| {
                responder.respond(agent_client_protocol::RequestPermissionResponse::new(
                    agent_client_protocol::RequestPermissionOutcome::Cancelled,
                ))
            },
            sacp::on_receive_request!(),
        )
        .connect_with(transport, async |cx| {
            cx.send_request(
                agent_client_protocol::InitializeRequest::new(
                    agent_client_protocol::ProtocolVersion::V1,
                ),
            )
            .block_task()
            .await?;

            cx.build_session_cwd()?
                .block_task()
                .run_until(async |mut session| {
                    session.send_prompt("first")?;
                    let _ = session.read_to_string().await?;

                    session.send_prompt("second")?;
                    let _ = session.read_to_string().await?;

                    session.send_prompt("third")?;
                    let _ = session.read_to_string().await?;

                    Ok(())
                })
                .await
        })
        .await
        .unwrap();

    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let snapshot = stream_db.snapshot().await;
    assert_eq!(
        snapshot.prompt_turns.len(),
        3,
        "expected 3 prompt turns, got {}",
        snapshot.prompt_turns.len()
    );
    assert!(
        snapshot.prompt_turns.values().all(|t| t.state == durable_acp_rs::state::PromptTurnState::Completed),
        "all turns should be completed"
    );
}
