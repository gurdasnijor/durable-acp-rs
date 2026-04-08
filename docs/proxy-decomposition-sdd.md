# SDD: Proxy Decomposition — Kill ConductorState, Kill build_conductor

> Status: Proposed
> Supersedes: `transport-bridge-conductor-state-sdd.md` "Fix 2: Dissolve ConductorState"
> Ref: P/ACP spec — https://agentclientprotocol.github.io/symposium-acp/proxying-acp.html
> Ref: Sparkle component — https://github.com/sparkle-ai-space/sparkle-mcp/blob/main/src/acp_component.rs
> Ref: ACP cookbook — `reusable_components`, `running_proxies_with_conductor`, `per_session_mcp_server`

## Problem

Two anti-patterns in the current codebase:

### Anti-pattern 1: ConductorState god object

`ConductorState` is a god object that leaks across every layer:

```
conductor_state.rs (298 lines)
├── StreamServer ownership       ← infrastructure
├── logical_connection_id        ← identity
├── RuntimeState (Mutex)         ← transient queue
│   ├── paused: bool
│   ├── active_prompt_turn
│   ├── queued: VecDeque<QueuedPrompt>
│   └── seq_by_prompt_turn
├── session_to_prompt_turn       ← prompt tracking
├── write_state_event()          ← persistence
├── update_connection()          ← persistence
├── enqueue_prompt()             ← queue management
├── take_next_prompt()           ← queue management
├── cancel_queued_turn()         ← queue management
├── cancel_all_queued()          ← queue management
├── reorder_queue()              ← queue management
├── set_paused()                 ← queue management
├── record_chunk()               ← persistence
└── finish_prompt_turn()         ← persistence
```

This has three problems:

1. **Mixed concerns.** The prompt queue is transient runtime state (never journaled to
   the durable stream). State persistence is a cross-cutting observation concern. These
   are orthogonal and shouldn't share a type.

2. **The conductor doesn't need any of this.** `ConductorImpl` composes proxies and
   routes messages. It has no business holding a reference to StreamServer, a connection
   ID, or a prompt queue. Those are *proxy* concerns.

3. **Violates the P/ACP principle.** The whole point of proxy chains is that cross-cutting
   concerns are separate, composable components. The queue is one concern. State
   observation is another. They should be separate proxies.

### Anti-pattern 2: Hand-rolled initialization

`build_conductor` and `ConductorState::init()` hand-write initialization entrypoints
that the SDK already provides patterns for:

```rust
// Current: custom build_conductor wrapper
pub fn build_conductor(app: Arc<ConductorState>, agent: AcpAgent) -> ConductorImpl<sacp::Agent> {
    ConductorImpl::new_agent(
        "durable-acp".to_string(),
        ProxiesAndAgent::new(agent)
            .proxy(DurableStateProxy { app })
            .proxy(PeerMcpProxy),
        McpBridgeMode::default(),
    )
}
```

```rust
// Current: ConductorState::init() does manual initialization
async fn init(stream_server: StreamServer) -> Result<Self> {
    let logical_connection_id = Uuid::new_v4().to_string();
    let app = Self { ... };
    app.write_state_event("connection", "insert", ...).await?;
    Ok(app)
}
```

The SDK cookbook (`reusable_components`) shows the canonical pattern: each component
is a struct implementing `ConnectTo<Conductor>`. State lives in the struct fields and
gets captured in the `connect_to` closure scope. There is no wrapper function — the
conductor is composed inline using `ProxiesAndAgent`.

The cookbook's `per_session_mcp_server` pattern shows how session-level initialization
(like writing a connection row) belongs inside the proxy's `on_receive_request_from`
handler for `NewSessionRequest`, not in a separate `init()` method.

Sparkle's `acp_component.rs` demonstrates this at scale: all state (`PendingEmbodiments`,
`SessionDbs`, `PromptCounter`, `ResponseBuffer`) lives in the `serve()` closure scope.
No shared god object. No wrapper function.

