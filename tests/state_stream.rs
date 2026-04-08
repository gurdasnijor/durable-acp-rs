//! Integration tests: state.rs ↔ durable streams service.
//!
//! Verifies that state events round-trip through the durable streams server:
//! write → persist to disk → read back → StreamDb materialize → correct state.
//!
//! Tests all 7 entity types and the full lifecycle of each.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use durable_acp_rs::stream_subscriber::StreamSubscriber;
use durable_acp_rs::stream_server::StreamServer;
use durable_acp_rs::state::*;

mod common;
use common::TestApp;

async fn test_ds() -> StreamServer {
    common::test_ds().await
}

async fn test_app() -> TestApp {
    let ds = test_ds().await;
    TestApp::new(ds).await
}

// ---------------------------------------------------------------------------
// All 7 entity types: insert → materialize → snapshot correct
// ---------------------------------------------------------------------------

#[tokio::test]
async fn connection_insert_materializes() {
    let app = test_app().await;
    let snapshot = app.stream_server.stream_db.snapshot().await;
    // TestApp::new auto-inserts a connection
    assert_eq!(snapshot.connections.len(), 1);
    let conn = snapshot.connections.get(&app.connection_id).unwrap();
    assert_eq!(conn.state, ConnectionState::Created);
    assert!(conn.latest_session_id.is_none());
}

#[tokio::test]
async fn prompt_turn_insert_materializes() {
    let app = test_app().await;
    let row = PromptTurnRow {
        prompt_turn_id: "pt-1".to_string(),
        logical_connection_id: app.connection_id.clone(),
        session_id: "s-1".to_string(),
        request_id: "req-1".to_string(),
        text: Some("hello agent".to_string()),
        state: PromptTurnState::Active,
            position: None,
        stop_reason: None,
        started_at: 100,
        completed_at: None,
    };
    app.write_state_event("prompt_turn", "insert", "pt-1", Some(&row))
        .await
        .unwrap();

    let snapshot = app.stream_server.stream_db.snapshot().await;
    assert_eq!(snapshot.prompt_turns.len(), 1);
    let turn = &snapshot.prompt_turns["pt-1"];
    assert_eq!(turn.text, Some("hello agent".to_string()));
    assert_eq!(turn.state, PromptTurnState::Active);
}

