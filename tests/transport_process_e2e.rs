//! Process-level end-to-end transport test.
//!
//! Spawns the real `durable-acp-rs` binary in WebSocket-only mode, points it at
//! the `testy` ACP agent binary, connects as an ACP client over WebSocket, and
//! verifies:
//! 1. The conductor accepts a non-stdio ACP transport.
//! 2. The proxy chain comes up and prompts flow successfully.
//! 3. Durable state is written to the embedded stream server.

use std::net::TcpListener;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use durable_acp_rs::transport::WebSocketTransport;

fn free_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
    let port = listener.local_addr().expect("local addr").port();
    drop(listener);
    port
}

fn conductor_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_durable-acp-rs"))
}

fn testy_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_testy"))
}

async fn wait_for_ws_ready(ws_url: &str) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        match tokio_tungstenite::connect_async(ws_url).await {
            Ok((ws, _)) => {
                drop(ws);
                return;
            }
            Err(err) => {
                if tokio::time::Instant::now() >= deadline {
                    panic!("timed out waiting for ws endpoint {ws_url}: {err}");
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    }
}

#[tokio::test]
async fn spawned_conductor_accepts_websocket_acp_client() {
    let port = free_port();
    let state_stream = format!("transport-e2e-{}", uuid::Uuid::new_v4());
    let ws_url = format!("ws://127.0.0.1:{}/acp", port + 1);
    let stream_url = format!("http://127.0.0.1:{port}/streams/{state_stream}");

    let mut child = std::process::Command::new(conductor_bin())
        .arg("--port")
        .arg(port.to_string())
        .arg("--state-stream")
        .arg(&state_stream)
        .arg("--name")
        .arg("transport-e2e")
        .arg(testy_bin())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn durable-acp-rs");

    let result = async {
        wait_for_ws_ready(&ws_url).await;

        let transport = sacp::DynConnectTo::new(WebSocketTransport { url: ws_url.clone() });

        sacp::Client
            .builder()
            .name("transport-e2e-client")
            .on_receive_request(
                async |_req: agent_client_protocol::RequestPermissionRequest, responder, _cx| {
                    responder.respond(agent_client_protocol::RequestPermissionResponse::new(
                        agent_client_protocol::RequestPermissionOutcome::Cancelled,
                    ))
                },
                sacp::on_receive_request!(),
            )
            .connect_with(transport, async |cx| {
                cx.send_request(
                    agent_client_protocol::InitializeRequest::new(
                        agent_client_protocol::ProtocolVersion::V1,
                    ),
                )
                .block_task()
                .await?;

                cx.build_session_cwd()?
                    .block_task()
                    .run_until(async |mut session| {
                        session.send_prompt("hello over websocket transport")?;
                        let response = session.read_to_string().await?;
                        assert_eq!(response, "", "testy prompt response should be empty text");
                        Ok(())
                    })
                    .await
            })
            .await
            .expect("websocket ACP flow should succeed");

        tokio::time::sleep(Duration::from_millis(200)).await;

        let body = reqwest::Client::new()
            .get(&stream_url)
            .send()
            .await
            .expect("fetch durable state stream")
            .text()
            .await
            .expect("read state stream body");

        assert!(
            body.contains("connection"),
            "state stream should contain connection events: {body}"
        );
        assert!(
            body.contains("session/prompt"),
            "state stream should contain session/prompt trace events: {body}"
        );
    }
    .await;

    let _ = child.kill();
    let _ = child.wait();

    result
}
