# Durable ACP — Project Index

> Single entry point. Read this first, then follow the SDDs in order.

## What This Is

A Rust multi-agent orchestrator built on [ACP](https://agentclientprotocol.com)
(Agent Client Protocol). Runs AI coding agents with durable state persistence,
agent-to-agent messaging, and a TUI dashboard. Uses the
[`sacp`](https://crates.io/crates/sacp) conductor framework and
[durable streams](https://github.com/durable-streams/durable-streams) for
state.

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
┌─────────────────────────────────────────────────────────────┐
│  Dashboard (thin TUI, ~200 lines)                           │
│  Spawns N conductor subprocesses, REST API + SSE client     │
└──────────┬──────────────────────────────────────────────────┘
           │ spawns per agent
┌──────────▼──────────────────────────────────────────────────┐
│  sacp-conductor subprocess (per agent)                      │
│  Config: { proxies: [...], agent: {...} }                   │
│                                                             │
│  ┌─ Proxy Chain ──────────────────────────────────────────┐ │
│  │  DurableStateProxy (standalone binary)                 │ │
│  │    intercepts → persists to durable stream             │ │
│  │  PeerMcpProxy (standalone binary)                      │ │
│  │    injects list_agents + prompt_agent MCP tools        │ │
│  │  Agent (npx/uvx/binary from ACP registry)              │ │
│  └────────────────────────────────────────────────────────┘ │
│                                                             │
│  Durable Streams Server (shared, one per dashboard)         │
│  REST API (shared)                                          │
│  StreamDB (materialized collections)                        │
└─────────────────────────────────────────────────────────────┘
           │ peering (HTTP)
┌──────────▼──────────────────────────────────────────────────┐
│  Other conductor subprocesses                               │
│  PeerMcpProxy → prompt_agent → HTTP POST/SSE to peer API   │
└─────────────────────────────────────────────────────────────┘
```

**Key principle:** Each proxy is a standalone binary. The conductor
config chains them. The dashboard is a thin client over REST API + SSE.
No custom in-process wiring.

## Execution Sequence

Work these SDDs in order. Each builds on the previous.

### Phase 1: SDK Alignment (~2-3 days)

Align with ACP ecosystem paved roads before building new features.

| # | Task | SDD | Effort |
|---|---|---|---|
| 1.1 | Migrate proxies to `sacp-proxy` v3.0.0 | [sdk-alignment.md](sdk-alignment.md) | ~1-2 days |
| 1.2 | Package proxies as standalone binaries | [sdk-alignment.md](sdk-alignment.md) | ~0.5 day |
| 1.3 | Move dashboard to subprocess-per-agent | [sdk-alignment.md](sdk-alignment.md) | ~1 day |
| 1.4 | Remove `AgentRouter`, `TuiState` mutex, `LocalSet` wiring | [sdk-alignment.md](sdk-alignment.md) | included |
| 1.5 | Fix API prompt routing (use conductor config) | [sdk-alignment.md](sdk-alignment.md) | included |

### Phase 2: Close Gaps (~4.5 days)

Fill remaining feature gaps needed for Flamecast integration.

| # | Task | SDD | Effort |
|---|---|---|---|
| 2.1 | File-backed durable streams storage | [known-limitations-sdd.md](known-limitations-sdd.md) §1 | ~0.5 day |
| 2.2 | WebSocket multiplexing (Flamecast protocol) | [event-subscribers-sdd.md](event-subscribers-sdd.md) | ~1.5 days |
| 2.3 | Webhooks (HTTP POST + HMAC) | [event-subscribers-sdd.md](event-subscribers-sdd.md) | ~0.5 day |
| 2.4 | File system access endpoints | [known-limitations-sdd.md](known-limitations-sdd.md) §4 | ~0.5 day |
| 2.5 | Terminal management API | [known-limitations-sdd.md](known-limitations-sdd.md) §5 | ~1 day |
| 2.6 | Session CRUD API (Flamecast-compatible) | [flamecast-integration-sdd.md](flamecast-integration-sdd.md) Phase 1 | ~0.5 day |

### Phase 3: Flamecast Integration (~3 days)

Connect to Flamecast React UI and cut its backend.

| # | Task | SDD | Effort |
|---|---|---|---|
| 3.1 | Permission resolution API | [flamecast-integration-sdd.md](flamecast-integration-sdd.md) Phase 2 | ~0.5 day |
| 3.2 | Agent templates API | [flamecast-integration-sdd.md](flamecast-integration-sdd.md) Phase 4 | ~0.5 day |
| 3.3 | Pluggable transports (TCP/WS) | [flamecast-integration-sdd.md](flamecast-integration-sdd.md) Transports | ~1 day |
| 3.4 | Runtime providers (Docker/E2B) | [known-limitations-sdd.md](known-limitations-sdd.md) §6 | ~1 day |

### Phase 4: Polish

| # | Task | Notes |
|---|---|---|
| 4.1 | Dashboard scrollable output | iocraft `ScrollView` keyboard |
| 4.2 | Permission UX in dashboard | Verify channel wiring e2e |
| 4.3 | Dead code cleanup | `Output` enum, `AgentHandle`, warnings |
| 4.4 | Editor integration testing | Zed, VS Code as ACP clients |

## SDDs (read in order)

| Doc | Status | What It Covers |
|---|---|---|
| [sdk-alignment.md](sdk-alignment.md) | 🔜 Phase 1 | `sacp-proxy` v3.0.0, standalone proxy binaries, subprocess-per-agent, dashboard simplification |
| [known-limitations-sdd.md](known-limitations-sdd.md) | 🔜 Phase 1-2 | File storage, API routing, terminal API, filesystem API, runtime providers |
| [event-subscribers-sdd.md](event-subscribers-sdd.md) | 🔜 Phase 2 | Unified WebSocket + webhook + SSE subscriber model |
| [flamecast-integration-sdd.md](flamecast-integration-sdd.md) | 🔜 Phase 2-3 | Flamecast API compatibility, pluggable transports, what to cut |
| [multi-agent-conductor-sdd.md](multi-agent-conductor-sdd.md) | ✅ Done | Single-process multi-agent (being replaced by subprocess model in Phase 1) |

## Key References

- [ACP Protocol](https://agentclientprotocol.com/protocol/overview)
- [Conductor Spec](https://agentclientprotocol.github.io/symposium-acp/conductor.html)
- [Proxy Chains RFD](https://agentclientprotocol.com/rfds/proxy-chains)
- [MCP-over-ACP RFD](https://agentclientprotocol.com/rfds/mcp-over-acp)
- [Rust SDK v1 RFD](https://agentclientprotocol.com/rfds/rust-sdk-v1)
- [ACP Cookbook](https://github.com/agentclientprotocol/rust-sdk/tree/main/src/agent-client-protocol-cookbook)
- [Agent Registry](https://agentclientprotocol.com/registry)
- [Durable Streams Protocol](https://github.com/durable-streams/durable-streams/blob/main/PROTOCOL.md)
- [Flamecast](https://github.com/anthropics/flamecast) (`~/smithery/flamecast`)

## Project Structure

```
src/
  main.rs              Conductor CLI (editor integration)
  conductor.rs         DurableStateProxy + conductor wiring (~400 lines → ~150 after Phase 1)
  peer_mcp.rs          PeerMcpProxy (list_agents, prompt_agent)
  agent_router.rs      In-process peer routing (delete in Phase 1)
  acp_registry.rs      ACP registry CDN client
  registry.rs          Local peer registry
  app.rs               AppState, queue, chunk recording
  state.rs             StreamDB, collections, STATE-PROTOCOL
  durable_streams.rs   Embedded durable streams server
  api.rs               REST API (axum)
  bin/
    dashboard.rs       TUI dashboard (~794 lines → ~200 after Phase 1)
    agents.rs          Config manager + registry picker (iocraft)
    chat.rs            Single-agent interactive chat
    run.rs             Headless multi-agent runner (migrate to v1 API)
    peer.rs            Agent-to-agent CLI (HTTP)
docs/
  index.md             This file
  sdk-alignment.md     Phase 1: SDK paved roads
  known-limitations-sdd.md    Gaps + fixes
  event-subscribers-sdd.md    WS/webhook/SSE
  flamecast-integration-sdd.md  Flamecast API + transports
  multi-agent-conductor-sdd.md  In-process model (superseded by Phase 1)
agents.toml            Agent configuration
```
