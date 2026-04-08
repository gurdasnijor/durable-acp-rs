# SDD: Flamecast Runtime Swap — Replace session-host with durable-acp-rs

> **Goal:** Replace Flamecast's Go `session-host` binary and PGLite/Postgres
> storage with `durable-acp-rs` as the conductor layer. The React UI stays.
> State observation moves from EventBus+WebSocket to durable stream SSE.
>
> **Scope:** Flamecast (`~/smithery/flamecast`) + durable-acp-rs + distributed-acp
> (`~/gurdasnijor/distributed-acp` — `@durable-acp/state`, `@durable-acp/client`)

## Principle

durable-acp-rs IS the session host. It already runs the conductor, persists
all ACP state to a durable stream, serves a REST API for queue/filesystem,
and accepts ACP clients via `/acp` WebSocket. Flamecast's runtime providers
just need to spawn it instead of `session-host`.

## What Gets Cut

| Flamecast Component | Package/File | Reason |
|---|---|---|
| `session-host-go` | `packages/session-host-go/` | Conductor replaces it entirely |
| `@flamecast/psql` | `packages/flamecast-psql/` | StreamDB replaces session/runtime queries |
| `EventBus` | `packages/flamecast/src/flamecast/events/bus.ts` | Durable stream SSE replaces in-memory ring buffer |
| WS multiplexer `/ws` | `packages/flamecast/src/flamecast/events/channels.ts` | SSE + ACP WS replaces channel-based WebSocket |
| `WebhookDeliveryEngine` | `packages/flamecast/src/flamecast/events/webhooks.ts` | `durable-acp-rs/src/webhook.rs` replaces it |
| Session callback protocol | `Flamecast.handleSessionEvent()` | `DurableStateProxy` intercepts ACP automatically |
| `FlamecastStorage` session methods | `packages/protocol/src/storage.ts` | `DurableACPClient` collections |

## What Stays (Rewired)

| Component | Change |
|---|---|
| Runtime providers (Docker/E2B) | Spawn command → `durable-acp-rs --port X <agent>` |
| Agent templates | Keep simple store (file/KV). Drop Postgres tables. |
| Hono API | Thin proxy: templates CRUD + forward queue/file to conductor REST |
| React UI + hooks | Rewire to `DurableACPClient` + `useLiveQuery` |

## Seam Lines

Four integration points between Flamecast (TypeScript) and durable-acp-rs (Rust):

### Seam 1: Runtime spawn

Runtime providers create a container/sandbox running `durable-acp-rs`.

```typescript
// packages/runtime-docker/src/index.ts — change the spawn command
// Before:
cmd: ["session-host", "--port", "8080", "--callback-url", callbackUrl]

// After:
cmd: ["durable-acp-rs", "--port", "4437", agentCommand...]
// No callback URL needed — state flows through the durable stream
```

The runtime returns two URLs:
```typescript
interface ConductorEndpoints {
  stateStreamUrl: string;  // http://host:4437/streams/durable-acp-state
  websocketUrl: string;    // ws://host:4438/acp
  apiUrl: string;          // http://host:4438
}
```

### Seam 2: State observation (electric-sync-sdd.md)

`@durable-acp/state` subscribes to the conductor's durable stream via SSE
and materializes into reactive TanStack DB collections.

```typescript
import { createDurableACPDB } from "@durable-acp/state";

const db = createDurableACPDB({ stateStreamUrl });
await db.preload();

db.collections.connections     // ConnectionRow[] — reactive
db.collections.promptTurns     // PromptTurnRow[] — reactive
db.collections.chunks          // ChunkRow[] — reactive
db.collections.permissions     // PermissionRow[] — reactive
```

This replaces:
- `FlamecastStorage.listAllSessions()` → `db.collections.connections`
- `FlamecastStorage.getSessionMeta(id)` → `db.collections.connections.find(...)`
- `EventBus.pushEvent()` → automatic (DurableStateProxy writes, SSE delivers)
- `EventBus` ring buffer + seq numbering → durable stream offsets (built-in)

### Seam 3: Prompt and control (ACP over WebSocket)

