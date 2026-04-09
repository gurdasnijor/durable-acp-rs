# SDD: Flamecast Runtime Boundary for durable-acp-rs

> Status: Proposed
> Scope: boundary between Flamecast runtime providers and `durable-acp-rs`
> Related:
> - [`docs/conductor-redesign/architecture.md`](./conductor-redesign/architecture.md)
> - [`docs/flamecast-gap-closure-sdd.md`](./flamecast-gap-closure-sdd.md)
> - `/Users/gnijor/smithery/flamecast/packages/protocol/src/runtime.ts`
> - `/Users/gnijor/smithery/flamecast/packages/protocol/src/runtime-host.ts`

## Executive Summary

Flamecast should own the **runtime host** role.

`durable-acp-rs` should not try to become the full Flamecast runtime abstraction.
It should instead become the managed process launched **inside** a Flamecast
runtime instance, where it provides:

- a conductor-backed ACP endpoint
- durable trace/state streaming
- generic proxy-chain / MCP behavior

Flamecast should own:

- runtime instance lifecycle
- workspace ownership and mounting
- image/container/sandbox concerns
- session-to-instance mapping
- auth and browser-facing product APIs

This keeps `durable-acp-rs` close to the ACP conductor architecture while
letting Flamecast satisfy the broader unified-runtime and runtime-lifecycle RFCs.

## Existing Flamecast Boundary

Flamecast already has a pluggable runtime provider seam in
`@flamecast/protocol/runtime`.

That interface already makes Flamecast the natural owner of:

- `start(instanceId)`
- `stop(instanceId)`
- `pause(instanceId)`
- `delete(instanceId)`
- `getInstanceStatus(instanceId)`
- `fetchSession(sessionId, request)`
- `fetchInstance(instanceId, request)`

This is the correct architectural layer for:

- Docker / E2B / remote sandbox specifics
- workspace upload or bind-mount logic
- runtime inventory and persistence
- browser/mobile attachment semantics

## Recommended Boundary

### Flamecast owns runtime concerns

Flamecast runtime providers should own:

- creating or resuming runtime instances
- allocating ports / public URLs
- copying, syncing, or mounting workspaces
- selecting images / containers / sandboxes
- launching `durable-acp-rs` inside the runtime
- stopping / pausing / deleting instances
- persisting instance identity and recovery metadata

### durable-acp-rs owns ACP substrate concerns

`durable-acp-rs` should own:

- conductor + proxy chain composition
- ACP endpoint hosting
- passive trace capture
- durable state stream emission
- generic MCP proxy functionality
- read-side state materialization

It should not own:

- runtime instance inventory
- workspace lifecycle
- browser-specific reconnect semantics
- product queue policy
- broad product REST semantics

## Integration Modes

There are two possible integration modes.

### Mode A: Native ACP mode

This is the recommended target architecture.

```text
Flamecast Runtime Provider
  -> starts runtime instance
  -> launches durable-acp-rs
  -> returns ACP endpoint + state stream endpoint

Browser / mobile / automation
  -> talks ACP for commands
  -> consumes durable stream / SSE for state
```

Characteristics:

- closest to ACP and conductor architecture
- smallest custom code footprint in `durable-acp-rs`
- best fit for the ongoing rearchitecture

Required Flamecast evolution:

- treat ACP endpoint and state stream as first-class runtime outputs
- stop assuming all runtimes expose the legacy session-host prompt API
- store a `stateStreamUrl` or equivalent runtime endpoint metadata explicitly

### Mode B: Session-host compatibility shim

This is a transitional adapter only.

```text
Flamecast Runtime Provider
  -> launches durable-acp-rs
  -> translates legacy SessionHost /start, /terminate, /files requests
  -> exposes ACP websocket URL as the runtime websocket
```

Characteristics:

- useful for validating packaging and ownership
- compatible with current Flamecast runtime-provider shape
- does not fully solve prompt / permission / terminal semantics

Limitations:

- `durable-acp-rs` does not natively implement the old session-host HTTP contract
- prompt submission should move to ACP rather than legacy REST
- permissions should eventually be driven by durable state + ACP or a dedicated product API

## Recommended Runtime Model

### Top-level lifetime

The stable long-lived unit should be the **runtime instance**.

That runtime instance owns:

- workspace
- environment
- lifecycle
- public routing metadata

### Inside the runtime instance

Run `durable-acp-rs` as a managed process:

```text
runtime instance
  -> durable-acp-rs
      -> conductor
      -> proxy chain
      -> agent subprocess
      -> ACP endpoint
      -> durable state stream
```

### Session model

Short term:

- allow one Flamecast session per `durable-acp-rs` process if that matches current constraints

Long term:

- decide whether one runtime instance hosts:
  - one conductor-backed agent
  - one or more ACP sessions
  - or one conductor per session

That choice belongs primarily to Flamecast’s runtime layer, not to the
conductor internals.

## Required Contract Changes

### 1. Runtime outputs should include state stream information

Current persisted runtime info stores:

- `hostUrl`
- `websocketUrl`
- `runtimeName`
- `runtimeMeta`

For native durable-acp integration, Flamecast should also know:

- `stateStreamUrl`

This can be introduced either:

- as a first-class persisted field
- or temporarily inside `runtimeMeta`

The first option is cleaner.

### 2. ACP should become a first-class command surface

Today Flamecast still assumes a legacy session-host HTTP prompt model.

For durable-acp-native integration, the command surface should become:

- ACP endpoint for command/control
- durable stream for state

Flamecast may still provide product-level REST adapters, but those should be
owned by Flamecast, not pushed down into `durable-acp-rs`.

### 3. Runtime provider should own adapter behavior

If Flamecast still needs compatibility with:

- `/start`
- `/terminate`
- `/files`
- `/fs/tree`

that translation belongs in the Flamecast runtime provider layer.

It should not force `durable-acp-rs` to become a general-purpose runtime-host
API server.

## Concrete Recommendation

### Short term

- implement a `DurableAcpRuntime` provider in Flamecast
- let it launch `durable-acp-rs`
- let it persist:
  - API base URL
  - ACP websocket URL
  - state stream URL
- use it as a transitional validation layer

### Medium term

- teach Flamecast’s session/runtime layer about native ACP + durable stream
- reduce dependence on the old session-host HTTP prompt model
- move browser-facing adaptation logic into Flamecast

### Long term

- treat `durable-acp-rs` as a reusable runtime payload
- let Flamecast runtime providers decide whether that payload runs:
  - locally
  - in Docker
  - in E2B
  - in another remote environment

## Acceptance Criteria

This boundary is considered validated when:

1. a Flamecast runtime provider can start `durable-acp-rs` as a managed process
2. Flamecast can persist the runtime endpoints needed to reconnect:
   - ACP endpoint
   - state stream
3. Flamecast, not `durable-acp-rs`, owns runtime instance lifecycle
4. `durable-acp-rs` is not required to grow a large product-specific runtime-host API
5. the native target remains:
   - ACP for commands
   - durable stream for state
