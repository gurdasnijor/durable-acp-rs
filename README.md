# durable-acp-rs

A Rust [ACP Conductor](https://agentclientprotocol.github.io/symposium-acp/conductor.html) that persists all ACP messages to a durable state stream. From the editor's perspective, it's a standard [ACP agent](https://agentclientprotocol.com/protocol/overview#agent). Under the hood, it intercepts all ACP traffic, persists it, and routes to the actual agent.

Any [ACP client](https://agentclientprotocol.com/protocol/overview#client) (editors, CLIs, browser UIs) gets durable history, queue management, multi-session support, and real-time observability without requiring the client or agent to change.

## Quick Start

```bash
# Install agents from the ACP registry (interactive picker)
cargo run --bin install

# Start all agents from agents.toml
cargo run --bin run

# Or chat interactively with a single agent
cargo run --bin chat
```

## Install Agents

Browse and install agents from the [ACP Registry](https://agentclientprotocol.com/registry) (27+ agents):

```bash
# Interactive multi-select with checkbox UI
cargo run --bin install

# Install specific agents by ID
cargo run --bin install -- claude-acp gemini cline

# List what's installed
cargo run --bin install -- --list

# Browse the registry without installing
cargo run --bin run -- --list
```

Agents are installed to `.agent-bin/` and automatically added to `agents.toml`.

## Manage Agents

```bash
cargo run --bin agents                          # list configured agents
cargo run --bin agents -- add claude-acp        # add from ACP registry (auto-assigns port)
cargo run --bin agents -- add gemini --port 4441  # add with explicit port
cargo run --bin agents -- add cline --name my-cline  # add with custom name
cargo run --bin agents -- remove agent-b        # remove by name
cargo run --bin agents -- clear                 # remove all
```

## Multi-Agent Setup

### agents.toml

Define agents to run. Agent IDs are resolved from the [ACP registry](https://cdn.agentclientprotocol.com/registry/v1/latest/registry.json), or specify a raw command:

```toml
[[agent]]
name = "agent-a"
port = 4437
agent = "claude-acp"   # ACP registry ID

[[agent]]
name = "agent-b"
port = 4439
agent = "claude-acp"

# Raw command (no registry lookup)
# [[agent]]
# name = "custom"
# port = 4441
# command = ["node", "my-agent.js", "--acp"]
```

CLI args mirror the TOML structure:

```bash
cargo run --bin run -- --agent claude-acp --name my-agent --port 4437
```

### Start All Agents

```bash
cargo run --bin run
```

This fetches the ACP registry, resolves agent IDs to commands, spawns all conductors, initializes ACP sessions, and keeps everything alive. Ctrl-C stops all.

## Agent-to-Agent Messaging (MCP Peering)

Each conductor injects two [MCP tools](https://agentclientprotocol.com/rfds/mcp-over-acp) into the agent's session via the proxy chain:

- **`list_agents`** -- discover peer agents from the local registry
- **`prompt_agent`** -- send a prompt to a named peer and get the complete response

These appear as native tools alongside the agent's built-in tools (Bash, Read, Write, etc.). No special prompting needed -- agents discover peers automatically.

### How It Works

```
Agent A                    Registry                    Agent B
   |                          |                           |
   |  list_agents() --------->|                           |
   |  <-- [{name: "agent-b",  |                           |
   |        api_url: ":4440"}]|                           |
   |                          |                           |
   |  prompt_agent(                                       |
   |    name="agent-b",       POST /api/v1/.../prompt --->|
   |    text="write a haiku") GET  /api/v1/.../stream --->|
   |                          |  <-- SSE chunks           |
   |  <-- "Ownership and     |                           |
   |       borrowing dance..." |                           |
```

The `prompt_agent` tool:
1. Looks up the peer in the local registry (`~/.config/durable-acp/registry.json`)
2. Finds the peer's active connection and session via REST API
3. Submits a prompt via `POST /api/v1/connections/{id}/prompt`
4. Streams the response via SSE (`GET /api/v1/prompt-turns/{id}/stream`)
5. Returns the accumulated text as the tool result

### Example

With two agents running:

```bash
# In agent-a's session, the agent can:
# 1. Call list_agents → discovers agent-b
# 2. Call prompt_agent(name="agent-b", text="write a haiku about rust")
# 3. Receive agent-b's complete response as a tool result
```

Via the REST API:

```bash
# Ask agent-a to talk to agent-b
curl -X POST localhost:4438/api/v1/connections/{id}/prompt \
  -H 'Content-Type: application/json' \
  -d '{"sessionId": "...", "text": "use list_agents to find peers, then prompt_agent to ask agent-b to write a haiku"}'
```

### Peer CLI

For agent-to-agent messaging from the command line (without going through a conductor):

```bash
# List registered agents
cargo run --bin peer -- list

# Send a prompt to a peer
cargo run --bin peer -- prompt --agent agent-b "write a haiku about rust"
```

## Binaries

| Binary | Purpose |
|---|---|
| `agents` | Manage `agents.toml` -- add, remove, list configured agents |
| `install` | Interactive agent installer -- browse ACP registry, install to `.agent-bin/` |
| `run` | Multi-agent runner -- start all agents from `agents.toml`, maintain ACP sessions |
| `chat` | Interactive terminal chat -- connect directly to an agent, stream responses |
| `peer` | Agent-to-agent CLI -- list running peers, send prompts via REST API |
| `durable-acp-rs` | Conductor binary -- spawned by clients/editors as `agent_command` |

## REST API

While conductors are running, query state at `http://localhost:{port+1}`:

```bash
# List connections
curl localhost:4438/api/v1/connections

# Submit a prompt (returns promptTurnId for tracking)
curl -X POST localhost:4438/api/v1/connections/{id}/prompt \
  -H 'Content-Type: application/json' \
  -d '{"sessionId": "...", "text": "hello"}'
# => {"queued": true, "promptTurnId": "uuid"}

# Stream response chunks via SSE
curl -N localhost:4438/api/v1/prompt-turns/{promptTurnId}/stream

# Get all chunks (non-streaming)
curl localhost:4438/api/v1/prompt-turns/{promptTurnId}/chunks

# Resume streaming from a sequence number
curl localhost:4438/api/v1/prompt-turns/{id}/stream?afterSeq=3

# View queued prompts
curl localhost:4438/api/v1/connections/{id}/queue

# Cancel / pause / resume
curl -X POST localhost:4438/api/v1/connections/{id}/cancel \
  -H 'Content-Type: application/json' -d '{"sessionId": "..."}'
curl -X POST localhost:4438/api/v1/connections/{id}/queue/pause
curl -X POST localhost:4438/api/v1/connections/{id}/queue/resume

# View peer registry
curl localhost:4438/api/v1/registry
```

## Durable Streams

The conductor embeds a [Durable Streams](https://github.com/durable-streams/durable-streams/blob/main/PROTOCOL.md) HTTP server. All state is persisted as [STATE-PROTOCOL](https://github.com/durable-streams/durable-streams/blob/main/packages/state/STATE-PROTOCOL.md) events.

```bash
# Read the raw state stream
curl http://localhost:4437/streams/durable-acp-state

# Subscribe via SSE (live updates)
curl http://localhost:4437/streams/durable-acp-state?live=sse
```

## Architecture

```
                        ┌─────────────────────────────────────────────┐
Client ──ACP──► Conductor                                             │
                │  DurableStateProxy  (intercept + persist)           │
                │  PeerMcpProxy       (list_agents, prompt_agent)     │
                │  Agent (claude-agent-acp, gemini, cline, etc.)      │
                │                                                     │
                │  Embedded Durable Streams Server (:4437)            │
                │  REST API (:4438)                                   │
                │  StreamDB (in-memory materialized state)            │
                └─────────────────────────────────────────────────────┘
                         ▲                              ▲
                    stdio (ACP)                    HTTP/SSE
                         │                              │
                    Editor/CLI                   @durable-acp/client
                                                 or peer agents
```

Proxy chain: `Client → DurableStateProxy → PeerMcpProxy → Agent`

- **DurableStateProxy** -- intercepts all ACP messages, persists to state stream
- **PeerMcpProxy** -- injects `list_agents` and `prompt_agent` MCP tools via [MCP-over-ACP](https://agentclientprotocol.com/rfds/mcp-over-acp)
- **Agent** -- any ACP-compatible agent from the [registry](https://agentclientprotocol.com/registry)

## Project Structure

```
src/
  main.rs              Conductor CLI entry point
  lib.rs               Module exports
  conductor.rs         DurableStateProxy + PeerMcpProxy wiring
  peer_mcp.rs          MCP server with list_agents + prompt_agent tools
  acp_registry.rs      ACP registry client (cdn.agentclientprotocol.com)
  registry.rs          Local agent registry (~/.config/durable-acp/)
  app.rs               AppState -- runtime state, queue, chunk recording
  state.rs             StreamDB + schema types
  durable_streams.rs   Embedded durable streams HTTP server
  api.rs               REST API endpoints (axum)
  bin/
    agents.rs          Agent config manager (add/remove/list)
    install.rs         Interactive agent installer
    run.rs             Multi-agent runner
    chat.rs            Interactive terminal chat client
    peer.rs            Agent-to-agent CLI
agents.toml            Multi-agent configuration
tests/
  basic.rs             Unit tests
```

## Development

```bash
cargo build            # build all binaries
cargo test             # run tests
cargo build --release  # optimized build

# Debug logging
RUST_LOG=debug cargo run --bin chat
RUST_LOG=sacp=trace cargo run --bin run
```

### Prerequisites

- Rust 2024 edition (1.85+)
- Node.js / npm (for npx-based agents)
- `ANTHROPIC_API_KEY` (for Claude), `GEMINI_API_KEY` (for Gemini), etc.

### Known Issues

- `agent-client-protocol-schema v0.11.4` doesn't include `usage_update` sent by `claude-agent-acp v0.25.3`. Handled gracefully by skipping unknown variants.
- The `submit_prompt` API sends directly to the agent (bypasses the proxy's inbound handler). State recording is done explicitly in the API handler.

## References

**ACP:**
- [Protocol Overview](https://agentclientprotocol.com/protocol/overview)
- [Proxy Chains RFD](https://agentclientprotocol.com/rfds/proxy-chains)
- [MCP-over-ACP RFD](https://agentclientprotocol.com/rfds/mcp-over-acp)
- [Conductor Spec](https://agentclientprotocol.github.io/symposium-acp/conductor.html)
- [Agent Registry](https://agentclientprotocol.com/registry)
- [Rust SDK](https://github.com/agentclientprotocol/rust-sdk)
- [Cookbook](https://github.com/agentclientprotocol/rust-sdk/tree/main/src/agent-client-protocol-cookbook)

**Durable Streams:**
- [Protocol Spec](https://github.com/durable-streams/durable-streams/blob/main/PROTOCOL.md)
- [STATE-PROTOCOL Spec](https://github.com/durable-streams/durable-streams/blob/main/packages/state/STATE-PROTOCOL.md)
