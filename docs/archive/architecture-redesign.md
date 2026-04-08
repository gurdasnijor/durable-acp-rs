# Architecture Redesign — Module Clarity

> Status: Proposal. No code changes yet.
> Goal: Make the codebase legible to someone reading it for the first time.

## The Problem

Module names don't communicate what they do:

| Current name | What you'd guess | What it actually does |
|---|---|---|
| `app.rs` | Application entry point? | Conductor process shared state: queue runtime + state write helpers |
| `durable_streams.rs` | Durable streams client? | Embedded durable streams **server** + disk storage |
| `durable_session.rs` | Session management? | SSE **subscriber** to a remote durable stream |
| `state.rs` | Some state? | StreamDb materialization engine + all 7 entity row types |
| `conductor.rs` | The conductor? | 27-line glue function (`build_conductor`) |
| `durable_state_proxy.rs` | Proxy for durable state? | Actually correct — but 287 lines of ACP proxy handlers mixed with queue drain logic |

## Current Dependency Graph

```
state.rs                    ◄── Pure data types + StreamDb engine (356 lines)
  │                             No dependencies. The foundation.
  │
  ├── durable_streams.rs    ◄── Embedded DS server + FileStorage (153 lines)
  │     │                       Owns StreamDb. Writes to disk + applies events.
  │     │
  │     └── app.rs          ◄── Queue runtime + state write helpers (318 lines)
  │           │                  Wraps EmbeddedDurableStreams. THE HUB — 6 consumers.
  │           │
  │           ├── durable_state_proxy.rs  ◄── ACP proxy (287 lines)
  │           │     │                         Intercepts ACP messages, writes via AppState.
  │           │     │                         Also contains drive_queue() — queue drain logic.
  │           │     │
  │           │     └── conductor.rs      ◄── Wires proxy chain (27 lines)
  │           │
  │           ├── api.rs                  ◄── REST API + /acp WebSocket (250 lines)
  │           │                               Queue mgmt, filesystem, terminal kill.
  │           │
  │           └── main.rs                 ◄── CLI entry point (81 lines)
  │
  ├── durable_session.rs    ◄── SSE subscriber — remote reader (321 lines)
  │                              Feeds remote stream into its own StreamDb.
  │
  └── webhook.rs            ◄── Background forwarder (336 lines)
                                  Subscribes to StreamDb changes, POSTs webhooks.

Independent leaves (no internal deps):
  client.rs        ◄── Reusable ACP client (handler trait + prompt channel)
  transport.rs     ◄── WebSocket/TCP/AxumWs transport impls
  registry.rs      ◄── Peer agent registry (file-based)
  acp_registry.rs  ◄── CDN agent registry client
  peer_mcp.rs      ◄── MCP tools proxy (list_agents, prompt_agent)
```

## What's Wrong

### 1. `app.rs` is a grab bag

`AppState` has two jobs that shouldn't be one struct:

**Job A: Queue runtime** (conductor-specific)
- `enqueue_prompt()`, `take_next_prompt()`
- `cancel_queued_turn()`, `cancel_all_queued()`, `reorder_queue()`
- `set_paused()`
- `runtime: Arc<Mutex<RuntimeState>>` (paused flag, active turn, VecDeque)

**Job B: State I/O** (generic durable stream operations)
- `write_state_event()` — serialize STATE-PROTOCOL envelope, append to stream
- `record_chunk()` — write chunk with sequence numbering
- `finish_prompt_turn()` — update turn state in StreamDb + stream
- `update_connection()` — read-modify-write connection row

These are coupled because `drive_queue()` (in `durable_state_proxy.rs`) needs
both — it takes from the queue AND writes state events. But conceptually
they're different: one is "conductor runtime" and the other is "stream writer."

### 2. `durable_streams.rs` name suggests a client, not a server

It runs `axum::serve()` and binds to a port — it's a server. The name
`durable_streams` could mean anything. `EmbeddedDurableStreams` is better
but still vague. Compare:
- `embedded_streams.rs` / `StreamServer` — immediately obvious
- `durable_streams.rs` / `EmbeddedDurableStreams` — requires reading the code

### 3. `durable_session.rs` name suggests session management

