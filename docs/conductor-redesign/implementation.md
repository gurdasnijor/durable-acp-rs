# SDD: Implementation Detail — Passive State Observation

> Status: Proposed
> Parent: [`architecture.md`](architecture.md) (authoritative architecture)
> Companion: [`testing-and-hosting.md`](testing-and-hosting.md) (test matrix and hosting surface)

This document covers the **implementation specifics** of replacing active state
interception with passive SDK tracing. For architectural rationale, decisions,
and migration plan, see [`architecture.md`](architecture.md).

---

## DurableStreamTracer — WriteEvent Implementation

The SDK's `TraceWriter` accepts any `WriteEvent` impl. Ours appends to the
durable stream via a channel + async writer task:

```rust
struct DurableStreamTracer {
    tx: mpsc::UnboundedSender<TraceEvent>,
}

impl WriteEvent for DurableStreamTracer {
    fn write_event(&mut self, event: &TraceEvent) -> std::io::Result<()> {
        self.tx.unbounded_send(event.clone())
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::BrokenPipe, e))
    }
}

impl DurableStreamTracer {
    fn start(stream_server: StreamServer, stream_name: String) -> Self {
        let (tx, mut rx) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            while let Some(event) = rx.recv().await {
                let _ = stream_server.append_json(&stream_name, &event).await;
            }
        });
        Self { tx }
    }
}
```

~30 lines. Replaces `durable_state_proxy.rs` (288 lines).

---

## Stream Format: StateEnvelope → TraceEvent

### Before (custom state events)

```json
{
  "type": "prompt_turn",
  "key": "turn-123",
  "headers": { "operation": "insert" },
  "value": { "promptTurnId": "turn-123", "state": "active", ... }
}
```

### After (SDK-native TraceEvent)

```json
{
  "type": "request",
  "ts": 1.234,
  "protocol": "acp",
  "from": "Client",
  "to": "Proxy(0)",
  "id": "req-1",
  "method": "session/prompt",
  "params": { "sessionId": "sess-1", "prompt": [...] }
}
```

```json
{
  "type": "notification",
  "ts": 1.567,
  "protocol": "acp",
  "from": "Agent",
  "to": "Proxy(0)",
  "method": "session/update",
  "session": "sess-1",
  "params": { "update": { "type": "agentMessageChunk", ... } }
}
```

```json
{
  "type": "response",
  "ts": 2.345,
  "from": "Agent",
  "to": "Client",
  "id": "req-1",
  "is_error": false,
  "payload": { "stopReason": "endTurn" }
}
```

### StreamDb materialization table

`StreamDb.apply_json_message()` changes from parsing `StateEnvelope` to parsing
`TraceEvent`:

| TraceEvent | Materialized as |
|---|---|
| Request `session/new` | Connection row (state: attached, cwd, session_id) |
| Request `session/prompt` | PromptTurn row (state: active) |
| Response to `session/prompt` | PromptTurn update (state: completed/broken) |
| Notification `session/update` | Chunk row (text, thinking, tool_call, tool_result) |
| Request `session/requestPermission` | Permission row |
| Response to `session/requestPermission` | Permission update (resolved) |
| Request `session/createTerminal` | Terminal row |

---

## Prompt-Turn Materialization: The Hard Part

Prompt turns are an **application concept**, not an SDK concept. The SDK handles
session bootstrap and message routing but does not track "prompt turns."

With `TraceEvent`, prompt turns are **derived on the read side**. `StreamDb` sees
a `session/prompt` request, generates a prompt_turn_id, and correlates subsequent
notifications and the response by session_id and request id.

**This is the hardest part of the redesign** and the main place hidden complexity
could grow. It is an application-level projection problem, not a proxy-chains
concern. Two implementation requirements:

### 1. Request/response correlation

The read side must persist request metadata (at minimum: JSON-RPC request ID →
generated prompt_turn_id) to correlate responses back to the prompt turn that
originated them. TraceEvent includes `id` fields for this, but StreamDb needs
an internal index keyed by request ID.

### 2. Prompt serialization assumption

The current codebase enforces single-prompt-per-session via the server-side
queue. With that queue removed, the read side must define the contract:

- **Assume serialized prompts per session** (recommended for now). Document as
  a constraint. The client is responsible for not sending overlapping prompts.
  This matches current behavior and keeps StreamDb simple.
- **Handle concurrent prompts** (future, if needed). Would require request-ID
  correlation in `session/update` notifications — needs ACP spec verification.

---

## ConductorState Dissolution: Where Each Piece Goes

