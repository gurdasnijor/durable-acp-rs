# SDD: Spike 2 Session-First Remote Demo

> Status: Proposed
> Scope: validate a remote hosted-conductor UX using `durable-acp-rs` directly
> Related:
> - [`docs/hosted-conductor-architecture-sdd.md`](./hosted-conductor-architecture-sdd.md)
> - [`docs/spike-1-hosted-conductor-baseline-sdd.md`](./spike-1-hosted-conductor-baseline-sdd.md)
> - [`docs/transport-sdd.md`](./transport-sdd.md)

## Purpose

Spike 2 exists to answer one practical question:

> Can a remotely hosted `durable-acp-rs` instance provide a credible “remote
> agent on my computer/VM” experience using only ACP, durable state streaming,
> and the existing helper API?

This spike should validate the session-first remote model before introducing:

- full Flamecast runtime orchestration
- runtime lifecycle UI
- snapshotting/persistent volume guarantees
- a generalized remote computer control plane

## Why This Spike Matters

This is the first spike that directly validates the user-facing thesis behind
recent internal discussion:

- the agent can run remotely
- the client can be a browser or phone
- ACP remains the stable interaction boundary
- durable state survives client disconnects
- the remote environment is usable without a local terminal session

If this spike works, the team will have strong evidence that the hosted
conductor model is enough to support a meaningful remote UX.

## Goals

The spike should prove that a remote `durable-acp-rs` process can:

- expose ACP over WebSocket to a remote client
- expose durable state over the state stream
- expose filesystem inspection helpers
- support a real session/new + prompt flow from a remote client
- support disconnect/reconnect without losing the visible state history of the session

## Non-Goals

This spike should not attempt to solve:

- full runtime inventory or lifecycle management
- multiple remote computers
- browser authentication and multi-tenancy
- strong environment durability or snapshotting
- full terminal interactivity
- full Flamecast integration
- Slack/automation initiation surfaces

## Architecture Under Test

```text
Remote host / VM
  -> durable-acp-rs
      -> ACP endpoint (/acp)
      -> durable stream (/streams/...)
      -> product helper API (/api/v1/...)
      -> conductor
      -> proxy chain
      -> terminal agent

Remote browser / phone / thin client
  -> ACP WebSocket client
  -> state stream subscriber
  -> filesystem browser
```

Important constraints:

- the remote process is long-lived enough to host a session
- the client is session-first, not runtime-first
- the happy path should not require Flamecast

## Demo Story

The spike should support a simple, compelling demo:

1. Start `durable-acp-rs` on a remote host or VM against a real workspace.
2. Open a thin client from another machine or phone.
3. Connect to `/acp`.
4. Create a session.
5. Wait for session/cwd materialization to appear in the durable state view.
6. Send prompts and receive responses.
7. Watch prompt turns and chunks in the durable state view.
8. Browse the remote file tree.
9. Disconnect the client.
10. Reconnect and confirm the state view is still coherent.
11. If desired, create a new ACP session and continue working from the same remote workspace.

This demo should not depend on a local terminal remaining open.

## Deliverables

### 1. Remote hosting recipe

There should be a documented and repeatable way to start `durable-acp-rs` on a
remote machine or VM with:

- reachable ACP WebSocket endpoint
- reachable durable state stream
- reachable helper API

### 2. Thin remote client

There should be a minimal client or demo UI that can:

- connect to ACP over WS
- initialize
- create a session
- send a prompt
- receive streamed output
- subscribe to state stream updates
- inspect remote files

Preferred artifact:

- a minimal web page usable from a browser or phone
- implemented as a real ACP client using the TypeScript SDK ACP surface
- speaking to the hosted `/acp` endpoint over WebSocket

Fallback:

- a script plus a separate minimal state/file viewer

This does not need to be a polished product UI.

### 3. Disconnect/reconnect behavior

The spike should demonstrate that:

- the durable state history remains visible after client disconnect
- client disconnection does not erase visible state
- reconnecting restores the state view from the stream/replay model

This spike does **not** require:

- resuming the same live ACP session over a new WebSocket connection
- keeping the same conductor+agent subprocess alive after the browser disconnects

## Explicit Design Decisions

### Decision 1: Session-first, not runtime-first

The remote UX should be modeled around:

- connecting to a hosted ACP endpoint
- creating/continuing a session

It should not assume:

- pre-starting empty runtime instances from a runtime dashboard

Important clarification:

- each ACP WebSocket connection currently hosts its own conductor session
- reconnecting the browser/client means reconnecting to the durable state view
- continued prompting after reconnect will use a new ACP session unless a later
  spike adds transport/session bridging or `load_session`-based recovery

### Decision 2: ACP/session durability is the baseline

This spike relies on:

- durable ACP trace
- replayable materialized state

It does not require:

- persistent worker processes after failure
- workspace snapshotting
- volume snapshots

### Decision 3: Remote filesystem access is helper functionality

Filesystem browsing is useful for the demo, but it remains a helper API
outside ACP proper.

The ACP happy path is still:

- initialize
- session/new
- session/prompt

## Acceptance Criteria

### A. Remote session flow

- a remote client can connect over WS
- initialize succeeds
- session/new succeeds
- at least one prompt/response round trip succeeds

### B. Remote state visibility

