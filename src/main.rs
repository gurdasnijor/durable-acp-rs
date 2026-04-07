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
use durable_acp_rs::app::AppState;
use durable_acp_rs::conductor::build_conductor;

#[derive(Debug, Parser)]
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
        .with_env_filter(EnvFilter::from_default_env())
        .without_time()
        .init();

    let cli = Cli::parse();
    let bind = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), cli.port);
    let app = Arc::new(AppState::new(bind, cli.state_stream).await?);

    let api_router = api::router(app.clone());
    spawn_api_server(cli.port + 1, api_router).await?;

    let agent = AcpAgent::from_args(cli.agent_command).context("parse agent command")?;
    let conductor = build_conductor(app, agent);
    conductor
        .run(ByteStreams::new(
            tokio_util::compat::TokioAsyncWriteCompatExt::compat_write(tokio::io::stdout()),
            tokio_util::compat::TokioAsyncReadCompatExt::compat(tokio::io::stdin()),
        ))
        .await
        .context("run conductor")
}

async fn spawn_api_server(port: u16, router: Router) -> Result<()> {
    let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], port))).await?;
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    Ok(())
}
