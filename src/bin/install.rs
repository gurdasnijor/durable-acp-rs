//! Interactive agent installer.
//!
//! Fetches the ACP registry, lets you pick agents with a checkbox UI,
//! installs them locally to .agent-bin/, and updates agents.toml.
//!
//! Usage:
//!   cargo run --bin install              # interactive multi-select
//!   cargo run --bin install -- claude-acp gemini  # install by ID
//!   cargo run --bin install -- --list    # show installed agents

use std::io::Write as _;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::Parser;
use console::style;
use dialoguer::MultiSelect;
use serde::Deserialize;

#[derive(Debug, Parser)]
#[command(name = "install", about = "Install ACP agents locally")]
struct Cli {
    /// Agent IDs to install (non-interactive)
    agents: Vec<String>,

    /// List installed agents
    #[arg(long)]
    list: bool,

    /// Install directory
    #[arg(long)]
    dir: Option<PathBuf>,
}

fn install_dir(cli_dir: &Option<PathBuf>) -> PathBuf {
    cli_dir
        .clone()
        .unwrap_or_else(|| PathBuf::from(".agent-bin"))
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let dir = install_dir(&cli.dir);

    if cli.list {
        return list_installed(&dir);
    }

    eprintln!("{}", style("Fetching ACP agent registry...").dim());
    let registry = durable_acp_rs::acp_registry::fetch_registry().await?;

    let selected = if cli.agents.is_empty() {
        interactive_select(&registry, &dir)?
    } else {
        for id in &cli.agents {
            if !registry.agents.iter().any(|a| a.id == *id) {
                bail!("Unknown agent '{}'. Run without args to browse.", id);
            }
        }
        cli.agents.clone()
    };

    if selected.is_empty() {
        eprintln!("No agents selected.");
        return Ok(());
    }

    std::fs::create_dir_all(&dir)?;
    eprintln!();

    let mut installed = Vec::new();
    for id in &selected {
        let agent = registry.agents.iter().find(|a| a.id == *id).unwrap();
        if install_agent(&dir, agent).await? {
            installed.push(id.clone());
        }
    }

    if installed.is_empty() {
        return Ok(());
    }

    // Update agents.toml
    let toml_path = PathBuf::from("agents.toml");
    update_agents_toml(&toml_path, &dir, &installed, &registry)?;

    eprintln!(
        "\n{}",
        style(format!(
            "Installed {} agent(s). Run: cargo run --bin run",
            installed.len()
        ))
        .green()
        .bold()
    );

    Ok(())
}

fn interactive_select(
    registry: &durable_acp_rs::acp_registry::RemoteRegistry,
    install_dir: &PathBuf,
) -> Result<Vec<String>> {
    // Check which are already installed
    let installed: std::collections::HashSet<String> = if install_dir.exists() {
        std::fs::read_dir(install_dir)?
            .filter_map(|e| e.ok())
            .filter(|e| e.path().join("agent.json").exists())
            .filter_map(|e| e.file_name().into_string().ok())
            .collect()
    } else {
        std::collections::HashSet::new()
    };

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

            let status = if installed.contains(&a.id) {
                " [installed]"
            } else {
                ""
            };

            let desc = if a.description.len() > 40 {
                format!("{}...", &a.description[..37])
            } else {
                a.description.clone()
            };

            format!(
                "{:<18} {:>5}  {:<3}  {}{}",
                a.id, a.version, dist, desc, status
            )
        })
        .collect();

    let selections = MultiSelect::new()
        .with_prompt("Select agents to install (space to toggle, enter to confirm)")
        .items(&items)
        .interact()?;

    let selected: Vec<String> = selections
        .into_iter()
        .map(|i| registry.agents[i].id.clone())
        .collect();

    let (already, new): (Vec<_>, Vec<_>) =
        selected.into_iter().partition(|id| installed.contains(id));

    if !already.is_empty() {
        eprintln!(
            "\n{} Already installed: {}",
            style("✓").green(),
            already.join(", ")
        );
    }

    Ok(new)
}

