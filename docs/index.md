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
| W4 | API prompt routing fix | 🔄 In progress | Routing `submit_prompt` through `session.connection()` — see [sdk-alignment.md](sdk-alignment.md) §1.5 |
| W5 | Conductor config support | 🔜 Ready | Depends on W2 (done) |
| W6 | File-backed storage | ✅ Done | [known-limitations-sdd.md](known-limitations-sdd.md) §1 |
| W7 | ~~EventSubscriber trait~~ | ⏭ ELIMINATED | StreamDB IS the subscriber — see [electric-sync-sdd.md](electric-sync-sdd.md) |
| W7a | ~~WebSocket subscriber~~ | ⏭ ELIMINATED | StreamDB subscribes via SSE directly |
| W7b | Webhook forwarder | 🔜 Ready | Tiny SSE→HTTP script (~50 lines) |
| W7c | ~~Generalized SSE~~ | ⏭ ELIMINATED | DS server SSE already works |
| W8 | ~~Flamecast TS client~~ | ⏭ ELIMINATED | Already exists (`@durable-acp/client`) — verify schema compat only |
| W9 | Pluggable transports (TCP/WS) | 🔜 Ready | [flamecast-integration-sdd.md](flamecast-integration-sdd.md) |
| W10 | Runtime providers (Docker/E2B) | 🔜 Ready | Depends on W9 |
| W11 | File system access API | ✅ Done | [known-limitations-sdd.md](known-limitations-sdd.md) §4 |
| W12 | Terminal management API | ✅ Done | [known-limitations-sdd.md](known-limitations-sdd.md) §5 |
| — | Queue CRUD (cancel, clear, reorder) | ✅ Done | REST endpoints for queue management |

### Remaining Work

- **W4** (in progress) — route API prompts through `session.connection()` instead of bypassing proxy chain
- **W5** — conductor config support (`sacp-conductor --config`)
- **W7b** — webhook forwarder (SSE→HTTP)
- **W9** — pluggable transports (TCP/WS for remote agents)
- **W10** — runtime providers (Docker/E2B)

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
| [sdk-alignment.md](sdk-alignment.md) | 🔄 W4 in progress | Standalone binaries, subprocess model, API fix |
| [known-limitations-sdd.md](known-limitations-sdd.md) | 🔄 Mostly done | Storage ✅, API routing 🔄, filesystem ✅, terminals ✅, runtime providers 🔜 |
| [electric-sync-sdd.md](electric-sync-sdd.md) | ✅ Design done | Native StreamDB integration — eliminates W7/W7a/W7c/W8-build |
| [event-subscribers-sdd.md](event-subscribers-sdd.md) | ⏭ Superseded | Replaced by StreamDB approach in `electric-sync-sdd.md` |
| [flamecast-integration-sdd.md](flamecast-integration-sdd.md) | 🔜 Track C | Flamecast API, pluggable transports, what to cut |

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
