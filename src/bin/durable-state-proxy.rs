//! Standalone DurableStateProxy binary.
//!
//! Wraps the DurableStateProxy as a conductor and serves on stdio.
//! Used as a proxy in a conductor chain:
//!
//!   sacp-conductor agent \
//!     "durable-state-proxy --port 4437 --state-stream durable-acp-state" \
//!     "peer-mcp-proxy" \
//!     "npx @agentclientprotocol/claude-agent-acp"

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;

use anyhow::{Context, Result};
use clap::Parser;
use sacp::ByteStreams;
use sacp_conductor::{ConductorImpl, McpBridgeMode, ProxiesAndAgent};
use sacp_tokio::AcpAgent;

use durable_acp_rs::app::AppState;
use durable_acp_rs::conductor::DurableStateProxy;
use durable_acp_rs::durable_streams::EmbeddedDurableStreams;

#[derive(Debug, Parser)]
#[command(name = "durable-state-proxy", about = "Standalone durable state proxy")]
struct Cli {
    #[arg(long, default_value_t = 4437)]
    port: u16,
    #[arg(long, default_value = "durable-acp-state")]
    state_stream: String,
    #[arg(trailing_var_arg = true, required = true)]
    agent_command: Vec<String>,
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

    let agent = AcpAgent::from_args(cli.agent_command).context("parse agent command")?;

    ConductorImpl::new_agent(
        "durable-state-proxy".to_string(),
        ProxiesAndAgent::new(agent).proxy(DurableStateProxy { app }),
        McpBridgeMode::default(),
    )
    .run(ByteStreams::new(
        tokio_util::compat::TokioAsyncWriteCompatExt::compat_write(tokio::io::stdout()),
        tokio_util::compat::TokioAsyncReadCompatExt::compat(tokio::io::stdin()),
    ))
    .await
    .context("run durable-state-proxy")
}
