# Handoff Document

> Status as of 2026-04-07. Start here if you're picking up this project.

## What This Is

A Rust multi-agent orchestrator built on the [ACP](https://agentclientprotocol.com) (Agent Client Protocol) conductor framework. It runs multiple AI coding agents (Claude, Gemini, Codex, etc.) in a single process with a fullscreen TUI dashboard, durable state persistence, and in-process agent-to-agent messaging.

**Repo:** https://github.com/gurdasnijor/durable-acp-rs

## What Works

- **Dashboard TUI** (`cargo run --bin dashboard`) — fullscreen iocraft UI, N agents in one process, streaming responses, tab between agents
- **Agent-to-agent messaging** — agents have `list_agents` + `prompt_agent` MCP tools, route in-process via `AgentRouter` channels
- **Multi-turn sessions** — `unstable_session_usage` feature enabled, sessions survive across prompts
- **Agent config** (`cargo run --bin agents -- add`) — interactive iocraft picker, browses [ACP registry](https://cdn.agentclientprotocol.com/registry/v1/latest/registry.json), writes `agents.toml`
- **Durable state** — all ACP messages persist to an embedded durable streams server, StreamDB materializes collections
- **REST API** — connections, prompt submission, SSE streaming, queue management, peer registry
- **Single-agent chat** (`cargo run --bin chat`) — direct ACP connection with interactive permissions
- **Conductor binary** (`durable-acp-rs`) — editor-spawnable, stdio ACP, works as `agent_command`

## What Doesn't Work / Known Issues

1. **Permission handling in dashboard** — the `cx.spawn` approach for interactive permissions is wired but untested. The permission prompt renders in the TUI but the response channel flow hasn't been verified end-to-end.

2. **`submit_prompt` API bypasses proxy inbound** — the REST API's POST `/prompt` sends `cx.send_request_to(Agent, ...)` which goes directly to the agent, not through `on_receive_request_from(Client, ...)`. State recording is done explicitly in the API handler. This works but duplicates logic from the conductor's proxy handler.

3. **Dead code warnings** — `Output` enum and `AgentHandle` struct in `dashboard.rs` are unused (replaced by direct `TuiState` writes). Should be cleaned up.

4. **Some agents fail to start** — agents with binary distributions (cursor, goose, kimi, opencode) need their binaries installed separately. The dashboard shows them as `✕`. Only `npx`/`uvx`-based agents work out of the box.

5. **Peer prompts block the session** — when agent-a calls `prompt_agent(agent-b, ...)`, agent-a's session blocks until agent-b responds. If agent-b is slow or stuck, agent-a hangs. No timeout on the in-process channel path (the HTTP path has 120s timeout).

6. **No scrollback navigation** — the ScrollView renders output but there's no keyboard scrolling (arrow keys don't scroll the output pane). Would need iocraft's `ScrollView` keyboard integration.

## Architecture (Key Files)

| File | What It Does |
|---|---|
| `src/bin/dashboard.rs` | The main binary. Spawns N in-process conductors, runs iocraft fullscreen TUI, routes prompts via channels |
| `src/conductor.rs` | `DurableStateProxy` (intercepts ACP messages, persists state) + `build_conductor_with_peer_mcp` |
| `src/peer_mcp.rs` | `PeerMcpProxy` — MCP server with `list_agents`/`prompt_agent`, checks `AgentRouter` then falls back to HTTP |
| `src/agent_router.rs` | In-process peer routing — shared map of agent name → prompt channel |
| `src/app.rs` | `AppState` — queue management, chunk recording, durable streams integration |
| `src/state.rs` | `StreamDB` — materializes STATE-PROTOCOL events into `BTreeMap` collections |
| `src/acp_registry.rs` | Fetches agent metadata from ACP CDN, resolves IDs to `npx`/`uvx`/binary commands |
| `src/api.rs` | REST API (axum) — connections, prompts, SSE streaming, queue, registry |
| `src/bin/agents.rs` | Config manager — iocraft registry picker, agents.toml CRUD |
| `src/bin/chat.rs` | Single-agent interactive chat client |

## How the Dashboard Works

```
main()
  → fetch ACP registry
  → start shared EmbeddedDurableStreams + REST API
  → set up AgentRouter (global, for in-process peering)
  → LocalSet::run_until:
      for each agent in agents.toml:
        → spawn_local(run_agent(...))
      → element!(Dashboard).fullscreen().await
```

Each `run_agent`:
```
  → AcpAgent::from_args(command)   // spawns agent subprocess
  → Client.builder()
      .on_receive_request(permission_handler)   // routes to TUI
      .with_spawned(|_cx| conductor.run(duplex_transport))   // in-memory pipes
      .connect_with(client_transport, |connection| {
          initialize → new_session → prompt loop (reads from prompt_rx channel)
      })
```

The TUI and conductors share one `LocalSet`. The iocraft render loop yields between frames, letting conductor tasks process. A 100ms tick timer (`use_future`) forces re-renders so state changes appear.

## Key Dependencies

| Crate | Version | Why |
|---|---|---|
| `sacp` | 11.0.0 | Conductor framework, proxy chain, `ConnectTo`, `Client.builder()` |
| `sacp-conductor` | 11.0.0 | `ConductorImpl`, `ProxiesAndAgent`, `McpBridgeMode` |
| `sacp-tokio` | 11.0.0 | `AcpAgent` — spawns agent subprocess |
| `agent-client-protocol` | 0.10.4 | Typed ACP schema. Uses `unstable_session_usage` feature for `UsageUpdate` |
| `durable-streams-server` | 0.1.3 | Embedded HTTP server for durable streams protocol |
| `iocraft` | 0.8.0 | React-like terminal UI components |
| `axum` | 0.8 | REST API |

`sacp` and `agent-client-protocol` share `agent-client-protocol-schema v0.11.4` — types are identical.

## Design Docs

| Doc | Status | Description |
|---|---|---|
| `docs/architecture.md` | Current | System architecture, state model, REST API, proxy chain |
| `docs/multi-agent-conductor-sdd.md` | ✅ Implemented | Single-process multi-agent dashboard design |
| `docs/flamecast-integration-sdd.md` | 🔜 Ready | Flamecast API compatibility, pluggable transports, what to cut |
| `docs/event-subscribers-sdd.md` | 🔜 Ready | Unified WebSocket/webhook/SSE subscriber model |
| `docs/HANDOFF.md` | Current | This file — start here |
| `SDD-multi-agent-messaging.md` | ⚠️ Superseded | Original messaging SDD — replaced by `agent_router.rs` + `peer_mcp.rs` |

## What to Work On Next

### SDDs Ready for Execution

1. **Flamecast Integration** (`docs/flamecast-integration-sdd.md`) — Session CRUD API, pluggable transports, Flamecast API compatibility. This is the path to replacing Flamecast's backend with durable-acp-rs while keeping the React UI.

2. **Event Subscribers** (`docs/event-subscribers-sdd.md`) — Unified WebSocket + webhook + SSE model. Delivers both Flamecast RFCs (multi-session WebSocket, webhooks) with a single `EventSubscriber` trait.

### Dashboard Polish

3. **Scrollable output** — wire up arrow keys / mouse wheel to iocraft's `ScrollView`.

4. **Peer prompt timeout** — add timeout on in-process `AgentRouter` path.

5. **Permission UX** — verify interactive permission flow end-to-end.

6. **Clean up dead code** — remove `Output` enum, `AgentHandle` struct, warnings.

### Infrastructure

7. **Persistent storage** — SQLite or file-backed durable streams server.

8. **Pluggable transports** — TCP/WebSocket `ByteStreams` for remote agents (spec'd in `flamecast-integration-sdd.md`).

9. **Editor integration** — test `durable-acp-rs` as `agent_command` in Zed, VS Code.

## Flamecast Integration Opportunity

[Flamecast](~/smithery/flamecast) (`@flamecast/sdk`) is the TypeScript control plane for ACP agents — it manages session lifecycle, brokers permissions, persists session metadata, and has a React web UI. It shares the same architectural goals as durable-acp-rs but in a different language.

**The key insight:** durable-acp-rs could plug into Flamecast transparently as the conductor layer, providing durable sessions without Flamecast needing to change.

### How it could work

Flamecast already uses ACP client connections to talk to agents. If you swap the agent command to point at `durable-acp-rs` (which wraps the real agent), Flamecast gets durable state for free:

```
Flamecast (TypeScript)
  → AcpBridge (ACP client connection)
    → durable-acp-rs (conductor, transparent proxy)
      → DurableStateProxy (persists to durable stream)
      → PeerMcpProxy (agent-to-agent tools)
      → claude-agent-acp (the actual agent)
```

Flamecast doesn't know the conductor is there — it sees a standard ACP agent. But now:
- Every session has durable state in a stream
- Flamecast's React UI could subscribe to the durable stream for richer observability
- Agent-to-agent messaging works across Flamecast-managed sessions
- Session recovery on reconnect (replay from stream)

### Integration paths

1. **Transparent proxy** (easiest): Configure Flamecast's runtime provider to use `durable-acp-rs <agent-command>` instead of the raw agent command. Zero code changes to Flamecast. The conductor's REST API and durable streams are available as bonus endpoints.

2. **Shared durable streams**: Point Flamecast's `FlamecastStorage` (currently Drizzle/PGLite/Postgres) at the durable streams server. The STATE-PROTOCOL events are the same schema as `@durable-acp/state` from the TypeScript implementation — they're designed to be cross-compatible.

3. **Replace FlamecastStorage with durable streams**: Instead of Drizzle-backed storage, Flamecast could use the durable stream as its primary storage, with the `StreamDB` pattern for materialization. This would unify the storage layer across both implementations.

4. **Flamecast as dashboard**: Flamecast's React UI is more polished than the TUI dashboard. Running durable-acp-rs as the conductor layer while using Flamecast's web UI for visualization would combine the best of both: Rust performance + TypeScript UI.

### durable-acp-rs as the control plane

The Rust SDK already has `SessionBuilder` ([source](https://github.com/agentclientprotocol/rust-sdk/blob/main/src/agent-client-protocol-core/src/session.rs)) which handles session creation, MCP server injection, and provides `ActiveSession` with `send_prompt` / `read_update`. Combined with `ConductorImpl` + our durable state layer, this IS a control plane. What's needed:

| Flamecast API | Implementation Path |
|---|---|
| `POST /sessions` (create) | Spawn `ConductorImpl` + `AcpAgent`, initialize, create session via `SessionBuilder` |
| `DELETE /sessions/:id` (stop) | Drop the conductor task (kills agent subprocess) |
| `GET /sessions` (list) | Read `connections` collection from `StreamDB` |
| `GET /sessions/:id/events` (stream) | SSE from durable stream filtered by `logical_connection_id` |
| `POST /sessions/:id/prompt` | Already exists in `api.rs` |
| `POST /sessions/:id/permissions/:id/resolve` | Route to permission handler's response channel |
| `GET /agents` (templates) | `acp_registry::fetch_registry()` + `agents.toml` |
| `WS /ws` (bidirectional) | Wrap `StreamDb::subscribe_changes()` + prompt submission |

The `StreamDB` collections (`connections`, `prompt_turns`, `chunks`, `permissions`, `terminals`) map directly to Flamecast's session state. The STATE-PROTOCOL event schema was designed to be compatible with `@durable-acp/state`.

### What to explore

- Check if Flamecast's `AcpBridge` / `@acp/runtime-bridge` can accept a custom agent command (likely yes — the runtime providers already support this)
- Verify the STATE-PROTOCOL event schema matches between the Rust and TypeScript implementations
- Test: `flamecast dev` with agent command set to `durable-acp-rs npx @agentclientprotocol/claude-agent-acp`
- Check if Flamecast's WebSocket protocol can subscribe to the durable stream's SSE endpoint
- Build the session CRUD API endpoints to match Flamecast's API shape — the data layer already exists

## Running It

```bash
# Add agents
cargo run --bin agents -- add     # interactive picker
cargo run --bin agents -- add claude-acp gemini codex-acp  # by ID

# Run dashboard
cargo run --bin dashboard

# Or single-agent chat
cargo run --bin chat

# Build release (much faster agent startup)
cargo build --release
cargo run --release --bin dashboard
```

Requires: `ANTHROPIC_API_KEY` for Claude, `GEMINI_API_KEY` for Gemini, etc.
