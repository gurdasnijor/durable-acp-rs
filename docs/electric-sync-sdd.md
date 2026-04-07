# SDD: Electric Sync — DS Stream → Postgres → Electric → Clients

> Replaces W6 (file storage), W7/W7a/W7b/W7c (event subscribers) with
> infrastructure that already exists in `durable-streams-server`.

## The Idea

The DS server is the source of truth. Everything else is derived:

```
DurableStateProxy                DS Server              Sync Service           Postgres              Electric SQL           Clients
─────────────────                ─────────              ────────────           ────────              ────────────           ───────
intercepts ACP ──POST──► append-only stream
                                   │
                              SSE broadcast ──────► subscribes via SSE
                                   │                     │
                              direct SSE clients    INSERT into session_events
                              (dashboard TUI,            │
                               peer agents)         Postgres stores
                                                    (durable, queryable)
                                                         │
                                                    WAL replication ──► Shape API
                                                                           │
                                                                      ShapeStream ──► DurableACPClient (TS)
                                                                                      Flamecast React hooks
                                                                                      
                                                    pg_notify trigger ──► Webhook worker ──► HTTP POST
                                                    SQL queries ──► Analytics, Grafana
```

## What It Delivers

| SDD Workstream | Custom Rust Code | With Electric Sync |
|---|---|---|
| W6: File-backed storage | `FileStorage` impl (~100 lines) | Postgres via sync service (0 Rust lines) |
| W7: EventSubscriber trait | Trait + manager (~100 lines) | Not needed — Electric IS the subscriber |
| W7a: WebSocket subscriber | `WsSubscriber` (~200 lines) | Electric Shape API (HTTP streaming) |
| W7b: Webhooks | `WebhookSubscriber` (~100 lines) | Postgres trigger + `pg_notify` + tiny worker |
| W7c: Generalized SSE | Custom axum SSE (~50 lines) | DS server SSE already works + Electric |
| **Total** | **~550 lines custom Rust** | **~200 lines sync service + webhook worker** |

Plus we get for free:
- SQL queries over all session data
- Grafana dashboards
- Postgres full-text search over chunks
- Analytics (prompt count, token usage, agent performance)
- Backup/restore via pg_dump

## Components

### 1. DS Server (already running)

No changes. The embedded `durable-streams-server` already:
- Accepts POST (append to stream)
- Serves GET (read from offset)
- Streams SSE (`?live=sse&offset=N`)
- Handles producer idempotency

### 2. Sync Service (reference impl exists)

