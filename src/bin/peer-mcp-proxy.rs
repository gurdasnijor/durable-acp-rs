//! Standalone PeerMcpProxy — chainable proxy over stdio.
//!
//! The conductor spawns this as a subprocess in the proxy chain:
//!
//!   sacp-conductor agent \
//!     "durable-state-proxy --port 4437" \
//!     "peer-mcp-proxy" \
//!     "npx @agentclientprotocol/claude-agent-acp"

use anyhow::{Context, Result};
use sacp::{ByteStreams, Conductor, ConnectTo};

use durable_acp_rs::peer_mcp::PeerMcpProxy;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .without_time()
        .init();

    ConnectTo::<Conductor>::connect_to(
        PeerMcpProxy,
        ByteStreams::new(
            tokio_util::compat::TokioAsyncWriteCompatExt::compat_write(tokio::io::stdout()),
            tokio_util::compat::TokioAsyncReadCompatExt::compat(tokio::io::stdin()),
        ),
    )
    .await
    .context("peer-mcp-proxy")
}
