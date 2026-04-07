# SDD: Flamecast Integration — What to Cut, What to Share

> **Status: 🔜 READY FOR EXECUTION**
> - Phase 1 (Session CRUD): ~1-2 days — add 6 endpoints to `api.rs`
> - Phase 2 (Permissions + Queue): ~1 day — wire permission channels to API
> - Phase 3 (WebSocket): ~2-3 days — see `event-subscribers-sdd.md`
> - Phase 4 (Agent Templates): ~0.5 day — thin wrapper over `agents.toml` + registry
> - Pluggable Transports: ~1 day — `Transport` enum + TCP/WS `ByteStreams`

## The Opportunity

Flamecast and durable-acp-rs solve the same problem from different angles:
- **Flamecast**: TypeScript control plane with React UI, Hono API, PGLite/Postgres storage
- **durable-acp-rs**: Rust conductor with TUI, durable streams, in-process peering

The Rust layer can replace Flamecast's entire backend while keeping the React UI.
More interestingly, it can add capabilities Flamecast doesn't have (MCP peering,
durable streams, in-process multi-agent) transparently.

## Flamecast API Surface (from source)

### REST API (`packages/flamecast/src/flamecast/api.ts`)

```
GET    /health
GET    /agent-templates          → list registered agent configs
POST   /agent-templates          → register new agent template
PUT    /agent-templates/:id      → update template

GET    /runtimes                 → list runtime instances
POST   /runtimes/:type/start     → start a runtime
POST   /runtimes/:name/stop      → stop runtime
DELETE /runtimes/:name           → delete runtime
POST   /runtimes/:name/pause     → pause runtime
GET    /runtimes/:name/files     → file preview
GET    /runtimes/:name/fs/*      → filesystem/git access

GET    /agents                   → list sessions (agents)
POST   /agents                   → create session
GET    /agents/:id               → get session snapshot
POST   /agents/:id/prompts       → send prompt
POST   /agents/:id/events        → inject event
POST   /agents/:id/permissions/:reqId → resolve permission
DELETE /agents/:id               → terminate session
GET    /agents/:id/queue         → get prompt queue
DELETE /agents/:id/queue/:queueId → remove from queue
DELETE /agents/:id/queue         → clear queue
PUT    /agents/:id/queue         → reorder queue
POST   /agents/:id/queue/pause   → pause queue
POST   /agents/:id/queue/resume  → resume queue
GET    /agents/:id/files         → session file preview
GET    /agents/:id/fs/snapshot   → filesystem snapshot
GET    /agents/:id/stream        → SSE event stream
```

### WebSocket Protocol (`packages/protocol/src/ws-channels.ts`)

Multiplexed channel-based WebSocket at `ws://host/ws`:

**Server → Client:**
- `connected` — connection established with connectionId
- `subscribed` / `unsubscribed` — channel subscription confirmation
- `event` — channel event (session updates, chunks, tool calls, etc.)
- `session.created` / `session.terminated` — lifecycle events
- `error` — error notification
- `pong` — keepalive

**Client → Server:**
- `subscribe` / `unsubscribe` — channel subscription with optional `since` seq
- `prompt` — send prompt to session
- `permission.respond` — resolve permission request
- `cancel` — cancel active prompt
- `terminate` — kill session
- `queue.reorder` / `queue.clear` / `queue.pause` / `queue.resume`
- `terminal.create` / `terminal.input` / `terminal.resize` / `terminal.kill`
- `ping` — keepalive

### Storage Interface (`packages/protocol/src/storage.ts`)

```typescript
interface FlamecastStorage {
  // Agent templates
  seedAgentTemplates(templates)
  listAgentTemplates()
  getAgentTemplate(id)
  saveAgentTemplate(template)
  updateAgentTemplate(id, patch)

  // Sessions
  createSession(meta, runtimeInfo?, webhooks?)
  updateSession(id, patch)
  getSessionMeta(id)
  getStoredSession(id)
  listAllSessions()
  listActiveSessionsWithRuntime()
  finalizeSession(id, reason)

  // Runtimes
  saveRuntimeInstance(instance)
  listRuntimeInstances()
  deleteRuntimeInstance(name)
}
```

## What durable-acp-rs Already Covers

