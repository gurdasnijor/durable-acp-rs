//! Multi-agent runner.
//!
//! Reads agents.toml (or CLI args) and starts multiple conductor instances,
//! each wrapping an ACP agent. Resolves agent IDs from the ACP registry.
//!
//! Usage:
//!   cargo run --bin run                           # run all agents from agents.toml
//!   cargo run --bin run -- --config my-agents.toml
//!   cargo run --bin run -- --agent claude-acp     # run a single agent
//!   cargo run --bin run -- --list                 # browse the ACP registry

use std::path::PathBuf;
use std::process::Stdio;

use agent_client_protocol::{self as acp, Agent as _};
use anyhow::{Context, Result, bail};
use clap::Parser;
use serde::Deserialize;
use tokio::signal;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

#[derive(Debug, Parser)]
#[command(name = "run", about = "Run durable-acp agents")]
struct Cli {
    /// Path to agents.toml config file
    #[arg(long, default_value = "agents.toml")]
    config: PathBuf,

    /// Run a single agent by ACP registry ID (overrides config file)
    #[arg(long)]
    agent: Option<String>,

    /// Agent name (used with --agent)
    #[arg(long, default_value = "default")]
    name: String,

    /// Base port (used with --agent)
    #[arg(long, default_value_t = 4437)]
    port: u16,

    /// List available agents from the ACP registry
    #[arg(long)]
    list: bool,
}

#[derive(Debug, Deserialize)]
struct Config {
    agent: Vec<AgentConfig>,
}

#[derive(Debug, Clone, Deserialize)]
struct AgentConfig {
    name: String,
    port: u16,
    /// ACP registry agent ID (e.g. "claude-acp", "gemini", "cline")
    agent: Option<String>,
    /// Raw command (overrides registry lookup)
    command: Option<Vec<String>>,
    #[serde(default = "default_state_stream")]
    state_stream: String,
}

fn default_state_stream() -> String {
    "durable-acp-state".to_string()
}

/// Minimal ACP client that auto-approves permissions and logs notifications.
struct HeadlessClient {
    #[allow(dead_code)]
    name: String,
}

#[async_trait::async_trait(?Send)]
impl acp::Client for HeadlessClient {
    async fn request_permission(
        &self,
        args: acp::RequestPermissionRequest,
    ) -> acp::Result<acp::RequestPermissionResponse> {
        let outcome = if let Some(opt) = args.options.first() {
            acp::RequestPermissionOutcome::Selected(acp::SelectedPermissionOutcome::new(
                opt.option_id.clone(),
            ))
        } else {
            acp::RequestPermissionOutcome::Cancelled
        };
        Ok(acp::RequestPermissionResponse::new(outcome))
    }

    async fn session_notification(
        &self,
        _args: acp::SessionNotification,
    ) -> acp::Result<(), acp::Error> {
        // Notifications are handled by the conductor's proxy chain
        Ok(())
    }

    async fn ext_method(&self, _args: acp::ExtRequest) -> acp::Result<acp::ExtResponse> {
        Err(acp::Error::method_not_found())
    }

