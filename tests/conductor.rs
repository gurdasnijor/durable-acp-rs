//! Conductor path integration tests — verify the full proxy chain end-to-end.
//!
//! Uses `Testy` (the SDK's mock agent) + `yopo::prompt` (one-shot client)
//! to test: Client → DurableStateProxy → PeerMcpProxy → Agent
//! entirely in-process with no subprocess.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;

use agent_client_protocol_test::testy::{Testy, TestyCommand};
use sacp_conductor::{ConductorImpl, McpBridgeMode, ProxiesAndAgent};
use tokio::io::duplex;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

use durable_acp_rs::conductor_state::ConductorState;
use durable_acp_rs::durable_state_proxy::DurableStateProxy;
use durable_acp_rs::stream_server::StreamServer;
use durable_acp_rs::peer_mcp::PeerMcpProxy;
use durable_acp_rs::state::{ChunkType, ConnectionState, PromptTurnState};

async fn test_app() -> Arc<ConductorState> {
    let tmp = tempfile::tempdir().unwrap();
    let bind = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0);
    let ds = StreamServer::start_with_dir(
        bind,
        "durable-acp-state",
        tmp.path().to_path_buf(),
    )
    .await
    .unwrap();
    std::mem::forget(tmp);
    Arc::new(ConductorState::with_shared_streams(ds).await.unwrap())
}

/// Wire up: client ↔ conductor (DurableStateProxy + PeerMcpProxy + Testy)
/// Returns the response text from the agent.
async fn run_prompt(app: Arc<ConductorState>, prompt: &str) -> String {
    let (editor_write, conductor_read) = duplex(8192);
    let (conductor_write, editor_read) = duplex(8192);

    let conductor_app = app.clone();
    let conductor_handle = tokio::spawn(async move {
        ConductorImpl::new_agent(
            "test-conductor".to_string(),
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
// Core conductor path: prompt flows through proxy chain
// ---------------------------------------------------------------------------

#[tokio::test]
async fn conductor_greet_through_proxy_chain() {
    let app = test_app().await;
    let result = run_prompt(app, &TestyCommand::Greet.to_prompt()).await;
    assert_eq!(result, "Hello, world!");
}

#[tokio::test]
async fn conductor_echo_through_proxy_chain() {
    let app = test_app().await;
    let result = run_prompt(
        app,
        &TestyCommand::Echo {
            message: "test message".to_string(),
        }
        .to_prompt(),
    )
    .await;
    assert_eq!(result, "test message");
}

// ---------------------------------------------------------------------------
// DurableStateProxy: state persisted to StreamDB during prompt
// ---------------------------------------------------------------------------

#[tokio::test]
async fn proxy_persists_connection_state() {
    let app = test_app().await;

    // Before prompt: connection exists but not attached
    let snapshot = app.stream_server.stream_db.snapshot().await;
    assert_eq!(
        snapshot
            .connections
            .get(&app.logical_connection_id)
            .unwrap()
            .state,
        ConnectionState::Created
    );

    let _ = run_prompt(app.clone(), &TestyCommand::Greet.to_prompt()).await;

    // After prompt: connection should be attached with session_id and cwd
    let snapshot = app.stream_server.stream_db.snapshot().await;
    let conn = snapshot
        .connections
        .get(&app.logical_connection_id)
        .unwrap();
    assert_eq!(conn.state, ConnectionState::Attached);
    assert!(conn.latest_session_id.is_some());
    assert!(conn.cwd.is_some());
}

#[tokio::test]
async fn proxy_persists_prompt_turn_lifecycle() {
    let app = test_app().await;
    let _ = run_prompt(app.clone(), &TestyCommand::Greet.to_prompt()).await;

    let snapshot = app.stream_server.stream_db.snapshot().await;

    // Should have exactly one prompt turn
    assert_eq!(snapshot.prompt_turns.len(), 1);
    let turn = snapshot.prompt_turns.values().next().unwrap();
    assert_eq!(turn.logical_connection_id, app.logical_connection_id);
    assert_eq!(turn.state, PromptTurnState::Completed);
    assert!(turn.stop_reason.is_some());
    assert!(turn.completed_at.is_some());
}

#[tokio::test]
async fn proxy_persists_chunks() {
    let app = test_app().await;
    let _ = run_prompt(
        app.clone(),
        &TestyCommand::Echo {
            message: "chunk test".to_string(),
        }
        .to_prompt(),
    )
    .await;

    let snapshot = app.stream_server.stream_db.snapshot().await;

    // Should have at least a text chunk + stop chunk
    assert!(snapshot.chunks.len() >= 2, "expected >=2 chunks, got {}", snapshot.chunks.len());

    let mut chunks: Vec<_> = snapshot.chunks.values().collect();
    chunks.sort_by_key(|c| c.seq);

    // Should have a text chunk with our content
    let text_chunks: Vec<_> = chunks
        .iter()
        .filter(|c| c.chunk_type == ChunkType::Text)
        .collect();
    assert!(
        !text_chunks.is_empty(),
        "expected at least one text chunk"
    );

    // Should have a stop chunk
    let stop_chunks: Vec<_> = chunks
        .iter()
        .filter(|c| c.chunk_type == ChunkType::Stop)
        .collect();
    assert_eq!(stop_chunks.len(), 1, "expected exactly one stop chunk");
}

#[tokio::test]
async fn proxy_persists_pending_request() {
    let app = test_app().await;
    let _ = run_prompt(app.clone(), &TestyCommand::Greet.to_prompt()).await;

    let snapshot = app.stream_server.stream_db.snapshot().await;

    // Should have a pending_request for the prompt
    assert!(
        !snapshot.pending_requests.is_empty(),
        "expected at least one pending_request"
    );
}

// ---------------------------------------------------------------------------
// State survives across prompts (same ConductorState, same stream)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn multiple_prompts_accumulate_state() {
    let app = test_app().await;

    let _ = run_prompt(
        app.clone(),
        &TestyCommand::Echo {
            message: "first".to_string(),
        }
        .to_prompt(),
    )
    .await;
    let _ = run_prompt(
        app.clone(),
        &TestyCommand::Echo {
            message: "second".to_string(),
        }
        .to_prompt(),
    )
    .await;

    let snapshot = app.stream_server.stream_db.snapshot().await;

    // Two prompt turns, both completed
    assert_eq!(snapshot.prompt_turns.len(), 2);
    assert!(snapshot
        .prompt_turns
        .values()
        .all(|t| t.state == PromptTurnState::Completed));

    // Chunks from both prompts
    assert!(snapshot.chunks.len() >= 4, "expected >=4 chunks from 2 prompts");
}

// ---------------------------------------------------------------------------
// Durable stream: state readable via HTTP after conductor writes
// ---------------------------------------------------------------------------

#[tokio::test]
async fn state_stream_contains_conductor_output() {
    let app = test_app().await;
    let _ = run_prompt(app.clone(), &TestyCommand::Greet.to_prompt()).await;

    // Read the raw durable stream via HTTP
    let http = reqwest::Client::new();
    let resp = http.get(app.state_stream_url()).send().await.unwrap();
    assert!(resp.status().is_success());
    let body = resp.text().await.unwrap();

    // Should contain connection, prompt_turn, and chunk events
    assert!(body.contains("connection"), "stream should contain connection events");
    assert!(body.contains("prompt_turn"), "stream should contain prompt_turn events");
    assert!(body.contains("chunk"), "stream should contain chunk events");
}
