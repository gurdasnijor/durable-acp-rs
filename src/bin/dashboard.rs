//! Multi-agent TUI dashboard — subprocess per agent.
//!
//! Spawns each agent as a conductor subprocess, bootstraps via ACP client
//! (Initialize + NewSession), then uses REST API for prompts/streaming.
//!
//! Doubles as a minimal integration test harness: proves a thin ACP client
//! can wire up conductors with no in-process glue.
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

use durable_acp_rs::state::{ChunkRow, ChunkType};

// ---------------------------------------------------------------------------
// CLI + Config
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
    #[serde(default = "default_ss")]
    state_stream: String,
}

#[derive(Debug, Deserialize)]
struct Config {
    agent: Vec<AgentConfig>,
}

fn default_ss() -> String {
    "durable-acp-state".to_string()
}

// ---------------------------------------------------------------------------
// Minimal ACP client — auto-approves permissions
// ---------------------------------------------------------------------------

struct DashboardClient {
    #[allow(dead_code)]
    name: String,
}

#[async_trait::async_trait(?Send)]
impl acp::Client for DashboardClient {
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
// Shared TUI state
// ---------------------------------------------------------------------------

#[derive(Clone, Default)]
struct TuiState {
    inner: Arc<Mutex<TuiStateInner>>,
}

#[derive(Default)]
struct TuiStateInner {
    agents: Vec<AgentUiState>,
}

struct AgentUiState {
    name: String,
    api_url: String,
    state: String,
    output: Vec<String>,
}

impl TuiState {
    fn set_state(&self, name: &str, state: &str) {
        let mut inner = self.inner.lock().unwrap();
        if let Some(a) = inner.agents.iter_mut().find(|a| a.name == name) {
            a.state = state.to_string();
        }
    }

    fn push_text(&self, name: &str, text: &str) {
        let mut inner = self.inner.lock().unwrap();
        if let Some(a) = inner.agents.iter_mut().find(|a| a.name == name) {
            let should_append = a.output.last().map(|l| !l.ends_with('\n')).unwrap_or(false);
            if should_append {
                a.output.last_mut().unwrap().push_str(text);
            } else {
                a.output.push(text.to_string());
            }
            let excess = a.output.len().saturating_sub(200);
            if excess > 0 { a.output.drain(..excess); }
        }
    }

    fn snapshot(&self, agent_idx: usize) -> (Vec<(String, String)>, Vec<String>) {
        let inner = self.inner.lock().unwrap();
        let agents: Vec<(String, String)> = inner.agents.iter()
            .map(|a| (a.name.clone(), a.state.clone())).collect();
        let output = inner.agents.get(agent_idx)
            .map(|a| {
                let start = a.output.len().saturating_sub(30);
                a.output[start..].to_vec()
            }).unwrap_or_default();
        (agents, output)
    }