## Proposal: Two Proxies, No ConductorState, No build_conductor

Split into two independent proxy components plus a shared infrastructure struct:

```
Client → QueueProxy → DurableStateProxy → PeerMcpProxy → Agent
            │                  │
            │                  └── observes all messages, journals to durable stream
            └── holds prompts, releases one at a time, manages pause/cancel/reorder
```

### QueueProxy

**Concern:** Prompt flow control. When do prompts reach the agent?

Intercepts `PromptRequest` from Client. Holds them in an in-memory queue. Releases
one at a time (serial execution). Provides pause/resume/cancel/reorder.

Writes prompt lifecycle state events to the durable stream:
- `prompt_turn` insert (state: queued)
- `prompt_turn` update (state: active) when released
- `prompt_turn` update (state: completed/cancelled/broken) on result/cancel
- `pending_request` insert/update for the prompt's JSONRPC request

```rust
pub struct QueueProxy {
    stream_server: StreamServer,
    conn_id: String,
    queue: Arc<PromptQueue>,  // exposed to API routes for management
}
```

`PromptQueue` (already extracted in `src/prompt_queue.rs`) holds the transient queue
state: `VecDeque<QueuedPrompt>`, paused flag, active turn, sequence counters.

The API routes get `Arc<PromptQueue>` to call `set_paused`, `cancel_turn`,
`cancel_all`, `reorder`.

### DurableStateProxy

**Concern:** State observation. What happened during the session?

Intercepts all messages flowing through the chain and journals content to the durable
stream. Does NOT manage the queue. Does NOT hold `QueuedPrompt` or `Responder`.

Records:
- `SessionNotification` → chunk rows (text, thinking, tool_call, tool_result)
- `RequestPermissionRequest/Response` → permission rows
- `CreateTerminalRequest` → terminal rows
- `TerminalOutputRequest` → tool_result chunks
- `NewSessionRequest` → connection state update (attached, cwd, session_id)

```rust
pub struct DurableStateProxy {
    stream_server: StreamServer,
    conn_id: String,
    session_to_prompt_turn: Arc<RwLock<HashMap<String, String>>>,
}
```

### StreamServer stays where it is

`StreamServer` is infrastructure. It's created in `main.rs` and passed to whichever
components need it. It's not owned by any conductor-level abstraction.

### ConductorState is deleted

Nothing replaces it.

### conductor.rs is deleted

`build_conductor` is a thin wrapper over `ConductorImpl::new_agent` that exists only
because `ConductorState` needed to be threaded through. Without `ConductorState`, the
conductor composition is a single expression in `main.rs` — following the cookbook's
`running_proxies_with_conductor` pattern:

```rust
// Cookbook pattern: inline composition, no wrapper
let conductor = ConductorImpl::new_agent(
    "durable-acp",
    ProxiesAndAgent::new(agent)
        .proxy(queue_proxy)
        .proxy(state_proxy)
        .proxy(PeerMcpProxy),
    McpBridgeMode::default(),
);
conductor.run(transport).await?;
```

Each proxy is a `ConnectTo<Conductor>` per the cookbook's `reusable_components` pattern.
The conductor doesn't know or care what they do internally.

---

## Design Decision: Proxy Chain Ordering

The ordering of QueueProxy and DurableStateProxy in the chain affects what each proxy
sees:

### Option A: Client → QueueProxy → DurableStateProxy → Agent (proposed)

QueueProxy intercepts the prompt from the client. When it releases the prompt, it
flows through DurableStateProxy on the way to the agent.

- **QueueProxy sees:** original prompt submission from client
- **DurableStateProxy sees:** prompt only when released from queue (= active)
- **DurableStateProxy sees:** all agent responses, notifications, permissions

QueueProxy writes the prompt lifecycle events (queued → active → completed) because
it's the one managing those transitions. DurableStateProxy writes content events
(chunks, permissions, terminals) because it observes the agent's responses.

