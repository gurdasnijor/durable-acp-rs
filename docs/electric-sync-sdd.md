# SDD: Client Integration — Native StreamDB

> Replaces W6/W7 with the native durable streams client stack that
> already exists in `distributed-acp`. (Not Electric SQL — uses DS
> server SSE + TanStack DB directly.)

## The Realization

The integration layer is **already built**. The `@durable-acp/state` and
`@durable-acp/client` packages in `~/gurdasnijor/distributed-acp/` provide
exactly what we need:

```
durable-acp-rs (Rust)                @durable-acp/state (TypeScript)
─────────────────────                ────────────────────────────────
DurableStateProxy                    createDurableACPDB({ stateStreamUrl })
  writes STATE-PROTOCOL events         subscribes to same stream via SSE
  to DS server                         materializes into TanStack DB collections
                                       reactive queries via useLiveQuery

Same stream. Same protocol. Same schema.
```

## What Already Exists

### `@durable-acp/state` (`distributed-acp/packages/durable-acp-state/`)

Schema + StreamDB factory:

```typescript
// Schema maps STATE-PROTOCOL entity types to Zod validators
const durableACPState = createStateSchema({
  connections:     { type: "connection",      primaryKey: "logicalConnectionId", schema: z.object({...}) },
  promptTurns:     { type: "prompt_turn",     primaryKey: "promptTurnId",        schema: z.object({...}) },
  chunks:          { type: "chunk",           primaryKey: "chunkId",             schema: z.object({...}) },
  permissions:     { type: "permission",      primaryKey: "requestId",           schema: z.object({...}) },
  terminals:       { type: "terminal",        primaryKey: "terminalId",          schema: z.object({...}) },
  pendingRequests: { type: "pending_request", primaryKey: "requestId",           schema: z.object({...}) },
  runtimeInstances:{ type: "runtime_instance",primaryKey: "instanceId",          schema: z.object({...}) },
});

// StreamDB factory — connects to DS stream, materializes state
const db = createDurableACPDB({
  stateStreamUrl: "http://localhost:4437/streams/durable-acp-state"
});
await db.preload();

// Typed reactive collections (TanStack DB)
db.collections.connections     // Collection<ConnectionRow>
db.collections.promptTurns     // Collection<PromptTurnRow>
db.collections.chunks          // Collection<ChunkRow>
db.collections.permissions     // Collection<PermissionRow>
```

Derived collections:
- `createQueuedTurnsCollection({ promptTurns })` — state === "queued"
- `createActiveTurnsCollection({ promptTurns })` — state === "active"
- `createPendingPermissionsCollection({ permissions })` — state === "pending"

### `@durable-acp/client` (`distributed-acp/packages/durable-acp-client/`)

High-level client wrapping StreamDB:

```typescript
const client = new DurableACPClient({
  stateStreamUrl: "http://localhost:4437/streams/durable-acp-state",
  serverUrl: "http://localhost:4438",
  connectionId: "..."
});

// Reactive collections (auto-sync from stream)
client.connections
client.promptTurns
client.chunks
client.queuedTurns        // derived
client.activeTurns        // derived
client.pendingPermissions // derived

// Commands (POST to REST API)
await client.prompt("hello")
await client.cancel()
await client.pause()
await client.resume()
await client.resolvePermission(requestId, optionId)

// Subscriptions
client.subscribePromptTurns(callback)
client.subscribeChunks(callback)
```

## What This Means

### No custom subscriber system needed

The StreamDB client subscribes to the DS stream via SSE, parses
STATE-PROTOCOL events, and materializes them into reactive TanStack DB
collections. This IS the subscriber — `StreamDb::subscribe_changes()`
on the TypeScript side. We don't need to build W7 (EventSubscriber trait),
W7a (WebSocket subscriber), or W7c (generalized SSE) for the TypeScript
read path.

### No file-backed storage needed for the client path