#[tokio::test]
async fn chunk_insert_materializes() {
    let app = test_app().await;
    app.record_chunk("pt-1", ChunkType::Text, "first chunk".to_string())
        .await
        .unwrap();
    app.record_chunk("pt-1", ChunkType::ToolCall, r#"{"tool":"read"}"#.to_string())
        .await
        .unwrap();
    app.record_chunk("pt-1", ChunkType::Stop, r#"{"stopReason":"end_turn"}"#.to_string())
        .await
        .unwrap();

    let snapshot = app.stream_server.stream_db.snapshot().await;
    assert_eq!(snapshot.chunks.len(), 3);

    let mut chunks: Vec<_> = snapshot.chunks.values().collect();
    chunks.sort_by_key(|c| c.seq);
    assert_eq!(chunks[0].chunk_type, ChunkType::Text);
    assert_eq!(chunks[0].content, "first chunk");
    assert_eq!(chunks[0].seq, 0);
    assert_eq!(chunks[1].chunk_type, ChunkType::ToolCall);
    assert_eq!(chunks[1].seq, 1);
    assert_eq!(chunks[2].chunk_type, ChunkType::Stop);
    assert_eq!(chunks[2].seq, 2);
}

#[tokio::test]
async fn permission_insert_materializes() {
    let app = test_app().await;
    let perm = PermissionRow {
        request_id: "perm-1".to_string(),
        jsonrpc_id: serde_json::json!(42),
        logical_connection_id: app.connection_id.clone(),
        session_id: "s-1".to_string(),
        prompt_turn_id: "pt-1".to_string(),
        title: Some("Read /etc/passwd".to_string()),
        tool_call_id: Some("tc-1".to_string()),
        options: Some(vec![
            PermissionOptionRow {
                option_id: "opt-allow".to_string(),
                name: "Allow".to_string(),
                kind: "allow".to_string(),
            },
            PermissionOptionRow {
                option_id: "opt-deny".to_string(),
                name: "Deny".to_string(),
                kind: "deny".to_string(),
            },
        ]),
        state: PendingRequestState::Pending,
        outcome: None,
        created_at: 100,
        resolved_at: None,
    };
    app.write_state_event("permission", "insert", "perm-1", Some(&perm))
        .await
        .unwrap();

    let snapshot = app.stream_server.stream_db.snapshot().await;
    assert_eq!(snapshot.permissions.len(), 1);
    let p = &snapshot.permissions["perm-1"];
    assert_eq!(p.title, Some("Read /etc/passwd".to_string()));
    assert_eq!(p.state, PendingRequestState::Pending);
    assert_eq!(p.options.as_ref().unwrap().len(), 2);
}

#[tokio::test]
async fn terminal_insert_materializes() {
    let app = test_app().await;
    let term = TerminalRow {
        terminal_id: "term-1".to_string(),
        logical_connection_id: app.connection_id.clone(),
        session_id: "s-1".to_string(),
        prompt_turn_id: Some("pt-1".to_string()),
        state: TerminalState::Open,
        command: Some("bash".to_string()),
        exit_code: None,
        signal: None,
        created_at: 100,
        updated_at: 100,
    };
    app.write_state_event("terminal", "insert", "term-1", Some(&term))
        .await
        .unwrap();

    let snapshot = app.stream_server.stream_db.snapshot().await;
    assert_eq!(snapshot.terminals.len(), 1);
    let t = &snapshot.terminals["term-1"];
    assert_eq!(t.state, TerminalState::Open);
    assert_eq!(t.command, Some("bash".to_string()));
}

// ---------------------------------------------------------------------------
// Update operations: state transitions tracked
// ---------------------------------------------------------------------------

#[tokio::test]
async fn connection_update_tracks_state_transition() {
    let app = test_app().await;

    app.update_connection(|row| {
        row.state = ConnectionState::Attached;
        row.latest_session_id = Some("session-abc".to_string());
        row.cwd = Some("/home/user/project".to_string());
    })
    .await
    .unwrap();

    let snapshot = app.stream_server.stream_db.snapshot().await;
    let conn = snapshot.connections.get(&app.connection_id).unwrap();
    assert_eq!(conn.state, ConnectionState::Attached);
    assert_eq!(conn.latest_session_id, Some("session-abc".to_string()));
    assert_eq!(conn.cwd, Some("/home/user/project".to_string()));
}

#[tokio::test]
async fn prompt_turn_lifecycle_queued_active_completed() {
    let app = test_app().await;

    // Insert as queued
    let row = PromptTurnRow {
        prompt_turn_id: "lc-1".to_string(),
        logical_connection_id: app.connection_id.clone(),
        session_id: "s-1".to_string(),
        request_id: "req-lc".to_string(),
        text: Some("lifecycle test".to_string()),
        state: PromptTurnState::Active,
            position: None,
        stop_reason: None,
        started_at: 100,
        completed_at: None,
    };
    app.write_state_event("prompt_turn", "insert", "lc-1", Some(&row))
        .await
        .unwrap();

    // Update to active
    let mut active = row.clone();
    active.state = PromptTurnState::Active;
    app.write_state_event("prompt_turn", "update", "lc-1", Some(&active))
        .await
        .unwrap();

    let snapshot = app.stream_server.stream_db.snapshot().await;
    assert_eq!(snapshot.prompt_turns["lc-1"].state, PromptTurnState::Active);

    // Complete
    app.finish_prompt_turn("lc-1", "end_turn".to_string(), PromptTurnState::Completed)
        .await
        .unwrap();

    let snapshot = app.stream_server.stream_db.snapshot().await;
    let turn = &snapshot.prompt_turns["lc-1"];
    assert_eq!(turn.state, PromptTurnState::Completed);
    assert_eq!(turn.stop_reason, Some("end_turn".to_string()));
    assert!(turn.completed_at.is_some());
}

#[tokio::test]
async fn permission_lifecycle_pending_to_resolved() {
    let app = test_app().await;

    let perm = PermissionRow {
        request_id: "plc-1".to_string(),
        jsonrpc_id: serde_json::json!(1),
        logical_connection_id: app.connection_id.clone(),
        session_id: "s-1".to_string(),
        prompt_turn_id: "pt-1".to_string(),
        title: Some("Test permission".to_string()),
        tool_call_id: None,
        options: None,
        state: PendingRequestState::Pending,
        outcome: None,
        created_at: 100,
        resolved_at: None,
    };
    app.write_state_event("permission", "insert", "plc-1", Some(&perm))
        .await
        .unwrap();

    // Resolve it
    let mut resolved = perm.clone();
    resolved.state = PendingRequestState::Resolved;
    resolved.outcome = Some("opt-allow".to_string());
    resolved.resolved_at = Some(200);
    app.write_state_event("permission", "update", "plc-1", Some(&resolved))
        .await
        .unwrap();

    let snapshot = app.stream_server.stream_db.snapshot().await;
    let p = &snapshot.permissions["plc-1"];
    assert_eq!(p.state, PendingRequestState::Resolved);
    assert_eq!(p.outcome, Some("opt-allow".to_string()));
    assert_eq!(p.resolved_at, Some(200));
}

// ---------------------------------------------------------------------------
// Delete operations
// ---------------------------------------------------------------------------

#[tokio::test]
async fn delete_removes_from_collection() {
    let app = test_app().await;
    app.record_chunk("pt-1", ChunkType::Text, "to delete".to_string())
        .await
        .unwrap();

    let snapshot = app.stream_server.stream_db.snapshot().await;
    assert_eq!(snapshot.chunks.len(), 1);
    let chunk_id = snapshot.chunks.keys().next().unwrap().clone();

    // Delete it
    app.write_state_event::<serde_json::Value>("chunk", "delete", &chunk_id, None)
        .await
        .unwrap();

    let snapshot = app.stream_server.stream_db.snapshot().await;
    assert_eq!(snapshot.chunks.len(), 0);
}

// ---------------------------------------------------------------------------
// Persistence: state survives restart, replays into new StreamDb
// ---------------------------------------------------------------------------

#[tokio::test]
async fn state_replays_from_disk_into_new_stream_db() {
    let tmp = tempfile::tempdir().unwrap();
    let bind = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0);

    // Instance 1: write diverse state
    {
        let ds = StreamServer::start_with_dir(
            bind,
            "durable-acp-state",
            tmp.path().to_path_buf(),
        )
        .await
        .unwrap();
        let app = TestApp::new(ds).await;

        app.record_chunk("pt-1", ChunkType::Text, "survived restart".to_string())
            .await
            .unwrap();
        app.update_connection(|row| {
            row.state = ConnectionState::Attached;
            row.cwd = Some("/project".to_string());
        })
        .await
        .unwrap();
    }

    // Instance 2: same dir, new StreamDb — should replay everything
    let bind2 = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0);
    let ds2 = StreamServer::start_with_dir(
        bind2,
        "durable-acp-state",
        tmp.path().to_path_buf(),
    )
    .await
    .unwrap();

    let snapshot = ds2.stream_db.snapshot().await;
    assert_eq!(snapshot.connections.len(), 1);
    let conn = snapshot.connections.values().next().unwrap();
    assert_eq!(conn.state, ConnectionState::Attached);
    assert_eq!(conn.cwd, Some("/project".to_string()));
    assert_eq!(snapshot.chunks.len(), 1);
    assert_eq!(
        snapshot.chunks.values().next().unwrap().content,
        "survived restart"
    );
}

