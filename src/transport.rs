//! Pluggable transports for connecting ACP clients to conductors.
//!
//! Each transport implements `ConnectTo<R>` from sacp — the same
//! abstraction used by `AcpAgent` (stdio subprocess), `ByteStreams`
//! (raw byte streams), and `Channel` (in-process zero-serialization).

use anyhow::Result;
use serde::Deserialize;

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
