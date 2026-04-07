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
- `prompt_agent` — routes through `AgentRouter` (in-process channels) or
  falls back to HTTP REST API for cross-process/remote peers

The conductor's `McpBridgeMode` handles `mcp/connect`, `mcp/message`,
`mcp/disconnect` automatically. Agents see these as native tools.

## Multi-Agent Architecture

**Implemented:** Single-process, N in-process conductors (see `multi-agent-conductor-sdd.md`).

```
┌──────────────────────────────────────────────────────────────┐
│  Dashboard Process (cargo run --bin dashboard)                │
│                                                              │
│  TUI (iocraft) ←── in-process channels ──→ Agent Manager     │
│                                                              │
│  Conductor A: Client → DurableStateProxy → PeerMcpProxy → Agent
│  Conductor B: Client → DurableStateProxy → PeerMcpProxy → Agent
│  Conductor C: Client → DurableStateProxy → PeerMcpProxy → Agent
│                                                              │
│  Shared: EmbeddedDurableStreams + REST API (one port pair)    │
│  Shared: AgentRouter (in-process peer messaging)             │
└──────────────────────────────────────────────────────────────┘
```

One conductor per agent, all in-process on a single `LocalSet`. Connected
via `tokio::io::duplex` (in-memory pipes). iocraft render loop and conductor
tasks interleave on the same thread.

### Agent-to-Agent Communication

```
Agent A calls prompt_agent(name="agent-b", text="...")
  → PeerMcpProxy checks AgentRouter (in-process)
  → Routes through agent-b's session channel
  → Collects streamed response → returns as tool result
  → Falls back to HTTP if agent is in a different process
```

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

Shared API for all agents at `port + 1` (default 4438):

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
| `dashboard` | Fullscreen multi-agent TUI (primary interface) | In-process channels |
| `agents` | Config manager for agents.toml + ACP registry picker | — |
| `chat` | Interactive single-agent chat | ACP over stdio |
| `run` | Headless multi-agent runner | In-process channels |
| `peer` | Agent-to-agent CLI | REST API + SSE |
| `durable-acp-rs` | Conductor — spawned by editors as `agent_command` | ACP over stdio |

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
- **Single-instance drain loop** — multi-instance deferred to Durable Streams CAS
- **No scrollback navigation** — output pane auto-scrolls but no keyboard scroll
- **Peer prompts block the session** — no timeout on in-process routing path

## Target Architecture

The current system works end-to-end: multi-agent dashboard, in-process
peering, durable state. The target architecture extends it across three axes:

### 1. Unified Event Subscribers (see `event-subscribers-sdd.md`)

```
StreamDb::subscribe_changes()
         │
    SubscriberManager
    ┌────┼──────────────┐
    ▼    ▼              ▼
   WS    Webhook        SSE
```

All three are the same `EventSubscriber` trait — subscribe to changes,
filter by session/event, dispatch via different transport. Delivers both
[Flamecast RFCs](https://flamecast.mintlify.app/rfcs/) (multi-session
WebSocket, webhooks). The durable stream gives replay (`?since=N`) for
all three.

