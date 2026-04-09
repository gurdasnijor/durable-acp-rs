# SDD: Closing the Flamecast Integration Gaps

> Status: Proposed
> Scope: `durable-acp-rs` + Flamecast integration branch
> Primary repos:
> - `/Users/gnijor/gurdasnijor/durable-acp-rs`
> - `/Users/gnijor/smithery/flamecast` (`rust-conductor-integration`)
> Related references:
> - `conductor-architecture-synthesis-sdd.md`
> - `proxy-decomposition-sdd.md`
> - `testing-and-acp-hosting-sdd.md`
> - Flamecast RFCs:
>   - unified runtime
>   - webhooks
>   - multi-session websocket
>   - terminal sessions
>   - runtime lifecycle
>   - prebuilt images

## Executive Summary

The integration is already relatively strong at the **ACP control plane** and
**durable state observation** layers, but it is still incomplete at the
**runtime product model** layer.

Today, the stack is closest to supporting:

- remote ACP control of a hosted agent/conductor
- durable state streaming to a browser UI
- webhook-based stateless integrations

It is furthest from fully satisfying:

- unified runtime abstraction
- runtime lifecycle and instance management
- prebuilt image / Docker runtime model
- literal multi-session WebSocket semantics as currently written in Flamecast

The main problem is no longer "can the system speak ACP?" The main problem is
that the runtime/hosting/lifecycle model is still ambiguous.

This SDD describes how to close those gaps from the current architecture rather
than replacing everything at once.

## Current State

## What is already working or close

### In `durable-acp-rs`

- conductor-backed ACP endpoint
- proxy chain
- durable stream for state observation
- StreamDb-style read-side state projection
- webhook forwarder
- terminal-related state model
- WebSocket-based ACP transport exposure
- process-level non-stdio ACP e2e coverage

### In the Flamecast integration branch

- active attempt to use `durable-acp-rs` as the runtime/session host replacement
- documented seam around ACP + durable state
- branch-local design work on transport/hosting issues

## What is still structurally unclear

### 1. Runtime lifetime unit

We have not settled which thing is long-lived:

- one conductor per browser or ACP connection
- one conductor per session
- one conductor per agent
- one conductor per runtime instance
- one conductor per workspace

Without this decision, it is impossible to cleanly align runtime lifecycle,
remote reconnect behavior, and browser/mobile UX.

### 2. Hosted ACP surface vs product API surface

The current `/acp` exposure makes ACP hosting look like an HTTP API feature,
but architecturally it is a transport-hosting concern.

### 3. Remote workspace model

