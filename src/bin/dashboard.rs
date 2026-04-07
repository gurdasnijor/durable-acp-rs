//! Multi-agent TUI dashboard — single process, N conductors.
//!
//! Runs all agents from agents.toml in one process with in-process
//! conductors. The TUI acts as Terminal Client for all agents.
//!
//! Usage:
//!   cargo run --bin dashboard
//!   cargo run --bin dashboard -- --agent claude-acp

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use agent_client_protocol::{
    InitializeRequest, PromptRequest, ProtocolVersion, RequestPermissionOutcome,
    RequestPermissionRequest, RequestPermissionResponse, SelectedPermissionOutcome,
    SessionNotification, SessionUpdate, ContentBlock, ContentChunk,
};
use anyhow::{Context, Result, bail};
use clap::Parser as ClapParser;
use iocraft::prelude::*;
use sacp::{Client, Dispatch, SessionMessage, on_receive_request};
use sacp_conductor::{ConductorImpl, McpBridgeMode, ProxiesAndAgent};
use sacp_tokio::AcpAgent;
use serde::Deserialize;
use tokio::io::duplex;
use tokio::sync::{mpsc, oneshot};
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

use durable_acp_rs::app::AppState;
use durable_acp_rs::conductor::DurableStateProxy;
use durable_acp_rs::durable_streams::EmbeddedDurableStreams;
use durable_acp_rs::peer_mcp::PeerMcpProxy;

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
    #[allow(dead_code)]
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
// Channel types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
enum Output {
    Ready,
    Text(String),
    ToolCall(String),
    Thinking,
    Stop,
    Error(String),
    Permission(PermReq),
}

#[derive(Debug, Clone)]
struct PermReq {
    title: String,
    options: Vec<(String, String)>, // (option_id, name)
}

// ---------------------------------------------------------------------------
// Agent handle
// ---------------------------------------------------------------------------

struct AgentHandle {
    name: String,
    prompt_tx: mpsc::UnboundedSender<String>,
    output_rx: mpsc::UnboundedReceiver<Output>,
    perm_tx: mpsc::UnboundedSender<Option<String>>, // None = deny, Some(id) = approve
}

// ---------------------------------------------------------------------------
// Shared TUI state (Arc<Mutex> so iocraft can read it)
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
    state: String, // "starting", "ready", "error"
    output: Vec<String>,
    perm_pending: Option<PermReq>,
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
            if excess > 0 {
                a.output.drain(..excess);
            }
        }
    }

    fn set_perm(&self, name: &str, perm: Option<PermReq>) {
        let mut inner = self.inner.lock().unwrap();
        if let Some(a) = inner.agents.iter_mut().find(|a| a.name == name) {
            a.perm_pending = perm;
        }
    }

    fn snapshot(&self, agent_idx: usize) -> (Vec<(String, String)>, Vec<String>, Option<PermReq>) {
        let inner = self.inner.lock().unwrap();
        let agents: Vec<(String, String)> = inner
            .agents
            .iter()
            .map(|a| (a.name.clone(), a.state.clone()))
            .collect();
        let output = inner
            .agents
            .get(agent_idx)
            .map(|a| {
                let start = a.output.len().saturating_sub(30);
                a.output[start..].to_vec()
            })
            .unwrap_or_default();
        let perm = inner
            .agents
            .get(agent_idx)
            .and_then(|a| a.perm_pending.clone());
        (agents, output, perm)
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
    perm_fn: Option<Arc<dyn Fn(usize, Option<String>) + Send + Sync>>,
}

