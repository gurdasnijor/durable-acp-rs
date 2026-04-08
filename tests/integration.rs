//! Integration tests — verify components interact as expected.
//!
//! Guided by docs/index.md architecture:
//! - State observation via durable stream + StreamDB (not REST)
//! - REST API is queue management + filesystem only
//! - Webhook forwarder fires coalesced events on state transitions

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;

use durable_acp_rs::api;
use durable_acp_rs::stream_subscriber::StreamSubscriber;

mod common;
use common::TestApp;
use durable_acp_rs::state::{
    ChunkType, CollectionChange, ConnectionState, PendingRequestState, PermissionRow,
    PromptTurnRow, PromptTurnState, TerminalRow, TerminalState,
};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

async fn test_app() -> Arc<TestApp> {
    common::test_app().await
}

/// Spin up the REST API on an ephemeral port, return the base URL.
async fn test_server(app: Arc<TestApp>) -> String {
    let router = api::router(api::ApiState {
        stream_server: app.stream_server.clone(),
        connection_id: app.connection_id.clone(),
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    format!("http://{addr}")
}

// ---------------------------------------------------------------------------
// StreamDB: subscribe_changes fires on state mutations
// ---------------------------------------------------------------------------

#[tokio::test]
async fn stream_db_notifies_on_connection_insert() {
    let app = test_app().await;
    let mut rx = app.stream_server.stream_db.subscribe_changes();

    // TestApp::new already inserted a connection — drain that notification
    // by checking the snapshot directly
    let snapshot = app.stream_server.stream_db.snapshot().await;
    assert_eq!(snapshot.connections.len(), 1);

    // Update the connection — should trigger a notification
    app.update_connection(|row| row.state = ConnectionState::Attached)
        .await
        .unwrap();

    let change = tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv())
        .await
        .expect("timeout waiting for change")
        .unwrap();
    assert!(matches!(change, CollectionChange::Connections));

    let snapshot = app.stream_server.stream_db.snapshot().await;
    let conn = snapshot.connections.values().next().unwrap();
    assert_eq!(conn.state, ConnectionState::Attached);
}

#[tokio::test]
async fn stream_db_notifies_on_chunk_insert() {
    let app = test_app().await;
    let mut rx = app.stream_server.stream_db.subscribe_changes();

    // Drain connection insert notification
    let _ = tokio::time::timeout(std::time::Duration::from_millis(100), rx.recv()).await;

    app.record_chunk("turn-1", ChunkType::Text, "hello".to_string())
        .await
        .unwrap();

    let change = tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv())
        .await
        .expect("timeout")
        .unwrap();
    assert!(matches!(change, CollectionChange::Chunks));
}

// ---------------------------------------------------------------------------
// Filesystem access: read file + directory tree + path traversal protection
// ---------------------------------------------------------------------------

#[tokio::test]
async fn filesystem_read_file() {
    let app = test_app().await;
    let conn_id = app.connection_id.clone();

    // Set cwd on the connection to a temp directory with a test file
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("test.txt"), "file content").unwrap();
    app.update_connection(|row| {
        row.cwd = Some(tmp.path().to_string_lossy().to_string());
    })
    .await
    .unwrap();

    let base = test_server(app.clone()).await;
    let http = reqwest::Client::new();

    let resp = http
        .get(format!(
            "{base}/api/v1/connections/{conn_id}/files?path=test.txt"
        ))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success());
    assert_eq!(resp.text().await.unwrap(), "file content");
}

#[tokio::test]
async fn filesystem_tree() {
    let app = test_app().await;
    let conn_id = app.connection_id.clone();

    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("a.txt"), "").unwrap();
    std::fs::create_dir(tmp.path().join("subdir")).unwrap();

    app.update_connection(|row| {
        row.cwd = Some(tmp.path().to_string_lossy().to_string());
    })
    .await
    .unwrap();

    let base = test_server(app.clone()).await;
    let http = reqwest::Client::new();

    let resp = http
        .get(format!("{base}/api/v1/connections/{conn_id}/fs/tree"))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success());
    let entries: Vec<serde_json::Value> = resp.json().await.unwrap();
    let names: Vec<&str> = entries.iter().filter_map(|e| e["name"].as_str()).collect();
    assert!(names.contains(&"a.txt"));
    assert!(names.contains(&"subdir"));

    // Directories sort before files
    let types: Vec<&str> = entries.iter().filter_map(|e| e["type"].as_str()).collect();
    assert_eq!(types[0], "directory");
}