"Durable session" in the context of ACP could mean persisted ACP sessions.
It's actually an SSE stream subscriber. Compare:
- `stream_subscriber.rs` / `StreamSubscriber` — obvious
- `durable_session.rs` / `DurableSession` — misleading

### 4. `conductor.rs` is 27 lines of glue

It's just `build_conductor(app, agent) → ConductorImpl`. Does this need
its own file? It could be a function in `main.rs` or `durable_state_proxy.rs`.

### 5. Queue drain lives in the wrong place

`drive_queue()` is in `durable_state_proxy.rs` but it's really queue
orchestration — it takes from the queue, updates state, sends the request
to the agent, handles the response. It's called from the proxy's
`PromptRequest` handler but does more than proxying.

## Proposed Changes

### Option A: Renames only (minimal churn)

| Current | Rename to | Type rename |
|---|---|---|
| `app.rs` | `conductor_state.rs` | `AppState` → `ConductorState` |
| `durable_streams.rs` | `stream_server.rs` | `EmbeddedDurableStreams` → `StreamServer` |
| `durable_session.rs` | `stream_subscriber.rs` | `DurableSession` → `StreamSubscriber` |
| `conductor.rs` | inline into `main.rs` | `build_conductor()` → local function |

**Effort:** ~1 hour. Find-and-replace across all files + tests.
**Risk:** Low. No logic changes.

### Option B: Renames + split AppState (moderate)

Everything in Option A, plus:

Split `ConductorState` into two:
1. `QueueManager` — owns `RuntimeState`, provides queue ops
2. `StateWriter` — wraps `StreamServer`, provides `write_state_event`, `record_chunk`, etc.

`ConductorState` becomes a thin struct holding both:
```rust
pub struct ConductorState {
    pub queue: QueueManager,
    pub writer: StateWriter,
    pub session_to_prompt_turn: Arc<RwLock<HashMap<String, String>>>,
}
```

**Effort:** ~3 hours. Split struct, update all call sites.
**Risk:** Medium. Touch every file that uses AppState.
**Benefit:** Each component has a single clear purpose. Testable independently.

### Option C: Full reorganization (ambitious)

Everything in Option B, plus:

Move `drive_queue()` out of `durable_state_proxy.rs` into `QueueManager`:
```rust
impl QueueManager {
    pub fn drive(&self, cx: ConnectionTo<Conductor>, writer: &StateWriter) -> Result<()> { ... }
}
```