    fn api_url(&self, agent_idx: usize) -> Option<String> {
        self.inner.lock().unwrap().agents.get(agent_idx).map(|a| a.api_url.clone())
    }
}

// ---------------------------------------------------------------------------
// REST API helpers — prompt via HTTP
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ConnectionInfo {
    logical_connection_id: String,
    latest_session_id: Option<String>,
    state: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PromptResult {
    prompt_turn_id: String,
}

async fn submit_prompt_http(http: &reqwest::Client, api_url: &str, text: &str) -> Result<String> {
    let connections: Vec<ConnectionInfo> = http
        .get(format!("{api_url}/api/v1/connections")).send().await?.json().await?;
    let conn = connections.iter()
        .find(|c| c.state == "attached")
        .or(connections.first())
        .ok_or_else(|| anyhow::anyhow!("no connections"))?;
    let session_id = conn.latest_session_id.as_deref()
        .ok_or_else(|| anyhow::anyhow!("no active session"))?;
    let result: PromptResult = http
        .post(format!("{api_url}/api/v1/connections/{}/prompt", conn.logical_connection_id))
        .json(&serde_json::json!({ "sessionId": session_id, "text": text }))
        .send().await?.json().await?;
    Ok(result.prompt_turn_id)
}

async fn stream_response(http: &reqwest::Client, api_url: &str, turn_id: &str, tui: &TuiState, name: &str) {
    let Ok(response) = http
        .get(format!("{api_url}/api/v1/prompt-turns/{turn_id}/stream"))
        .timeout(std::time::Duration::from_secs(120))
        .send().await
    else { return };

    use futures::StreamExt;
    let mut buf = String::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let Ok(bytes) = chunk else { break };
        buf.push_str(&String::from_utf8_lossy(&bytes));
        while let Some(end) = buf.find("\n\n") {
            let event = buf[..end].to_string();
            buf = buf[end + 2..].to_string();
            for line in event.lines() {
                if let Some(data) = line.strip_prefix("data:").map(str::trim) {
                    if let Ok(ev) = serde_json::from_str::<ChunkRow>(data) {
                        match ev.chunk_type {
                            ChunkType::Text => tui.push_text(name, &ev.content),
                            ChunkType::ToolCall => tui.push_text(name, &format!("\n[tool] {}\n", ev.content)),
                            ChunkType::Thinking => tui.push_text(name, "."),
                            ChunkType::Stop => return,
                            ChunkType::Error => {
                                tui.push_text(name, &format!("[error] {}\n", ev.content));
                                return;
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// TUI Component
// ---------------------------------------------------------------------------

#[derive(Default, Props)]
struct DashboardProps {
    tui: Option<TuiState>,
    agent_count: usize,
    prompt_fn: Option<Arc<dyn Fn(usize, String) + Send + Sync>>,
}

#[component]
fn Dashboard(props: &DashboardProps, mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let (term_w, term_h) = hooks.use_terminal_size();
    let mut system = hooks.use_context_mut::<SystemContext>();
    let mut selected = hooks.use_state(|| 0usize);
    let mut input = hooks.use_state(|| String::new());
    let mut done = hooks.use_state(|| false);

    let mut tick = hooks.use_state(|| 0u64);
    hooks.use_future(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            tick.set(tick.get().wrapping_add(1));
        }
    });

    let sidebar_w: u32 = 22;
    let output_w = (term_w as u32).saturating_sub(sidebar_w + 3);
    let output_h = (term_h as u32).saturating_sub(6);
    let agent_count = props.agent_count;
    let prompt_fn = props.prompt_fn.clone();

    hooks.use_terminal_events(move |event| match event {
        TerminalEvent::Key(KeyEvent { code, kind, .. }) if kind != KeyEventKind::Release => {
            match code {
                KeyCode::Enter => {
                    let text = input.to_string();
                    if !text.is_empty() {
                        if let Some(ref f) = prompt_fn { f(selected.get(), text); }
                        input.set(String::new());
                    }
                }
                KeyCode::Tab => selected.set((selected.get() + 1) % agent_count.max(1)),
                KeyCode::BackTab => selected.set((selected.get() + agent_count.max(1) - 1) % agent_count.max(1)),
                KeyCode::Char(c) => { let mut s = input.to_string(); s.push(c); input.set(s); }
                KeyCode::Backspace => { let mut s = input.to_string(); s.pop(); input.set(s); }
                KeyCode::Esc => done.set(true),
                _ => {}
            }
        }
        _ => {}
    });

    if done.get() { system.exit(); }

    let (agents, output) = props.tui.as_ref().map(|t| t.snapshot(selected.get())).unwrap_or_default();
    let sel = selected.get();
    let sel_name = agents.get(sel).map(|a| a.0.as_str()).unwrap_or("");

    let border = Color::AnsiValue(60);
    let dim = Color::AnsiValue(242);
    let active = Color::AnsiValue(110);
    let header = Color::AnsiValue(145);
    let ready_c = Color::AnsiValue(114);
    let starting_c = Color::AnsiValue(179);
    let error_c = Color::AnsiValue(196);

    element! {
        View(flex_direction: FlexDirection::Column, width: term_w, height: term_h) {
            View(padding_left: 1, height: 1) {
                Text(content: "durable-acp", weight: Weight::Bold, color: active)
                Text(content: format!("  {} agents", agents.len()), color: dim)
                Text(content: "  tab", color: active)
                Text(content: "=switch  ", color: dim)
                Text(content: "esc", color: active)
                Text(content: "=quit", color: dim)
            }
            View(flex_direction: FlexDirection::Row, height: output_h) {
                View(width: sidebar_w, flex_direction: FlexDirection::Column, border_style: BorderStyle::Round, border_color: border) {
                    View(padding_left: 1, border_style: BorderStyle::Single, border_edges: Edges::Bottom, border_color: border) {
                        Text(content: "Agents", weight: Weight::Bold, color: header)
                    }
                    #(agents.iter().enumerate().map(|(i, (name, state))| {
                        let is_sel = i == sel;
                        let bg = if is_sel { Some(Color::AnsiValue(236)) } else { None };
                        let ind = if is_sel { ">" } else { " " };
                        let (dot, _dc) = match state.as_str() {
                            "ready" => ("●", ready_c),
                            "starting" => ("○", starting_c),
                            _ => ("✕", error_c),
                        };
                        element! {
                            View(background_color: bg, padding_left: 1) {
                                Text(content: format!("{} {} {}", ind, dot, name), color: if is_sel { active } else { dim }, wrap: TextWrap::NoWrap)
                            }
                        }
                    }))
                }
                View(width: output_w, flex_direction: FlexDirection::Column, border_style: BorderStyle::Round, border_color: border, margin_left: 1) {
                    View(padding_left: 1, border_style: BorderStyle::Single, border_edges: Edges::Bottom, border_color: border) {
                        Text(content: sel_name.to_string(), weight: Weight::Bold, color: header, wrap: TextWrap::NoWrap)
                    }
                    ScrollView {
                        View(flex_direction: FlexDirection::Column) {
                            #(output.iter().map(|line| {
                                element! { View { Text(content: line.clone(), color: Color::AnsiValue(252)) } }
                            }))
                        }
                    }
                }
            }
            View(width: term_w, height: 3, border_style: BorderStyle::Round, border_color: active) {
                View(padding_left: 1) {
                    Text(content: format!("[{}] > {}_", sel_name, input.to_string()), color: active, wrap: TextWrap::NoWrap)
                }
            }
        }
    }
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
            state_stream: default_ss(),
        }]
    } else {
        let p = &cli.config;
        if !p.exists() { bail!("Config '{}' not found.", p.display()); }
        let config: Config = toml::from_str(&std::fs::read_to_string(p)?)?;
        config.agent
    };

    if agents.is_empty() { bail!("No agents configured."); }

    eprintln!("Fetching ACP agent registry...");
    let registry = durable_acp_rs::acp_registry::fetch_registry().await?;

    let mut resolved: Vec<(AgentConfig, Vec<String>)> = Vec::new();
    for config in &agents {
        let command = if let Some(cmd) = &config.command {
            cmd.clone()
        } else if let Some(agent_id) = &config.agent {
            let remote = registry.agents.iter()
                .find(|a| a.id == *agent_id)
                .with_context(|| format!("Agent '{}' not found", agent_id))?;
            remote.resolve_command()?
        } else {
            bail!("Agent '{}' needs 'agent' or 'command'", config.name);
        };
        resolved.push((config.clone(), command));
    }

    let conductor_bin = std::env::current_exe()?
        .parent().unwrap()
        .join("durable-acp-rs");
    if !conductor_bin.exists() {
        bail!("Conductor binary not found at {}. Run `cargo build` first.", conductor_bin.display());
    }

    // TUI state
    let tui = TuiState::default();
    {
        let mut inner = tui.inner.lock().unwrap();
        for (config, _) in &resolved {
            inner.agents.push(AgentUiState {
                name: config.name.clone(),
                api_url: format!("http://127.0.0.1:{}", config.port + 1),
                state: "starting".to_string(),
                output: vec![],
            });
        }
    }

    // LocalSet needed for ClientSideConnection (!Send futures)
    let local = tokio::task::LocalSet::new();
    let tui_clone = tui.clone();
    let agents_ref = agents.clone();

    local
        .run_until(async move {
            // Spawn each conductor subprocess and bootstrap via ACP client
            for (config, command) in &resolved {
                let mut conductor_args = vec![
                    "--name".to_string(), config.name.clone(),
                    "--port".to_string(), config.port.to_string(),
                    "--state-stream".to_string(), config.state_stream.clone(),
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
                let api_url = format!("http://127.0.0.1:{}", config.port + 1);
                let tui2 = tui_clone.clone();

                // Register in peer registry
                let _ = durable_acp_rs::registry::register(durable_acp_rs::registry::AgentEntry {
                    name: config.name.clone(),
                    api_url: api_url.clone(),
                    logical_connection_id: uuid::Uuid::new_v4().to_string(),
                    registered_at: durable_acp_rs::app::now_ms(),
                });

                // Connect as ACP client — the minimal glue
                let (conn, handle_io) = acp::ClientSideConnection::new(
                    DashboardClient { name: name.clone() },
                    outgoing,
                    incoming,
                    |fut| { tokio::task::spawn_local(fut); },
                );
                tokio::task::spawn_local(handle_io);

                // Initialize + create session, then mark ready
                tokio::task::spawn_local(async move {
                    match async {
                        conn.initialize(
                            acp::InitializeRequest::new(acp::ProtocolVersion::V1)
                                .client_info(acp::Implementation::new("durable-acp-dashboard", "0.1.0")
                                    .title("Dashboard")),
                        ).await?;
                        conn.new_session(
                            acp::NewSessionRequest::new(std::env::current_dir()?),
                        ).await?;
                        Ok::<_, anyhow::Error>(())
                    }.await {
                        Ok(()) => {
                            tui2.set_state(&name, "ready");
                            tui2.push_text(&name, &format!("[{}] Ready ({})\n", name, api_url));
                        }
                        Err(e) => {
                            tui2.set_state(&name, "error");
                            tui2.push_text(&name, &format!("[error] {}\n", e));
                        }
                    }

                    // Keep connection alive until dashboard exits
                    let (_tx, rx) = tokio::sync::oneshot::channel::<()>();
                    let _ = rx.await;
                    drop(child);
                    Ok::<_, anyhow::Error>(())
                });
            }

            // Prompt callback — submits via REST API
            let http = reqwest::Client::new();
            let tui_prompt = tui_clone.clone();
            let prompt_fn: Arc<dyn Fn(usize, String) + Send + Sync> = Arc::new(move |idx, text| {
                let tui = tui_prompt.clone();
                let http = http.clone();
                let Some(api_url) = tui.api_url(idx) else { return };
                let name = tui.inner.lock().unwrap().agents.get(idx)
                    .map(|a| a.name.clone()).unwrap_or_default();
                tui.push_text(&name, &format!("> {}\n", text));
                tokio::task::spawn_local(async move {
                    match submit_prompt_http(&http, &api_url, &text).await {
                        Ok(turn_id) => stream_response(&http, &api_url, &turn_id, &tui, &name).await,
                        Err(e) => tui.push_text(&name, &format!("[error] {}\n", e)),
                    }
                    tui.push_text(&name, "\n");
                });
            });

            let agent_count = resolved.len();
            element!(Dashboard(tui: tui_clone, agent_count: agent_count, prompt_fn: prompt_fn))
                .fullscreen().await.ok();

            // Cleanup
            for config in agents_ref.iter() {
                let _ = durable_acp_rs::registry::unregister(&config.name);
            }

            Ok::<_, anyhow::Error>(())
        })
        .await?;

    Ok(())
}
