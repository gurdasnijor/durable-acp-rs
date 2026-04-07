# SDD: Authentication

> **Effort:** ~1 day
> **Pattern:** [DS server auth proxy](https://thesampaton.github.io/durable-streams-rust-server/architecture/auth-proxy.html)

## Design Principle

The DS server and REST API have **no built-in auth**. Authentication is
handled by a reverse proxy in front. This is intentional — it keeps the
core simple and lets you use whatever auth provider you want.

## Architecture

```
Client (browser / CLI / agent)
  │
  │  Authorization: Bearer <jwt>
  ▼
Auth Proxy (Envoy / Cloudflare Access / middleware)
  │  validates JWT signature against JWKS
  │  checks iss, aud, exp claims
  │  extracts sub claim
  │
  │  X-JWT-Sub: user-123
  ▼
durable-acp-rs
  │  reads X-JWT-Sub header
  │  scopes all operations to authenticated user
  ▼
DS Server (:4437)     REST API (:4438)
```

## Three Deployment Modes

### 1. Local dev — no auth (today)

No proxy. Direct access to DS server and REST API. Fine for development.

### 2. Self-hosted — Envoy proxy

```yaml
# docker-compose.yml
services:
  envoy:
    image: envoyproxy/envoy
    volumes:
      - ./envoy.yaml:/etc/envoy/envoy.yaml
      - ./jwks.json:/etc/envoy/jwks.json
    ports: ["443:443"]

  durable-acp:
    build: .
    # Not exposed externally — only Envoy routes to it
    expose: ["4437", "4438"]
```

Envoy config (key parts):

```yaml
http_filters:
  - name: envoy.filters.http.jwt_authn
    typed_config:
      providers:
        auth_provider:
          local_jwks:
            filename: /etc/envoy/jwks.json
          from_headers:
            - name: Authorization
              value_prefix: "Bearer "
          forward_payload_header: X-JWT-Sub
          issuer: "your-issuer"
          audiences: ["durable-acp"]
      rules:
        - match: { prefix: "/" }
          requires: { provider_name: auth_provider }
        - match: { prefix: "/healthz" }
          # No auth for health checks

route_config:
  virtual_hosts:
    - routes:
        - match: { prefix: "/streams" }
          route:
            cluster: ds_server
            timeout: 0s          # SSE: no route timeout
            idle_timeout: 120s   # Let DS server control lifecycle
        - match: { prefix: "/api" }
          route:
            cluster: api_server
```

### 3. Cloudflare — Access + Workers

Cloudflare Access validates JWTs at the edge. The Worker reads
`CF-Access-Authenticated-User-Email` (or custom JWT claims):

```typescript
// Cloudflare Worker middleware
async function authMiddleware(request, env) {
  const email = request.headers.get("CF-Access-Authenticated-User-Email");
  if (!email) return new Response("Unauthorized", { status: 401 });
  
  // Forward to durable-acp-rs with user identity
  const upstream = new Request(request);
  upstream.headers.set("X-JWT-Sub", email);
  return fetch(upstream);
}
```

## Rust Implementation (~50 lines)

Add auth middleware to `api.rs`:

```rust
use axum::extract::Request;
use axum::middleware::{self, Next};
use axum::response::Response;

/// Extract authenticated user from X-JWT-Sub header (set by auth proxy).
/// Returns 401 if header is missing and AUTH_REQUIRED=true.
async fn auth_middleware(
    request: Request,
    next: Next,
) -> Result<Response, axum::http::StatusCode> {
    let auth_required = std::env::var("AUTH_REQUIRED").unwrap_or_default() == "true";
    
    if auth_required {
        let user = request.headers()
            .get("X-JWT-Sub")
            .and_then(|v| v.to_str().ok());
        
        if user.is_none() {
            return Err(axum::http::StatusCode::UNAUTHORIZED);
        }
    }
    
    Ok(next.run(request).await)
}

// In router():
pub fn router(app: Arc<AppState>) -> Router {
    Router::new()
        .route("/api/v1/connections", get(list_connections))
        // ... existing routes ...
        .layer(middleware::from_fn(auth_middleware))
        .with_state(app)
}
```

## Scoping

Once auth is in place, scope data access by user:

| Scope Level | How | Stream Name Pattern |
|---|---|---|
| No scoping | All users see all agents | `durable-acp-state` (today) |
| Per-user | Each user has own agents | `state-{userId}` |
| Per-org | Org members share agents | `state-{orgId}` |
| Per-session | Fine-grained access | `state-{sessionId}` + ACL |

For the initial integration, **no scoping** (all authenticated users see
all agents) is fine. Per-user/per-org scoping is a follow-up.

## Agent Auth (data plane → control plane)

When agents run on the data plane (containers), they need to authenticate
to write to the control plane's DS server:

```toml
# agents.toml
[auth]
service_account_jwt = "eyJ..."  # long-lived service account token

# Or use environment variable
# AUTH_TOKEN=eyJ... cargo run --bin durable-acp-rs -- ...
```

The conductor's `DurableStateProxy` includes this token when writing to
the DS server:

```rust
// In durable_streams.rs, when writing to remote DS server
let mut headers = reqwest::header::HeaderMap::new();
if let Ok(token) = std::env::var("AUTH_TOKEN") {
    headers.insert("Authorization", format!("Bearer {token}").parse()?);
}
```

## What's NOT in Scope

- **User management** — use an external IdP (Clerk, Auth0, Cognito)
- **RBAC** — all authenticated users have full access initially
- **API keys** — JWT-only for now (API keys are a follow-up)
- **mTLS** — for data plane agent-to-control-plane, use JWT initially

## Files to Change

| File | Change |
|---|---|
| `src/api.rs` | Add `auth_middleware` layer (~30 lines) |
| `src/durable_streams.rs` | Add `AUTH_TOKEN` header for remote writes (~10 lines) |
| `docker-compose.yml` | Add Envoy proxy service (~20 lines) |
| `envoy.yaml` | JWT validation config (~40 lines) |
