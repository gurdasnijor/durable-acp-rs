//! Tests for the `DurableAcpClient` — the decoupled ACP client interface.
//!
//! Uses Testy mock agent + in-process conductor. No subprocess, no LocalSet.

use std::sync::{Arc, Mutex};

use agent_client_protocol_test::testy::{Testy, TestyCommand};
use sacp_conductor::{ConductorImpl, McpBridgeMode, ProxiesAndAgent};
use tokio::io::duplex;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

use durable_acp_rs::client::{self, AcpClientHandler};
use durable_acp_rs::durable_stream_tracer::DurableStreamTracer;
use durable_acp_rs::peer_mcp::PeerMcpProxy;

mod common;
use common::TestApp;

// ---------------------------------------------------------------------------
// Test handler — collects events
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
enum TestEvent {
    Ready(String),
    Text(String, String),
    Error(String, String),
}

struct TestHandler {
    events: Arc<Mutex<Vec<TestEvent>>>,
}

impl AcpClientHandler for TestHandler {
    fn on_ready(&self, name: &str) {
        self.events.lock().unwrap().push(TestEvent::Ready(name.to_string()));
    }
    fn on_text(&self, name: &str, text: &str) {
        self.events.lock().unwrap().push(TestEvent::Text(name.to_string(), text.to_string()));
    }
    fn on_error(&self, name: &str, error: &str) {
        self.events.lock().unwrap().push(TestEvent::Error(name.to_string(), error.to_string()));
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Set up in-process conductor + Testy, return the client-side transport.
fn setup_conductor(
    app: Arc<TestApp>,
) -> (sacp::DynConnectTo<sacp::Client>, tokio::task::JoinHandle<Result<(), sacp::Error>>) {
    let (editor_write, conductor_read) = duplex(8192);
    let (conductor_write, editor_read) = duplex(8192);

    let tracer = DurableStreamTracer::start(
        app.stream_server.clone(),
        app.stream_server.state_stream.clone(),
    );
    let handle = tokio::spawn(async move {
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

    let transport = sacp::DynConnectTo::new(sacp::ByteStreams::new(
        editor_write.compat_write(),
        editor_read.compat(),
    ));

    (transport, handle)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn client_receives_ready_event() {
    let app = common::test_app().await;
    let (transport, conductor) = setup_conductor(app);

    let events = Arc::new(Mutex::new(Vec::new()));
    let handler = Arc::new(TestHandler { events: events.clone() });

    let (prompt_tx, prompt_rx) = tokio::sync::mpsc::unbounded_channel();

    let client_task = tokio::spawn(async move {
        client::run_acp_client(
            client::AcpClientConfig {
                name: "test-agent".to_string(),
                transport,
            },
            handler,
            prompt_rx,
        )
        .await
    });

    // Wait for ready
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    let evts = events.lock().unwrap();
    assert!(
        evts.iter().any(|e| matches!(e, TestEvent::Ready(n) if n == "test-agent")),
        "expected Ready event, got: {:?}",
        *evts,
    );

    drop(prompt_tx);
    let _ = client_task.await;
    conductor.abort();
}

#[tokio::test]
async fn client_sends_prompt_receives_text() {
    let app = common::test_app().await;
    let (transport, conductor) = setup_conductor(app);

    let events = Arc::new(Mutex::new(Vec::new()));
    let handler = Arc::new(TestHandler { events: events.clone() });

    let (prompt_tx, prompt_rx) = tokio::sync::mpsc::unbounded_channel();

    let client_task = tokio::spawn(async move {
        client::run_acp_client(
            client::AcpClientConfig {
                name: "echo-agent".to_string(),
                transport,
            },
            handler,
            prompt_rx,
        )
        .await
    });

    // Wait for ready, then send prompt
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    prompt_tx
        .send(TestyCommand::Echo { message: "hello from client".to_string() }.to_prompt())
        .unwrap();

    // Wait for response
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    let evts = events.lock().unwrap();
    assert!(
        evts.iter().any(|e| matches!(e, TestEvent::Text(_, t) if t.contains("hello from client"))),
        "expected Text event with echoed content, got: {:?}",
        *evts,
    );

    drop(prompt_tx);
    let _ = client_task.await;
    conductor.abort();
}

#[tokio::test]
async fn client_handles_multiple_prompts() {
    let app = common::test_app().await;
    let (transport, conductor) = setup_conductor(app);

    let events = Arc::new(Mutex::new(Vec::new()));
    let handler = Arc::new(TestHandler { events: events.clone() });

    let (prompt_tx, prompt_rx) = tokio::sync::mpsc::unbounded_channel();

    let client_task = tokio::spawn(async move {
        client::run_acp_client(
            client::AcpClientConfig {
                name: "multi".to_string(),
                transport,
            },
            handler,
            prompt_rx,
        )
        .await
    });

    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    prompt_tx.send(TestyCommand::Greet.to_prompt()).unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;

    prompt_tx
        .send(TestyCommand::Echo { message: "second".to_string() }.to_prompt())
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;

    let evts = events.lock().unwrap();
    let text_events: Vec<_> = evts
        .iter()
        .filter(|e| matches!(e, TestEvent::Text(..)))
        .collect();
    assert_eq!(
        text_events.len(),
        2,
        "expected 2 text events, got: {:?}",
        text_events,
    );

    drop(prompt_tx);
    let _ = client_task.await;
    conductor.abort();
}

#[tokio::test]
async fn client_shutdown_on_channel_close() {
    let app = common::test_app().await;
    let (transport, conductor) = setup_conductor(app);

    let events = Arc::new(Mutex::new(Vec::new()));
    let handler = Arc::new(TestHandler { events: events.clone() });

    let (prompt_tx, prompt_rx) = tokio::sync::mpsc::unbounded_channel();

    let client_task = tokio::spawn(async move {
        client::run_acp_client(
            client::AcpClientConfig {
                name: "shutdown".to_string(),
                transport,
            },
            handler,
            prompt_rx,
        )
        .await
    });

    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    // Drop the sender — client should exit gracefully
    drop(prompt_tx);

    let result = tokio::time::timeout(std::time::Duration::from_secs(5), client_task)
        .await
        .expect("client should shut down within 5s");

    // Should complete without panic
    assert!(result.is_ok(), "client task panicked");
    conductor.abort();
}
