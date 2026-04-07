# SDD: Deployment — Control Plane + Data Plane Split

## Architecture

Split the system into two planes that can deploy independently:

```
┌─────────────────────────────────────────────────────────┐
│  Control Plane (serverless — Cloudflare Workers, Vercel) │
│                                                          │
│  Auth Proxy (JWT validation)                             │
│  REST API (session CRUD, prompt, queue, permissions)     │
│  DS Server (durable streams — state stream read/write)   │
│  StreamDB (materialized state for API queries)           │
│  Webhook dispatch                                        │
│                                                          │
│  Stateless. Reads/writes to DS server.                   │
│  Scales horizontally.                                    │
└──────────────────────┬──────────────────────────────────┘
                       │ spawns / connects
┌──────────────────────▼──────────────────────────────────┐
│  Data Plane (full compute — Docker, Fly.io, bare metal)  │
│                                                          │
│  sacp-conductor per agent                                │
│    DurableStateProxy → writes to control plane DS server │
│    PeerMcpProxy → HTTP to other conductors               │
│    Agent subprocess (claude-agent-acp, gemini, etc.)     │
│                                                          │
│  Stateful. Each agent needs a long-running process.      │
│  GPU access for local models.                            │
└─────────────────────────────────────────────────────────┘
```

## Why Split?

| Concern | Control Plane | Data Plane |
|---|---|---|
| Compute model | Request/response (short-lived) | Long-running process |
| Scaling | Horizontal (stateless workers) | Vertical (per-agent) |
| Storage | DS server (append-only stream) | None (writes to control plane) |
| Auth | JWT validation at edge | Internal trust (VPC/mTLS) |
| Cost | Pay-per-request | Pay-per-agent-hour |
| Deploy target | Serverless (CF Workers, Vercel Edge) | Containers (Docker, Fly, E2B) |

## Control Plane Deployment

### Option 1: Cloudflare Workers + Durable Objects

```
┌─ Cloudflare Edge ──────────────────────────┐
│  Worker: REST API (Hono)                    │
│    → auth middleware (JWT from Clerk/Auth0) │
│    → routes: /agents, /prompt, /stream     │
│                                            │
│  Durable Object: DS Server                 │
│    → one DO per state stream               │
│    → append-only log with SSE             │
│    → StreamDB materialization              │
│                                            │
│  Worker: Webhook Dispatch                  │
│    → subscribes to DO events               │
│    → POSTs to registered URLs              │
└────────────────────────────────────────────┘
```

The DS server protocol (HTTP append/read/SSE) maps well to Durable Objects:
- Append = `POST` to DO
- Read = `GET` from DO (catches up from log)
- SSE = WebSocket upgrade on DO (Cloudflare supports this)

**Effort:** ~2-3 days. Port `api.rs` to Hono Worker. DS server becomes a DO.

### Option 2: Vercel Edge Functions + KV/Postgres

```
┌─ Vercel ───────────────────────────────────┐
│  Edge Function: REST API                   │
│    → NextAuth / Clerk for auth             │
│    → routes: /api/agents, /api/prompt      │
│                                            │
│  Serverless Function: DS Server adapter    │
│    → Vercel KV or Neon Postgres for state  │
│    → SSE via Vercel's streaming response   │
│                                            │
│  Cron: Webhook dispatch                    │
│    → polls for new events, dispatches      │
└────────────────────────────────────────────┘
```

**Effort:** ~2 days. Less natural fit than CF (no Durable Objects).

### Option 3: Self-hosted (Docker Compose)

```yaml
services:
  auth-proxy:
    image: envoyproxy/envoy
    # JWT validation, forwards X-JWT-Sub
    # Long route/idle timeouts for SSE
    ports: ["443:443"]

  control-plane:
    build: .  # durable-acp-rs binary
    command: ["durable-acp-rs", "--api-only", "--port", "4437"]
    # No agent subprocess — just DS server + REST API
    environment:
      AUTH_HEADER: X-JWT-Sub

  # Data plane agents spawned separately (see below)
```

**Effort:** ~0.5 day. Just Docker Compose + Envoy config.

## Data Plane Deployment

### Option 1: Docker containers (self-hosted / Fly.io)

```dockerfile
FROM node:20-slim
RUN npm install -g @agentclientprotocol/claude-agent-acp

# Install conductor + proxies
COPY target/release/durable-acp-rs /usr/local/bin/
COPY target/release/durable-state-proxy /usr/local/bin/
COPY target/release/peer-mcp-proxy /usr/local/bin/

ENV DS_SERVER_URL=https://control-plane.example.com
ENV ANTHROPIC_API_KEY=sk-...

CMD ["sacp-conductor", "agent", \
     "durable-state-proxy --stream-url $DS_SERVER_URL/streams/durable-acp-state", \
     "peer-mcp-proxy", \
     "claude-agent-acp"]
```

