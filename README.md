# durable-acp-rs

A Rust [ACP Conductor](https://agentclientprotocol.github.io/symposium-acp/conductor.html) that persists all ACP messages to a durable state stream. From the editor's perspective, it's a standard [ACP agent](https://agentclientprotocol.com/protocol/overview#agent). Under the hood, it intercepts all ACP traffic, persists it, and routes to the actual agent.

Any [ACP client](https://agentclientprotocol.com/protocol/overview#client) (editors, CLIs, browser UIs) gets durable history, queue management, multi-session support, and real-time observability without requiring the client or agent to change.

## Quick Start

```bash
# Interactive chat with Claude (default agent)
cargo run --bin chat

# Chat with a specific agent
cargo run --bin chat -- npx @agentclientprotocol/claude-agent-acp

# Run the conductor directly (for editor integration)
cargo run --bin durable-acp-rs -- npx @agentclientprotocol/claude-agent-acp
```

## ACP Agent Registry

Any agent in the [ACP Registry](https://agentclientprotocol.com/registry) works. Browse the full list:

```bash
curl -s https://cdn.agentclientprotocol.com/registry/v1/latest/registry.json | python3 -m json.tool
```

Popular agents (npx-based):

| Agent | Command |
|---|---|
| Claude | `npx @agentclientprotocol/claude-agent-acp` |
| Gemini | `npx @anthropic-ai/gemini-acp` |
| Codex | `npx @anthropic-ai/codex-acp` |
| Cline | `npx cline --acp` |
| GitHub Copilot | `npx @anthropic-ai/github-copilot-cli-acp` |

Use any of them with either binary:

```bash
cargo run --bin chat -- npx cline --acp
cargo run --bin durable-acp-rs -- npx cline --acp
```

## Binaries

### `chat` -- Interactive ACP Client

A terminal chat client that connects directly to an ACP agent. Useful for testing and development.

```bash
cargo run --bin chat [-- AGENT_COMMAND...]
```

- Default agent: `npx @agentclientprotocol/claude-agent-acp`
- Streams text responses, shows tool calls with titles, displays thinking
- Auto-approves permission requests (logs them to stderr)
- `Ctrl-D` to quit

### `durable-acp-rs` -- Durable ACP Conductor

The main conductor binary. Designed to be spawned by an ACP client (editor) as `agent_command`. Communicates over stdin/stdout via ACP JSON-RPC.

```bash
cargo run --bin durable-acp-rs -- [OPTIONS] AGENT_COMMAND...
```

**Options:**

| Flag | Default | Description |
|---|---|---|
| `--port PORT` | `4437` | Durable streams server port |
| `--state-stream NAME` | `durable-acp-state` | State stream name |

**Ports used:**

- `PORT` (default 4437) -- Durable streams HTTP server (append/subscribe/SSE)
- `PORT+1` (default 4438) -- REST API for external clients

**Editor integration:**

Configure your editor to use the conductor as its agent command:

```
agent_command: durable-acp-rs npx @agentclientprotocol/claude-agent-acp
```

The editor sends standard ACP over stdio. It doesn't know about durable streams.

## REST API

While the conductor is running, query state at `http://localhost:4438`:

```bash
# List connections
curl http://localhost:4438/api/v1/connections

# View queued prompts
curl http://localhost:4438/api/v1/connections/{id}/queue

# Submit a prompt (returns promptTurnId for tracking)
curl -X POST http://localhost:4438/api/v1/connections/{id}/prompt \
  -H 'Content-Type: application/json' \
  -d '{"sessionId": "...", "text": "hello"}'
# => {"queued": true, "promptTurnId": "uuid"}

# Stream response chunks via SSE
curl http://localhost:4438/api/v1/prompt-turns/{promptTurnId}/stream
# => data: {"chunkId":"...","promptTurnId":"...","type":"text","content":"Hello","seq":0,...}
# => data: {"chunkId":"...","promptTurnId":"...","type":"stop","content":"{...}","seq":5,...}

# Get all chunks for a prompt turn (non-streaming)
curl http://localhost:4438/api/v1/prompt-turns/{promptTurnId}/chunks

# Resume streaming from a specific sequence number
curl http://localhost:4438/api/v1/prompt-turns/{promptTurnId}/stream?afterSeq=3

# Cancel active prompt
curl -X POST http://localhost:4438/api/v1/connections/{id}/cancel \
  -H 'Content-Type: application/json' \
  -d '{"sessionId": "..."}'

# Pause / resume queue
curl -X POST http://localhost:4438/api/v1/connections/{id}/queue/pause
curl -X POST http://localhost:4438/api/v1/connections/{id}/queue/resume
```

### Submit-and-Stream Pattern

For programmatic use (e.g. agent-to-agent messaging):

```bash
# 1. Find the active connection
CONN=$(curl -s localhost:4438/api/v1/connections | jq -r '.[0].logicalConnectionId')
SESSION=$(curl -s localhost:4438/api/v1/connections | jq -r '.[0].latestSessionId')

# 2. Submit a prompt
TURN=$(curl -s -X POST localhost:4438/api/v1/connections/$CONN/prompt \
  -H 'Content-Type: application/json' \
  -d "{\"sessionId\": \"$SESSION\", \"text\": \"hello\"}" | jq -r '.promptTurnId')

# 3. Stream the response
curl -N localhost:4438/api/v1/prompt-turns/$TURN/stream
```

## Durable Streams

The conductor embeds a [Durable Streams](https://github.com/durable-streams/durable-streams/blob/main/PROTOCOL.md) HTTP server. All state (connections, prompt turns, chunks, permissions, terminals) is persisted as [STATE-PROTOCOL](https://github.com/durable-streams/durable-streams/blob/main/packages/state/STATE-PROTOCOL.md) events to a single append-only stream.

```bash
# Read the raw state stream
curl http://localhost:4437/streams/durable-acp-state

# Subscribe via SSE (live updates)
curl http://localhost:4437/streams/durable-acp-state?live=sse
```

External clients (like the TypeScript `@durable-acp/client`) can subscribe to this stream for reactive UX: queue visibility, history replay, multi-session dashboards.

## Architecture

```
Editor (ACP client)          durable-acp-rs (conductor)           Agent
        |                           |                               |
        |  stdin/stdout (ACP)       |  stdin/stdout (ACP)           |
        |                           |                               |
        |  session/prompt --------->|                               |
        |                           |  -> persist promptTurn        |
        |                           |  -> drain queue               |
        |                           |  session/prompt ------------->|
        |                           |                               |
        |                           |  <-- session/update (text)    |
        |                           |  -> persist chunk             |
        |  <-- session/update       |                               |
        |                           |                               |
        |                           |    State Stream (durable)     |
        |                           |    - connections              |
        |                           |    - promptTurns              |
        |                           |    - chunks                   |
        |                           |    - permissions              |
        |                           |    - terminals                |
```

The conductor occupies both ACP roles:
- **Implements Agent** -- editors see it as a standard ACP agent
- **Implements Client** (via proxy) -- the real agent sees it as a standard ACP client

## Project Structure

```
src/
  main.rs              CLI entry point for the conductor
  lib.rs               Module exports
  conductor.rs         DurableStateProxy -- intercepts ACP messages, persists state
  app.rs               AppState -- runtime state, queue management, chunk recording
  state.rs             StreamDB + schema types (connections, turns, chunks, etc.)
  durable_streams.rs   Embedded durable streams HTTP server
  api.rs               REST API endpoints (axum)
  bin/
    chat.rs            Interactive ACP chat client
tests/
  basic.rs             Unit tests for StreamDB, durable streams, AppState
```

## Dependencies

| Crate | Purpose |
|---|---|
| `sacp` | ACP conductor framework (proxy chain, message routing) |
| `sacp-conductor` | Conductor orchestrator (`ConductorImpl`, `ProxiesAndAgent`) |
| `sacp-tokio` | Agent process spawning (`AcpAgent`) |
| `agent-client-protocol` | Typed ACP schema (requests, responses, notifications) |
| `durable-streams-server` | Embedded durable streams HTTP server |
| `axum` | REST API server |
| `tokio` | Async runtime |

`sacp` and `agent-client-protocol` share the same underlying schema crate (`agent-client-protocol-schema`), so their types are identical and interchangeable.

## Development

```bash
# Build
cargo build

# Run tests
cargo test

# Build release
cargo build --release

# Run with debug logging
RUST_LOG=debug cargo run --bin chat

# Run with sacp trace logging
RUST_LOG=sacp=trace cargo run --bin chat
```

### Prerequisites

- Rust 2024 edition (1.85+)
- Node.js / npm (for `npx @agentclientprotocol/claude-agent-acp`)
- `ANTHROPIC_API_KEY` environment variable (for Claude-based agents)

### Known Issues

- `agent-client-protocol-schema v0.11.4` does not include the `usage_update` session notification variant sent by `claude-agent-acp v0.25.3`. The chat client handles this gracefully by skipping unknown variants.

## References

**ACP:**
- [Protocol Overview](https://agentclientprotocol.com/protocol/overview)
- [Proxy Chains RFD](https://agentclientprotocol.com/rfds/proxy-chains)
- [Conductor Spec](https://agentclientprotocol.github.io/symposium-acp/conductor.html)
- [Rust SDK](https://github.com/agentclientprotocol/rust-sdk)
- [Cookbook](https://github.com/agentclientprotocol/rust-sdk/tree/main/src/agent-client-protocol-cookbook)
- [claude-agent-acp](https://github.com/agentclientprotocol/claude-agent-acp)

**Durable Streams:**
- [Protocol Spec](https://github.com/durable-streams/durable-streams/blob/main/PROTOCOL.md)
- [STATE-PROTOCOL Spec](https://github.com/durable-streams/durable-streams/blob/main/packages/state/STATE-PROTOCOL.md)
- [StreamDB Design](https://github.com/durable-streams/durable-streams/blob/main/docs/stream-db.md)

**Design Docs:**
- [Architecture](../distributed-acp/docs/v1-finish/README.md)
- [Rust Conductor SDD](../distributed-acp/docs/v1-finish/rust-conductor-sdd.md)