The screenshot use case ("upload my folder, run remotely, monitor from my
phone") depends less on ACP and more on:

- where the workspace lives
- how it is mounted or synced
- how runtime instances own or share that workspace

### 4. RFC mismatch around WebSocket and queueing

The older Flamecast RFCs assume:

- a proprietary WebSocket control/event plane
- server-side queue semantics
- richer REST control APIs

The current conductor direction is:

- ACP for commands/control
- durable stream / SSE for state
- no server-side queue for now

Those are not irreconcilable, but they require a deliberate reinterpretation of
the RFC goals.

## Architectural Principle

Separate the stack into three layers:

```text
1. Runtime layer
   owns workspace, environment, lifecycle, ports, deployment shape

2. ACP host / conductor layer
   owns proxy-chain composition, ACP routing, hosted ACP endpoints, durable
   observation, MCP extensions

3. Product/UI layer
   owns browser/mobile UX, local queueing if desired, templates CRUD, operator
   views, and orchestration workflows
```

The current architecture is strongest in layer 2.

Most remaining RFC gaps live in layers 1 and 3.

## Gap Analysis by RFC

## 1. Unified Runtime RFC

### Intent of the RFC

Provide one portable runtime abstraction across:

- local
- docker
- remote/sandbox

with consistent `setup` + `spawn` semantics and optional Docker/image escape
hatches.

### Current alignment

Partial.

The stack already aligns on one important point:

- ACP should be the harness boundary

That means a runtime should eventually be judged by:

- can it prepare a workspace/environment
- can it start an ACP-capable agent endpoint inside that environment
- can Flamecast talk to it through the same control/state primitives

### Main gaps

- `durable-acp-rs` is not yet the runtime abstraction; it is a conductor/host
- no settled story for runtime instance ownership
- no consistent workspace mounting/sync model
- no explicit `setup` / `spawn` / package / image lifecycle in the Rust side

### Required evolution

#### Phase U1: define the runtime boundary

The runtime should own:

- environment allocation
- workspace mount/sync
- image/container/sandbox configuration
- launching the conductor-backed ACP endpoint

The conductor should not own those concerns.

#### Phase U2: standardize the runtime contract in Flamecast

The Flamecast runtime interface should evolve toward:

- `start(instanceId, template, workspace)`
- returns a `ConductorEndpoints`-like surface:
  - ACP endpoint
  - state stream
  - product API base URL

#### Phase U3: make `durable-acp-rs` a runtime payload, not the runtime abstraction

`durable-acp-rs` should become the process launched **inside** local/docker/remote
runtimes, not the thing that tries to subsume all runtime concerns itself.

### Directional assessment

Moderate architectural gap.

## 2. Webhooks RFC

### Intent of the RFC

Allow stateless integrations to:

- trigger work
- receive event delivery by HTTP callbacks
- avoid persistent WebSocket connections

### Current alignment

Strong.

`durable-acp-rs` already has the right architectural shape here:

- durable state stream
- webhook forwarder
- signed delivery
- retries
- coalesced higher-level event types

### Main gaps

- exact event payload contract must be reconciled with Flamecast’s RFC shape
- permission-response flow needs a clean stateless API path
- docs and integration branch still assume older session control surfaces

### Required evolution

#### Phase W1: define the webhook contract of record

Pick one canonical payload shape and delivery contract.

#### Phase W2: define stateless command endpoints

For webhook-driven systems, support:

- prompt submission
- permission resolution

without requiring a persistent ACP client in the caller.

This may be a product-facing server concern even if Flamecast itself uses ACP.

### Directional assessment

Close.

## 3. Multi-Session WebSocket RFC

### Intent of the RFC

Provide:

- one shared WebSocket
- selective subscriptions
- multi-session and cross-session visibility

### Current alignment

Low if interpreted literally.

The new conductor direction is not “build a big custom multiplexed WebSocket.”
It is:

- ACP endpoint for commands/control
- durable stream/SSE for state

### Recommended reinterpretation

Treat the RFC as a **product capability RFC**, not a literal protocol mandate.

Its real goals are:

- multi-session visibility
- selective observation
- efficient browser connection model

Those goals can likely be met by:

- ACP endpoint(s) for control
- durable stream + materialized collections for observation
- optional UI-layer multiplexing if needed

### Main gaps

- current integration branch still carries legacy WebSocket assumptions
- no explicit “replacement model” has been declared for channel subscriptions
- terminal/file/queue channels are still described in old WebSocket terms

### Required evolution

#### Phase M1: formally replace the old WS model

Document that browser/mobile clients consume:

- ACP endpoint for control
- durable stream for state

#### Phase M2: re-express channel semantics as read-side selectors

Instead of WS channels like `session:{id}:terminal`, the system should offer:

- collection/selector based queries over materialized durable state

#### Phase M3: add a thin optional adapter only if product ergonomics demand it

If Flamecast still wants a single browser transport abstraction, build a thin
adapter over the new primitives rather than preserving the old proprietary
control/event plane.

### Directional assessment

Far from literal alignment, but potentially close to functional alignment.

## 4. Terminal Sessions RFC

### Intent of the RFC

Provide:

- real-time terminal output
- terminal buffering/history
- interactive input and resize
- terminal session listing and inspection

### Current alignment

Partial.

The state model already points in the right direction:

- terminals exist as app-level projected state
- some terminal lifecycle support exists

### Main gaps

- rich interactive terminal behavior is not fully defined
- transport contract for terminal input/output is not yet cleanly separated
- browser/client UX contract still reflects older WebSocket assumptions

### Required evolution

#### Phase T1: decide terminal interaction model

Split terminal support into:

- observable terminal state/history
- interactive terminal transport

Those are related but not identical.

#### Phase T2: persist what the product actually needs

Define:

- whether terminal output is durable or transient
- retention limits
- buffering contract

#### Phase T3: define frontend transport

Choose whether interactive terminal input/output is:

- part of ACP-derived product state plus a small command API, or
- a dedicated terminal transport surface

### Directional assessment

Moderate gap.

## 5. Runtime Lifecycle RFC

### Intent of the RFC

Provide:

- named runtime instances
- start/stop/pause
- UI visibility
- session-to-runtime scoping
- restart recovery

### Current alignment

Low.

This is one of the biggest open gaps.

### Main gaps

- no settled runtime-instance model
- no stable mapping between conductor lifetime and runtime instance
- no consistent pause/resume semantics across local/docker/remote
- no clear session scoping model in the new architecture

### Required evolution

#### Phase R1: choose the canonical lifecycle unit

Recommended direction:

- runtime instance is the long-lived host/container/sandbox/workspace unit
- inside it lives one or more conductor-backed agent endpoints

Do not make browser connections the lifetime unit.

#### Phase R2: define ownership

Runtime instance owns:

- workspace
- environment
- deployment addressability
- lifecycle state

Conductor owns:

- ACP routing
- proxy chain
- state observation

#### Phase R3: expose runtime inventory to Flamecast

Flamecast should track:

- runtime instances
- status
- sessions/connections scoped to an instance

### Directional assessment

Large gap.

## 6. Prebuilt Images RFC

### Intent of the RFC

Make Docker and containerized execution easy through:

- prebuilt base images
- `setup` / `spawn` portability
- managed entrypoints
- optional Dockerfile escape hatch

### Current alignment

Low at the system level.

This is mostly not a `durable-acp-rs` problem.

### Main gaps

- image inference is a Flamecast runtime concern
- managed entrypoint is a runtime/container concern
- package installation and setup orchestration are runtime concerns

### Required evolution

#### Phase P1: make `durable-acp-rs` an expected payload inside the image/runtime

The runtime/image should launch the conductor-backed ACP endpoint.

#### Phase P2: keep `setup` / `spawn` in Flamecast runtime land

Do not push image/build logic into the conductor.

#### Phase P3: standardize endpoint contract after launch

Prebuilt-image runtimes should return the same endpoint bundle as local/remote:

- ACP endpoint
- state stream
- product API base

### Directional assessment

Still far, but mostly outside the conductor itself.

## Cross-Cutting Design Changes Needed

## 1. Standardize endpoint bundle

Everything should converge on one integration surface:

```ts
interface ConductorEndpoints {
  acpUrl: string;
  stateStreamUrl: string;
  apiUrl: string;
  connectionId?: string;
  runtimeInstance?: string;
}
```

This is the contract Flamecast should consume regardless of runtime type.

## 2. Separate hosting surfaces

Move toward:

- ACP hosting surface
- product API surface
- durable stream surface

Do not treat `/acp` as ordinary REST API behavior.

## 3. Decide browser interaction model

Recommended:

- browser/mobile uses ACP only when it truly needs command/control
- browser/mobile uses durable stream for observation
- no giant custom event WebSocket unless clearly justified

## 4. Remove stale queue assumptions

Queueing should not block the architecture.

Near-term decision:

- no server-side queueing requirement
- if queueing is desired, do it first at the client/product layer

## 5. Treat remote workspace as a first-class problem

For the “upload folder and run remotely” use case, define:

- workspace sync vs mount
- who owns file lifecycle
- how filesystem APIs are scoped
- how runtime instances map to workspaces

This is essential for product readiness.

## 6. Clarify cross-process peering contract

`PeerMcpProxy` conceptually fits, but its cross-process transport contract should
match the future reduced public surface.

## Recommended Evolution Plan

## Phase 1: Stabilize the conductor architecture

In `durable-acp-rs`:

- complete the passive observation redesign
- eliminate `DurableStateProxy`
- eliminate server-side queue assumptions
- separate ACP hosting from `api.rs`
- define the durable state projection contract

Success criteria:

- one coherent conductor architecture
- one coherent state model
- tests for routing, replay, materialization, transport equivalence

## Phase 2: Define the runtime contract

Across both repos:

- agree on runtime instance lifetime
- define endpoint bundle returned by runtimes
- define workspace ownership and mounting/sync model

Success criteria:

- one template/runtime launches one stable conductor-backed endpoint bundle

## Phase 3: Replace the old browser/session transport assumptions

In Flamecast:

- stop depending on proprietary per-session WS/event assumptions as the primary model
- adopt:
  - ACP for commands/control
  - durable state for observation

Success criteria:

- browser/mobile can reconnect without killing runtime/session state

## Phase 4: Align product capabilities to RFC intent

Re-express RFCs in the new architecture:

- Webhooks via durable-acp-rs forwarder + stateless commands
- Multi-session visibility via selectors/materialized state rather than old WS channels
- Terminal UX through explicit state + interactive terminal transport contract
- Runtime lifecycle through Flamecast runtime layer, not conductor hacks
- Prebuilt images through runtime implementation, not conductor complexity

## Phase 5: End-to-end validation

Add a cross-repo validation matrix for:

- local runtime
- remote runtime
- browser reconnect
- webhook-only integration
- terminal session flow
- runtime start/stop/pause
- remote workspace scenario

## Ownership by Layer

## `durable-acp-rs`

Should own:

- conductor/proxy-chain correctness
- hosted ACP endpoint(s)
- durable observation/state stream
- webhook forwarding
- MCP peering mechanics

Should not own:

- Docker/image authoring model
- runtime instance inventory as a product abstraction
- workspace upload UX
- browser-specific queue/session abstractions

## Flamecast host/runtime layer

Should own:

- unified runtime abstraction
- prebuilt images
- runtime lifecycle and inventory
- workspace strategy
- templates CRUD
- product UX

Should consume `durable-acp-rs` as a reusable runtime payload/control plane.

## Shared / integration seam

Should own:

- endpoint bundle contract
- auth/scoping model
- permission resolution flow
- cross-repo e2e tests

## What “done” looks like

The architecture is directionally aligned when all of these are true:

1. Any runtime can launch a conductor-backed ACP endpoint bundle consistently.
2. Flamecast can connect to that bundle without special-case transport logic.
3. Browser/mobile reconnects do not kill the underlying session/runtime.
4. State observation is durable and decoupled from command transport.
5. Webhook integrations work without persistent browser or WS sessions.
6. Runtime lifecycle is modeled at the runtime layer, not hacked into the conductor.
7. Terminal and multi-session capabilities are expressed in terms of the new
   primitives, not the old proprietary event plane.

## Summary

The remaining work is not primarily about ACP anymore.

The remaining work is about:

- choosing a stable runtime lifetime model
- making `durable-acp-rs` a thinner, cleaner conductor host
- making Flamecast the owner of runtime/product concerns
- defining one clean integration seam between them

If that separation is held consistently, the RFCs stop competing with each
other and become layered:

- `durable-acp-rs` satisfies the conductor/control/state substrate
- Flamecast satisfies the runtime and product abstraction
- the integration seam stays ACP-centric and portable
