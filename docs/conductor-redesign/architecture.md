# SDD: Conductor Architecture Synthesis

> Status: Proposed
> Purpose: consolidate the current architectural direction into one coherent
> document for planning and implementation
> Sub-documents:
> - [`implementation.md`](implementation.md) — DurableStreamTracer, stream format, StreamDb materialization, ConductorState dissolution, file changes
> - [`testing-and-hosting.md`](testing-and-hosting.md) — test matrix, ACP hosting surface separation
> - [`queue-scope.md`](queue-scope.md) — queue scope decision rationale

## Executive Summary

The repo should move toward a much thinner, more ACP-native architecture:

- keep the conductor transport-agnostic
- keep proxies generic ACP components
- remove product-specific queueing from the conductor
- remove `DurableStateProxy` as an active interceptor
- use SDK tracing/snoop for passive wire observation
- move application interpretation into the read side
- expose ACP as a hosted transport surface, not as part of the HTTP API
- optimize for the Flamecast integration contract: ACP for commands, durable
  stream for state

The main goal is to **shrink custom runtime code** while staying close to the
ACP conductor and proxy-chain architecture.

## External Alignment

This direction is aligned with:

- ACP Conductor spec
- ACP proxy-chains RFD
- Rust SDK conductor/session/tracing model

Key ideas from the proxy-chains model:

- the conductor is the message-routing authority
- proxies are reusable ACP components, not transport/process managers
- proxies should not own product-specific app state unless absolutely necessary
- transport hosting is an outer concern; the conductor should run over any
  suitable transport

## Current Problems

### 1. `ConductorState` is too broad

Today `ConductorState` mixes:

- infrastructure ownership
- connection identity
- transient queue runtime
- prompt lifecycle bookkeeping
- durable write helpers

This couples the proxy layer, API layer, and state projection.

### 2. `DurableStateProxy` is an app-specific interceptor

Today the main proxy is not just observing ACP traffic. It:

- intercepts specific ACP message types
- creates prompt-turn IDs
- tracks prompt lifecycle
- manages `session_to_prompt_turn`
- writes application-level rows directly
- owns queue drain behavior

This is the largest custom runtime surface in the repo and the least aligned
with the conductor/proxy model.

### 3. Server-side queueing is architectural drag

We have explicitly decided that queueing is out of scope for the conductor for
now. It is:

- transient
- local UX behavior
- not a protocol concern
- not worth preserving server-side under YAGNI

### 4. ACP hosting is conflated with the HTTP API

The current `/acp` WebSocket route works, but it makes hosted ACP transport look
like just another HTTP API route. That obscures the conductor's actual role as a
transport-agnostic orchestrator.

### 5. Docs and code still reflect an older, larger backend surface

There are still stale assumptions about:

- queue REST APIs
- prompt REST APIs
- server-owned prompt scheduling
- cross-process peer flows that depend on REST endpoints not clearly retained

Those assumptions increase pressure to keep unnecessary custom code alive.

## Core Decisions

## Decision 1: No server-side queueing

The conductor will not own:

- queue state
- pause/resume
- reorder
- cancel-before-send
- queue REST endpoints

If queueing is needed later, it should first live in the app / `acp::Client`
layer.

## Decision 2: No custom session proxy is needed right now

Current code review shows no real need to mutate `NewSessionRequest`.

Current `session/new` needs are observational only:

- capture `session_id`
- capture `cwd`

Those should be derived from the observed wire flow, not from a custom session
interceptor, unless future requirements appear.

## Decision 3: Replace active state interception with passive observation

The write side should become a dumb pipe:

- use SDK `trace_to()`
- use SDK snoop/trace wiring
- append `TraceEvent`s to the durable stream

The read side should become smarter:

- derive app collections from trace events
- correlate requests/responses
- materialize connection/session/prompt/permission/terminal state

## Decision 4: Treat ACP exposure as a hosting surface, not an API feature

The conductor should remain transport-agnostic:

```rust
conductor.run(transport)
```

The host application decides whether to expose ACP over:

- stdio
- WebSocket
- TCP

That logic should live outside `api.rs`.

## Target Architecture

```text
App / Dashboard / Flamecast
  → optional local prompt scheduler
  → ACP client
  → hosted ACP transport surface
      → Conductor
          → PeerMcpProxy
          → Agent

Passive observation
  → SDK SnooperComponent / TraceWriter
  → durable stream of TraceEvents
  → StreamDb read-side materialization
  → dashboard / Flamecast / webhooks / observers
```

## Responsibilities by Layer

### Host application

Owns:

- which ACP transports are exposed
- product HTTP routes
- process startup / composition

Does not own conductor semantics beyond composing them.

### Conductor

Owns:

- proxy-chain composition
- role-correct initialization
- message routing
- MCP-over-ACP bridge behavior
- passive observation hooks

Does not own:

- queue policy
- app-local scheduling
- HTTP route layout

### Proxies

Own:

- generic ACP extension behavior
- MCP tools/resources when appropriate

Do not own:

- queue state
- durable app materialization
- transport/process management

### Read side (`StreamDb`)

Owns:

- materialization of app-level collections from trace events
- request/response correlation
- projection of prompt turns, chunks, permissions, terminals, connection state

This is where application interpretation belongs.

## Recommended Code Shape

```text
src/
  main.rs or host entrypoint
  acp_server.rs          ACP hosting surface(s)
  api.rs                 product HTTP API only
  transport.rs           transport adapters only
  peer_mcp.rs            proxy-provided MCP tools
  state.rs               read-side materialization
  stream_server.rs       durable stream host
  stream_subscriber.rs   remote stream subscriber
  webhook.rs             webhook fanout from materialized state
```

### Remove or strongly deprecate

