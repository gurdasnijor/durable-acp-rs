use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use durable_acp_rs::stream_server::StreamServer;
use durable_acp_rs::state::{
    ChunkType, ConnectionRow, ConnectionState, PromptTurnRow, PromptTurnState, StateEnvelope,
    StateHeaders,
};

mod common;

#[tokio::test]
async fn stream_db_applies_state_events() {
    let db = durable_acp_rs::state::StreamDb::new();
    let row = ConnectionRow {
        logical_connection_id: "conn-1".to_string(),
        state: ConnectionState::Created,
        latest_session_id: None,
        cwd: None,
        last_error: None,
            queue_paused: None,
        created_at: 1,
        updated_at: 1,
    };
    let event = StateEnvelope {
        entity_type: "connection".to_string(),
        key: "conn-1".to_string(),
        headers: StateHeaders {
            operation: "insert".to_string(),
        },
        value: Some(row),
    };

    db.apply_json_message(&serde_json::to_vec(&event).unwrap())
        .await
        .unwrap();

    let snapshot = db.snapshot().await;
    assert_eq!(snapshot.connections.len(), 1);
    assert_eq!(
        snapshot.connections["conn-1"].state,
        ConnectionState::Created
    );
}

#[tokio::test]
async fn embedded_durable_streams_serves_stream_protocol() {
    let tmp = tempfile::tempdir().unwrap();
    let bind = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0);
    let streams = StreamServer::start_with_dir(bind, "state", tmp.path().to_path_buf())
        .await
        .unwrap();

    let client = reqwest::Client::new();
    let create = client
        .put(streams.stream_url("events"))
        .header("content-type", "application/json")
        .send()
        .await
        .unwrap();
    assert!(create.status().is_success());

    let append = client
        .post(streams.stream_url("events"))
        .header("content-type", "application/json")
        .body(r#"{"hello":"world"}"#)
        .send()
        .await
        .unwrap();
    assert!(append.status().is_success());

    let read = client
        .get(streams.stream_url("events"))
        .send()
        .await
        .unwrap();
    assert!(read.status().is_success());
    let body = read.text().await.unwrap();
    assert!(body.contains("hello"));
}

#[tokio::test]
async fn app_state_persists_chunks_into_state_stream() {
    let app = common::test_app().await;

    let prompt_turn = PromptTurnRow {
        prompt_turn_id: "turn-1".to_string(),
        logical_connection_id: app.connection_id.clone(),
        session_id: "session-1".to_string(),
        request_id: "req-1".to_string(),
        text: Some("hello".to_string()),
        state: PromptTurnState::Active,
            position: None,
        stop_reason: None,
        started_at: 1,
        completed_at: None,
    };
    app.write_state_event("prompt_turn", "insert", "turn-1", Some(&prompt_turn))
        .await
        .unwrap();
    app.record_chunk("turn-1", ChunkType::Text, "hi".to_string())
        .await
        .unwrap();

    let snapshot = app.stream_server.stream_db.snapshot().await;
    assert_eq!(snapshot.prompt_turns.len(), 1);
    assert_eq!(snapshot.chunks.len(), 1);
    assert_eq!(snapshot.chunks.values().next().unwrap().content, "hi");
}

#[tokio::test]
async fn file_storage_survives_restart() {
    let tmp = tempfile::tempdir().unwrap();
    let bind = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0);

    // First instance: write state
    {
        let ds = StreamServer::start_with_dir(
            bind,
            "durable-acp-state",
            tmp.path().to_path_buf(),
        )
        .await
        .unwrap();
        let app = common::TestApp::new(ds).await;
        app.record_chunk("turn-1", ChunkType::Text, "persisted".to_string())
            .await
            .unwrap();
    }

    // Second instance: state should be replayed from disk
    let bind2 = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0);
    let ds2 = StreamServer::start_with_dir(
        bind2,
        "durable-acp-state",
        tmp.path().to_path_buf(),
    )
    .await
    .unwrap();

    let snapshot = ds2.stream_db.snapshot().await;
    assert_eq!(snapshot.chunks.len(), 1);
    assert_eq!(
        snapshot.chunks.values().next().unwrap().content,
        "persisted"
    );
}