    async fn ext_notification(&self, _args: acp::ExtNotification) -> acp::Result<()> {
        Ok(())
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    if cli.list {
        return list_agents().await;
    }

    let agents = if let Some(agent_id) = &cli.agent {
        vec![AgentConfig {
            name: cli.name.clone(),
            port: cli.port,
            agent: Some(agent_id.clone()),
            command: None,
            state_stream: default_state_stream(),
        }]
    } else {
        let config_path = &cli.config;
        if !config_path.exists() {
            bail!(
                "Config file '{}' not found. Create one or use --agent <id>.\n\
                 Run with --list to see available agents.",
                config_path.display()
            );
        }
        let config_str = std::fs::read_to_string(config_path)
            .with_context(|| format!("read {}", config_path.display()))?;
        let config: Config =
            toml::from_str(&config_str).with_context(|| format!("parse {}", config_path.display()))?;
        config.agent
    };

    if agents.is_empty() {
        bail!("No agents configured.");
    }

    eprintln!("Fetching ACP agent registry...");
    let registry = durable_acp_rs::acp_registry::fetch_registry().await?;

    let conductor_bin = std::env::current_exe()?
        .parent()
        .unwrap()
        .join("durable-acp-rs");
    if !conductor_bin.exists() {
        bail!(
            "Conductor binary not found at {}. Run `cargo build` first.",
            conductor_bin.display()
        );
    }

    // Resolve all agent commands
    let mut resolved: Vec<(AgentConfig, Vec<String>)> = Vec::new();
    for agent_config in &agents {
        let command = if let Some(cmd) = &agent_config.command {
            cmd.clone()
        } else if let Some(agent_id) = &agent_config.agent {
            let remote = registry
                .agents
                .iter()
                .find(|a| a.id == *agent_id)
                .with_context(|| {
                    format!("Agent '{}' not found in ACP registry", agent_id)
                })?;
            remote.resolve_command()?
        } else {
            bail!("Agent '{}' needs 'agent' or 'command'", agent_config.name);
        };

        eprintln!(
            "  {} (port {}/{}) -> {}",
            agent_config.name,
            agent_config.port,
            agent_config.port + 1,
            command.join(" ")
        );
        resolved.push((agent_config.clone(), command));
    }

    eprintln!();

    // Spawn each conductor as a subprocess and connect as an ACP client.
    // Uses LocalSet because agent-client-protocol futures are !Send.
    let local_set = tokio::task::LocalSet::new();
    local_set
        .run_until(async {
            let mut handles = Vec::new();

            for (config, command) in &resolved {
                let mut conductor_args = vec![
                    "--name".to_string(),
                    config.name.clone(),
                    "--port".to_string(),
                    config.port.to_string(),
                    "--state-stream".to_string(),
                    config.state_stream.clone(),
                ];
                conductor_args.extend(command.iter().cloned());

                let mut child = tokio::process::Command::new(&conductor_bin)
                    .args(&conductor_args)
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::inherit())
                    .kill_on_drop(true)
                    .spawn()
                    .with_context(|| format!("spawn conductor for '{}'", config.name))?;

                let outgoing = child.stdin.take().unwrap().compat_write();
                let incoming = child.stdout.take().unwrap().compat();
                let name = config.name.clone();

                // Connect as ACP client — this keeps the conductor alive
                let (conn, handle_io) = acp::ClientSideConnection::new(
                    HeadlessClient { name: name.clone() },
                    outgoing,
                    incoming,
                    |fut| {
                        tokio::task::spawn_local(fut);
                    },
                );

                // Run IO in background
                tokio::task::spawn_local(handle_io);

                // Initialize + create session in background
                let handle = tokio::task::spawn_local(async move {
                    conn.initialize(
                        acp::InitializeRequest::new(acp::ProtocolVersion::V1).client_info(
                            acp::Implementation::new("durable-acp-run", "0.1.0")
                                .title("Durable ACP Runner"),
                        ),
                    )
                    .await?;

                    let _session = conn
                        .new_session(acp::NewSessionRequest::new(std::env::current_dir()?))
                        .await?;

                    eprintln!("[{}] Ready (API: http://127.0.0.1:{})", name, 0 /* filled below */);

                    // Keep alive until the connection drops
                    // The conn is held here, keeping the ACP session open
                    let (_tx, rx) = tokio::sync::oneshot::channel::<()>();
                    let _ = rx.await;
                    drop(child);

                    Ok::<_, anyhow::Error>(())
                });

                handles.push((config.name.clone(), handle));
            }

            // Print status
            eprintln!("{} agent(s) started. Ctrl-C to stop.\n", handles.len());
            for config in agents.iter() {
                eprintln!(
                    "  {:<15} API: http://127.0.0.1:{}   Streams: http://127.0.0.1:{}",
                    config.name,
                    config.port + 1,
                    config.port
                );
            }
            eprintln!("\nPeer tools (list_agents, prompt_agent) available in each agent.");
            eprintln!("Submit prompts via: curl -X POST localhost:{}/api/v1/connections/{{id}}/prompt", agents[0].port + 1);
            eprintln!();

            // Wait for Ctrl-C
            signal::ctrl_c().await?;
            eprintln!("\nShutting down...");

            // Abort all tasks
            for (name, handle) in handles {
                eprintln!("  Stopping {}...", name);
                handle.abort();
            }

            // Clean up registry
            for config in &agents {
                let _ = durable_acp_rs::registry::unregister(&config.name);
            }

            Ok::<_, anyhow::Error>(())
        })
        .await
}

async fn list_agents() -> Result<()> {
    let registry = durable_acp_rs::acp_registry::fetch_registry().await?;

    println!(
        "{:<20} {:<12} {:<10} {}",
        "ID", "VERSION", "TYPE", "DESCRIPTION"
    );
    println!("{}", "-".repeat(80));

    for agent in &registry.agents {
        let dist_type = if agent.distribution.npx.is_some() {
            "npx"
        } else if agent.distribution.uvx.is_some() {
            "uvx"
        } else if agent.distribution.binary.is_some() {
            "binary"
        } else {
            "?"
        };

        let desc = if agent.description.len() > 40 {
            format!("{}...", &agent.description[..37])
        } else {
            agent.description.clone()
        };

        println!(
            "{:<20} {:<12} {:<10} {}",
            agent.id, agent.version, dist_type, desc
        );
    }

    println!("\nUse in agents.toml:");
    println!("  [[agent]]");
    println!("  name = \"my-agent\"");
    println!("  port = 4437");
    println!("  agent = \"claude-acp\"  # use any ID from above");

    Ok(())
}