| Flamecast Feature | durable-acp-rs | Status |
|---|---|---|
| Session creation | `ConductorImpl::new_agent()` + `SessionBuilder` | ✅ Working |
| Session termination | Drop conductor task | ✅ Working |
| List sessions | `GET /connections` (StreamDB) | ✅ Working |
| Send prompt | `POST /connections/{id}/prompt` + in-process channels | ✅ Working |
| Stream response | `GET /prompt-turns/{id}/stream` (SSE) | ✅ Working |
| Permission brokering | `on_receive_request(RequestPermissionRequest, ...)` | ✅ Working |
| Queue management | Drain loop, pause/resume | ✅ Working |
| Agent templates | `agents.toml` + ACP registry | ✅ Working |
| Durable state | STATE-PROTOCOL events → StreamDB | ✅ Working |
| MCP tool injection | PeerMcpProxy via MCP-over-ACP | ✅ Working |
| Agent-to-agent messaging | AgentRouter (in-process) + HTTP fallback | ✅ Working |
| File system access | N/A | ❌ Missing |
| Terminal management | Tracked in state but not exposed | ⚠️ Partial |
| WebSocket multiplexing | N/A (SSE only) | ❌ Missing |
| Runtime providers (Docker, E2B) | N/A (local subprocess only) | ❌ Missing |
| Webhooks | N/A | ❌ Missing |

## What Flamecast Could Cut

If durable-acp-rs becomes the conductor/backend layer:

### Cut entirely
- **AcpBridge / runtime-bridge** — replaced by `ConductorImpl` + in-process `Client.builder().with_spawned()`
- **FlamecastStorage (PGLite/Postgres)** — replaced by durable streams + StreamDB
- **Session lifecycle management** — handled by the conductor's `ActiveSession`
- **Event bus** — replaced by `StreamDb::subscribe_changes()` + durable stream SSE

### Keep but rewire
- **React UI** — point at durable-acp-rs REST API + add WebSocket endpoint
- **Agent templates** — read from `agents.toml` or ACP registry CDN via new API
- **Runtime providers** — keep Docker/E2B providers but have them spawn `durable-acp-rs <agent>` instead of raw agent commands

### Gains from integration
- **MCP peering for free** — any session gets `list_agents` + `prompt_agent` automatically
- **Durable sessions** — transparent persistence without Postgres
- **In-process multi-agent** — N agents in one process vs N separate host processes
- **Stream-based observability** — any client subscribes via HTTP SSE, not just WebSocket

## Implementation Plan: Flamecast-Compatible API

Add these endpoints to `api.rs` to match Flamecast's API shape:

### Phase 1: Session CRUD (enables React UI)

```rust
// Map Flamecast's /agents/* to our /api/v1/* namespace
POST   /api/v1/agents              → create session (spawn conductor, init, new_session)
GET    /api/v1/agents              → list sessions (from StreamDB connections)
GET    /api/v1/agents/:id          → session snapshot (connection + recent chunks + queue)
DELETE /api/v1/agents/:id          → terminate session (drop conductor task)
POST   /api/v1/agents/:id/prompts  → send prompt (same as existing /connections/:id/prompt)
GET    /api/v1/agents/:id/stream   → SSE event stream (same as /prompt-turns/:id/stream but session-scoped)
```

### Phase 2: Permission + Queue (full control)

```rust
POST   /api/v1/agents/:id/permissions/:reqId  → resolve permission (route to permission channel)
GET    /api/v1/agents/:id/queue               → list queued prompts
POST   /api/v1/agents/:id/queue/pause         → pause (already exists)
POST   /api/v1/agents/:id/queue/resume        → resume (already exists)
DELETE /api/v1/agents/:id/queue/:queueId      → remove queued item
PUT    /api/v1/agents/:id/queue               → reorder queue
```

### Phase 3: WebSocket (real-time bidirectional)

```rust
// Single multiplexed WebSocket matching Flamecast's channel protocol
WS /ws
  → subscribe/unsubscribe to channels (session events, queue updates)
  → prompt submission
  → permission resolution
  → queue management
  → terminal I/O
```

This is the biggest lift but unlocks the React UI's real-time features.

### Phase 4: Agent Templates API

```rust
GET    /api/v1/agent-templates     → list (from agents.toml + ACP registry)
POST   /api/v1/agent-templates     → register (write to agents.toml)
PUT    /api/v1/agent-templates/:id → update
```

## MCP Peering Across Flamecast Sessions

This is the most exciting part. Today Flamecast sessions are isolated — each
session talks to one agent with no awareness of other sessions. With
durable-acp-rs as the conductor layer:

1. Every session gets `list_agents` and `prompt_agent` MCP tools automatically
2. Agents can discover and message each other across sessions
3. Multi-model workflows work out of the box (Claude session asks Gemini session for a review)
4. The Flamecast UI could show peer conversations in real-time

The MCP tools are injected by the `PeerMcpProxy` in the conductor's proxy
chain. Flamecast doesn't need to know about them — they appear as native
tools in the agent's session.

For in-process routing, the `AgentRouter` handles it with zero HTTP.
For cross-process (different Flamecast instances), the HTTP fallback works.

## TypeScript Client: `@durable-acp/client`

