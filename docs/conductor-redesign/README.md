# Conductor Redesign

Align the durable-acp-rs conductor with the ACP proxy-chains architecture.

## Goal

Shrink custom runtime code by using SDK primitives for conductor composition,
message routing, and passive state observation. Move application-specific
interpretation to the read side. Keep the conductor thin and protocol-focused.

**Before:** ConductorState god object, active DurableStateProxy interceptor,
server-side queue, hand-rolled prompt lifecycle, custom stream format.

**After:** Inline conductor composition, passive SDK tracing to durable stream,
read-side materialization from TraceEvents, no server-side queue, no custom
session proxy.

## Documents

| Document | Purpose |
|---|---|
| [`architecture.md`](architecture.md) | **Start here.** Authoritative architecture: decisions, target shape, responsibilities by layer, migration phases, open questions. |
| [`implementation.md`](implementation.md) | Implementation detail: DurableStreamTracer code, stream format change, StreamDb materialization, ConductorState dissolution map, boot sequence, file changes. |
| [`testing-and-hosting.md`](testing-and-hosting.md) | Test matrix (MVP + recommended), ACP hosting surface separation (acp_server.rs vs api.rs vs transport.rs). |
| [`queue-scope.md`](queue-scope.md) | Decision rationale for cutting server-side queueing. Client-side queue model if needed later. |

## Key References

- [ACP proxy-chains RFD](https://agentclientprotocol.com/rfds/proxy-chains)
- SDK tracing: `agent-client-protocol-conductor/src/trace.rs`, `snoop.rs`
- SDK cookbook: `reusable_components`, `running_proxies_with_conductor`
- [`@durable-streams/state`](https://github.com/durable-streams/durable-streams/tree/main/packages/state) (read-side materialization reference)
