use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;

use anyhow::{Context, Result};
use axum::Router;
use clap::Parser;
use sacp::ByteStreams;
use sacp_tokio::AcpAgent;
use tokio::net::TcpListener;
use tracing_subscriber::EnvFilter;

use durable_acp_rs::api;
use durable_acp_rs::conductor_state::ConductorState;
use durable_acp_rs::conductor::build_conductor;
use durable_acp_rs::registry;

#[derive(Debug, Parser)]
struct Cli {
    #[arg(long, default_value_t = 4437)]
    port: u16,
    #[arg(long, default_value = "durable-acp-state")]
    state_stream: String,
    #[arg(long, default_value = "default")]
    name: String,
    #[arg(trailing_var_arg = true, required = true)]
    agent_command: Vec<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .without_time()
        .init();

    let cli = Cli::parse();
    let bind = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), cli.port);
    let app = Arc::new(ConductorState::new(bind, cli.state_stream).await?);
    let api_port = cli.port + 1;

    let agent_name = cli.name.clone();
    registry::register(registry::AgentEntry {
        name: agent_name.clone(),
        api_url: format!("http://127.0.0.1:{api_port}"),
        logical_connection_id: app.logical_connection_id.clone(),
        registered_at: durable_acp_rs::conductor_state::now_ms(),
    })
    .context("register agent")?;

    tracing::info!(name = %agent_name, api_port, streams_port = cli.port, "Conductor started");

    let acp_config = api::AcpEndpointConfig {
        agent_command: cli.agent_command.clone(),
        stream_server: app.stream_server.clone(),
    };
    let api_router = api::router(app.clone(), Some(acp_config));
    spawn_api_server(api_port, api_router).await?;

    let agent = AcpAgent::from_args(cli.agent_command).context("parse agent command")?;

    let conductor = build_conductor(app, agent);

    let result = conductor
        .run(ByteStreams::new(
            tokio_util::compat::TokioAsyncWriteCompatExt::compat_write(tokio::io::stdout()),
            tokio_util::compat::TokioAsyncReadCompatExt::compat(tokio::io::stdin()),
        ))
        .await
        .context("run conductor");

    let _ = registry::unregister(&agent_name);
    result
}

async fn spawn_api_server(port: u16, router: Router) -> Result<()> {
    let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], port))).await?;
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    Ok(())
}
