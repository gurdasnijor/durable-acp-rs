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

## Agent-to-Agent Messaging

Each agent automatically gets `list_agents` and `prompt_agent` [MCP tools](https://agentclientprotocol.com/rfds/mcp-over-acp) injected via the proxy chain. Agents discover and message peers without any configuration.

```
Agent A                     AgentRouter                   Agent B
   |                            |                            |
   |  list_agents() ----------->|                            |
   |  <-- [agent-a, agent-b,   |                            |
   |       codex-acp, ...]     |                            |
   |                            |                            |
   |  prompt_agent(             |                            |
   |    name="agent-b",         |  in-process channel ------>|
   |    text="write a haiku")   |  <-- streamed response     |
   |  <-- complete text         |                            |
```

In the dashboard, peer communication routes through **in-process channels** — zero HTTP overhead. The `prompt_agent` tool sends a prompt through the peer's session channel, collects streamed text from `read_update`, and returns the complete response.

When agents run in separate processes, `prompt_agent` falls back to the REST API (HTTP POST + SSE streaming).

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

### Durable State

All state persists to a single [durable stream](https://github.com/durable-streams/durable-streams/blob/main/PROTOCOL.md) as [STATE-PROTOCOL](https://github.com/durable-streams/durable-streams/blob/main/packages/state/STATE-PROTOCOL.md) events. Collections: connections, prompt turns, chunks, permissions, terminals.

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
