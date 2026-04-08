# SDD: API Architecture — ACP-Compliant Client Model

> **Status: ✅ IMPLEMENTED** (PR #2)
>
> `submit_prompt`, `cancel_turn`, `proxy_connection`, `capture_proxy_connection`
> all deleted. REST API is read-only + queue management.

## Problem (solved) It stashes a `proxy_connection` (the conductor's internal handle)
and calls `cx.send_request_to(Agent, ...)` directly, skipping
`DurableStateProxy`'s handlers. This requires manual state recording
(~50 lines) that duplicates what the proxy already does.

This is architecturally wrong. The [P/ACP spec](https://agentclientprotocol.com/rfds/proxy-chains)
defines one client per conductor:

> From the editor's perspective, it spawns one conductor process and
> communicates using normal ACP over stdio. The editor doesn't know
> about the proxy chain.

The conductor accepts **one ACP client** on its transport (stdio, WS, TCP).
All messages from that client flow through the proxy chain. There is no
"inject a prompt from the side" API — that's not how ACP works.

## Current Architecture (wrong)

```
Editor/Dashboard ──ACP (stdio)──► Conductor ──proxy chain──► Agent
                                      │
REST API ──proxy_connection.send_to(Agent)──► Agent directly
               ↑ BYPASSES proxy chain
               ↑ Manual state recording
               ↑ ~50 lines of duplicated logic
```

## Target Architecture

```
Editor/Dashboard ──ACP (stdio)──► Conductor ──proxy chain──► Agent
                                      │
REST API ──read-only──► StreamDB     (connections, chunks, queue, files, terminals)
         ──queue mgmt──► AppState    (pause, resume, cancel, reorder)
```

### Principle: All Prompt Submission Goes Through ACP

- **Dashboard TUI** → `conn.prompt()` on `ClientSideConnection` → stdio → conductor → proxy chain
- **Flamecast React UI** → `DurableACPClient.prompt()` → ACP over WebSocket → conductor → proxy chain
- **CLI client** → `Client.builder().connect_with()` → stdio → conductor → proxy chain
- **REST API** → **does not submit prompts**. Read-only state + queue management.

### Why No REST Prompt Endpoint?

1. **Proxy chain bypass** — the conductor's proxy chain is the core value
   (durable state, MCP tools, context injection). Bypassing it means no
   durability, no peering, no tool injection.

2. **Duplicated state recording** — the bypass requires manual
   `write_state_event`, `record_chunk`, `finish_prompt_turn` in the API
   handler, duplicating what `DurableStateProxy` already does.

3. **Not ACP-compliant** — the ACP protocol defines prompt submission as
   a `PromptRequest` message on the ACP connection. HTTP POST is not
   an ACP message.

4. **Single client constraint** — the conductor accepts one client. The
   REST API can't be a second client without multiplexing.

### What the REST API IS For

The REST API serves **state observation and queue management** — things
that don't need to flow through the ACP proxy chain:

**Read-only (from StreamDB):**
- `GET /connections` — list agents/sessions
- `GET /connections/{id}/queue` — queue state
- `GET /prompt-turns/{id}/stream` — SSE stream chunks
- `GET /prompt-turns/{id}/chunks` — all chunks
- `GET /connections/{id}/files` — workspace files
- `GET /connections/{id}/fs/tree` — directory listing
- `GET /registry` — peer agents

**Queue management (AppState):**
- `POST /connections/{id}/queue/pause` — pause drain loop
- `POST /connections/{id}/queue/resume` — resume drain loop
- `DELETE /connections/{id}/queue/{id}` — cancel queued item
- `DELETE /connections/{id}/queue` — clear queue
- `PUT /connections/{id}/queue` — reorder

**Terminal management:**
- `POST /connections/{id}/terminals` — create
- `POST /connections/{id}/terminals/{id}/input` — send input
- `GET /connections/{id}/terminals/{id}/output` — SSE output
- `DELETE /connections/{id}/terminals/{id}` — kill

### How Clients Submit Prompts

Each client type connects as an ACP client:

**Dashboard (TUI):**
```rust
// Already works — ClientSideConnection over subprocess stdio
let (conn, handle_io) = acp::ClientSideConnection::new(client, outgoing, incoming, spawn_fn);
conn.initialize(...).await?;
conn.new_session(...).await?;
conn.prompt(PromptRequest::new(session_id, text)).await?;
// → conductor → DurableStateProxy records state → agent
```

**Flamecast React UI:**
```typescript
// DurableACPClient subscribes to state stream for reads
const client = new DurableACPClient({ stateStreamUrl, serverUrl });

// Prompt submission via ACP WebSocket (not REST)
// Flamecast's AcpBridge already does this — it connects to the
// conductor's stdio via the runtime provider
```

**External automation / scripts:**
```bash
# Use the conductor binary directly as an ACP agent
echo '{"method":"session/prompt","params":{...}}' | durable-acp-rs npx claude-agent-acp
```

## What Changes

### Delete from `api.rs`

```rust
// DELETE these (~60 lines):
async fn submit_prompt(...)  // entire function
async fn cancel_turn(...)    // uses proxy_connection

// DELETE route registrations:
.route("/api/v1/connections/{id}/prompt", post(submit_prompt))
.route("/api/v1/connections/{id}/cancel", post(cancel_turn))
```

### Delete from `app.rs`

```rust
// DELETE these fields + methods:
pub proxy_connection: Arc<Mutex<Option<ConnectionTo<Conductor>>>>
pub async fn capture_proxy_connection(...)
```

### Delete from `conductor.rs`

```rust
// DELETE these lines (capture_proxy_connection calls):
app.capture_proxy_connection(cx.clone()).await;  // line 33
app.capture_proxy_connection(cx.clone()).await;  // line 48
```

### Keep in `api.rs`

Everything else stays — it's all read-only or queue management:
- `list_connections`, `get_queue`, `pause_queue`, `resume_queue`
- `stream_prompt_turn`, `get_chunks`
- `get_registry`
- `get_file`, `get_fs_tree`
- `create_terminal`, `terminal_input`, `terminal_output`, `kill_terminal`
- Queue CRUD: `cancel_queued_turn`, `clear_queue`, `reorder_queue`

### Cancel via queue management

`cancel_turn` currently sends `CancelNotification` through `proxy_connection`.
After removing the endpoint, cancellation is handled via:
- Queue management: `DELETE /connections/{id}/queue/{turnId}` for queued items
- ACP client: `conn.cancel()` from the connected client (dashboard)
- The `DurableStateProxy` handles the ACP `CancelNotification` and updates state

## Impact

| Metric | Before | After |
|---|---|---|
| `api.rs` | 508 lines | ~440 lines |
| `app.rs` | 328 lines | ~310 lines |
| `conductor.rs` | 278 lines | ~274 lines |
| Manual state recording | ~50 lines in submit_prompt | 0 |
| `proxy_connection` field | exists | deleted |
| `capture_proxy_connection` | exists | deleted |
| Prompt paths | 2 (ACP + REST bypass) | 1 (ACP only) |

## Migration for Existing REST Clients

If any external client currently uses `POST /connections/{id}/prompt`,
they need to switch to ACP:

**Option 1: Connect as ACP client**
```typescript
const conn = new ClientSideConnection(client, transport);
conn.initialize(...);
conn.prompt(...);
```

**Option 2: Use DurableACPClient**
```typescript
const client = new DurableACPClient({ stateStreamUrl, serverUrl });
client.prompt(text);  // routes through ACP internally
```

**Option 3: Keep the endpoint as deprecated**
If backwards compatibility is needed, keep `submit_prompt` with the
`proxy_connection` bypass but mark it as deprecated:
```rust
/// DEPRECATED: Use ACP client connection for prompt submission.
/// This endpoint bypasses the proxy chain and manually records state.
async fn submit_prompt(...) { ... }
```

## Relationship to Other SDDs

- **`sdk-alignment.md`** — W4 (API bypass fix) is superseded by this SDD.
  Instead of fixing the bypass, we remove it.
- **`known-limitations-sdd.md` §2** — resolved by deletion, not by fixing.
- **`flamecast-integration-sdd.md`** — Flamecast's `AcpBridge` already
  connects as an ACP client. No REST prompt endpoint needed.
- **`electric-sync-sdd.md`** — `DurableACPClient` subscribes to state
  stream for reads. Prompt submission is via ACP, not REST.
