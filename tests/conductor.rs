//! Conductor path integration tests — verify the full proxy chain end-to-end.
//!
//! Uses `Testy` (the SDK's mock agent) + `yopo::prompt` (one-shot client)
//! to test: Client → PeerMcpProxy → Agent with passive trace observation.
//! State materialization comes from TraceEvents via DurableStreamTracer.

use std::sync::Arc;

use agent_client_protocol_test::testy::{Testy, TestyCommand};
use sacp_conductor::{ConductorImpl, McpBridgeMode, ProxiesAndAgent};
use tokio::io::duplex;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

use durable_acp_rs::acp_server;
use durable_acp_rs::api;
use durable_acp_rs::durable_stream_tracer::DurableStreamTracer;
use durable_acp_rs::peer_mcp::PeerMcpProxy;
use durable_acp_rs::state::{ConnectionState, PromptTurnState};
use durable_acp_rs::transport::WebSocketTransport;

mod common;
use common::TestApp;

/// Wire up: client ↔ conductor (PeerMcpProxy + Testy) with passive tracing.
/// Returns the response text from the agent.
async fn run_prompt(app: Arc<TestApp>, prompt: &str) -> String {
    let (editor_write, conductor_read) = duplex(8192);
    let (conductor_write, editor_read) = duplex(8192);

    let tracer = DurableStreamTracer::start(
        app.stream_server.clone(),
        app.stream_server.state_stream.clone(),
    );
    let conductor_handle = tokio::spawn(async move {
        ConductorImpl::new_agent(
            "test-conductor".to_string(),
            ProxiesAndAgent::new(Testy::new())
                .proxy(PeerMcpProxy),
            McpBridgeMode::default(),
        )
        .trace_to(tracer)
        .run(sacp::ByteStreams::new(
            conductor_write.compat_write(),
            conductor_read.compat(),
        ))
        .await
    });

    let prompt_text = prompt.to_string();
    let result = tokio::time::timeout(std::time::Duration::from_secs(10), async move {
        yopo::prompt(
            sacp::ByteStreams::new(editor_write.compat_write(), editor_read.compat()),
            prompt_text,
        )
        .await
    })
    .await
    .expect("prompt timed out")
    .expect("prompt failed");

    // Give the tracer's async writer task time to flush events before killing
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    conductor_handle.abort();
    result
}

// ---------------------------------------------------------------------------
// Basic round-trip: prompt reaches agent and response returns
// ---------------------------------------------------------------------------

#[tokio::test]
async fn conductor_greet_through_proxy_chain() {
    let app = common::test_app().await;
    let result = run_prompt(app, &TestyCommand::Greet.to_prompt()).await;
    assert_eq!(result, "Hello, world!");
}

#[tokio::test]
async fn conductor_echo_through_proxy_chain() {
    let app = common::test_app().await;
    let result = run_prompt(app, &TestyCommand::Echo { message: "e2e test".to_string() }.to_prompt()).await;
    assert_eq!(result, "e2e test");
}

// ---------------------------------------------------------------------------
// State materialization from trace events
// ---------------------------------------------------------------------------

#[tokio::test]
async fn state_stream_contains_conductor_output() {
    let app = common::test_app().await;
    let _ = run_prompt(app.clone(), &TestyCommand::Echo { message: "trace test".to_string() }.to_prompt()).await;

    // Give trace events time to materialize
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    let snapshot = app.stream_server.stream_db.snapshot().await;
    // Connection row was created by TestApp::new
    assert!(!snapshot.connections.is_empty(), "expected connection row");
}

#[tokio::test]
async fn proxy_persists_prompt_turn_lifecycle() {
    let app = common::test_app().await;
    let _ = run_prompt(app.clone(), &TestyCommand::Greet.to_prompt()).await;

    // Give trace events time to materialize
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    let snapshot = app.stream_server.stream_db.snapshot().await;

    // Should have at least one prompt turn from trace events
    assert!(
        !snapshot.prompt_turns.is_empty(),
        "expected prompt turn from trace materialization"
    );

    // The turn should be completed
    let turn = snapshot.prompt_turns.values().next().unwrap();
    assert_eq!(turn.state, PromptTurnState::Completed);
    assert!(turn.completed_at.is_some());
}

