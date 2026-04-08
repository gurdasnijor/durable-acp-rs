# SDD: Minimal Conductor, Client-Side Queue Later

> Status: Proposed
> Parent: [`architecture.md`](architecture.md)
> Decision: cut all server-side queue/admission scope for now

## Problem

The current architecture keeps drifting toward putting prompt scheduling policy
inside the conductor:

- queueing
- pause/resume
- reorder
- cancel-before-send
- prompt lifecycle bookkeeping tightly coupled to proxy handlers

That is too much scope for what is currently just transient behavior.

If the queue is:

- in-memory only
- owned by one local UI/app process
- not shared across remote clients
- not part of the protocol contract

then it should not live in the conductor at all.

## Decision

For now, the conductor should do **no server-side turn admission or queue
management**.

This means:

- no `QueueProxy`
- no server-side `TurnAdmission`
- no conductor-owned transient prompt queue
- no REST queue API

The conductor immediately forwards prompts downstream.

If queueing is needed later, it should first be implemented on the
**`acp::Client` / app side**, not in the conductor.

## Architecture

```text
App / Dashboard / TUI
  → optional in-memory prompt scheduler
  → acp::Client / ActiveSession
  → Conductor
      → PeerMcpProxy
      → Agent

Tracing / state observation
  - passive via SDK trace/snoop
```

## Responsibilities

### Conductor

The conductor owns only protocol-level concerns:

- session bootstrap
- proxy-chain composition
- MCP injection / MCP-over-ACP support
- passive observation of ACP traffic

It does not own local UX scheduling policy.

In the current codebase, it also does **not** need a dedicated custom
`SessionProxy` component for `session/new`. There is no existing need to mutate
or decorate `NewSessionRequest` before forwarding it.

### App / `acp::Client`

If a dashboard or TUI wants queue-like behavior, it owns it locally:

- hold prompts in memory
- release prompts one at a time
- pause/resume
- reorder
- cancel-before-send

That is an application concern, not a conductor concern.

## Why This Is Better

### 1. Matches the real scope

Right now the queue is transient and local. That is a poor fit for conductor-side
state and proxy interception.

### 2. Keeps the conductor protocol-focused

The conductor should be responsible for ACP routing and composition, not local UI
behavior.

### 3. Uses the SDK where it is strongest

The SDK already provides strong session primitives for bootstrap and proxying.
That is where conductor customization should sit.

### 4. Preserves an easy path later

If real shared queueing requirements appear later, they can still be added.
Nothing about immediate forwarding prevents introducing a scheduler in front of
prompt submission at a later date.

## Recommended Conductor Shape

### 1. immediate session forwarding

`session/new` should remain as small as possible.

If the conductor needs explicit handling at all, it should use the SDK session
helpers directly:

- `build_session_from(request)`
- `on_proxy_session_start(...)` or `start_session_proxy(...)`

But a named `SessionProxy` type is **not required** unless future requirements
appear.

### 2. `PeerMcpProxy`

Unchanged.

### 3. No queue proxy

`session/prompt` should pass through immediately.

If intercepted at all, it should only be for observation or narrow validation,
not scheduling policy.

## Current `session/new` Needs

Reviewing the current repo, the only practical needs around `session/new` are
observational:

- capture `session_id`
- capture `cwd`

Those values are used elsewhere in the app model:

- peer-to-peer HTTP prompting needs `latest_session_id`
- filesystem APIs need `cwd`

That means the architecture must preserve a way to materialize those values, but
it does **not** imply a dedicated custom session proxy today.

## When A Custom `SessionProxy` Would Become Justified

Introduce one only if a real need appears, such as:

- injecting per-session MCP servers dynamically
- validating or rewriting `NewSessionRequest`
- attaching explicit non-derivable session registration side effects

Until then, `session/new` customization is unnecessary scope.

## Client-Side Queueing Model

If queueing is later desired in a dashboard/TUI, the preferred first
implementation is:

```text
PromptScheduler (in app memory)
  → ActiveSession::send_prompt(...)
  → ActiveSession::read_update() / read_to_string()
```

That scheduler can own:

- `queued_prompts`
- `paused`
- `active_turn`
- cancel-before-send
- reorder

with no conductor changes.

## What We Are Explicitly Not Building

- server-side prompt queue
- queue REST endpoints
- queue durability
- queue state in the durable stream
- server-side prompt admission abstraction

This is intentional YAGNI.

## Future Upgrade Path

If shared queueing becomes necessary later, add it in this order:

### Phase 1

Implement queueing in the app/client layer only.

This is still local, transient, and easy to remove or revise.

### Phase 2

Only if multiple clients or remote control require one canonical queue:

- introduce a server-side scheduler
- add queue APIs
- emit explicit queue state events

This should be justified by real multi-client requirements, not anticipated now.

## Acceptance Criteria

- conductor forwards prompts immediately
- no server-side queue state exists
- no server-side queue API exists
- session bootstrap remains minimal and may use SDK session proxy primitives directly
- any future queueing can be added first in the app/client layer

## Summary

The minimal architecture is:

- keep the conductor small
- keep it protocol-focused
- treat transient prompt queueing as a client concern
- only move queueing into the conductor if real shared-server requirements emerge

That is the cleanest YAGNI position and still leaves a straightforward path for
adding queueing later.
