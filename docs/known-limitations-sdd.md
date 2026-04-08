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
  DurableACPClient subscribes, materializes into TanStack DB collections

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

## 2. API Bypasses Proxy Chain — ✅ RESOLVED

Resolved by removing REST prompt/cancel endpoints entirely. All prompt
submission goes through ACP (stdio or `/acp` WebSocket). The REST API
is now queue management + filesystem only. See [api-architecture-sdd.md](api-architecture-sdd.md).

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

## 7. WebSocket + Webhooks — ✅ RESOLVED

**WebSocket:** The `/acp` endpoint provides full ACP client transport
over WebSocket using `sacp::Channel::duplex()`. No custom multiplexing
protocol needed — clients connect as standard ACP clients.

**Webhooks:** RFC-aligned forwarder in `src/webhook.rs`. Coalesced
events (`end_turn`, `error`, `permission_request`), HMAC-SHA256 signing,
5 retries with exponential backoff. Configured via `[[webhook]]` in
`agents.toml`.

---

## Gap Summary

| Gap | Status | Notes |
|---|---|---|
| File-backed storage | ✅ FIXED | `FileStorage` impl |
| File system access | ✅ FIXED | Read-only fs endpoints |
| Terminal management | ✅ FIXED | State in stream, kill via REST |
| Queue CRUD | ✅ FIXED | Cancel, clear, reorder endpoints |
| API proxy bypass | ✅ RESOLVED | REST prompt endpoints removed; all prompts go through ACP |
| WebSocket transport | ✅ FIXED | `/acp` endpoint with `Channel::duplex()` |
| Webhooks | ✅ FIXED | RFC-aligned forwarder with HMAC + retries |
| Runtime providers | 🔜 Ready | Depends on client-side transport resolution (W9) |
| Single-instance drain | Deferred | Needs upstream conditional appends |

