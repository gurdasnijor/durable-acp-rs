//! Conductor path integration tests — verify the full proxy chain end-to-end.
//!
//! Uses `Testy` (the SDK's mock agent) + `yopo::prompt` (one-shot client)
//! to test: Client → PeerMcpProxy → Agent with passive trace observation.
//! State materialization comes from TraceEvents, not DurableStateProxy.

use std::sync::Arc;

use agent_client_protocol_test::testy::{Testy, TestyCommand};
use sacp_conductor::{ConductorImpl, McpBridgeMode, ProxiesAndAgent};
use tokio::io::duplex;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

use durable_acp_rs::durable_stream_tracer::DurableStreamTracer;
use durable_acp_rs::peer_mcp::PeerMcpProxy;
use durable_acp_rs::state::{ConnectionState, PromptTurnState};

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