**Pro:** Clean separation. Each proxy writes events for what it controls.
**Con:** DurableStateProxy doesn't see the original submission time.

### Option B: Client → DurableStateProxy → QueueProxy → Agent

DurableStateProxy sees the prompt on submission. QueueProxy holds it and releases it
later.

- **DurableStateProxy sees:** prompt at submission time
- **DurableStateProxy misses:** the queued→active transition (happens inside QueueProxy)
- **QueueProxy sees:** only prompts

**Pro:** DurableStateProxy sees everything from the client side.
**Con:** DurableStateProxy can't tell when the prompt is released from queue. Still
needs QueueProxy to write its own state events.

### Recommendation: Option A

In both options, QueueProxy needs to write its own lifecycle state events (it's the
only one that knows when prompts are queued, released, cancelled). Given that, putting
QueueProxy first is cleaner — DurableStateProxy only deals with agent-side events and
never needs to reason about queue state.

---

## Design Decision: session_to_prompt_turn Coupling

DurableStateProxy needs to know which `prompt_turn_id` a `SessionNotification` belongs
to (so it can attach chunks to the right turn). Currently this mapping lives in
`ConductorState.session_to_prompt_turn`.

With the split, QueueProxy creates the prompt_turn and knows the mapping. DurableStateProxy
needs it to record chunks.

### Options

**A. Read from StreamDb.** QueueProxy writes prompt_turn rows to the durable stream.
DurableStateProxy can query `stream_db.snapshot()` to find the active prompt_turn for
a session. The StreamServer's local StreamDb is always up-to-date (writes are applied
synchronously).

**B. Shared Arc<RwLock<HashMap>>.** Both proxies get a reference to the same map.
QueueProxy writes to it, DurableStateProxy reads from it.

**C. Message annotation.** QueueProxy adds the prompt_turn_id to the prompt's metadata
(`_meta` field) as it flows to the agent. DurableStateProxy reads it from there.
SessionNotifications include session_id, so DurableStateProxy can maintain its own map
after seeing the annotated prompt.

### Recommendation: Option A (read from StreamDb)

StreamDb is already the source of truth. No new shared state needed. DurableStateProxy
already uses `stream_db.snapshot()` for permission updates. Consistent pattern.

```rust
// In DurableStateProxy's SessionNotification handler:
fn find_active_turn(stream_db: &StreamDb, session_id: &str) -> Option<String> {
    let snapshot = stream_db.snapshot().await;
    snapshot.prompt_turns.values()
        .find(|t| t.session_id == session_id && t.state == PromptTurnState::Active)
        .map(|t| t.prompt_turn_id.clone())
}
```

---

## Boot Sequence (main.rs after)

Follows the cookbook's `running_proxies_with_conductor` pattern — infrastructure first,
then inline conductor composition. No `ConductorState::new()`, no `build_conductor()`.

```rust
// 1. Infrastructure (only thing created before the conductor)
let stream_server = StreamServer::start(bind, state_stream).await?;

// 2. Create proxy components (each is a ConnectTo<Conductor> per cookbook pattern)
//    Connection identity is generated inside QueueProxy, not in main.rs —
//    following the Sparkle pattern where components own their initialization.
let queue = Arc::new(PromptQueue::new());
let queue_proxy = QueueProxy::new(stream_server.clone(), queue.clone());
let state_proxy = DurableStateProxy::new(stream_server.clone());

// 3. API routes (queue handle for management, stream_server for reads)
let api = api::router(ApiState {
    stream_server: stream_server.clone(),
    queue: queue.clone(),
});
spawn_api_server(api_port, api).await?;

// 4. Conductor — inline composition per cookbook, no wrapper function
let agent = AcpAgent::from_args(cli.agent_command)?;
ConductorImpl::new_agent(
    "durable-acp",
    ProxiesAndAgent::new(agent)
        .proxy(queue_proxy)
        .proxy(state_proxy)
        .proxy(PeerMcpProxy),
    McpBridgeMode::default(),
)
.run(transport)
.await?;
```

