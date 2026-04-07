//! Multi-agent runner.
//!
//! Reads agents.toml (or CLI args) and starts multiple conductor instances,
//! each wrapping an ACP agent. Resolves agent IDs from the ACP registry.
//!
//! Usage:
//!   cargo run --bin run                           # run all agents from agents.toml
//!   cargo run --bin run -- --config my-agents.toml
//!   cargo run --bin run -- --agent claude-acp     # run a single agent (auto-names, auto-ports)

use std::path::PathBuf;
use std::process::Stdio;

use anyhow::{Context, Result, bail};
use clap::Parser;
use serde::Deserialize;
use tokio::signal;

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

#[derive(Debug, Deserialize)]
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

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    if cli.list {
        return list_agents().await;
    }

    let agents = if let Some(agent_id) = &cli.agent {
        // Single agent from CLI
        vec![AgentConfig {
            name: cli.name.clone(),
            port: cli.port,
            agent: Some(agent_id.clone()),
            command: None,
            state_stream: default_state_stream(),
        }]
    } else {
        // Load from config file
        let config_path = &cli.config;
        if !config_path.exists() {
            bail!(
                "Config file '{}' not found. Create one or use --agent <id>.\n\
                 Run with --list to see available agents from the ACP registry.",
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
        bail!("No agents configured. Add [[agent]] entries to agents.toml or use --agent <id>.");
    }

    // Resolve agent commands from registry
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

    let chat_bin = std::env::current_exe()?
        .parent()
        .unwrap()
        .join("chat");

    let mut children = Vec::new();

    for agent_config in &agents {
        let command = if let Some(cmd) = &agent_config.command {
            cmd.clone()
        } else if let Some(agent_id) = &agent_config.agent {
            let remote = registry
                .agents
                .iter()
                .find(|a| a.id == *agent_id)
                .with_context(|| {
                    let ids: Vec<_> = registry.agents.iter().map(|a| a.id.as_str()).collect();
                    format!(
                        "Agent '{}' not found in ACP registry. Available: {:?}",
                        agent_id, ids
                    )
                })?;
            remote
                .resolve_command()
                .with_context(|| format!("resolve command for '{}'", agent_id))?
        } else {
            bail!(
                "Agent '{}' has neither 'agent' nor 'command' specified",
                agent_config.name
            );
        };

        eprintln!(
            "  {} (port {}/{}) -> {}",
            agent_config.name,
            agent_config.port,
            agent_config.port + 1,
            command.join(" ")
        );

        // Build conductor args: --name X --port Y --state-stream Z <agent_command...>
        let mut conductor_args = vec![
            "--name".to_string(),
            agent_config.name.clone(),
            "--port".to_string(),
            agent_config.port.to_string(),
            "--state-stream".to_string(),
            agent_config.state_stream.clone(),
        ];
        conductor_args.extend(command);

        // Spawn: chat -> conductor -> agent
        let mut full_args = vec![conductor_bin.to_string_lossy().to_string()];
        full_args.extend(conductor_args);

        let child = tokio::process::Command::new(&chat_bin)
            .args(&full_args)
            .stdin(Stdio::null())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .with_context(|| format!("spawn agent '{}'", agent_config.name))?;

        children.push((agent_config.name.clone(), child));
    }

    eprintln!("\n{} agent(s) started. Ctrl-C to stop all.\n", children.len());
    eprintln!("Peer tools (list_agents, prompt_agent) are available in each agent's session.");
    eprintln!("Registry: http://127.0.0.1:{}/api/v1/registry\n", agents[0].port + 1);

    // Wait for Ctrl-C
    signal::ctrl_c().await?;
    eprintln!("\nShutting down...");

    for (name, mut child) in children {
        eprintln!("  Stopping {}...", name);
        let _ = child.kill().await;
    }

    // Clean up registry
    for agent_config in &agents {
        let _ = durable_acp_rs::registry::unregister(&agent_config.name);
    }

    Ok(())
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