Each container = one agent. The conductor writes to the control plane's
DS server via HTTP. The agent subprocess runs inside the container.

```bash
# Fly.io
fly launch --dockerfile Dockerfile
fly scale count 5  # 5 agents

# Docker Compose
docker-compose up --scale agent=5
```

### Option 2: E2B Sandboxes

Flamecast already has `@flamecast/runtime-e2b`. The conductor runs inside
the sandbox, writes state to the control plane's DS server.

```typescript
const sandbox = await Sandbox.create({
  template: "durable-acp",
  envs: { DS_SERVER_URL: controlPlaneUrl }
});
```

### Option 3: Cloudflare Containers (upcoming)

When Cloudflare Containers GA, agents could run in containers managed by
the same Cloudflare account as the control plane Workers. Internal
networking, no auth needed between control/data planes.

## Auth Architecture

Per the [DS server auth proxy pattern](https://thesampaton.github.io/durable-streams-rust-server/architecture/auth-proxy.html):

```
Client (browser/CLI)
  → sends JWT in Authorization header
  → Auth Proxy (Envoy / Cloudflare Access / middleware)
    → validates JWT against JWKS
    → extracts sub claim → X-JWT-Sub header
    → forwards to DS server / REST API
  → DS server receives authenticated request
```

The DS server itself has **no auth**. The auth proxy handles it.

### JWT Flow

```
1. Client authenticates with IdP (Clerk, Auth0, Cognito)
2. Client receives JWT
3. Client sends requests with Authorization: Bearer <jwt>
4. Auth proxy validates signature, iss, aud, exp
5. Auth proxy extracts sub → X-JWT-Sub
6. DS server / REST API sees X-JWT-Sub as authenticated user
7. Stream access scoped by user (optional: per-user streams)
```

### Scoping

| Scope | How |
|---|---|
| Per-user streams | Stream name includes user ID: `state-{userId}` |
| Per-org streams | Stream name includes org ID: `state-{orgId}` |
| Multi-tenant | Auth proxy validates org membership in JWT claims |
| Agent auth | Conductor uses service account JWT to write to streams |

### Implementation for durable-acp-rs

```rust
// In api.rs — extract authenticated user from header
fn extract_user(req: &Request) -> Option<String> {
    req.headers()
        .get("X-JWT-Sub")
        .and_then(|v| v.to_str().ok())
        .map(String::from)
}

// Scope operations to authenticated user
async fn list_connections(
    State(app): State<Arc<AppState>>,
    user: AuthenticatedUser,  // extracted from X-JWT-Sub
) -> Json<Vec<ConnectionRow>> {
    let snapshot = app.durable_streams.stream_db.snapshot().await;
    // Filter by user's connections only
    Json(snapshot.connections.values()
        .filter(|c| c.owner == user.id)
        .cloned().collect())
}
```

**Effort:** ~1 day for auth middleware + scoping. Uses existing Envoy/proxy
patterns from the DS server docs.

## Deployment Matrix

| Deployment | Control Plane | Data Plane | Auth | Effort |
|---|---|---|---|---|
| **Local dev** | `cargo run` (embedded DS) | Subprocess per agent | None | 0 (today) |
| **Docker self-hosted** | Docker + Envoy | Docker containers | JWT via Envoy | ~1 day |
| **Cloudflare** | Workers + DO | CF Containers / Fly | CF Access | ~3 days |
| **Vercel** | Edge Functions | Fly / E2B | NextAuth | ~2 days |
| **Hybrid** | CF Workers | E2B sandboxes | JWT | ~2 days |

## Network Topology

```
Internet
  │
  ▼
Auth Proxy (Envoy / CF Access)
  │
  ├── Control Plane (serverless)
  │     DS Server (:4437)
  │     REST API (:4438)
  │
  ├── Data Plane Agent A (container)
  │     Conductor → DurableStateProxy → writes to DS Server
  │     Agent subprocess (claude-agent-acp)
  │
  ├── Data Plane Agent B (container)
  │     Conductor → DurableStateProxy → writes to DS Server
  │     Agent subprocess (gemini)
  │
  └── TypeScript Clients
        DurableACPClient → subscribes to DS Server SSE
        Flamecast React UI
```

All data plane agents write to the same DS server. The control plane
is the single source of truth. Agents discover peers via the registry
endpoint on the control plane.
