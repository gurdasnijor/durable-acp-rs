# SDD: Single-Process Multi-Agent Conductor

## Problem

The current multi-agent setup spawns N conductor processes and communicates
with them via REST API + SSE. This is fragile:
- Process lifecycle management is error-prone (kill signals, cleanup)
- REST API round-trips add latency vs. in-process message passing
- The TUI render loop and tokio runtime fight over the event loop
- N processes × M proxies = too many moving parts

## Insight from the Conductor Spec

The conductor spec shows:

```
Terminal Client
    ├── terminal client (role)
    └── terminal agent (role)
           │
      Conductor
       ├── conductor → proxy → Context Proxy
       ├── conductor → proxy → Tool Filter Proxy
       └── terminal client → terminal agent → Terminal Agent
```

The conductor manages **one proxy chain** per instance. But nothing prevents
**one process from running N conductors**. Each conductor is just a set of
connected components — there's no global state that prevents multiple
conductors coexisting.

The dashboard process acts as the **Terminal Client** for all conductors.
It holds N `ConductorImpl` instances, each with its own proxy chain and agent.
Sessions are scoped per-conductor — the dashboard routes prompts to the right
conductor based on the user's agent selection.

## Architecture

```
┌──────────────────────────────────────────────────────────────┐
│  Single Process (dashboard binary)                            │
│                                                              │
│  ┌─────────────────────────────────────────────────────────┐ │
│  │  TUI (iocraft)                                          │ │
│  │  - Agent sidebar with status                            │ │
│  │  - Output pane with streamed responses                  │ │
│  │  - Input bar for prompts                                │ │
│  │  - Tab to switch agents                                 │ │
│  └────────┬────────────────────────────────────────────────┘ │
│           │ in-process channels                              │
│  ┌────────▼────────────────────────────────────────────────┐ │
│  │  Agent Manager                                          │ │
│  │                                                         │ │
│  │  agent-a:                                               │ │
│  │    Client.builder()                                     │ │
│  │      .connect_with(conductor_a, |conn| { ... })         │ │
│  │    conductor_a = ConductorImpl::new_agent(              │ │
│  │      ProxiesAndAgent::new(AcpAgent("claude-acp"))       │ │
│  │        .proxy(DurableStateProxy { app_a })              │ │
│  │        .proxy(PeerMcpProxy)                             │ │
│  │    )                                                    │ │
│  │    session_a: ActiveSession                             │ │
│  │                                                         │ │
│  │  agent-b:                                               │ │
│  │    Client.builder()                                     │ │
│  │      .connect_with(conductor_b, |conn| { ... })         │ │
│  │    conductor_b = ConductorImpl::new_agent(              │ │
│  │      ProxiesAndAgent::new(AcpAgent("gemini"))           │ │
│  │        .proxy(DurableStateProxy { app_b })              │ │
│  │        .proxy(PeerMcpProxy)                             │ │
│  │    )                                                    │ │
│  │    session_b: ActiveSession                             │ │
│  │                                                         │ │
│  └─────────────────────────────────────────────────────────┘ │
│                                                              │
│  Each conductor spawns its agent as a subprocess (stdio).    │
│  The TUI and all conductors share one tokio runtime.         │
│  No REST API needed for prompt submission — use in-process   │
│  ActiveSession::send_prompt() directly.                      │
└──────────────────────────────────────────────────────────────┘
```

## How It Works

### 1. Process startup

```rust
fn main() {
    let agents = load_agents_toml();
    let registry = fetch_acp_registry();

    // Single tokio runtime + LocalSet (required by sacp)
    let local_set = tokio::task::LocalSet::new();
    local_set.run_until(async {
        let mut manager = AgentManager::new();

        for config in agents {
            let command = resolve_command(&config, &registry);
            manager.spawn_agent(config, command).await;
        }

        // Run TUI — manager provides channels for prompt/response
        run_tui(manager).await;
    });
}
```

### 2. Spawning an agent

Each agent gets its own `ConductorImpl` wired into a `Client.builder()` via
`with_spawned`. This is the pattern from the conductor tests:

```rust
impl AgentManager {
    async fn spawn_agent(&mut self, config: AgentConfig, command: Vec<String>) {
        let agent = AcpAgent::from_args(command);
        let app = AppState::new(bind, config.state_stream).await;

        // Client connects to conductor, conductor connects to agent
        let (prompt_tx, prompt_rx) = channel();
        let (output_tx, output_rx) = channel();

        tokio::task::spawn_local(async move {
            Client.builder()
                .name(&config.name)
                .on_receive_request(
                    // Handle permissions
                    async |req: RequestPermissionRequest, responder, _cx| {
                        // auto-approve or route to TUI
                    },
                    on_receive_request!(),
                )
                .with_spawned(|_cx| async move {
                    // The conductor runs inside with_spawned
                    ConductorImpl::new_agent(
                        config.name.clone(),
                        ProxiesAndAgent::new(agent)
                            .proxy(DurableStateProxy { app })
                            .proxy(PeerMcpProxy),
                        McpBridgeMode::default(),
                    )
                })
                .connect_with(transport, async |connection| {
                    // Initialize
                    connection.send_request(InitializeRequest::new(ProtocolVersion::V1))
                        .block_task().await?;

                    // Create session
                    connection.build_session_cwd()?
                        .block_task()
                        .run_until(async |mut session| {
                            // Signal ready
                            output_tx.send(Output::Ready);

                            // Process prompts from TUI
                            while let Some(text) = prompt_rx.recv().await {
                                session.send_prompt(&text)?;
                                loop {
                                    match session.read_update().await? {
                                        SessionMessage::SessionMessage(dispatch) => {
                                            // Parse and send to TUI
                                            output_tx.send(Output::Chunk(text));
                                        }
                                        SessionMessage::StopReason(reason) => {
                                            output_tx.send(Output::Done(reason));
                                            break;
                                        }
                                    }
                                }
                            }
                            Ok(())
                        })
                        .await
                })
                .await;
        });

        self.agents.push(AgentHandle {
            name: config.name,
            prompt_tx,
            output_rx,
        });
    }
}
```

### 3. TUI interaction

The TUI reads from `output_rx` channels and writes to `prompt_tx` channels.
No REST API, no HTTP, no SSE — pure in-process message passing.

```rust
// User types prompt, hits enter
let text = input_buf.to_string();
let agent = &agents[selected_agent];
agent.prompt_tx.send(text);

// TUI polls output channels for streaming responses
for agent in &agents {
    while let Ok(output) = agent.output_rx.try_recv() {
        match output {
            Output::Ready => set_agent_state(&agent.name, "ready"),
            Output::Chunk(text) => append_output(&agent.name, &text),
            Output::Done(reason) => append_output(&agent.name, "\n"),
        }
    }
}
```

### 4. Agent-to-agent (peer) communication

Same as before — the `PeerMcpProxy` provides `list_agents` and `prompt_agent`
tools. These still use the REST API to talk to peers because each conductor
has its own `AppState` with its own embedded durable streams server and REST
API. The REST API remains useful for:
- External clients subscribing to state
- Agent-to-agent messaging across process boundaries
- Programmatic access from scripts/CLIs

The difference: the REST API is no longer needed for the TUI's own prompt
submission. That goes through in-process channels.

## Key Decisions

1. **One process, N conductors** — each conductor manages one proxy chain.
   No changes to `ConductorImpl` needed.

2. **`Client.builder().with_spawned().connect_with()`** — the pattern from
   the conductor tests. The client and conductor run in the same process,
   connected via in-memory transport.

3. **In-process channels for TUI ↔ agent** — `mpsc` channels carry prompts
   down and streamed chunks up. No HTTP round-trips.

4. **REST API still runs per-agent** — for external access and peer messaging.
   The TUI just doesn't use it for its own prompts.

5. **Single `LocalSet`** — all conductors share one LocalSet on one thread.
   sacp futures are `!Send`, so everything runs on the same thread. This is
   fine — the actual work (LLM inference) happens in the agent subprocess,
   not in the conductor.

6. **iocraft TUI runs on the same thread** — `smol::block_on()` drives the
   render loop. It needs to yield to the LocalSet for conductor tasks. This
   may require using `iocraft`'s async render hooks (`use_future`) to poll
   channels, or running the TUI on a separate thread with the channels
   bridging the gap.

## Open Questions

- **TUI + LocalSet coexistence**: iocraft's `render_loop()` and tokio's
  `LocalSet::run_until()` both want to own the thread. Options:
  - Run TUI on a separate OS thread, bridge via `Arc<Mutex>` + channels
  - Use iocraft's `use_future` hook to poll inside the render loop
  - Use a custom executor that interleaves both

- **Permission routing**: When an agent requests permission, should the TUI
  show an interactive prompt? Or auto-approve? If interactive, the permission
  handler needs to send to the TUI channel and await a response.

- **Error handling**: If one conductor crashes, the others should continue.
  Each conductor runs in its own `spawn_local` — a panic in one shouldn't
  bring down the process.

## Files to Change

| File | Change |
|---|---|
| `src/bin/dashboard.rs` | Rewrite: remove subprocess spawning, use `Client.builder().with_spawned()` pattern |
| `src/conductor.rs` | No changes — `build_conductor_with_peer_mcp` already returns `ConductorImpl` |
| `src/app.rs` | No changes — each agent gets its own `AppState` |
| `Cargo.toml` | No new deps needed |

## What This Replaces

The current `dashboard.rs` spawns N conductor subprocesses and talks to them
via REST API. The new version:
- Eliminates subprocess management
- Eliminates REST API latency for prompt submission
- Gets streaming responses via in-process channels instead of SSE
- Has a single process to start/stop
- Still runs the REST API per-agent for external/peer access
