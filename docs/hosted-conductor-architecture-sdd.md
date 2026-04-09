# SDD: Hosted Conductor Architecture and Validation Plan

> Status: Proposed
> Scope: convergence plan for `durable-acp-rs`, Flamecast, and remote/cloud execution
> Related:
> - [`docs/conductor-redesign/architecture.md`](./conductor-redesign/architecture.md)
> - [`docs/flamecast-runtime-boundary-sdd.md`](./flamecast-runtime-boundary-sdd.md)
> - [`docs/flamecast-gap-closure-sdd.md`](./flamecast-gap-closure-sdd.md)
> - [`docs/transport-sdd.md`](./transport-sdd.md)
> - [Symposium ACP transport architecture](https://agentclientprotocol.github.io/symposium-acp/transport-architecture.html?highlight=transport#design-principles)
> - [Symposium ACP conductor design](https://agentclientprotocol.github.io/symposium-acp/conductor.html)
> - [ACP proxy-chains RFD](https://agentclientprotocol.com/rfds/proxy-chains)

## Executive Summary

The current direction should converge on three layers:

- **Flamecast** as the run/workflow control plane
- **`durable-acp-rs`** as a durable hosted conductor and ACP harness
- **sandbox/VM/computer** as the explicit execution boundary

`durable-acp-rs` should not become the full runtime platform. It should become
the durable, hosted runtime for standard ACP primitives:

- conductor-backed agent endpoints
- proxy-chain composition
- transport exposure
- durable trace/state observation
- generic harness behavior around ACP

This keeps the system aligned with Symposium ACP while still supporting the
broader product direction discussed for Flamecast: remote execution, resumable
ACP sessions, collaboration surfaces, and shared reviewable artifacts.

## What The Specs Already Provide

The ACP / Symposium stack already defines the most important low-level
abstractions.

### Transport / protocol separation

The transport architecture explicitly separates:

- **protocol layer**
  - request ID assignment
  - request/response correlation
  - method dispatch
  - error handling
- **transport layer**
  - moving bytes or messages
  - serialization/deserialization
  - connection management

Implication:

- the same logical ACP surface can be hosted over stdio, WebSocket, TCP, named
  pipes, or in-process channels without changing the protocol semantics

### Conductor semantics

The conductor spec already defines the component that:

- orchestrates proxy chains
- initializes proxies and terminal agents correctly
- routes successor messages
- appears to the client as an ordinary ACP agent

Implication:

- `durable-acp-rs` does not need to invent a replacement abstraction for proxy
  chain orchestration
- it should host, persist, and operationalize the conductor model

### Proxy-chain semantics

The proxy-chains RFD already gives the right mental model:

- client sees one agent surface
- conductor manages the chain
- proxies are reusable ACP components
- transport/process boundaries are implementation details behind that surface

Implication:

- `durable-acp-rs` is best understood as a **hosted durable runtime for ACP
  conductor topologies**, not as a brand-new orchestration model

## Recommended System Roles

### Flamecast

Flamecast should own:

- collaboration surfaces
- lifecycle and orchestration
- scheduling
- auth / tenancy / sharing
- runtime inventory
- workspace and sandbox selection
- product-facing APIs and review surfaces

### durable-acp-rs

`durable-acp-rs` should own:

- hosted conductor runtime
- ACP endpoint exposure
- proxy-chain composition
- durable ACP trace capture
- read-side state materialization
- generic harness capabilities at the ACP layer

Examples of valid scope:

- peer MCP proxying
- context injection proxies
- response filtering/transformation
- run/session execution trace
- policy/observation around ACP traffic

Examples of invalid or risky scope:

- VM or image lifecycle ownership
- broad product REST surface
- browser-specific UX logic
- team/workspace inventory
- full run/collaboration control plane

### Sandbox / VM / computer

The sandbox should own:

- repository workspace
- process execution
- scoped credentials
- configured tools
- isolation boundary

### Worker / agent

The worker should be ephemeral:

- performs one slice of work
- writes artifacts and trace
- terminates

## Recommended Framing For durable-acp-rs

Avoid:

- “Terraform for ACP topologies”
- “the whole runtime platform”
- “the VM manager”

Prefer:

- **durable hosted conductor runtime**
- **durable ACP harness**
- **hosted ACP data plane**

Those are more accurate to the spec and to the repo’s strengths.

## Design Proposal

## Proposal A: Hosted Conductor Runtime

This is the recommended baseline.

`durable-acp-rs` provides:

- one or more conductor-backed ACP endpoints
- optional product HTTP helpers
- durable trace/state stream
- proxy-chain composition around a terminal agent

The client sees:

- one ACP agent endpoint
- optional state stream

The conductor internally manages:

- proxy chain
- successor routing
- initialization

This aligns directly with Symposium ACP.

### Why this is the recommended base

- smallest conceptual distance from the specs
- preserves ACP as the stable harness boundary
- minimizes custom runtime semantics inside `durable-acp-rs`
- works locally and remotely
- composes cleanly with Flamecast as control plane

## Proposal B: Hosted Conductor + Declarative Topology Spec

This is a possible extension, not the starting point.

Add a declarative configuration layer describing:

- terminal agent command
- proxy chain membership
- transport exposure
- persistence/tracing policy
- optional placement hints

Example shape:

```toml
[endpoint]
transport = "ws"
path = "/acp"

[trace]
stream = "durable-acp-state"

[[components]]
id = "peer-mcp"
kind = "proxy"
impl = "peer_mcp"

[[components]]
id = "agent"
kind = "terminal_agent"
command = ["claude", "--print", "hello"]
```

This should be treated as:

- a **hosting/configuration spec**
- not a full infrastructure planner

### Why this is second, not first

- it is useful only after the hosted-conductor model is stable
- otherwise it risks hiding unresolved questions behind a bigger abstraction
- it should configure standard conductor/runtime primitives, not replace them

## Proposal C: Central Control Plane + Remote Computer Bridge

This is the long-term remote/cloud shape most aligned with the discussions.

```text
Flamecast
  -> durable run/control plane
  -> chooses computer/sandbox
  -> manages collaboration and review

Remote computer bridge
  -> hosts durable-acp-rs or ACP-compatible harness runtime
  -> exposes ACP endpoint + state stream
  -> runs agent in isolated workspace
```

This matches the “computer/sandbox separate from the agent control plane”
direction from recent internal discussion.

### Why this is attractive

- supports many computers / many agents
- avoids trapping too much control-plane scope inside one VM
- keeps ACP stable while changing execution placement
- matches the durable-run / ephemeral-worker model described in Spectre-like systems

### Why this should not block near-term progress

- it is a deployment and boundary evolution
- the hosted-conductor runtime can be validated first on one machine or one VM

## Scoping Decisions

### In scope for the next phase

- durable hosted conductor runtime
- strict ACP transport / product API separation
- stable state-stream projection
- session-first remote hosting path
- minimal filesystem / registry / terminal helper APIs
- Flamecast integration as a control-plane client of `durable-acp-rs`

### Out of scope for the next phase

- full VM/image lifecycle management inside `durable-acp-rs`
- multi-tenant runtime orchestration
- broad browser-specific product APIs inside `durable-acp-rs`
- queueing/server-side turn admission
- full “topology compiler” abstraction

## Durability Model

The stack should distinguish three forms of durability.

### 1. ACP/session durability

This is the baseline guarantee and should be owned by `durable-acp-rs`.

It includes:

- durable trace/event sink
- replayable ACP session history
- projected execution state
- support for ACP `load_session` / replay semantics

This is the primary durability guarantee for the hosted conductor runtime.

### 2. Environment durability

This is optional and runtime-dependent.

Examples:

- Docker volume snapshots
- Daytona workspace persistence
- E2B sandbox resume/snapshot behavior
- persistent working directories

This should be treated as a capability that can be ratcheted up by runtime
providers, not as a universal baseline assumption.

### 3. Workflow/product durability

This is optional and belongs above ACP when needed.

Examples:

- review state
- sharing and ownership
- scheduling metadata
- Slack/web/CLI linkage
- approvals beyond ACP-visible state

This does not need to exist as a separate durable object unless the product
really needs a richer collaborative abstraction than ACP sessions and their
artifacts.

## Key Design Questions

### 1. What is the baseline durable object?

Recommendation:

- the baseline durable object should be the **ACP session and its replayable event log**
- worker processes are ephemeral
- stronger environment durability is additive
- richer workflow durability is optional and product-driven

### 2. Where does the harness live?

Recommendation:

- near term: inside the sandbox/hosted environment for simplicity
- longer term: keep the control-plane responsibilities in Flamecast, even if the ACP harness runs close to the worker

### 3. How much runtime orchestration belongs in durable-acp-rs?

Recommendation:

- enough to host conductors, proxies, agents, and transports
- not enough to own VM fleet lifecycle or workspace platform semantics

### 4. Do projection types stay?

Recommendation:

- yes
- ACP SDK types should remain the wire/protocol types
- projection rows remain the read model for durable state and product queries
- keep projection rows minimal and isolate legacy compatibility fields

## Independent Validation Spikes

Each spike should be independently valuable and reversible.

## Spike 1: Hosted Conductor Baseline

Goal:

- validate `durable-acp-rs` as a clean hosted conductor runtime with no extra product assumptions

Deliverables:

- run agent behind conductor
- expose ACP over WebSocket and stdio
- emit durable trace/state stream
- pass a basic prompt e2e over WS and stdio

Success criteria:

- same ACP client code can talk to stdio-hosted and WS-hosted endpoints
- state stream materializes prompt turns/chunks/connections

## Spike 2: Session-First Remote Demo

Goal:

- validate the “remote computer” story without solving full runtime lifecycle

Deliverables:

- one remote host or VM running `durable-acp-rs`
- browser or phone connects over ACP WS
- file tree and state stream visible
- prompt/response works against a remote workspace

Success criteria:

- remote prompt flow works without local terminal
- reconnecting client can re-open the same durable state view

## Spike 3: Flamecast As Control Plane

Goal:

- validate the boundary where Flamecast acts as the control plane and `durable-acp-rs` is the hosted ACP harness

Deliverables:

- Flamecast creates or attaches to a session-facing control-plane object
- Flamecast launches or attaches to a hosted `durable-acp-rs`
- Flamecast stores ACP endpoint + state stream URL
- Flamecast uses ACP for control and the stream for observation

Success criteria:

- no legacy session-host prompt API required for the happy path
- one session-facing object can be revisited from browser surfaces

## Spike 4: Declarative Topology Config

Goal:

- validate whether a small declarative spec improves usability without obscuring the model

Deliverables:

- config file describing:
  - terminal agent command
  - proxy list
  - transport exposure
  - trace stream name
- host starts from config

Success criteria:

- useful for local/remote startup
- does not invent new lifecycle semantics
- remains a thin layer over standard conductor/runtime primitives

## Spike 5: Remote Computer Bridge

Goal:

- validate the longer-term “control plane outside, execution boundary inside” split

Deliverables:

- tiny “computer host” service
- starts isolated workspace execution
- runs `durable-acp-rs` inside or beside that workspace
- returns ACP endpoint + state stream metadata to Flamecast

Success criteria:

- many remote computers can be independently addressed
- control plane is not embedded in each worker session

## Spike 6: ACP Replay / `load_session` Recovery

Goal:

- validate that ACP/session durability is sufficient as the baseline recovery model

Deliverables:

- durable trace persisted for a real session
- hosted worker terminated
- new hosted worker or harness process started
- `load_session` used to restore the ACP-visible session

Success criteria:

- conversation/session state is reconstructed from replay
- client can continue from restored ACP state without preserving the old process

## Spike 7: Runtime-Specific Environment Durability

Goal:

- validate when environment durability materially improves the user experience

Deliverables:

- one runtime without snapshots
- one runtime with snapshot or persistent volume support
- same workflow exercised on both

Success criteria:

- clear understanding of what additional value snapshotting provides over ACP replay alone
- documented cost/complexity tradeoff by runtime type

## Implementation Sequence

Recommended order:

1. Hosted Conductor Baseline
2. Session-First Remote Demo
3. Flamecast As Control Plane
4. ACP Replay / `load_session` Recovery
5. Declarative Topology Config
6. Runtime-Specific Environment Durability
7. Remote Computer Bridge

Reasoning:

- this proves the core hosted-conductor role first
- then validates the user-facing remote experience
- then validates the control-plane boundary
- then tests ACP replay as the baseline durability guarantee
- then measures where stronger environment durability is worth the extra complexity
- only then adds optional abstraction layers

## Practical Recommendations

### Recommendation 1

Keep pushing `durable-acp-rs` toward:

- hosted conductor runtime
- durable trace/state plane
- reusable ACP harness behavior

### Recommendation 2

Keep pushing Flamecast toward:

- collaboration/control plane
- collaboration surfaces
- scheduling and lifecycle
- runtime inventory and sandbox selection

### Recommendation 3

Do not expand `durable-acp-rs` into:

- VM/image orchestrator
- browser-specific application backend
- full product run model

### Recommendation 4

Treat declarative topology as a later convenience layer, not the core abstraction.

## Proposed Next Work

- document `durable-acp-rs` externally as a durable hosted conductor runtime
- build the smallest remote demo around ACP endpoint + state stream + filesystem
- finish Flamecast’s native ACP + stream path
- add one explicit architecture note that ACP replay/session durability is the baseline guarantee
- evaluate stronger environment durability separately per runtime
- keep remote computer / VM orchestration outside `durable-acp-rs`

## Bottom Line

The system should converge on:

- **`durable-acp-rs`** as the durable hosted ACP harness around conductors and proxy chains
- **Flamecast** as the collaboration/control plane layered on top when needed
- **sandbox/computer** as the execution boundary
- **workers** as ephemeral executors

That is the cleanest way to match:

- the Symposium ACP transport and conductor model
- the proxy-chains extension vision
- the remote/cloud discussions shared internally
- the need for durable, reviewable, resumable collaborative runs
