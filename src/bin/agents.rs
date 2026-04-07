//! Agent configuration manager.
//!
//! Single tool for managing agents.toml — browse the ACP registry,
//! pick agents, install them, configure ports.
//!
//! Usage:
//!   cargo run --bin agents                    # list configured agents
//!   cargo run --bin agents -- add             # interactive: browse registry + select
//!   cargo run --bin agents -- add claude-acp  # add specific agent by ID
//!   cargo run --bin agents -- remove agent-b  # remove by name
//!   cargo run --bin agents -- clear           # remove all

use std::path::PathBuf;

use anyhow::{Result, bail};
use clap::{Parser, Subcommand};
use console::style;
use dialoguer::MultiSelect;
use serde::{Deserialize, Serialize};

#[derive(Debug, Parser)]
#[command(name = "agents", about = "Manage agents.toml")]
struct Cli {
    #[arg(long, default_value = "agents.toml")]
    config: PathBuf,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Add agents — interactive registry browser or by ID
    Add {
        /// Agent IDs to add (omit for interactive selection)
        agents: Vec<String>,
        /// Custom name (only with a single agent ID)
        #[arg(long)]
        name: Option<String>,
        /// Custom port (auto-assigned if omitted)
        #[arg(long)]
        port: Option<u16>,
    },
    /// Remove an agent by name
    Remove { name: String },
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

    match cli.command {
        None => list_agents(&cli.config),

        Some(Command::Add { agents, name, port }) => {
            if agents.is_empty() {
                // Interactive: fetch registry, show multi-select
                add_interactive(&cli.config).await
            } else {
                // Direct: add by ID
                for (i, agent_id) in agents.iter().enumerate() {
                    let n = if agents.len() == 1 { name.clone() } else { None };
                    let p = if agents.len() == 1 { port } else { None };
                    add_agent(&cli.config, agent_id, n, p, i == 0).await?;
                }
                Ok(())
            }
        }

        Some(Command::Remove { name }) => {
            let mut config = load_config(&cli.config)?;
            let before = config.agent.len();
            config.agent.retain(|a| a.name != name);
            if config.agent.len() == before {
                bail!("Agent '{}' not found", name);
            }
            save_config(&cli.config, &config)?;
            eprintln!("{} Removed {}", style("-").red().bold(), style(&name).red());
            Ok(())
        }

        Some(Command::Clear) => {
            save_config(&cli.config, &Config::default())?;
            eprintln!("{} Cleared all agents", style("x").red().bold());
            Ok(())
        }
    }
}

