# Durable ACP — Architecture & Design

## What This Is

A Rust ACP Conductor that intercepts all ACP messages, persists them to a
durable state stream, and routes them to actual agents. From the editor's
perspective, it's a standard ACP agent. Under the hood, it provides durable
history, queue management, multi-agent coordination, and real-time
observability.

## ACP Conductor Model

Per the [Conductor Spec](https://agentclientprotocol.github.io/symposium-acp/conductor.html),
a conductor manages **one proxy chain ending in one agent**:

```
Client ──ACP──► Conductor ──spawns & routes──► [Proxy0 → Proxy1 → ... → Agent]
```

Key rules from the spec:
- **Client** and **Agent** are terminal roles (no predecessor/successor respectively)
- **Proxies** are non-terminal — they forward messages between predecessor and successor
- The conductor sends `proxy/initialize` to proxies and `initialize` to the agent
- A conductor can run in terminal mode (`initialize`) or proxy mode (`proxy/initialize`)
- **One conductor = one proxy chain = one agent**

The conductor does NOT multiplex multiple agents. Multiple agents require
multiple conductor instances.

## Our Proxy Chain

```
Client ──ACP (stdio)──► Conductor
                            │
                            ├── DurableStateProxy
                            │     intercepts session/prompt, session/update,
                            │     request_permission, terminals
                            │     persists all to durable state stream
                            │
                            ├── PeerMcpProxy
                            │     injects list_agents + prompt_agent MCP tools
                            │     via MCP-over-ACP transport
                            │
                            └── Agent (claude-agent-acp, gemini, cline, etc.)
```

### DurableStateProxy (`src/conductor.rs`)

Implements `ConnectTo<Conductor>`. Intercepts:
- `PromptRequest` from Client → creates PromptTurnRow, enqueues, drives queue
- `SessionNotification` from Agent → records chunks (text, tool calls, thinking)
- `RequestPermissionRequest` from Agent → records permission lifecycle
- `CreateTerminalRequest` / `TerminalOutputRequest` → records terminal state

All state written as STATE-PROTOCOL events to the embedded durable stream.

### PeerMcpProxy (`src/peer_mcp.rs`)

Implements `ConnectTo<Conductor>`. Injects two MCP tools via
[MCP-over-ACP](https://agentclientprotocol.com/rfds/mcp-over-acp):

- `list_agents` — reads local registry, returns peer names + URLs
- `prompt_agent` — HTTP POST to peer's REST API, SSE streams the response

The conductor's `McpBridgeMode` handles the `mcp/connect`, `mcp/message`,
`mcp/disconnect` protocol automatically. Agents that support MCP-over-ACP
(like `claude-agent-acp`) see these as native tools.

## Multi-Agent Architecture

Since one conductor = one agent, multi-agent setups require N conductor
processes. The dashboard/runner manages this:

```
┌─────────────────────────────────────────────────────────────────┐
│  Dashboard / Runner                                             │
│                                                                 │
│  ClientSideConnection ──► Conductor-A ──► DurableState ──► Agent-A
│  ClientSideConnection ──► Conductor-B ──► DurableState ──► Agent-B
│  ClientSideConnection ──► Conductor-C ──► DurableState ──► Agent-C
│                               │                                 │
│                          REST API per conductor                 │
│                          (submit prompt, stream chunks)         │
│                                                                 │
│  Local Registry (~/.config/durable-acp/registry.json)           │
│  Each conductor registers on startup, unregisters on exit       │
└─────────────────────────────────────────────────────────────────┘
```

### Why N processes, not one?

The ACP spec defines a conductor as managing one proxy chain with one agent.
`ConductorImpl::new_agent()` takes a single `ProxiesAndAgent`. This is
intentional — the conductor handles initialization, capability negotiation,
and message routing for one chain.

Multi-agent coordination happens at a higher layer:
- **Registry** — file-based discovery of peer conductors
- **MCP tools** — agents call `prompt_agent` to message peers via HTTP
- **Dashboard** — TUI/REST interface multiplexing across conductors
- **Durable streams** — each conductor's state is independently observable

### Agent-to-Agent Communication

```
Agent A's Claude session                              Agent B's Conductor
        │                                                     │
        │  list_agents() ──► local registry                   │
        │  ◄── [{name: "agent-b", api: ":4440"}]             │
        │                                                     │
        │  prompt_agent(name="agent-b", text="...")           │
        │      POST :4440/api/v1/connections/{id}/prompt ────►│
        │      GET  :4440/api/v1/prompt-turns/{id}/stream ───►│
        │      ◄── SSE chunks (text, tool_call, stop)         │
        │  ◄── complete text response as tool result          │
```

The `prompt_agent` MCP tool:
1. Reads the local registry for the peer's API URL
2. Finds an attached connection + session via REST
3. Submits a prompt (`POST /prompt`)
4. Streams the response via SSE (`GET /stream`)
5. Returns accumulated text as the tool result (120s timeout)

## State Model

All state persists to a single durable stream per conductor as
[STATE-PROTOCOL](https://github.com/durable-streams/durable-streams/blob/main/packages/state/STATE-PROTOCOL.md) events:

```json
{
  "headers": { "operation": "insert", "type": "prompt_turn" },
  "key": "turn-uuid",
  "value": { "promptTurnId": "...", "state": "queued", ... }
}
```

### Collections

| Collection | Key | Lifecycle |
|---|---|---|
| `connections` | logical_connection_id | created → attached → broken/closed |
| `prompt_turns` | prompt_turn_id | queued → active → completed/cancelled/broken |
| `chunks` | chunk_id | text, tool_call, thinking, tool_result, stop, error |
| `permissions` | request_id | pending → resolved |
| `terminals` | terminal_id | open → exited/released |
| `pending_requests` | request_id | pending → resolved/orphaned |

### StreamDB (`src/state.rs`)

In-memory materialization of state from stream events:
- `BTreeMap<String, T>` per collection
- `broadcast::Sender<CollectionChange>` for reactive subscriptions
- `apply_json_message()` applies insert/update/delete operations
- `snapshot()` returns a cloned copy for read access

## REST API

Each conductor exposes an HTTP API at `port + 1`:

| Endpoint | Method | Purpose |
|---|---|---|
| `/api/v1/connections` | GET | List connections |
| `/api/v1/connections/{id}/prompt` | POST | Submit prompt (returns `promptTurnId`) |
| `/api/v1/connections/{id}/cancel` | POST | Cancel active prompt |
| `/api/v1/connections/{id}/queue` | GET | List queued prompts |
| `/api/v1/connections/{id}/queue/pause` | POST | Pause queue |
| `/api/v1/connections/{id}/queue/resume` | POST | Resume queue |
| `/api/v1/prompt-turns/{id}/stream` | GET | SSE stream of chunks |
| `/api/v1/prompt-turns/{id}/chunks` | GET | All chunks (non-streaming) |
| `/api/v1/registry` | GET | Peer registry |

### SSE Streaming

`GET /api/v1/prompt-turns/{id}/stream` returns Server-Sent Events:
- Sends existing chunks immediately
- Subscribes to `StreamDb.subscribe_changes()` for live updates
- Closes on stop/error chunk or 120s timeout
- Supports `?afterSeq=N` for resumable streaming

## Binaries

| Binary | Purpose | Transport |
|---|---|---|
| `durable-acp-rs` | Conductor — spawned by editors | ACP over stdio |
| `dashboard` | Multi-agent TUI | REST API + SSE |
| `run` | Headless multi-agent runner | REST API |
| `chat` | Interactive single-agent chat | ACP over stdio |
| `agents` | Config manager for agents.toml | — |
| `install` | Agent installer from ACP registry | — |
| `peer` | Agent-to-agent CLI | REST API + SSE |

## Key Dependencies

| Crate | Role |
|---|---|
| `sacp` | Conductor framework — proxy chain, message routing, `ConnectTo` |
| `sacp-conductor` | `ConductorImpl`, `ProxiesAndAgent`, `McpBridgeMode` |
| `sacp-tokio` | `AcpAgent` — spawns agent as subprocess |
| `agent-client-protocol` | Typed ACP schema — same `agent-client-protocol-schema` as sacp |
| `durable-streams-server` | Embedded HTTP server for durable streams |
| `iocraft` | Terminal UI components for dashboard and installer |

`sacp` and `agent-client-protocol` share `agent-client-protocol-schema v0.11.4`,
so types like `PromptRequest`, `SessionNotification`, `StopReason` are identical.

## Known Limitations

- **In-memory storage** — durable streams reset on restart (future: SQLite/file-backed)
- **No authentication** between agents — registry is local trust
- **`submit_prompt` API bypasses proxy inbound path** — records state explicitly
  in the API handler rather than routing through `on_receive_request_from(Client)`
- **`usage_update` session notification** — not in schema v0.11.4, skipped gracefully
- **Single-instance drain loop** — multi-instance deferred to Durable Streams CAS

## References

- [ACP Protocol Overview](https://agentclientprotocol.com/protocol/overview)
- [Proxy Chains RFD](https://agentclientprotocol.com/rfds/proxy-chains)
- [MCP-over-ACP RFD](https://agentclientprotocol.com/rfds/mcp-over-acp)
- [Conductor Spec](https://agentclientprotocol.github.io/symposium-acp/conductor.html)
- [Agent Registry](https://agentclientprotocol.com/registry)
- [Rust SDK](https://github.com/agentclientprotocol/rust-sdk)
- [Cookbook](https://github.com/agentclientprotocol/rust-sdk/tree/main/src/agent-client-protocol-cookbook)
- [Durable Streams Protocol](https://github.com/durable-streams/durable-streams/blob/main/PROTOCOL.md)
- [STATE-PROTOCOL](https://github.com/durable-streams/durable-streams/blob/main/packages/state/STATE-PROTOCOL.md)