```text
src/
  conductor_state.rs
  durable_state_proxy.rs
  conductor.rs           if it remains only as a trivial wrapper
```

## ACP Hosting Surface

## Principle

ACP transport hosting is not business API logic.

It should be separated from ordinary HTTP routes.

## Recommended modules

### `acp_server.rs`

Responsibilities:

- expose ACP over WebSocket
- expose ACP over TCP if desired
- provide stdio entrypoint helpers
- adapt accepted connections to `transport.rs` adapters
- call `conductor.run(transport)`

Possible API:

```rust
spawn_ws_acp_server(...)
spawn_tcp_acp_server(...)
run_stdio_acp(...)
```

Important: this document does not yet resolve the hosting model behind those
entrypoints. That still needs an explicit decision:

- one conductor per ACP connection, or
- one longer-lived hosted conductor/session bridge

Any implementation sketch should call this out directly instead of hiding it
behind an unexplained factory abstraction.

### `api.rs`

Responsibilities:

- ordinary HTTP product endpoints only
- no `/acp`

Examples if retained:

- registry
- agent templates
- filesystem access

Important: the exact `ApiState` shape remains dependent on whichever non-ACP
product endpoints survive the redesign. If filesystem resolution continues to
require a stable host-side logical connection identifier, `ApiState` will need
to carry more than just `stream_server`.

### `transport.rs`

Responsibilities:

- pure adapters only
- no app policy

Examples:

- `AxumWsTransport`
- `WebSocketTransport`
- `TcpTransport`

## Why this is the right shape

- the conductor stays reusable and transport-agnostic
- tests can compare transports cleanly
- Flamecast can think in terms of “ACP endpoint” instead of “special API route”
- the public API surface becomes much clearer

## Read-Side Materialization

## Stream format

Move from custom `StateEnvelope` events to SDK-native `TraceEvent`s.

Write side:

- passive
- append-only
- generic

Read side:

- derive collections needed by the app

## Collections to derive

- connections
- prompt turns
- chunks
- permissions
- terminals

Potentially also:

- runtime instances or host metadata, if still needed

## Important caveat: prompt-turn correlation

This is the hardest part of the redesign.

The read side must define how to correlate:

- `session/prompt` request
- subsequent session updates
- terminal prompt response

The design must make one of these explicit:

1. prompts are serialized per session, or
2. concurrent prompts are supported and request correlation is robust

This should be tested directly.

## Session Handling

No custom `SessionProxy` is required now.

Current needs around `session/new` are observational:

- `session_id`
- `cwd`

Those can be derived from trace events.

A custom session proxy should only be introduced later if there is a concrete
need to:

- inject per-session MCP servers dynamically
- validate or rewrite `NewSessionRequest`
- trigger non-derivable side effects on session start

## PeerMcpProxy

Conceptually, `PeerMcpProxy` still fits the proxy-chain model well:

- it is a generic ACP proxy
- it provides MCP tools
- it extends the agent through the proxy layer

However, its current cross-process integration needs revalidation against the
reduced server surface. It should not be assumed “unchanged” without checking
whether its peer communication contract still matches the actual server API.

## Flamecast Integration Contract

The target contract for Flamecast should be:

### Commands/control

ACP client over hosted ACP transport:

- initialize
- new session
- prompt
- cancel
- permissions

### State observation

Durable stream subscription:

- connection state
- prompt turns
- chunks
- permissions
- terminals

Flamecast should not require a large custom REST control plane to interact with
the conductor.

## Testing Strategy

The test suite should protect **architectural invariants**.

The detailed test matrix and MVP test set live in:

- [`testing-and-hosting.md`](testing-and-hosting.md)

At a high level, the architecture must be protected by tests covering:

- conductor routing invariants
- initialization role invariants
- passive observation and replay
- read-side materialization
- prompt-turn correlation
- MCP proxy behavior
- transport equivalence
- Flamecast-facing end-to-end contracts

## Migration Plan

### Phase 1: Align architecture docs

- update docs to remove server-side queue assumptions
- clarify ACP hosting as a separate surface from HTTP API
- clarify no custom session proxy is currently needed

### Phase 2: Shrink runtime responsibilities

- stop treating queueing as a conductor concern
- remove queue-specific runtime state
- narrow `api.rs` to actual product endpoints

### Phase 3: Replace active observation

- introduce durable trace writer
- move materialization logic into `StreamDb`
- delete `DurableStateProxy`

### Phase 4: Split hosting surfaces

- move ACP WebSocket hosting out of `api.rs`
- add `acp_server.rs`
- keep `transport.rs` as pure adapters

### Phase 5: Lock architecture with tests

- add transport equivalence tests
- add replay/materialization tests
- add Flamecast contract e2e

## Non-Goals

- no server-side prompt queue
- no queue REST API
- no queue durability
- no custom session interception without concrete need
- no extra public server surface unless demanded by Flamecast/product requirements

## Open Questions

1. Should connection identity be host-assigned before any ACP traffic, or fully
   derived from observed protocol events?

2. What exact contract do we want for concurrent prompts within one session?

3. Does `PeerMcpProxy` need a different cross-process transport strategy if the
   larger REST surface is removed?

4. Do we need a compatibility path for old `StateEnvelope` consumers during the
   transition to `TraceEvent` streams?

## Summary

The smallest, cleanest architecture is:

- conductor is transport-agnostic
- proxies are generic ACP extensions
- app-specific state is derived on the read side
- queueing is a client concern unless proven otherwise
- ACP hosting is separated from the HTTP API
- Flamecast integrates through two primitives:
  - ACP for commands
  - durable stream for state

This is the direction that minimizes custom code while staying closest to the
ACP conductor and proxy-chain architecture.
