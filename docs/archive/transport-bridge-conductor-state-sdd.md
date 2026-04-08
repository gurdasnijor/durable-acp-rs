# SDD: Transport Bridge + ConductorState Dissolution

> Status: Proposed
> Depends on: `architecture-redesign.md` (ConductorState is a grab bag)
> Depends on: `api-architecture-sdd.md` (one client per conductor, REST is read-only + queue mgmt)
> Ref: ACP conductor spec — https://agentclientprotocol.github.io/symposium-acp/conductor.html

## Problem

1. Each `/acp` WebSocket spawns a new conductor + agent subprocess + ConductorState
   (`api.rs:78-103`). Browser reconnects kill the old conductor, breaking MCP bridges
   and Claude Code's connection. The UI gets stuck on "Connecting..." because the
   durable stream's connection row goes stale.

2. `ConductorState` is a god object that mixes stream persistence, prompt queue
   management, and connection identity. It leaks into every layer (api, conductor,
   proxy, main, tests). It should not exist — the `DurableStateProxy` should own what
   it needs, and API routes should use stream primitives directly.

## Root Cause

`handle_acp_session` treats the WebSocket as a lifecycle boundary:

```rust
async fn handle_acp_session(socket: WebSocket, config: Arc<AcpEndpointConfig>) {
    let agent = AcpAgent::from_args(config.agent_command.clone());     // NEW subprocess
    let app = ConductorState::with_shared_streams(...).await;           // NEW state
    let conductor = build_conductor(app, agent);                        // NEW conductor
    conductor.run(AxumWsTransport { socket }).await;                    // dies when WS closes
}
```

The conductor should outlive any individual WebSocket. The stdio path in `main.rs:68-77`
already does this correctly — one conductor for the process lifetime.

---

## Fix 1: Transport Bridge

### SDK Pattern: duplex + ByteStreams

The ACP SDK's own tests (`initialization_sequence.rs`) show the canonical pattern for
decoupling conductor from transport:

```rust
let (editor_out, conductor_in) = duplex(1024);
let (conductor_out, editor_in) = duplex(1024);

Client.builder()
    .with_spawned(|_cx| async move {
        ConductorImpl::new_agent(...)
            .run(ByteStreams::new(conductor_out.compat_write(), conductor_in.compat()))
            .await
    })
    .connect_with(transport, editor_task)
    .await
```

The conductor runs on one end of a `duplex` channel via `ByteStreams`. The client
connects on the other end via whatever transport. Fully decoupled.

### Architecture

```
                    Boot (once)
                    ===========
Agent subprocess  <---->  Conductor
                          (proxy chain)
                              |
                         ByteStreams (duplex -- never EOF)
                              |
                        TransportBridge
                              |
                        Per-connection
                        ==============
                              |
                         attach/detach
                              |
                          WebSocket #N
                              |
                       Browser ACP Client
```

### TransportBridge

The bridge owns the client-side end of the duplex and manages WebSocket attach/detach.
Converts between the byte-oriented duplex and WebSocket text frames.

```rust
pub struct TransportBridge {
    /// Client-side of the duplex pair
    read_half: Mutex<tokio::io::ReadHalf<DuplexStream>>,
    write_half: Mutex<tokio::io::WriteHalf<DuplexStream>>,
}

impl TransportBridge {
    /// Create a bridge and return (bridge, conductor_transport).
    /// The conductor_transport is passed to conductor.run() once.
    pub fn new(buffer_size: usize) -> (Arc<Self>, ByteStreams) {
        let (conductor_stream, client_stream) = tokio::io::duplex(buffer_size);
        let (read_half, write_half) = tokio::io::split(client_stream);

        let bridge = Arc::new(Self {
            read_half: Mutex::new(read_half),
            write_half: Mutex::new(write_half),
        });

        let (conductor_read, conductor_write) = tokio::io::split(conductor_stream);
        let transport = ByteStreams::new(
            conductor_write.compat_write(),
            conductor_read.compat(),
        );

        (bridge, transport)
    }

    /// Forward messages between active WebSocket and the conductor duplex.
    /// Returns when the WebSocket closes. Conductor stays alive.
    pub async fn attach(&self, socket: WebSocket) {
        let (mut ws_write, mut ws_read) = split(socket);
        tokio::select! {
            _ = self.forward_ws_to_conductor(&mut ws_read) => {},
            _ = self.forward_conductor_to_ws(&mut ws_write) => {},
        }
    }
}
```

### Framing

`ByteStreams` is byte-oriented but ACP is newline-delimited JSON-RPC. The bridge
converts:
- **WS → duplex**: each text frame gets `\n` appended
- **duplex → WS**: split on `\n`, send each line as a WS text frame

