# durable-acp-rs

A Rust [ACP Conductor](https://agentclientprotocol.github.io/symposium-acp/conductor.html) with durable state, multi-agent orchestration, and a fullscreen TUI dashboard. Run any combination of [27+ ACP agents](https://agentclientprotocol.com/registry) in a single process with in-process peer-to-peer messaging.

## Quick Start

```bash
# 1. Add agents from the ACP registry (interactive picker)
cargo run --bin agents -- add

# 2. Launch the multi-agent dashboard
cargo run --bin dashboard
```

That's it. The dashboard spawns all agents from `agents.toml`, shows a fullscreen TUI with agent sidebar, streaming output, and a prompt input bar. Tab between agents, type prompts, watch responses stream in real-time.

## Dashboard

```
┌─────────────────────────────────────────────────────────────────────┐
│ durable-acp  5 agents  tab=switch  esc=quit                        │
│                                                                     │
│ ╭─ Agents ──╮ ╭─ agent-a ────────────────────────────────────────╮ │
│ │ ● agent-a │ │ [agent-a] Ready                                  │ │
│ │ > ● agent-b│ │ > hi                                            │ │
│ │ ● codex   │ │ Hi! How can I help you?                          │ │
│ │ ● deep    │ │ > talk to agent-b                                │ │
│ │ ● gemini  │ │ [tool] mcp__peer__prompt_agent                   │ │
│ │           │ │ Agent-b responded: "Hi! I'm Claude..."           │ │
│ ╰───────────╯ ╰──────────────────────────────────────────────────╯ │
│ ╭───────────────────────────────────────────────────────────────╮   │
│ │ [agent-b] > _                                                 │   │
│ ╰───────────────────────────────────────────────────────────────╯   │
└─────────────────────────────────────────────────────────────────────┘
```

**Features:**
- Fullscreen TUI with iocraft (React-like terminal components)
- Agent sidebar with live status (● ready, ○ starting, ✕ error)
- ScrollView output pane with text wrapping
- Tab/Shift-Tab to switch agents
- Interactive permission prompts
- In-process conductors — no subprocess management
- Shared durable streams — one state stream for all agents

## Manage Agents

```bash
cargo run --bin agents                      # list configured agents
cargo run --bin agents -- add               # interactive: browse ACP registry + select
cargo run --bin agents -- add claude-acp    # add specific agent by ID
cargo run --bin agents -- add gemini --name my-gemini  # custom name
cargo run --bin agents -- remove agent-b    # remove by name
cargo run --bin agents -- clear             # remove all
```

Agent IDs resolve from the [ACP Registry](https://agentclientprotocol.com/registry) at runtime — no local installation needed. `npx`, `uvx`, and binary distributions are all supported.

### agents.toml

```toml
[[agent]]
name = "agent-a"
port = 4437
agent = "claude-acp"   # ACP registry ID

[[agent]]
name = "agent-b"
port = 4439
agent = "claude-acp"

[[agent]]
name = "gemini"
port = 4441
agent = "gemini"

# Raw command (no registry lookup)
# [[agent]]
# name = "custom"
# port = 4443
# command = ["node", "my-agent.js", "--acp"]
```

## Agent-to-Agent Messaging (MCP Peering)

Each agent automatically gets `list_agents` and `prompt_agent` tools injected as native MCP tools via the [MCP-over-ACP](https://agentclientprotocol.com/rfds/mcp-over-acp) transport. No configuration, no prompting — the tools appear alongside the agent's built-in tools (Bash, Read, Write, etc.).

### How MCP-over-ACP works

The `PeerMcpProxy` sits in the conductor's proxy chain. When the conductor initializes a session, it:

1. Sends `proxy/initialize` to the proxy (per the [conductor spec](https://agentclientprotocol.github.io/symposium-acp/conductor.html))
2. The proxy declares an MCP server with `list_agents` + `prompt_agent` tools
3. The conductor adds the MCP server URL (`acp:<uuid>`) to the `NewSessionRequest`'s `mcpServers` list
4. The agent connects to the MCP server via `mcp/connect`, `mcp/message` — all over the existing ACP connection
5. The agent calls `tools/list` and discovers the peer tools

The agent doesn't know the tools come from a proxy — they look like any other MCP tool. The conductor handles all the `mcp/connect`/`mcp/message`/`mcp/disconnect` routing via its `McpBridgeMode`.

### In-process routing (dashboard)

When agents run in the same process (the dashboard), peer prompts route through the `AgentRouter` — a shared registry of in-process channels:

```
Agent A calls prompt_agent(name="agent-b", text="...")
  → PeerMcpProxy checks AgentRouter (global, in-process)
  → AgentRouter finds agent-b's prompt channel
  → Sends prompt through agent-b's session (same LocalSet)
  → agent-b's read_update loop streams text into response buffer
  → Complete response returned via oneshot channel
  → Zero HTTP, zero serialization, zero latency
```

### Cross-process routing (separate conductors)

When agents run as separate processes, `prompt_agent` falls back to HTTP:

```
Agent A calls prompt_agent(name="agent-b", text="...")
  → PeerMcpProxy checks AgentRouter (not found — different process)
  → Falls back to HTTP via local registry (~/.config/durable-acp/registry.json)
  → POST agent-b:4440/api/v1/connections/{id}/prompt
  → GET  agent-b:4440/api/v1/prompt-turns/{id}/stream (SSE)
  → Accumulates text chunks until stop
  → Returns complete response as tool result
```

### What agents can do with peering

- **Delegate tasks**: "ask agent-b to review this code"
- **Multi-model workflows**: Claude writes code, Gemini reviews it, Codex tests it
- **Collaborative debugging**: agents share findings via prompts
- **Fan-out**: one agent prompts multiple peers in sequence

The agents don't need special instructions. Claude naturally uses `list_agents` to discover peers and `prompt_agent` to message them when asked to collaborate.

## All Binaries

| Binary | Purpose |
|---|---|
| `dashboard` | Fullscreen multi-agent TUI — the main interface |
| `agents` | Manage `agents.toml` — add/remove agents with iocraft registry picker |
| `chat` | Single-agent interactive chat (direct ACP connection, no conductor) |
| `run` | Headless multi-agent runner (no TUI, for scripts/CI) |
| `peer` | Agent-to-agent CLI — list peers, send prompts |
| `durable-acp-rs` | Conductor binary — spawned by editors as `agent_command` |

## Architecture

### Single-Process Multi-Agent

The dashboard runs N `ConductorImpl` instances in one process, each managing its own proxy chain + agent subprocess. All conductors share one tokio `LocalSet` — the iocraft render loop and conductor tasks interleave on the same thread.

```
┌──────────────────────────────────────────────────────────────┐
│  Dashboard Process                                            │
│                                                              │
│  TUI (iocraft) ←── in-process channels ──→ Agent Manager     │
│                                                              │
│  Conductor A:  Client → DurableStateProxy → PeerMcpProxy → Agent (claude-acp)
│  Conductor B:  Client → DurableStateProxy → PeerMcpProxy → Agent (gemini)
│  Conductor C:  Client → DurableStateProxy → PeerMcpProxy → Agent (codex-acp)
│                                                              │
│  Shared: EmbeddedDurableStreams (:4437) + REST API (:4438)   │
│  Shared: StreamDB (in-memory, all agents' state)             │
│  Shared: AgentRouter (in-process peer messaging)             │
└──────────────────────────────────────────────────────────────┘
```

### Proxy Chain

Each conductor manages one proxy chain per the [ACP Conductor Spec](https://agentclientprotocol.github.io/symposium-acp/conductor.html):

- **DurableStateProxy** — intercepts all ACP messages, persists to durable state stream
- **PeerMcpProxy** — injects `list_agents` + `prompt_agent` MCP tools via [MCP-over-ACP](https://agentclientprotocol.com/rfds/mcp-over-acp)
- **Agent** — any ACP-compatible agent from the [registry](https://agentclientprotocol.com/registry)

### Durable State — The Integration Layer

The durable state stream is the central innovation. Every ACP message that passes through the conductor is persisted to an append-only [durable stream](https://github.com/durable-streams/durable-streams/blob/main/PROTOCOL.md) as [STATE-PROTOCOL](https://github.com/durable-streams/durable-streams/blob/main/packages/state/STATE-PROTOCOL.md) events. This gives you three things for free:

**1. Decoupling across network boundaries.** The state stream is an HTTP resource. Any client — a web dashboard, a CLI, a mobile app, another agent — can subscribe via SSE and see every prompt, every response chunk, every tool call, every permission request as it happens. The client doesn't need to speak ACP or be connected to the conductor's stdio. It just reads HTTP.

```bash
# Any client, anywhere, can watch the full agent session
curl http://conductor-host:4437/streams/durable-acp-state?live=sse
```

**2. Free durability via proxy.** Because the conductor sits in the proxy chain, durability is transparent. The agent and client don't know state is being recorded — they speak standard ACP. Swap in the conductor as `agent_command` and every session is automatically durable. Remove it and everything works the same, just without persistence. Zero code changes on either side.

**3. Durable sessions — reconnect and resume.** The state stream is the complete history. If a client disconnects and reconnects, it replays from the stream and has the full session: every prompt, every response, every tool call. The `StreamDB` materializes the stream into in-memory collections (`connections`, `prompt_turns`, `chunks`, `permissions`, `terminals`) keyed by `logical_connection_id`. External clients subscribe to collection changes for reactive UX.

```
State Stream (append-only log):
  insert connection {id: "abc", state: "created"}
  update connection {id: "abc", state: "attached", sessionId: "xyz"}
  insert prompt_turn {id: "t1", state: "queued", text: "hello"}
  update prompt_turn {id: "t1", state: "active"}
  insert chunk {turnId: "t1", type: "text", content: "Hi!", seq: 0}
  insert chunk {turnId: "t1", type: "text", content: " How can I help?", seq: 1}
  insert chunk {turnId: "t1", type: "stop", seq: 2}
  update prompt_turn {id: "t1", state: "completed"}
```

The `StreamDB` watches this stream and maintains live, queryable collections. Multiple conductors write to the same stream — the `logical_connection_id` partitions each agent's state.

## REST API

```bash
curl localhost:4438/api/v1/connections                          # list all agents' connections
curl localhost:4438/api/v1/connections/{id}/prompt -X POST \    # submit prompt
  -H 'Content-Type: application/json' \
  -d '{"sessionId": "...", "text": "hello"}'
curl -N localhost:4438/api/v1/prompt-turns/{id}/stream          # SSE stream chunks
curl localhost:4438/api/v1/prompt-turns/{id}/chunks             # all chunks
curl localhost:4438/api/v1/registry                             # peer registry
curl localhost:4438/api/v1/connections/{id}/queue                # queued prompts
curl -X POST localhost:4438/api/v1/connections/{id}/queue/pause  # pause queue
```

## Project Structure

```
src/
  main.rs              Conductor CLI (for editor integration)
  conductor.rs         DurableStateProxy + conductor wiring
  peer_mcp.rs          MCP tools: list_agents, prompt_agent
  agent_router.rs      In-process peer routing channels
  acp_registry.rs      ACP registry client (cdn.agentclientprotocol.com)
  registry.rs          Local peer registry (~/.config/durable-acp/)
  app.rs               AppState, queue management, chunk recording
  state.rs             StreamDB, schema types, collections
  durable_streams.rs   Embedded durable streams HTTP server
  api.rs               REST API (axum)
  bin/
    dashboard.rs       Fullscreen multi-agent TUI
    agents.rs          Agent config manager with registry picker
    chat.rs            Single-agent interactive chat
    run.rs             Headless multi-agent runner
    peer.rs            Agent-to-agent CLI
docs/
  architecture.md      System architecture
  multi-agent-conductor-sdd.md  Single-process multi-agent design
agents.toml            Agent configuration
```

## Development

```bash
cargo build                     # all binaries
cargo test                      # unit tests
cargo build --release           # optimized

RUST_LOG=debug cargo run --bin dashboard   # debug logging
RUST_LOG=sacp=trace cargo run --bin chat   # trace ACP messages
```

### Prerequisites

- Rust 2024 edition (1.85+)
- Node.js / npm (for npx-based agents like claude-acp, gemini, cline)
- API keys: `ANTHROPIC_API_KEY` (Claude), `GEMINI_API_KEY` (Gemini), etc.

## References

**ACP:**
- [Protocol Overview](https://agentclientprotocol.com/protocol/overview)
- [Conductor Spec](https://agentclientprotocol.github.io/symposium-acp/conductor.html)
- [Proxy Chains RFD](https://agentclientprotocol.com/rfds/proxy-chains)
- [MCP-over-ACP RFD](https://agentclientprotocol.com/rfds/mcp-over-acp)
- [Agent Registry](https://agentclientprotocol.com/registry)
- [Rust SDK](https://github.com/agentclientprotocol/rust-sdk)
- [Cookbook](https://github.com/agentclientprotocol/rust-sdk/tree/main/src/agent-client-protocol-cookbook)

**Durable Streams:**
- [Protocol Spec](https://github.com/durable-streams/durable-streams/blob/main/PROTOCOL.md)
- [STATE-PROTOCOL](https://github.com/durable-streams/durable-streams/blob/main/packages/state/STATE-PROTOCOL.md)
