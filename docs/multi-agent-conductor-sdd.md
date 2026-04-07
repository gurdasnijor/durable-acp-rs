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
│  │  - Permission prompts inline                            │ │
│  │  - Input bar for prompts                                │ │
│  │  - Tab to switch agents                                 │ │
│  └────────┬────────────────────────────────────────────────┘ │
│           │ in-process channels                              │
│  ┌────────▼────────────────────────────────────────────────┐ │
│  │  Agent Manager                                          │ │
│  │                                                         │ │
│  │  Shared: EmbeddedDurableStreams (one server, one stream) │ │
│  │  Shared: REST API (one port, all agents' state)         │ │
│  │                                                         │ │
│  │  agent-a:                                               │ │
│  │    Client.builder()                                     │ │
│  │      .on_receive_request(permission_handler)            │ │
│  │      .with_spawned(conductor_a)                         │ │
│  │      .connect_with(transport, session_loop)             │ │
│  │    AppState(shared_streams, "agent-a")                   │ │
│  │    prompt_tx / output_rx channels                       │ │
│  │                                                         │ │
│  │  agent-b:                                               │ │
│  │    Client.builder()                                     │ │
│  │      .on_receive_request(permission_handler)            │ │
│  │      .with_spawned(conductor_b)                         │ │
│  │      .connect_with(transport, session_loop)             │ │
│  │    AppState(shared_streams, "agent-b")                   │ │
│  │    prompt_tx / output_rx channels                       │ │
│  └─────────────────────────────────────────────────────────┘ │
└──────────────────────────────────────────────────────────────┘
```

## Runtime Model

### The LocalSet + iocraft solution

`LocalSet::run_until(fut)` doesn't "own the thread" exclusively — it pins
`!Send` futures to the current thread. iocraft's `render_loop()` is a normal
future. Nest them:

```rust
#[tokio::main(flavor = "current_thread")]
async fn main() {
    let local = tokio::task::LocalSet::new();
    local.run_until(async {
        // Spawn conductor tasks (!Send, need spawn_local)
        for agent in agents {
            tokio::task::spawn_local(run_conductor(agent));
        }
        // TUI render loop runs on same LocalSet — conductors
        // execute between frames/events
        smol::block_on(element!(Dashboard(...)).render_loop()).unwrap();
    }).await;
}
```

The `spawn_local` tasks run whenever the render loop yields (every frame/event).
No separate threads, no channel bridging, no fighting.

## How It Works

### 1. Process startup

```rust
#[tokio::main(flavor = "current_thread")]
async fn main() {
    let agents = load_agents_toml();
    let registry = fetch_acp_registry().await;

    // One shared durable streams server
    let durable_streams = EmbeddedDurableStreams::start(bind, "durable-acp-state").await;

    // One shared REST API
    spawn_api_server(api_port, durable_streams.clone()).await;

    let local = tokio::task::LocalSet::new();
    local.run_until(async {
        let mut manager = AgentManager::new();

        for config in agents {
            let command = resolve_command(&config, &registry);
            manager.spawn_agent(config, command, durable_streams.clone()).await;
        }

        // Run TUI — conductors interleave on same thread
        smol::block_on(
            element!(Dashboard(manager: manager.handle())).render_loop()
        ).unwrap();
    }).await;
}
```

### 2. Spawning an agent

Each agent gets its own `ConductorImpl` wired into a `Client.builder()` via
`with_spawned`. This is the pattern from the conductor tests:

```rust
impl AgentManager {
    async fn spawn_agent(
        &mut self,
        config: AgentConfig,
        command: Vec<String>,
        durable_streams: EmbeddedDurableStreams,
    ) {
        let agent = AcpAgent::from_args(command);
        let app = AppState::with_shared_streams(durable_streams, &config.name);

        let (prompt_tx, prompt_rx) = mpsc::unbounded_channel();
        let (output_tx, output_rx) = mpsc::unbounded_channel();
        let (perm_tx, perm_rx) = mpsc::unbounded_channel();

        tokio::task::spawn_local(async move {
            Client.builder()
                .name(&config.name)
                .on_receive_request(
                    permission_handler(perm_tx, perm_rx),
                    on_receive_request!(),
                )
                .with_spawned(|_cx| async move {
                    ConductorImpl::new_agent(
                        config.name.clone(),
                        ProxiesAndAgent::new(agent)
                            .proxy(DurableStateProxy { app })
                            .proxy(PeerMcpProxy),
                        McpBridgeMode::default(),
                    )
                })
                .connect_with(transport, async |connection| {
                    connection.send_request(InitializeRequest::new(ProtocolVersion::V1))
                        .block_task().await?;

                    connection.build_session_cwd()?
                        .block_task()
                        .run_until(async |mut session| {
                            output_tx.send(Output::Ready);

                            while let Some(text) = prompt_rx.recv().await {
                                session.send_prompt(&text)?;
                                loop {
                                    match session.read_update().await? {
                                        SessionMessage::SessionMessage(dispatch) => {
                                            // Parse notification, send to TUI
                                            output_tx.send(Output::Chunk(parse(dispatch)));
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
            perm_rx,
        });
    }
}
```

### 3. TUI interaction

The TUI reads from `output_rx` channels and writes to `prompt_tx` channels.
No REST API, no HTTP, no SSE — pure in-process message passing.

```rust
// User types prompt, hits enter
agent.prompt_tx.send(text);

// TUI polls output channels (in use_future or render loop)
while let Ok(output) = agent.output_rx.try_recv() {
    match output {
        Output::Ready => set_agent_state("ready"),
        Output::Chunk(chunk) => append_output(chunk),
        Output::Done(reason) => append_output("\n"),
        Output::Permission(req) => show_permission_prompt(req),
    }
}
```

### 4. Permission handling (interactive)

When an agent requests permission, the handler sends the request to the TUI
via channel and awaits the user's response:

```rust
enum Output {
    Ready,
    Chunk(ChunkData),
    Done(StopReason),
    Permission(PermissionRequest),
}

enum PermissionResponse {
    Approve(PermissionOptionId),
    Deny,
}
```

**Flow:**

```
Agent requests permission
  → Client.on_receive_request handler fires
  → Handler sends Output::Permission(req) to TUI via output_tx
  → Handler awaits response on perm_response_rx channel
  → TUI displays permission prompt inline in the output pane:

     ┌─ Output — agent-a ──────────────────────────────┐
     │ I need to read the file src/main.rs             │
     │                                                  │
     │ ⚠ Permission: Read file src/main.rs             │
     │   [1] Always Allow  [2] Allow  [3] Deny         │
     │   Choose (1-3): _                                │
     └──────────────────────────────────────────────────┘

  → User presses 1/2/3
  → TUI sends PermissionResponse via perm_response_tx
  → Handler receives response, calls responder.respond()
  → Agent continues
```

**Implementation:**

```rust
fn permission_handler(
    output_tx: Sender<Output>,
    response_rx: Receiver<PermissionResponse>,
) -> impl AsyncFnMut(RequestPermissionRequest, Responder, ConnectionTo) -> Result<()> {
    async move |req, responder, cx| {
        // Send to TUI
        output_tx.send(Output::Permission(PermissionRequest {
            agent_name: agent_name.clone(),
            title: req.tool_call.fields.title.clone(),
            options: req.options.clone(),
            response_tx: oneshot::channel(),
        }));

        // Wait for user response (this runs on the event loop,
        // but cx.spawn moves it off so the connection keeps processing)
        cx.spawn(async move {
            let response = response_rx.recv().await;
            let outcome = match response {
                Some(PermissionResponse::Approve(id)) =>
                    RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(id)),
                _ => RequestPermissionOutcome::Cancelled,
            };
            responder.respond(RequestPermissionResponse::new(outcome))
        })?;
        Ok(())
    }
}
```

**Key detail:** The permission handler uses `cx.spawn()` to move the blocking
wait off the event loop. This is critical — the cookbook warns: "Message
handlers run on the event loop. Blocking in a handler prevents the connection
from processing new messages." The `cx.spawn` creates a task that awaits the
user's response without blocking.

**TUI key handling during permission prompt:**

When a permission is pending, the TUI switches input mode. Instead of typing
into the prompt bar, number keys (1/2/3) resolve the permission:

```rust
// In use_terminal_events
if permission_pending.get() {
    match code {
        KeyCode::Char('1') => perm_response_tx.send(Approve(options[0].id)),
        KeyCode::Char('2') => perm_response_tx.send(Approve(options[1].id)),
        KeyCode::Char('3') | KeyCode::Char('n') => perm_response_tx.send(Deny),
        _ => {}
    }
}
```

### 5. Shared state infrastructure

Per the reference architecture, there is **one** durable streams server and
**one** state stream for all agents. Multiple conductors write to the same
stream, differentiated by `logical_connection_id`.

```rust
// One shared durable streams server + StreamDB
let durable_streams = EmbeddedDurableStreams::start(bind, "durable-acp-state").await?;

// Each agent gets its own AppState but shares the durable streams server
let app_a = AppState::with_shared_streams(durable_streams.clone(), "agent-a");
let app_b = AppState::with_shared_streams(durable_streams.clone(), "agent-b");
```

This means:
- **One HTTP port** for durable streams (not N)
- **One REST API port** for all agents (not N)
- **One state stream** with all connections, turns, chunks
- External clients see all agents' state in one subscription
- The `StreamDB` already keys everything by `logical_connection_id`

### 6. Agent-to-agent (peer) communication

The `PeerMcpProxy` provides `list_agents` and `prompt_agent` tools. Since
all agents share one process, peer communication can go through in-process
channels instead of HTTP. The `prompt_agent` tool sends a prompt to a peer's
channel and streams the response back — zero network overhead.

The REST API remains for:
- External clients subscribing to state
- Remote agent-to-agent messaging (future)
- Programmatic access from scripts/CLIs

## Key Decisions

1. **One process, N conductors** — each conductor manages one proxy chain.
   No changes to `ConductorImpl` needed.

2. **`Client.builder().with_spawned().connect_with()`** — the pattern from
   the conductor tests. Client and conductor in same process, connected via
   in-memory transport.

3. **In-process channels for TUI ↔ agent** — `mpsc` channels carry prompts
   down and streamed chunks + permission requests up. No HTTP round-trips.

4. **Shared durable streams** — one embedded server, one state stream, one
   REST API. All conductors write to the same stream, keyed by
   `logical_connection_id`. Matches the reference architecture.

5. **In-process peering** — `prompt_agent` routes through channels, not HTTP,
   when the target agent is in the same process.

6. **Single `LocalSet`** — all conductors share one LocalSet on one thread.
   sacp futures are `!Send`, so everything runs on the same thread. This is
   fine — the actual work (LLM inference) happens in the agent subprocess,
   not in the conductor.

7. **iocraft render loop inside `LocalSet::run_until`** — the TUI future
   runs on the same LocalSet as the conductor tasks. Conductor tasks
   execute between render frames when the TUI yields.

8. **Interactive permissions** — permission requests route to the TUI via
   `Output::Permission`. The handler uses `cx.spawn()` to await the user's
   response without blocking the event loop. The TUI switches to permission
   input mode (number keys) while a request is pending.

## Error Handling

- If one conductor crashes, the others continue. Each runs in its own
  `spawn_local` task.
- Crashed agents show error status in the TUI sidebar (● → ✕).
- The TUI remains responsive — it's not blocked on any single agent.
- Parse errors from unknown message types (e.g. `usage_update`) are
  skipped, not fatal.

## Files to Change

| File | Change |
|---|---|
| `src/bin/dashboard.rs` | Rewrite: in-process conductors, LocalSet+iocraft, permission routing |
| `src/app.rs` | Add `AppState::with_shared_streams()` constructor |
| `src/conductor.rs` | No changes |
| `src/peer_mcp.rs` | Add in-process peer routing option alongside HTTP |
| `src/api.rs` | Serve all agents' state from shared StreamDB |
| `Cargo.toml` | No new deps needed |

## What This Replaces

The current `dashboard.rs` spawns N conductor subprocesses and talks to them
via REST API. The new version:
- Eliminates subprocess management (conductors are in-process)
- Eliminates REST API latency for prompt submission
- Gets streaming responses via in-process channels instead of SSE
- Has a single process to start/stop
- Interactive permission handling (not auto-approve)
- Shared state stream for all agents
- Still runs REST API for external access
