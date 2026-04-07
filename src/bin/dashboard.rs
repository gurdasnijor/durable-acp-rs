//! Multi-agent TUI dashboard.
//!
//! Starts agents from agents.toml, shows status, lets you prompt any agent
//! and streams responses in real-time.
//!
//! Usage:
//!   cargo run --bin dashboard
//!   cargo run --bin dashboard -- --agent claude-acp

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, Mutex};

use agent_client_protocol::{self as acp, Agent as _};
use anyhow::{Context, Result, bail};
use clap::Parser as ClapParser;
use iocraft::prelude::*;
use serde::Deserialize;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

// ---------------------------------------------------------------------------
// CLI
// ---------------------------------------------------------------------------

#[derive(Debug, ClapParser)]
#[command(name = "dashboard", about = "Multi-agent TUI dashboard")]
struct Cli {
    #[arg(long, default_value = "agents.toml")]
    config: PathBuf,
    #[arg(long)]
    agent: Option<String>,
    #[arg(long, default_value = "default")]
    name: String,
    #[arg(long, default_value_t = 4437)]
    port: u16,
}

#[derive(Debug, Clone, Deserialize)]
struct AgentConfig {
    name: String,
    port: u16,
    agent: Option<String>,
    command: Option<Vec<String>>,
    #[serde(default = "default_state_stream")]
    state_stream: String,
}

#[derive(Debug, Deserialize)]
struct Config {
    agent: Vec<AgentConfig>,
}

fn default_state_stream() -> String {
    "durable-acp-state".to_string()
}

// ---------------------------------------------------------------------------
// Shared state between TUI and async tasks
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
struct AgentStatus {
    name: String,
    port: u16,
    state: String, // "starting", "ready", "error"
}

#[derive(Clone, Debug)]
struct OutputLine {
    agent: String,
    text: String,
}

#[derive(Clone)]
struct DashState {
    agents: Arc<Mutex<Vec<AgentStatus>>>,
    output: Arc<Mutex<Vec<OutputLine>>>,
    input_tx: tokio::sync::mpsc::UnboundedSender<(String, String)>, // (agent_name, text)
}

impl DashState {
    fn new(input_tx: tokio::sync::mpsc::UnboundedSender<(String, String)>) -> Self {
        Self {
            agents: Arc::new(Mutex::new(Vec::new())),
            output: Arc::new(Mutex::new(Vec::new())),
            input_tx,
        }
    }

    fn set_agent_state(&self, name: &str, state: &str) {
        let mut agents = self.agents.lock().unwrap();
        if let Some(a) = agents.iter_mut().find(|a| a.name == name) {
            a.state = state.to_string();
        }
    }

    fn push_output(&self, agent: &str, text: &str) {
        let mut output = self.output.lock().unwrap();
        // Append to last line if same agent and not newline-terminated
        let should_append = output
            .last()
            .map(|l| l.agent == agent && !l.text.ends_with('\n'))
            .unwrap_or(false);
        if should_append {
            output.last_mut().unwrap().text.push_str(text);
        } else {
            output.push(OutputLine {
                agent: agent.to_string(),
                text: text.to_string(),
            });
            let excess = output.len().saturating_sub(200);
            if excess > 0 {
                output.drain(..excess);
            }
        }
    }

    fn push_system(&self, text: &str) {
        let mut output = self.output.lock().unwrap();
        output.push(OutputLine {
            agent: "system".to_string(),
            text: text.to_string(),
        });
    }
}

// ---------------------------------------------------------------------------
// Headless ACP client (for conductor connections)
// ---------------------------------------------------------------------------

