# SDD: Spike 3 Flamecast as Control Plane

> Status: Proposed
> Scope: validate Flamecast as a control plane over hosted `durable-acp-rs`
> Related:
> - [`docs/hosted-conductor-architecture-sdd.md`](./hosted-conductor-architecture-sdd.md)
> - [`docs/flamecast-runtime-boundary-sdd.md`](./flamecast-runtime-boundary-sdd.md)
> - [`docs/spike-2-session-first-remote-demo-sdd.md`](./spike-2-session-first-remote-demo-sdd.md)

## Purpose

Spike 3 exists to answer one key boundary question:

> Can Flamecast act as a collaboration/control plane on top of
> `durable-acp-rs`, using ACP for command/control and the durable state stream
> for observation, without relying on the legacy session-host prompt API?

This spike should validate the integration boundary, not redesign either
system.

## Why This Spike Matters

If Spike 2 proves that a hosted `durable-acp-rs` instance is enough for a
session-first remote experience, Spike 3 proves whether Flamecast can sit above
that cleanly.

This is the critical product boundary:

- `durable-acp-rs` as the hosted ACP harness
- Flamecast as the collaboration/control plane

If this spike succeeds, the team can stop treating `durable-acp-rs` like a
partial session-host clone and instead integrate against its actual strengths:

- ACP endpoint
- durable state stream
- narrow helper API

## Goals

The spike should prove that Flamecast can:

- launch or attach to a hosted `durable-acp-rs` instance
- store ACP endpoint and state stream metadata
- use ACP for initialize/session/prompt flows
- use the durable stream or state surface for observation
- present a usable session-centric UI without depending on legacy prompt REST

## Non-Goals

This spike should not attempt to solve:

- full multi-runtime inventory redesign
- full runtime-page UX for `durable-acp-rs`
- worker crash recovery
- runtime snapshotting
- multi-computer orchestration
- final auth or multi-tenant product semantics

## Architecture Under Test

```text
Flamecast
  -> session/run metadata
  -> browser-facing product UI
  -> runtime provider or integration adapter

runtime provider / adapter
  -> starts or attaches to durable-acp-rs
  -> returns:
     - ACP endpoint
     - state stream URL
     - helper API base URL

durable-acp-rs
  -> /acp
  -> /streams/...
  -> /api/v1/...
  -> conductor + proxy chain + agent
```

Important constraint:

- Flamecast should consume `durable-acp-rs` as an external hosted ACP/session
  substrate, not force it back into the old session-host HTTP prompt contract

## Core Hypothesis

The primary integration contract should be:

- **ACP endpoint** for command/control
- **durable state stream** for observation
- **helper API** only for non-ACP concerns like filesystem browsing

Not:

- legacy `/start` + `/prompt` + `/terminate` prompt REST as the main control path

## Deliverables

### 1. Runtime/provider integration path

There should be a clear way for Flamecast to:

- start or attach to a `durable-acp-rs` process
- persist the returned connection metadata
- treat that metadata as the source of truth for the session UI

Minimum persisted metadata:

- ACP WebSocket URL
- state stream URL
- helper API base URL
- runtime/provider identity

### 2. Native ACP client path in Flamecast

Flamecast should be able to drive a session using:

- the TypeScript ACP client path
- not the legacy session-host prompt REST path

At minimum:

- initialize
- session/new
- session/prompt

### 3. Durable observation path in Flamecast

Flamecast should be able to observe the session using:

- durable stream subscription
- or a debug/state endpoint temporarily for the spike

The preferred path is the durable stream itself.

### 4. Usable session-centric UI flow

The Flamecast user should be able to:

- create or open a session backed by `durable-acp-rs`
- send prompts
- see streamed responses
- see state/history
- browse files after session/cwd materialization

This flow should avoid runtime-first assumptions where possible.

## Explicit Design Decisions

### Decision 1: Flamecast is a control plane, not the transport itself

Flamecast should store and route to ACP/state endpoints, not replace ACP with
its own bespoke prompt protocol.