// ---------------------------------------------------------------------------
// StreamSubscriber: remote StreamDb materializes same state as local
// ---------------------------------------------------------------------------

#[tokio::test]
async fn remote_session_matches_local_state() {
    let app = test_app().await;

    // Write varied state locally
    app.record_chunk("pt-1", ChunkType::Text, "local text".to_string())
        .await
        .unwrap();
    app.update_connection(|row| {
        row.state = ConnectionState::Attached;
    })
    .await
    .unwrap();

    let perm = PermissionRow {
        request_id: "remote-perm".to_string(),
        jsonrpc_id: serde_json::json!(5),
        logical_connection_id: app.connection_id.clone(),
        session_id: "s-1".to_string(),
        prompt_turn_id: "pt-1".to_string(),
        title: Some("Remote test".to_string()),
        tool_call_id: None,
        options: None,
        state: PendingRequestState::Pending,
        outcome: None,
        created_at: 100,
        resolved_at: None,
    };
    app.write_state_event("permission", "insert", "remote-perm", Some(&perm))
        .await
        .unwrap();

    // Remote StreamSubscriber connects via SSE
    let mut session = StreamSubscriber::new(app.state_stream_url());
    session.preload().await.unwrap();

    let local = app.stream_server.stream_db.snapshot().await;
    let remote = session.stream_db().snapshot().await;

    // Same number of entities
    assert_eq!(remote.connections.len(), local.connections.len());
    assert_eq!(remote.chunks.len(), local.chunks.len());
    assert_eq!(remote.permissions.len(), local.permissions.len());

    // Same content
    let remote_conn = remote.connections.get(&app.connection_id).unwrap();
    assert_eq!(remote_conn.state, ConnectionState::Attached);

    let remote_chunk = remote.chunks.values().next().unwrap();
    assert_eq!(remote_chunk.content, "local text");

    let remote_perm = remote.permissions.get("remote-perm").unwrap();
    assert_eq!(remote_perm.title, Some("Remote test".to_string()));

    session.disconnect();
}

