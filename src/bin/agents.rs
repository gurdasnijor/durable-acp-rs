//! Agent configuration manager for agents.toml.
//!
//! Usage:
//!   cargo run --bin agents                    # list configured agents
//!   cargo run --bin agents -- add claude-acp  # add from ACP registry
//!   cargo run --bin agents -- add claude-acp --name my-claude --port 4441
//!   cargo run --bin agents -- remove agent-b  # remove by name
//!   cargo run --bin agents -- clear           # remove all agents

use std::path::PathBuf;

use anyhow::{Result, bail};
use clap::{Parser, Subcommand};
use console::style;
use serde::{Deserialize, Serialize};

#[derive(Debug, Parser)]
#[command(name = "agents", about = "Manage agents.toml configuration")]
struct Cli {
    /// Path to agents.toml
    #[arg(long, default_value = "agents.toml")]
    config: PathBuf,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Add an agent (by ACP registry ID or raw command)
    Add {
        /// ACP registry ID (e.g. claude-acp, gemini, cline)
        agent: String,
        /// Agent name (defaults to registry ID)
        #[arg(long)]
        name: Option<String>,
        /// Port (auto-assigned if omitted)
        #[arg(long)]
        port: Option<u16>,
    },
    /// Remove an agent by name
    Remove {
        /// Agent name to remove
        name: String,
    },
    /// Remove all agents
    Clear,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AgentEntry {
    name: String,
    port: u16,
    #[serde(skip_serializing_if = "Option::is_none")]
    agent: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    command: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    state_stream: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Default)]
struct Config {
    #[serde(default)]
    agent: Vec<AgentEntry>,
}

fn load_config(path: &PathBuf) -> Result<Config> {
    if !path.exists() {
        return Ok(Config::default());
    }
    let content = std::fs::read_to_string(path)?;
    Ok(toml::from_str(&content)?)
}

fn save_config(path: &PathBuf, config: &Config) -> Result<()> {
    let content = toml::to_string_pretty(config)?;
    std::fs::write(path, content)?;
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let config_path = cli.config;

    match cli.command {
        None => {
            // List configured agents
            let config = load_config(&config_path)?;
            if config.agent.is_empty() {
                eprintln!("{}", style("No agents configured.").dim());
                eprintln!("  Add one:  cargo run --bin agents -- add claude-acp");
                eprintln!("  Browse:   cargo run --bin run -- --list");
                return Ok(());
            }
            println!(
                "{:<18} {:<8} {:<18} {}",
                style("NAME").bold(),
                style("PORT").bold(),
                style("AGENT").bold(),
                style("COMMAND").bold()
            );
            println!("{}", "-".repeat(70));
            for agent in &config.agent {
                let agent_id = agent.agent.as_deref().unwrap_or("-");
                let cmd = agent
                    .command
                    .as_ref()
                    .map(|c| c.join(" "))
                    .unwrap_or_default();
                println!(
                    "{:<18} {:<8} {:<18} {}",
                    style(&agent.name).green(),
                    agent.port,
                    agent_id,
                    style(&cmd).dim()
                );
            }
            eprintln!(
                "\n{} agent(s) configured in {}",
                config.agent.len(),
                config_path.display()
            );
        }

        Some(Command::Add { agent, name, port }) => {
            let mut config = load_config(&config_path)?;
            let agent_name = name.unwrap_or_else(|| agent.clone());

            // Check for duplicate name
            if config.agent.iter().any(|a| a.name == agent_name) {
                bail!(
                    "Agent '{}' already exists. Remove it first: cargo run --bin agents -- remove {}",
                    agent_name,
                    agent_name
                );
            }

            // Auto-assign port
            let port = port.unwrap_or_else(|| {
                let max = config.agent.iter().map(|a| a.port).max().unwrap_or(4435);
                max + 2
            });

            // Check if it's an installed local agent
            let agent_bin_dir = PathBuf::from(".agent-bin").join(&agent);
            let command = if agent_bin_dir.join("run.sh").exists() {
                Some(vec![agent_bin_dir
                    .canonicalize()
                    .unwrap_or(agent_bin_dir)
                    .join("run.sh")
                    .to_string_lossy()
                    .to_string()])
            } else {
                None
            };

            config.agent.push(AgentEntry {
                name: agent_name.clone(),
                port,
                agent: Some(agent.clone()),
                command,
                state_stream: None,
            });

            save_config(&config_path, &config)?;
            eprintln!(
                "{} Added {} (port {}/{}, agent: {})",
                style("+").green().bold(),
                style(&agent_name).green(),
                port,
                port + 1,
                agent
            );
        }

        Some(Command::Remove { name }) => {
            let mut config = load_config(&config_path)?;
            let before = config.agent.len();
            config.agent.retain(|a| a.name != name);
            if config.agent.len() == before {
                bail!("Agent '{}' not found in {}", name, config_path.display());
            }
            save_config(&config_path, &config)?;
            eprintln!(
                "{} Removed {}",
                style("-").red().bold(),
                style(&name).red()
            );
        }

        Some(Command::Clear) => {
            let config = Config::default();
            save_config(&config_path, &config)?;
            eprintln!("{} Cleared all agents", style("x").red().bold());
        }
    }

    Ok(())
}
