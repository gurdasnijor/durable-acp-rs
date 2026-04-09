---
name: Architecture handoff — state management, boundaries, orthogonality
description: Detailed handoff for next agent to fix architectural issues: state management, component boundaries, transport vs streaming vs conductor concerns, global state leaks.
type: project
---

## Current State (2026-04-08)

The system works end-to-end: Flamecast UI → ACP WebSocket → conductor → Claude Code agent → responses render in chat. But the architecture has significant boundary violations and state management issues.

## What was just shipped

### durable-acp-rs (Rust conductor)
- **Shared ConductorState** across WS connections (was creating new state per WS)
- **STATE-PROTOCOL compliance** — `type` field at top level per durable-streams spec
- **Binary WebSocket frame support** — `use-acp` sends binary (ndJsonStream), was being dropped
- **Skip non-content SessionUpdate variants** — no more raw JSON in chat
- **stream_subscriber.rs** swapped to `durable-streams` crate client (from hand-rolled SSE)
- **react.tsx** stripped to observation-only (`DurableStreamProvider`)

### flamecast (React UI)
- **use-acp** for ACP connection (replaced 260 lines of hand-rolled ClientSideConnection)
- Manual `initialize` + `newSession` in provider effect (use-acp doesn't auto-init)
- Prompts via `agent.prompt()`, permissions via SDK types

## Architectural Problems to Fix

### 1. ConductorState is still a god object

`src/conductor_state.rs` mixes:
- **Stream writing** (`write_state_event`, `record_chunk`, `update_connection`) — should be behind `DurableStateProxy`
- **Prompt queue** (`enqueue_prompt`, `take_next_prompt`, `cancel/pause/reorder`) — runtime state
- **Connection identity** (`logical_connection_id`) — just a string
- **Session mapping** (`session_to_prompt_turn`) — proxy-internal concern

All of this leaks into: `api.rs` (queue management routes), `conductor.rs`, `main.rs`, `durable_state_proxy.rs`, all test files.

**Previous work (on a separate branch, not merged):** A `PromptQueue` type was extracted (`src/prompt_queue.rs`), `DurableStateProxy` was rewritten to hold primitives directly, `api.rs` got an `ApiState` struct. This compiled and tests passed but was shelved for the quick fix. See `docs/transport-bridge-conductor-state-sdd.md` and `memory/project_transport_bridge.md`.

### 2. Transport vs conductor lifecycle

Each `/acp` WebSocket still spawns a NEW conductor + agent subprocess. When the browser reloads, the old conductor dies (killing MCP bridges, Claude Code connection). The durable stream preserves state, but the agent restarts.

The SDK model: `ConductorImpl::run(self, transport)` consumes the conductor. One conductor = one transport = one lifecycle. `AcpAgent` spawns a subprocess on `connect_to` and kills it on drop.

**Options explored but not implemented:**
- Transport bridge (duplex pattern from SDK's `initialization_sequence.rs`) — conductor runs on duplex, WS attaches/detaches. See `docs/transport-bridge-conductor-state-sdd.md`.
- Decided against for now: the SDK doesn't support persistent conductors natively.

### 3. `DurableStateProxy` does too much

It intercepts ACP messages AND manages the prompt queue AND writes to the durable stream AND maps session IDs to prompt turns. The queue drain logic (`drive_queue`) is especially entangled.

Should be: proxy intercepts messages, delegates to focused components.

### 4. API routes reach through ConductorState

`api.rs` holds `Arc<ConductorState>` for:
- Queue management (pause/resume/cancel/reorder) — needs runtime queue state
- Filesystem access — needs `cwd` from connection row (readable from StreamDb)
- Terminal kill — needs to write to stream

Per `docs/api-architecture-sdd.md`: REST API should be read-only + queue management. All prompt submission goes through ACP.

### 5. Flamecast provider does manual ACP init

`packages/ui/src/provider.tsx` has a `useEffect` that calls `agent.initialize()` + `agent.newSession()` after `use-acp` connects. This is because `use-acp` doesn't auto-initialize — it's designed for `stdio-to-ws` where the agent drives initialization.

This should either be fixed upstream in `use-acp` or encapsulated properly.

### 6. Multiple React instances / build tooling brittleness

- `@durable-acp/server` is symlinked from the Rust repo into Flamecast's node_modules
- Stale `dist/` causes Vite to resolve compiled JS instead of source `.tsx`
- `use-acp` brought a second React copy (fixed with `resolve.dedupe` in vite.config.ts)
- tsconfig needs manual path mapping for the symlink

## Key SDK References

The next agent MUST read these before making changes:

- **SDK cookbook:** `/Users/gnijor/.cargo/git/checkouts/rust-sdk-762d172c5e846930/ec9ceae/src/agent-client-protocol-cookbook/src/lib.rs` — patterns for clients, proxies, agents, conductors
- **Conductor implementation:** `/Users/gnijor/.cargo/git/checkouts/rust-sdk-762d172c5e846930/ec9ceae/src/agent-client-protocol-conductor/src/conductor.rs` — `ConductorImpl::run()` consumes self, uses reactive mode
- **AcpAgent:** `/Users/gnijor/.cargo/git/checkouts/rust-sdk-762d172c5e846930/ec9ceae/src/agent-client-protocol-tokio/src/acp_agent.rs` — spawns subprocess on `connect_to`, kills on drop
- **Initialization tests:** `/Users/gnijor/.cargo/git/checkouts/rust-sdk-762d172c5e846930/ec9ceae/src/agent-client-protocol-conductor/tests/initialization_sequence.rs` — duplex+ByteStreams pattern
- **SACP design:** https://agentclientprotocol.github.io/symposium-acp/sacp-design.html — roles, ConnectTo, reactive vs active modes
- **Conductor spec:** https://agentclientprotocol.github.io/symposium-acp/conductor.html — initialization flow, proxy chains
- **STATE-PROTOCOL:** https://github.com/durable-streams/durable-streams/blob/main/packages/state/STATE-PROTOCOL.md
- **use-acp:** https://github.com/marimo-team/use-acp — React hooks for ACP, used by Flamecast
- **durable-streams client-rust:** https://github.com/durable-streams/durable-streams/tree/main/packages/client-rust — replaced our hand-rolled stream_subscriber

## Existing SDDs (in repo)

- `docs/architecture-redesign.md` — module naming, ConductorState as grab bag, dependency graph
- `docs/api-architecture-sdd.md` — REST is read-only + queue mgmt, all prompts through ACP
- `docs/transport-bridge-conductor-state-sdd.md` — transport bridge + ConductorState dissolution plan
- `docs/transport-bridge-design.md` (in flamecast repo) — duplex bridge design

## File Map

### Rust (durable-acp-rs)
| File | What it does | Problems |
|---|---|---|
| `src/conductor_state.rs` | God object: stream writing + queue + identity | Should be dissolved |
| `src/durable_state_proxy.rs` | ACP proxy: intercepts messages, writes state | Does too much, owns queue drain logic |
| `src/api.rs` | REST API + /acp WebSocket | Reaches through ConductorState for everything |
| `src/conductor.rs` | 27-line glue: `build_conductor(app, agent)` | Takes `Arc<ConductorState>` |
| `src/main.rs` | CLI entry, creates state, starts conductor | Wires everything together |
| `src/transport.rs` | WS/TCP/stdio transports | AxumWsTransport now handles binary frames |
| `src/state.rs` | StreamDb + row types + STATE-PROTOCOL envelope | Foundation, mostly clean |
| `src/stream_server.rs` | Embedded durable streams server | Clean |
| `src/stream_subscriber.rs` | SSE subscriber (now uses durable-streams client) | Recently rewritten |
| `src/prompt_queue.rs` | Extracted queue (from shelved branch) | File exists but unused on main |
| `src/peer_mcp.rs` | MCP tools proxy (list_agents, prompt_agent) | Clean |
| `src/client.rs` | Reusable ACP client | Clean |

### TypeScript (durable-acp-rs/ts/)
| File | What it does |
|---|---|
| `packages/server/src/react.tsx` | DurableStreamProvider — observation only |
| `packages/server/src/index.ts` | Server exports |
| `packages/durable-session/src/schema.ts` | Zod schema matching STATE-PROTOCOL |
| `packages/durable-session/src/collection.ts` | StreamDB factory |

### Flamecast (smithery/flamecast)
| File | What it does |
|---|---|
| `packages/ui/src/provider.tsx` | FlamecastProvider: DurableStreamProvider + useAcpClient + manual init |
| `packages/ui/src/index.ts` | Re-exports |
| `apps/client/src/routes/index.tsx` | Home page: checks ACP + stream connection |
| `apps/client/src/components/session-chat.tsx` | Chat UI: use-acp notifications + prompt |

## Test Coverage

82 tests pass. Key test files:
- `tests/conductor.rs` — full proxy chain e2e with Testy mock agent
- `tests/e2e_full_stack.rs` — ACP + durable stream observation
- `tests/state_stream.rs` — all 7 entity types round-trip through stream
- `tests/websocket_acp.rs` — WS endpoint with real agent binary
- `tests/integration.rs` — REST API + stream subscriptions
- `tests/transport_reconnect.rs` — shared state across conductor lifetimes (new)

## User Preferences

- **Always check SDK APIs first** before hand-rolling. Source of thrash all session.
- User wants proper component boundaries, not god objects.
- User wants to use established libraries (use-acp, durable-streams client) not hand-rolled implementations.
- User prefers TDD approach — write failing test first.
- User wants SDDs written before implementation.