| Responsibility | Current location | After |
|---|---|---|
| `StreamServer` instance | `ConductorState.stream_server` | Owned by `main.rs`, passed to tracer and API state |
| `logical_connection_id` | `ConductorState.logical_connection_id` | Startup-generated in `main.rs`, passed to `ApiState` and registry |
| `stream_db` access | Via `stream_server.stream_db` | API routes access `StreamServer.stream_db` directly |
| `write_state_event()` | `ConductorState` method | **Deleted** — tracer writes raw TraceEvents |
| Queue state | `ConductorState.runtime` | **Deleted** — per [`queue-scope.md`](queue-scope.md) |
| `session_to_prompt_turn` | `ConductorState` field | **Deleted** — read side derives from trace events |

### API state replacement

```rust
pub struct ApiState {
    pub stream_server: StreamServer,   // for snapshot reads + stream writes
    pub connection_id: String,         // for filesystem path resolution
}
```

### Non-queue REST routes that survive

- **`GET /files`**, **`GET /fs/tree`** — resolve paths using `connection_id` +
  `stream_db.snapshot()` for `cwd` lookup. Unchanged logic, different state holder.
- **`GET /registry`** — reads file-based registry. No ConductorState dependency.
- **`DELETE /terminals/{tid}`** — writes terminal state update to the stream.
  This is a mutation the passive tracer does not cover (it only observes ACP
  wire traffic). Must write directly to `StreamServer` or be reconsidered as
  part of the stream format migration.
- **`GET /agent-templates`** — reads `agents.toml`. No dependency.

---

## Boot Sequence (main.rs)

```rust
// 1. Infrastructure
let stream_server = StreamServer::start(bind, state_stream).await?;
let connection_id = Uuid::new_v4().to_string();

// 2. Register in peer registry (needs identity at startup)
registry::register(registry::AgentEntry {
    name: cli.name.clone(),
    api_url: format!("http://127.0.0.1:{api_port}"),
    logical_connection_id: connection_id.clone(),
    registered_at: now_ms(),
})?;

// 3. API routes
let api = api::router(ApiState {
    stream_server: stream_server.clone(),
    connection_id: connection_id.clone(),
});
spawn_api_server(api_port, api).await?;

// 4. Passive state observation
let tracer = DurableStreamTracer::start(
    stream_server.clone(),
    state_stream.clone(),
);

// 5. Conductor — inline composition
let agent = AcpAgent::from_args(cli.agent_command)?;
ConductorImpl::new_agent(
    "durable-acp",
    ProxiesAndAgent::new(agent)
        .proxy(PeerMcpProxy),
    McpBridgeMode::default(),
)
.trace_to(tracer)
.run(transport)
.await?;
```

---

## File Changes

| File | Before | After |
|---|---|---|
| `conductor_state.rs` | God object (298 lines) | **Deleted** |
| `conductor.rs` | `build_conductor` wrapper (27 lines) | **Deleted** — inline in main.rs |
| `durable_state_proxy.rs` | Active interceptor (288 lines) | **Deleted** — replaced by `DurableStreamTracer` |
| `durable_stream_tracer.rs` | **New** | `WriteEvent` impl (~30 lines) |
| `state.rs` (StreamDb) | Parses `StateEnvelope` | Parses `TraceEvent` — smarter read side |
| `api.rs` | `Arc<ConductorState>` | `ApiState { stream_server, connection_id }` — queue routes deleted |
| `main.rs` | ConductorState + build_conductor | Infrastructure + inline composition + `.trace_to()` |

## SDK Primitives Used

| SDK Primitive | How We Use It |
|---|---|
| `SnooperComponent` | Wraps all components for passive observation (via `.trace_to()`) |
| `TraceWriter` / `WriteEvent` | `DurableStreamTracer` implements `WriteEvent` |
| `ConductorImpl.trace_to()` | Wires snoop → tracer automatically |
| `TraceEvent` | Stream format — Request/Response/Notification |
| `ProxiesAndAgent::new(agent).proxy(...)` | Inline conductor composition |

---

## Acceptance Criteria

This redesign is gated on the MVP test set defined in
[`testing-and-hosting.md`](testing-and-hosting.md). Summary:

1. `in_process_proxy_chain_round_trip`
2. `proxy_vs_agent_initialization_roles`
3. `trace_replay_matches_live_state`
4. `single_session_multiple_prompts_materialize_correctly`
5. `ws_transport_behaves_like_stdio_transport`
6. `flamecast_contract_acp_plus_state_stream`

See that document for full test descriptions and recommended additional tests.
