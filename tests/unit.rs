//! Unit tests for exported types, registry, transport config, StreamDb edges,
//! trace event materialization, and serde round-trips.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use durable_acp_rs::registry;
use durable_acp_rs::state::*;
use durable_acp_rs::stream_server::StreamServer;
use durable_acp_rs::transport::TransportConfig;
use sacp_conductor::trace::TraceEvent;

// ---------------------------------------------------------------------------
// Registry: register / unregister / read
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore] // Registry tests share a global file — run serially with --ignored
async fn registry_register_and_read() {
    // Clean slate
    let _ = registry::unregister("test-reg-1");
    let _ = registry::unregister("test-reg-2");

    registry::register(registry::AgentEntry {
        name: "test-reg-1".to_string(),
        api_url: "http://127.0.0.1:4001".to_string(),
        logical_connection_id: "conn-1".to_string(),
        registered_at: 100,
    })
    .unwrap();

    registry::register(registry::AgentEntry {
        name: "test-reg-2".to_string(),
        api_url: "http://127.0.0.1:4002".to_string(),
        logical_connection_id: "conn-2".to_string(),
        registered_at: 200,
    })
    .unwrap();

    let reg = registry::read_registry().unwrap();
    assert!(reg.agents.iter().any(|a| a.name == "test-reg-1"));
    assert!(reg.agents.iter().any(|a| a.name == "test-reg-2"));

    // Cleanup
    registry::unregister("test-reg-1").unwrap();
    registry::unregister("test-reg-2").unwrap();
}

#[tokio::test]
#[ignore]
async fn registry_unregister_removes_entry() {
    let _ = registry::unregister("test-unreg");

    registry::register(registry::AgentEntry {
        name: "test-unreg".to_string(),
        api_url: "http://127.0.0.1:5000".to_string(),
        logical_connection_id: "conn-x".to_string(),
        registered_at: 100,
    })
    .unwrap();

    registry::unregister("test-unreg").unwrap();

    let reg = registry::read_registry().unwrap();
    assert!(!reg.agents.iter().any(|a| a.name == "test-unreg"));
}

#[tokio::test]
#[ignore]
async fn registry_unregister_nonexistent_is_ok() {
    // Should not error
    let result = registry::unregister("definitely-does-not-exist-12345");
    assert!(result.is_ok());
}

#[tokio::test(flavor = "multi_thread")]
#[ignore] // Registry tests share a global file — run with --ignored to avoid races
async fn registry_duplicate_register_overwrites() {
    let _ = registry::unregister("test-dup");

    registry::register(registry::AgentEntry {
        name: "test-dup".to_string(),
        api_url: "http://old:1000".to_string(),
        logical_connection_id: "old".to_string(),
        registered_at: 1,
    })
    .unwrap();

    registry::register(registry::AgentEntry {
        name: "test-dup".to_string(),
        api_url: "http://new:2000".to_string(),
        logical_connection_id: "new".to_string(),
        registered_at: 2,
    })
    .unwrap();

    let reg = registry::read_registry().unwrap();
    let entries: Vec<_> = reg.agents.iter().filter(|a| a.name == "test-dup").collect();
    assert_eq!(entries.len(), 1, "duplicate should be overwritten, not appended");
    assert_eq!(entries[0].api_url, "http://new:2000");

    registry::unregister("test-dup").unwrap();
}

// ---------------------------------------------------------------------------
// TransportConfig: TOML deserialization
// ---------------------------------------------------------------------------