Merge `conductor.rs` into `durable_state_proxy.rs` (it's just composition).

Final module list:
```
src/
  state.rs               StreamDb + row types (unchanged)
  stream_server.rs       Embedded DS server + FileStorage (was durable_streams.rs)
  stream_subscriber.rs   SSE subscriber to remote stream (was durable_session.rs)
  conductor_state.rs     QueueManager + StateWriter (was app.rs, split)
  proxy.rs               DurableStateProxy + build_conductor (was durable_state_proxy.rs + conductor.rs)
  peer_mcp.rs            PeerMcpProxy (unchanged)
  api.rs                 REST API + /acp WebSocket (unchanged)
  client.rs              Reusable ACP client (unchanged)
  transport.rs           Transport impls (unchanged)
  webhook.rs             Webhook forwarder (unchanged)
  registry.rs            Peer registry (unchanged)
  acp_registry.rs        CDN registry (unchanged)
```

**Effort:** ~1 day.
**Risk:** Higher — moving `drive_queue` changes the proxy's internal flow.
**Benefit:** Every module has one job. Names match what they do.

## Runtime Flow (unchanged by any option)

```
Editor/Dashboard ──stdio──► Conductor process
                               │
                               ├── ConductorImpl.run(transport)
                               │     ├── DurableStateProxy
                               │     │     writes ──► StateWriter ──► StreamServer
                               │     │     queue  ──► QueueManager ──► drive_queue
                               │     └── PeerMcpProxy
                               │           connects to peers via /acp WS
                               │
                               ├── REST API (:port+1)
                               │     ├── /acp WebSocket → spawns conductor
                               │     ├── queue ops → QueueManager
                               │     └── filesystem → StateWriter (cwd lookup)
                               │
                               └── Webhook forwarder → StreamDb → HTTP POST

Remote observer ──SSE──► StreamServer (:port)
                               StreamSubscriber → local StreamDb
```

## Recommendation

**Start with Option A** (renames only). It costs 1 hour, fixes the naming
confusion, and doesn't risk breaking anything. If the codebase grows or
new contributors join, Option B gives better modularity.

Option C is only worth it if `drive_queue` becomes more complex (e.g.,
priority queues, rate limiting, multi-instance coordination).

## Alignment with TypeScript (`distributed-acp`)

The TS monorepo at `~/gurdasnijor/distributed-acp` has 13 packages.
Here's how they map to Rust modules:

### Direct Mappings

| TS Package | Rust Module | Notes |
|---|---|---|
| `@durable-acp/state` | `state.rs` | Schema + StreamDb. TS has 3 files (schema, types, collection); Rust merges into one. |
| `@durable-acp/conductor` | `app.rs` + `durable_state_proxy.rs` + `conductor.rs` | TS has one class (`DurableACPConductor`); Rust splits across 3 files. |
| `@durable-acp/conductor` (transports) | `transport.rs` | Both define Stdio/WS/TCP. Functionally identical. |
| `@durable-acp/conductor` (peer-mcp) | `peer_mcp.rs` | Same tools (list_agents, prompt_agent). |
| `@durable-acp/conductor` (registry) | `registry.rs` | Same local JSON registry pattern. |
| `@durable-acp/client` | `client.rs` | Reusable ACP client. TS wraps REST+SSE; Rust wraps ACP connection. |
| `@durable-acp/server` | `api.rs` + `main.rs` | HTTP routes + entry point. |
| `@durable-acp/server` (webhooks) | `webhook.rs` | Same HMAC, retries, coalesced events. |

### TS-only (no Rust equivalent yet)

| TS Package | What it does | Rust gap |
|---|---|---|
| `@durable-acp/runtime` | Runtime abstraction (Local, Docker, E2B) | W10 — only remaining workstream |
| `@durable-acp/agents` | Agent catalog/store/manifest | Rust uses `acp_registry.rs` (CDN fetch) — different approach |
| `storage-psql` | PostgreSQL backend | Rust only has FileStorage |
| `runtime-docker` | Docker runtime | Part of W10 |
| `runtime-e2b` | E2B sandbox runtime | Part of W10 |
| `protocol` | Zod-validated message types | Rust uses serde derives — no separate protocol package |

### Rust-only (no TS equivalent)

| Rust Module | What it does | Why TS doesn't have it |
|---|---|---|
| `durable_streams.rs` | Embedded DS server | TS uses external `@durable-streams/server` |
| `durable_session.rs` | SSE subscriber | TS uses `@durable-streams/client` (external) |
| `acp_registry.rs` | Remote agent CDN registry | TS resolves agents differently |
| `bin/dashboard.rs` | TUI | TS has React UI (separate package) |
| `bin/agents.rs` | Multi-agent shared streams | TS uses separate server instances |

### Naming Mismatch (same thing, different name)

| Concept | TS name | Rust name | Proposed Rust rename |
|---|---|---|---|
| "The thing that runs in the conductor process" | `DurableACPConductor` | `AppState` | `ConductorState` |
| "The embedded durable streams server" | N/A (external service) | `EmbeddedDurableStreams` | `StreamServer` |
| "The SSE state subscriber" | `DurableStreamClient` | `DurableSession` | `StreamSubscriber` |
| "The proxy that records state" | `conductor.ts` (inline) | `DurableStateProxy` | Keep (good name) |

### Structural Difference: Monorepo vs Single Crate

TS has 13 npm packages in a Turborepo workspace. Rust has one crate with
13 modules. For alignment, Rust could split into workspace crates:

```
durable-acp/                    # Rust workspace
  crates/
    durable-acp-state/          # StreamDb + types (≈ @durable-acp/state)
    durable-acp-conductor/      # Proxy + queue + state writing (≈ @durable-acp/conductor)
    durable-acp-server/         # API + webhooks (≈ @durable-acp/server)
    durable-acp-client/         # ACP client (≈ @durable-acp/client)
  src/
    main.rs                     # CLI entry point
    bin/dashboard.rs            # TUI
```

**Not recommended yet.** Single crate is simpler for a small team. Split
when there are multiple consumers of individual components.

## Decision Log

| Date | Decision | Rationale |
|---|---|---|
| — | Pending | — |