The [durable-streams-server repo](https://thesampaton.github.io/durable-streams-rust-server/architecture/sync.html)
provides a reference sync service (`e2e/sync/sync.mjs`) that does exactly
this: subscribes to a DS stream via SSE, parses events, INSERTs into
Postgres. We deploy it, not write it.

Configuration only — point it at our DS stream URL and Postgres:

```bash
DS_STREAM_URL=http://localhost:4437/streams/durable-acp-state
DATABASE_URL=postgresql://localhost/durable_acp
```

The sync service handles SSE parsing, offset tracking, reconnection,
and Postgres insertion. Zero custom code needed.

### 3. Postgres Schema

```sql
-- All STATE-PROTOCOL events land here
CREATE TABLE session_events (
  id BIGSERIAL PRIMARY KEY,
  stream_name TEXT NOT NULL,
  event_type TEXT NOT NULL,        -- "connection", "prompt_turn", "chunk", "permission", "terminal"
  event_key TEXT NOT NULL,         -- logical_connection_id, prompt_turn_id, chunk_id, etc.
  operation TEXT NOT NULL,         -- "insert", "update", "delete"
  payload JSONB NOT NULL,          -- the full value object
  ds_offset TEXT,                  -- DS stream offset for resumption
  received_at TIMESTAMPTZ DEFAULT NOW()
);

-- Indexes for common queries
CREATE INDEX idx_session_events_type ON session_events(event_type);
CREATE INDEX idx_session_events_key ON session_events(event_key);
CREATE INDEX idx_session_events_type_key ON session_events(event_type, event_key);

-- Materialized views for fast access (optional)
CREATE VIEW connections AS
  SELECT DISTINCT ON (event_key)
    event_key as logical_connection_id,
    payload
  FROM session_events
  WHERE event_type = 'connection'
  ORDER BY event_key, id DESC;

CREATE VIEW latest_chunks AS
  SELECT event_key as chunk_id,
    payload->>'promptTurnId' as prompt_turn_id,
    payload->>'type' as chunk_type,
    payload->>'content' as content,
    (payload->>'seq')::int as seq
  FROM session_events
  WHERE event_type = 'chunk'
  ORDER BY (payload->>'seq')::int;

-- Webhook registration
CREATE TABLE webhooks (
  id SERIAL PRIMARY KEY,
  url TEXT NOT NULL,
  secret TEXT,
  event_types TEXT[] DEFAULT '{}',
  created_at TIMESTAMPTZ DEFAULT NOW()
);

-- Webhook trigger
CREATE OR REPLACE FUNCTION notify_webhook() RETURNS TRIGGER AS $$
BEGIN
  PERFORM pg_notify('webhook_events', row_to_json(NEW)::text);
  RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER session_event_webhook
  AFTER INSERT ON session_events
  FOR EACH ROW EXECUTE FUNCTION notify_webhook();
```

### 4. Electric SQL

Reads the Postgres WAL, exposes Shape API:

```bash
# Docker compose
electric:
  image: electricsql/electric
  environment:
    DATABASE_URL: postgresql://...
  ports:
    - "3000:3000"
```

TypeScript clients subscribe to shapes:

```typescript
import { ShapeStream } from "@electric-sql/client";

// All chunks for a specific prompt turn
const chunks = new ShapeStream({
  url: `${electricUrl}/v1/shape`,
  params: {
    table: "session_events",
    where: `event_type = 'chunk' AND event_key LIKE 'turn-abc%'`
  }
});

chunks.subscribe((messages) => {
  for (const msg of messages) {
    if (msg.headers.operation === "insert") {
      console.log("New chunk:", msg.value.payload.content);
    }
  }
});
```

### 5. Webhook Worker (~50 lines)

Listens to `pg_notify`, POSTs to registered URLs:

```javascript
// webhook-worker.mjs
const pg = new Client(process.env.DATABASE_URL);
await pg.connect();
await pg.query("LISTEN webhook_events");

const webhooks = await pg.query("SELECT * FROM webhooks");

pg.on("notification", async (msg) => {
  const event = JSON.parse(msg.payload);
  
  for (const webhook of webhooks.rows) {
    if (webhook.event_types.length === 0 || 
        webhook.event_types.includes(event.event_type)) {
      const body = JSON.stringify(event.payload);
      const sig = hmac(webhook.secret, body);
      await fetch(webhook.url, {
        method: "POST",
        headers: { 
          "Content-Type": "application/json",
          "X-Webhook-Signature": sig 
        },
        body
      });
    }
  }
});
```

### 6. TypeScript Client (DurableACPClient)

```typescript
import { ShapeStream } from "@electric-sql/client";

class DurableACPClient {
  private electricUrl: string;
  private dsUrl: string;

  // Reactive collections via Electric shapes
  connections = new ShapeStream({
    url: `${this.electricUrl}/v1/shape`,
    params: { table: "session_events", where: "event_type = 'connection'" }
  });

  promptTurns = new ShapeStream({
    url: `${this.electricUrl}/v1/shape`,
    params: { table: "session_events", where: "event_type = 'prompt_turn'" }
  });

  chunks = new ShapeStream({
    url: `${this.electricUrl}/v1/shape`,
    params: { table: "session_events", where: "event_type = 'chunk'" }
  });

  permissions = new ShapeStream({
    url: `${this.electricUrl}/v1/shape`,
    params: { table: "session_events", where: "event_type = 'permission'" }
  });

  // Commands go through DS server REST API
  async prompt(text: string) {
    await fetch(`${this.dsUrl}/api/v1/connections/${this.connId}/prompt`, {
      method: "POST",
      body: JSON.stringify({ sessionId: this.sessionId, text })
    });
  }

  async cancel() { /* POST to DS API */ }
  async pause() { /* POST to DS API */ }
  async resolvePermission(id: string, optionId: string) { /* POST to DS API */ }
}
```

## How It Integrates with Flamecast

```
Flamecast React UI
  → DurableACPClient (uses Electric ShapeStream)
    → Reactive collections auto-update on every agent response
    → No custom WebSocket, no event bus, no polling

Flamecast cuts:
  FlamecastStorage (PGLite)  → Postgres (via sync service)
  Event bus                  → Electric ShapeStream
  Custom WS protocol         → Electric Shape API (HTTP)
  Session metadata tables    → session_events table (one table, all state)

Flamecast keeps:
  AcpBridge (connects to conductor subprocess)
  React UI (rewire hooks to DurableACPClient)
  Runtime providers (spawn conductor with proxies)
```

## What We Still Build in Rust

The durable-acp-rs codebase still owns:
- `DurableStateProxy` — intercepts ACP, writes to DS stream
- `PeerMcpProxy` — agent-to-agent MCP tools
- `AppState` + `StreamDB` — in-memory materialization for the REST API
- REST API — prompt submission, queue management, registry
- Dashboard TUI — reads from REST API + SSE (direct from DS server)

We do NOT build:
- File-backed storage (Postgres handles durability)
- WebSocket subscriber (Electric handles reactive sync)
- Webhook subscriber (Postgres trigger handles it)
- Custom SSE generalization (DS server SSE + Electric)

## Docker Compose (dev stack)

```yaml
services:
  postgres:
    image: postgres:16
    environment:
      POSTGRES_DB: durable_acp
      POSTGRES_PASSWORD: secret
    command: ["postgres", "-c", "wal_level=logical"]
    ports: ["5432:5432"]

  electric:
    image: electricsql/electric
    environment:
      DATABASE_URL: postgresql://postgres:secret@postgres/durable_acp
    ports: ["3000:3000"]
    depends_on: [postgres]

  sync-service:
    build: ./sync-service
    environment:
      DS_STREAM_URL: http://host.docker.internal:4437/streams/durable-acp-state
      DATABASE_URL: postgresql://postgres:secret@postgres/durable_acp
    depends_on: [postgres]

  webhook-worker:
    build: ./webhook-worker
    environment:
      DATABASE_URL: postgresql://postgres:secret@postgres/durable_acp
    depends_on: [postgres]
```

## Effort

| Component | New Code | Effort |
|---|---|---|
| Postgres schema (SQL) | ~40 lines | 0.5 day |
| Sync service | 0 lines (deploy reference impl) | config only |
| Webhook worker (JS) | ~50 lines | 0.5 day |
| Docker compose | ~30 lines | included |
| DurableACPClient (TS) with Electric | ~100 lines | 1 day |
| **Total** | **~220 lines** | **~2 days** |

Compared to W6+W7 custom Rust: ~550 lines, ~4 days.

Saves ~2 days, eliminates ~550 lines of custom Rust, AND gives us
SQL queries, Grafana, full-text search, analytics, backup/restore —
none of which the custom approach provides.
