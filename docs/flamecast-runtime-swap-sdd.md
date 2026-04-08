# SDD: Flamecast Integration — durable-acp-rs Owns Hosting, Flamecast Owns UI

> **Goal:** Flamecast becomes a pure UI/API layer. durable-acp-rs owns all
> agent hosting, lifecycle management, state persistence, and ACP transport.
> Flamecast connects to running conductors — it does not spawn or manage them.
>
> **Scope:** Flamecast (`~/smithery/flamecast`) + durable-acp-rs + distributed-acp
> (`~/gurdasnijor/distributed-acp` — `@durable-acp/state`, `@durable-acp/client`)

## Principle

durable-acp-rs owns everything below the wire: agent processes, conductor
proxy chains, durable state persistence, webhooks, queue management,
filesystem access. Flamecast receives conductor endpoints (however they
were started — Docker, k8s, E2B, manual) and connects with two primitives:

1. **ACP client** → `ws://host:port+1/acp` (prompt, cancel, permissions)
2. **Durable state** → `http://host:port/streams/durable-acp-state` (reactive collections)

Flamecast does not spawn, manage, or monitor agent processes.

## What Gets Cut from Flamecast

| Component | Package/File | Why |
|---|---|---|
| `session-host-go` | `packages/session-host-go/` | durable-acp-rs IS the session host |
| Runtime providers | `packages/runtime-docker/`, `packages/runtime-e2b/` | durable-acp-rs owns hosting |
| `SessionService` | `packages/flamecast/src/flamecast/session-service.ts` | No process management in Flamecast |
| `@flamecast/psql` | `packages/flamecast-psql/` | StreamDB replaces all session/state queries |
| `EventBus` | `packages/flamecast/src/flamecast/events/bus.ts` | Durable stream SSE replaces in-memory ring buffer |
| WS multiplexer `/ws` | `packages/flamecast/src/flamecast/events/channels.ts` | SSE + ACP WS replaces channel-based WebSocket |
| `WebhookDeliveryEngine` | `packages/flamecast/src/flamecast/events/webhooks.ts` | `durable-acp-rs/src/webhook.rs` handles it |
| Session callbacks | `Flamecast.handleSessionEvent()` | `DurableStateProxy` intercepts ACP automatically |
| `FlamecastStorage` session methods | `packages/protocol/src/storage.ts` | `DurableACPClient` collections |

## What Stays in Flamecast (Rewired)

| Component | Change |
|---|---|
| Agent templates | Keep simple store (file/KV). Drop Postgres tables. |
| Hono API | Thin layer: templates CRUD + proxy queue/file requests to conductor REST |
| React UI + hooks | Rewire to `DurableACPClient` (`@agentclientprotocol/sdk` + `@durable-acp/state`) |

## Seam Lines

Two seams — that's it. Flamecast receives `ConductorEndpoints` and connects:

```typescript
interface ConductorEndpoints {
  acpUrl: string;          // ws://host:4438/acp — ACP client connection
  stateStreamUrl: string;  // http://host:4437/streams/durable-acp-state — SSE
  apiUrl: string;          // http://host:4438 — queue mgmt + filesystem REST
}
```

### Seam 1: ACP client (`@agentclientprotocol/sdk`)

Standard ACP protocol over WebSocket. All prompt/cancel/permission operations.
Flamecast connects as a client — same as the Rust TUI dashboard does.

### Seam 2: Durable state (`@durable-acp/state`)

SSE subscription to durable stream. Materializes into reactive TanStack DB
collections. Replaces EventBus, FlamecastStorage, WebSocket multiplexer.

### Queue/filesystem: REST proxy

Queue management and filesystem access proxied through Hono to conductor REST:
- `POST /queue/pause`, `/queue/resume`
- `DELETE /queue/:turnId`, `DELETE /queue`, `PUT /queue`
- `GET /files`, `GET /fs/tree`

## React Hook Migration

