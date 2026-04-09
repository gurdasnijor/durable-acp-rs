use std::net::{IpAddr, SocketAddr};

use anyhow::{Context, Result};
use axum::Router;
use clap::Parser;
use sacp::ByteStreams;
use sacp_tokio::AcpAgent;
use tokio::net::TcpListener;
use tracing_subscriber::EnvFilter;
use uuid::Uuid;

use sacp_conductor::{ConductorImpl, McpBridgeMode, ProxiesAndAgent};

use durable_acp_rs::acp_server;
use durable_acp_rs::api;
use durable_acp_rs::durable_stream_tracer::DurableStreamTracer;
use durable_acp_rs::peer_mcp::PeerMcpProxy;
use durable_acp_rs::registry;
use durable_acp_rs::state;
use durable_acp_rs::stream_server::StreamServer;

#[derive(Debug, Parser)]
struct Cli {
    #[arg(long, default_value_t = 4437)]
    port: u16,
    /// Bind address for the stream server and API.
    /// Use 0.0.0.0 for remote access.
    #[arg(long, default_value = "127.0.0.1")]
    host: String,
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
    let host: IpAddr = cli.host.parse().context("invalid --host address")?;
    let bind = SocketAddr::new(host, cli.port);

    // 1. Infrastructure
    let stream_server = if let Ok(dir) = std::env::var("DATA_DIR") {
        StreamServer::start_with_dir(
            bind, &cli.state_stream, std::path::PathBuf::from(dir),
        ).await?
    } else {
        StreamServer::start(bind, &cli.state_stream).await?
    };
    let connection_id = Uuid::new_v4().to_string();
    let api_port = cli.port + 1;

    // 2. Write initial connection row
    let conn_row = state::ConnectionRow {
        logical_connection_id: connection_id.clone(),
        state: state::ConnectionState::Created,
        latest_session_id: None,
        cwd: None,
        last_error: None,
        queue_paused: None,
        created_at: state::now_ms(),
        updated_at: state::now_ms(),
    };
    let envelope = state::StateEnvelope {
        entity_type: "connection".to_string(),
        key: connection_id.clone(),
        headers: state::StateHeaders { operation: "insert".to_string() },
        value: Some(&conn_row),
    };
    stream_server.append_json(&stream_server.state_stream, &envelope).await?;

    // Set connection ID for trace event materialization
    stream_server.stream_db.set_connection_id(connection_id.clone()).await;

    // 3. Register in peer registry
    let agent_name = cli.name.clone();
    registry::register(registry::AgentEntry {
        name: agent_name.clone(),
        api_url: format!("http://127.0.0.1:{api_port}"),
        logical_connection_id: connection_id.clone(),
        registered_at: state::now_ms(),
    })
    .context("register agent")?;

    tracing::info!(name = %agent_name, api_port, streams_port = cli.port, "Conductor started");

    // 4. Product API + ACP hosting (separate concerns, composed into one server)
    let product_api = api::router(api::ApiState {
        stream_server: stream_server.clone(),
        connection_id: connection_id.clone(),
    });
    let acp_transport = acp_server::router(acp_server::AcpEndpointConfig {
        agent_command: cli.agent_command.clone(),
        stream_server: stream_server.clone(),
        connection_id: connection_id.clone(),
    });
    let combined = Router::new().merge(product_api).merge(acp_transport);
    spawn_api_server(host, api_port, combined).await?;

    // 5. Conductor mode selection.
    //
    // Default: stdio mode. The conductor reads/writes ACP JSON-RPC over stdin/stdout.
    // This works for both:
    //   - TTY usage (human running the binary directly)
    //   - subprocess usage (dashboard/SDK spawns this binary with piped stdin/stdout)
    //
    // WS-only mode: set DURABLE_ACP_WS_ONLY=1 to skip the stdio conductor.
    // The process stays alive serving only /acp WebSocket connections.
    // Use this when the binary is started by a process manager that provides
    // no meaningful stdin (e.g., systemd, Docker without -it).
    let ws_only = std::env::var("DURABLE_ACP_WS_ONLY").is_ok();
    if !ws_only {
        let agent = AcpAgent::from_args(cli.agent_command).context("parse agent command")?;
        let tracer = DurableStreamTracer::start(
            stream_server.clone(),
            stream_server.state_stream.clone(),
        );
        let conductor = ConductorImpl::new_agent(
            "durable-acp".to_string(),
            ProxiesAndAgent::new(agent)
                .proxy(PeerMcpProxy),
            McpBridgeMode::default(),
        )
        .trace_to(tracer);
        let result = conductor
            .run(ByteStreams::new(
                tokio_util::compat::TokioAsyncWriteCompatExt::compat_write(tokio::io::stdout()),
                tokio_util::compat::TokioAsyncReadCompatExt::compat(tokio::io::stdin()),
            ))
            .await
            .context("run conductor");
        let _ = registry::unregister(&agent_name);
        result
    } else {
        tracing::info!("No TTY on stdin — running in WebSocket-only mode (/acp)");
        tokio::signal::ctrl_c().await.ok();
        let _ = registry::unregister(&agent_name);
        Ok(())
    }
}

async fn spawn_api_server(host: IpAddr, port: u16, router: Router) -> Result<()> {
    let listener = TcpListener::bind(SocketAddr::new(host, port)).await?;
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    Ok(())
}
