# Importing durable-acp-rs into Flamecast

> How to make this repo consumable by a TypeScript monorepo.

## Integration Model: Sidecar Process

durable-acp-rs is a **long-running conductor server**. Flamecast spawns it
as a subprocess and communicates over well-defined protocols:

```
Flamecast (TypeScript)                     durable-acp-rs (Rust binary)
──────────────────────                     ──────────────────────────────
Runtime provider spawns ───────────────►   durable-acp-rs --port 4437 npx claude-agent-acp
                                           │
@durable-acp/transport ──WS /acp──────►    │ ACP protocol (prompts, cancel, permissions)
                                           │
durable-session ─────────SSE──────────►    │ /streams/durable-acp-state (state observation)
                                           │
fetch() ─────────────────REST─────────►    │ /api/v1/*/queue/* (queue management)
```

No WASM, no napi-rs, no FFI. Three protocols, all HTTP-based.

## What Already Exists

### TypeScript packages (in `ts/packages/`)

| Package | What it does | Status |
|---|---|---|
| `@durable-acp/transport` | WebSocket ACP client adapter. `connectWs(url)` → ready-to-prompt `AcpConnection`. | ✅ Built |
| `durable-session` | StreamDB factory for durable-acp state. SSE → reactive TanStack DB collections. | ✅ Built |

### Rust binary

The `durable-acp-rs` binary starts a conductor process that exposes:
- **stdio** — ACP protocol (for editor integrations)
- **WebSocket at `:port+1/acp`** — ACP protocol (for Flamecast)
- **HTTP at `:port/streams/*`** — durable streams SSE (for state observation)
- **HTTP at `:port+1/api/v1/*`** — queue management + filesystem

## What Flamecast Needs to Do

### Step 1: Add the Rust binary to the monorepo

Create a workspace package that wraps the Rust binary:

```
packages/
  durable-acp-server/
    package.json          ← workspace package
    build.sh              ← cargo build --release
    src/
      index.ts            ← spawn helper + typed client
      spawn.ts            ← spawn binary, return { ports, pid, kill() }
```

```json
// packages/durable-acp-server/package.json
{
  "name": "@flamecast/durable-acp-server",
  "scripts": {
    "build": "./build.sh"
  },
  "dependencies": {
    "@durable-acp/transport": "workspace:*",
    "durable-session": "workspace:*"
  }
}
```

```typescript
// packages/durable-acp-server/src/spawn.ts
import { spawn } from "child_process";
import path from "path";

export interface DurableAcpServer {
  pid: number;
  streamsPort: number;   // :port  — durable streams SSE
  apiPort: number;        // :port+1 — REST API + /acp WebSocket
  streamsUrl: string;     // http://127.0.0.1:{port}/streams/durable-acp-state
  acpUrl: string;         // ws://127.0.0.1:{port+1}/acp
  apiUrl: string;         // http://127.0.0.1:{port+1}
  kill(): void;
}

export function spawnDurableAcp(opts: {
  port?: number;
  agentCommand: string[];
  name?: string;
}): DurableAcpServer {
  const port = opts.port ?? 4437;
  const binary = path.resolve(__dirname, "../../target/release/durable-acp-rs");

  const child = spawn(binary, [
    "--port", String(port),
    "--name", opts.name ?? "default",
    ...opts.agentCommand,
  ], {
    stdio: ["pipe", "pipe", "inherit"],
  });

  return {
    pid: child.pid!,
    streamsPort: port,
    apiPort: port + 1,
    streamsUrl: `http://127.0.0.1:${port}/streams/durable-acp-state`,
    acpUrl: `ws://127.0.0.1:${port + 1}/acp`,
    apiUrl: `http://127.0.0.1:${port + 1}`,
    kill: () => child.kill(),
  };
}
```

### Step 2: Connect from Flamecast's runtime provider

```typescript
// In Flamecast's SessionService or RuntimeProvider:
import { spawnDurableAcp } from "@flamecast/durable-acp-server";
import { connectWs } from "@durable-acp/transport";
import { createDurableACPDB } from "durable-session";

// Spawn conductor
const server = spawnDurableAcp({
  port: 4437,
  agentCommand: ["npx", "@agentclientprotocol/claude-agent-acp"],
  name: "session-1",
});

// Connect ACP client (for prompts/commands)
const acp = await connectWs(server.acpUrl, {
  onSessionUpdate: (n) => emitToUI(n),
});

// Subscribe to state (for reactive UI)
const db = createDurableACPDB({
  stateStreamUrl: server.streamsUrl,
});
await db.preload();

// Now:
// - Send prompts:     acp.prompt("hello")
// - Read state:       db.collections.connections, db.collections.chunks, etc.
// - Manage queue:     fetch(`${server.apiUrl}/api/v1/connections/{id}/queue/pause`, { method: "POST" })
```

### Step 3: Replace FlamecastStorage

```typescript
// Before (PGLite/Postgres):
class PgLiteStorage implements FlamecastStorage {
  async listSessions() { return db.select().from(sessions); }
}

// After (durable stream):
class DurableStreamStorage implements FlamecastStorage {
  constructor(private db: DurableACPDB) {}
  async listSessions() {
    return [...this.db.collections.connections.values()]
      .map(toSessionMeta);
  }
}
```

### Step 4: Wire React hooks

```typescript
// Before: custom event bus + PGLite queries
const sessions = useQuery(pgLite, "SELECT * FROM sessions");

// After: TanStack DB live queries from durable stream
const sessions = useLiveQuery(
  db.collections.connections,
  (q) => q.where("state", "!=", "closed")
);
```

## Turbo.json Integration

```json
{
  "tasks": {
    "@flamecast/durable-acp-server#build": {
      "outputs": ["target/release/durable-acp-rs"],
      "cache": true
    },
    "@flamecast/web#dev": {
      "dependsOn": ["@flamecast/durable-acp-server#build"]
    }
  }
}
```

## What Flamecast Cuts

| Current Flamecast Component | Replacement |
|---|---|
| `FlamecastStorage` (PGLite/Postgres) | `DurableStreamStorage` wrapping durable-session |
| `@flamecast/psql` package | Delete |
| Custom event bus | `db.collections.*.subscribeChanges()` |
| Session metadata tables | `ConnectionRow` + `PromptTurnRow` in stream |
| AcpBridge + runtime-bridge | `@durable-acp/transport` `connectWs()` |
| Permission brokering logic | `DurableStateProxy` handles it |
| WebSocket server | `/acp` endpoint on conductor |

## What Flamecast Gains

- **MCP peering** — agents discover and message each other automatically
- **Durable sessions** — replay from any stream offset
- **Zero database** — FileStorage replaces Postgres
- **Stream-based observability** — any HTTP client subscribes via SSE
- **Webhook integration** — coalesced events with HMAC signing

## Pre-built Binary Distribution (future)

For CI and production, avoid building Rust in the Flamecast repo:

```json
// package.json postinstall script
"postinstall": "node scripts/download-durable-acp.js"
```

Download pre-built binaries from GitHub releases per platform:
- `durable-acp-rs-x86_64-apple-darwin`
- `durable-acp-rs-aarch64-apple-darwin`
- `durable-acp-rs-x86_64-unknown-linux-gnu`

Similar pattern to `esbuild`, `turbo`, `biome` — platform-specific npm packages
with `optionalDependencies`.