#[tokio::test]
async fn filesystem_path_traversal_blocked() {
    let app = test_app().await;
    let conn_id = app.connection_id.clone();

    let tmp = tempfile::tempdir().unwrap();
    app.update_connection(|row| {
        row.cwd = Some(tmp.path().to_string_lossy().to_string());
    })
    .await
    .unwrap();

    let base = test_server(app.clone()).await;
    let http = reqwest::Client::new();

    let resp = http
        .get(format!(
            "{base}/api/v1/connections/{conn_id}/files?path=../../etc/passwd"
        ))
        .send()
        .await
        .unwrap();
    // Should be 403 Forbidden or 404 Not Found (not 200)
    assert!(
        resp.status() == 403 || resp.status() == 404,
        "path traversal should be blocked, got {}",
        resp.status()
    );
}

// ---------------------------------------------------------------------------
// Terminal kill via REST updates state in StreamDB
// ---------------------------------------------------------------------------

#[tokio::test]
async fn kill_terminal_updates_stream_db() {
    let app = test_app().await;
    let conn_id = app.connection_id.clone();

    // Insert a terminal into state
    let terminal = TerminalRow {
        terminal_id: "term-1".to_string(),
        logical_connection_id: conn_id.clone(),
        session_id: "s1".to_string(),
        prompt_turn_id: None,
        state: TerminalState::Open,
        command: Some("bash".to_string()),
        exit_code: None,
        signal: None,
        created_at: 1,
        updated_at: 1,
    };
    app.write_state_event("terminal", "insert", "term-1", Some(&terminal))
        .await
        .unwrap();

    let base = test_server(app.clone()).await;
    let http = reqwest::Client::new();

    let resp = http
        .delete(format!(
            "{base}/api/v1/connections/{conn_id}/terminals/term-1"
        ))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success());

    // Verify terminal state changed in StreamDB
    let snapshot = app.stream_server.stream_db.snapshot().await;
    let term = snapshot.terminals.get("term-1").unwrap();
    assert_eq!(term.state, TerminalState::Released);
}

// ---------------------------------------------------------------------------
// Webhook forwarder: state transitions → coalesced events
// ---------------------------------------------------------------------------

#[tokio::test]
async fn webhook_detects_prompt_turn_completion() {
    use durable_acp_rs::webhook;

    let app = test_app().await;

    // Mock HTTP server to receive webhooks
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let mock_router = {
        let tx = tx.clone();
        axum::Router::new().route(
            "/webhook",
            axum::routing::post(move |body: String| {
                let tx = tx.clone();
                async move {
                    let _ = tx.send(body);
                    ""
                }
            }),
        )
    };
    let mock_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mock_addr = mock_listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(mock_listener, mock_router).await.unwrap() });

    let config = webhook::WebhookConfig {
        url: format!("http://{mock_addr}/webhook"),
        events: vec!["*".to_string()],
        secret: Some("test-secret".to_string()),
    };

    let _handle = webhook::spawn_forwarder(
        app.stream_server.stream_db.clone(),
        vec![config],
    );

    // Insert a prompt turn as "active"
    let turn = PromptTurnRow {
        prompt_turn_id: "turn-1".to_string(),
        logical_connection_id: app.connection_id.clone(),
        session_id: "s1".to_string(),
        request_id: "req-1".to_string(),
        text: Some("hello".to_string()),
        state: PromptTurnState::Active,
            position: None,
        stop_reason: None,
        started_at: 1,
        completed_at: None,
    };
    app.write_state_event("prompt_turn", "insert", "turn-1", Some(&turn))
        .await
        .unwrap();

    // No webhook yet — active is not a terminal state
    let result = tokio::time::timeout(std::time::Duration::from_millis(200), rx.recv()).await;
    assert!(result.is_err(), "should not fire webhook for active state");

    // Now complete the turn
    app.finish_prompt_turn("turn-1", "end_turn".to_string(), PromptTurnState::Completed)
        .await
        .unwrap();

    // Should receive webhook with end_turn event
    let body = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
        .await
        .expect("timeout waiting for webhook")
        .expect("channel closed");

    let payload: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(payload["event"]["type"], "end_turn");
    assert_eq!(payload["sessionId"], "s1");
    assert!(payload["eventId"].is_string());
    assert!(payload["timestamp"].is_string());
}

