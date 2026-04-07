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

- `docs/architecture.md` — full system architecture, state model, REST API
- `docs/multi-agent-conductor-sdd.md` — single-process multi-agent design (the "why" behind the dashboard architecture)
- `SDD-multi-agent-messaging.md` — original messaging SDD (partially implemented)

## What to Work On Next

### High Impact

1. **Scrollable output** — wire up arrow keys / mouse wheel to the iocraft `ScrollView`. Currently output scrolls down but you can't scroll back up.

2. **Peer prompt timeout** — add a timeout on the in-process `AgentRouter` path. Currently only the HTTP fallback has a 120s timeout.

3. **Multi-model workflows** — test with heterogeneous agents (Claude + Gemini + Codex). The infrastructure works, but real multi-model collaboration hasn't been exercised.

4. **Permission UX** — verify the interactive permission flow end-to-end in the dashboard. The channel wiring exists but needs testing.

### Medium Impact

5. **Clean up dead code** — remove `Output` enum, `AgentHandle` struct, unused imports/warnings in `dashboard.rs`.

6. **Error display** — agent startup errors (missing binaries, API key issues) should show in the TUI sidebar, not just as `✕`.

7. **Persistent storage** — the durable streams server is in-memory. Adding SQLite or file-backed storage would survive conductor restarts.

8. **Remote agents** — the `AgentRouter` currently only routes in-process. The HTTP fallback works for cross-process, but there's no discovery for remote agents (different machines).

### Lower Priority

9. **Agent status heartbeat** — detect when an agent subprocess dies and update the sidebar status.

10. **Queue visualization** — the REST API exposes queue state but the TUI doesn't show it.

11. **Conductor as editor plugin** — test `durable-acp-rs` as `agent_command` in Zed, VS Code, or other ACP-compatible editors.

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
