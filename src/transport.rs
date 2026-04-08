//! Pluggable transports for connecting ACP clients to conductors.
//!
//! Uses sacp's built-in transport abstractions:
//! - `AcpAgent` (stdio subprocess) — default
//! - `ByteStreams` (any AsyncRead + AsyncWrite) — TCP
//! - `Lines` (Sink<String> + Stream<String>) — WebSocket text frames

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

/// WebSocket transport — adapts WS text frames to sacp::Lines.
pub struct WebSocketTransport {
    pub url: String,
}

impl sacp::ConnectTo<sacp::Client> for WebSocketTransport {
    async fn connect_to(self, client: impl sacp::ConnectTo<sacp::Agent>) -> Result<(), sacp::Error> {
        let (ws, _) = tokio_tungstenite::connect_async(&self.url)
            .await
            .map_err(|e| sacp::util::internal_error(format!("WebSocket connect: {}", e)))?;

        let (write, read) = futures::StreamExt::split(ws);

        // Adapt WS Sink<Message> → Sink<String, Error = io::Error>
        let outgoing = futures::SinkExt::with(
            futures::SinkExt::sink_map_err(write, |e| std::io::Error::other(e)),
            |s: String| async move {
                Ok::<_, std::io::Error>(tokio_tungstenite::tungstenite::Message::Text(s.into()))
            },
        );

        // Adapt WS Stream<Message> → Stream<io::Result<String>>
        let incoming = futures::StreamExt::filter_map(read, |msg| async move {
            match msg {
                Ok(tokio_tungstenite::tungstenite::Message::Text(t)) => {
                    Some(Ok(t.to_string()))
                }
                Err(e) => Some(Err(std::io::Error::other(e))),
                _ => None, // skip non-text frames
            }
        });

        // sacp::Lines handles JSON-RPC parsing/serialization
        sacp::ConnectTo::<sacp::Client>::connect_to(
            sacp::Lines::new(outgoing, incoming),
            client,
        ).await
    }
}

/// TCP transport — wraps TCP stream as sacp::ByteStreams.
pub struct TcpTransport {
    pub host: String,
    pub port: u16,
}

impl sacp::ConnectTo<sacp::Client> for TcpTransport {
    async fn connect_to(self, client: impl sacp::ConnectTo<sacp::Agent>) -> Result<(), sacp::Error> {
        let stream = tokio::net::TcpStream::connect((self.host.as_str(), self.port))
            .await
            .map_err(|e| sacp::util::internal_error(format!("TCP connect: {}", e)))?;

        let (read, write) = stream.into_split();

        sacp::ConnectTo::<sacp::Client>::connect_to(
            sacp::ByteStreams::new(write.compat_write(), read.compat()),
            client,
        ).await
    }
}