```typescript
// Before: useFlamecastSession — WebSocket channel subscription + EventBus
function useFlamecastSession(sessionId: string, websocketUrl?: string) {
  // Connects to ws://host/ws, subscribes to channels, parses EventBus events
  return { events, prompt, cancel, respondToPermission, ... };
}

// After: useDurableACPSession — two primitives composed
function useDurableACPSession(endpoints: ConductorEndpoints) {
  // Primitive 1: Durable state (reactive collections from SSE)
  const db = useMemo(() => createDurableACPDB({
    stateStreamUrl: endpoints.stateStreamUrl,
  }), [endpoints.stateStreamUrl]);

  const chunks = useLiveQuery(db.collections.chunks);
  const turns = useLiveQuery(db.collections.promptTurns);
  const permissions = useLiveQuery(db.collections.permissions);

  // Primitive 2: ACP client (prompt/control over WS)
  const acpClient = useAcpClient(endpoints.acpUrl);

  return {
    chunks, turns, permissions,
    prompt: (text) => acpClient.prompt(text),
    cancel: () => acpClient.cancel(),
    respondToPermission: (reqId, optionId) =>
      acpClient.respondToPermission(reqId, optionId),
  };
}
```

## Phases

### Phase 1: Wire up the two primitives (~2 days)

**Acceptance criteria:**
- [ ] `@durable-acp/state` and `@durable-acp/client` published to npm
- [ ] `DurableACPClient` uses `@agentclientprotocol/sdk` for ACP (not REST POSTs)
- [ ] `useFlamecastSession` rewired to `useDurableACPSession` (StreamDB + ACP SDK)
- [ ] React UI renders live chunks, prompt turns, permissions from durable stream SSE
- [ ] Prompt/cancel/permissions work through ACP WS
- [ ] Verified against a locally running `durable-acp-rs` conductor

**Changes:**
- `distributed-acp/packages/durable-acp-client/` → replace REST POST commands with `@agentclientprotocol/sdk`
- `distributed-acp/packages/durable-acp-state/` → npm publish
- `distributed-acp/packages/durable-acp-client/` → npm publish
- `packages/ui/src/hooks/use-flamecast-session.ts` → rewrite to `useDurableACPSession`
- `packages/ui/src/hooks/use-sessions.ts` → read from `db.collections.connections`

### Phase 2: Delete Flamecast backend (~1-2 days)

**Acceptance criteria:**
- [ ] `session-host-go` package deleted
- [ ] `packages/runtime-docker/` and `packages/runtime-e2b/` deleted
- [ ] `SessionService` deleted
- [ ] `EventBus`, WS multiplexer `/ws`, channel routing deleted
- [ ] `WebhookDeliveryEngine` deleted
- [ ] `FlamecastStorage` reduced to agent templates only
- [ ] `@flamecast/psql` deleted — no Postgres/PGLite dependency
- [ ] Session callback protocol (`handleSessionEvent`) deleted

**Changes:**
- Delete: `packages/session-host-go/`, `packages/runtime-docker/`, `packages/runtime-e2b/`
- Delete: `packages/flamecast-psql/`
- Delete: `packages/flamecast/src/flamecast/events/` (bus, channels, webhooks)
- Delete: `packages/flamecast/src/flamecast/session-service.ts`
- Simplify: `packages/protocol/src/storage.ts` → templates only
- Simplify: `packages/flamecast/src/flamecast/api.ts` → templates CRUD + REST proxy
- Simplify: `packages/flamecast/src/flamecast/index.ts` → remove session/storage/callback handling

### Phase 3: Verify (~0.5 day)

**Acceptance criteria:**
- [ ] Flamecast `package.json` has no `pg`, `drizzle-orm`, `pglite` deps
- [ ] All Flamecast guide scenarios verified working
- [ ] Session list, session detail, prompt, cancel, permissions all work
- [ ] Queue management (pause/resume/reorder) works via REST proxy
- [ ] File browser works via REST proxy

## What durable-acp-rs Provides (already complete)

