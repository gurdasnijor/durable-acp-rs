# SDD: Testing Strategy and ACP Hosting Surface

> Status: Proposed
> Parent: [`architecture.md`](architecture.md)
> Companion: [`queue-scope.md`](queue-scope.md)
> Goal: define the minimum high-value test matrix and clarify how the conductor
> should be exposed over WebSocket/stdin/TCP without conflating transport hosting
> with the HTTP API surface

## Problem

Two related issues remain unresolved:

1. We need a tighter test strategy that verifies architectural invariants, not
   just happy-path feature behavior.
2. The current `/acp` WebSocket exposure in `src/api.rs` makes ACP transport
   hosting look like part of the public HTTP API, even though transport is an
   outer hosting concern and not part of the conductor's proxy-chain model.

The conductor should remain transport-agnostic. It should accept a transport and
run the proxy chain over it. The application decides which transports to expose.

## Architectural Principle

The conductor owns:

- proxy-chain composition
- initialization semantics
- message routing
- passive observation hooks

The conductor does **not** own:

- HTTP route layout
- whether ACP is exposed over stdio, WebSocket, or TCP
- product-level REST API shape

That is the responsibility of the host application.

## Recommended Testing Strategy

The test suite should center on **invariants and contracts**, not on reproducing
implementation details.

### 1. Conductor routing invariants

Verify that:

- a client request reaches the final agent through the configured proxy chain
- agent notifications and responses come back to the client exactly once
- the conductor remains the single message-routing authority

Recommended test:

- `in_process_proxy_chain_round_trip`

Shape:

- use in-memory/duplex transport
- configure the real conductor with the real proxy chain
- use a test agent
- assert prompt round-trip and response delivery

### 2. Initialization role invariants

Verify that:

- proxy components receive proxy initialization
- the terminal agent receives standard initialization
- role assignment remains consistent if proxies are added/removed

Recommended test:

- `proxy_vs_agent_initialization_roles`

This is fundamental to alignment with the ACP proxy-chain model.

### 3. Passive observation invariants

Verify that:

- the observation layer does not mutate ACP flow
- every observed wire message is written once
- tracing can be replayed into equivalent materialized state

Recommended tests:

- `trace_replay_matches_live_state`
- `trace_layer_is_passive`

### 4. Materialization invariants

Verify that `StreamDb` derives the required app state from the observed stream:

- connection state
- `latest_session_id`
- `cwd`
- prompt turns
- chunks
- permissions
- terminals

Recommended tests:

- `materializes_connection_from_session_new`
- `materializes_prompt_turns_from_prompt_flow`
- `materializes_permissions_and_terminals`

### 5. Prompt-turn correlation invariants

This is the highest-risk part of the read-side design.

Verify that:

- a `session/prompt` request maps to the correct updates and terminal response
- sequential prompts in one session produce distinct prompt turns in order
- if concurrent prompts per session are allowed, correlation remains correct
- if concurrent prompts are not supported, they are rejected or serialized by contract

Recommended tests:

- `single_session_multiple_prompts_materialize_correctly`
- `concurrent_prompt_correlation_is_defined`

The second test should encode the intended contract explicitly.

### 6. MCP proxy invariants

Verify that:

- `PeerMcpProxy` exposes MCP tools through the conductor path
- MCP-over-ACP message flow is routed correctly through the conductor
- any cross-process peering contract actually matches the exposed server surface

Recommended tests:

- `peer_mcp_tools_visible_to_agent`
- `mcp_tool_call_round_trip`
- `cross_process_peer_prompt_contract` (only if cross-process peering remains in scope)

### 7. Transport equivalence invariants

Verify that the same ACP session behavior works across exposed transports:

- stdio/byte streams
- WebSocket
- TCP if supported/exposed

Recommended tests:

- `ws_transport_behaves_like_stdio_transport`
- `tcp_transport_behaves_like_stdio_transport` (if TCP is kept)

Assertion target:

- same logical prompt/result behavior
- same materialized state

### 8. Flamecast-facing integration contract

This is the highest-value product test.

Verify that a web client can:

- connect via ACP transport
- initialize
- create a session
- send prompts
- receive streamed updates

and that a separate state subscriber can:

- subscribe to the durable stream
- materialize the same session state independently

Recommended test:

- `flamecast_contract_acp_plus_state_stream`

This is the real “does the architecture work for Flamecast?” test.

### 9. Reconnect and replay invariants

Verify that:

