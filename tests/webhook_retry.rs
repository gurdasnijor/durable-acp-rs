//! Webhook retry behavior — verify exponential backoff when delivery fails.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use durable_acp_rs::conductor_state::ConductorState;
use durable_acp_rs::state::{PromptTurnRow, PromptTurnState};
use durable_acp_rs::stream_server::StreamServer;
use durable_acp_rs::webhook;

async fn test_app() -> Arc<ConductorState> {
    let tmp = tempfile::tempdir().unwrap();
    let bind = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0);
    let ds = StreamServer::start_with_dir(bind, "durable-acp-state", tmp.path().to_path_buf())
        .await
        .unwrap();
    std::mem::forget(tmp);
    Arc::new(ConductorState::with_shared_streams(ds).await.unwrap())
}

#[tokio::test]
async fn webhook_retries_on_server_error() {
    let app = test_app().await;

    let attempt_count = Arc::new(AtomicUsize::new(0));
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();

    // Mock server: fail first attempt with 500, succeed on 2nd (after 5s retry)
    let counter = attempt_count.clone();
    let mock_router = axum::Router::new().route(
        "/webhook",
        axum::routing::post(move |body: String| {
            let count = counter.fetch_add(1, Ordering::SeqCst);
            let tx = tx.clone();
            async move {
                if count < 1 {
                    (axum::http::StatusCode::INTERNAL_SERVER_ERROR, "fail")
                } else {
                    let _ = tx.send(body);
                    (axum::http::StatusCode::OK, "ok")
                }
            }
        }),
    );
    let mock_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mock_addr = mock_listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(mock_listener, mock_router).await.unwrap() });

    // Use very short retry delays for testing (override not possible,
    // so we just verify delivery eventually succeeds)
    let _handle = webhook::spawn_forwarder(
        app.stream_server.stream_db.clone(),
        vec![webhook::WebhookConfig {
            url: format!("http://{mock_addr}/webhook"),
            events: vec!["*".to_string()],
            secret: None,
        }],
    );

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // Trigger an end_turn event
    let turn = PromptTurnRow {
        prompt_turn_id: "retry-turn".to_string(),
        logical_connection_id: app.logical_connection_id.clone(),
        session_id: "s1".to_string(),
        request_id: "req-retry".to_string(),
        text: Some("retry test".to_string()),
        state: PromptTurnState::Completed,
        position: None,
        stop_reason: Some("end_turn".to_string()),
        started_at: 1,
        completed_at: Some(2),
    };
    app.write_state_event("prompt_turn", "insert", "retry-turn", Some(&turn))
        .await
        .unwrap();

    // Wait for delivery — first retry delay is 5s
    let body = tokio::time::timeout(std::time::Duration::from_secs(10), rx.recv())
        .await
        .expect("webhook should succeed after 1 retry (5s delay)")
        .expect("channel closed");

    // Verify it was retried (2 attempts: 1 failure + 1 success)
    let attempts = attempt_count.load(Ordering::SeqCst);
    assert!(
        attempts >= 2,
        "expected at least 2 attempts (1 failure + 1 success), got {attempts}"
    );

    // Verify payload is correct
    let payload: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(payload["event"]["type"], "end_turn");
}

#[tokio::test]
async fn webhook_no_delivery_to_unreachable_server() {
    let app = test_app().await;

    // Point at a port that nothing is listening on
    let _handle = webhook::spawn_forwarder(
        app.stream_server.stream_db.clone(),
        vec![webhook::WebhookConfig {
            url: "http://127.0.0.1:1/webhook".to_string(), // port 1 — unreachable
            events: vec!["*".to_string()],
            secret: None,
        }],
    );

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // Trigger event — should not panic or crash the forwarder
    let turn = PromptTurnRow {
        prompt_turn_id: "unreachable-turn".to_string(),
        logical_connection_id: app.logical_connection_id.clone(),
        session_id: "s1".to_string(),
        request_id: "req-unreach".to_string(),
        text: None,
        state: PromptTurnState::Completed,
        position: None,
        stop_reason: Some("end_turn".to_string()),
        started_at: 1,
        completed_at: Some(2),
    };
    app.write_state_event("prompt_turn", "insert", "unreachable-turn", Some(&turn))
        .await
        .unwrap();

    // Forwarder should not crash — just log warnings. Give it a moment.
    tokio::time::sleep(std::time::Duration::from_secs(1)).await;

    // If we get here without panic, the test passes — forwarder is resilient
}
