//! Multi-agent TUI dashboard — thin TUI over `DurableAcpClient`.
//!
//! The ACP client lifecycle (connect, init, session, prompt loop) lives
//! in `src/client.rs`. This binary is just the TUI + orchestration.
//!
//! Usage:
//!   cargo run --bin dashboard
//!   cargo run --bin dashboard -- --agent claude-acp

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, bail};
use clap::Parser as ClapParser;
use iocraft::prelude::*;
use serde::Deserialize;

use durable_acp_rs::client::{self, AcpClientHandler};

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
    transport: Option<durable_acp_rs::transport::TransportConfig>,
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

// No custom Client impl needed — sacp::Client.builder().on_receive_request()
// handles permission requests inline.

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

}

// ---------------------------------------------------------------------------
// AcpClientHandler impl — bridges ACP events to TuiState
// ---------------------------------------------------------------------------

struct TuiHandler {
    tui: TuiState,
}

impl AcpClientHandler for TuiHandler {
    fn on_ready(&self, name: &str) {
        self.tui.set_state(name, "ready");
        self.tui.push_text(name, &format!("[{name}] Ready\n"));
    }

    fn on_text(&self, name: &str, text: &str) {
        self.tui.push_text(name, text);
        self.tui.push_text(name, "\n");
    }

    fn on_error(&self, name: &str, error: &str) {
        self.tui.set_state(name, "error");
        self.tui.push_text(name, &format!("[error] {error}\n"));
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
            transport: None,
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
                state: "starting".to_string(),
                output: vec![],
            });
        }
    }

    // LocalSet needed — ACP client futures are !Send with stdio transport
    let local = tokio::task::LocalSet::new();
    let tui_clone = tui.clone();
    let agents_ref = agents.clone();

    local
        .run_until(async move {
            // Spawn an ACP client per agent — collect handles for prompt submission
            let mut handles: Vec<client::AcpClientHandle> = Vec::new();

            for (config, command) in &resolved {
                let name = config.name.clone();

                // Resolve transport from agents.toml config
                use durable_acp_rs::transport::{TransportConfig, WebSocketTransport, TcpTransport};
                let transport: sacp::DynConnectTo<sacp::Client> = match &config.transport {
                    Some(TransportConfig::Ws { url }) => {
                        sacp::DynConnectTo::new(WebSocketTransport { url: url.clone() })
                    }
                    Some(TransportConfig::Tcp { host, port }) => {
                        sacp::DynConnectTo::new(TcpTransport { host: host.clone(), port: *port })
                    }
                    None | Some(TransportConfig::Stdio) => {
                        let mut conductor_args = vec![
                            "--name".to_string(), config.name.clone(),
                            "--port".to_string(), config.port.to_string(),
                            "--state-stream".to_string(), config.state_stream.clone(),
                        ];
                        conductor_args.extend(command.iter().cloned());
                        let mut full_command = vec![conductor_bin.to_string_lossy().to_string()];
                        full_command.extend(conductor_args);
                        sacp::DynConnectTo::new(sacp_tokio::AcpAgent::from_args(full_command)
                            .with_context(|| format!("parse conductor command for '{}'", name))?)
                    }
                };

                // Register in peer registry
                let _ = durable_acp_rs::registry::register(durable_acp_rs::registry::AgentEntry {
                    name: config.name.clone(),
                    api_url: format!("http://127.0.0.1:{}", config.port + 1),
                    logical_connection_id: uuid::Uuid::new_v4().to_string(),
                    registered_at: durable_acp_rs::app::now_ms(),
                });

                let handler = Arc::new(TuiHandler { tui: tui_clone.clone() });
                let handle = client::spawn_acp_client(
                    client::AcpClientConfig { name, transport },
                    handler,
                );
                handles.push(handle);
            }

            // Prompt callback — sends to the ACP client via its handle
            let prompt_fn: Arc<dyn Fn(usize, String) + Send + Sync> = {
                let tui = tui_clone.clone();
                // Move handles into a shared ref for the callback
                let handles = Arc::new(handles);
                Arc::new(move |idx, text| {
                    let name = tui.inner.lock().unwrap().agents.get(idx)
                        .map(|a| a.name.clone()).unwrap_or_default();
                    tui.push_text(&name, &format!("> {}\n", text));
                    if let Some(h) = handles.get(idx) {
                        let _ = h.prompt_tx.send(text);
                    }
                })
            };

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
