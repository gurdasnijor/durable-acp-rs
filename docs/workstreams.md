# Workstreams & Dependencies

> Extracted from all SDDs. Each workstream is independently executable
> once its dependencies are met.

## Dependency Graph

```
                    ┌──────────────┐
                    │ W1: sacp-proxy│
                    │ migration    │
                    └──────┬───────┘
                           │
              ┌────────────┼────────────┐
              ▼            ▼            ▼
     ┌────────────┐ ┌────────────┐ ┌──────────────┐
     │W2: standalone│ │W3: dashboard│ │W4: API fix   │
     │proxy binaries│ │subprocess  │ │(prompt route)│
     └──────┬─────┘ └──────┬─────┘ └──────────────┘
            │              │
            ▼              ▼
     ┌─────────────────────────┐
     │W5: conductor config     │
     │(sacp-conductor --config)│
     └────────────┬────────────┘
                  │
    ┌─────────────┼──────────────────┐
    ▼             ▼                  ▼
┌────────┐ ┌───────────┐  ┌──────────────────┐
│W6: file│ │W7: event  │  │W9: pluggable     │
│storage │ │subscribers│  │transports        │
└────────┘ └─────┬─────┘  └────────┬─────────┘
                 │                 │
           ┌─────┼─────┐          ▼
           ▼     ▼     ▼   ┌──────────────┐
        ┌────┐┌────┐┌────┐ │W10: runtime  │
        │ WS ││SSE ││Hook││ │providers     │
        └──┬─┘└────┘└────┘ └──────────────┘
           │
           ▼
    ┌──────────────┐
    │W8: Flamecast │
    │API + TS client│
    └──────────────┘

Independent (no deps):
  W11: file system access
  W12: terminal management
```

## Workstream Definitions

### W1: sacp-proxy Migration
**Source:** `sdk-alignment.md` §1.1
**Effort:** 1-2 days
**Depends on:** nothing
**Delivers:** `conductor.rs` rewritten with `JrHandlerChain` (~400→150 lines)
**Files:** `src/conductor.rs`, `Cargo.toml` (add `sacp-proxy = "3.0.0"`)

---

### W2: Standalone Proxy Binaries
**Source:** `sdk-alignment.md` §1.2
**Effort:** 0.5 day
**Depends on:** W1
**Delivers:** `durable-state-proxy` and `peer-mcp-proxy` as CLI binaries
**Files:** `src/bin/durable-state-proxy.rs`, `src/bin/peer-mcp-proxy.rs`, `Cargo.toml`

---

### W3: Dashboard → Subprocess Model
**Source:** `sdk-alignment.md` §1.3
**Effort:** 1 day
**Depends on:** W2
**Delivers:** `dashboard.rs` rewritten as thin TUI over REST+SSE (~794→200 lines). Deletes `agent_router.rs`, `TuiState`, `LocalSet` wiring.
**Files:** `src/bin/dashboard.rs` (rewrite), delete `src/agent_router.rs`

---

### W4: API Prompt Routing Fix
**Source:** `known-limitations-sdd.md` §2
**Effort:** 0.5 day
**Depends on:** W1 (proxy runs as subprocess, prompt goes through conductor)
**Delivers:** API prompts route through conductor's `ConductorMessage` queue, not `cx.send_request_to(Agent)`
**Files:** `src/api.rs`, `src/app.rs` (remove `proxy_connection`)

---

### W5: Conductor Config Support
**Source:** `sdk-alignment.md` §1.2 (implied), cookbook pattern
**Effort:** 0.5 day
**Depends on:** W2
**Delivers:** Agents can be run with `sacp-conductor --config agent.json` using standard ecosystem tooling
**Files:** Documentation + example configs, possibly `agents.toml` → `conductor.json` converter

---

### W6: File-Backed Storage
**Source:** `known-limitations-sdd.md` §1
**Effort:** 0.5 day
**Depends on:** nothing (can run in parallel with W1-W5)
**Delivers:** `FileStorage` impl for `durable-streams-server` `Storage` trait. State survives restart.
**Files:** `src/durable_streams.rs` (add `FileStorage`)

---