### Backpressure

Unlike channels, `tokio::io::duplex` has built-in backpressure via its buffer.
When no WebSocket is attached:

- **Conductor writes**: fill the duplex buffer, then block. Naturally throttles the
  conductor.
- **Conductor reads**: blocks waiting for client input. Correct behavior.

When a WebSocket attaches, the bridge drains the duplex read side, unblocking the
conductor. No unbounded buffering, no message loss, no drain-on-attach logic.

**Acceptable tradeoff**: if the conductor fills the buffer while no WS is attached, it
stalls until a WS connects. For durable-acp this is fine — the conductor only writes to
the client transport in response to ACP requests, and the client needs to be connected
to handle those.

### WebSocket Handler (after)

```rust
async fn ws_acp_handler(
    ws: WebSocketUpgrade,
    State(bridge): State<Arc<TransportBridge>>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| async move {
        bridge.attach(socket).await;
        // WS closed -- conductor stays alive, bridge stays alive
    })
}
```

No more `AcpAgent::from_args`, no more `ConductorState::with_shared_streams`, no more
`build_conductor` per connection.

---

## Fix 2: Dissolve ConductorState

### What ConductorState currently holds

| Field | Where it belongs |
|---|---|
| `stream_server: StreamServer` | Already its own type — pass directly |
| `logical_connection_id: String` | Generated at startup, passed as param |
| `runtime: Arc<Mutex<RuntimeState>>` | New `PromptQueue` type |
| `session_to_prompt_turn` | `DurableStateProxy` internal only |

### New type: PromptQueue

Focused type owning the in-memory queue. Lives in `src/prompt_queue.rs`.

Holds: paused flag, active prompt turn, queued prompts (VecDeque), sequence counters.

Methods: `take_next`, `clear_active`, `next_seq`, `set_paused`, `enqueue`,
`cancel_turn`, `cancel_all`, `reorder`.

### After

| Piece | Lives in | Shared with |
|---|---|---|
| `StreamServer` | created in `main.rs` | proxy, API routes |
| `conn_id: String` | generated in `main.rs` | proxy, API routes |
| `PromptQueue` | created in `main.rs` | proxy, API routes |
| `session_to_prompt_turn` | `DurableStateProxy` field | proxy only |
| Write helpers | methods on `DurableStateProxy` | proxy only |

### DurableStateProxy

```rust
pub struct DurableStateProxy {
    pub stream_server: StreamServer,
    pub conn_id: String,
    pub queue: Arc<PromptQueue>,
    session_to_prompt_turn: Arc<RwLock<HashMap<String, String>>>,
}
```

### API route state

```rust
pub struct ApiState {
    pub stream_server: StreamServer,
    pub conn_id: String,
    pub queue: Arc<PromptQueue>,
}
```

### build_conductor

```rust
pub fn build_conductor(
    stream_server: StreamServer,
    conn_id: String,
    queue: Arc<PromptQueue>,
    agent: AcpAgent,
) -> ConductorImpl<sacp::Agent>
```

---

## Boot Sequence (main.rs)

```rust
// 1. Stream server (durable state)
let stream_server = StreamServer::start(...).await?;

// 2. Connection identity
let conn_id = Uuid::new_v4().to_string();

// 3. Prompt queue (shared between proxy and API)
let queue = Arc::new(PromptQueue::new());

// 4. Write initial connection row to durable stream
write_connection_row(&stream_server, &conn_id).await?;

// 5. Transport bridge (duplex pair)
let (bridge, conductor_transport) = TransportBridge::new(64 * 1024);

// 6. Conductor (started ONCE, runs against duplex)
let agent = AcpAgent::from_args(cli.agent_command)?;
let conductor = build_conductor(
    stream_server.clone(), conn_id.clone(), queue.clone(), agent,
);
tokio::spawn(conductor.run(conductor_transport));

// 7. API router
let api = api::router(
    ApiState { stream_server, conn_id, queue },
    bridge,
);
```

---

## What Changes

| Component | Before | After |
|---|---|---|
| `api.rs` AcpEndpointConfig | `agent_command` + `stream_server` | Replaced by `Arc<TransportBridge>` |
| `handle_acp_session` | Creates conductor+agent+state per WS | Calls `bridge.attach(socket)` |
| `main.rs` WebSocket mode | Keeps API alive, no conductor | Starts conductor at boot with bridge |
| Agent subprocess | Per-WebSocket | Per-process (one) |
| ConductorState | Per-WebSocket, god object | Deleted — primitives passed directly |
| API route state | `Arc<ConductorState>` | `ApiState { stream_server, conn_id, queue }` |
| New: `prompt_queue.rs` | — | Focused in-memory queue type |
| New: `TransportBridge` | — | In `transport.rs` |
| Deleted: `conductor_state.rs` | — | Gone |

