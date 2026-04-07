# SDD: Known Limitations — Implementation Paths

> Four technical debts that should be resolved before production use.

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

## 2. No Authentication Between Agents

**Problem:** The peer registry (`~/.config/durable-acp/registry.json`) is
trusted by default. Any agent can register and any agent can prompt any
other agent. The HTTP fallback path in `prompt_agent` has no auth.

**Impact:** Fine for local development. Unacceptable for remote agents or
multi-tenant deployments.

**Fix — two layers:**

### Layer 1: Shared secret per agent (quick)

Each agent gets a `secret` in `agents.toml`. The `prompt_agent` MCP tool
and REST API require it as a `Bearer` token:

```toml
[[agent]]
name = "agent-a"
agent = "claude-acp"
secret = "random-256-bit-hex"
```

```rust
// REST API: verify Bearer token
fn verify_auth(req: &Request, expected: &str) -> bool {
    req.headers().get("Authorization")
        .and_then(|v| v.to_str().ok())
        .map(|v| v.strip_prefix("Bearer "))
        .flatten()
        == Some(expected)
}

// AgentRouter: attach secret to registration
router.register(name, tx, secret);
```

### Layer 2: mTLS for remote agents (proper)

When pluggable transports land (see `flamecast-integration-sdd.md`), TCP
connections should support mTLS. Each agent has a client certificate. The
conductor verifies the certificate before accepting the connection.

```toml
[[agent]]
name = "remote-agent"
transport = "tcp"
host = "gpu-server"
port = 9000
tls_cert = "certs/agent.pem"
tls_key = "certs/agent-key.pem"
tls_ca = "certs/ca.pem"
```

**Files to change:**
- `src/registry.rs` — add `secret` field to `AgentEntry`
- `src/peer_mcp.rs` — pass `Authorization` header in HTTP fallback
- `src/api.rs` — verify `Bearer` token on mutation endpoints
- `agents.toml` — add `secret` field

---

## 3. `submit_prompt` API Bypasses Proxy Inbound Path

**Problem:** `POST /api/v1/connections/{id}/prompt` calls
`cx.send_request_to(Agent, request)` which sends directly to the agent,
bypassing the `DurableStateProxy`'s `on_receive_request_from(Client, ...)`
handler. The API handler explicitly records the `PromptTurnRow` and
`session_to_prompt_turn` mapping, duplicating logic from the proxy.

**Impact:** Two code paths for the same operation. If the proxy's
enqueue/record logic changes, the API handler must be updated in sync.
The API path doesn't go through the queue — it sends directly to the agent.

**Root cause:** `send_request_to(Agent, ...)` sends downstream from the
proxy's position. The proxy only intercepts requests coming FROM Client,
but the API isn't the Client role.

**Fix options:**

### Option A: Inject request from Client side (ideal)

Find a way to inject the request into the conductor's handler chain as if
it came from the Client transport. This would route through the proxy
naturally. The challenge: `ConnectionTo<Conductor>` doesn't have a
"send as if from Client" API.

Possible approach: use the conductor's `with_spawned` callback to expose
a channel that accepts external requests and feeds them into the client
side of the transport:

```rust
// During conductor setup
let (external_prompt_tx, external_prompt_rx) = mpsc::unbounded_channel();

// In the client transport handler, merge external prompts
// with TUI prompts and send through the ACP connection
```

### Option B: Share the enqueue logic (pragmatic)

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

**Recommended:** Option B for now, Option A when the WebSocket layer lands.

**Files to change:**
- `src/app.rs` — extract shared enqueue logic
- `src/api.rs` — call shared function
- `src/conductor.rs` — call shared function from proxy handler

---

## 4. Single-Instance Drain Loop

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