#[tokio::test]
async fn webhook_detects_permission_request() {
    use durable_acp_rs::webhook;

    let app = test_app().await;

    // Use an axum mock server (more reliable than raw TCP)
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let mock_router = {
        let tx = tx.clone();
        axum::Router::new().route(
            "/webhook",
            axum::routing::post(move |body: String| {
                let tx = tx.clone();
                async move {
                    let _ = tx.send(body);
                    ""
                }
            }),
        )
    };
    let mock_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mock_addr = mock_listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(mock_listener, mock_router).await.unwrap() });

    let _handle = webhook::spawn_forwarder(
        app.stream_server.stream_db.clone(),
        vec![webhook::WebhookConfig {
            url: format!("http://{mock_addr}/webhook"),
            events: vec!["permission_request".to_string()],
            secret: None,
        }],
    );

    // Give forwarder time to start listening
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // Insert a pending permission
    let perm = PermissionRow {
        request_id: "perm-1".to_string(),
        jsonrpc_id: serde_json::json!(1),
        logical_connection_id: app.connection_id.clone(),
        session_id: "s1".to_string(),
        prompt_turn_id: "turn-1".to_string(),
        title: Some("Read file".to_string()),
        tool_call_id: Some("tc-1".to_string()),
        options: None,
        state: PendingRequestState::Pending,
        outcome: None,
        created_at: 1,
        resolved_at: None,
    };
    app.write_state_event("permission", "insert", "perm-1", Some(&perm))
        .await
        .unwrap();

    let body = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
        .await
        .expect("timeout waiting for webhook")
        .expect("channel closed");

    let payload: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(payload["event"]["type"], "permission_request");
    assert_eq!(payload["event"]["data"]["title"], "Read file");
}

// ---------------------------------------------------------------------------
// Durable stream SSE: state available via HTTP (architecture key principle)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn state_stream_accessible_via_http() {
    let app = test_app().await;

    // Write a chunk
    app.record_chunk("turn-1", ChunkType::Text, "streamed".to_string())
        .await
        .unwrap();

    // Read back from the durable stream HTTP endpoint
    let http = reqwest::Client::new();
    let resp = http
        .get(app.state_stream_url())
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success());
    let body = resp.text().await.unwrap();
    assert!(body.contains("streamed"), "state stream should contain chunk data");
    assert!(body.contains("chunk"), "state stream should contain entity type");
}

// ---------------------------------------------------------------------------
// TestApp init: creates connection in StreamDB automatically
// ---------------------------------------------------------------------------

#[tokio::test]
async fn app_state_init_creates_connection() {
    let app = test_app().await;
    let snapshot = app.stream_server.stream_db.snapshot().await;
    assert_eq!(snapshot.connections.len(), 1);
    let conn = snapshot.connections.get(&app.connection_id).unwrap();
    assert_eq!(conn.state, ConnectionState::Created);
}

// ---------------------------------------------------------------------------
// Multiple TestApp instances share one durable stream
// ---------------------------------------------------------------------------

#[tokio::test]
async fn shared_durable_streams_see_each_others_state() {
    let tmp = tempfile::tempdir().unwrap();
    let bind = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0);
    let ds = durable_acp_rs::stream_server::StreamServer::start_with_dir(
        bind, "durable-acp-state", tmp.path().to_path_buf(),
    )
    .await
    .unwrap();

    let app_a = TestApp::new(ds.clone()).await;
    let app_b = TestApp::new(ds.clone()).await;

    // Both should see both connections
    let snapshot = ds.stream_db.snapshot().await;
    assert_eq!(snapshot.connections.len(), 2);
    assert!(snapshot.connections.contains_key(&app_a.connection_id));
    assert!(snapshot.connections.contains_key(&app_b.connection_id));

    // App A writes a chunk, App B should see it via shared StreamDB
    app_a
        .record_chunk("turn-1", ChunkType::Text, "from A".to_string())
        .await
        .unwrap();

    let snapshot = ds.stream_db.snapshot().await;
    assert_eq!(snapshot.chunks.len(), 1);
    assert_eq!(snapshot.chunks.values().next().unwrap().content, "from A");
}

// ---------------------------------------------------------------------------
// Webhook: HMAC signature verification
// ---------------------------------------------------------------------------