| Capability | durable-acp-rs | Flamecast consumes via |
|---|---|---|
| Agent lifecycle | Conductor + proxy chain | (doesn't — durable-acp-rs owns it) |
| ACP transport | `/acp` WebSocket | `@agentclientprotocol/sdk` (Seam 1) |
| State persistence | DurableStateProxy → durable stream | `@durable-acp/state` SSE (Seam 2) |
| Queue management | REST `/api/v1/*/queue/*` | REST proxy |
| Filesystem access | REST `/api/v1/*/files`, `/fs/tree` | REST proxy |
| Webhooks | `src/webhook.rs` (HMAC, retries) | (doesn't — durable-acp-rs owns it) |
| Peer messaging | PeerMcpProxy (MCP tools) | Automatic via proxy chain |

## TypeScript Client Architecture: Two Independent Primitives

The TypeScript integration is built from two completely decoupled concerns:

### Primitive 1: ACP Client (`@agentclientprotocol/sdk`)

Standard ACP protocol client, transport-agnostic. Knows nothing about durability.

- Connect, initialize, sessions, prompt, cancel, permissions
- Transport: WebSocket (`/acp`), stdio, TCP — any `ConnectTo` equivalent
- Same protocol the Rust dashboard uses via `Client.builder().connect_with()`
- All prompt submission goes through ACP (architecture principle — no REST bypass)

```typescript
import { Client } from "@agentclientprotocol/sdk";

const client = new Client({ transport: new WebSocketTransport("ws://host:4438/acp") });
await client.initialize();
const session = await client.newSession({ cwd: "/workspace" });
await session.prompt("hello");
await session.cancel();
```

### Primitive 2: Durable State Client (`@durable-acp/state`)

Subscribes to the durable stream via SSE. Materializes state into reactive
TanStack DB collections. Read-only — no commands, no protocol awareness.
Pattern follows [`durable-session`](https://github.com/electric-sql/transport/blob/main/packages/durable-session/src/client.ts).

- Declarative queries over materialized durable state
- Listeners on collections (chunks, turns, permissions, terminals)
- Derived collections: `queuedTurns`, `activeTurns`, `pendingPermissions`

```typescript
import { createDurableACPDB } from "@durable-acp/state";

const db = createDurableACPDB({ stateStreamUrl: "http://host:4437/streams/durable-acp-state" });
await db.preload();

db.collections.chunks          // Collection<ChunkRow> — reactive
db.collections.promptTurns     // Collection<PromptTurnRow> — reactive
db.collections.permissions     // Collection<PermissionRow> — reactive
// Derived:
queuedTurns, activeTurns, pendingPermissions
```

### These never reference each other

| Use case | Primitive 1 (ACP) | Primitive 2 (State) |
|---|---|---|
| Monitoring dashboard | — | ✅ |
| Headless script | ✅ | — |
| Flamecast React UI | ✅ | ✅ |
| Slackbot | ✅ | — (uses webhooks) |

### Rust reference: `src/client.rs`

The same decoupling exists on the Rust side. `src/client.rs` is a reusable
ACP client decoupled from any UI — transport-agnostic (`DynConnectTo`),
events via `AcpClientHandler` trait, prompts via channel. The TUI dashboard
imports it instead of inlining ACP lifecycle code. This is the Rust analog
of the TypeScript ACP client primitive.

### `DurableACPClient` — convenience composition

`DurableACPClient` (`@durable-acp/client`) is not a third primitive. It
composes one ACP client + one durable state client into a single interface
for consumers that need both (e.g., Flamecast UI):

```typescript
const client = new DurableACPClient({
  acpUrl: "ws://host:4438/acp",
  stateStreamUrl: "http://host:4437/streams/durable-acp-state",
});

// Commands (through ACP client — goes through conductor proxy chain):
await client.prompt("hello");
await client.cancel();
await client.resolvePermission(id, opt);

// Queue management (REST — no proxy chain needed):
await client.pause();
await client.resume();

// Reactive state (from durable stream SSE — materialized collections):
client.collections.chunks
client.collections.promptTurns
client.onChunkChanges((changes) => { ... });
```
