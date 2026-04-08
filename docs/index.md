# Durable ACP — Project Index

> Single entry point. Read this, then pick up any workstream whose
> dependencies are met.

## What This Is

A Rust multi-agent orchestrator built on [ACP](https://agentclientprotocol.com).
Runs AI coding agents with durable state persistence, agent-to-agent
messaging, and a TUI dashboard. Uses the [ACP Rust SDK](https://github.com/agentclientprotocol/rust-sdk)
(git dependency) + [durable streams](https://github.com/durable-streams/durable-streams).

All ACP crates (`sacp`, `sacp-conductor`, `sacp-tokio`, `agent-client-protocol`)
come from one git repo at one commit — guaranteed compatible.

**Repo:** https://github.com/gurdasnijor/durable-acp-rs

## Current State (working)

- Fullscreen TUI dashboard with N agents (`cargo run --bin dashboard`)
- Agent-to-agent MCP peering (`list_agents`, `prompt_agent`)
- Durable state persistence (all ACP messages → state stream)
- ACP registry integration (27+ agents from CDN)
- WebSocket ACP endpoint (`/acp`) — remote clients connect as ACP clients
- REST API: queue management + filesystem access (state observation via durable stream)
- Webhook forwarder: RFC-aligned coalesced events, HMAC signing, retries
- Interactive agent config picker (`cargo run --bin agents -- add`)

## Architecture

```
┌─────────────────────────────────────────────────────────────────────┐
│  ACP Clients (all use ConnectTo abstraction)                        │
│  Dashboard TUI (stdio) │ Flamecast UI (WebSocket) │ Editor (stdio) │
└───────┬────────────────────────┬───────────────────────────────────┘
        │                        │
        │ stdio              WS /acp
        │                        │
┌───────▼────────────────────────▼───────────────────────────────────┐
│  durable-acp-rs                                                     │
│                                                                     │
│  ConductorImpl ─── DurableStateProxy ─── PeerMcpProxy ─── Agent    │
│    (proxy chain: intercepts all ACP → persists → injects MCP tools) │
│                                                                     │
│  REST API (:port+1)                                                 │
│    /acp (WebSocket) — ACP client transport, spawns conductor        │
│    /api/v1/*/queue/* — queue management                             │
│    /api/v1/*/files, /api/v1/*/fs/tree — filesystem access           │
│    /api/v1/registry — peer discovery                                │
│                                                                     │
│  Durable Streams Server (:port) ──► FileStorage (persistent)        │
│    /streams/durable-acp-state — SSE subscription for all state      │
│    StreamDB (materialized collections in memory)                    │
│                                                                     │
│  Webhook Forwarder (background task)                                │
│    Coalesced events → HTTP POST with HMAC signing + retries         │
└─────────────────────────────────────────────────────────────────────┘
        │ peering (HTTP between conductors)
        │ DurableACPClient (TypeScript, subscribes via SSE)
```

**Key principles:**
- All prompt submission goes through ACP — no REST bypass
- State observation via durable stream SSE (not REST endpoints)
- REST API is queue management + filesystem only
- Dashboard uses `Client.builder().connect_with()` (SDK paved road)
- ACP deps from git (`agentclientprotocol/rust-sdk`), not crates.io

## Workstream Status

| W# | Task | Status | Notes |
|---|---|---|---|
| W1 | sacp-proxy migration | ⛔ BLOCKED | `sacp-proxy` 3.0.0 depends on `sacp` 2.0.0; we're on 11.0.0. Wait for upstream. |
| W2 | Standalone proxy binaries | ✅ Done | `durable-state-proxy`, `peer-mcp-proxy` binaries |
| W3 | Dashboard → subprocess model | ✅ Done | Uses `Client.builder().connect_with()` (SDK) |
| W4 | API → read-only (remove prompt bypass) | ✅ Done | REST is read-only; `submit_prompt`/`cancel_turn` removed. See [api-architecture-sdd.md](api-architecture-sdd.md) |
| W5 | Conductor config support | 🔜 Ready | Depends on W2 (done) |
| W6 | File-backed storage | ✅ Done | [known-limitations-sdd.md](known-limitations-sdd.md) §1 |
| W7 | ~~EventSubscriber trait~~ | ⏭ ELIMINATED | StreamDB IS the subscriber — see [electric-sync-sdd.md](electric-sync-sdd.md) |
| W7a | ~~WebSocket subscriber~~ | ⏭ ELIMINATED | StreamDB subscribes via SSE directly |
| W7b | Webhook forwarder | ✅ Done | RFC-aligned: coalesced events, HMAC signing, retries. See `src/webhook.rs` |
| W7c | ~~Generalized SSE~~ | ⏭ ELIMINATED | DS server SSE already works |
| W8 | Schema compatibility verified | ✅ Done | Rust ↔ TypeScript match. See [schema-compatibility.md](schema-compatibility.md) |
| W9 | Pluggable transports | ✅ Done | Server: `/acp` WS (Channel::duplex). Client: WebSocketTransport (Lines), TcpTransport (ByteStreams). See [transport-sdd.md](transport-sdd.md) |
| W10 | Runtime providers (Docker/E2B) | 🔜 Ready | Depends on W9 |
| W11 | File system access API | ✅ Done | [known-limitations-sdd.md](known-limitations-sdd.md) §4 |
| W12 | Terminal management API | ✅ Done | [known-limitations-sdd.md](known-limitations-sdd.md) §5 |
| — | Queue CRUD (cancel, clear, reorder) | ✅ Done | REST endpoints for queue management |
| — | Proxy extraction | ✅ Done | conductor.rs 278→27 lines; proxy logic → durable_state_proxy.rs (287 lines) |

### Remaining Work

- **W5** — conductor config support (`sacp-conductor --config`)
- **W10** — runtime providers (Docker/E2B)

### Architecture Principle

All prompt submission goes through ACP (the paved road per P/ACP spec).
State observation via durable stream SSE — the REST API is **queue management + filesystem only**.
No bypass of the conductor's proxy chain. See [api-architecture-sdd.md](api-architecture-sdd.md).

## Flamecast Integration Shape

durable-acp-rs plugs into Flamecast as the conductor layer:

```
Flamecast SessionService.startSession()
  → spawns: sacp-conductor agent \
      "durable-state-proxy --stream-url ..." \
      "peer-mcp-proxy" \
      "npx claude-agent-acp"
  → AcpBridge connects via stdio (unchanged)
  → DurableStateProxy persists → Durable Stream
  → PeerMcpProxy injects peer tools

Flamecast reads from durable stream (replaces FlamecastStorage):
  DurableACPClient (TypeScript) subscribes via WS/SSE
  → reactive collections: connections, promptTurns, chunks, permissions
  → commands: prompt, cancel, pause, resume, resolvePermission
```

**What Flamecast cuts:**
- `FlamecastStorage` (PGLite/Postgres) → `DurableStreamStorage` adapter
- Event bus → `StreamDb::subscribe_changes()` via `DurableACPClient`
- Session metadata tables → `ConnectionRow` + `PromptTurnRow` in stream
- `@flamecast/psql` package → delete

**What Flamecast gains:**
- MCP peering across all sessions (automatic, free)
- Durable sessions (replay from stream offset)
- Stream-based observability (any HTTP client subscribes)

## SDDs

| Doc | Status | What It Covers |
|---|---|---|
| [api-architecture-sdd.md](api-architecture-sdd.md) | ✅ Done | REST API is queue mgmt + filesystem. Prompt/state through ACP/stream. |
| [schema-compatibility.md](schema-compatibility.md) | ✅ Done | Rust ↔ TypeScript schema verified compatible |
| [known-limitations-sdd.md](known-limitations-sdd.md) | 🔄 Mostly done | Storage ✅, filesystem ✅, terminals ✅, runtime providers 🔜 |
| [electric-sync-sdd.md](electric-sync-sdd.md) | ✅ Design done | Native StreamDB — TS client already exists |
| [flamecast-integration-sdd.md](flamecast-integration-sdd.md) | 🔜 Track C | Flamecast API, transports, what to cut |
| [flamecast-capabilities.md](flamecast-capabilities.md) | ✅ Done | Maps each Flamecast guide to our infrastructure |
| [auth-sdd.md](auth-sdd.md) | 🔜 Ready | JWT auth proxy (Envoy/CF Access) |
| [deployment-sdd.md](deployment-sdd.md) | 🔜 Ready | Control plane / data plane split |
| [transport-sdd.md](transport-sdd.md) | ✅ Done | Pluggable transports — `/acp` WS, WebSocketTransport, TcpTransport |
| [event-subscribers-sdd.md](event-subscribers-sdd.md) | ⏭ Superseded | Replaced by StreamDB in `electric-sync-sdd.md` |

## Key References

- [ACP Protocol](https://agentclientprotocol.com/protocol/overview) · [Conductor Spec](https://agentclientprotocol.github.io/symposium-acp/conductor.html) · [Proxy Chains RFD](https://agentclientprotocol.com/rfds/proxy-chains)
- [MCP-over-ACP RFD](https://agentclientprotocol.com/rfds/mcp-over-acp) · [Rust SDK v1 RFD](https://agentclientprotocol.com/rfds/rust-sdk-v1) · [ACP Cookbook](https://github.com/agentclientprotocol/rust-sdk/tree/main/src/agent-client-protocol-cookbook)
- [Agent Registry](https://agentclientprotocol.com/registry) · [Durable Streams Protocol](https://github.com/durable-streams/durable-streams/blob/main/PROTOCOL.md) · [Flamecast](~/smithery/flamecast)

## Project Structure

```
src/
  main.rs              Conductor CLI — stdio + /acp WebSocket (81 lines)
  conductor.rs         Pure composition — wires proxies + agent (27 lines)
  durable_state_proxy.rs  DurableStateProxy — intercepts ACP → persists to stream (287 lines)
  peer_mcp.rs          PeerMcpProxy — list_agents, prompt_agent (172 lines)
  acp_registry.rs      ACP registry CDN client
  registry.rs          Local peer registry
  app.rs               AppState, queue, chunk recording (318 lines)
  state.rs             StreamDB, collections, STATE-PROTOCOL (356 lines)
  durable_streams.rs   Embedded durable streams server
  api.rs               Queue mgmt + filesystem + /acp WebSocket (281 lines)
  webhook.rs           RFC-aligned webhook forwarder (336 lines)
  transport.rs         WebSocketTransport + TcpTransport + TransportConfig (82 lines)
  bin/
    dashboard.rs       TUI dashboard — Client.builder().connect_with() (522 lines)
    agents.rs          Config manager + registry picker
agents.toml            Agent configuration
```
