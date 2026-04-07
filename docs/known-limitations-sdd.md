# SDD: Known Limitations — Implementation Paths

> Three technical debts to resolve. Each has a clear fix.

## 1. In-Memory Storage

**Problem:** `EmbeddedDurableStreams` uses `InMemoryStorage`. All state
is lost on process exit.

**Fix:** Implement `Storage` trait with file-backed storage. The trait
is already pluggable:

```rust
struct FileStorage {
    base_dir: PathBuf,  // ~/.local/share/durable-acp/streams/
}
// Each stream = {base_dir}/{name}.jsonl (append-only)
// Each record = u32 length prefix + JSON bytes
// On startup: read + replay into StreamDB
```

**Effort:** ~0.5 day. No new deps (`std::fs`).

**Files:** `src/durable_streams.rs` (add `FileStorage`), `src/app.rs` (pick backend from config)

---

## 2. API Bypasses Proxy Chain

**Problem:** `POST /connections/{id}/prompt` calls `cx.send_request_to(Agent, ...)`
which sends directly to the agent, skipping the `DurableStateProxy` handler
and the conductor's central `ConductorMessage` queue.

```
Client (transport) → ConductorMessage queue → proxy chain → Agent  ✅
API → cx.send_to(Agent) → Agent directly                           ❌
```

**Fix:** Route API prompts through the `AgentRouter` channel, which feeds
into `session.send_prompt()`. This writes to the client side of the ACP
transport, entering the conductor from the Client direction and flowing
through the full proxy chain:

```rust
// API handler (fixed):
let router = agent_router::global_router();
router.prompt(&agent_name, &text).await;
// → prompt_tx channel → session.send_prompt()
// → writes to duplex client transport
// → conductor ConductorMessage queue
// → DurableStateProxy intercepts
// → agent receives prompt
```

This unifies all three prompt paths (TUI, peer MCP, REST API) into one
channel. Zero code duplication.

**Effort:** ~0.5 day. Remove `proxy_connection` from `AppState`.

**Files:** `src/api.rs` (use `AgentRouter`), `src/app.rs` (remove `proxy_connection`)

---

## 3. Single-Instance Drain Loop

**Problem:** The drain loop runs inside each conductor. If multiple
processes write to the same stream, both could drain the same prompt.

**Impact:** Not a problem today (single-process, each conductor owns its
queue). Only matters with multiple dashboard instances or remote conductors
sharing a stream.

**Fix:** Compare-and-swap on the durable stream. Atomically update
`queued → active` before draining. Requires conditional appends on the
stream server (upstream protocol extension).

**Status:** Deferred — single-instance enforced by convention. The
conductor's `ConductorMessage` queue already serializes within a process.

**Effort:** Depends on upstream. ~1 day once conditional appends land.

---

## 4. File System Access — Missing

**Problem:** Flamecast exposes `GET /agents/:id/files` (file preview) and
`GET /agents/:id/fs/snapshot` (filesystem tree). We have nothing — agents
can read/write files via their own tools, but the control plane can't
browse the workspace.

**Design:** The agent's workspace is the `cwd` passed to `NewSessionRequest`.
We already know it. Expose read-only filesystem endpoints:

```rust
// New endpoints in api.rs
GET /api/v1/agents/:id/files?path=src/main.rs    → file contents
GET /api/v1/agents/:id/fs/tree                     → directory listing
GET /api/v1/agents/:id/fs/tree?path=src            → subtree
```

Implementation is pure `std::fs` — no ACP involvement:

```rust
async fn get_file(
    Path((agent_id, file_path)): Path<(String, String)>,
    State(app): State<Arc<AppState>>,
) -> Result<String, StatusCode> {
    let cwd = get_agent_cwd(&app, &agent_id)?;
    let full_path = cwd.join(&file_path);
    // Validate path doesn't escape cwd
    if !full_path.starts_with(&cwd) {
        return Err(StatusCode::FORBIDDEN);
    }
    std::fs::read_to_string(full_path)
        .map_err(|_| StatusCode::NOT_FOUND)
}
```

**Note:** The `cwd` is available from the `NewSessionRequest` that was
sent during session creation. Store it in `ConnectionRow` or a new
`SessionMetadata` collection.

**Effort:** ~0.5 day.

**Files:** `src/api.rs` (add endpoints), `src/state.rs` (store cwd)

---

## 5. Terminal Management — Partial

**Problem:** The `DurableStateProxy` already intercepts
`CreateTerminalRequest`, `TerminalOutputRequest`, and records terminal
state in the `terminals` collection. But there's no API to:
- Create terminals from the control plane
- Send input to terminals
- Stream terminal output

**Design:** Terminals are agent-side resources. The control plane needs to
forward terminal operations through the ACP connection:

```rust
// New endpoints
POST   /api/v1/agents/:id/terminals           → create terminal
POST   /api/v1/agents/:id/terminals/:tid/input → send input
GET    /api/v1/agents/:id/terminals/:tid/output → SSE stream output
DELETE /api/v1/agents/:id/terminals/:tid       → kill terminal
```

For creation and input, route through the agent's ACP connection:

```rust
// Create terminal: send CreateTerminalRequest through session
cx.send_request_to(Agent, CreateTerminalRequest::new(command, session_id))

// Send input: send TerminalInputRequest through session
cx.send_request_to(Agent, TerminalInputRequest { terminal_id, data })
```

For output streaming, the `DurableStateProxy` already records
`TerminalOutputRequest` events. Subscribe to `CollectionChange::Terminals`
from the `StreamDB` and dispatch via SSE (or WebSocket channel once the
subscriber model lands).

**Effort:** ~1 day. Terminal creation/input is straightforward forwarding.
Output streaming reuses the existing SSE infrastructure.

**Files:** `src/api.rs` (add endpoints), `src/conductor.rs` (expose terminal forwarding)

---

## 6. Runtime Providers (Docker, E2B) — Missing

**Problem:** Flamecast has pluggable runtime providers (`local`, `docker`,
`e2b`) that spin up isolated environments for agents. We only support
local subprocess via `AcpAgent`.

**Design:** This is a natural extension of pluggable transports (see
`flamecast-integration-sdd.md`). A runtime provider:
1. Provisions the environment (start container, create sandbox)
2. Returns a `ByteStreams` transport (TCP to the container)
3. The conductor connects via that transport — everything else is the same

```rust
trait RuntimeProvider: Send + Sync {
    /// Provision an environment and return a transport to it.
    async fn start(&self, config: &AgentConfig) -> Result<Box<dyn Transport>>;
    /// Tear down the environment.
    async fn stop(&self, instance_id: &str) -> Result<()>;
}

// Implementations
struct LocalProvider;      // AcpAgent::from_args (today)
struct DockerProvider;     // docker run → TCP to container port
struct E2bProvider;        // E2B API → TCP to sandbox port
```

In `agents.toml`:

```toml
[[agent]]
name = "sandboxed-claude"
agent = "claude-acp"
runtime = "docker"           # or "e2b"

[agent.runtime_config]
image = "node:20"            # docker-specific
setup = ["npm install"]      # run before agent starts
```

The conductor inside the container runs `durable-acp-rs` as the entrypoint.
It listens on a TCP port. The dashboard connects via TCP transport. All
durable state flows back to the shared durable stream server.

**Dependencies:** Pluggable transports (TCP `ByteStreams`) must land first.

**Effort:** ~2-3 days per provider. Docker is simplest (just `docker run`
+ TCP). E2B requires their SDK integration.

**Files:** `src/runtime/mod.rs` (trait), `src/runtime/local.rs`,
`src/runtime/docker.rs`, `agents.toml` (add runtime config)

---

## 7. WebSocket Multiplexing + Webhooks — Missing

These are fully designed in `event-subscribers-sdd.md`. Summary:

**WebSocket:** Single multiplexed endpoint at `WS /ws` matching
[Flamecast's channel protocol](https://flamecast.mintlify.app/rfcs/multi-session-websocket).
Implemented as a `WsSubscriber` that dispatches events from
`StreamDb::subscribe_changes()` and accepts commands (prompt, permission
resolve, queue management, terminal I/O).

**Webhooks:** `WebhookSubscriber` that POSTs events to HTTP endpoints
with HMAC signing. Registered via `POST /api/v1/webhooks` or
`[[webhook]]` in `agents.toml`.

Both are the same `EventSubscriber` trait — subscribe to changes, filter,
dispatch via different transport.

**Effort:** ~3 days total (see `event-subscribers-sdd.md` for breakdown).

---

## Gap Summary

| Gap | Design | Effort | Depends On |
|---|---|---|---|
| File system access | Read-only fs endpoints + cwd tracking | ~0.5 day | — |
| Terminal management | Forward terminal ops via ACP, stream via SSE | ~1 day | — |
| WebSocket multiplexing | `WsSubscriber` + Flamecast channel protocol | ~1.5 days | — |
| Webhooks | `WebhookSubscriber` + HMAC | ~0.5 day | — |
| Runtime providers | `RuntimeProvider` trait + Docker/E2B impls | ~2-3 days/provider | Pluggable transports |
| File-backed storage | `FileStorage` impl for durable streams | ~0.5 day | — |
| API proxy bypass fix | Route through `AgentRouter` channel | ~0.5 day | — |
| Single-instance drain | CAS on durable stream | ~1 day | Upstream protocol ext |

**Total to close all gaps except runtime providers:** ~4.5 days