## What Doesn't Change

- `StreamServer`, `StreamDb`, `StreamSubscriber`, `state.rs` — untouched
- `PeerMcpProxy` — untouched
- All proxy message interception logic — same behavior, different field access
- React/TS code — untouched
- `client.rs`, `registry.rs`, `webhook.rs`, `acp_registry.rs` — untouched

## Order of Implementation

1. `src/prompt_queue.rs` — new (done)
2. `src/transport.rs` — add `TransportBridge`
3. `src/durable_state_proxy.rs` — replace `Arc<ConductorState>` with direct primitives
4. `src/api.rs` — `ApiState` + bridge-based WS handler
5. `src/conductor.rs` — take primitives instead of `Arc<ConductorState>`
6. `src/main.rs` — boot conductor once with bridge
7. `src/lib.rs` — add `prompt_queue` module, remove `conductor_state`
8. `src/bin/durable-state-proxy.rs` — update
9. Tests — update all
10. Delete `src/conductor_state.rs`

## Open Questions

1. **Stdio + WS coexistence**: Should `main.rs` support both simultaneously? Current
   proposal: mutually exclusive based on TTY detection. Stdio mode uses `ByteStreams`
   on stdin/stdout directly. WS mode uses bridge.

2. **Multiple concurrent WS clients**: New attach replaces old. Could extend to
   fan-out later if needed.

3. **Duplex buffer sizing**: 64KB should be sufficient for JSON-RPC messages.







----------
# Handoff: Transport Architecture Fix

## Context

The `durable-acp-rs` Rust conductor binary manages an ACP proxy chain between a client and an agent (e.g. Claude Code). Flamecast's React UI connects to it to observe state (via SSE/durable stream) and submit prompts.

Key repos:
- `/Users/gnijor/gurdasnijor/durable-acp-rs` — Rust conductor binary
- `/Users/gnijor/smithery/flamecast` — Flamecast monorepo (React UI + packages)
- ACP SDK: `/Users/gnijor/.cargo/git/checkouts/rust-sdk-762d172c5e846930/ec9ceae/`

## The Bug

Each `/acp` WebSocket connection spawns a new conductor + agent subprocess + ConductorState (`api.rs:78-103`). When the browser reconnects (page reload, HMR), the old conductor dies, killing the MCP bridge. Claude Code loses its MCP connection. UI stuck on "Connecting..."

## Root Cause

The browser is wired as a direct ACP client to the conductor via WebSocket. WebSocket connections are inherently ephemeral (page reloads kill them). The ACP SDK is designed so that the conductor lives and dies with its transport — `conductor.run(transport)` runs in reactive mode until the transport closes, then everything (conductor, agent subprocess, MCP bridges) is dropped.

This is correct for editors (Zed, VS Code) that connect via stable stdio pipes. It's wrong for a browser.

## What We Explored

1. **TransportBridge approach** — Custom bridge between WebSocket and conductor using `tokio::io::duplex`. Conductor runs on one end, WebSocket attaches/detaches from the other. Written but not wired up. Design doc at `docs/transport-bridge-design.md` and SDD at `docs/transport-bridge-conductor-state-sdd.md`.

2. **Shared ConductorState** — Other agent partially implemented sharing ConductorState across WebSocket connections but still creates a new conductor + agent subprocess per WS. Half-fix that doesn't solve the core problem. See uncommitted changes in durable-acp-rs (`src/api.rs`, `src/transport.rs`, `src/prompt_queue.rs`).

3. **ConductorState dissolution** — Identified that `ConductorState` is a god object. Extracted `PromptQueue` (`src/prompt_queue.rs`, done). Full dissolution planned but not executed.

## The Right Architecture

The browser should NOT be a direct ACP client to the conductor. Instead, the server itself should be the ACP client using the SDK's standard patterns.

### Current (broken)

```
Browser --WebSocket/ACP--> Conductor --proxy chain--> Agent
         (ephemeral)        (dies on WS close)
```

### Correct

```
Browser --REST/SSE--> durable-acp-rs server
                           |
                      internal ACP client (stable, server-scoped)
                           |
                      Conductor --proxy chain--> Agent
                           |
                      DurableStateProxy writes to durable stream
                           |
                      Browser observes via SSE
```

The server acts as the ACP client using `Client.builder().connect_with(transport, ...)` on a duplex pair. The conductor runs on the other end. API routes inject prompts through a channel to the internal client. The `DurableStateProxy` intercepts everything and writes to the durable stream. The browser observes via SSE and sends commands via REST.