The integration point for Flamecast is the existing `DurableACPClient`
from `~/gurdasnijor/distributed-acp/packages/durable-acp-client/`. This
TypeScript client already provides exactly what Flamecast needs:

### What `DurableACPClient` does

```typescript
const client = new DurableACPClient({
  stateStreamUrl: "http://localhost:4437/streams/durable-acp-state",
  serverUrl: "http://localhost:4438",
  connectionId: "..."
});

// Reactive collections (subscribed via SSE/WS)
client.connections       // ConnectionRow[]
client.promptTurns       // PromptTurnRow[]
client.chunks            // ChunkRow[]
client.permissions       // PermissionRow[]
client.queuedTurns       // derived: state == "queued"
client.activeTurns       // derived: state == "active"
client.pendingPermissions // derived: state == "pending"

// Commands (POST to REST API)
await client.prompt("hello")
await client.cancel()
await client.pause()
await client.resume()
await client.reorder(turnIds)
await client.resolvePermission(requestId, optionId)

// Subscriptions
client.subscribePromptTurns(callback)
client.subscribeChunks(callback)
```

### How Flamecast uses it

`FlamecastStorage` becomes a thin wrapper over `DurableACPClient`:

```typescript
// Before: FlamecastStorage queries PGLite/Postgres
class PgLiteStorage implements FlamecastStorage {
  async listAllSessions() { return db.select().from(sessions); }
  async getSessionMeta(id) { return db.select().from(sessions).where(...); }
}

// After: FlamecastStorage reads from DurableACPClient
class DurableStreamStorage implements FlamecastStorage {
  private client: DurableACPClient;

  async listAllSessions() {
    return this.client.connections.map(toSessionMeta);
  }
  async getSessionMeta(id) {
    return toSessionMeta(this.client.getConnection(id));
  }
  async createSession(meta) {
    // Session created by conductor startup, not by storage
    // Just track the connection ID
  }
}
```

The event bus becomes `client.subscribePromptTurns()` +
`client.subscribeChunks()`. The React UI hooks become:

```typescript
// Before: useFlamecastSession polls WebSocket
function useSession(id) {
  const [chunks, setChunks] = useState([]);
  ws.on("event", (e) => { if (e.sessionId === id) setChunks(...) });
}

// After: useSession subscribes to DurableACPClient
function useSession(id) {
  const client = useDurableACPClient(id);
  return {
    chunks: client.chunks,
    turns: client.promptTurns,
    permissions: client.pendingPermissions,
  };
}
```

### What needs to change in the TypeScript client

The `DurableACPClient` from `distributed-acp` was built for the TypeScript
conductor. For durable-acp-rs integration:

1. **Verify schema compatibility** — the STATE-PROTOCOL events from the
   Rust conductor must match the schema the TypeScript client expects.
   Both use the same `@durable-acp/state` schema design.

2. **REST API endpoint mapping** — the client POSTs to `/api/v1/connections/{id}/prompt`.
   Verify the Rust REST API matches exactly (field names, camelCase, etc.)

3. **Publish as `@durable-acp/client`** — the client should be a standalone
   npm package that Flamecast imports. It's currently in the `distributed-acp`
   monorepo.

### What Flamecast cuts after integration

| Flamecast Component | Replaced By | Cut? |
|---|---|---|
| `FlamecastStorage` interface | `DurableACPClient` collections | Implement `DurableStreamStorage` adapter |
| `@flamecast/psql` package | Not needed — no SQL | Delete |
| Event bus (`eventBus`) | `client.subscribe*()` | Delete |
| Session metadata tables | `ConnectionRow` in stream | Delete |
| `AcpBridge` class | Still needed — connects to conductor stdio | Keep |
| React UI hooks | Rewire to `DurableACPClient` | Modify |

## Durable Streams as the Universal Integration Point

The durable stream is the key architectural primitive:

```
Flamecast React UI ──── subscribes ────► Durable Stream (SSE)
Flamecast CLI      ──── subscribes ────► Durable Stream (SSE)
Other tools        ──── subscribes ────► Durable Stream (SSE)
                                              ▲
                                              │ writes
                                              │
                                    durable-acp-rs conductor
                                    (intercepts all ACP traffic)
```

Any client that speaks HTTP can observe agent sessions. The stream is the
single source of truth. The StreamDB materializes it into queryable
collections. Multiple conductors write to the same stream.

This means Flamecast's React UI could drop its own event bus and WebSocket
protocol in favor of subscribing to the durable stream directly. The
STATE-PROTOCOL events are designed to be compatible with the TypeScript
`@durable-acp/state` package.

## Pluggable Transports — Agents Over Network Boundaries

The current model is local-only: the conductor spawns the agent as a
subprocess and connects via stdio. But the transport layer is already
abstracted — `ConductorImpl::run()` takes `ByteStreams` (any `AsyncRead +
AsyncWrite`), and `AcpAgent` is just one `ConnectTo<Client>` implementation.

