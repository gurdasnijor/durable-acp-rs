# SDD: Known Limitations — Implementation Paths

> Three technical debts that should be resolved before production use.

## 1. In-Memory Storage — Durable Streams Reset on Restart

**Problem:** `EmbeddedDurableStreams` uses `InMemoryStorage` from
`durable-streams-server`. All state is lost on process exit. The "durable"
in durable streams is only as durable as the process lifetime.

**Impact:** No session recovery after crash/restart. No historical data
across runs. The `StreamDB.rebuild_state()` method exists but has nothing
to rebuild from.

**Fix:** The `durable-streams-server` crate's `Storage` trait is pluggable.
`InMemoryStorage` is one implementation. Add a file-backed or SQLite
implementation:

```rust
// The trait we need to implement (from durable-streams-server)
pub trait Storage: Send + Sync {
    fn create_stream(&self, name: &str, config: StreamConfig) -> Result<()>;
    fn append(&self, name: &str, data: Bytes, content_type: &str) -> Result<Offset>;
    fn read(&self, name: &str, offset: &Offset) -> Result<ReadResult>;
    fn delete_stream(&self, name: &str) -> Result<()>;
    fn list_streams(&self) -> Result<Vec<String>>;
    fn stream_metadata(&self, name: &str) -> Result<StreamMetadata>;
}
```

**Options:**

| Approach | Effort | Durability | Performance |
|---|---|---|---|
| **SQLite** (via `rusqlite`) | ~1 day | Full | Good (WAL mode) |
| **File-per-stream** (append-only file) | ~0.5 day | Full | Great (sequential writes) |
| **Write-ahead log** (binary format) | ~1-2 days | Full | Best |

**Recommended:** File-per-stream is simplest. Each stream is an append-only
file with length-prefixed JSON records. On startup, read and replay into
`StreamDB`. This matches how durable streams are designed to work.

```rust
struct FileStorage {
    base_dir: PathBuf,  // e.g. ~/.local/share/durable-acp/streams/
}

// Each stream = one file: {base_dir}/{stream_name}.jsonl
// Each record = u32 length prefix + JSON bytes
// Read = seek to offset, read forward
// Append = open in append mode, write length + data
```

**Files to change:**
- `src/durable_streams.rs` — add `FileStorage` implementing `Storage`
- `src/app.rs` — `AppState::new()` picks storage backend from config
- `Cargo.toml` — no new deps (just `std::fs`)

---

## 2. `submit_prompt` API Bypasses Proxy Inbound Path

**Problem:** `POST /api/v1/connections/{id}/prompt` calls
`cx.send_request_to(Agent, request)` which sends directly to the agent,
bypassing the `DurableStateProxy`'s `on_receive_request_from(Client, ...)`
handler. The API handler explicitly records the `PromptTurnRow` and
`session_to_prompt_turn` mapping, duplicating logic from the proxy.

**Impact:** Two code paths for the same operation. If the proxy's
enqueue/record logic changes, the API handler must be updated in sync.
The API path doesn't go through the queue — it sends directly to the agent.

**Root cause:** The connection API (`ConnectionTo<Conductor>`) only supports
sending **outgoing** messages to peers. It cannot re-inject a message into
the **incoming** handler chain:

```
                            ┌── DurableStateProxy ──────────────────────┐
                            │                                           │
Client (stdin) ──────► on_receive_from(Client, req)                     │
                            │    enqueue → drive_queue                  │
                            │         ↓                                 │
                            │    send_to(Agent) ───────────────► Agent  │
                            │                                           │
API ──────────────────── send_to(Agent) ────────────────────────► Agent │
                            │   ↑ BYPASSES the handler chain            │
                            └───────────────────────────────────────────┘
```

`send_request_to(Agent, ...)` sends downstream from the proxy's position.
The proxy only fires `on_receive_request_from(Client, ...)` for messages
arriving from the Client transport. There is no `inject_from(Client, req)`
API on `ConnectionTo<Conductor>`.

**Fix options:**

### Option A: Write to the client-side transport (ideal)

The dashboard creates `duplex` pipes for each conductor. The client side
of the transport is `client_out` / `client_in`. An external prompt could
be written directly to `client_out` as a raw ACP JSON-RPC message, which
would enter the conductor from the Client side and trigger the proxy
handler naturally:

```rust
// During setup, keep a handle to the client-side writer
let (client_out, conductor_in) = duplex(64 * 1024);
let client_writer = client_out.clone(); // for external injection

// To inject a prompt from the API:
let jsonrpc = format_jsonrpc_request("session/prompt", &prompt);
client_writer.write_all(jsonrpc.as_bytes()).await?;
```

This is architecturally clean but requires raw JSON-RPC formatting and
managing request IDs / response routing manually.

### Option B: Share the enqueue logic (pragmatic, recommended)

Extract the enqueue + state recording logic into a shared function that
both the proxy handler and the API handler call. This doesn't fix the
architectural issue but eliminates the code duplication:

```rust
// Shared function used by both proxy and API
pub async fn enqueue_and_record(
    app: &AppState,
    prompt: PromptRequest,
    responder: impl PromptResponder,
) -> Result<String> {
    // Create turn, enqueue, drive queue — one implementation
}
```

### Option C: Remove the API prompt endpoint (simplest)

The dashboard uses in-process channels, not the REST API, to send prompts.
The API endpoint is only needed for external clients. If external clients
use the WebSocket protocol instead (from `event-subscribers-sdd.md`),
the REST prompt endpoint becomes unnecessary.

### Option C: Route through the session channel (paved road)

The dashboard already solves this correctly. `run_agent` has a prompt
channel that feeds into `session.send_prompt()`, which writes to the
client side of the ACP connection. This goes through the full
Client → Conductor → DurableStateProxy → Agent path:

```rust
// Dashboard: prompt goes through proper ACP path
prompt_tx.send(text);  // TUI or peer sends via channel
// Inside run_agent:
session.send_prompt(&text)?;  // writes to ACP client connection
// → Conductor routes through proxy chain
// → DurableStateProxy intercepts, enqueues, persists
// → Agent receives the prompt
```

The REST API should route through the same channel instead of calling
`cx.send_request_to(Agent, ...)` directly. The `AgentRouter` already
has per-agent prompt channels. The API just needs to use them:

```rust
// In submit_prompt API handler:
let router = agent_router::global_router();
if let Some(result) = router.prompt(&agent_name, &text).await {
    // Routed through session.send_prompt() — full proxy chain
}
```

This is the cleanest fix — zero code duplication, proper proxy routing,
and it unifies the API, TUI, and peer prompt paths into one channel.

**Recommended:** Option C. The `AgentRouter` is already wired. Just make
the REST API use it instead of `cx.send_request_to`.

**Files to change:**
- `src/api.rs` — use `AgentRouter::prompt()` instead of `cx.send_request_to`
- Remove `proxy_connection` from `AppState` (no longer needed for API prompts)

---

## 3. Single-Instance Drain Loop

**Problem:** The drain loop (queued → active → send to agent → complete)
runs inside each conductor. If two processes write to the same durable
stream, both could try to drain the same queued prompt.

**Impact:** Not a problem today (single-process dashboard, each conductor
owns its queue). Becomes a problem when:
- Multiple dashboard instances share a durable stream
- Remote conductors write to a shared stream server
- The control plane API allows external prompt submission

**Fix:** Compare-and-swap (CAS) on the durable stream. Before draining a
queued turn, atomically update it from `queued` to `active`. If the CAS
fails, another instance already claimed it.

**Implementation path:**

The [Durable Streams Protocol](https://github.com/durable-streams/durable-streams/blob/main/PROTOCOL.md)
supports conditional appends via `If-Match` / `If-None-Match` headers on
`POST /streams/:name`. Use this to implement CAS:

```rust
// Attempt to claim a queued turn
let event = StateEvent::Update {
    entity_type: "prompt_turn",
    key: turn_id,
    value: PromptTurnRow { state: Active, ... },
    // Conditional: only if current state is Queued
    condition: Condition::FieldEquals("state", "queued"),
};

match durable_streams.append_conditional(&event).await {
    Ok(_) => { /* we claimed it, proceed */ }
    Err(ConflictError) => { /* another instance claimed it, skip */ }
}
```

This requires the durable streams server to support conditional writes,
which is a protocol extension. Until then, single-instance is enforced
by convention (only one dashboard process at a time).

**Files to change:**
- `durable-streams-server` — add conditional append support (upstream)
- `src/app.rs` — use conditional append in `take_next_prompt`
- `src/durable_streams.rs` — expose conditional append API
