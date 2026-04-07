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

See `docs/known-limitations-sdd.md` for full details and fixes.

1. **In-memory storage** — state lost on restart. Fix: file-backed `Storage` impl (~0.5 day).

2. **API bypasses proxy chain** — `POST /prompt` sends directly to agent. Fix: route through `AgentRouter` channel → `session.send_prompt()` (~0.5 day).

3. **Single-instance drain loop** — fine in single-process, needs CAS for multi-instance. Deferred.

4. **Permission handling untested** — `cx.spawn` wiring exists but the TUI response flow isn't verified end-to-end.

5. **Dead code** — `Output` enum, `AgentHandle` struct in `dashboard.rs`. Cleanup needed.

6. **Some agents fail to start** — binary-distribution agents need manual install. Only `npx`/`uvx` work out of the box.

7. **No scrollback** — output auto-scrolls but can't scroll back up.

8. **Peer prompts block** — no timeout on in-process `AgentRouter` path.

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

### Priority 1: Align with ACP SDK Paved Roads

The [Rust SDK v1 RFD](https://agentclientprotocol.com/rfds/rust-sdk-v1)
defines the canonical patterns. We're already using the v1 API (via `sacp`),
but at a low level. Aligning with higher-level abstractions reduces code
and prevents drift from the ecosystem.

**1a. Migrate to `sacp-proxy` v3.0.0** (~1-2 days)

Replace `sacp::Proxy.builder()` with `JrHandlerChain` + `ProxyHandler`.
This cuts `conductor.rs` from ~400 lines to ~150 and gives us:
- Automatic init/forwarding/session tracking/MCP wiring
- `McpServiceRegistry` replaces manual `McpServer::builder()`
- Transport-agnostic `.proxy().serve(ByteStreams)`
- Composable proxy crates

```rust
// Before: manual Proxy.builder() with 6 handler registrations
// After:
JrHandlerChain::new()
    .name("durable-state")
    .on_request_from_client::<PromptRequest>(|req, cx| async { ... })
    .on_notification_from_agent::<SessionNotification>(|notif, cx| async { ... })
    .proxy()
    .serve(transport)
```

**1b. Fix API prompt routing** (~0.5 day)

Route REST API prompts through `AgentRouter` → `session.send_prompt()`
instead of `cx.send_request_to(Agent, ...)`. This sends through the
client transport → conductor queue → proxy chain (correct path). See
`known-limitations-sdd.md` §2.

**1c. Migrate `run.rs` from v0 to v1 API** (~0.5 day)

`run.rs` uses `agent-client-protocol::ClientSideConnection` — the v0
trait-based API that the v1 RFD explicitly replaces. Should use
`sacp::Client.builder().connect_with()` like `dashboard.rs` already does.

**1d. Simplify `agent_router.rs`**

The `AgentRouter` fills a gap the SDK intentionally doesn't address —
cross-conductor peer routing. The SDK's `ActiveSession` manages one
session on one chain. Our router serializes access across N chains.

The custom `PeerPromptRequest` + oneshot channel + bridge task pattern
works but is verbose. Potential simplification: store `ActiveSession`
references directly (wrapped in `Rc<RefCell>` since `!Send` on LocalSet)
and call `session.send_prompt()` / `session.read_to_string()` without
the channel indirection. The serialization concern (one prompt at a time)
is handled by the `&mut self` borrow on `ActiveSession`.

This would eliminate `PeerPromptRequest`, the bridge task in `dashboard.rs`,
and the oneshot response channel — replacing ~100 lines of custom wiring
with direct SDK calls.

**1e. Remove manual JSON deserialization workaround**

Both `chat.rs` and `dashboard.rs` manually deserialize `SessionNotification`
via `serde_json::from_value` to skip unknown variants (the `usage_update`
issue). With `unstable_session_usage` enabled, this workaround may no
longer be needed — test using `MatchDispatch::if_notification` directly.

### Priority 2: Close Remaining Gaps (see `known-limitations-sdd.md`)

| Gap | Effort | Described In |
|---|---|---|
| File system access | ~0.5 day | `known-limitations-sdd.md` §4 |
| Terminal management API | ~1 day | `known-limitations-sdd.md` §5 |
| File-backed storage | ~0.5 day | `known-limitations-sdd.md` §1 |
| WebSocket multiplexing | ~1.5 days | `event-subscribers-sdd.md` |
| Webhooks | ~0.5 day | `event-subscribers-sdd.md` |
| Runtime providers | ~2-3 days/provider | `known-limitations-sdd.md` §6 |

### Priority 3: Flamecast Integration (see `flamecast-integration-sdd.md`)

| Phase | What | Effort |
|---|---|---|
| Phase 1 | Session CRUD API (`/agents` endpoints) | ~1-2 days |
| Phase 2 | Permission resolution + queue API | ~1 day |
| Phase 3 | WebSocket (from event-subscribers-sdd) | ~2-3 days |
| Phase 4 | Agent templates API | ~0.5 day |
| Transports | TCP/WS `ByteStreams` for remote agents | ~1 day |

### Priority 4: Dashboard Polish

- Scrollable output (arrow keys / mouse wheel on `ScrollView`)
- Peer prompt timeout on in-process path
- Permission UX verification
- Dead code cleanup (`Output` enum, `AgentHandle` struct)
- Editor integration testing (Zed, VS Code)

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