### Decision 2: `durable-acp-rs` remains session-first

For this spike, Flamecast should integrate with the current session-first model.

This means:

- the runtime page may remain an imperfect fit
- the session creation path is the canonical integration path

### Decision 3: Legacy session-host compatibility is transitional only

If any compatibility shim remains for:

- `/start`
- `/terminate`
- `/files`

it should be treated as an adapter, not as the target architecture.

## Acceptance Criteria

### A. Session creation

- Flamecast can create a session backed by `durable-acp-rs`
- the resulting session record contains ACP/state metadata needed by the UI

### B. Prompt/control path

- the happy path uses ACP, not legacy prompt REST
- a user can prompt the agent from Flamecast and receive a response

### C. Observation path

- the session UI can show durable state/history
- the UI does not depend on polling a legacy prompt API for core session state

### D. Filesystem helper path

- file tree or file preview works after session/cwd materialization
- helper API usage is clearly separated from ACP control

## Proposed Validation Plan

### Validation 1: Flamecast session creation

Create a `durable-acp-rs`-backed session from Flamecast.

Verify:

- runtime/provider metadata is persisted
- ACP URL and state stream URL are visible in the session/runtime metadata

### Validation 2: Native ACP prompt flow

Use the Flamecast browser path to:

- initialize
- create a session
- send a prompt
- receive a response

Verify:

- no legacy prompt REST endpoint is needed for the happy path

### Validation 3: Durable state rendering

Verify the Flamecast UI can render:

- prompt turns
- chunks / output updates
- connection/session metadata

using the durable stream or state surface

### Validation 4: Session revisit

Reload the page or re-open the session.

Verify:

- the session-facing UI can reconstruct state/history
- if continued prompting is needed, Flamecast can establish a fresh ACP client
  session against the hosted runtime

This spike does not require same-session ACP continuation across reconnect.

## Proposed Implementation Tasks

### Task 1: Normalize session metadata in Flamecast

Ensure Flamecast stores and surfaces:

- ACP WebSocket URL
- state stream URL
- helper API base URL

### Task 2: Use the native ACP client path in the Flamecast UI

Leverage the browser ACP client path already introduced for durable-acp-backed
sessions.

The spike should avoid reintroducing legacy prompt REST coupling.

### Task 3: Wire durable state into the session view

Use the durable stream or temporary state endpoint to populate:

- prompt history
- chunks/output
- session metadata

### Task 4: Document the session-first limitation honestly

Flamecast should not pretend that `durable-acp-rs` supports:

- empty runtime prestart
- same-session ACP continuation after browser reconnect

unless those are actually implemented.

## Risks

### Risk 1: Flamecast still assumes runtime-first semantics in some UI paths

The runtime pages may continue to steer users into flows that do not match the
session-first hosted-conductor model.

### Risk 2: Metadata shape remains underdefined

If ACP URL, state stream URL, and helper API base URL are not first-class
enough in Flamecast, the integration will stay brittle.

### Risk 3: Durable observation path may be split across temporary and target APIs

The spike may use `/api/v1/state` or similar debug surfaces for convenience,
but the intended long-term observation path should remain the durable stream.

## Open Questions

### 1. Should the runtime page participate in this spike?

Recommendation:

- no
- keep the spike focused on the session creation and session interaction flow

### 2. Should Flamecast subscribe to the durable stream directly, or use a temporary state endpoint first?

Recommendation:

- prefer direct durable stream subscription if it is easy enough
- allow temporary state-endpoint polling only if it materially speeds up the spike

### 3. What is the minimum session metadata contract Flamecast needs?

Recommendation:

- ACP WebSocket URL
- state stream URL
- helper API base URL
- runtime/provider identity

## Success Outcome

If the spike succeeds, the team should be able to say:

- Flamecast can act as a control plane over `durable-acp-rs`
- the integration uses ACP for command/control and durable state for observation
- the current session-first hosted-conductor model is viable inside Flamecast
- any remaining product gaps are about lifecycle UX and richer workflow
  semantics, not about the core integration boundary
