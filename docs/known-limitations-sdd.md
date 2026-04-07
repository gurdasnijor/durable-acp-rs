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