fn list_agents(config_path: &PathBuf) -> Result<()> {
    let config = load_config(config_path)?;
    if config.agent.is_empty() {
        eprintln!("{}", style("No agents configured.").dim());
        eprintln!("  cargo run --bin agents -- add    # browse ACP registry");
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
    for a in &config.agent {
        println!(
            "{:<18} {:<8} {:<18} {}",
            style(&a.name).green(),
            a.port,
            a.agent.as_deref().unwrap_or("-"),
            style(a.command.as_ref().map(|c| c.join(" ")).unwrap_or_default()).dim()
        );
    }
    eprintln!(
        "\n{} agent(s). Run: cargo run --bin run",
        config.agent.len()
    );
    Ok(())
}

async fn add_interactive(config_path: &PathBuf) -> Result<()> {
    eprintln!("{}", style("Fetching ACP agent registry...").dim());
    let registry = durable_acp_rs::acp_registry::fetch_registry().await?;
    let config = load_config(config_path)?;

    let configured: std::collections::HashSet<&str> =
        config.agent.iter().filter_map(|a| a.agent.as_deref()).collect();

    let items: Vec<String> = registry
        .agents
        .iter()
        .map(|a| {
            let dist = if a.distribution.npx.is_some() {
                "npx"
            } else if a.distribution.uvx.is_some() {
                "uvx"
            } else {
                "bin"
            };
            let tag = if configured.contains(a.id.as_str()) {
                " [configured]"
            } else {
                ""
            };
            let desc = if a.description.len() > 38 {
                format!("{}...", &a.description[..35])
            } else {
                a.description.clone()
            };
            format!(
                "{:<18} {:>7}  {:<3}  {}{}",
                a.id, a.version, dist, desc, tag
            )
        })
        .collect();

    let selections = MultiSelect::new()
        .with_prompt("Select agents to add (space = toggle, enter = confirm)")
        .items(&items)
        .interact()?;

    if selections.is_empty() {
        eprintln!("No agents selected.");
        return Ok(());
    }

    let mut config = load_config(config_path)?;
    let mut added = 0;

    for idx in selections {
        let agent = &registry.agents[idx];
        if configured.contains(agent.id.as_str()) {
            eprintln!(
                "  {} {} already configured",
                style("~").yellow(),
                agent.id
            );
            continue;
        }

        let port = next_port(&config);
        let command = resolve_local_command(&agent.id, &agent)?;

        config.agent.push(AgentEntry {
            name: agent.id.clone(),
            port,
            agent: Some(agent.id.clone()),
            command,
        });

        // Install if npx and not already installed
        maybe_install(&agent.id, &agent).await;

        eprintln!(
            "  {} {} (port {}/{})",
            style("+").green().bold(),
            style(&agent.id).green(),
            port,
            port + 1
        );
        added += 1;
    }

    if added > 0 {
        save_config(config_path, &config)?;
        eprintln!(
            "\n{} {} agent(s) added. Run: cargo run --bin run",
            style("Done.").green().bold(),
            added
        );
    }

    Ok(())
}

async fn add_agent(
    config_path: &PathBuf,
    agent_id: &str,
    name: Option<String>,
    port: Option<u16>,
    _fetch_registry: bool,
) -> Result<()> {
    let mut config = load_config(config_path)?;
    let agent_name = name.unwrap_or_else(|| agent_id.to_string());

    if config.agent.iter().any(|a| a.name == agent_name) {
        bail!("'{}' already configured. Use: agents remove {}", agent_name, agent_name);
    }

    let port = port.unwrap_or_else(|| next_port(&config));

    // Check for local install
    let agent_bin = PathBuf::from(".agent-bin").join(agent_id);
    let command = if agent_bin.join("run.sh").exists() {
        Some(vec![agent_bin
            .canonicalize()
            .unwrap_or(agent_bin)
            .join("run.sh")
            .to_string_lossy()
            .to_string()])
    } else {
        None
    };

    config.agent.push(AgentEntry {
        name: agent_name.clone(),
        port,
        agent: Some(agent_id.to_string()),
        command,
    });

    save_config(config_path, &config)?;
    eprintln!(
        "{} Added {} (port {}/{}, agent: {})",
        style("+").green().bold(),
        style(&agent_name).green(),
        port,
        port + 1,
        agent_id
    );
    Ok(())
}

fn next_port(config: &Config) -> u16 {
    let max = config.agent.iter().map(|a| a.port).max().unwrap_or(4435);
    max + 2
}

fn resolve_local_command(
    id: &str,
    _agent: &durable_acp_rs::acp_registry::RemoteAgent,
) -> Result<Option<Vec<String>>> {
    let agent_bin = PathBuf::from(".agent-bin").join(id);
    if agent_bin.join("run.sh").exists() {
        Ok(Some(vec![agent_bin
            .canonicalize()
            .unwrap_or(agent_bin)
            .join("run.sh")
            .to_string_lossy()
            .to_string()]))
    } else {
        Ok(None)
    }
}

async fn maybe_install(id: &str, agent: &durable_acp_rs::acp_registry::RemoteAgent) {
    let agent_dir = PathBuf::from(".agent-bin").join(id);
    if agent_dir.join("run.sh").exists() || agent_dir.join("agent.json").exists() {
        return; // already installed
    }

    if let Some(npx) = &agent.distribution.npx {
        eprint!(
            "  {} {}...",
            style("Installing").cyan(),
            style(id).bold()
        );
        std::io::Write::flush(&mut std::io::stderr()).ok();

        let _ = std::fs::create_dir_all(&agent_dir);
        let status = std::process::Command::new("npm")
            .args(["install", "--prefix", &agent_dir.to_string_lossy(), &npx.package])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();

        match status {
            Ok(s) if s.success() => {
                // Create launcher
                let bin_name = npx.package.split('/').last().unwrap_or(id);
                let bin_name = bin_name.split('@').next().unwrap_or(bin_name);
                let args_str = npx
                    .args
                    .iter()
                    .map(|a| format!("\"{}\"", a))
                    .collect::<Vec<_>>()
                    .join(" ");
                let launcher = agent_dir.join("run.sh");
                let _ = std::fs::write(
                    &launcher,
                    format!("#!/bin/bash\nexec \"$(dirname \"$0\")/node_modules/.bin/{bin_name}\" {args_str} \"$@\"\n"),
                );
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let _ = std::fs::set_permissions(&launcher, std::fs::Permissions::from_mode(0o755));
                }
                eprintln!(" {}", style("OK").green());
            }
            _ => eprintln!(" {}", style("SKIP").yellow()),
        }
    }
}