Prompts go through ACP, not REST. The React UI connects as an ACP client:

```typescript
// New: ACP client connection for prompt/control
// Uses @anthropic/acp-client or raw WebSocket with JSON-RPC
const ws = new WebSocket(websocketUrl); // ws://host:4438/acp
// → Initialize, NewSession, Prompt, RequestPermission responses
```

This replaces:
- `POST /agents/:id/prompts` → ACP `PromptRequest` over WS
- `POST /agents/:id/permissions/:reqId` → ACP `RequestPermissionResponse` over WS
- `POST /agents/:id/cancel` → ACP cancellation over WS
- `POST /agents/:id/terminate` → close WS connection (conductor terminates)

### Seam 4: Queue and filesystem (REST proxy)

Queue management and filesystem access stay as REST, proxied through Hono:

```typescript
// Hono route → forwards to conductor REST API
app.post("/agents/:id/queue/pause", async (c) => {
  const { apiUrl } = getEndpoints(c.req.param("id"));
  return fetch(`${apiUrl}/api/v1/connections/${id}/queue/pause`, { method: "POST" });
});
```

Endpoints proxied:
- `POST /queue/pause`, `/queue/resume`
- `DELETE /queue/:turnId`, `DELETE /queue`
- `PUT /queue` (reorder)
- `GET /files`, `GET /fs/tree`

## React Hook Migration

```typescript
// Before: useFlamecastSession — WebSocket channel subscription
function useFlamecastSession(sessionId: string, websocketUrl?: string) {
  // Connects to ws://host/ws
  // Subscribes to channel "session:{sessionId}"
  // Parses multiplexed events from EventBus
  return { events, prompt, cancel, respondToPermission, ... };
}

// After: useDurableACPSession — StreamDB + ACP WS
function useDurableACPSession(sessionId: string, endpoints: ConductorEndpoints) {
  const db = useMemo(() => createDurableACPDB({
    stateStreamUrl: endpoints.stateStreamUrl,
  }), [endpoints.stateStreamUrl]);

  const chunks = useLiveQuery(db.collections.chunks);
  const turns = useLiveQuery(db.collections.promptTurns);
  const permissions = useLiveQuery(db.collections.permissions);

  // ACP client for prompt/control
  const acpClient = useAcpClient(endpoints.websocketUrl);

  return {
    chunks,
    turns,
    permissions,
    prompt: (text) => acpClient.prompt(text),
    cancel: () => acpClient.cancel(),
    respondToPermission: (reqId, optionId) =>
      acpClient.respondToPermission(reqId, optionId),
  };
}
```

## Phases

### Phase 1: Runtime swap (~1 day)

**Acceptance criteria:**
- [ ] Docker runtime spawns `durable-acp-rs --port X <agent>` instead of `session-host`
- [ ] `SessionService.startSession()` extracts `{ stateStreamUrl, websocketUrl, apiUrl }` from conductor
- [ ] Conductor starts, agent subprocess runs, `/acp` WS accepts connections
- [ ] `curl http://host:4437/streams/durable-acp-state` returns SSE events
- [ ] Existing Flamecast tests pass with new spawn command

**Changes:**
- `packages/runtime-docker/src/index.ts` — swap spawn command
- `packages/runtime-e2b/src/index.ts` — swap spawn command
- `packages/protocol/src/runtime.ts` — add `stateStreamUrl` to `SessionRuntimeInfo`
- Dockerfile — install `durable-acp-rs` binary + agent runtimes (node, etc.)

**Does NOT change:** React UI, EventBus, FlamecastStorage, WebSocket protocol.
Phase 1 is a backend-only swap. The old event path still works via
session-host compatibility shim if needed.

### Phase 2: State observation swap (~1-2 days)

**Acceptance criteria:**
- [ ] `@durable-acp/state` and `@durable-acp/client` published to npm
- [ ] `useFlamecastSession` rewired to `useDurableACPSession` (StreamDB + ACP WS)
- [ ] React UI renders live chunks, prompt turns, permissions from durable stream SSE
- [ ] EventBus deleted — all state flows through durable stream
- [ ] WebSocket `/ws` handler deleted — SSE replaces it
- [ ] Channel routing (`events/channels.ts`) deleted

