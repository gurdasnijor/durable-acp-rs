//! Agent configuration manager with rich terminal UI.
//!
//! Usage:
//!   cargo run --bin agents                    # list configured agents
//!   cargo run --bin agents -- add             # interactive registry browser
//!   cargo run --bin agents -- add claude-acp  # add specific agent
//!   cargo run --bin agents -- remove agent-b  # remove by name
//!   cargo run --bin agents -- clear           # remove all

use std::path::PathBuf;

use anyhow::{Result, bail};
use clap::{Parser, Subcommand};
use iocraft::prelude::*;
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
        #[arg(long)]
        name: Option<String>,
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
    std::fs::write(path, toml::to_string_pretty(config)?)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// iocraft components
// ---------------------------------------------------------------------------

fn print_agent_table(agents: &[AgentEntry]) {
    println!(
        "{:<18} {:<8} {:<18} {}",
        "NAME", "PORT", "AGENT", "COMMAND"
    );
    println!("{}", "-".repeat(70));
    for a in agents {
        let cmd = a.command.as_ref().map(|c| c.join(" ")).unwrap_or_default();
        println!(
            "{:<18} {:<8} {:<18} {}",
            a.name,
            a.port,
            a.agent.as_deref().unwrap_or("-"),
            cmd
        );
    }
}

type SharedResult = std::sync::Arc<std::sync::Mutex<Vec<usize>>>;

#[derive(Default, Props)]
struct RegistryPickerProps {
    agents: Vec<RegistryItem>,
    result: Option<SharedResult>,
}

#[derive(Clone)]
struct RegistryItem {
    id: String,
    version: String,
    dist: String,
    desc: String,
    configured: bool,
}

#[component]
fn RegistryPicker(props: &RegistryPickerProps, mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let mut system = hooks.use_context_mut::<SystemContext>();
    let mut cursor = hooks.use_state(|| 0usize);
    let mut selected = hooks.use_state(|| 0u64);
    let mut done = hooks.use_state(|| false);

    let agent_count = props.agents.len();
    let result_arc = props.result.clone();

    hooks.use_terminal_events(move |event| match event {
        TerminalEvent::Key(KeyEvent { code, kind, .. }) if kind != KeyEventKind::Release => {
            match code {
                KeyCode::Up => cursor.set(cursor.get().saturating_sub(1)),
                KeyCode::Down => cursor.set((cursor.get() + 1).min(agent_count - 1)),
                KeyCode::Char(' ') => {
                    selected.set(selected.get() ^ (1 << cursor.get()));
                }
                KeyCode::Enter => {
                    let bits = selected.get();
                    let indices: Vec<usize> = (0..agent_count)
                        .filter(|i| bits & (1 << i) != 0)
                        .collect();
                    if let Some(ref r) = result_arc {
                        *r.lock().unwrap() = indices;
                    }
                    done.set(true);
                }
                KeyCode::Char('q') | KeyCode::Esc => {
                    done.set(true);
                }
                _ => {}
            }
        }
        _ => {}
    });

    if done.get() {
        system.exit();
    }

    let bits = selected.get();

    element! {
        View(flex_direction: FlexDirection::Column, width: 90) {
            View(margin_bottom: 1) {
                Text(content: "Select agents to add (space=toggle, enter=confirm, q=cancel)", color: Color::Grey)
            }
            View(flex_direction: FlexDirection::Column, border_style: BorderStyle::Round, border_color: Color::Blue) {
                View(border_style: BorderStyle::Single, border_edges: Edges::Bottom, border_color: Color::Grey) {
                    View(width: 4) { Text(content: "", weight: Weight::Bold) }
                    View(width: 22pct) { Text(content: "ID", weight: Weight::Bold, color: Color::White) }
                    View(width: 12pct) { Text(content: "Version", weight: Weight::Bold, color: Color::White) }
                    View(width: 6pct) { Text(content: "Type", weight: Weight::Bold, color: Color::White) }
                    View(width: 55pct) { Text(content: "Description", weight: Weight::Bold, color: Color::White) }
                }
                #(props.agents.iter().enumerate().map(|(i, a)| {
                    let is_cursor = i == cursor.get();
                    let is_selected = bits & (1 << i) != 0;
                    let marker = if is_selected { "[x]" } else { "[ ]" };
                    let bg = if is_cursor { Some(Color::DarkBlue) } else if i % 2 == 1 { Some(Color::DarkGrey) } else { None };
                    let tag = if a.configured { " *" } else { "" };
                    element! {
                        View(background_color: bg) {
                            View(width: 4) { Text(content: marker, color: if is_selected { Color::Green } else { Color::Grey }) }
                            View(width: 22pct) { Text(content: format!("{}{}", a.id, tag), color: if a.configured { Color::DarkGrey } else { Color::Cyan }) }
                            View(width: 12pct) { Text(content: a.version.clone(), color: Color::Grey) }
                            View(width: 6pct) { Text(content: a.dist.clone()) }
                            View(width: 55pct) { Text(content: a.desc.clone(), color: Color::Grey) }
                        }
                    }
                }))
            }
            View(margin_top: 1) {
                Text(content: "* = already configured", color: Color::DarkGrey)
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        None => {
            let config = load_config(&cli.config)?;
            if config.agent.is_empty() {
                eprintln!("No agents configured. Run: cargo run --bin agents -- add");
                return Ok(());
            }
            print_agent_table(&config.agent);
            eprintln!("\n{} agent(s) in {}", config.agent.len(), cli.config.display());
        }

        Some(Command::Add { agents, name, port }) => {
            if agents.is_empty() {
                add_interactive(&cli.config).await?;
            } else {
                let mut config = load_config(&cli.config)?;
                for agent_id in &agents {
                    let agent_name = if agents.len() == 1 {
                        name.clone().unwrap_or_else(|| agent_id.clone())
                    } else {
                        agent_id.clone()
                    };
                    if config.agent.iter().any(|a| a.name == agent_name) {
                        eprintln!("'{}' already configured, skipping", agent_name);
                        continue;
                    }
                    let p = if agents.len() == 1 { port } else { None };
                    let p = p.unwrap_or_else(|| next_port(&config));
                    let command = local_command(agent_id);
                    config.agent.push(AgentEntry {
                        name: agent_name.clone(),
                        port: p,
                        agent: Some(agent_id.clone()),
                        command,
                    });
                    eprintln!("+ {} (port {}/{})", agent_name, p, p + 1);
                }
                save_config(&cli.config, &config)?;
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
            eprintln!("- Removed {}", name);
        }

        Some(Command::Clear) => {
            save_config(&cli.config, &Config::default())?;
            eprintln!("Cleared all agents");
        }
    }

    Ok(())
}

