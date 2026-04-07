# SDD: Multi-Agent Peer-to-Peer Messaging over durable-acp-rs

## Overview

This document describes the changes needed to allow multiple `durable-acp-rs` instances (each
wrapping a Claude agent via `@agentclientprotocol/claude-agent-acp`) to send prompts to one
another and receive streamed responses, forming a peer-to-peer agent mesh.

---

## Current Architecture

```
┌────────────────────────────────────────────────────────────────┐
│  durable-acp-rs process                                        │
│                                                                │
│  ┌──────────────────┐   sacp stdio   ┌─────────────────────┐  │
│  │  DurableStateProxy│◄──────────────►│ claude-agent-acp   │  │
│  │  (conductor)     │   (port 4437)  │ (node subprocess)  │  │
│  └──────────────────┘                └─────────────────────┘  │
│           │                                                    │
│  ┌────────▼──────────┐  append-only  ┌─────────────────────┐  │
│  │  AppState         │──────────────►│ InMemoryStorage     │  │
│  │  - logical_conn   │               │ (durable-streams)   │  │
│  │  - prompt queue   │               │ SSE at :4437/streams│  │
│  │  - session map    │               └─────────────────────┘  │
│  └────────┬──────────┘                                        │
│           │                          ┌─────────────────────┐  │
│  ┌────────▼──────────┐               │  StreamDb           │  │
│  │  HTTP API :4438   │               │  (in-memory replica │  │
│  │  POST .../prompt  │               │  + broadcast chan)  │  │
│  │  GET  .../queue   │               └─────────────────────┘  │
│  └───────────────────┘                                        │
└────────────────────────────────────────────────────────────────┘
```

**Key types** (already exist):
- `ChunkRow` (`state.rs`) — every agent output chunk stored with `prompt_turn_id` + monotonic `seq`
- `PromptTurnRow` — full lifecycle: `Queued → Active → Completed | Broken | Cancelled`
- `StreamDb.subscribe_changes()` — `broadcast::Receiver<CollectionChange>` for real-time events
- `submit_prompt` (`api.rs:64`) — fires `PromptRequest` into the live ACP session; currently returns
  `{ queued: bool }` without a `promptTurnId`

**Gap**: `submit_prompt` is fire-and-forget. The caller has no turn ID to track the response and
no endpoint to read chunks from.

---

## Design

### 1. Fix `submit_prompt` — return a `promptTurnId`

`submit_prompt` currently spawns a task and returns immediately. The turn ID is created inside
`AppState::enqueue_prompt` (`app.rs:98`) but never surfaced. Two changes:

**`app.rs`** — `enqueue_prompt` already returns `Result<String>` (the `prompt_turn_id`). The
caller just drops it. Keep this signature.

**`api.rs`** — change `PromptAccepted`:

```rust
// Before
struct PromptAccepted { queued: bool }

// After
struct PromptAccepted {
    queued: bool,
    prompt_turn_id: String,
}
```