**Changes:**
- `packages/ui/src/hooks/use-flamecast-session.ts` → rewrite to `useDurableACPSession`
- `packages/ui/src/hooks/use-sessions.ts` → read from `db.collections.connections`
- `packages/flamecast/src/flamecast/events/` → delete `bus.ts`, `channels.ts`
- `packages/flamecast/src/flamecast/api.ts` → remove `/ws` upgrade handler
- `distributed-acp/packages/durable-acp-state/` → npm publish
- `distributed-acp/packages/durable-acp-client/` → npm publish

### Phase 3: Storage swap (~1 day)

**Acceptance criteria:**
- [ ] `FlamecastStorage` reduced to agent templates only (no session/runtime methods)
- [ ] `@flamecast/psql` deleted — no Postgres/PGLite dependency
- [ ] Agent templates stored in simple file/KV (or keep thin Postgres for templates only)
- [ ] Session list page, session detail page work from StreamDB collections
- [ ] `listAllSessions()` reads from `db.collections.connections`
- [ ] `getStoredSession(id)` reads from StreamDB snapshot

**Changes:**
- `packages/protocol/src/storage.ts` — remove session/runtime methods
- `packages/flamecast-psql/` → delete package
- `packages/flamecast/src/flamecast/index.ts` → remove storage session calls
- Agent templates → simple JSON file or lightweight store

### Phase 4: Cleanup (~0.5 day)

**Acceptance criteria:**
- [ ] `session-host-go` package deleted
- [ ] `WebhookDeliveryEngine` deleted (conductor's `webhook.rs` handles it)
- [ ] Hono API simplified to: agent templates CRUD + proxy to conductor REST
- [ ] Session callback protocol (`handleSessionEvent`) deleted
- [ ] Flamecast `package.json` has no `pg`, `drizzle-orm`, `pglite` deps
- [ ] All existing Flamecast guide scenarios verified working

**Changes:**
- `packages/session-host-go/` → delete
- `packages/flamecast/src/flamecast/events/webhooks.ts` → delete
- `packages/flamecast/src/flamecast/api.ts` → simplify routes
- `packages/flamecast/src/flamecast/session-service.ts` → simplify (no callback handling)

## What durable-acp-rs Gains

Nothing code-wise — durable-acp-rs is already complete for this integration.
W10 (Docker/E2B runtime providers) accelerates Phase 1 but isn't blocking:
the Dockerfile from `deployment-sdd.md` works today.

## Prompt/Control: ACP Transport for DurableACPClient

Per architecture principle: **all prompt submission goes through ACP**.
No REST bypass of the conductor's proxy chain.

`@durable-acp/client` currently sends `prompt()`, `cancel()`, and
`resolvePermission()` as REST POSTs. These need to be replaced with an
**ACP WebSocket transport** that speaks JSON-RPC over the `/acp` endpoint:

```typescript
// New: AcpTransport for @durable-acp/client
// Connects to ws://host:port+1/acp, speaks ACP JSON-RPC
class AcpTransport {
  private ws: WebSocket;
  private session: AcpSession;

  async connect(url: string): Promise<void>;       // → initialize + newSession
  async prompt(text: string): Promise<void>;        // → ACP PromptRequest
  async cancel(): Promise<void>;                    // → ACP cancellation
  async resolvePermission(reqId, optionId): Promise<void>; // → ACP response
}
```

This replaces the existing `WsTransport` (which speaks Flamecast's old
channel-based protocol, not ACP).

**What stays as REST** (queue management — no proxy chain needed):
- `client.pause()` / `client.resume()` → `POST /queue/pause`, `/queue/resume`
- `client.reorder(turnIds)` → `PUT /queue`

**What moves to ACP WS:**
- `client.prompt(text)` → ACP `PromptRequest` over `/acp` WS
- `client.cancel()` → ACP cancellation over `/acp` WS
- `client.resolvePermission()` → ACP `RequestPermissionResponse` over `/acp` WS

**State observation** stays as durable stream SSE (unchanged).