**Separation from ACP spec:**
- Proxies intercept (per [Proxy Chains RFD](https://agentclientprotocol.com/rfds/proxy-chains))
- Subscribers consume (our addition)
- The durable stream bridges them

### 2. Pluggable Transports (see `flamecast-integration-sdd.md`)

```
ConductorImpl::run(transport: ByteStreams<W, R>)
                         ▲
            ┌────────────┼────────────┐
            │            │            │
      stdio (local)   TCP/TLS    WebSocket
      AcpAgent        TcpStream  tokio-tungstenite
      (today)         (trivial)  (trivial)
```

All three are `AsyncRead + AsyncWrite`. The conductor, proxies, and
StreamDB don't know which transport they're on. Enables:
- Remote agents on GPU servers / cloud VMs / E2B sandboxes
- Remote conductors connected from dashboard via TCP
- Cross-machine peering via HTTP fallback (already works)

```toml
# agents.toml
[[agent]]
name = "local-claude"
agent = "claude-acp"

[[agent]]
name = "remote-gemini"
transport = "tcp"
host = "gpu-server.internal"
port = 9000
```

### 3. Flamecast-Compatible Control Plane (see `flamecast-integration-sdd.md`)

```
┌─────────────────────────────────────────────────────────┐
│  durable-acp-rs                                         │
│                                                         │
│  Phase 1: Session CRUD API                              │
│    POST/GET/DELETE /agents — create, list, terminate    │
│    POST /agents/:id/prompts — send prompt               │
│    GET /agents/:id/stream — SSE events                  │
│                                                         │
│  Phase 2: Permissions + Queue                           │
│    POST /agents/:id/permissions/:id — resolve           │
│    GET/PUT/DELETE /agents/:id/queue                     │
│                                                         │
│  Phase 3: WebSocket (unified subscriber)                │
│    WS /ws — channel-based multiplex                     │
│    subscribe/prompt/permission/queue/terminal            │
│                                                         │
│  Phase 4: Agent Templates                               │
│    GET/POST/PUT /agent-templates                        │
└─────────────────────────────────────────────────────────┘
         ▲                              ▲
    Flamecast React UI              Any HTTP client
    (drop AcpBridge,                (curl, scripts,
     FlamecastStorage,               other services)
     event bus)
```

Flamecast's React UI points at durable-acp-rs endpoints. Flamecast cuts:
- `AcpBridge` / `runtime-bridge` → `ConductorImpl`
- `FlamecastStorage` (PGLite/Postgres) → durable streams + StreamDB
- Event bus → `StreamDb::subscribe_changes()`
- Session lifecycle → `SessionBuilder` + `ActiveSession`

Flamecast gains:
- MCP peering across all sessions (automatic)
- In-process multi-agent (N agents, one process)
- Durable sessions (replay from stream)
- Pluggable transports (Docker/E2B agents via TCP)

### 4. Persistent Storage + API Fix (see `known-limitations-sdd.md`)

```
EmbeddedDurableStreams
    ├── InMemoryStorage (today)
    └── FileStorage (target)
            base_dir: ~/.local/share/durable-acp/streams/
            format: append-only .jsonl per stream
            startup: read + replay into StreamDB

API prompt routing (target):
    API handler → AgentRouter.prompt() → prompt_tx channel
      → session.send_prompt() → client transport
      → ConductorMessage queue → proxy chain → agent
    (same path as TUI and peer prompts)
```

### Target Architecture Diagram

```
┌──────────────────────────────────────────────────────────────────┐
│  durable-acp-rs process                                          │
│                                                                  │
│  ┌─ Frontends ────────────────────────────────────────────────┐  │
│  │  TUI (iocraft)    REST API (axum)    WebSocket    Webhooks │  │
│  └────────┬──────────────┬──────────────────┬────────────┬───┘  │
│           │              │                  │            │       │
│  ┌────────▼──────────────▼──────────────────▼────────────▼───┐  │
│  │  SubscriberManager + AgentRouter                           │  │
│  │  (in-process channels, event dispatch)                     │  │
│  └────────┬───────────────────────────────────────────────────┘  │
│           │                                                      │
│  ┌────────▼───────────────────────────────────────────────────┐  │
│  │  N × ConductorImpl (one per agent)                         │  │
│  │                                                            │  │
│  │  Conductor A: Client → DurableStateProxy → PeerMcpProxy   │  │
│  │                → Agent (stdio / TCP / WebSocket)           │  │
│  │  Conductor B: Client → DurableStateProxy → PeerMcpProxy   │  │
│  │                → Agent (stdio / TCP / WebSocket)           │  │
│  └────────┬───────────────────────────────────────────────────┘  │
│           │                                                      │
│  ┌────────▼───────────────────────────────────────────────────┐  │
│  │  Durable Streams Server (one instance)                     │  │
│  │  State Stream → StreamDB (materialized collections)        │  │
│  │  Storage: InMemory → FileStorage (target)                  │  │
│  └────────────────────────────────────────────────────────────┘  │
└──────────────────────────────────────────────────────────────────┘
```

## SDDs

| Doc | Status | Description |
|---|---|---|
| `multi-agent-conductor-sdd.md` | ✅ Implemented | Single-process multi-agent dashboard |
| `flamecast-integration-sdd.md` | 🔜 Ready | Flamecast API + pluggable transports (~5 days) |
| `event-subscribers-sdd.md` | 🔜 Ready | WebSocket + webhooks + SSE (~3 days) |
| `known-limitations-sdd.md` | 🔜 Ready | Storage, API fix, drain loop (~1.5 days for first two) |

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