struct HeadlessClient;

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
        Ok(())
    }

    async fn ext_method(&self, _args: acp::ExtRequest) -> acp::Result<acp::ExtResponse> {
        Err(acp::Error::method_not_found())
    }

    async fn ext_notification(&self, _args: acp::ExtNotification) -> acp::Result<()> {
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// TUI Component
// ---------------------------------------------------------------------------

#[derive(Default, Props)]
struct DashboardProps {
    dash: Option<DashState>,
    agent_names: Vec<String>,
}

#[component]
fn Dashboard(props: &DashboardProps, mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let mut system = hooks.use_context_mut::<SystemContext>();
    let mut selected_agent = hooks.use_state(|| 0usize);
    let mut input_buf = hooks.use_state(|| String::new());
    let mut should_exit = hooks.use_state(|| false);

    let agent_count = props.agent_names.len();
    let dash = props.dash.clone();

    hooks.use_terminal_events(move |event| match event {
        TerminalEvent::Key(KeyEvent { code, kind, .. }) if kind != KeyEventKind::Release => {
            match code {
                    KeyCode::Enter => {
                        let text = input_buf.to_string();
                        if !text.is_empty() {
                            if let Some(ref d) = dash {
                                let agents = d.agents.lock().unwrap();
                                let sel = selected_agent.get();
                                if sel < agents.len() {
                                    let name = agents[sel].name.clone();
                                    drop(agents);
                                    let _ = d.input_tx.send((name, text.clone()));
                                }
                            }
                            input_buf.set(String::new());
                        }
                    }
                    KeyCode::Char(c) => {
                        let mut s = input_buf.to_string();
                        s.push(c);
                        input_buf.set(s);
                    }
                    KeyCode::Backspace => {
                        let mut s = input_buf.to_string();
                        s.pop();
                        input_buf.set(s);
                    }
                    KeyCode::Tab => {
                        selected_agent.set((selected_agent.get() + 1) % agent_count.max(1));
                    }
                    KeyCode::BackTab => {
                        selected_agent.set(
                            (selected_agent.get() + agent_count.max(1) - 1) % agent_count.max(1),
                        );
                    }
                    KeyCode::Esc => should_exit.set(true),
                    _ => {}
                }
        }
        _ => {}
    });

    if should_exit.get() {
        system.exit();
    }

    // Read current state
    let agents: Vec<AgentStatus> = props
        .dash
        .as_ref()
        .map(|d| d.agents.lock().unwrap().clone())
        .unwrap_or_default();

    let output: Vec<OutputLine> = props
        .dash
        .as_ref()
        .map(|d| {
            let o = d.output.lock().unwrap();
            let start = o.len().saturating_sub(30);
            o[start..].to_vec()
        })
        .unwrap_or_default();

    let sel = selected_agent.get();
    let selected_name = agents.get(sel).map(|a| a.name.as_str()).unwrap_or("");

    // Colors
    let border = Color::AnsiValue(60);
    let header = Color::AnsiValue(145);
    let dim = Color::AnsiValue(242);
    let active = Color::AnsiValue(110);
    let ready_color = Color::AnsiValue(114);
    let starting_color = Color::AnsiValue(179);

    element! {
        View(flex_direction: FlexDirection::Column, width: 100pct, height: 100pct) {
            // Header
            View(padding_left: 1, margin_bottom: 1) {
                Text(content: "durable-acp", weight: Weight::Bold, color: active)
                Text(content: format!("  {} agents", agents.len()), color: dim)
                Text(content: "  tab=switch agent  esc=quit", color: dim)
            }

            // Main area: agents sidebar + output
            View(flex_grow: 1.0, flex_direction: FlexDirection::Row) {
                // Agent list (sidebar)
                View(width: 20, flex_direction: FlexDirection::Column, border_style: BorderStyle::Round, border_color: border) {
                    View(padding_left: 1, border_style: BorderStyle::Single, border_edges: Edges::Bottom, border_color: border) {
                        Text(content: "Agents", weight: Weight::Bold, color: header)
                    }
                    #(agents.iter().enumerate().map(|(i, a)| {
                        let is_sel = i == sel;
                        let bg = if is_sel { Some(Color::AnsiValue(236)) } else { None };
                        let indicator = if is_sel { ">" } else { " " };
                        let state_color = match a.state.as_str() {
                            "ready" => ready_color,
                            "starting" => starting_color,
                            _ => Color::AnsiValue(196),
                        };
                        let dot = match a.state.as_str() {
                            "ready" => "●",
                            "starting" => "○",
                            _ => "✕",
                        };
                        element! {
                            View(background_color: bg, padding_left: 1) {
                                Text(content: format!("{} {} {}", indicator, dot, a.name), color: if is_sel { active } else { dim })
                            }
                        }
                    }))
                }

                // Output area
                View(flex_grow: 1.0, flex_direction: FlexDirection::Column, border_style: BorderStyle::Round, border_color: border, margin_left: 1) {
                    View(padding_left: 1, border_style: BorderStyle::Single, border_edges: Edges::Bottom, border_color: border) {
                        Text(content: format!("Output — {}", selected_name), weight: Weight::Bold, color: header)
                    }
                    View(flex_grow: 1.0, flex_direction: FlexDirection::Column, padding: 1) {
                        #(output.iter().filter(|l| l.agent == selected_name || l.agent == "system").map(|line| {
                            let color = if line.agent == "system" { dim } else { Color::AnsiValue(252) };
                            element! {
                                View {
                                    Text(content: line.text.clone(), color: color)
                                }
                            }
                        }))
                    }
                }
            }

            // Input bar
            View(border_style: BorderStyle::Round, border_color: active, margin_top: 1) {
                View(padding_left: 1, width: 100pct) {
                    Text(content: format!("[{}] > {}_", selected_name, input_buf.to_string()), color: active)
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Prompt submission via REST API
// ---------------------------------------------------------------------------

async fn submit_and_stream(
    dash: DashState,
    agent_name: String,
    api_port: u16,
    text: String,
) {
    let client = reqwest::Client::new();
    let api_url = format!("http://127.0.0.1:{}", api_port);

    let connections: Vec<serde_json::Value> = match client
        .get(format!("{api_url}/api/v1/connections"))
        .send()
        .await
    {
        Ok(resp) => resp.json().await.unwrap_or_default(),
        Err(e) => {
            dash.push_output(&agent_name, &format!("[error: {}]\n", e));
            return;
        }
    };

    let conn = match connections.first() {
        Some(c) => c,
        None => {
            dash.push_output(&agent_name, "[error: no connections]\n");
            return;
        }
    };

    let conn_id = conn["logicalConnectionId"].as_str().unwrap_or("");
    let session_id = conn["latestSessionId"].as_str().unwrap_or("");

    if session_id.is_empty() {
        dash.push_output(&agent_name, "[error: no session]\n");
        return;
    }

    // Submit prompt
    dash.push_output(&agent_name, &format!("> {}\n", text));

    let result: serde_json::Value = match client
        .post(format!("{api_url}/api/v1/connections/{conn_id}/prompt"))
        .json(&serde_json::json!({ "sessionId": session_id, "text": text }))
        .send()
        .await
    {
        Ok(resp) => resp.json().await.unwrap_or_default(),
        Err(e) => {
            dash.push_output(&agent_name, &format!("[error: {}]\n", e));
            return;
        }
    };

    let turn_id = result["promptTurnId"].as_str().unwrap_or("");
    if turn_id.is_empty() {
        dash.push_output(&agent_name, "[error: no promptTurnId]\n");
        return;
    }

    // Stream response via SSE
    let resp = match client
        .get(format!("{api_url}/api/v1/prompt-turns/{turn_id}/stream"))
        .timeout(std::time::Duration::from_secs(120))
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            dash.push_output(&agent_name, &format!("[error: {}]\n", e));
            return;
        }
    };

    use futures::StreamExt;
    let mut stream = resp.bytes_stream();
    let mut buf = String::new();

    while let Some(chunk) = stream.next().await {
        let bytes = match chunk {
            Ok(b) => b,
            Err(_) => break,
        };
        buf.push_str(&String::from_utf8_lossy(&bytes));

        while let Some(end) = buf.find("\n\n") {
            let event = buf[..end].to_string();
            buf = buf[end + 2..].to_string();

            for line in event.lines() {
                if let Some(data) = line.strip_prefix("data:") {
                    let data = data.trim();
                    if data.is_empty() {
                        continue;
                    }
                    if let Ok(chunk) = serde_json::from_str::<serde_json::Value>(data) {
                        let chunk_type = chunk["type"].as_str().unwrap_or("");
                        let content = chunk["content"].as_str().unwrap_or("");
                        match chunk_type {
                            "text" => dash.push_output(&agent_name, content),
                            "tool_call" => {
                                dash.push_output(&agent_name, &format!("\n[tool] {}\n", content))
                            }
                            "stop" => {
                                dash.push_output(&agent_name, "\n");
                                return;
                            }
                            "error" => {
                                dash.push_output(
                                    &agent_name,
                                    &format!("\n[error] {}\n", content),
                                );
                                return;
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
    }
    dash.push_output(&agent_name, "\n");
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    let agents: Vec<AgentConfig> = if let Some(agent_id) = &cli.agent {
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
            bail!("Config '{}' not found. Use --agent <id>.", config_path.display());
        }
        let config: Config = toml::from_str(&std::fs::read_to_string(config_path)?)?;
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

    // Resolve commands
    let mut resolved: Vec<(AgentConfig, Vec<String>)> = Vec::new();
    for config in &agents {
        let command = if let Some(cmd) = &config.command {
            cmd.clone()
        } else if let Some(agent_id) = &config.agent {
            let remote = registry
                .agents
                .iter()
                .find(|a| a.id == *agent_id)
                .with_context(|| format!("Agent '{}' not found", agent_id))?;
            remote.resolve_command()?
        } else {
            bail!("Agent '{}' needs 'agent' or 'command'", config.name);
        };
        resolved.push((config.clone(), command));
    }

    let (input_tx, mut input_rx) = tokio::sync::mpsc::unbounded_channel();
    let dash = DashState::new(input_tx);

    // Initialize agent status
    {
        let mut statuses = dash.agents.lock().unwrap();
        for (config, _) in &resolved {
            statuses.push(AgentStatus {
                name: config.name.clone(),
                port: config.port,
                state: "starting".to_string(),
            });
        }
    }

    let agent_names: Vec<String> = resolved.iter().map(|(c, _)| c.name.clone()).collect();

    // Spawn conductors
    let local_set = tokio::task::LocalSet::new();
    let dash_clone = dash.clone();

    local_set.spawn_local(async move {
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

            let child_result = tokio::process::Command::new(&conductor_bin)
                .args(&conductor_args)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .kill_on_drop(true)
                .spawn();

            let mut child = match child_result {
                Ok(c) => c,
                Err(e) => {
                    dash_clone.set_agent_state(&config.name, "error");
                    dash_clone.push_system(&format!("[{}] Failed to start: {}", config.name, e));
                    continue;
                }
            };

            let outgoing = child.stdin.take().unwrap().compat_write();
            let incoming = child.stdout.take().unwrap().compat();
            let name = config.name.clone();
            let dash2 = dash_clone.clone();

            let (conn, handle_io) = acp::ClientSideConnection::new(
                HeadlessClient,
                outgoing,
                incoming,
                |fut| {
                    tokio::task::spawn_local(fut);
                },
            );

            tokio::task::spawn_local(handle_io);

            tokio::task::spawn_local(async move {
                match conn
                    .initialize(
                        acp::InitializeRequest::new(acp::ProtocolVersion::V1).client_info(
                            acp::Implementation::new("durable-acp-dashboard", "0.1.0")
                                .title("Dashboard"),
                        ),
                    )
                    .await
                {
                    Ok(_) => {}
                    Err(e) => {
                        dash2.set_agent_state(&name, "error");
                        dash2.push_system(&format!("[{}] Init failed: {}", name, e));
                        return;
                    }
                }

                match conn
                    .new_session(acp::NewSessionRequest::new(
                        std::env::current_dir().unwrap_or_default(),
                    ))
                    .await
                {
                    Ok(_) => {
                        dash2.set_agent_state(&name, "ready");
                        dash2.push_system(&format!("[{}] Ready", name));
                    }
                    Err(e) => {
                        dash2.set_agent_state(&name, "error");
                        dash2.push_system(&format!("[{}] Session failed: {}", name, e));
                    }
                }

                // Keep alive
                let (_tx, rx) = tokio::sync::oneshot::channel::<()>();
                let _ = rx.await;
                drop(child);
            });
        }
    });

    // Prompt submission task
    let dash_for_input = dash.clone();
    let agents_for_input = agents.clone();
    tokio::spawn(async move {
        while let Some((agent_name, text)) = input_rx.recv().await {
            let port = agents_for_input
                .iter()
                .find(|a| a.name == agent_name)
                .map(|a| a.port + 1)
                .unwrap_or(4438);
            let d = dash_for_input.clone();
            tokio::spawn(submit_and_stream(d, agent_name, port, text));
        }
    });

    // Run TUI on the local set
    local_set
        .run_until(async {
            smol::block_on(
                element!(Dashboard(dash: dash.clone(), agent_names: agent_names)).render_loop(),
            )?;

            // Cleanup
            for config in &agents {
                let _ = durable_acp_rs::registry::unregister(&config.name);
            }
            Ok::<_, anyhow::Error>(())
        })
        .await
}