### Transport abstraction

```
ConductorImpl::run(transport: ByteStreams<W, R>)
                                  ▲
                                  │
                    ┌─────────────┼─────────────┐
                    │             │             │
              stdio (local)   TCP/TLS     WebSocket
              AcpAgent        TcpStream   tokio-tungstenite
              (today)         (trivial)   (trivial)
```

All three are `AsyncRead + AsyncWrite`. The conductor doesn't care which
one it gets.

### What this enables

**1. Remote agents** — an agent runs on a different machine (GPU server,
cloud VM, E2B sandbox). The conductor connects via TCP or WebSocket instead
of stdio:

```rust
// Local (today)
let agent = AcpAgent::from_args(command);  // spawns subprocess
conductor.run(agent_byte_streams).await;

// Remote (new)
let stream = TcpStream::connect("agent-host:9000").await?;
let transport = ByteStreams::new(stream.clone(), stream);
conductor.run(transport).await;
```

The conductor's proxy chain (DurableStateProxy, PeerMcpProxy) works
identically — it doesn't know if the agent is local or remote.

**2. Remote conductors** — the dashboard connects to conductors running
on remote machines. Currently uses `tokio::io::duplex` (in-memory).
Replace with TCP:

```rust
// In-process (today)
let (client_out, conductor_in) = duplex(64 * 1024);
let transport = ByteStreams::new(client_out.compat_write(), client_in.compat());

// Remote (new)
let stream = TcpStream::connect("conductor-host:4436").await?;
let transport = ByteStreams::new(stream.clone(), stream);
```

**3. Durable streams over network** — the embedded durable streams server
already listens on HTTP. Remote clients subscribe via SSE. Remote
conductors could write to a shared durable streams server:

```
Dashboard (machine A)
  ├── Conductor-local (in-process, machine A)
  └── Conductor-remote (TCP to machine B)
         └── Agent (running on machine B)
              └── Writes to shared durable stream (machine A, HTTP)
```

### Implementation: transport registry

```rust
enum Transport {
    /// Spawn local subprocess, connect via stdio (default)
    Local { command: Vec<String> },
    /// Connect to remote agent via TCP
    Tcp { host: String, port: u16, tls: bool },
    /// Connect to remote agent via WebSocket
    WebSocket { url: String },
}
```

In `agents.toml`:

```toml
# Local agent (default — same as today)
[[agent]]
name = "agent-a"
agent = "claude-acp"

# Remote agent via TCP
[[agent]]
name = "remote-claude"
transport = "tcp"
host = "gpu-server.internal"
port = 9000

# Remote agent via WebSocket
[[agent]]
name = "cloud-agent"
transport = "ws"
url = "wss://agents.example.com/agent-b"

# Remote agent in E2B sandbox
[[agent]]
name = "sandbox-agent"
transport = "tcp"
host = "sandbox-12345.e2b.dev"
port = 9000
```

### What changes

| Component | Change Needed |
|---|---|
| `agents.toml` schema | Add `transport`, `host`, `port`, `url` fields |
| `dashboard.rs` | Match on `Transport` enum, create appropriate `ByteStreams` |
| `AgentRouter` | Add HTTP fallback for remote peers (already exists) |
| `PeerMcpProxy` | Already falls back to HTTP for cross-process — works for remote too |
| Durable streams | Already HTTP — remote agents can write to shared server |
| `ConductorImpl` | No changes — already takes generic `ByteStreams` |

### Peering across network boundaries

The `prompt_agent` MCP tool already has two paths:

1. **In-process** — `AgentRouter` channels (zero overhead)
2. **HTTP fallback** — REST API + SSE (works cross-process)

Path 2 works across machines with no changes — the peer registry just
needs the remote agent's API URL:

```json
{
  "agents": [
    { "name": "local-claude", "api_url": "http://127.0.0.1:4438" },
    { "name": "remote-gemini", "api_url": "https://gpu-server:4438" }
  ]
}
```

`prompt_agent(name="remote-gemini", text="...")` would POST to the
remote server's REST API and stream the response via SSE. The MCP tool
doesn't know or care that the agent is on a different machine.

### Flamecast runtime providers

Flamecast already has Docker and E2B runtime providers that spin up agents
in containers/sandboxes. These could be adapted to:

1. Start the container/sandbox with `durable-acp-rs` as the entrypoint
2. The conductor inside the container listens on a TCP port
3. The dashboard connects to it via TCP transport
4. All durable state flows back to the central durable streams server

This gives you sandboxed agents with full durability and peering — the
container runs the agent, the conductor proxies ACP, the durable stream
captures everything, and peer agents can message it via HTTP.
