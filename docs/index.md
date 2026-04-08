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
| W1 | ~~sacp-proxy migration~~ | ⏭ ELIMINATED | Proxies built natively as `ConnectTo<Conductor>` components — no external crate needed |
| W2 | Standalone proxy binaries | ✅ Done | `durable-state-proxy`, `peer-mcp-proxy` binaries |
| W3 | Dashboard → subprocess model | ✅ Done | Uses `Client.builder().connect_with()` (SDK) |
| W4 | API → read-only (remove prompt bypass) | ✅ Done | REST is read-only; `submit_prompt`/`cancel_turn` removed. See [api-architecture-sdd.md](api-architecture-sdd.md) |
| W5 | ~~Conductor config support~~ | ⏭ DEFERRED | Not needed — proxy chain is application-specific, hardcoded in `build_conductor()` |
| W6 | File-backed storage | ✅ Done | [known-limitations-sdd.md](known-limitations-sdd.md) §1 |
| W7 | ~~EventSubscriber trait~~ | ⏭ ELIMINATED | StreamDB IS the subscriber — see [electric-sync-sdd.md](electric-sync-sdd.md) |
| W7a | ~~WebSocket subscriber~~ | ⏭ ELIMINATED | StreamDB subscribes via SSE directly |
| W7b | Webhook forwarder | ✅ Done | RFC-aligned: coalesced events, HMAC signing, retries. See `src/webhook.rs` |
| W7c | ~~Generalized SSE~~ | ⏭ ELIMINATED | DS server SSE already works |
| W8 | Schema compatibility verified | ✅ Done | Rust ↔ TypeScript match. See [schema-compatibility.md](schema-compatibility.md) |
| W9 | Pluggable transports | ✅ Done | Server: `/acp` WS (AxumWsTransport→Lines). Client: WebSocketTransport (Lines), TcpTransport (ByteStreams). All use `conductor.run(transport)`. See [transport-sdd.md](transport-sdd.md) |
| W10 | ~~Runtime providers~~ | ⏭ ELIMINATED | durable-acp-rs owns hosting. Flamecast is UI only. See [flamecast-runtime-swap-sdd.md](flamecast-runtime-swap-sdd.md) |
| W11 | File system access API | ✅ Done | [known-limitations-sdd.md](known-limitations-sdd.md) §4 |
| W12 | Terminal management API | ✅ Done | [known-limitations-sdd.md](known-limitations-sdd.md) §5 |
| — | Queue CRUD (cancel, clear, reorder) | ✅ Done | REST endpoints for queue management |
| — | Proxy extraction | ✅ Done | conductor.rs 278→27 lines; proxy logic → durable_state_proxy.rs (287 lines) |

### Remaining Work

All Rust workstreams complete. Next: Flamecast integration (Phase 1-3 in [flamecast-runtime-swap-sdd.md](flamecast-runtime-swap-sdd.md)).

### Architecture Principle

All prompt submission goes through ACP (the paved road per P/ACP spec).
State observation via durable stream SSE — the REST API is **queue management + filesystem only**.
No bypass of the conductor's proxy chain. See [api-architecture-sdd.md](api-architecture-sdd.md).

## Flamecast Integration

Flamecast is UI only. durable-acp-rs owns all hosting and lifecycle.

```typescript
import { startServer, connectWs, createDurableACPDB } from "@durable-acp/server";

const server = await startServer({ agent: "claude-acp" });
const acp = await connectWs(server.acpUrl);           // in-band: prompts
const db = createDurableACPDB({ stateStreamUrl: server.streamUrl }); // out-of-band: state
```

Two primitives, decoupled:
- **ACP client** (`@agentclientprotocol/sdk` via `@durable-acp/transport`) — prompt, cancel, permissions
- **Durable session** (`durable-session`) — reactive materialized state from SSE

See [flamecast-runtime-swap-sdd.md](flamecast-runtime-swap-sdd.md) for full integration plan + acceptance criteria.
See [flamecast-import.md](flamecast-import.md) for npm package structure.

## SDDs

| Doc | Status | What It Covers |
|---|---|---|
| [api-architecture-sdd.md](api-architecture-sdd.md) | ✅ Done | REST API is queue mgmt + filesystem. Prompt/state through ACP/stream. |
| [schema-compatibility.md](schema-compatibility.md) | ✅ Done | Rust ↔ TypeScript schema verified compatible |
| [known-limitations-sdd.md](known-limitations-sdd.md) | 🔄 Mostly done | Storage ✅, filesystem ✅, terminals ✅, runtime providers 🔜 |
| [electric-sync-sdd.md](electric-sync-sdd.md) | ✅ Design done | Native StreamDB — TS client already exists |
| [flamecast-runtime-swap-sdd.md](flamecast-runtime-swap-sdd.md) | ✅ Ready | Flamecast → UI only. Two primitives, 3 phases. |
| [flamecast-import.md](flamecast-import.md) | ✅ Ready | `@durable-acp/server` npm package + platform binaries |
| [flamecast-integration-sdd.md](flamecast-integration-sdd.md) | ⏭ Superseded | Replaced by runtime-swap-sdd |
| [flamecast-capabilities.md](flamecast-capabilities.md) | ✅ Done | Maps each Flamecast guide to our infrastructure |
| [architecture-redesign.md](architecture-redesign.md) | ✅ Done (Option A) | Module renames for clarity |
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
  main.rs                Conductor CLI — stdio + /acp WebSocket (81 lines)
  conductor.rs           Proxy chain composition — build_conductor() (27 lines)
  conductor_state.rs     Queue runtime + state write helpers (318 lines)
  durable_state_proxy.rs DurableStateProxy — intercepts ACP → persists to stream (287 lines)
  peer_mcp.rs            PeerMcpProxy — list_agents, prompt_agent (172 lines)
  client.rs              Reusable ACP client — handler trait + prompt channel (126 lines)
  stream_server.rs       Embedded durable streams HTTP server (153 lines)
  stream_subscriber.rs   SSE subscriber to remote durable stream (321 lines)
  state.rs               StreamDB, collections, STATE-PROTOCOL (356 lines)
  api.rs                 Queue mgmt + filesystem + /acp WebSocket (250 lines)
  transport.rs           WebSocket/TCP/AxumWs transport impls (113 lines)
  webhook.rs             RFC-aligned webhook forwarder (336 lines)
  registry.rs            Local peer registry
  acp_registry.rs        ACP registry CDN client
  bin/
    dashboard.rs         TUI dashboard — Client.builder().connect_with() (407 lines)
    ds_server.rs         Bare durable streams server for testing (18 lines)
    agents.rs            Config manager + registry picker

ts/
  packages/
    server/              @durable-acp/server — startServer(), platform binary resolution
    acp-transport/       @durable-acp/transport — WebSocket adapter + connectWs()
    durable-session/     durable-session — reactive materialized state from SSE

agents.toml              Agent configuration
```