- prompt turns appear in the state view
- chunk updates appear in the state view
- connection/session metadata appears in the state view
- filesystem helpers become usable only after session/cwd materialization has occurred

### C. Reconnect behavior

- disconnecting the thin client does not destroy the visible session state history
- reconnecting shows a coherent state/history view via the durable stream

For this spike, reconnect means:

- reconnecting to the same hosted `durable-acp-rs` process and durable stream

It does not mean:

- recovering the same live ACP session over a new WebSocket connection
- recovering after worker or host teardown
- restoring a deleted workspace
- full environment resurrection

### D. Filesystem visibility

- the client can retrieve a remote file preview
- the client can retrieve a directory tree rooted at the session workspace

## Proposed Demo Surface

There are two acceptable forms for the client side of the spike.

### Option A: Minimal web page

A tiny browser page that:

- uses the TypeScript ACP client implementation rather than a bespoke protocol wrapper
- opens the ACP WS connection
- opens a separate state-stream subscription
- shows prompt input and text output
- renders prompt turns/chunks from the state stream
- allows basic file tree inspection

This is the preferred option.

Reason:

- it validates ACP compliance on both sides of the wire
- it proves a standards-based browser client can interoperate with the hosted conductor directly
- it avoids demo success depending on a one-off custom client shim
- it makes the two-connection model explicit:
  - ACP WebSocket for command/control
  - state stream subscription for durable observation

### Option B: Script + simple dashboard

A combination of:

- one CLI or script to drive ACP prompts
- one stream-viewer page or terminal dashboard for state

Option A is more convincing for the “phone/browser” story, but either is
acceptable for the spike.

## Host Exposure Requirements

The remote host must expose:

- a reachable ACP WebSocket endpoint
- a reachable durable stream endpoint
- a reachable helper API endpoint

This can be done via:

- direct bind to a reachable interface
- SSH tunnel
- reverse proxy
- port forwarding

The spike should not fail due to an accidental localhost-only bind when remote
access is part of the demo goal.

## Proposed Test / Validation Plan

### Validation 1: Localhost remote simulation

Run the server locally, but connect through the same WS + stream APIs as a
remote client would use.

This validates:

- the client contract
- the hosting surfaces
- session-first UX flow

### Validation 2: Real remote host

Run the same setup on:

- a VM
- a remote dev box
- or a cloud sandbox with reachable networking

This validates:

- remote reachability
- no hidden localhost assumptions
- browser ACP client interoperability over the real hosted transport

### Validation 3: Reconnect

Start a session, send at least one prompt, disconnect the client, reconnect,
and verify:

- the state view is still reconstructible
- the browser can reconnect to ACP and create a fresh session if continued prompting is desired

### Validation 4: Filesystem helper behavior

Verify the remote client can:

- fetch one file preview
- fetch one directory listing

without any local shell access.

## Proposed Implementation Tasks

### Task 1: Define the remote startup path

Document how to run the hosted conductor remotely, including:

- ports
- bind addresses
- data directory
- agent command

Required:

- add a configurable bind address for hosted surfaces
- avoid a localhost-only default when the demo is intended to be remotely reachable

### Task 2: Build the thinnest viable remote client

Implement only what is needed to demonstrate:

- TypeScript SDK ACP client interoperability in the browser
- ACP connectivity
- separate durable stream subscription
- prompt flow
- state stream subscription
- basic filesystem browsing

### Task 3: Validate reconnect assumptions

Make sure reconnect behavior is based on:

- stream replay / materialized state
- not hidden in-memory client state
- and is documented as state continuity, not live ACP session continuity

### Task 4: Write one end-to-end remote demo test or harness

This can be:

- a process-level integration harness
- or a documented manual demo script with exact steps

## Risks

### Risk 1: Chunk/state visibility may still lag behind prompt flow

If the prompt works but the state stream misses chunk updates, the remote demo
will feel incomplete even if ACP is functioning.

### Risk 2: Filesystem helpers may depend on connection/session metadata timing

If `cwd` and session metadata projection are delayed or incomplete, the remote
file browser will be unreliable.

### Risk 3: Bind-address and reachability issues may mask the architecture

The spike should avoid failing for incidental networking reasons like:

- binding only to localhost when remote access is desired
- CORS mismatches
- missing proxy or tunnel setup

## Open Questions

### 1. What is the smallest acceptable client?

The team should decide whether the spike requires:

- a browser page using the TypeScript ACP client implementation
- or a script + minimal viewer

Recommendation:

- require the browser page
- allow the script only as a debugging aid

### 2. Is a single remote workspace enough?

For this spike, probably yes. The goal is not multi-computer orchestration.

### 3. What reconnect guarantee are we claiming?

This spike should be explicit:

- reconnect to the same hosted process and durable stream
- durable state/history continuity
- not same-session ACP continuation over a new WebSocket connection
- not full worker crash recovery across environment teardown
- not snapshot-backed environment restoration

## Success Outcome

If the spike succeeds, the team should be able to say:

- a remotely hosted `durable-acp-rs` instance is already enough for a real
  session-first remote agent demo
- a standards-based browser ACP client interoperates with it directly
- ACP + durable state stream + minimal helper API form a credible remote
  interaction model
- the current remote model provides durable state continuity, even though live
  ACP session continuity across reconnect remains future work
- the next layer of questions is product/control-plane orchestration, not
  whether the hosted-conductor model works at all