#[component]
fn Dashboard(props: &DashboardProps, mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let mut system = hooks.use_context_mut::<SystemContext>();
    let mut selected = hooks.use_state(|| 0usize);
    let mut input = hooks.use_state(|| String::new());
    let mut done = hooks.use_state(|| false);

    // Poll for state changes at 10Hz to trigger re-renders
    // (conductor tasks update TuiState via Arc<Mutex>, iocraft needs a wake signal)
    let mut tick = hooks.use_state(|| 0u64);
    hooks.use_future(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            tick.set(tick.get().wrapping_add(1));
        }
    });

    let agent_count = props.agent_count;
    let prompt_fn = props.prompt_fn.clone();
    let perm_fn = props.perm_fn.clone();

    hooks.use_terminal_events(move |event| match event {
        TerminalEvent::Key(KeyEvent { code, kind, .. }) if kind != KeyEventKind::Release => {
            match code {
                KeyCode::Enter => {
                    let text = input.to_string();
                    if !text.is_empty() {
                        if let Some(ref f) = prompt_fn {
                            f(selected.get(), text);
                        }
                        input.set(String::new());
                    }
                }
                KeyCode::Tab => selected.set((selected.get() + 1) % agent_count.max(1)),
                KeyCode::BackTab => {
                    selected.set((selected.get() + agent_count.max(1) - 1) % agent_count.max(1))
                }
                KeyCode::Char(c) => {
                    // Check if permission pending — number keys resolve it
                    // Otherwise append to input
                    if c >= '1' && c <= '9' {
                        if let Some(ref f) = perm_fn {
                            // Try to resolve permission (perm_fn checks if pending)
                            f(selected.get(), Some(c.to_string()));
                            return;
                        }
                    }
                    if c == 'n' || c == 'N' {
                        if let Some(ref f) = perm_fn {
                            f(selected.get(), None);
                            return;
                        }
                    }
                    let mut s = input.to_string();
                    s.push(c);
                    input.set(s);
                }
                KeyCode::Backspace => {
                    let mut s = input.to_string();
                    s.pop();
                    input.set(s);
                }
                KeyCode::Esc => done.set(true),
                _ => {}
            }
        }
        _ => {}
    });

    if done.get() {
        system.exit();
    }

    // Read snapshot
    let (agents, output, perm) = props
        .tui
        .as_ref()
        .map(|t| t.snapshot(selected.get()))
        .unwrap_or_default();

    let sel = selected.get();
    let sel_name = agents.get(sel).map(|a| a.0.as_str()).unwrap_or("");

    // Colors
    let border = Color::AnsiValue(60);
    let header = Color::AnsiValue(145);
    let dim = Color::AnsiValue(242);
    let active = Color::AnsiValue(110);
    let ready_c = Color::AnsiValue(114);
    let starting_c = Color::AnsiValue(179);
    let error_c = Color::AnsiValue(196);
    let warn_c = Color::AnsiValue(214);

    element! {
        View(flex_direction: FlexDirection::Column, width: 100pct, height: 100pct) {
            // Header
            View(padding_left: 1) {
                Text(content: "durable-acp", weight: Weight::Bold, color: active)
                Text(content: format!("  {} agents", agents.len()), color: dim)
                Text(content: "  tab", color: active)
                Text(content: "=switch  ", color: dim)
                Text(content: "esc", color: active)
                Text(content: "=quit", color: dim)
            }

            // Main: sidebar + output
            View(flex_grow: 1.0, flex_direction: FlexDirection::Row, margin_top: 1) {
                // Sidebar
                View(width: 22, flex_direction: FlexDirection::Column, border_style: BorderStyle::Round, border_color: border) {
                    View(padding_left: 1, border_style: BorderStyle::Single, border_edges: Edges::Bottom, border_color: border) {
                        Text(content: "Agents", weight: Weight::Bold, color: header)
                    }
                    #(agents.iter().enumerate().map(|(i, (name, state))| {
                        let is_sel = i == sel;
                        let bg = if is_sel { Some(Color::AnsiValue(236)) } else { None };
                        let ind = if is_sel { ">" } else { " " };
                        let (dot, dc) = match state.as_str() {
                            "ready" => ("●", ready_c),
                            "starting" => ("○", starting_c),
                            _ => ("✕", error_c),
                        };
                        element! {
                            View(background_color: bg, padding_left: 1) {
                                Text(content: format!("{} {} {}", ind, dot, name), color: if is_sel { active } else { dim })
                            }
                        }
                    }))
                }

                // Output
                View(flex_grow: 1.0, flex_direction: FlexDirection::Column, border_style: BorderStyle::Round, border_color: border, margin_left: 1) {
                    View(padding_left: 1, border_style: BorderStyle::Single, border_edges: Edges::Bottom, border_color: border) {
                        Text(content: format!("{}", sel_name), weight: Weight::Bold, color: header)
                    }
                    View(flex_grow: 1.0, flex_direction: FlexDirection::Column, padding: 1) {
                        #(output.iter().map(|line| {
                            element! { View { Text(content: line.clone(), color: Color::AnsiValue(252)) } }
                        }))

                        // Permission prompt (if pending)
                        #(perm.as_ref().map(|p| {
                            element! {
                                View(flex_direction: FlexDirection::Column, margin_top: 1, border_style: BorderStyle::Round, border_color: warn_c, padding: 1) {
                                    Text(content: format!("Permission: {}", p.title), color: warn_c, weight: Weight::Bold)
                                    #(p.options.iter().enumerate().map(|(i, (_id, name))| {
                                        element! {
                                            View { Text(content: format!("  [{}] {}", i + 1, name), color: header) }
                                        }
                                    }))
                                    Text(content: "  [n] Deny", color: dim)
                                }
                            }
                        }))
                    }
                }
            }

            // Input
            View(border_style: BorderStyle::Round, border_color: active, margin_top: 1) {
                View(padding_left: 1, width: 100pct) {
                    Text(content: format!("[{}] > {}_", sel_name, input.to_string()), color: active)
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
        if !p.exists() {
            bail!("Config '{}' not found.", p.display());
        }
        let config: Config = toml::from_str(&std::fs::read_to_string(p)?)?;
        config.agent
    };

    if agents.is_empty() {
        bail!("No agents configured.");
    }

    // Resolve commands from ACP registry
    eprintln!("Fetching ACP agent registry...");
    let registry = durable_acp_rs::acp_registry::fetch_registry().await?;

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

    // Shared durable streams server
    let bind = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), cli.port);
    let durable_streams = EmbeddedDurableStreams::start(bind, "durable-acp-state").await?;

    // Shared REST API
    let api_app_state = Arc::new(
        AppState::with_shared_streams(durable_streams.clone()).await?,
    );
    let api_router = durable_acp_rs::api::router(api_app_state);
    let api_listener =
        tokio::net::TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], cli.port + 1))).await?;
    tokio::spawn(async move {
        let _ = axum::serve(api_listener, api_router).await;
    });

    // TUI state
    let tui = TuiState::default();
    {
        let mut inner = tui.inner.lock().unwrap();
        for (config, _) in &resolved {
            inner.agents.push(AgentUiState {
                name: config.name.clone(),
                state: "starting".to_string(),
                output: vec![],
                perm_pending: None,
            });
        }
    }

    // Set up in-process agent router
    let router = durable_acp_rs::agent_router::AgentRouter::new();
    durable_acp_rs::agent_router::set_global_router(router.clone());

    // Prompt type: text + optional response channel for peer routing
    type PromptMsg = (String, Option<oneshot::Sender<Result<String, String>>>);

    // Channels for each agent
    let mut prompt_txs: Vec<mpsc::UnboundedSender<PromptMsg>> = Vec::new();
    let mut perm_txs: Vec<mpsc::UnboundedSender<Option<String>>> = Vec::new();

    let local = tokio::task::LocalSet::new();
    let tui_clone = tui.clone();

    // Spawn agents and run TUI on the same LocalSet
    local
        .run_until(async move {
            for (config, command) in &resolved {
                let (prompt_tx, prompt_rx) = mpsc::unbounded_channel::<PromptMsg>();
                let (perm_response_tx, perm_response_rx) =
                    mpsc::unbounded_channel::<Option<String>>();

                prompt_txs.push(prompt_tx.clone());
                perm_txs.push(perm_response_tx);

                let name = config.name.clone();
                let tui2 = tui_clone.clone();
                let ds = durable_streams.clone();

                // Register in-process peer router
                let (peer_tx, mut peer_rx) =
                    mpsc::unbounded_channel::<durable_acp_rs::agent_router::PeerPromptRequest>();
                router.register(name.clone(), peer_tx);

                // Bridge peer requests → prompt channel with response oneshot
                let prompt_tx_for_peer = prompt_tx.clone();
                tokio::task::spawn_local(async move {
                    while let Some(req) = peer_rx.recv().await {
                        let _ = prompt_tx_for_peer.send((req.text, Some(req.response_tx)));
                    }
                });

                let agent = AcpAgent::from_args(command.clone())
                    .with_context(|| format!("parse agent command for '{}'", name))
                    .unwrap();

                // Spawn conductor + client on this LocalSet
                tokio::task::spawn_local(run_agent(name, agent, ds, tui2, prompt_rx, perm_response_rx));
            }

            // Build prompt/perm callbacks for TUI
            let prompt_txs = Arc::new(prompt_txs);
            let perm_txs = Arc::new(perm_txs);
            let tui_for_perm = tui_clone.clone();

            let prompt_fn: Arc<dyn Fn(usize, String) + Send + Sync> = {
                let txs = prompt_txs.clone();
                Arc::new(move |idx, text| {
                    if let Some(tx) = txs.get(idx) {
                        let _ = tx.send((text, None)); // None = TUI prompt, no response needed
                    }
                })
            };

            let perm_fn: Arc<dyn Fn(usize, Option<String>) + Send + Sync> = {
                let txs = perm_txs.clone();
                let tui = tui_for_perm;
                Arc::new(move |idx, response| {
                    // Check if there's a pending permission for this agent
                    let inner = tui.inner.lock().unwrap();
                    let perm = inner.agents.get(idx).and_then(|a| a.perm_pending.clone());
                    drop(inner);

                    if let Some(perm) = perm {
                        let option_id = response.and_then(|r| {
                            r.parse::<usize>()
                                .ok()
                                .and_then(|n| perm.options.get(n - 1))
                                .map(|(id, _)| id.clone())
                        });
                        if let Some(tx) = txs.get(idx) {
                            let _ = tx.send(option_id);
                        }
                        tui.set_perm(
                            &inner_name(&tui, idx),
                            None,
                        );
                    }
                })
            };

            // Run TUI — render_loop is a standard Future, awaiting it on
            // the LocalSet lets spawn_local conductor tasks interleave
            let agent_count = resolved.len();
            element!(Dashboard(
                tui: tui_clone,
                agent_count: agent_count,
                prompt_fn: prompt_fn,
                perm_fn: perm_fn,
            ))
            .render_loop()
            .await
            .ok();

            // Cleanup
            for config in agents.iter() {
                let _ = durable_acp_rs::registry::unregister(&config.name);
            }
        })
        .await;

    Ok(())
}