- a state subscriber can disconnect and replay to the same state
- the durable stream remains authoritative for state reconstruction

Recommended tests:

- `state_subscriber_reconnect_rebuilds_state`
- `stream_rebuild_matches_live_materialization`

### 10. Negative-path invariants

Recommended tests:

- malformed ACP frame over WS
- agent exits mid-session
- unknown peer or missing session id
- corrupt or partial event during replay

These should verify failure behavior stays explicit and bounded.

## Minimal Recommended Test Set

If we want a compact suite with high architectural value, keep these first:

1. `in_process_proxy_chain_round_trip`
2. `proxy_vs_agent_initialization_roles`
3. `trace_replay_matches_live_state`
4. `single_session_multiple_prompts_materialize_correctly`
5. `ws_transport_behaves_like_stdio_transport`
6. `flamecast_contract_acp_plus_state_stream`

That is the minimum set that protects the core architecture.

## ACP Hosting Surface

## Problem with the current shape

The current `/acp` route in `src/api.rs` works, but it implies that ACP
transport hosting is part of the product HTTP API. That is conceptually muddy.

ACP over WebSocket is not a “REST feature.” It is one possible **hosted ACP
transport**.

The conductor itself should stay agnostic:

```rust
conductor.run(transport)
```

This means WebSocket should be treated as a host adapter, not as API business
logic.

## Recommended separation

Split the runtime into two surfaces:

### 1. ACP hosting surface

Examples:

- stdio
- WebSocket ACP
- TCP ACP

These are all just ways to provide a transport to the conductor.

### 2. HTTP product API surface

Examples:

- registry
- templates
- filesystem access (if retained)
- other product-specific helper endpoints

This surface should not own ACP semantics.

## Recommended module shape

```text
src/
  acp_server.rs      ACP hosting surface
  api.rs             ordinary HTTP product endpoints only
  transport.rs       transport adapters only
```

### `acp_server.rs`

Responsibilities:

- expose ACP over WebSocket, TCP, etc.
- adapt accepted connections into transport objects
- call `conductor.run(transport)`

Examples:

- `serve_ws_acp(...)`
- `serve_tcp_acp(...)`
- `run_stdio_acp(...)`

### `api.rs`

Responsibilities:

- ordinary HTTP routes only
- no `/acp`
- no transport-hosting logic

### `transport.rs`

Responsibilities:

- `AxumWsTransport`
- `WebSocketTransport`
- `TcpTransport`

Pure adapters. No app policy.

## Why this is better

### 1. Keeps the conductor model clean

The conductor runs over a transport. It does not “own WebSocket routes.”

### 2. Makes transport exposure composable

A host can expose:

- stdio only
- WebSocket only
- TCP only
- multiple surfaces at once

without changing conductor logic.

### 3. Better fit for Flamecast

Flamecast should think of ACP as:

- “connect to an ACP endpoint”

not:

- “call a special HTTP API route that happens to host a conductor”

### 4. Easier testing

Transport equivalence tests become straightforward when the transport host layer
is separated from the product API layer.

## Recommended runtime composition

```rust
// 1. Shared infrastructure
let stream_server = StreamServer::start(bind, state_stream).await?;

// 2. Product HTTP API
let api = api::router(ApiState {
    stream_server: stream_server.clone(),
    connection_id: connection_id.clone(),
});
spawn_http_api(api_addr, api).await?;

// 3. ACP hosting surface(s)
spawn_ws_acp_server(ws_addr, make_conductor_factory(...)).await?;
spawn_tcp_acp_server(tcp_addr, make_conductor_factory(...)).await?;

// 4. Stdio mode (optional binary path)
run_stdio_acp(make_conductor_factory(...)).await?;
```

The important thing is that all of these produce transports for the same
conductor composition instead of embedding conductor semantics in the API module.

## Handoff Recommendation

When refactoring, do these in order:

1. Define the intended public server surface
2. Move ACP hosting out of `api.rs`
3. Keep conductor composition transport-agnostic
4. Add transport-equivalence tests
5. Add one Flamecast-facing end-to-end contract test

## Summary

The conductor should remain a transport-agnostic proxy-chain orchestrator.

The host application should decide how ACP is exposed:

- stdio
- WebSocket
- TCP

Testing should focus on invariants:

- routing
- initialization roles
- passive observation
- materialization
- transport equivalence
- Flamecast-facing end-to-end contracts

That is the cleanest architecture for minimizing custom code while preserving
confidence in the system.
