//! Interactive agent installer.
//!
//! Fetches the ACP registry, lets you pick agents, and installs them locally
//! so they boot fast (no npx/uvx fetch on every start).
//!
//! Usage:
//!   cargo run --bin install              # interactive select
//!   cargo run --bin install -- claude-acp gemini  # install specific agents
//!   cargo run --bin install -- --list    # show installed agents

use std::io::Write as _;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::Parser;

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

    eprintln!("Fetching ACP agent registry...");
    let registry = durable_acp_rs::acp_registry::fetch_registry().await?;

    let selected = if cli.agents.is_empty() {
        interactive_select(&registry)?
    } else {
        // Validate IDs
        for id in &cli.agents {
            if !registry.agents.iter().any(|a| a.id == *id) {
                bail!("Unknown agent '{}'. Run with --list or no args to browse.", id);
            }
        }
        cli.agents.clone()
    };

    if selected.is_empty() {
        eprintln!("No agents selected.");
        return Ok(());
    }

    std::fs::create_dir_all(&dir)?;

    for id in &selected {
        let agent = registry.agents.iter().find(|a| a.id == *id).unwrap();
        install_agent(&dir, agent).await?;
    }

    eprintln!("\nInstalled {} agent(s) to {}/", selected.len(), dir.display());
    eprintln!("Add to agents.toml:");
    for id in &selected {
        let agent = registry.agents.iter().find(|a| a.id == *id).unwrap();
        let cmd = agent.resolve_command()?;
        eprintln!("  [[agent]]");
        eprintln!("  name = \"{}\"", id);
        eprintln!("  port = 4437");
        eprintln!("  command = {:?}", cmd);
        eprintln!();
    }

    Ok(())
}

fn interactive_select(
    registry: &durable_acp_rs::acp_registry::RemoteRegistry,
) -> Result<Vec<String>> {
    let stdin = std::io::stdin();

    eprintln!("\nAvailable ACP agents:\n");
    for (i, agent) in registry.agents.iter().enumerate() {
        let dist = if agent.distribution.npx.is_some() {
            "npx"
        } else if agent.distribution.uvx.is_some() {
            "uvx"
        } else {
            "bin"
        };

        let desc = if agent.description.len() > 45 {
            format!("{}...", &agent.description[..42])
        } else {
            agent.description.clone()
        };

        eprintln!(
            "  {:>2}) {:<20} [{:<3}] {}",
            i + 1,
            agent.id,
            dist,
            desc
        );
    }

    eprintln!();
    eprint!("Enter numbers separated by spaces (e.g. 1 4 5), or 'all': ");
    std::io::stderr().flush()?;

    let mut input = String::new();
    stdin.read_line(&mut input)?;
    let input = input.trim();

    if input.is_empty() {
        return Ok(vec![]);
    }

    if input == "all" {
        return Ok(registry.agents.iter().map(|a| a.id.clone()).collect());
    }

    let mut selected = Vec::new();
    for part in input.split_whitespace() {
        if let Ok(n) = part.parse::<usize>() {
            if n >= 1 && n <= registry.agents.len() {
                selected.push(registry.agents[n - 1].id.clone());
            } else {
                eprintln!("  Skipping invalid number: {}", n);
            }
        } else if registry.agents.iter().any(|a| a.id == part) {
            selected.push(part.to_string());
        } else {
            eprintln!("  Skipping unknown: {}", part);
        }
    }

    Ok(selected)
}

async fn install_agent(
    dir: &PathBuf,
    agent: &durable_acp_rs::acp_registry::RemoteAgent,
) -> Result<()> {
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

    // For npx agents: pre-install the package
    if let Some(npx) = &agent.distribution.npx {
        eprint!("  Installing {} (npm)...", agent.id);
        std::io::stderr().flush()?;

        let status = std::process::Command::new("npm")
            .args(["install", "--prefix", &agent_dir.to_string_lossy(), &npx.package])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .context("run npm install")?;

        if !status.success() {
            eprintln!(" FAILED (npm exit {})", status.code().unwrap_or(-1));
            return Ok(());
        }

        // Write a launcher script
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

        eprintln!(" OK ({})", npx.package);
    } else if let Some(uvx) = &agent.distribution.uvx {
        eprint!("  Installing {} (pip)...", agent.id);
        std::io::stderr().flush()?;

        let status = std::process::Command::new("uv")
            .args(["tool", "install", &uvx.package])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();

        match status {
            Ok(s) if s.success() => eprintln!(" OK ({})", uvx.package),
            _ => eprintln!(" SKIP (uv not available or install failed)"),
        }
    } else if let Some(binaries) = &agent.distribution.binary {
        let platform = durable_acp_rs::acp_registry::current_platform();
        if let Some(bin) = binaries.get(&platform) {
            if let Some(archive_url) = &bin.archive {
                eprint!("  Downloading {} ({})...", agent.id, platform);
                std::io::stderr().flush()?;

                let client = reqwest::Client::new();
                let resp = client.get(archive_url).send().await?;
                let bytes = resp.bytes().await?;

                let archive_path = agent_dir.join("archive");
                std::fs::write(&archive_path, &bytes)?;

                // Extract based on extension
                if archive_url.ends_with(".tar.gz") || archive_url.ends_with(".tgz") {
                    let status = std::process::Command::new("tar")
                        .args(["xzf", &archive_path.to_string_lossy(), "-C", &agent_dir.to_string_lossy()])
                        .status()?;
                    if !status.success() {
                        eprintln!(" FAILED (tar)");
                        return Ok(());
                    }
                } else if archive_url.ends_with(".zip") {
                    let status = std::process::Command::new("unzip")
                        .args(["-o", &archive_path.to_string_lossy().to_string(), "-d", &agent_dir.to_string_lossy().to_string()])
                        .stdout(std::process::Stdio::null())
                        .status()?;
                    if !status.success() {
                        eprintln!(" FAILED (unzip)");
                        return Ok(());
                    }
                }

                std::fs::remove_file(&archive_path).ok();
                eprintln!(" OK");
            }
        } else {
            eprintln!("  Skipping {} (no binary for {})", agent.id, platform);
        }
    }

    Ok(())
}

fn list_installed(dir: &PathBuf) -> Result<()> {
    if !dir.exists() {
        eprintln!("No agents installed (directory {} doesn't exist).", dir.display());
        eprintln!("Run: cargo run --bin install");
        return Ok(());
    }

    println!("{:<20} {:<12} {}", "ID", "VERSION", "PATH");
    println!("{}", "-".repeat(60));

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
                meta["id"].as_str().unwrap_or("?"),
                meta["version"].as_str().unwrap_or("?"),
                entry.path().display()
            );
        }
    }

    Ok(())
}