No external ACP transport from the browser. No WebSocket to the conductor. No bridge needed.

### Why this works

- **Conductor lifecycle**: tied to the server process, not to a browser tab
- **Agent subprocess**: started once, lives for the server's lifetime
- **MCP bridges**: survive browser reconnects because the conductor never dies
- **Standard SDK usage**: `Client.builder().connect_with()` + `conductor.run()` on a duplex — same pattern as SDK tests (`initialization_sequence.rs`, `nested_conductor.rs`)
- **No custom infrastructure**: uses SDK primitives (ByteStreams, duplex, ConnectTo)

### What the `/acp` WebSocket endpoint becomes

Either:
- **Removed entirely** — browser doesn't need it, prompts go via REST
- **Kept for real ACP clients** (editors) that want to connect to this conductor — but the browser UI doesn't use it

### Seam changes

| Seam | Before | After |
|------|--------|-------|
| State observation | SSE/durable stream | Unchanged |
| Prompt submission | Browser → WebSocket/ACP → Conductor | Browser → REST → Server → internal ACP client → Conductor |
| Queue management | REST API | Unchanged |
| Permissions | WebSocket/ACP roundtrip | Durable stream (agent writes permission to stream, browser observes via SSE, browser resolves via REST, server forwards to internal client) |

## Existing Work (in durable-acp-rs working tree)

- `src/prompt_queue.rs` — new, clean extraction of queue logic from ConductorState. Ready to use.
- `src/transport.rs` — `TransportBridge` added but based on the old approach. The duplex/ByteStreams part is reusable; the WS attach/detach logic is no longer needed.
- `src/api.rs` — partial changes (shared ConductorState). Needs full rewrite per new architecture.
- `ts/packages/server/src/react.tsx` — session state changes. Unrelated.

## Key SDK References

- `ConductorImpl::run()`: `src/agent-client-protocol-conductor/src/conductor.rs:226-269` — uses `Builder.connect_to(transport)` in reactive mode
- `AcpAgent::connect_to()`: `src/agent-client-protocol-tokio/src/acp_agent.rs:280-371` — spawns subprocess, creates Lines transport, kills on drop via ChildGuard
- `ConnectTo` trait: consumed on use, one-shot — everything is designed to be per-connection
- Test patterns: `tests/initialization_sequence.rs`, `tests/nested_conductor.rs` — duplex + ByteStreams for in-process conductor

## Implementation Sketch

```rust
// main.rs boot
let stream_server = StreamServer::start(...).await?;
let conn_id = Uuid::new_v4().to_string();
let queue = Arc::new(PromptQueue::new());

// duplex pair: server (client role) <-> conductor
let (client_stream, conductor_stream) = tokio::io::duplex(64 * 1024);

// Conductor side (agent mode, reactive)
let agent = AcpAgent::from_args(cli.agent_command)?;
let conductor = build_conductor(stream_server.clone(), conn_id.clone(), queue.clone(), agent);
tokio::spawn(async move {
    conductor.run(ByteStreams::new(
        conductor_write.compat_write(),
        conductor_read.compat(),
    )).await
});

// Client side (active mode, drives sessions)
let (prompt_tx, prompt_rx) = mpsc::unbounded_channel();
tokio::spawn(async move {
    Client.builder()
        .connect_with(ByteStreams::new(
            client_write.compat_write(),
            client_read.compat(),
        ), async |cx| {
            // Initialize, create session, then loop on prompts from API
            let init = cx.send_request(InitializeRequest::new(...)).block_task().await?;
            cx.build_session_cwd()?.run_until(async |session| {
                while let Some(text) = prompt_rx.recv().await {
                    session.send_prompt(&text)?;
                    // Response flows through DurableStateProxy to stream
                }
                Ok(())
            }).await
        })
        .await
});

// API routes receive prompt_tx to inject prompts
let api = api::router(ApiState { stream_server, conn_id, queue, prompt_tx });
```

## Open Questions for Next Agent

1. **Permission flow**: Agent sends permission request → proxy writes to stream → browser sees it via SSE → browser responds via REST → how does the REST handler resolve the ACP permission? Needs access to the client connection or a channel.

2. **Session management**: The internal client creates one session. What happens when the browser wants multiple sessions or the session needs to be recreated?

3. **ConductorState dissolution**: Still needed. `PromptQueue` is done. `DurableStateProxy` needs to take primitives instead of `Arc<ConductorState>`. See SDD for details.

4. **`/acp` WebSocket endpoint**: Keep for external ACP clients, or remove entirely?

5. **Existing partial work**: The other agent's changes in durable-acp-rs working tree need to be reconciled — keep `prompt_queue.rs`, rework everything else.
