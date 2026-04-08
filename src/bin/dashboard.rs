//! Multi-agent TUI dashboard — ACP client per agent.
//!
//! Spawns each agent as a conductor subprocess, bootstraps via ACP client
//! (Initialize + NewSession), sends prompts and reads responses through ACP.
//!
//! Usage:
//!   cargo run --bin dashboard
//!   cargo run --bin dashboard -- --agent claude-acp

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use agent_client_protocol::{self as acp};
use anyhow::{Context, Result, bail};
use clap::Parser as ClapParser;
use iocraft::prelude::*;
use serde::Deserialize;

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
    prompt_tx: tokio::sync::mpsc::UnboundedSender<String>,
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

    fn send_prompt(&self, agent_idx: usize, text: String) {
        let inner = self.inner.lock().unwrap();
        if let Some(a) = inner.agents.get(agent_idx) {
            let _ = a.prompt_tx.send(text);
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

    // TUI state — create a prompt channel per agent
    let tui = TuiState::default();
    let mut prompt_receivers: Vec<tokio::sync::mpsc::UnboundedReceiver<String>> = Vec::new();
    {
        let mut inner = tui.inner.lock().unwrap();
        for (config, _) in &resolved {
            let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
            inner.agents.push(AgentUiState {
                name: config.name.clone(),
                state: "starting".to_string(),
                output: vec![],
                prompt_tx: tx,
            });
            prompt_receivers.push(rx);
        }
    }

    // LocalSet needed for ClientSideConnection (!Send futures)
    let local = tokio::task::LocalSet::new();
    let tui_clone = tui.clone();
    let agents_ref = agents.clone();

    local
        .run_until(async move {
            // Connect to each agent — transport resolved from config
            for ((config, command), mut prompt_rx) in resolved.iter().zip(prompt_receivers) {
                let name = config.name.clone();
                let api_url = format!("http://127.0.0.1:{}", config.port + 1);
                let tui2 = tui_clone.clone();

                // Resolve transport from agents.toml config using SDK's DynConnectTo
                use durable_acp_rs::transport::{TransportConfig, WebSocketTransport, TcpTransport};
                let transport: sacp::DynConnectTo<sacp::Client> = match &config.transport {
                    Some(TransportConfig::Ws { url }) => {
                        sacp::DynConnectTo::new(WebSocketTransport { url: url.clone() })
                    }
                    Some(TransportConfig::Tcp { host, port }) => {
                        sacp::DynConnectTo::new(TcpTransport { host: host.clone(), port: *port })
                    }
                    None | Some(TransportConfig::Stdio) => {
                        // Default: spawn conductor subprocess
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
                    api_url: api_url.clone(),
                    logical_connection_id: uuid::Uuid::new_v4().to_string(),
                    registered_at: durable_acp_rs::app::now_ms(),
                });

                // Connect as ACP client using SDK primitives
                tokio::task::spawn_local({
                    let name = name.clone();
                    let tui2 = tui2.clone();
                    async move {
                        let result = sacp::Client
                            .builder()
                            .name(&format!("{}-client", name))
                            .on_receive_request(
                                async |req: acp::RequestPermissionRequest, responder, _cx| {
                                    // Auto-approve permissions
                                    let outcome = if let Some(opt) = req.options.first() {
                                        acp::RequestPermissionOutcome::Selected(
                                            acp::SelectedPermissionOutcome::new(opt.option_id.clone()),
                                        )
                                    } else {
                                        acp::RequestPermissionOutcome::Cancelled
                                    };
                                    responder.respond(acp::RequestPermissionResponse::new(outcome))
                                },
                                sacp::on_receive_request!(),
                            )
                            .connect_with(transport, async |cx| {
                                // Initialize
                                cx.send_request(acp::InitializeRequest::new(acp::ProtocolVersion::V1))
                                    .block_task()
                                    .await?;

                                // Create session and run prompt loop
                                cx.build_session_cwd()?
                                    .block_task()
                                    .run_until(async |mut session| {
                                        tui2.set_state(&name, "ready");
                                        tui2.push_text(&name, &format!("[{}] Ready ({})\n", name, api_url));

                                        while let Some(text) = prompt_rx.recv().await {
                                            session.send_prompt(&text)?;
                                            let response = session.read_to_string().await?;
                                            tui2.push_text(&name, &response);
                                            tui2.push_text(&name, "\n");
                                        }
                                        Ok(())
                                    })
                                    .await
                            })
                            .await;

                        if let Err(e) = result {
                            tui2.set_state(&name, "error");
                            tui2.push_text(&name, &format!("[error] {}\n", e));
                        }
                    }
                });
            }

            // Prompt callback — sends via ACP channel, response streamed by connect_with closure
            let tui_prompt = tui_clone.clone();
            let prompt_fn: Arc<dyn Fn(usize, String) + Send + Sync> = Arc::new(move |idx, text| {
                let tui = tui_prompt.clone();
                let name = tui.inner.lock().unwrap().agents.get(idx)
                    .map(|a| a.name.clone()).unwrap_or_default();
                tui.push_text(&name, &format!("> {}\n", text));
                tui.send_prompt(idx, text);
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