async fn add_interactive(config_path: &PathBuf) -> Result<()> {
    eprintln!("Fetching ACP agent registry...");
    let registry = durable_acp_rs::acp_registry::fetch_registry().await?;
    let config = load_config(config_path)?;

    let configured: std::collections::HashSet<&str> = config
        .agent
        .iter()
        .filter_map(|a| a.agent.as_deref())
        .collect();

    let items: Vec<RegistryItem> = registry
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
            let desc = if a.description.len() > 50 {
                format!("{}...", &a.description[..47])
            } else {
                a.description.clone()
            };
            RegistryItem {
                id: a.id.clone(),
                version: a.version.clone(),
                dist: dist.to_string(),
                desc,
                configured: configured.contains(a.id.as_str()),
            }
        })
        .collect();

    let result_arc: SharedResult = std::sync::Arc::new(std::sync::Mutex::new(vec![]));
    smol::block_on(
        element!(RegistryPicker(agents: items.clone(), result: result_arc.clone()))
            .render_loop(),
    )?;
    let result = result_arc.lock().unwrap().clone();

    if result.is_empty() {
        eprintln!("No agents selected.");
        return Ok(());
    }

    let mut config = load_config(config_path)?;
    let mut added = 0;

    for idx in result {
        let item = &items[idx];
        if item.configured {
            eprintln!("~ {} already configured", item.id);
            continue;
        }

        let port = next_port(&config);
        let agent = &registry.agents[idx];

        // Auto-install npx agents
        maybe_install(&item.id, agent).await;
        let command = local_command(&item.id);

        config.agent.push(AgentEntry {
            name: item.id.clone(),
            port,
            agent: Some(item.id.clone()),
            command,
        });

        eprintln!("+ {} (port {}/{})", item.id, port, port + 1);
        added += 1;
    }

    if added > 0 {
        save_config(config_path, &config)?;
        eprintln!("\n{} agent(s) added. Run: cargo run --bin run", added);
    }

    Ok(())
}

fn next_port(config: &Config) -> u16 {
    config.agent.iter().map(|a| a.port).max().unwrap_or(4435) + 2
}

fn local_command(id: &str) -> Option<Vec<String>> {
    let p = PathBuf::from(".agent-bin").join(id).join("run.sh");
    if p.exists() {
        Some(vec![p.canonicalize().unwrap_or(p).to_string_lossy().to_string()])
    } else {
        None
    }
}

async fn maybe_install(id: &str, agent: &durable_acp_rs::acp_registry::RemoteAgent) {
    let agent_dir = PathBuf::from(".agent-bin").join(id);
    if agent_dir.join("run.sh").exists() {
        return;
    }
    if let Some(npx) = &agent.distribution.npx {
        eprint!("  Installing {}...", id);
        let _ = std::io::Write::flush(&mut std::io::stderr());
        let _ = std::fs::create_dir_all(&agent_dir);
        let ok = std::process::Command::new("npm")
            .args(["install", "--prefix", &agent_dir.to_string_lossy(), &npx.package])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if ok {
            let bin_name = npx.package.split('/').last().unwrap_or(id);
            let bin_name = bin_name.split('@').next().unwrap_or(bin_name);
            let args = npx.args.iter().map(|a| format!("\"{a}\"")).collect::<Vec<_>>().join(" ");
            let _ = std::fs::write(
                agent_dir.join("run.sh"),
                format!("#!/bin/bash\nexec \"$(dirname \"$0\")/node_modules/.bin/{bin_name}\" {args} \"$@\"\n"),
            );
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(agent_dir.join("run.sh"), std::fs::Permissions::from_mode(0o755));
            }
            eprintln!(" OK");
        } else {
            eprintln!(" FAILED");
        }
    }
}