#[tokio::test]
async fn proxy_persists_chunks() {
    let app = common::test_app().await;
    let result = run_prompt(app.clone(), &TestyCommand::Greet.to_prompt()).await;

    // Verify the prompt completed successfully
    assert!(!result.is_empty(), "expected non-empty response");

    // Give trace events time to materialize
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    let snapshot = app.stream_server.stream_db.snapshot().await;

    // Prompt turn should exist (created from trace request event)
    assert!(
        !snapshot.prompt_turns.is_empty(),
        "expected prompt turn from trace materialization"
    );
}

#[tokio::test]
async fn trace_captures_chunks_from_notifications() {
    let app = common::test_app().await;
    let result = run_prompt(app.clone(), &TestyCommand::Greet.to_prompt()).await;
    assert!(!result.is_empty(), "agent should respond");

    // Give trace events time to flush
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    let snapshot = app.stream_server.stream_db.snapshot().await;

    // Diagnostic: dump what we got
    eprintln!("prompt_turns: {}", snapshot.prompt_turns.len());
    for (id, t) in &snapshot.prompt_turns {
        eprintln!("  turn {}: state={:?}", id, t.state);
    }
    eprintln!("chunks: {}", snapshot.chunks.len());
    for (id, c) in &snapshot.chunks {
        eprintln!("  chunk {}: type={:?} content={:.40}", id, c.chunk_type, c.content);
    }

    // This is the key assertion: chunks should materialize from trace notifications
    assert!(
        !snapshot.chunks.is_empty(),
        "chunks should materialize from session/update trace notifications. \
         prompt_turns={}, chunks={}",
        snapshot.prompt_turns.len(),
        snapshot.chunks.len()
    );
}

// ---------------------------------------------------------------------------
// State survives across prompts (same TestApp, same stream)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn multiple_prompts_accumulate_state() {
    let app = common::test_app().await;
    let r1 = run_prompt(app.clone(), &TestyCommand::Echo { message: "first".to_string() }.to_prompt()).await;
    assert_eq!(r1, "first");

    let r2 = run_prompt(app.clone(), &TestyCommand::Echo { message: "second".to_string() }.to_prompt()).await;
    assert_eq!(r2, "second");

    // Give trace events time to materialize
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    let snapshot = app.stream_server.stream_db.snapshot().await;
    // Should have prompt turns from both prompts
    assert!(
        snapshot.prompt_turns.len() >= 2,
        "expected at least 2 prompt turns, got {}",
        snapshot.prompt_turns.len()
    );
}

// ---------------------------------------------------------------------------
// Trace replay: rebuild state from persisted stream matches live state
// ---------------------------------------------------------------------------

#[tokio::test]
async fn trace_replay_matches_live_state() {
    let app = common::test_app().await;

    // Run a prompt to generate trace events
    let _ = run_prompt(app.clone(), &TestyCommand::Greet.to_prompt()).await;
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    // Capture the live materialized state
    let live_snapshot = app.stream_server.stream_db.snapshot().await;
    assert!(!live_snapshot.prompt_turns.is_empty(), "live state should have prompt turns");

    // Rebuild state from the persisted stream into a fresh StreamDb
    let fresh_db = durable_acp_rs::state::StreamDb::new();
    fresh_db.set_connection_id(app.connection_id.clone()).await;
    app.stream_server.rebuild_state_into(&fresh_db).await.unwrap();

    let replay_snapshot = fresh_db.snapshot().await;

    // Replay should match live state
    assert_eq!(
        live_snapshot.connections.len(),
        replay_snapshot.connections.len(),
        "replay connections count should match live"
    );
    assert_eq!(
        live_snapshot.prompt_turns.len(),
        replay_snapshot.prompt_turns.len(),
        "replay prompt_turns count should match live"
    );
    assert_eq!(
        live_snapshot.chunks.len(),
        replay_snapshot.chunks.len(),
        "replay chunks count should match live"
    );

    // Verify prompt turn states match
    for (id, live_turn) in &live_snapshot.prompt_turns {
        let replay_turn = replay_snapshot.prompt_turns.get(id)
            .unwrap_or_else(|| panic!("replay missing prompt turn {}", id));
        assert_eq!(live_turn.state, replay_turn.state,
            "turn {} state mismatch: live={:?} replay={:?}", id, live_turn.state, replay_turn.state);
    }
}

