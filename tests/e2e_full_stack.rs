//! End-to-end: ACP client → conductor proxy chain → durable session observer.
//!
//! Verifies the complete system:
//! 1. ACP client sends prompts through the conductor's proxy chain
//! 2. DurableStateProxy persists all state to the durable stream
//! 3. A remote DurableSession (SSE subscriber) materializes the same state
//!
//! This proves the two-primitive architecture works:
//! - In-band: ACP client → conductor (prompt, cancel, permissions)
//! - Out-of-band: DurableSession → durable stream (reactive state)

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;

use agent_client_protocol_test::testy::{Testy, TestyCommand};
use sacp_conductor::{ConductorImpl, McpBridgeMode, ProxiesAndAgent};
use tokio::io::duplex;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

use durable_acp_rs::app::AppState;
use durable_acp_rs::durable_session::DurableSession;
use durable_acp_rs::durable_state_proxy::DurableStateProxy;
use durable_acp_rs::durable_streams::EmbeddedDurableStreams;
use durable_acp_rs::peer_mcp::PeerMcpProxy;
use durable_acp_rs::state::{ChunkType, ConnectionState, PromptTurnState};

async fn test_app() -> Arc<AppState> {
    let tmp = tempfile::tempdir().unwrap();
    let bind = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0);
    let ds = EmbeddedDurableStreams::start_with_dir(
        bind,
        "durable-acp-state",
        tmp.path().to_path_buf(),
    )
    .await
    .unwrap();
    std::mem::forget(tmp);
    Arc::new(AppState::with_shared_streams(ds).await.unwrap())
}

async fn run_prompt(app: Arc<AppState>, prompt: &str) -> String {
    let (editor_write, conductor_read) = duplex(8192);
    let (conductor_write, editor_read) = duplex(8192);

    let conductor_app = app.clone();
    let conductor_handle = tokio::spawn(async move {
        ConductorImpl::new_agent(
            "e2e-conductor".to_string(),
            ProxiesAndAgent::new(Testy::new())
                .proxy(DurableStateProxy {
                    app: conductor_app,
                })
                .proxy(PeerMcpProxy),
            McpBridgeMode::default(),
        )
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

    conductor_handle.abort();
    result
}

// ---------------------------------------------------------------------------
// E2E: remote DurableSession observes in-band ACP prompt results
// ---------------------------------------------------------------------------

#[tokio::test]
async fn e2e_remote_session_observes_prompt_flow() {
    let app = test_app().await;

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

    // 2. Connect a remote DurableSession via SSE (out-of-band observer)
    let mut session = DurableSession::new(app.state_stream_url());
    session.preload().await.unwrap();

    // 3. Verify the remote session sees the same state as the conductor's local StreamDb
    let local = app.durable_streams.stream_db.snapshot().await;
    let remote = session.stream_db().snapshot().await;

    // Connections
    assert_eq!(remote.connections.len(), local.connections.len());
    let remote_conn = remote
        .connections
        .get(&app.logical_connection_id)
        .expect("remote should see the connection");
    assert_eq!(remote_conn.state, ConnectionState::Attached);
    assert!(remote_conn.cwd.is_some());
    assert!(remote_conn.latest_session_id.is_some());

    // Prompt turns
    assert_eq!(remote.prompt_turns.len(), local.prompt_turns.len());
    assert_eq!(remote.prompt_turns.len(), 1);
    let remote_turn = remote.prompt_turns.values().next().unwrap();
    assert_eq!(remote_turn.state, PromptTurnState::Completed);
    assert!(remote_turn.stop_reason.is_some());
    assert!(remote_turn.completed_at.is_some());

    // Chunks — at least text + stop
    assert_eq!(remote.chunks.len(), local.chunks.len());
    assert!(remote.chunks.len() >= 2);
    let text_chunks: Vec<_> = remote
        .chunks
        .values()
        .filter(|c| c.chunk_type == ChunkType::Text)
        .collect();
    assert!(!text_chunks.is_empty());

    // Pending requests
    assert_eq!(
        remote.pending_requests.len(),
        local.pending_requests.len()
    );

    session.disconnect();
}

#[tokio::test]
async fn e2e_remote_session_observes_multiple_prompts() {
    let app = test_app().await;

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
    let mut session = DurableSession::new(app.state_stream_url());
    session.preload().await.unwrap();

    let remote = session.stream_db().snapshot().await;

    // Two prompt turns, both completed
    assert_eq!(remote.prompt_turns.len(), 2);
    assert!(remote
        .prompt_turns
        .values()
        .all(|t| t.state == PromptTurnState::Completed));

    // Chunks from both prompts
    assert!(
        remote.chunks.len() >= 4,
        "expected >=4 chunks from 2 prompts, got {}",
        remote.chunks.len()
    );

    session.disconnect();
}

#[tokio::test]
async fn e2e_change_subscriptions_fire_for_prompt_events() {
    let app = test_app().await;

    // Subscribe to changes BEFORE the prompt
    let mut rx = app.durable_streams.stream_db.subscribe_changes();

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

    // Should have seen connection, prompt_turn, chunk, and pending_request changes
    use durable_acp_rs::state::CollectionChange;
    assert!(
        changes.iter().any(|c| matches!(c, CollectionChange::Connections)),
        "should have connection change"
    );
    assert!(
        changes.iter().any(|c| matches!(c, CollectionChange::PromptTurns)),
        "should have prompt_turn change"
    );
    assert!(
        changes.iter().any(|c| matches!(c, CollectionChange::Chunks)),
        "should have chunk change"
    );
    assert!(
        changes
            .iter()
            .any(|c| matches!(c, CollectionChange::PendingRequests)),
        "should have pending_request change"
    );
}