### W7: Event Subscribers (trait + manager)
**Source:** `event-subscribers-sdd.md`
**Effort:** 0.5 day (trait + manager only)
**Depends on:** nothing
**Delivers:** `EventSubscriber` trait, `SubscriberManager`, `SessionEvent` type
**Files:** `src/subscriber.rs` (new)

---

### W7a: WebSocket Subscriber
**Source:** `event-subscribers-sdd.md`
**Effort:** 1.5 days
**Depends on:** W7
**Delivers:** `WsSubscriber` implementing Flamecast's channel protocol. `WS /ws` endpoint.
**Files:** `src/ws_subscriber.rs` (new), `src/api.rs` (add WS route), `Cargo.toml` (add `axum-extra`)

---

### W7b: Webhook Subscriber
**Source:** `event-subscribers-sdd.md`
**Effort:** 0.5 day
**Depends on:** W7
**Delivers:** `WebhookSubscriber` with HMAC signing. `POST /webhooks` registration.
**Files:** `src/webhook_subscriber.rs` (new), `src/api.rs`, `Cargo.toml` (add `hmac`, `sha2`)

---

### W7c: Generalized SSE
**Source:** `event-subscribers-sdd.md`
**Effort:** 0.5 day
**Depends on:** W7
**Delivers:** Session-scoped and global SSE endpoints using `SseSubscriber`
**Files:** `src/api.rs` (generalize existing SSE)

---

### W8: Flamecast API + TypeScript Client
**Source:** `flamecast-integration-sdd.md` Phases 1-4
**Effort:** 2-3 days
**Depends on:** W7a (WebSocket needed for React UI)
**Delivers:** Flamecast-compatible REST API, `@durable-acp/client` npm package, `DurableStreamStorage` adapter
**Files:** `src/api.rs` (add session CRUD, permissions, templates), TypeScript client package

---

### W9: Pluggable Transports
**Source:** `flamecast-integration-sdd.md` Transports section
**Effort:** 1 day
**Depends on:** W2 (standalone binaries exist)
**Delivers:** `Transport` enum (Local/TCP/WS), TCP `ByteStreams` for remote agents
**Files:** `agents.toml` schema extension, `src/bin/dashboard.rs` or conductor config

---

### W10: Runtime Providers
**Source:** `known-limitations-sdd.md` §6
**Effort:** 2-3 days per provider
**Depends on:** W9 (pluggable transports)
**Delivers:** `RuntimeProvider` trait + Docker and E2B implementations
**Files:** `src/runtime/mod.rs`, `src/runtime/docker.rs`, `src/runtime/e2b.rs`

---

### W11: File System Access
**Source:** `known-limitations-sdd.md` §4
**Effort:** 0.5 day
**Depends on:** nothing
**Delivers:** `GET /agents/:id/files`, `GET /agents/:id/fs/tree` endpoints
**Files:** `src/api.rs`, `src/state.rs` (store cwd)

---

### W12: Terminal Management API
**Source:** `known-limitations-sdd.md` §5
**Effort:** 1 day
**Depends on:** nothing
**Delivers:** Terminal create/input/output/kill endpoints
**Files:** `src/api.rs`

---

## Execution Tracks

These workstreams can run on three parallel tracks:

### Track A: SDK Alignment (critical path)
```
W1 → W2 → W3 + W5
          → W4
```
~3 days. Must complete before Phase 2 work is clean.

### Track B: Infrastructure (parallelizable)
```
W6 (file storage)           — independent
W7 → W7a + W7b + W7c       — subscriber system
W11 (filesystem access)     — independent
W12 (terminal API)          — independent
```
~4 days total, all can start immediately.

### Track C: Integration (depends on A + B)
```
W7a → W8 (Flamecast API + TS client)
W2  → W9 (pluggable transports)
W9  → W10 (runtime providers)
```
~4-6 days, starts after Track A delivers standalone binaries.

### Recommended Sequence for Single Developer
```
Week 1: W1 → W2 → W3 + W4 (SDK alignment — clears technical debt)
Week 2: W6 + W7 → W7a + W7b (storage + subscribers)
Week 3: W8 + W11 + W12 (Flamecast API + missing features)
Week 4: W9 → W10 (transports + runtime providers)
```