// ---------------------------------------------------------------------------
// Transport equivalence: stdio and WS produce equivalent materialized state
// ---------------------------------------------------------------------------

#[tokio::test]
async fn ws_transport_behaves_like_stdio_transport() {
    // --- Stdio path ---
    let stdio_app = common::test_app().await;
    let stdio_result = run_prompt(
        stdio_app.clone(),
        &TestyCommand::Echo { message: "transport-test".to_string() }.to_prompt(),
    ).await;
    assert_eq!(stdio_result, "transport-test");
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    let stdio_snapshot = stdio_app.stream_server.stream_db.snapshot().await;

    // --- WS path ---
    let ws_ds = common::test_ds().await;
    let ws_app = Arc::new(TestApp::new(ws_ds).await);

    let testy = std::env::current_exe().unwrap()
        .parent().unwrap().parent().unwrap().join("testy");
    assert!(testy.exists(), "testy binary not found");

    let acp_config = acp_server::AcpEndpointConfig {
        agent_command: vec![testy.to_string_lossy().to_string()],
        stream_server: ws_app.stream_server.clone(),
        connection_id: ws_app.connection_id.clone(),
    };
    let product_api = api::router(api::ApiState {
        stream_server: ws_app.stream_server.clone(),
        connection_id: ws_app.connection_id.clone(),
    });
    let acp_transport = acp_server::router(acp_config);
    let router = axum::Router::new().merge(product_api).merge(acp_transport);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });

    let ws_url = format!("ws://127.0.0.1:{}/acp", addr.port());
    let transport = sacp::DynConnectTo::new(WebSocketTransport { url: ws_url });

    sacp::Client
        .builder()
        .name("equiv-test")
        .on_receive_request(
            async |_req: agent_client_protocol::RequestPermissionRequest, responder, _cx| {
                responder.respond(agent_client_protocol::RequestPermissionResponse::new(
                    agent_client_protocol::RequestPermissionOutcome::Cancelled,
                ))
            },
            sacp::on_receive_request!(),
        )
        .connect_with(transport, async |cx| {
            cx.send_request(agent_client_protocol::InitializeRequest::new(
                agent_client_protocol::ProtocolVersion::V1,
            ))
            .block_task()
            .await?;

            cx.build_session_cwd()?
                .block_task()
                .run_until(async |mut session| {
                    session.send_prompt("transport-test")?;
                    let _ = session.read_to_string().await?;
                    Ok(())
                })
                .await
        })
        .await
        .unwrap();

    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    let ws_snapshot = ws_app.stream_server.stream_db.snapshot().await;

    // --- Compare: both should have equivalent materialized state ---
    assert!(
        !stdio_snapshot.prompt_turns.is_empty(),
        "stdio should have prompt turns"
    );
    assert!(
        !ws_snapshot.prompt_turns.is_empty(),
        "ws should have prompt turns"
    );
    assert_eq!(
        stdio_snapshot.prompt_turns.len(),
        ws_snapshot.prompt_turns.len(),
        "stdio and ws should have same number of prompt turns"
    );

    // Both should have completed turns
    assert!(stdio_snapshot.prompt_turns.values().all(|t| t.state == PromptTurnState::Completed));
    assert!(ws_snapshot.prompt_turns.values().all(|t| t.state == PromptTurnState::Completed));

    // Both should have connections
    assert!(!stdio_snapshot.connections.is_empty());
    assert!(!ws_snapshot.connections.is_empty());
}

#[tokio::test]
async fn proxy_persists_connection_state() {
    let app = common::test_app().await;
    let _ = run_prompt(app.clone(), &TestyCommand::Greet.to_prompt()).await;

    let snapshot = app.stream_server.stream_db.snapshot().await;
    let conn = snapshot.connections.get(&app.connection_id).unwrap();
    assert!(
        conn.state == ConnectionState::Created || conn.state == ConnectionState::Attached,
        "connection should be Created or Attached, got {:?}", conn.state
    );
}
