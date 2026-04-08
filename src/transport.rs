//! Pluggable transports for connecting ACP clients to conductors.
//!
//! Each transport implements `ConnectTo<R>` from sacp — the same
//! abstraction used by `AcpAgent` (stdio subprocess), `ByteStreams`
//! (raw byte streams), and `Channel` (in-process zero-serialization).
//!
//! The dashboard resolves transport from agents.toml config and passes
//! it to `Client.builder().connect_with(transport, |cx| {...})`.

use serde::Deserialize;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

/// Transport configuration — declared per agent in agents.toml.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum TransportConfig {
    /// Default: spawn conductor as subprocess, connect via stdio.
    Stdio,
    /// Connect to a remote conductor via WebSocket.
    Ws { url: String },
    /// Connect to a remote conductor via raw TCP.
    Tcp { host: String, port: u16 },
}

/// WebSocket transport — connects to a remote conductor at a WebSocket URL.
/// Bridges WS text frames to sacp::Channel messages.
pub struct WebSocketTransport {
    pub url: String,
}

impl sacp::ConnectTo<sacp::Client> for WebSocketTransport {
    async fn connect_to(self, client: impl sacp::ConnectTo<sacp::Agent>) -> Result<(), sacp::Error> {
        let (ws, _) = tokio_tungstenite::connect_async(&self.url)
            .await
            .map_err(|e| sacp::util::internal_error(format!("WebSocket connect failed: {}", e)))?;

        let (write, read) = futures::StreamExt::split(ws);

        // Bridge WS frames ↔ Channel
        let (channel_transport, channel_client) = sacp::Channel::duplex();
        let sacp::Channel { tx, rx: mut from_client_rx } = channel_transport;

        // WS → Channel (incoming)
        tokio::spawn(async move {
            use futures::StreamExt;
            let mut read = read;
            while let Some(Ok(msg)) = read.next().await {
                if let tokio_tungstenite::tungstenite::Message::Text(text) = msg {
                    if let Ok(parsed) = serde_json::from_str::<sacp::jsonrpcmsg::Message>(&text) {
                        if tx.unbounded_send(Ok(parsed)).is_err() { break; }
                    }
                }
            }
        });

        // Channel → WS (outgoing)
        tokio::spawn(async move {
            use futures::SinkExt;
            let mut write = write;
            use futures::StreamExt as _;
            while let Some(msg) = from_client_rx.next().await {
                let message = match msg { Ok(m) => m, Err(_) => break };
                if let Ok(json) = serde_json::to_string(&message) {
                    if write.send(tokio_tungstenite::tungstenite::Message::Text(json.into())).await.is_err() { break; }
                }
            }
        });

        // Connect the client to our channel
        sacp::ConnectTo::<sacp::Client>::connect_to(channel_client, client).await
    }
}

/// TCP transport — connects to a remote conductor via raw TCP.
pub struct TcpTransport {
    pub host: String,
    pub port: u16,
}

impl sacp::ConnectTo<sacp::Client> for TcpTransport {
    async fn connect_to(self, client: impl sacp::ConnectTo<sacp::Agent>) -> Result<(), sacp::Error> {
        let stream = tokio::net::TcpStream::connect((self.host.as_str(), self.port))
            .await
            .map_err(|e| sacp::util::internal_error(format!("TCP connect failed: {}", e)))?;

        let (read, write) = stream.into_split();

        sacp::ConnectTo::<sacp::Client>::connect_to(
            sacp::ByteStreams::new(write.compat_write(), read.compat()),
            client,
        )
        .await
    }
}