async fn install_agent(
    dir: &PathBuf,
    agent: &durable_acp_rs::acp_registry::RemoteAgent,
) -> Result<bool> {
    let agent_dir = dir.join(&agent.id);
    std::fs::create_dir_all(&agent_dir)?;

    // Write metadata
    let meta = serde_json::json!({
        "id": agent.id,
        "name": agent.name,
        "version": agent.version,
        "description": agent.description,
    });
    std::fs::write(
        agent_dir.join("agent.json"),
        serde_json::to_string_pretty(&meta)?,
    )?;

    if let Some(npx) = &agent.distribution.npx {
        eprint!(
            "  {} {}...",
            style("Installing").cyan(),
            style(&agent.id).bold()
        );
        std::io::stderr().flush()?;

        let status = std::process::Command::new("npm")
            .args([
                "install",
                "--prefix",
                &agent_dir.to_string_lossy(),
                &npx.package,
            ])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .context("run npm install")?;

        if !status.success() {
            eprintln!(" {}", style("FAILED").red());
            return Ok(false);
        }

        // Create launcher script
        let bin_name = npx.package.split('/').last().unwrap_or(&agent.id);
        let bin_name = bin_name.split('@').next().unwrap_or(bin_name);
        let launcher = agent_dir.join("run.sh");
        let args_str = npx
            .args
            .iter()
            .map(|a| format!("\"{}\"", a))
            .collect::<Vec<_>>()
            .join(" ");
        std::fs::write(
            &launcher,
            format!(
                "#!/bin/bash\nexec \"$(dirname \"$0\")/node_modules/.bin/{bin_name}\" {args_str} \"$@\"\n"
            ),
        )?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&launcher, std::fs::Permissions::from_mode(0o755))?;
        }

        eprintln!(" {}", style("OK").green());
        return Ok(true);
    }

    if let Some(uvx) = &agent.distribution.uvx {
        eprint!(
            "  {} {}...",
            style("Installing").cyan(),
            style(&agent.id).bold()
        );
        std::io::stderr().flush()?;

        let status = std::process::Command::new("uv")
            .args(["tool", "install", &uvx.package])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();

        match status {
            Ok(s) if s.success() => {
                eprintln!(" {}", style("OK").green());
                return Ok(true);
            }
            _ => {
                eprintln!(" {} (uv not available)", style("SKIP").yellow());
                return Ok(false);
            }
        }
    }

    if let Some(binaries) = &agent.distribution.binary {
        let platform = durable_acp_rs::acp_registry::current_platform();
        if let Some(bin) = binaries.get(&platform) {
            if let Some(archive_url) = &bin.archive {
                eprint!(
                    "  {} {} ({})...",
                    style("Downloading").cyan(),
                    style(&agent.id).bold(),
                    platform
                );
                std::io::stderr().flush()?;

                let client = reqwest::Client::new();
                let resp = client.get(archive_url).send().await?;
                let bytes = resp.bytes().await?;

                let archive_path = agent_dir.join("archive");
                std::fs::write(&archive_path, &bytes)?;

                if archive_url.ends_with(".tar.gz") || archive_url.ends_with(".tgz") {
                    let status = std::process::Command::new("tar")
                        .args([
                            "xzf",
                            &archive_path.to_string_lossy(),
                            "-C",
                            &agent_dir.to_string_lossy(),
                        ])
                        .status()?;
                    if !status.success() {
                        eprintln!(" {}", style("FAILED").red());
                        return Ok(false);
                    }
                } else if archive_url.ends_with(".zip") {
                    let status = std::process::Command::new("unzip")
                        .args([
                            "-o",
                            &archive_path.to_string_lossy().to_string(),
                            "-d",
                            &agent_dir.to_string_lossy().to_string(),
                        ])
                        .stdout(std::process::Stdio::null())
                        .status()?;
                    if !status.success() {
                        eprintln!(" {}", style("FAILED").red());
                        return Ok(false);
                    }
                }

                std::fs::remove_file(&archive_path).ok();
                eprintln!(" {}", style("OK").green());
                return Ok(true);
            }
        } else {
            eprintln!(
                "  {} {} (no binary for {})",
                style("Skip").yellow(),
                agent.id,
                platform
            );
        }
    }

    Ok(false)
}

