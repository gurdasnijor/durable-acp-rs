# Spike 3 Handoff: Flamecast as Control Plane

> Read `docs/spike-3-flamecast-control-plane-sdd.md` for the full spike spec.
> This document orients a fresh session on what exists, what's proven, and where to start.

## What just shipped (Spikes 1 + 2)

`durable-acp-rs` is now a clean hosted conductor runtime:

- **ACP data plane**: `/acp` WebSocket endpoint in `src/acp_server.rs`
- **Durable state**: TraceEvents written to durable stream via SDK `.trace_to()`, materialized by `StreamDb` in `src/state.rs`
- **Product API**: filesystem + registry + debug state endpoint in `src/api.rs`
- **Stream server**: durable-streams-server on the main port (e.g., 4437)
- **API server**: product API + ACP hosting on port+1 (e.g., 4438)
- **Browser client proven**: `ts/packages/spike2-client/` connects via `@agentclientprotocol/sdk` `ClientSideConnection` using JSON text frames over WebSocket

### Three endpoints a consumer needs

| Endpoint | Port | Purpose |
|---|---|---|
| `ws://{host}:{port+1}/acp` | 4438 | ACP WebSocket (initialize, session/new, prompt) |
| `http://{host}:{port}/streams/durable-acp-state` | 4437 | Durable event stream (SSE with `?live=sse`) |
| `http://{host}:{port+1}/api/v1/...` | 4438 | Helper API (files, registry, state snapshot) |

### How the browser client works

See `ts/packages/spike2-client/src/app.tsx` — the pattern is identical to Flamecast's existing `use-durable-acp-session.ts`:

```ts
const connection = new acp.ClientSideConnection(clientHandler, createWebSocketStream(ws));
await connection.initialize({ protocolVersion: acp.PROTOCOL_VERSION, ... });
const session = await connection.newSession({ cwd: "/", mcpServers: [] });
await connection.prompt({ sessionId, prompt: [{ type: "text", text }] });
```

Flamecast already has this pattern in `packages/ui/src/hooks/use-durable-acp-session.ts`.

## What Spike 3 needs to prove

Can Flamecast consume `durable-acp-rs` as an external ACP substrate using only:
- ACP for command/control
- Durable stream for observation
- Helper API for filesystem

Without falling back to the legacy session-host `/start` + `/prompt` + `/terminate` REST contract.

## Where to look in Flamecast

| Concern | Current location | What needs to change |
|---|---|---|
| Runtime/provider abstraction | `packages/flamecast/src/flamecast/api.ts` | Store ACP endpoint + stream URL as first-class session metadata |
| ACP client path | `packages/ui/src/hooks/use-durable-acp-session.ts` | Already works — this IS the pattern. Wire it as the primary path, not a fallback. |
| State observation | `packages/ui/src/hooks/use-session-state.ts` | **Preferred**: subscribe to the durable stream directly. `/api/v1/state` is debug-only fallback. |
| Session metadata | Session records in Flamecast DB | Add `acpEndpoint`, `stateStreamUrl`, `helperApiUrl` fields. **Source of truth lives on the session record**, not on a runtime/provider record — sessions are the durable object per the architecture SDD. Runtime metadata may also reference these URLs but they are derived, not authoritative. |

### Canonical success path

The session/Agents flow is the canonical path for Spike 3. The runtime page is **not** the success criterion. If runtime UI continues to work that's a bonus, but the spike succeeds when:

1. A user creates a session backed by durable-acp-rs
2. The session view uses ACP for prompts and the durable stream for state
3. No legacy session-host REST appears in the happy path

## What NOT to change in durable-acp-rs

No Rust changes are expected for the happy path of Spike 3. The hosted conductor surface is sufficient as-is.

If Flamecast integration exposes a real gap (e.g., a missing helper field or a session-metadata endpoint), small Rust tweaks are fine — but treat them as scope additions, not the default mode.

## Out of scope for Spike 3

To prevent scope creep, these are explicitly NOT part of this spike:

- Runtime page redesign or runtime-list UX work
- Same-session reconnect continuation (reconnecting to an existing ACP session after WS drop)
- Worker crash recovery via `load_session` replay (that's Spike 6)
- Runtime/environment snapshotting (Spike 7)
- Multi-tenant or auth-related Flamecast surface changes
- Cross-process peer MCP refactoring

## Suggested implementation order

1. **Add ACP/stream metadata to Flamecast session records** — store the three URLs when creating a session backed by durable-acp-rs
2. **Wire `use-durable-acp-session.ts` as the primary client path** — it already uses `ClientSideConnection` against a WS endpoint
3. **Wire durable state into the session view** — either subscribe to the stream directly or poll `/api/v1/state` for the spike
4. **Verify the happy path without legacy REST** — initialize → session → prompt → response → state visible

## Key gotchas from Spike 2

- The `@agentclientprotocol/sdk` uses JSON text frames over WebSocket, NOT binary. The `use-acp` package uses binary frames and is incompatible with our transport. Use `ClientSideConnection` directly.
- Each WS connection spawns a new conductor + agent subprocess. Disconnecting kills it. "Reconnect" means new session with old state visible from the stream.
- `cwd` and `session_id` materialize from TraceEvents — filesystem helpers need the session to be established before they work.
- The durable stream accumulates across sessions. Filter by `startedAt` timestamp or session_id for current-session views.

## Success looks like

A Flamecast user can:
1. Create a session backed by durable-acp-rs
2. Send prompts and see responses (via ACP, not REST)
3. See session history/state (via durable stream, not legacy polling)
4. Browse files (via helper API)
5. No legacy `/start` or `/prompt` REST calls in the happy path
