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
                    │ BLOCKED      │ sacp-proxy 3.0.0 depends on sacp 2.0.0
                    │ (we're 11.0) │ Wait for upstream update
                    └──────────────┘

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

### Track A: SDK Alignment (critical path, ~2 days)

| W# | Task | Effort | Depends | SDD |
|---|---|---|---|---|
| W1 | ~~Migrate to `sacp-proxy` v3.0.0~~ | BLOCKED | — | `sacp-proxy` 3.0.0 depends on `sacp` 2.0.0; we're on 11.0.0. Wait for upstream. |
| W2 | Standalone proxy binaries | 0.5d | — | [sdk-alignment.md](sdk-alignment.md) §1.2 |
| W3 | Dashboard → subprocess model | 1d | W2 | [sdk-alignment.md](sdk-alignment.md) §1.3 |
| W4 | API prompt routing fix | 0.5d | — | [known-limitations-sdd.md](known-limitations-sdd.md) §2 |
| W5 | Conductor config support | 0.5d | W2 | [sdk-alignment.md](sdk-alignment.md) §1.2 |

**What changes (W2-W4, using current sacp 11.0.0 APIs):**

| File | Current | After | How |
|---|---|---|---|
| `conductor.rs` | 400 lines | 400 lines | Stays as-is (W1 blocked). Proxy binaries wrap it. |
| `peer_mcp.rs` | 230 lines | 230 lines | Stays as-is (W1 blocked). Proxy binary wraps it. |
| `dashboard.rs` | 794 lines | 200 lines | Subprocess model — thin TUI over REST+SSE |
| `api.rs` | 315 lines | 150 lines | Delete bypass hack, route through conductor |
| `app.rs` | 284 lines | 240 lines | Delete `proxy_connection` |
| `agent_router.rs` | 80 lines | deleted | HTTP peering replaces in-process channels |
| **Total core** | **~2100 lines** | **~1220 lines** | **-42%** (W1 adds another -23% when unblocked) |

**Key migrations (W2-W4):**
- In-process `ConductorImpl` → conductor subprocess per agent
- In-process `AgentRouter` channels → HTTP peering (already works)
- `proxy_connection` bypass hack → prompts route through conductor subprocess
- `ClientSideConnection` (v0) in `run.rs` → subprocess model

### Track B: Infrastructure (parallelizable, ~2.5 days)

| W# | Task | Effort | Depends | SDD |
|---|---|---|---|---|
| W6 | File-backed storage (Rust-side) | 0.5d | — | [known-limitations-sdd.md](known-limitations-sdd.md) §1 |
| W7 | ~~EventSubscriber trait~~ | ELIMINATED | — | Not needed — `@durable-acp/state` StreamDB IS the subscriber |
| W7a | ~~WebSocket subscriber~~ | ELIMINATED | — | StreamDB subscribes via SSE directly |
| W7b | Webhook forwarder | 0.5d | — | Tiny SSE→HTTP script (~50 lines) |
| W7c | ~~Generalized SSE~~ | ELIMINATED | — | DS server SSE already works |
| W11 | File system access API | 0.5d | — | [known-limitations-sdd.md](known-limitations-sdd.md) §4 |
| W12 | Terminal management API | 1d | — | [known-limitations-sdd.md](known-limitations-sdd.md) §5 |

**Key discovery:** The `@durable-acp/state` and `@durable-acp/client`
packages (in `~/gurdasnijor/distributed-acp/`) already provide the
TypeScript integration layer. They subscribe to the DS stream via SSE,
materialize into TanStack DB collections, and provide reactive queries.
See [electric-sync-sdd.md](electric-sync-sdd.md) for details.

### Track C: Integration (depends on A + B, ~3-4 days)

| W# | Task | Effort | Depends | SDD |
|---|---|---|---|---|
| W8 | ~~Flamecast TS client~~ → verify schema compat | 0.5d | — | Client already exists (`@durable-acp/client`). Just verify Rust schema matches TS Zod schemas. |
| W9 | Pluggable transports (TCP/WS) | 1d | W2 | [flamecast-integration-sdd.md](flamecast-integration-sdd.md) |
| W10 | Runtime providers (Docker/E2B) | 2-3d | W9 | [known-limitations-sdd.md](known-limitations-sdd.md) §6 |

**Key discovery:** The `@durable-acp/client` TypeScript package already
exists. `FlamecastStorage` becomes a thin adapter wrapping `DurableACPClient`.
See [electric-sync-sdd.md](electric-sync-sdd.md).

### Recommended Sequence (single developer)

```
Week 1: W2 → W3 + W4           Standalone binaries + subprocess dashboard + API fix
Week 2: W6 + W8 + W11 + W12    File storage + schema verify + filesystem + terminals
Week 3: W9 → W10               Transports + runtime providers
```

**Total: ~3 weeks.** W1 (sacp-proxy) deferred until upstream updates.
W7/W7a/W7c/W8-build eliminated (existing TS packages).

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
| [sdk-alignment.md](sdk-alignment.md) | 🔜 Track A | Standalone binaries, subprocess model. W1 (sacp-proxy) BLOCKED. |
| [known-limitations-sdd.md](known-limitations-sdd.md) | 🔜 Track A+B | Storage, API routing, filesystem, terminals, runtime providers |
| [electric-sync-sdd.md](electric-sync-sdd.md) | 🔜 Track B+C | Native StreamDB integration — eliminates W7/W7a/W7c/W8-build |
| [event-subscribers-sdd.md](event-subscribers-sdd.md) | ⚠️ Superseded | Replaced by StreamDB approach in `electric-sync-sdd.md` |
| [flamecast-integration-sdd.md](flamecast-integration-sdd.md) | 🔜 Track C | Flamecast API, pluggable transports, what to cut |
| [workstreams.md](workstreams.md) | Reference | Detailed per-workstream specs |
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
