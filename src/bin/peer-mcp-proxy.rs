//! Standalone PeerMcpProxy binary.
//!
//! Wraps the PeerMcpProxy as a conductor and serves on stdio.
//! Used as a proxy in a conductor chain:
//!
//!   sacp-conductor agent \
//!     "durable-state-proxy --port 4437" \
//!     "peer-mcp-proxy" \
//!     "npx @agentclientprotocol/claude-agent-acp"

use anyhow::{Context, Result};
use clap::Parser;
use sacp::ByteStreams;
use sacp_conductor::{ConductorImpl, McpBridgeMode, ProxiesAndAgent};
use sacp_tokio::AcpAgent;

use durable_acp_rs::peer_mcp::PeerMcpProxy;

#[derive(Debug, Parser)]
#[command(name = "peer-mcp-proxy", about = "Standalone peer MCP proxy")]
struct Cli {
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
    let agent = AcpAgent::from_args(cli.agent_command).context("parse agent command")?;

    ConductorImpl::new_agent(
        "peer-mcp-proxy".to_string(),
        ProxiesAndAgent::new(agent).proxy(PeerMcpProxy),
        McpBridgeMode::default(),
    )
    .run(ByteStreams::new(
        tokio_util::compat::TokioAsyncWriteCompatExt::compat_write(tokio::io::stdout()),
        tokio_util::compat::TokioAsyncReadCompatExt::compat(tokio::io::stdin()),
    ))
    .await
    .context("run peer-mcp-proxy")
}