// ---------------------------------------------------------------------------
// Change subscriptions fire for each entity type
// ---------------------------------------------------------------------------

#[tokio::test]
async fn subscribe_fires_for_all_entity_types() {
    let app = test_app().await;
    let mut rx = app.stream_server.stream_db.subscribe_changes();

    // Drain connection insert from init
    let _ = tokio::time::timeout(std::time::Duration::from_millis(50), rx.recv()).await;

    // PromptTurn
    let turn = PromptTurnRow {
        prompt_turn_id: "sub-pt".to_string(),
        logical_connection_id: app.connection_id.clone(),
        session_id: "s".to_string(),
        request_id: "r".to_string(),
        text: None,
        state: PromptTurnState::Active,
            position: None,
        stop_reason: None,
        started_at: 1,
        completed_at: None,
    };
    app.write_state_event("prompt_turn", "insert", "sub-pt", Some(&turn))
        .await
        .unwrap();
    let change = rx.recv().await.unwrap();
    assert!(matches!(change, CollectionChange::PromptTurns));

    // Chunk
    app.record_chunk("sub-pt", ChunkType::Text, "x".to_string())
        .await
        .unwrap();
    let change = rx.recv().await.unwrap();
    assert!(matches!(change, CollectionChange::Chunks));

    // Permission
    let perm = PermissionRow {
        request_id: "sub-perm".to_string(),
        jsonrpc_id: serde_json::json!(1),
        logical_connection_id: app.connection_id.clone(),
        session_id: "s".to_string(),
        prompt_turn_id: "sub-pt".to_string(),
        title: None,
        tool_call_id: None,
        options: None,
        state: PendingRequestState::Pending,
        outcome: None,
        created_at: 1,
        resolved_at: None,
    };
    app.write_state_event("permission", "insert", "sub-perm", Some(&perm))
        .await
        .unwrap();
    let change = rx.recv().await.unwrap();
    assert!(matches!(change, CollectionChange::Permissions));

    // Terminal
    let term = TerminalRow {
        terminal_id: "sub-term".to_string(),
        logical_connection_id: app.connection_id.clone(),
        session_id: "s".to_string(),
        prompt_turn_id: None,
        state: TerminalState::Open,
        command: None,
        exit_code: None,
        signal: None,
        created_at: 1,
        updated_at: 1,
    };
    app.write_state_event("terminal", "insert", "sub-term", Some(&term))
        .await
        .unwrap();
    let change = rx.recv().await.unwrap();
    assert!(matches!(change, CollectionChange::Terminals));
}
