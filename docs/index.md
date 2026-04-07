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

## Workstreams

12 workstreams extracted from all SDDs. Each is independently executable
once dependencies are met.

### Dependency Graph

```
                    ┌──────────────┐
                    │ W1: sacp-proxy│
                    │ migration    │
                    └──────┬───────┘
                           │
              ┌────────────┼────────────┐
              ▼            ▼            ▼
       ┌───────────┐ ┌───────────┐ ┌──────────┐
       │W2: proxy  │ │W3: dash   │ │W4: API   │
       │binaries   │ │subprocess │ │fix       │
       └─────┬─────┘ └─────┬─────┘ └──────────┘
             │              │
             ▼              ▼
       ┌───────────────────────┐
       │W5: conductor config   │
       └───────────┬───────────┘
                   │
      ┌────────────┼────────────────┐
      ▼            ▼                ▼
 ┌────────┐  ┌──────────┐   ┌────────────┐
 │W6: file│  │W7: event │   │W9: plug-   │
 │storage │  │subscribers│   │gable xport│
 └────────┘  └────┬─────┘   └─────┬──────┘
                  │               │
            ┌─────┼─────┐        ▼
            ▼     ▼     ▼  ┌──────────┐
         ┌────┐┌────┐┌────┐│W10:      │
         │W7a ││W7b ││W7c ││runtime   │
         │ WS ││Hook││SSE ││providers │
         └──┬─┘└────┘└────┘└──────────┘
            │
            ▼
     ┌────────────┐
     │W8: FC API  │
     │+ TS client │
     └────────────┘

Independent: W6 (storage), W11 (filesystem), W12 (terminals)
```

### Track A: SDK Alignment (critical path, ~3 days)

| W# | Task | Effort | Depends | SDD |
|---|---|---|---|---|
| W1 | Migrate to `sacp-proxy` v3.0.0 | 1-2d | — | [sdk-alignment.md](sdk-alignment.md) §1.1 |
| W2 | Standalone proxy binaries | 0.5d | W1 | [sdk-alignment.md](sdk-alignment.md) §1.2 |
| W3 | Dashboard → subprocess model | 1d | W2 | [sdk-alignment.md](sdk-alignment.md) §1.3 |
| W4 | API prompt routing fix | 0.5d | W1 | [known-limitations-sdd.md](known-limitations-sdd.md) §2 |
| W5 | Conductor config support | 0.5d | W2 | [sdk-alignment.md](sdk-alignment.md) §1.2 |

**Delivers:** Clean architecture. `conductor.rs` ~150 lines, `dashboard.rs` ~200 lines. Delete `agent_router.rs`. Proxies work with standard `sacp-conductor` ecosystem.

### Track B: Infrastructure (parallelizable, ~4 days)

| W# | Task | Effort | Depends | SDD |
|---|---|---|---|---|
| W6 | File-backed storage | 0.5d | — | [known-limitations-sdd.md](known-limitations-sdd.md) §1 |
| W7 | EventSubscriber trait + manager | 0.5d | — | [event-subscribers-sdd.md](event-subscribers-sdd.md) |
| W7a | WebSocket subscriber (Flamecast protocol) | 1.5d | W7 | [event-subscribers-sdd.md](event-subscribers-sdd.md) |
| W7b | Webhook subscriber (HMAC) | 0.5d | W7 | [event-subscribers-sdd.md](event-subscribers-sdd.md) |
| W7c | Generalized SSE endpoints | 0.5d | W7 | [event-subscribers-sdd.md](event-subscribers-sdd.md) |
| W11 | File system access API | 0.5d | — | [known-limitations-sdd.md](known-limitations-sdd.md) §4 |
| W12 | Terminal management API | 1d | — | [known-limitations-sdd.md](known-limitations-sdd.md) §5 |

**Delivers:** Persistent storage, real-time events via WS/webhook/SSE, filesystem and terminal access. All Flamecast feature gaps closed.

### Track C: Integration (depends on A + B, ~4-6 days)

| W# | Task | Effort | Depends | SDD |
|---|---|---|---|---|
| W8 | Flamecast API + TS client | 2-3d | W7a | [flamecast-integration-sdd.md](flamecast-integration-sdd.md) |
| W9 | Pluggable transports (TCP/WS) | 1d | W2 | [flamecast-integration-sdd.md](flamecast-integration-sdd.md) |
| W10 | Runtime providers (Docker/E2B) | 2-3d | W9 | [known-limitations-sdd.md](known-limitations-sdd.md) §6 |

**Delivers:** Flamecast React UI points at durable-acp-rs. `FlamecastStorage` → `DurableStreamStorage`. Remote agents via TCP/WS. Docker/E2B sandboxes.

### Recommended Sequence (single developer)

```
Week 1: W1 → W2 → W3 + W4     SDK alignment (clears tech debt)
Week 2: W6 + W7 → W7a + W7b   Storage + event subscribers
Week 3: W8 + W11 + W12         Flamecast API + missing features
Week 4: W9 → W10               Transports + runtime providers
```

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

| Doc | Phase | What It Covers |
|---|---|---|
| [sdk-alignment.md](sdk-alignment.md) | Track A | `sacp-proxy` v3.0.0, standalone binaries, subprocess model |
| [known-limitations-sdd.md](known-limitations-sdd.md) | Track A+B | Storage, API routing, filesystem, terminals, runtime providers |
| [event-subscribers-sdd.md](event-subscribers-sdd.md) | Track B | WebSocket + webhook + SSE unified subscriber model |
| [flamecast-integration-sdd.md](flamecast-integration-sdd.md) | Track C | Flamecast API, TS client, pluggable transports |
| [workstreams.md](workstreams.md) | Reference | Detailed per-workstream specs + dependency analysis |
| [multi-agent-conductor-sdd.md](multi-agent-conductor-sdd.md) | ✅ Done | In-process model (superseded by subprocess in Track A) |

## Key References

- [ACP Protocol](https://agentclientprotocol.com/protocol/overview) · [Conductor Spec](https://agentclientprotocol.github.io/symposium-acp/conductor.html) · [Proxy Chains RFD](https://agentclientprotocol.com/rfds/proxy-chains)
- [MCP-over-ACP RFD](https://agentclientprotocol.com/rfds/mcp-over-acp) · [Rust SDK v1 RFD](https://agentclientprotocol.com/rfds/rust-sdk-v1) · [ACP Cookbook](https://github.com/agentclientprotocol/rust-sdk/tree/main/src/agent-client-protocol-cookbook)
- [Agent Registry](https://agentclientprotocol.com/registry) · [Durable Streams Protocol](https://github.com/durable-streams/durable-streams/blob/main/PROTOCOL.md) · [Flamecast](~/smithery/flamecast)

## Project Structure

```
src/
  main.rs              Conductor CLI (editor integration)
  conductor.rs         DurableStateProxy + wiring → ~150 lines after W1
  peer_mcp.rs          PeerMcpProxy (list_agents, prompt_agent)
  agent_router.rs      In-process routing → delete after W3
  acp_registry.rs      ACP registry CDN client
  registry.rs          Local peer registry
  app.rs               AppState, queue, chunk recording
  state.rs             StreamDB, collections, STATE-PROTOCOL
  durable_streams.rs   Embedded durable streams server
  api.rs               REST API (axum)
  bin/
    dashboard.rs       TUI → ~200 lines after W3
    agents.rs          Config manager + registry picker
    chat.rs            Single-agent chat
    run.rs             Headless runner → migrate in W3
    peer.rs            Agent-to-agent CLI (HTTP)
agents.toml            Agent configuration
```