fn inner_name(tui: &TuiState, idx: usize) -> String {
    tui.inner
        .lock()
        .unwrap()
        .agents
        .get(idx)
        .map(|a| a.name.clone())
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Per-agent conductor task
// ---------------------------------------------------------------------------

async fn run_agent(
    name: String,
    agent: AcpAgent,
    durable_streams: EmbeddedDurableStreams,
    tui: TuiState,
    mut prompt_rx: mpsc::UnboundedReceiver<(String, Option<oneshot::Sender<Result<String, String>>>)>,
    mut perm_response_rx: mpsc::UnboundedReceiver<Option<String>>,
) {
    let app = match AppState::with_shared_streams(durable_streams).await {
        Ok(app) => Arc::new(app),
        Err(e) => {
            tui.set_state(&name, "error");
            tui.push_text(&name, &format!("[error] {}\n", e));
            return;
        }
    };

    // Register in peer registry
    let _ = durable_acp_rs::registry::register(durable_acp_rs::registry::AgentEntry {
        name: name.clone(),
        api_url: "in-process".to_string(),
        logical_connection_id: app.logical_connection_id.clone(),
        registered_at: durable_acp_rs::app::now_ms(),
    });

    // In-memory transport: client <-> conductor
    let (client_out, conductor_in) = duplex(64 * 1024);
    let (conductor_out, client_in) = duplex(64 * 1024);

    let conductor_transport =
        sacp::ByteStreams::new(conductor_out.compat_write(), conductor_in.compat());
    let client_transport =
        sacp::ByteStreams::new(client_out.compat_write(), client_in.compat());

    let name2 = name.clone();
    let tui2 = tui.clone();

    // Permission channel for this agent
    let (perm_output_tx, mut perm_output_rx) = mpsc::unbounded_channel::<PermReq>();

    // Merge permission requests into the perm_response flow
    let tui_for_perm = tui.clone();
    let name_for_perm = name.clone();
    tokio::task::spawn_local(async move {
        while let Some(req) = perm_output_rx.recv().await {
            tui_for_perm.set_perm(&name_for_perm, Some(req));
        }
    });

    let result = Client
        .builder()
        .name(&format!("{}-client", name))
        .on_receive_request(
            {
                let perm_tx = perm_output_tx;
                let perm_rx = Arc::new(tokio::sync::Mutex::new(perm_response_rx));
                let name = name.clone();
                let tui = tui.clone();
                async move |req: RequestPermissionRequest, responder, cx| {
                    let title = req
                        .tool_call
                        .fields
                        .title
                        .clone()
                        .unwrap_or_else(|| "Permission".to_string());
                    let options: Vec<(String, String)> = req
                        .options
                        .iter()
                        .map(|o| (o.option_id.0.to_string(), o.name.clone()))
                        .collect();

                    let _ = perm_tx.send(PermReq {
                        title: title.clone(),
                        options: options.clone(),
                    });

                    let perm_rx = perm_rx.clone();
                    let tui = tui.clone();
                    let name = name.clone();

                    cx.spawn(async move {
                        let response = perm_rx.lock().await.recv().await.flatten();
                        let outcome = if let Some(option_id) = response {
                            RequestPermissionOutcome::Selected(
                                SelectedPermissionOutcome::new(option_id),
                            )
                        } else {
                            tui.push_text(&name, "[denied]\n");
                            RequestPermissionOutcome::Cancelled
                        };
                        responder.respond(RequestPermissionResponse::new(outcome))
                    })?;
                    Ok(())
                }
            },
            on_receive_request!(),
        )
        .with_spawned({
            let app = app.clone();
            move |_cx| async move {
                ConductorImpl::new_agent(
                    name2.clone(),
                    ProxiesAndAgent::new(agent)
                        .proxy(DurableStateProxy { app })
                        .proxy(PeerMcpProxy),
                    McpBridgeMode::default(),
                )
                .run(conductor_transport)
                .await
            }
        })
        .connect_with(client_transport, {
            let name = name.clone();
            let tui = tui2;
            async move |connection| {
                connection
                    .send_request(InitializeRequest::new(ProtocolVersion::V1))
                    .block_task()
                    .await?;

                connection
                    .build_session_cwd()?
                    .block_task()
                    .run_until(async |mut session| {
                        tui.set_state(&name, "ready");
                        tui.push_text(&name, &format!("[{}] Ready\n", name));

                        while let Some((text, mut response_tx)) = prompt_rx.recv().await {
                            tui.push_text(&name, &format!("> {}\n", text));
                            session.send_prompt(&text)?;
                            let mut response_buf = String::new();

                            loop {
                                let msg = match session.read_update().await {
                                    Ok(m) => m,
                                    Err(e) => {
                                        let s = e.to_string();
                                        if s.contains("Parse error")
                                            || s.contains("unknown variant")
                                            || s.contains("usage_update")
                                            || s.contains("deserialization")
                                        {
                                            continue;
                                        }
                                        tui.push_text(&name, &format!("[error] {}\n", s));
                                        break;
                                    }
                                };

                                match msg {
                                    SessionMessage::SessionMessage(dispatch) => {
                                        if let Dispatch::Notification(ref m) = dispatch {
                                            let params = serde_json::to_value(m.params())
                                                .unwrap_or_default();
                                            if let Ok(notif) =
                                                serde_json::from_value::<SessionNotification>(params)
                                            {
                                                match notif.update {
                                                    SessionUpdate::AgentMessageChunk(
                                                        ContentChunk {
                                                            content: ContentBlock::Text(t),
                                                            ..
                                                        },
                                                    ) => {
                                                        tui.push_text(&name, &t.text);
                                                        response_buf.push_str(&t.text);
                                                    }
                                                    SessionUpdate::ToolCall(tc) => {
                                                        tui.push_text(
                                                            &name,
                                                            &format!("\n[tool] {}\n", tc.title),
                                                        );
                                                    }
                                                    SessionUpdate::ToolCallUpdate(tc) => {
                                                        if let Some(title) = &tc.fields.title {
                                                            tui.push_text(
                                                                &name,
                                                                &format!("[update] {}\n", title),
                                                            );
                                                        }
                                                    }
                                                    _ => {}
                                                }
                                            }
                                        }
                                    }
                                    SessionMessage::StopReason(_) => {
                                        tui.push_text(&name, "\n");
                                        // Send response to peer if this was a peer prompt
                                        if let Some(tx) = response_tx.take() {
                                            let _ = tx.send(Ok(response_buf.clone()));
                                        }
                                        break;
                                    }
                                    _ => {}
                                }
                            }
                        }

                        Ok(())
                    })
                    .await
            }
        })
        .await;

    if let Err(e) = result {
        let s = e.to_string();
        // usage_update parse errors kill the connection but aren't fatal
        // — the agent completed the turn, just couldn't parse the final notification
        if s.contains("Parse error")
            || s.contains("unknown variant")
            || s.contains("usage_update")
            || s.contains("deserialization")
        {
            tui.set_state(&name, "error");
            // Don't dump the full error — just note the disconnect
            tui.push_text(&name, "[session ended — schema mismatch]\n");
        } else {
            tui.set_state(&name, "error");
            tui.push_text(&name, &format!("[error] {}\n", s));
        }
    }
}