### Connection identity

The current code generates `conn_id` in `ConductorState::init()` before any ACP
messages flow. But the cookbook's `per_session_mcp_server` pattern shows that
session-level initialization belongs inside the proxy's handler chain.

The connection row should be written when the `InitializeRequest` arrives (the conductor
is being initialized by a client), not at process startup. QueueProxy can handle this
in an `on_receive_request_from(Client, ...)` handler for `InitializeRequest`, or we
can use the conductor's `InstantiateProxiesAndAgent` trait to run initialization when
the `InitializeRequest` is received (see the cookbook's dynamic composition pattern
with closures).

Alternatively, if we want the connection row to exist before any client connects
(so the dashboard can show an "idle" conductor), it stays in `main.rs` — but as a
plain function call, not inside a `ConductorState::init()` method.

---

## What Changes

| File | Before | After |
|---|---|---|
| `conductor_state.rs` | God object (298 lines) | **Deleted** |
| `conductor.rs` | `build_conductor` wrapper (27 lines) | **Deleted** — inline in main.rs |
| `prompt_queue.rs` | Exists but unused | Holds `PromptQueue` (transient queue state) |
| `queue_proxy.rs` | **New** | `ConnectTo<Conductor>` — prompt flow control |
| `durable_state_proxy.rs` | Has queue logic + state logic | State observation only |
| `api.rs` | `Arc<ConductorState>` as state | `ApiState { stream_server, queue }` |
| `main.rs` | Creates ConductorState, calls build_conductor | Creates infrastructure, inline composition |

### New: `src/queue_proxy.rs`

Follows the cookbook's `reusable_components` pattern — struct with fields, `ConnectTo`
impl that builds the proxy. State captured in handler closures, not in a shared object.

```rust
/// Prompt flow control proxy — holds prompts, releases one at a time.
/// Follows the cookbook's `reusable_components` pattern.
pub struct QueueProxy {
    stream_server: StreamServer,
    queue: Arc<PromptQueue>,
}

impl ConnectTo<Conductor> for QueueProxy {
    async fn connect_to(self, client: impl ConnectTo<Proxy>) -> Result<(), sacp::Error> {
        let stream_server = self.stream_server;
        let queue = self.queue;
        // conn_id determined at session time, not at construction time
        let conn_id: Arc<RwLock<Option<String>>> = Default::default();

        sacp::Proxy.builder()
            .name("prompt-queue")
            // Capture connection identity from NewSessionRequest (per cookbook
            // per_session_mcp_server pattern — session-level init in handler)
            .on_receive_request_from(Client, {
                let conn_id = conn_id.clone();
                let stream_server = stream_server.clone();
                async move |req: NewSessionRequest, responder, cx| {
                    // Write connection row on first session (lazy init)
                    // ...
                    cx.send_request_to(Agent, req).forward_response_to(responder)?;
                    Ok(())
                }
            }, on_receive_request!())
            .on_receive_request_from(Client, {
                let queue = queue.clone();
                let stream_server = stream_server.clone();
                let conn_id = conn_id.clone();
                async move |req: PromptRequest, responder, cx| {
                    // 1. Create prompt_turn row (queued) → write to stream
                    // 2. Create pending_request row → write to stream
                    // 3. Enqueue in PromptQueue
                    // 4. drive_queue(cx) — if not paused and no active turn,
                    //    pop front, mark active, cx.send_request_to(Agent, prompt)
                    //    with on_receiving_result that writes completed/broken
                    Ok(())
                }
            }, on_receive_request!())
            .connect_to(client)
            .await
    }
}
```

`drive_queue` moves from `durable_state_proxy.rs` to `queue_proxy.rs`. The prompt
result handler (`on_receiving_result` that records stop_reason, marks completed/broken)
also moves here. This follows the Sparkle pattern where the component's `serve()`
method owns all the state and logic for its concern.

### Slimmed: `src/durable_state_proxy.rs`