#[tokio::test]
async fn webhook_sends_hmac_signature() {
    use durable_acp_rs::webhook;

    let app = test_app().await;

    let (header_tx, mut header_rx) = tokio::sync::mpsc::unbounded_channel::<(String, String)>();

    // Mock server that captures headers
    let mock_router = {
        axum::Router::new().route(
            "/webhook",
            axum::routing::post(
                move |headers: axum::http::HeaderMap, body: String| {
                    let tx = header_tx.clone();
                    async move {
                        let sig = headers.get("x-webhook-signature")
                            .map(|v| v.to_str().unwrap_or("").to_string())
                            .unwrap_or_default();
                        let _ = tx.send((sig, body));
                        ""
                    }
                },
            ),
        )
    };
    let mock_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mock_addr = mock_listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(mock_listener, mock_router).await.unwrap() });

    let secret = "test-hmac-secret";
    let _handle = webhook::spawn_forwarder(
        app.stream_server.stream_db.clone(),
        vec![webhook::WebhookConfig {
            url: format!("http://{mock_addr}/webhook"),
            events: vec!["*".to_string()],
            secret: Some(secret.to_string()),
        }],
    );

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // Trigger an end_turn event
    let turn = PromptTurnRow {
        prompt_turn_id: "hmac-turn".to_string(),
        logical_connection_id: app.connection_id.clone(),
        session_id: "s1".to_string(),
        request_id: "req-hmac".to_string(),
        text: Some("test".to_string()),
        state: PromptTurnState::Completed,
            position: None,
        stop_reason: Some("end_turn".to_string()),
        started_at: 1,
        completed_at: Some(2),
    };
    app.write_state_event("prompt_turn", "insert", "hmac-turn", Some(&turn))
        .await
        .unwrap();

    let (sig, body) = tokio::time::timeout(std::time::Duration::from_secs(2), header_rx.recv())
        .await
        .expect("timeout")
        .expect("channel closed");

    // Verify signature format
    assert!(sig.starts_with("sha256="), "signature should start with sha256=, got: {sig}");

    // Verify HMAC is correct
    use hmac::{Hmac, Mac};
    use sha2::Sha256;
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
    mac.update(body.as_bytes());
    let expected = mac.finalize().into_bytes();
    let expected_hex: String = expected.iter().map(|b| format!("{b:02x}")).collect();
    assert_eq!(sig, format!("sha256={expected_hex}"));
}

// ---------------------------------------------------------------------------
// Webhook: event filtering — only subscribed events are delivered
// ---------------------------------------------------------------------------

#[tokio::test]
async fn webhook_filters_by_event_type() {
    use durable_acp_rs::webhook;

    let app = test_app().await;

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let mock_router = {
        let tx = tx.clone();
        axum::Router::new().route(
            "/webhook",
            axum::routing::post(move |body: String| {
                let tx = tx.clone();
                async move { let _ = tx.send(body); "" }
            }),
        )
    };
    let mock_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mock_addr = mock_listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(mock_listener, mock_router).await.unwrap() });

    // Only subscribe to permission_request — not end_turn
    let _handle = webhook::spawn_forwarder(
        app.stream_server.stream_db.clone(),
        vec![webhook::WebhookConfig {
            url: format!("http://{mock_addr}/webhook"),
            events: vec!["permission_request".to_string()],
            secret: None,
        }],
    );

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // Trigger an end_turn — should NOT fire webhook
    let turn = PromptTurnRow {
        prompt_turn_id: "filter-turn".to_string(),
        logical_connection_id: app.connection_id.clone(),
        session_id: "s1".to_string(),
        request_id: "req-filter".to_string(),
        text: Some("test".to_string()),
        state: PromptTurnState::Completed,
            position: None,
        stop_reason: Some("end_turn".to_string()),
        started_at: 1,
        completed_at: Some(2),
    };
    app.write_state_event("prompt_turn", "insert", "filter-turn", Some(&turn))
        .await
        .unwrap();

    // Should timeout — end_turn filtered out
    let result = tokio::time::timeout(std::time::Duration::from_millis(300), rx.recv()).await;
    assert!(result.is_err(), "end_turn should be filtered out when only permission_request subscribed");

    // Now trigger a permission_request — should fire
    let perm = PermissionRow {
        request_id: "filter-perm".to_string(),
        jsonrpc_id: serde_json::json!(1),
        logical_connection_id: app.connection_id.clone(),
        session_id: "s1".to_string(),
        prompt_turn_id: "filter-turn".to_string(),
        title: Some("filtered test".to_string()),
        tool_call_id: None,
        options: None,
        state: PendingRequestState::Pending,
        outcome: None,
        created_at: 1,
        resolved_at: None,
    };
    app.write_state_event("permission", "insert", "filter-perm", Some(&perm))
        .await
        .unwrap();

    let body = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
        .await
        .expect("timeout — permission_request should not be filtered")
        .expect("channel closed");

    let payload: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(payload["event"]["type"], "permission_request");
}

