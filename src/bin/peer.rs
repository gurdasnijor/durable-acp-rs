//! Agent-to-agent messaging CLI.
//!
//! Discovers peer agents via the local registry, submits a prompt to a peer's
//! REST API, and streams the response to stdout.
//!
//! Usage:
//!   peer list                              # list registered agents
//!   peer prompt --agent agent-b "hello"    # send prompt, stream response

use std::io::Write as _;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use durable_acp_rs::registry;
use durable_acp_rs::state::ChunkRow;

#[derive(Debug, Parser)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// List registered agents
    List,
    /// Send a prompt to a peer agent and stream the response
    Prompt {
        /// Agent name (from registry)
        #[arg(long)]
        agent: String,
        /// Prompt text
        text: Vec<String>,
        /// Timeout in seconds
        #[arg(long, default_value_t = 120)]
        timeout: u64,
    },
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct ConnectionInfo {
    logical_connection_id: String,
    latest_session_id: Option<String>,
    state: String,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct PromptResult {
    prompt_turn_id: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Command::List => {
            let reg = registry::read_registry()?;
            if reg.agents.is_empty() {
                eprintln!("No agents registered.");
                eprintln!("Start a conductor with: cargo run --bin durable-acp-rs -- --name my-agent npx @agentclientprotocol/claude-agent-acp");
                return Ok(());
            }
            println!("{:<20} {:<35} {}", "NAME", "API URL", "CONNECTION ID");
            for agent in &reg.agents {
                println!(
                    "{:<20} {:<35} {}",
                    agent.name,
                    agent.api_url,
                    &agent.logical_connection_id[..8]
                );
            }
            Ok(())
        }
        Command::Prompt {
            agent,
            text,
            timeout,
        } => {
            let text = text.join(" ");
            if text.is_empty() {
                bail!("No prompt text provided");
            }
            prompt_agent(&agent, &text, timeout).await
        }
    }
}

async fn prompt_agent(name: &str, text: &str, timeout_secs: u64) -> Result<()> {
    let reg = registry::read_registry()?;
    let entry = reg
        .agents
        .iter()
        .find(|a| a.name == name)
        .with_context(|| {
            let names: Vec<_> = reg.agents.iter().map(|a| a.name.as_str()).collect();
            format!("Agent '{}' not found. Available: {:?}", name, names)
        })?;

    let client = reqwest::Client::new();
    let api_url = &entry.api_url;

    // 1. Find active connection + session
    let connections: Vec<ConnectionInfo> = client
        .get(format!("{api_url}/api/v1/connections"))
        .send()
        .await?
        .json()
        .await?;

    let conn = connections
        .iter()
        .find(|c| c.state == "attached")
        .or(connections.first())
        .context("No connections on peer agent")?;

    let session_id = conn
        .latest_session_id
        .as_deref()
        .context("Peer has no active session")?;

    eprintln!(
        "[peer] Sending to {} ({})",
        name,
        &conn.logical_connection_id[..8]
    );

    // 2. Submit prompt
    let result: PromptResult = client
        .post(format!(
            "{api_url}/api/v1/connections/{}/prompt",
            conn.logical_connection_id
        ))
        .json(&serde_json::json!({
            "sessionId": session_id,
            "text": text,
        }))
        .send()
        .await?
        .json()
        .await?;

    eprintln!("[peer] Turn: {}", &result.prompt_turn_id[..8]);

    // 3. Stream response via SSE
    let response = client
        .get(format!(
            "{api_url}/api/v1/prompt-turns/{}/stream",
            result.prompt_turn_id
        ))
        .timeout(std::time::Duration::from_secs(timeout_secs))
        .send()
        .await?;

    let mut stdout = std::io::BufWriter::new(std::io::stdout().lock());

    let mut buffer = String::new();
    let mut stream = response.bytes_stream();
    use futures::StreamExt;
    while let Some(chunk) = stream.next().await {
        let bytes = chunk?;
        buffer.push_str(&String::from_utf8_lossy(&bytes));

        // Parse SSE events from the buffer
        while let Some(event_end) = buffer.find("\n\n") {
            let event = buffer[..event_end].to_string();
            buffer = buffer[event_end + 2..].to_string();

            for line in event.lines() {
                if let Some(data) = line.strip_prefix("data:") {
                    let data = data.trim();
                    if data.is_empty() {
                        continue;
                    }
                    if let Ok(chunk) = serde_json::from_str::<ChunkRow>(data) {
                        match chunk.chunk_type {
                            durable_acp_rs::state::ChunkType::Text => {
                                write!(stdout, "{}", chunk.content)?;
                                stdout.flush()?;
                            }
                            durable_acp_rs::state::ChunkType::ToolCall => {
                                eprintln!("[tool] {}", chunk.content);
                            }
                            durable_acp_rs::state::ChunkType::Thinking => {
                                eprint!(".");
                            }
                            durable_acp_rs::state::ChunkType::Stop => {
                                writeln!(stdout)?;
                                return Ok(());
                            }
                            durable_acp_rs::state::ChunkType::Error => {
                                eprintln!("\n[error] {}", chunk.content);
                                bail!("Agent returned error");
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
    }

    writeln!(stdout)?;
    Ok(())
}
