# Durable ACP — Project Index

> Single entry point. Read this, then pick up any workstream whose
> dependencies are met.

## What This Is

A Rust multi-agent orchestrator built on [ACP](https://agentclientprotocol.com).
Runs AI coding agents with durable state persistence, agent-to-agent
messaging, and a TUI dashboard. Uses [`sacp`](https://crates.io/crates/sacp)
conductor framework + [durable streams](https://github.com/durable-streams/durable-streams).

**Repo:** https://github.com/gurdasnijor/durable-acp-rs

## Current State (working)

- Fullscreen TUI dashboard with N agents (`cargo run --bin dashboard`)
- Agent-to-agent MCP peering (`list_agents`, `prompt_agent`)
- Durable state persistence (all ACP messages → state stream)
- ACP registry integration (27+ agents from CDN)
- REST API + SSE streaming
- Interactive agent config picker (`cargo run --bin agents -- add`)

## Target Architecture

```
┌─────────────────────────────────────────────────────────────────────┐
│  Clients                                                            │
│  Dashboard TUI │ Flamecast React UI │ REST/WS/SSE clients          │
└───────┬────────────────┬─────────────────────┬─────────────────────┘
        │                │                     │
        │         ┌──────▼──────┐       ┌──────▼──────┐
        │         │ WS /ws      │       │ SSE /stream │
        │         │ (Flamecast  │       │ Webhooks    │
        │         │  protocol)  │       │             │
        │         └──────┬──────┘       └──────┬──────┘
        │                │                     │
┌───────▼────────────────▼─────────────────────▼─────────────────────┐
│  Control Plane (durable-acp-rs)                                     │
│                                                                     │
│  REST API ──── SubscriberManager ──── StreamDb                      │
│  Session CRUD     │  │  │          subscribe_changes()              │
│  Prompt/Cancel   WS SSE Webhook                                    │
│  Permissions                                                        │
│  Queue mgmt                                                        │
│  Agent templates                                                    │
│  File system                                                        │
│  Terminal I/O                                                       │
└───────┬─────────────────────────────────────────────────────────────┘
        │ spawns per agent
┌───────▼─────────────────────────────────────────────────────────────┐
│  sacp-conductor subprocess                                          │
│  ┌─ Proxy Chain ──────────────────────────────────────────────────┐ │
│  │  DurableStateProxy (standalone binary)                         │ │
│  │    intercepts all ACP → persists to durable stream             │ │
│  │  PeerMcpProxy (standalone binary)                              │ │
│  │    injects list_agents + prompt_agent MCP tools                │ │
│  │  Agent (npx/uvx/binary, local/TCP/WS transport)                │ │
│  └────────────────────────────────────────────────────────────────┘ │
│                                                                     │
│  Durable Streams Server (:4437) ──► FileStorage (persistent)        │
│  StreamDB (materialized collections in memory)                      │
└─────────────────────────────────────────────────────────────────────┘
        │ peering (HTTP between conductors)
        │ DurableACPClient (TypeScript, subscribes via WS/SSE)
```

**Key principles:**
- Each proxy is a standalone binary, chained via conductor config
- Dashboard is a thin client over REST + SSE (no in-process wiring)
- Durable stream is the single source of truth
- `DurableACPClient` (TypeScript) is the integration point for Flamecast

## Workstream Status

| W# | Task | Status | Notes |
|---|---|---|---|
| W1 | sacp-proxy migration | ⛔ BLOCKED | `sacp-proxy` 3.0.0 depends on `sacp` 2.0.0; we're on 11.0.0. Wait for upstream. |
| W2 | Standalone proxy binaries | ✅ Done | [sdk-alignment.md](sdk-alignment.md) §1.2 |
| W3 | Dashboard → subprocess model | ✅ Done | [sdk-alignment.md](sdk-alignment.md) §1.3 |
| W4 | API → read-only (remove prompt bypass) | 🔄 In progress | Remove `submit_prompt`/`cancel_turn` — prompts go through ACP only. See [api-architecture-sdd.md](api-architecture-sdd.md) |
| W5 | Conductor config support | 🔜 Ready | Depends on W2 (done) |
| W6 | File-backed storage | ✅ Done | [known-limitations-sdd.md](known-limitations-sdd.md) §1 |
| W7 | ~~EventSubscriber trait~~ | ⏭ ELIMINATED | StreamDB IS the subscriber — see [electric-sync-sdd.md](electric-sync-sdd.md) |
| W7a | ~~WebSocket subscriber~~ | ⏭ ELIMINATED | StreamDB subscribes via SSE directly |
| W7b | Webhook forwarder | 🔜 Ready | Tiny SSE→HTTP script (~50 lines) |
| W7c | ~~Generalized SSE~~ | ⏭ ELIMINATED | DS server SSE already works |
| W8 | Schema compatibility verified | ✅ Done | Rust ↔ TypeScript match. See [schema-compatibility.md](schema-compatibility.md) |
| W9 | Pluggable transports (TCP/WS) | 🔜 Ready | [flamecast-integration-sdd.md](flamecast-integration-sdd.md) |
| W10 | Runtime providers (Docker/E2B) | 🔜 Ready | Depends on W9 |
| W11 | File system access API | ✅ Done | [known-limitations-sdd.md](known-limitations-sdd.md) §4 |
| W12 | Terminal management API | ✅ Done | [known-limitations-sdd.md](known-limitations-sdd.md) §5 |
| — | Queue CRUD (cancel, clear, reorder) | ✅ Done | REST endpoints for queue management |

### Remaining Work

- **W4** (in progress) — remove REST prompt/cancel endpoints, make API read-only. See [api-architecture-sdd.md](api-architecture-sdd.md).
- **W5** — conductor config support (`sacp-conductor --config`)
- **W7b** — webhook forwarder (SSE→HTTP, ~50 lines)
- **W9** — pluggable transports (TCP/WS for remote agents)
- **W10** — runtime providers (Docker/E2B)

### Architecture Principle

All prompt submission goes through ACP (the paved road per P/ACP spec).
The REST API is **read-only state observation + queue management**.
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
| [api-architecture-sdd.md](api-architecture-sdd.md) | 🔄 In progress | REST API is read-only. Prompt submission through ACP only. |
| [sdk-alignment.md](sdk-alignment.md) | 🔄 W4 in progress | Standalone binaries ✅, subprocess model ✅, API fix 🔄 |
| [schema-compatibility.md](schema-compatibility.md) | ✅ Done | Rust ↔ TypeScript schema verified compatible |
| [known-limitations-sdd.md](known-limitations-sdd.md) | 🔄 Mostly done | Storage ✅, filesystem ✅, terminals ✅, runtime providers 🔜 |
| [electric-sync-sdd.md](electric-sync-sdd.md) | ✅ Design done | Native StreamDB — TS client already exists |
| [flamecast-integration-sdd.md](flamecast-integration-sdd.md) | 🔜 Track C | Flamecast API, transports, what to cut |
| [flamecast-capabilities.md](flamecast-capabilities.md) | ✅ Done | Maps each Flamecast guide to our infrastructure |
| [auth-sdd.md](auth-sdd.md) | 🔜 Ready | JWT auth proxy (Envoy/CF Access) |
| [deployment-sdd.md](deployment-sdd.md) | 🔜 Ready | Control plane / data plane split |
| [event-subscribers-sdd.md](event-subscribers-sdd.md) | ⏭ Superseded | Replaced by StreamDB in `electric-sync-sdd.md` |

## Key References

- [ACP Protocol](https://agentclientprotocol.com/protocol/overview) · [Conductor Spec](https://agentclientprotocol.github.io/symposium-acp/conductor.html) · [Proxy Chains RFD](https://agentclientprotocol.com/rfds/proxy-chains)
- [MCP-over-ACP RFD](https://agentclientprotocol.com/rfds/mcp-over-acp) · [Rust SDK v1 RFD](https://agentclientprotocol.com/rfds/rust-sdk-v1) · [ACP Cookbook](https://github.com/agentclientprotocol/rust-sdk/tree/main/src/agent-client-protocol-cookbook)
- [Agent Registry](https://agentclientprotocol.com/registry) · [Durable Streams Protocol](https://github.com/durable-streams/durable-streams/blob/main/PROTOCOL.md) · [Flamecast](~/smithery/flamecast)

## Project Structure

```
src/
  main.rs              Conductor CLI (editor integration)
  conductor.rs         DurableStateProxy + wiring (278 lines)
  peer_mcp.rs          PeerMcpProxy — list_agents, prompt_agent (172 lines)
  acp_registry.rs      ACP registry CDN client
  registry.rs          Local peer registry
  app.rs               AppState, queue, chunk recording (328 lines)
  state.rs             StreamDB, collections, STATE-PROTOCOL
  durable_streams.rs   Embedded durable streams server
  api.rs               REST API — axum (508 lines)
  bin/
    dashboard.rs       Fullscreen TUI dashboard (531 lines)
    agents.rs          Config manager + registry picker
agents.toml            Agent configuration
```
