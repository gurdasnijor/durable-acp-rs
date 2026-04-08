//! Standalone DurableStateProxy — chainable proxy over stdio.
//!
//! The conductor spawns this as a subprocess in the proxy chain:
//!
//!   sacp-conductor agent \
//!     "durable-state-proxy --port 4437" \
//!     "peer-mcp-proxy" \
//!     "npx @agentclientprotocol/claude-agent-acp"

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;

use anyhow::{Context, Result};
use clap::Parser;
use sacp::{ByteStreams, Conductor, ConnectTo};

use durable_acp_rs::conductor_state::ConductorState;
use durable_acp_rs::durable_state_proxy::DurableStateProxy;
use durable_acp_rs::stream_server::StreamServer;

#[derive(Debug, Parser)]
#[command(name = "durable-state-proxy")]
struct Cli {
    #[arg(long, default_value_t = 4437)]
    port: u16,
    #[arg(long, default_value = "durable-acp-state")]
    state_stream: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .without_time()
        .init();

    let cli = Cli::parse();
    let bind = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), cli.port);
    let stream_server = StreamServer::start(bind, cli.state_stream).await?;
    let app = Arc::new(ConductorState::with_shared_streams(stream_server).await?);

    ConnectTo::<Conductor>::connect_to(
        DurableStateProxy { app },
        ByteStreams::new(
            tokio_util::compat::TokioAsyncWriteCompatExt::compat_write(tokio::io::stdout()),
            tokio_util::compat::TokioAsyncReadCompatExt::compat(tokio::io::stdin()),
        ),
    )
    .await
    .context("durable-state-proxy")
}