In `submit_prompt`, call `enqueue_prompt` synchronously (it's already `async`), capture the
returned `prompt_turn_id`, and include it in the response. Remove the `tokio::spawn` wrapper —
`enqueue_prompt` doesn't block.

### 2. New endpoint: `GET /api/v1/prompt-turns/{id}/stream` (SSE)

Add to `api::router`:

```rust
.route("/api/v1/prompt-turns/{id}/stream", get(stream_prompt_turn))
```

Handler behaviour:
1. Subscribe to `app.durable_streams.stream_db.subscribe_changes()` — a
   `broadcast::Receiver<CollectionChange>`.
2. Read existing chunks from the snapshot for this `prompt_turn_id` and send them immediately as
   SSE events.
3. On each `CollectionChange::Chunks` notification, diff against last-sent `seq`, emit new chunks.
4. When a chunk with `ChunkType::Stop` or `ChunkType::Error` is emitted, close the stream.

SSE event format (one JSON object per `data:` line):

```json
{ "seq": 0, "type": "text",      "content": "Hello, " }
{ "seq": 1, "type": "text",      "content": "world."  }
{ "seq": 2, "type": "tool_call", "content": "{...}"   }
{ "seq": 3, "type": "stop",      "content": "{\"stopReason\":\"end_turn\"}" }
```

Use `axum::response::Sse` with `tokio_stream::wrappers::BroadcastStream`.

**Why SSE and not polling?** `StreamDb` already has a broadcast channel. SSE lets the calling
agent await the response in a single blocking read rather than polling in a loop.

### 3. Agent Registry

Each instance needs to know the API URL and current session ID of its peers. Use a simple
shared JSON file (appropriate for local dev; swap for a central HTTP registry later).

#### Registry file: `~/.config/durable-acp/registry.json`

```json
{
  "agents": [
    {
      "name": "agent-a",
      "api_url": "http://127.0.0.1:4438",
      "logical_connection_id": "<uuid>",
      "registered_at": 1712345678000
    }
  ]
}
```

#### `--name` CLI flag

Add to `Cli` in `main.rs`:

```rust
#[arg(long, default_value = "default")]
name: String,
```

#### Registration on startup (`main.rs`)

After `AppState::new`, write an entry to the registry file. On clean exit (SIGTERM / CTRL-C),
remove it. Implement with a `tokio::signal::ctrl_c()` handler.

#### New endpoint: `GET /api/v1/registry` (proxy)

Returns the full contents of the registry file as JSON, so agents running under Claude can
discover peers via a single HTTP GET without needing filesystem access.

### 4. The Agent-to-Agent Tool (MCP or HTTP client binary)

Two delivery options:

#### Option A: MCP tool injected via `McpBridgeMode` (preferred)

`build_conductor` already passes `McpBridgeMode::default()`. Define a new MCP server
(`src/peer_mcp.rs`) exposing two tools:

**`list_agents`** — no args → returns `[{ name, apiUrl, logicalConnectionId }]` from the
registry.

**`prompt_agent`** — args: `{ name: string, text: string }` → performs the full
submit-and-stream sequence (see §5) and returns the agent's complete text response as a string.

Wire this MCP server into the conductor using `sacp_conductor`'s MCP bridge so Claude sees
these as native tools alongside its existing tools.

#### Option B: Standalone CLI binary `src/bin/peer.rs`

Claude calls it via `Bash` tool:

```bash
peer-prompt --agent agent-b --text "Summarise the findings"
# stdout: the complete text response from agent-b
# stderr: streaming progress indicators
```

Option A is cleaner (no shell escaping, streaming visible in tool call UI). Option B is simpler
to implement and works with any agent that has a Bash tool.

### 5. Full Submit-and-Await Sequence

Whether implemented as an MCP tool or a CLI binary, the sequence is:

```
1. GET  {peerApiUrl}/api/v1/connections
        → find connection where state == "attached"
        → extract latestSessionId

2. POST {peerApiUrl}/api/v1/connections/{logicalConnectionId}/prompt
        body: { sessionId, text }
        → response: { promptTurnId, queued }

3. GET  {peerApiUrl}/api/v1/prompt-turns/{promptTurnId}/stream   (SSE)
        → accumulate "text" chunks
        → close on "stop" or "error" chunk
        → return accumulated text (or error message)
```

Timeout: configurable, default 120 s. If the SSE connection drops before stop, retry from last
received `seq` using `?after_seq={n}` query param (add to the stream endpoint).

---

## Files to Create / Modify

| File | Change |
|------|--------|
| `src/api.rs` | Fix `PromptAccepted`, add `stream_prompt_turn` handler, add `GET /api/v1/registry` |
| `src/app.rs` | Surface `prompt_turn_id` from `submit_prompt` task |
| `src/main.rs` | Add `--name` flag, write/clean registry entry, wire peer MCP server |
| `src/peer_mcp.rs` | **New** — MCP server with `list_agents` + `prompt_agent` tools |
| `src/registry.rs` | **New** — read/write `~/.config/durable-acp/registry.json` |
| `src/bin/peer.rs` | **New** (Option B only) — CLI wrapper for the submit-and-await sequence |

No changes needed to `conductor.rs`, `durable_streams.rs`, `state.rs`, or `lib.rs`.

---

## Non-Goals (out of scope for this implementation)

- Persistent storage (registry is ephemeral; in-memory stream resets on restart)
- Authentication between agents
- Multi-hop routing (agent A → agent B → agent C in a chain requires each agent to invoke
  `prompt_agent` from within its own turn, which works automatically once the tool exists)
- Remote agents (all instances are local; URL scheme supports remote later)

---

## Running a Two-Agent Setup (once implemented)

Terminal 1:
```bash
cargo run --bin durable-acp-rs -- --port 4437 --name agent-a \
  npx @agentclientprotocol/claude-agent-acp
```

Terminal 2:
```bash
cargo run --bin durable-acp-rs -- --port 4439 --name agent-b \
  npx @agentclientprotocol/claude-agent-acp
```

Agent A's Claude instance will have `list_agents` and `prompt_agent` tools available. Asking it
"send a message to agent-b and ask it to write a haiku" will cause it to:
1. Call `list_agents` → discovers agent-b at `:4440`
2. Call `prompt_agent(name="agent-b", text="write a haiku")`
3. Block until agent-b responds, then return the haiku as the tool result
