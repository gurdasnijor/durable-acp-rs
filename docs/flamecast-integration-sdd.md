# SDD: Flamecast Integration — What to Cut, What to Share

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