fn update_agents_toml(
    path: &PathBuf,
    install_dir: &PathBuf,
    selected: &[String],
    registry: &durable_acp_rs::acp_registry::RemoteRegistry,
) -> Result<()> {
    let existing: Vec<AgentTomlEntry> = if path.exists() {
        let content = std::fs::read_to_string(path)?;
        let config: TomlConfig = toml::from_str(&content).unwrap_or_default();
        config.agent
    } else {
        vec![]
    };

    let existing_names: std::collections::HashSet<&str> =
        existing.iter().map(|a| a.name.as_str()).collect();
    let max_port = existing.iter().map(|a| a.port).max().unwrap_or(4435);

    let mut new_entries = Vec::new();
    let mut next_port = max_port + 2;

    for id in selected {
        if existing_names.contains(id.as_str()) {
            continue;
        }

        let agent = registry.agents.iter().find(|a| a.id == *id).unwrap();
        let agent_dir = install_dir.join(id);

        let command = if agent_dir.join("run.sh").exists() {
            vec![agent_dir
                .canonicalize()
                .unwrap_or(agent_dir.clone())
                .join("run.sh")
                .to_string_lossy()
                .to_string()]
        } else {
            agent.resolve_command()?
        };

        new_entries.push((id.clone(), next_port, command));
        next_port += 2;
    }

    if new_entries.is_empty() {
        return Ok(());
    }

    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;

    if existing.is_empty() && !path.exists() {
        writeln!(file, "# Durable ACP agent configuration")?;
        writeln!(file)?;
    }

    for (name, port, command) in &new_entries {
        writeln!(file)?;
        writeln!(file, "[[agent]]")?;
        writeln!(file, "name = \"{}\"", name)?;
        writeln!(file, "port = {}", port)?;
        let cmd_str = command
            .iter()
            .map(|c| format!("\"{}\"", c))
            .collect::<Vec<_>>()
            .join(", ");
        writeln!(file, "command = [{}]", cmd_str)?;
    }

    eprintln!(
        "\n{} Updated agents.toml with {} new agent(s).",
        style("+").green(),
        new_entries.len()
    );
    Ok(())
}

#[derive(Debug, Deserialize, Default)]
struct TomlConfig {
    #[serde(default)]
    agent: Vec<AgentTomlEntry>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct AgentTomlEntry {
    name: String,
    port: u16,
    agent: Option<String>,
    command: Option<Vec<String>>,
}

fn list_installed(dir: &PathBuf) -> Result<()> {
    if !dir.exists() {
        eprintln!(
            "{}",
            style("No agents installed. Run: cargo run --bin install").dim()
        );
        return Ok(());
    }

    println!(
        "{:<20} {:<12} {}",
        style("ID").bold(),
        style("VERSION").bold(),
        style("PATH").bold()
    );
    println!("{}", "-".repeat(60));

    let mut count = 0;
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let meta_path = entry.path().join("agent.json");
        if meta_path.exists() {
            let meta: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(&meta_path)?)?;
            println!(
                "{:<20} {:<12} {}",
                style(meta["id"].as_str().unwrap_or("?")).green(),
                meta["version"].as_str().unwrap_or("?"),
                style(entry.path().display()).dim()
            );
            count += 1;
        }
    }

    if count == 0 {
        eprintln!(
            "{}",
            style("No agents installed. Run: cargo run --bin install").dim()
        );
    }

    Ok(())
}
