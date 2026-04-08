# SDD: Known Limitations — Implementation Paths

> Gaps to close for production use and Flamecast integration.

## Integration Shape

durable-acp-rs plugs into Flamecast transparently as the conductor layer:

```
Flamecast SessionService.startSession()
  → runtime provider spawns:
      sacp-conductor agent \
        "durable-state-proxy --stream-url ..." \
        "peer-mcp-proxy" \
        "npx claude-agent-acp"
  → AcpBridge connects via stdio (unchanged — sees standard ACP agent)
  → DurableStateProxy persists all ACP traffic to durable stream
  → PeerMcpProxy injects agent-to-agent tools

Flamecast reads state from durable stream (replaces FlamecastStorage):
  GET /streams/durable-acp-state          → full state replay
  GET /streams/durable-acp-state?live=sse → real-time subscription
  REST API /api/v1/connections            → session list
  REST API /api/v1/prompt-turns/{id}/*    → chunks, SSE streaming

What Flamecast cuts:
  FlamecastStorage (PGLite/Postgres) → durable stream + StreamDB
  Event bus                          → StreamDb::subscribe_changes()
  Session metadata tables            → ConnectionRow + PromptTurnRow in stream
  Permission brokering logic         → DurableStateProxy intercepts
  (gains) Agent-to-agent messaging   → PeerMcpProxy (free, automatic)
```

The gaps below are what's needed to make this integration complete.

## 1. In-Memory Storage — ✅ FIXED

`FileStorage` implements the `Storage` trait with file-backed persistence.
State survives process restart.

---

## 2. API Bypasses Proxy Chain — 🔄 IN PROGRESS

**Problem:** `POST /connections/{id}/prompt` bypasses the proxy chain,
sending directly to the agent instead of through `DurableStateProxy`.

**Fix:** Use `session.connection()` to route prompts through the client
side of the ACP connection, so the proxy chain fires automatically.

**Challenge:** `ClientSideConnection` is `!Send`. The REST API runs on a
multi-threaded tokio runtime. Solution: channel bridge from API → dashboard
LocalSet thread → `session.connection()`.

See [sdk-alignment.md](sdk-alignment.md) §1.5 for implementation details.

**Files:** `src/api.rs`, `src/app.rs`, `src/conductor.rs`

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

## 4. File System Access — ✅ FIXED

File system endpoints implemented: `GET /agents/:id/files`, `GET /agents/:id/fs/tree`.

---

## 5. Terminal Management — ✅ FIXED

Terminal CRUD endpoints implemented: create, input, output (SSE), kill.

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

| Gap | Status | Notes |
|---|---|---|
| File-backed storage | ✅ FIXED | `FileStorage` impl |
| File system access | ✅ FIXED | Read-only fs endpoints |
| Terminal management | ✅ FIXED | Full CRUD + SSE output |
| Queue CRUD | ✅ FIXED | Cancel, clear, reorder endpoints |
| API proxy bypass | 🔄 IN PROGRESS | `session.connection()` approach — see [sdk-alignment.md](sdk-alignment.md) §1.5 |
| WebSocket multiplexing | ⏭ ELIMINATED | StreamDB subscribes via SSE — see [electric-sync-sdd.md](electric-sync-sdd.md) |
| Webhooks | 🔜 Ready | Tiny SSE→HTTP forwarder |
| Runtime providers | 🔜 Ready | Depends on pluggable transports (W9) |
| Single-instance drain | Deferred | Needs upstream conditional appends |