#[test]
fn transport_config_stdio_from_toml() {
    let config: TransportConfig = toml::from_str(r#"type = "stdio""#).unwrap();
    assert!(matches!(config, TransportConfig::Stdio));
}

#[test]
fn transport_config_ws_from_toml() {
    let config: TransportConfig =
        toml::from_str(r#"type = "ws"
url = "ws://gpu-server:4438/acp""#)
            .unwrap();
    match config {
        TransportConfig::Ws { url } => assert_eq!(url, "ws://gpu-server:4438/acp"),
        _ => panic!("expected Ws variant"),
    }
}

#[test]
fn transport_config_tcp_from_toml() {
    let config: TransportConfig =
        toml::from_str(r#"type = "tcp"
host = "10.0.0.5"
port = 9000"#)
            .unwrap();
    match config {
        TransportConfig::Tcp { host, port } => {
            assert_eq!(host, "10.0.0.5");
            assert_eq!(port, 9000);
        }
        _ => panic!("expected Tcp variant"),
    }
}

#[test]
fn transport_config_invalid_type_errors() {
    let result: Result<TransportConfig, _> = toml::from_str(r#"type = "smoke_signal""#);
    assert!(result.is_err());
}

// ---------------------------------------------------------------------------
// State serde round-trips — every row type survives JSON serialize/deserialize
// ---------------------------------------------------------------------------

#[test]
fn connection_row_round_trip() {
    let row = ConnectionRow {
        logical_connection_id: "conn-1".into(),
        state: ConnectionState::Attached,
        latest_session_id: Some("sess-1".into()),
        cwd: Some("/project".into()),
        last_error: None,
            queue_paused: None,
        created_at: 1000,
        updated_at: 2000,
    };
    let json = serde_json::to_string(&row).unwrap();
    let back: ConnectionRow = serde_json::from_str(&json).unwrap();
    assert_eq!(back.logical_connection_id, "conn-1");
    assert_eq!(back.state, ConnectionState::Attached);
    assert_eq!(back.cwd, Some("/project".into()));
    // Verify camelCase
    assert!(json.contains("logicalConnectionId"));
    assert!(json.contains("latestSessionId"));
}

#[test]
fn prompt_turn_row_round_trip() {
    let row = PromptTurnRow {
        prompt_turn_id: "pt-1".into(),
        logical_connection_id: "conn-1".into(),
        session_id: "s-1".into(),
        request_id: "req-1".into(),
        text: Some("hello".into()),
        state: PromptTurnState::Completed,
            position: None,
        stop_reason: Some("end_turn".into()),
        started_at: 100,
        completed_at: Some(200),
    };
    let json = serde_json::to_string(&row).unwrap();
    let back: PromptTurnRow = serde_json::from_str(&json).unwrap();
    assert_eq!(back.state, PromptTurnState::Completed);
    assert_eq!(back.stop_reason, Some("end_turn".into()));
    // Verify camelCase + snake_case enum
    assert!(json.contains("promptTurnId"));
    assert!(json.contains("\"completed\""));
}

#[test]
fn chunk_row_round_trip() {
    let row = ChunkRow {
        chunk_id: "c-1".into(),
        prompt_turn_id: "pt-1".into(),
        logical_connection_id: "conn-1".into(),
        chunk_type: ChunkType::ToolCall,
        content: r#"{"tool":"read_file"}"#.into(),
        seq: 5,
        created_at: 100,
    };
    let json = serde_json::to_string(&row).unwrap();
    let back: ChunkRow = serde_json::from_str(&json).unwrap();
    assert_eq!(back.chunk_type, ChunkType::ToolCall);
    assert_eq!(back.seq, 5);
    // chunk_type serializes as "type" via serde rename
    assert!(json.contains(r#""type":"tool_call""#));
}

#[test]
fn permission_row_round_trip() {
    let row = PermissionRow {
        request_id: "perm-1".into(),
        jsonrpc_id: serde_json::json!(42),
        logical_connection_id: "conn-1".into(),
        session_id: "s-1".into(),
        prompt_turn_id: "pt-1".into(),
        title: Some("Read file".into()),
        tool_call_id: Some("tc-1".into()),
        options: Some(vec![PermissionOptionRow {
            option_id: "opt-1".into(),
            name: "Allow".into(),
            kind: "allow".into(),
        }]),
        state: PendingRequestState::Pending,
        outcome: None,
        created_at: 100,
        resolved_at: None,
    };
    let json = serde_json::to_string(&row).unwrap();
    let back: PermissionRow = serde_json::from_str(&json).unwrap();
    assert_eq!(back.options.as_ref().unwrap().len(), 1);
    assert_eq!(back.options.unwrap()[0].name, "Allow");
}

#[test]
fn terminal_row_round_trip() {
    let row = TerminalRow {
        terminal_id: "t-1".into(),
        logical_connection_id: "conn-1".into(),
        session_id: "s-1".into(),
        prompt_turn_id: Some("pt-1".into()),
        state: TerminalState::Exited,
        command: Some("bash".into()),
        exit_code: Some(0),
        signal: None,
        created_at: 100,
        updated_at: 200,
    };
    let json = serde_json::to_string(&row).unwrap();
    let back: TerminalRow = serde_json::from_str(&json).unwrap();
    assert_eq!(back.state, TerminalState::Exited);
    assert_eq!(back.exit_code, Some(0));
}

#[test]
fn all_connection_states_serialize() {
    for state in [
        ConnectionState::Created,
        ConnectionState::Attached,
        ConnectionState::Broken,
        ConnectionState::Closed,
    ] {
        let json = serde_json::to_string(&state).unwrap();
        let back: ConnectionState = serde_json::from_str(&json).unwrap();
        assert_eq!(back, state);
    }
}

#[test]
fn all_prompt_turn_states_serialize() {
    for state in [
        PromptTurnState::Active,
        PromptTurnState::Active,
        PromptTurnState::Completed,
        PromptTurnState::Cancelled,
        PromptTurnState::Cancelled,
        PromptTurnState::Broken,
        PromptTurnState::Broken,
    ] {
        let json = serde_json::to_string(&state).unwrap();
        let back: PromptTurnState = serde_json::from_str(&json).unwrap();
        assert_eq!(back, state);
    }
}

#[test]
fn all_chunk_types_serialize() {
    for ct in [
        ChunkType::Text,
        ChunkType::ToolCall,
        ChunkType::Thinking,
        ChunkType::ToolResult,
        ChunkType::Error,
        ChunkType::Stop,
    ] {
        let json = serde_json::to_string(&ct).unwrap();
        let back: ChunkType = serde_json::from_str(&json).unwrap();
        assert_eq!(back, ct);
    }
}

// ---------------------------------------------------------------------------
// StreamDb edge cases
// ---------------------------------------------------------------------------

#[tokio::test]
async fn stream_db_unknown_entity_type_errors() {
    let db = StreamDb::new();
    let event = serde_json::json!({
        "type": "alien_entity",
        "key": "x",
        "headers": { "operation": "insert" },
        "value": { "foo": "bar" }
    });
    let result = db.apply_json_message(&serde_json::to_vec(&event).unwrap()).await;
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("unsupported entity type"));
}

#[tokio::test]
async fn stream_db_delete_nonexistent_is_noop() {
    let db = StreamDb::new();
    let event = serde_json::json!({
        "type": "connection",
        "key": "does-not-exist",
        "headers": { "operation": "delete" }
    });
    // Should not error — delete on nonexistent is a no-op
    db.apply_json_message(&serde_json::to_vec(&event).unwrap())
        .await
        .unwrap();
    let snapshot = db.snapshot().await;
    assert_eq!(snapshot.connections.len(), 0);
}

#[tokio::test]
async fn stream_db_update_creates_if_missing() {
    let db = StreamDb::new();
    let row = ConnectionRow {
        logical_connection_id: "upsert-1".into(),
        state: ConnectionState::Attached,
        latest_session_id: None,
        cwd: None,
        last_error: None,
            queue_paused: None,
        created_at: 1,
        updated_at: 1,
    };
    let event = StateEnvelope {
        entity_type: "connection".to_string(),
        key: "upsert-1".to_string(),
        headers: StateHeaders {
            operation: "update".to_string(),
        },
        value: Some(row),
    };
    db.apply_json_message(&serde_json::to_vec(&event).unwrap())
        .await
        .unwrap();
    let snapshot = db.snapshot().await;
    // Update on missing key should upsert
    assert_eq!(snapshot.connections.len(), 1);
    assert_eq!(snapshot.connections["upsert-1"].state, ConnectionState::Attached);
}

#[tokio::test]
async fn stream_db_missing_headers_errors() {
    let db = StreamDb::new();
    let bad = serde_json::json!({ "key": "x", "value": {} });
    let result = db.apply_json_message(&serde_json::to_vec(&bad).unwrap()).await;
    assert!(result.is_err());
}

// ---------------------------------------------------------------------------
// TestApp lifecycle semantics
// ---------------------------------------------------------------------------

mod common;

async fn test_app() -> common::TestApp {
    let ds = common::test_ds().await;
    common::TestApp::new(ds).await
}

#[tokio::test]
async fn finish_prompt_turn_marks_completed() {
    let app = test_app().await;

    // Write a turn to StreamDb
    let turn = PromptTurnRow {
        prompt_turn_id: "pt-active".into(),
        logical_connection_id: app.connection_id.clone(),
        session_id: "s".into(),
        request_id: "r".into(),
        text: None,
        state: PromptTurnState::Active,
            position: None,
        stop_reason: None,
        started_at: 1,
        completed_at: None,
    };
    app.write_state_event("prompt_turn", "insert", "pt-active", Some(&turn))
        .await
        .unwrap();

    app.finish_prompt_turn("pt-active", "end_turn".to_string(), PromptTurnState::Completed)
        .await
        .unwrap();

    let snapshot = app.stream_server.stream_db.snapshot().await;
    assert_eq!(snapshot.prompt_turns["pt-active"].state, PromptTurnState::Completed);
}

#[tokio::test]
async fn chunk_sequence_numbers_increment() {
    let app = test_app().await;

    app.record_chunk("pt-1", ChunkType::Text, "a".into()).await.unwrap();
    app.record_chunk("pt-1", ChunkType::Text, "b".into()).await.unwrap();
    app.record_chunk("pt-1", ChunkType::Text, "c".into()).await.unwrap();

    let snapshot = app.stream_server.stream_db.snapshot().await;
    let mut chunks: Vec<_> = snapshot.chunks.values().collect();
    chunks.sort_by_key(|c| c.seq);

    assert_eq!(chunks.len(), 3);
    assert_eq!(chunks[0].seq, 0);
    assert_eq!(chunks[1].seq, 1);
    assert_eq!(chunks[2].seq, 2);
    assert_eq!(chunks[0].content, "a");
    assert_eq!(chunks[2].content, "c");
}

#[tokio::test]
async fn chunk_sequences_independent_per_turn() {
    let app = test_app().await;

    app.record_chunk("pt-1", ChunkType::Text, "x".into()).await.unwrap();
    app.record_chunk("pt-1", ChunkType::Text, "y".into()).await.unwrap();
    app.record_chunk("pt-2", ChunkType::Text, "z".into()).await.unwrap();

    let snapshot = app.stream_server.stream_db.snapshot().await;
    let pt1: Vec<_> = snapshot.chunks.values().filter(|c| c.prompt_turn_id == "pt-1").collect();
    let pt2: Vec<_> = snapshot.chunks.values().filter(|c| c.prompt_turn_id == "pt-2").collect();

    assert_eq!(pt1.len(), 2);
    assert_eq!(pt2.len(), 1);
    // pt-2 starts at seq 0, independent of pt-1
    assert_eq!(pt2[0].seq, 0);
}

// ---------------------------------------------------------------------------
// TraceEvent materialization (smart read side)
// ---------------------------------------------------------------------------

fn make_request(method: &str, id: &str, params: serde_json::Value) -> TraceEvent {
    serde_json::from_value(serde_json::json!({
        "type": "request",
        "ts": 0.0,
        "protocol": "acp",
        "from": "Client",
        "to": "Agent",
        "id": id,
        "method": method,
        "params": params,
    })).unwrap()
}

fn make_response(id: &str, is_error: bool, payload: serde_json::Value) -> TraceEvent {
    serde_json::from_value(serde_json::json!({
        "type": "response",
        "ts": 0.0,
        "from": "Agent",
        "to": "Client",
        "id": id,
        "is_error": is_error,
        "payload": payload,
    })).unwrap()
}

fn make_notification(method: &str, session: &str, params: serde_json::Value) -> TraceEvent {
    serde_json::from_value(serde_json::json!({
        "type": "notification",
        "ts": 0.0,
        "protocol": "acp",
        "from": "Agent",
        "to": "Client",
        "method": method,
        "session": session,
        "params": params,
    })).unwrap()
}

async fn trace_db() -> StreamDb {
    let db = StreamDb::new();
    db.set_connection_id("conn-1".into()).await;
    // Pre-populate a connection row so session/new can update it
    let conn_event = serde_json::json!({
        "type": "connection",
        "key": "conn-1",
        "headers": { "operation": "insert" },
        "value": {
            "logicalConnectionId": "conn-1",
            "state": "created",
            "queuePaused": false,
            "createdAt": 1,
            "updatedAt": 1
        }
    });
    db.apply_json_message(&serde_json::to_vec(&conn_event).unwrap())
        .await
        .unwrap();
    db
}

#[tokio::test]
async fn trace_session_new_updates_connection() {
    let db = trace_db().await;

    // Request sets cwd
    let req = make_request("session/new", "req-1", serde_json::json!({
        "cwd": "/home/user/project"
    }));
    db.apply_trace_event(&req).await.unwrap();

    let snap = db.snapshot().await;
    let conn = &snap.connections["conn-1"];
    assert_eq!(conn.cwd.as_deref(), Some("/home/user/project"));

    // Response sets sessionId and marks Attached
    let resp = make_response("req-1", false, serde_json::json!({
        "sessionId": "sess-abc"
    }));
    db.apply_trace_event(&resp).await.unwrap();

    let snap = db.snapshot().await;
    let conn = &snap.connections["conn-1"];
    assert_eq!(conn.state, ConnectionState::Attached);
    assert_eq!(conn.latest_session_id.as_deref(), Some("sess-abc"));
}

#[tokio::test]
async fn trace_prompt_creates_turn() {
    let db = trace_db().await;

    let event = make_request("session/prompt", "req-prompt-1", serde_json::json!({
        "sessionId": "sess-1",
        "prompt": [{ "type": "text", "text": "Hello agent" }]
    }));
    db.apply_trace_event(&event).await.unwrap();

    let snap = db.snapshot().await;
    assert_eq!(snap.prompt_turns.len(), 1);
    let turn = snap.prompt_turns.values().next().unwrap();
    assert_eq!(turn.state, PromptTurnState::Active);
    assert_eq!(turn.session_id, "sess-1");
    assert_eq!(turn.text.as_deref(), Some("Hello agent"));
    assert_eq!(turn.logical_connection_id, "conn-1");
}

#[tokio::test]
async fn trace_response_completes_turn() {
    let db = trace_db().await;

    // Send prompt
    db.apply_trace_event(&make_request("session/prompt", "req-p1", serde_json::json!({
        "sessionId": "sess-1",
        "prompt": [{ "type": "text", "text": "test" }]
    }))).await.unwrap();

    // Send response
    db.apply_trace_event(&make_response("req-p1", false, serde_json::json!({
        "stopReason": "endTurn"
    }))).await.unwrap();

    let snap = db.snapshot().await;
    let turn = snap.prompt_turns.values().next().unwrap();
    assert_eq!(turn.state, PromptTurnState::Completed);
    assert_eq!(turn.stop_reason.as_deref(), Some("endTurn"));
    assert!(turn.completed_at.is_some());
}

#[tokio::test]
async fn trace_error_response_marks_broken() {
    let db = trace_db().await;

    db.apply_trace_event(&make_request("session/prompt", "req-err", serde_json::json!({
        "sessionId": "sess-1",
        "prompt": [{ "type": "text", "text": "test" }]
    }))).await.unwrap();

    db.apply_trace_event(&make_response("req-err", true, serde_json::json!({
        "message": "agent crashed"
    }))).await.unwrap();

    let snap = db.snapshot().await;
    let turn = snap.prompt_turns.values().next().unwrap();
    assert_eq!(turn.state, PromptTurnState::Broken);
}

#[tokio::test]
async fn trace_notification_creates_chunks() {
    let db = trace_db().await;

    // Start a prompt turn
    db.apply_trace_event(&make_request("session/prompt", "req-c1", serde_json::json!({
        "sessionId": "sess-1",
        "prompt": [{ "type": "text", "text": "hi" }]
    }))).await.unwrap();

    // Agent sends text chunks
    db.apply_trace_event(&make_notification("session/update", "sess-1", serde_json::json!({
        "sessionId": "sess-1",
        "update": {
            "type": "agentMessageChunk",
            "content": { "type": "text", "text": "Hello " }
        }
    }))).await.unwrap();

    db.apply_trace_event(&make_notification("session/update", "sess-1", serde_json::json!({
        "sessionId": "sess-1",
        "update": {
            "type": "agentMessageChunk",
            "content": { "type": "text", "text": "world!" }
        }
    }))).await.unwrap();

    let snap = db.snapshot().await;
    assert_eq!(snap.chunks.len(), 2);
    let mut chunks: Vec<_> = snap.chunks.values().collect();
    chunks.sort_by_key(|c| c.seq);
    assert_eq!(chunks[0].content, "Hello ");
    assert_eq!(chunks[0].seq, 0);
    assert_eq!(chunks[0].chunk_type, ChunkType::Text);
    assert_eq!(chunks[1].content, "world!");
    assert_eq!(chunks[1].seq, 1);
}

#[tokio::test]
async fn trace_sequential_prompts_produce_distinct_turns() {
    let db = trace_db().await;

    // First prompt
    db.apply_trace_event(&make_request("session/prompt", "req-1", serde_json::json!({
        "sessionId": "sess-1",
        "prompt": [{ "type": "text", "text": "first" }]
    }))).await.unwrap();
    db.apply_trace_event(&make_response("req-1", false, serde_json::json!({
        "stopReason": "endTurn"
    }))).await.unwrap();

    // Second prompt
    db.apply_trace_event(&make_request("session/prompt", "req-2", serde_json::json!({
        "sessionId": "sess-1",
        "prompt": [{ "type": "text", "text": "second" }]
    }))).await.unwrap();
    db.apply_trace_event(&make_response("req-2", false, serde_json::json!({
        "stopReason": "endTurn"
    }))).await.unwrap();

    let snap = db.snapshot().await;
    assert_eq!(snap.prompt_turns.len(), 2);
    let mut turns: Vec<_> = snap.prompt_turns.values().collect();
    turns.sort_by_key(|t| t.started_at);
    assert_eq!(turns[0].text.as_deref(), Some("first"));
    assert_eq!(turns[0].state, PromptTurnState::Completed);
    assert_eq!(turns[1].text.as_deref(), Some("second"));
    assert_eq!(turns[1].state, PromptTurnState::Completed);
    // Different turn IDs
    assert_ne!(turns[0].prompt_turn_id, turns[1].prompt_turn_id);
}

#[tokio::test]
async fn trace_thinking_chunk_materializes() {
    let db = trace_db().await;

    db.apply_trace_event(&make_request("session/prompt", "req-t", serde_json::json!({
        "sessionId": "sess-1",
        "prompt": [{ "type": "text", "text": "think" }]
    }))).await.unwrap();

    db.apply_trace_event(&make_notification("session/update", "sess-1", serde_json::json!({
        "sessionId": "sess-1",
        "update": {
            "type": "agentThoughtChunk",
            "content": { "type": "text", "text": "hmm let me think..." }
        }
    }))).await.unwrap();

    let snap = db.snapshot().await;
    assert_eq!(snap.chunks.len(), 1);
    let chunk = snap.chunks.values().next().unwrap();
    assert_eq!(chunk.chunk_type, ChunkType::Thinking);
    assert_eq!(chunk.content, "hmm let me think...");
}

// ---------------------------------------------------------------------------
// DurableStreamTracer → StreamServer → StreamDb end-to-end
// ---------------------------------------------------------------------------

#[tokio::test]
async fn tracer_write_event_materializes_in_stream_db() {
    use sacp_conductor::trace::WriteEvent;
    use durable_acp_rs::durable_stream_tracer::DurableStreamTracer;

    // Set up a StreamServer with a fresh temp dir
    let tmp = tempfile::tempdir().unwrap();
    let bind = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0);
    let ss = StreamServer::start_with_dir(bind, "test-state", tmp.path().to_path_buf())
        .await
        .unwrap();
    std::mem::forget(tmp);

    // Set connection ID for trace materialization
    ss.stream_db.set_connection_id("conn-tracer".into()).await;

    // Seed a connection row so session/new can update it
    let conn_event = serde_json::json!({
        "type": "connection",
        "key": "conn-tracer",
        "headers": { "operation": "insert" },
        "value": {
            "logicalConnectionId": "conn-tracer",
            "state": "created",
            "queuePaused": false,
            "createdAt": 1,
            "updatedAt": 1
        }
    });
    ss.stream_db
        .apply_json_message(&serde_json::to_vec(&conn_event).unwrap())
        .await
        .unwrap();

    // Create the tracer pointed at this StreamServer
    let mut tracer = DurableStreamTracer::start(ss.clone(), "test-state".to_string());

    // Write a prompt request TraceEvent through the tracer
    let prompt_event: TraceEvent = serde_json::from_value(serde_json::json!({
        "type": "request",
        "ts": 1.0,
        "protocol": "acp",
        "from": "Client",
        "to": "Agent",
        "id": "req-tracer-1",
        "method": "session/prompt",
        "params": {
            "sessionId": "sess-tracer",
            "prompt": [{ "type": "text", "text": "hello via tracer" }]
        }
    })).unwrap();
    tracer.write_event(&prompt_event).unwrap();

    // Give the async writer task time to flush
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    // Verify the prompt turn was materialized in StreamDb
    let snap = ss.stream_db.snapshot().await;
    assert!(
        !snap.prompt_turns.is_empty(),
        "expected prompt turn materialized from tracer event, got none. \
         chunks={}, connections={}, prompt_turns={}",
        snap.chunks.len(), snap.connections.len(), snap.prompt_turns.len()
    );
    let turn = snap.prompt_turns.values().next().unwrap();
    assert_eq!(turn.state, PromptTurnState::Active);
    assert_eq!(turn.text.as_deref(), Some("hello via tracer"));
}
