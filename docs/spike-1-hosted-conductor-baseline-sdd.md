# SDD: Spike 1 Hosted Conductor Baseline

> Status: Proposed
> Scope: validate `durable-acp-rs` as a clean hosted ACP/conductor runtime
> Related:
> - [`docs/hosted-conductor-architecture-sdd.md`](./hosted-conductor-architecture-sdd.md)
> - [`docs/conductor-redesign/architecture.md`](./conductor-redesign/architecture.md)
> - [`docs/transport-sdd.md`](./transport-sdd.md)
> - [Symposium ACP transport architecture](https://agentclientprotocol.github.io/symposium-acp/transport-architecture.html?highlight=transport#design-principles)
> - [Symposium ACP conductor design](https://agentclientprotocol.github.io/symposium-acp/conductor.html)

## Purpose

Spike 1 exists to answer one narrow question:

> Can `durable-acp-rs` stand on its own as a clean, spec-aligned hosted
> conductor runtime with durable ACP replay/state, without relying on broader
> product/runtime abstractions?

This spike should validate the core hosted-conductor role before introducing:

- Flamecast-specific workflow semantics
- runtime-specific snapshotting
- declarative topology configuration
- remote computer bridge layers

## Goals

The spike should prove that `durable-acp-rs` can:

- host a conductor-backed ACP endpoint
- expose the same logical ACP surface over more than one transport
- attach durable tracing via `trace_to(...)`
- materialize replayable state from ACP trace events
- preserve a minimal product helper surface without entangling it with ACP hosting

## Non-Goals

This spike should not attempt to solve:

- remote VM or sandbox orchestration
- full Flamecast integration
- queueing or turn admission
- terminal UX beyond existing projection behavior
- runtime-specific volume or workspace snapshotting
- rich workflow or review semantics
- a declarative topology/config model

## Architecture Under Test

The target architecture for this spike is:

```text
ACP client
  -> stdio or WebSocket transport
  -> hosted conductor
  -> proxy chain
  -> terminal agent

hosted conductor
  -> DurableStreamTracer
  -> durable stream
  -> StreamDb materialization

product HTTP API
  -> filesystem helpers
  -> registry
  -> terminal state mutation
```

Important boundaries:

- ACP transport hosting is separate from product HTTP API
- conductor/proxy-chain semantics come from the ACP/Symposium model
- durability comes from the ACP trace sink plus read-side materialization

## Deliverables

### 1. Clean runtime shape

The codebase should present `durable-acp-rs` as:

- a hosted conductor runtime
- with ACP hosting separated from product HTTP API
- with durable trace/state support wired into the conductor

Concretely:

- ACP hosting should live in [src/acp_server.rs](/Users/gnijor/gurdasnijor/durable-acp-rs/src/acp_server.rs)
- product HTTP API should live in [src/api.rs](/Users/gnijor/gurdasnijor/durable-acp-rs/src/api.rs)
- the main runtime should compose them without re-coupling the concerns

### 2. Two valid ACP hosting modes

The spike should support:

- stdio-hosted ACP
- WebSocket-hosted ACP

The logical ACP surface exposed to the client should be equivalent:

- initialize
- session/new
- session/prompt
- session/update notifications

### 3. Durable trace and materialized state

The spike should persist enough ACP trace to reconstruct:

- connections
- sessions / latest session ID
- prompt turns
- streamed chunks

Materialization should come from trace/replay, not from an app-specific active
observer proxy.

### 4. Minimal helper API

The non-ACP API surface should remain intentionally narrow:

- filesystem inspection
- registry
- terminal release / mutation if still needed
- template discovery if still needed

That surface should not be required for the ACP happy path.

## Explicit Design Decisions

### Decision 1: ACP/session durability is the baseline guarantee

This spike treats durable ACP trace + replayable materialized state as the
core durability model.

It does not require:

- persistent workers
- persistent containers
- persistent volumes

### Decision 2: Product helpers stay outside the conductor

Filesystem and registry helpers may exist, but they are not part of ACP
transport hosting and should not bleed into the conductor path.

### Decision 3: No new abstraction above the conductor

This spike should not invent a broader orchestration abstraction.

`durable-acp-rs` is validating:

- hosted conductor runtime
- durable ACP harness

not:

- full runtime platform
- infrastructure planner

## Acceptance Criteria

### A. Runtime

- `durable-acp-rs` builds cleanly
- ACP hosting and product HTTP API are separated
- the process can run in:
  - stdio mode
  - WS-hosted mode

### B. Protocol behavior

- a client can initialize against both stdio and WS transports
- a client can create a session against both transports
- a client can send a prompt and receive streamed output / completion against both transports

### C. Durable state behavior

- the durable stream receives ACP trace events
- `StreamDb` materializes the expected read model from those trace events
- prompt turns and chunks are reconstructible from replay

### D. Testability

- there are clear integration/e2e tests for stdio and WS paths
- replay/materialization behavior is covered by tests
- `cargo build` passes
- `cargo test` passes

## Proposed Test Plan

### Test 1: Stdio round trip

Start the real binary with a test agent over stdio.

Verify:

- initialize works
- session/new works
- prompt works
- response arrives

### Test 2: WebSocket round trip

Start the real binary with `/acp` enabled.

Verify:

- initialize works over WS
- session/new works
- prompt works
- response arrives

### Test 3: Transport equivalence

Run the same prompt flow over stdio and WS.

Verify:

- equivalent user-visible ACP behavior
- equivalent materialized state shape after replay

### Test 4: Trace replay

Persist a real session’s trace emitted by the real tracer.

Rebuild `StreamDb` from the saved event stream.

Verify:

- same connections
- same prompt turns
- same chunks

### Test 5: API/ACP boundary

Verify:

- ACP prompt/session flow does not depend on filesystem or registry endpoints
- product helper routes work independently of ACP transport hosting

## Proposed Implementation Tasks

### Task 1: Finalize runtime composition

Ensure [src/main.rs](/Users/gnijor/gurdasnijor/durable-acp-rs/src/main.rs) cleanly composes:

- stream server
- product API router
- ACP hosting router
- stdio conductor mode

### Task 2: Normalize transport behavior

Make the stdio-vs-WS hosting behavior explicit and documented.

This includes settling:

- whether stdio is default
- when WS-only mode applies
- what environment flags or command-line flags control that

### Task 3: Harden trace materialization

Verify [src/state.rs](/Users/gnijor/gurdasnijor/durable-acp-rs/src/state.rs) correctly handles:

- trace request events
- trace response events
- trace notifications
- replay from persisted stream data

### Task 4: Make tests authoritative

Bring the test suite to a clear, truthful green state.

Required:

- no stale fixtures
- no claims of green status while tests are broken

### Task 5: Document the runtime externally

Describe `durable-acp-rs` as:

- a hosted conductor runtime
- a durable ACP harness

not:

- a VM orchestrator
- a full product control plane

## Risks

### Risk 1: Stdio semantics remain ambiguous

If stdio activation rules are unclear, client behavior will remain surprising
for subprocess and terminal users.

### Risk 2: Trace materialization may still miss some notification types

If chunk/terminal/permission updates are incomplete, replay will appear less
trustworthy than it actually is.

### Risk 3: Legacy compatibility fields obscure the model

Projection types may still contain compatibility baggage. That is acceptable
for the spike as long as the runtime path is trace-first and the behavior is
documented honestly.

## Open Questions

### 1. Should stdio be default unless explicitly disabled?

This should be decided as part of the spike, because it affects local and
subprocess hosting behavior.

### 2. Is the current read model sufficient for replay-based recovery?

The spike should clarify what is reconstructible today from ACP replay and what
still needs richer handling.

Baseline requirement for this spike:

- connections
- latest session id
- prompt turns
- chunks

Not required for this spike:

- permissions
- terminals
- richer workflow artifacts

### 3. What is the minimum helper API worth keeping in this runtime?

This should be decided based on actual consumers, not speculation.

## Success Outcome

If the spike succeeds, the team should be able to say:

- `durable-acp-rs` is a clean hosted conductor runtime
- it exposes ACP over interchangeable transports
- it provides durable ACP/session replay and read-side state
- it does not need larger runtime/control-plane abstractions to justify itself

That gives a stable base for all later work:

- remote hosting
- Flamecast control-plane integration
- runtime-specific durability
- optional declarative topology configuration