Removes:
- `app.enqueue_prompt()` call
- `drive_queue()` function
- `QueuedPrompt` handling
- `session_to_prompt_turn` management (replaced by StreamDb lookup)

Keeps:
- SessionNotification → chunk recording
- RequestPermissionRequest → permission recording
- CreateTerminalRequest → terminal recording
- TerminalOutputRequest → chunk recording
- NewSessionRequest → connection state update

### Slimmed: `src/api.rs`

Queue management routes (`pause`, `resume`, `cancel`, `clear`, `reorder`) call
`Arc<PromptQueue>` directly instead of `ConductorState` methods.

State reads (filesystem, registry, terminal kill) use `StreamServer` directly.

---

## What Doesn't Change

- `StreamServer`, `StreamDb`, `StreamSubscriber`, `state.rs` — untouched
- `PeerMcpProxy` — untouched
- `prompt_queue.rs` — already extracted, just needs to be wired
- React/TS code — untouched
- Tests — updated to construct primitives instead of ConductorState

## Chunk Sequence Numbers

Currently `ConductorState.runtime.seq_by_prompt_turn` tracks per-turn sequence numbers
for chunks. With the split, chunk recording moves to DurableStateProxy but sequence
tracking is per prompt_turn.

Options:
- DurableStateProxy maintains its own `seq_by_prompt_turn` HashMap (simplest)
- Derive seq from StreamDb chunk count (query on each write — slower but stateless)

Recommendation: DurableStateProxy owns its own seq counter. It's a local implementation
detail, not shared state.

---

## Cookbook Patterns Applied

Summary of which SDK cookbook patterns inform this design:

| Cookbook Pattern | How We Use It |
|---|---|
| `reusable_components` | QueueProxy and DurableStateProxy are structs implementing `ConnectTo<Conductor>` |
| `running_proxies_with_conductor` | Conductor composed inline in main.rs with `ProxiesAndAgent`, no wrapper |
| `per_session_mcp_server` | Connection identity and session state initialized in handler closures, not at startup |
| Sparkle `acp_component.rs` | All proxy state lives in `connect_to` closure scope, not in a shared object |

### What we stop doing

| Anti-pattern | Replacement |
|---|---|
| `ConductorState` god object | Each proxy owns its own state in its `ConnectTo` impl |
| `build_conductor()` wrapper function | Inline `ConductorImpl::new_agent(...)` in main.rs |
| `ConductorState::init()` constructor | Connection row written in handler chain or at boot as a plain call |
| Shared `Arc<ConductorState>` across all layers | Each consumer gets only what it needs (queue gets `Arc<PromptQueue>`, proxy gets `StreamServer`) |
| `session_to_prompt_turn` shared map | StreamDb lookup (source of truth) |

## Open Questions

1. **Queue proxy writes to the durable stream.** This means QueueProxy needs
   `StreamServer`. Is this OK, or should only DurableStateProxy write to the stream?
   The argument for QueueProxy writing: it owns the prompt lifecycle, and the lifecycle
   state events are tightly coupled to queue operations (you can't record "cancelled"
   without also removing from the queue).

2. **API-driven queue operations need stream writes.** When the REST API cancels a
   queued turn, it needs to both remove from `PromptQueue` AND write a "cancelled"
   state event. Currently `ConductorState.cancel_queued_turn()` does both. After the
   split, should the API route call `queue.cancel()` + `write_state_event()` separately?
   Or should `PromptQueue` take a `StreamServer` reference and do both?

3. **Ordering with PeerMcpProxy.** Current chain is QueueProxy → DurableStateProxy →
   PeerMcpProxy → Agent. PeerMcpProxy injects MCP servers. Should it be before or after
   the state proxies? (Probably last, closest to agent, unchanged from today.)

4. **Connection identity.** `conn_id` is generated in main.rs and passed to both proxies.
   Should it come from the ACP `InitializeRequest` instead? The Sparkle component
   derives session identity from the protocol, not from startup.
