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

use durable_acp_rs::app::AppState;
use durable_acp_rs::conductor::DurableStateProxy;
use durable_acp_rs::durable_streams::EmbeddedDurableStreams;

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
    let durable_streams = EmbeddedDurableStreams::start(bind, cli.state_stream).await?;
    let app = Arc::new(AppState::with_shared_streams(durable_streams).await?);

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