// ---------------------------------------------------------------------------
// Webhook: error event fires on broken prompt turn
// ---------------------------------------------------------------------------

#[tokio::test]
async fn webhook_fires_error_on_broken_turn() {
    use durable_acp_rs::webhook;

    let app = test_app().await;

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let mock_router = {
        let tx = tx.clone();
        axum::Router::new().route(
            "/webhook",
            axum::routing::post(move |body: String| {
                let tx = tx.clone();
                async move { let _ = tx.send(body); "" }
            }),
        )
    };
    let mock_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mock_addr = mock_listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(mock_listener, mock_router).await.unwrap() });

    let _handle = webhook::spawn_forwarder(
        app.stream_server.stream_db.clone(),
        vec![webhook::WebhookConfig {
            url: format!("http://{mock_addr}/webhook"),
            events: vec!["*".to_string()],
            secret: None,
        }],
    );

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // Insert a broken prompt turn directly
    let turn = PromptTurnRow {
        prompt_turn_id: "broken-turn".to_string(),
        logical_connection_id: app.connection_id.clone(),
        session_id: "s1".to_string(),
        request_id: "req-broken".to_string(),
        text: Some("test".to_string()),
        state: PromptTurnState::Broken,
            position: None,
        stop_reason: Some("agent crashed".to_string()),
        started_at: 1,
        completed_at: Some(2),
    };
    app.write_state_event("prompt_turn", "insert", "broken-turn", Some(&turn))
        .await
        .unwrap();

    let body = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
        .await
        .expect("timeout")
        .expect("channel closed");

    let payload: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(payload["event"]["type"], "error");
    assert_eq!(payload["event"]["data"]["stopReason"], "agent crashed");
}

// ---------------------------------------------------------------------------
// StreamSubscriber: SSE subscriber materializes remote state
// ---------------------------------------------------------------------------

#[tokio::test]
async fn durable_session_preloads_existing_state() {
    // Set up a conductor-side app that writes state
    let app = test_app().await;

    // Write some state before the session connects
    app.record_chunk("turn-1", ChunkType::Text, "pre-existing".to_string())
        .await
        .unwrap();

    // Connect a StreamSubscriber to the same stream via SSE
    let stream_url = app.state_stream_url();
    let mut session = StreamSubscriber::new(stream_url);
    session.preload().await.unwrap();

    // The session's StreamDb should have materialized the connection + chunk
    let snapshot = session.stream_db().snapshot().await;
    assert!(
        !snapshot.connections.is_empty(),
        "session should see the connection"
    );
    assert!(
        !snapshot.chunks.is_empty(),
        "session should see the pre-existing chunk"
    );
    assert_eq!(
        snapshot.chunks.values().next().unwrap().content,
        "pre-existing"
    );

    session.disconnect();
}

#[tokio::test]
async fn durable_session_receives_live_updates() {
    let app = test_app().await;
    let stream_url = app.state_stream_url();

    let mut session = StreamSubscriber::new(stream_url);
    session.preload().await.unwrap();

    // Subscribe to changes on the session's StreamDb
    let mut rx = session.stream_db().subscribe_changes();

    // Write new state AFTER session is connected
    app.record_chunk("turn-2", ChunkType::Text, "live update".to_string())
        .await
        .unwrap();

    // The session should receive the live update via SSE
    let change = tokio::time::timeout(std::time::Duration::from_secs(3), rx.recv())
        .await
        .expect("timeout waiting for live update")
        .unwrap();
    assert!(matches!(change, CollectionChange::Chunks));

    let snapshot = session.stream_db().snapshot().await;
    let live_chunk = snapshot.chunks.values().find(|c| c.content == "live update");
    assert!(live_chunk.is_some(), "session should see the live chunk");

    session.disconnect();
}