StreamDB materializes state in-memory on the client side from the stream.
The DS server is the source of truth. File-backed storage (W6) is still
useful for the Rust side (survive restarts), but the TypeScript client
doesn't need Postgres or files — it replays from the stream on connect.

### Flamecast integration is straightforward

```typescript
// Before: Flamecast uses FlamecastStorage (PGLite/Postgres)
class PgLiteStorage implements FlamecastStorage {
  async listAllSessions() { return db.select().from(sessions); }
}

// After: Flamecast uses DurableACPClient
class DurableACPStorage implements FlamecastStorage {
  constructor(private client: DurableACPClient) {}
  async listAllSessions() {
    return this.client.connections.map(toSessionMeta);
  }
}
```

The event bus becomes `client.subscribePromptTurns()`.
React hooks become `useLiveQuery` from TanStack DB.

## What We Still Need

| Need | Solution | Effort |
|---|---|---|
| Verify schema compat | Compare Rust `state.rs` types with TS `schema.ts` Zod schemas | 0.5 day |
| Publish `@durable-acp/state` | npm publish from `distributed-acp` monorepo | config only |
| Publish `@durable-acp/client` | npm publish from `distributed-acp` monorepo | config only |
| Point client at Rust DS server | Change `stateStreamUrl` to conductor's port | 1 line |
| Webhooks | DS server SSE → tiny webhook forwarder (or Postgres if analytics needed) | 0.5 day |
| Rust-side file storage (W6) | `FileStorage` impl for restart survival | 0.5 day |

### Schema compatibility check

The Rust `state.rs` types must produce JSON that matches the Zod schemas
in `@durable-acp/state/schema.ts`. Key fields to verify:

| Rust (`state.rs`) | TypeScript (`schema.ts`) | Match? |
|---|---|---|
| `ConnectionRow.logical_connection_id` | `logicalConnectionId` | camelCase ✅ (serde rename) |
| `PromptTurnRow.prompt_turn_id` | `promptTurnId` | camelCase ✅ |
| `ChunkRow.chunk_type` → `"type"` | `type` | ✅ (serde rename) |
| `ConnectionState::Created` | `"created"` | ✅ (snake_case serde) |

The Rust types use `#[serde(rename_all = "camelCase")]` which matches
the TypeScript Zod schemas. Should be compatible out of the box.

## Architecture (updated)

```
durable-acp-rs (Rust)                    TypeScript Clients
───────────────────                     ──────────────────
DurableStateProxy                        @durable-acp/state
  → writes STATE-PROTOCOL                  → createDurableACPDB(stateStreamUrl)
  → to DS Server (:4437)                   → subscribes via SSE
                                           → materializes into TanStack DB
REST API (:4438)                           → reactive collections
  → prompt, cancel, pause               
  → queue, permissions                   @durable-acp/client
                                           → wraps StreamDB + REST commands
            │                              → subscriptions, derived queries
            │                              
            ▼                            Flamecast React UI
     DS Stream (SSE)  ◄─────────────────── useLiveQuery hooks
     (source of truth)                     DurableACPClient

                                         Webhooks (optional)
     DS Stream (SSE)  ──► tiny forwarder ──► HTTP POST to URLs
```

## What This Replaces from Workstreams

| Workstream | Before | After |
|---|---|---|
| W6: File storage | Custom `FileStorage` Rust impl | Still needed for Rust-side restart survival |
| W7: EventSubscriber trait | Custom Rust trait + manager | Not needed — StreamDB IS the subscriber |
| W7a: WebSocket subscriber | Custom `WsSubscriber` | Not needed — StreamDB uses SSE directly |
| W7b: Webhooks | Custom `WebhookSubscriber` | Tiny SSE→HTTP forwarder (~50 lines) |
| W7c: Generalized SSE | Custom axum SSE | Not needed — DS server SSE already works |
| W8: Flamecast TS client | Build from scratch | Already exists — `@durable-acp/client` |

**Net effect:** W7/W7a/W7c/W8 are eliminated. W6 stays (Rust-side).
W7b becomes a tiny script. The TypeScript integration layer is done.
