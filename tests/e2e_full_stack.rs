//! End-to-end: ACP client → conductor proxy chain → durable session observer.
//!
//! Verifies the complete system:
//! 1. ACP client sends prompts through the conductor's proxy chain
//! 2. DurableStreamTracer passively records all state to the durable stream
//! 3. A remote StreamSubscriber (SSE subscriber) materializes the same state
//!
//! This proves the two-primitive architecture works:
//! - In-band: ACP client → conductor (prompt, cancel, permissions)
//! - Out-of-band: StreamSubscriber → durable stream (reactive state)

use std::sync::Arc;

use agent_client_protocol_test::testy::{Testy, TestyCommand};
use sacp_conductor::{ConductorImpl, McpBridgeMode, ProxiesAndAgent};
use tokio::io::duplex;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

use durable_acp_rs::durable_stream_tracer::DurableStreamTracer;
use durable_acp_rs::stream_subscriber::StreamSubscriber;
use durable_acp_rs::peer_mcp::PeerMcpProxy;
use durable_acp_rs::state::PromptTurnState;

mod common;
use common::TestApp;

async fn run_prompt(app: Arc<TestApp>, prompt: &str) -> String {
    let (editor_write, conductor_read) = duplex(8192);
    let (conductor_write, editor_read) = duplex(8192);

    let tracer = DurableStreamTracer::start(
        app.stream_server.clone(),
        app.stream_server.state_stream.clone(),
    );
    let conductor_handle = tokio::spawn(async move {
        ConductorImpl::new_agent(
            "e2e-conductor".to_string(),
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
    .expect("test timed out")
    .expect("prompt failed");

    // Give the tracer time to flush
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    conductor_handle.abort();
    result
}

// ---------------------------------------------------------------------------
// E2E: remote StreamSubscriber observes in-band ACP prompt results
// ---------------------------------------------------------------------------

#[tokio::test]
async fn e2e_remote_session_observes_prompt_flow() {
    let app = common::test_app().await;

    // 1. Send a prompt through the full ACP proxy chain
    let result = run_prompt(
        app.clone(),
        &TestyCommand::Echo {
            message: "e2e test".to_string(),
        }
        .to_prompt(),
    )
    .await;
    assert_eq!(result, "e2e test");

    // 2. Connect a remote StreamSubscriber via SSE (out-of-band observer)
    let mut session = StreamSubscriber::new(app.state_stream_url());
    session.preload().await.unwrap();

    // 3. Verify the remote session sees state
    let remote = session.stream_db().snapshot().await;

    // Connections
    assert!(
        !remote.connections.is_empty(),
        "remote should see the connection"
    );

    // Prompt turns
    assert!(
        !remote.prompt_turns.is_empty(),
        "remote should see prompt turns"
    );
    let remote_turn = remote.prompt_turns.values().next().unwrap();
    assert_eq!(remote_turn.state, PromptTurnState::Completed);
    assert!(remote_turn.completed_at.is_some());

    session.disconnect();
}

#[tokio::test]
async fn e2e_remote_session_observes_multiple_prompts() {
    let app = common::test_app().await;

    // Send two prompts
    let r1 = run_prompt(
        app.clone(),
        &TestyCommand::Echo {
            message: "first".to_string(),
        }
        .to_prompt(),
    )
    .await;
    assert_eq!(r1, "first");

    let r2 = run_prompt(
        app.clone(),
        &TestyCommand::Echo {
            message: "second".to_string(),
        }
        .to_prompt(),
    )
    .await;
    assert_eq!(r2, "second");

    // Remote observer connects after both prompts
    let mut session = StreamSubscriber::new(app.state_stream_url());
    session.preload().await.unwrap();

    let remote = session.stream_db().snapshot().await;

    // Two prompt turns, both completed
    assert!(
        remote.prompt_turns.len() >= 2,
        "expected >=2 prompt turns, got {}",
        remote.prompt_turns.len()
    );
    assert!(remote
        .prompt_turns
        .values()
        .all(|t| t.state == PromptTurnState::Completed));

    session.disconnect();
}

#[tokio::test]
async fn e2e_change_subscriptions_fire_for_prompt_events() {
    let app = common::test_app().await;

    // Subscribe to changes BEFORE the prompt
    let mut rx = app.stream_server.stream_db.subscribe_changes();

    // Drain the initial connection insert
    let _ = tokio::time::timeout(std::time::Duration::from_millis(100), rx.recv()).await;

    // Send a prompt
    let _ = run_prompt(
        app.clone(),
        &TestyCommand::Greet.to_prompt(),
    )
    .await;

    // Collect all changes that fired
    let mut changes = Vec::new();
    while let Ok(change) =
        tokio::time::timeout(std::time::Duration::from_millis(200), rx.recv()).await
    {
        if let Ok(c) = change {
            changes.push(c);
        }
    }

    // Should have seen connection and prompt_turn changes from trace events
    use durable_acp_rs::state::CollectionChange;
    assert!(
        changes.iter().any(|c| matches!(c, CollectionChange::Connections)),
        "should have connection change"
    );
    assert!(
        changes.iter().any(|c| matches!(c, CollectionChange::PromptTurns)),
        "should have prompt_turn change"
    );
}
