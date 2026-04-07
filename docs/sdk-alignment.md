# SDD: SDK Alignment — Paved Roads First

> **Status: 🔜 READY FOR EXECUTION — Phase 1 priority**
> Total effort: ~2-3 days

## Problem

The current implementation uses low-level SDK APIs and a custom in-process
multi-agent model that adds ~600 lines of complexity for negligible
performance gain. The ACP ecosystem provides higher-level abstractions
and a standard subprocess-per-agent pattern we should adopt.

## What to Change

### 1.1 Migrate to `sacp-proxy` v3.0.0 (~1-2 days)

**Current:** `conductor.rs` uses `sacp::Proxy.builder()` with 6 manual
handler registrations (~400 lines).

**Target:** Use `JrHandlerChain` + `ProxyHandler` from `sacp-proxy` v3.0.0
(~150 lines):

```rust
// DurableStateProxy as standalone binary
use sacp::JrHandlerChain;
use sacp_proxy::{AcpProxyExt, McpServiceRegistry};

JrHandlerChain::new()
    .name("durable-state")
    .on_request_from_client::<PromptRequest>(|req, request_cx, cx| async {
        // enqueue, drive queue, respond
    })
    .on_notification_from_agent::<SessionNotification>(|notif, cx| async {
        // record chunks
    })
    .on_request_from_agent::<RequestPermissionRequest>(|req, request_cx, cx| async {
        // record permission, forward
    })
    .provide_mcp(McpServiceRegistry::default())
    .proxy()
    .serve(sacp::ByteStreams::new(stdout, stdin))
    .await
```

**What `ProxyHandler` gives for free:**
- `InitializeProxyRequest` handling (accept proxy capability, forward)
- `NewSessionRequest` session tracking (add dynamic handler per session)
- Default message forwarding (unhandled → pass through)
- `_proxy/successor/*` envelope wrapping/unwrapping
- MCP-over-ACP bridge

**Files:**
- `src/conductor.rs` — rewrite `DurableStateProxy` using `JrHandlerChain`
- `Cargo.toml` — add `sacp-proxy = "3.0.0"`

### 1.2 Package proxies as standalone binaries (~0.5 day)

Create two new binaries that run as conductor subprocesses:

```
src/bin/durable-state-proxy.rs   # DurableStateProxy over stdio
src/bin/peer-mcp-proxy.rs        # PeerMcpProxy over stdio
```

Each is ~30 lines — parse CLI args, create the proxy, `.serve(stdio)`:

```rust
// src/bin/durable-state-proxy.rs
#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let streams = EmbeddedDurableStreams::connect(cli.stream_url).await?;
    let app = AppState::with_shared_streams(streams).await?;

    JrHandlerChain::new()
        .name("durable-state")
        .on_request_from_client::<PromptRequest>(/* ... */)
        .on_notification_from_agent::<SessionNotification>(/* ... */)
        .proxy()
        .serve(sacp::ByteStreams::new(stdout, stdin))
        .await
}
```

These work with the standard `sacp-conductor` binary:

```bash
sacp-conductor agent \
  "durable-state-proxy --stream-url http://localhost:4437/durable-acp-state" \
  "peer-mcp-proxy" \
  "npx @agentclientprotocol/claude-agent-acp"
```

**Files:**
- `src/bin/durable-state-proxy.rs` — new
- `src/bin/peer-mcp-proxy.rs` — new
- `Cargo.toml` — add `[[bin]]` entries

### 1.3 Move dashboard to subprocess-per-agent (~1 day)

**Current:** Dashboard spawns N `ConductorImpl` instances in-process
on one `LocalSet` with `Arc<Mutex<TuiState>>`, `AgentRouter`, bridge
tasks, tick timer (~794 lines).

**Target:** Dashboard spawns N conductor subprocesses and talks to their
REST APIs (~200 lines):

```rust
// Spawn conductor subprocess per agent
for config in agents {
    let child = Command::new("sacp-conductor")
        .args(["agent",
            &format!("durable-state-proxy --stream-url {}", stream_url),
            "peer-mcp-proxy",
            &config.command.join(" "),
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .kill_on_drop(true)
        .spawn()?;

    // Register in peer registry
    registry::register(AgentEntry { name, api_url, ... });
}

// TUI polls REST API for state, submits prompts via HTTP
element!(Dashboard(agents: agent_configs)).fullscreen().await
```

The TUI component becomes a pure REST client:
- `GET /connections` → agent status
- `POST /connections/{id}/prompt` → send prompt
- `GET /prompt-turns/{id}/stream` → SSE stream response
- `GET /registry` → peer list

No `LocalSet`, no `Arc<Mutex>`, no channels, no `AgentRouter`.

**Files:**
- `src/bin/dashboard.rs` — rewrite (~200 lines)
- Delete `src/agent_router.rs`
- `src/lib.rs` — remove `pub mod agent_router`

### 1.4 Clean up stale code

After 1.1-1.3, these are deletable:

| File/Code | Reason |
|---|---|
| `src/agent_router.rs` | Replaced by HTTP peering |
| `AgentRouter` global `OnceLock` | No in-process routing |
| `HeadlessClient` in dashboard | No in-process ACP client |
| `TuiState` with `Arc<Mutex>` | TUI reads REST API directly |
| `run_agent()` function | Replaced by subprocess spawn |
| `Output` enum, `AgentHandle` struct | Dead code |
| `run.rs` v0 `ClientSideConnection` usage | Use subprocess model |

### 1.5 Remove manual JSON deserialization workaround

Both `chat.rs` and `dashboard.rs` manually deserialize `SessionNotification`
via `serde_json::from_value` to skip unknown variants. With
`unstable_session_usage` enabled, test whether `MatchDispatch::if_notification`
works directly. If yes, remove the workaround (~30 lines per file).

### Impact on other files

**`src/app.rs` (~284 lines → ~240)**
- Delete `proxy_connection: Arc<Mutex<Option<ConnectionTo<Conductor>>>>` — no API bypass
- Delete `set_proxy_connection()` — no longer called
- Delete `set_next_prompt_turn_id()` — no pre-generation needed
- Core stays: `enqueue_prompt`, `record_chunk`, `finish_prompt_turn`, `write_state_event`, queue management

**`src/api.rs` (~315 lines → ~150)**
- `submit_prompt` drops from 63 to ~10 lines — forward to conductor subprocess, no bypass hack
- `cancel_turn` simplifies — no `proxy_connection`
- `stream_prompt_turn` replaced by `SseSubscriber` when W7 lands
- Delete all `proxy_connection` usage
- Simple read endpoints (`list_connections`, `get_queue`, `get_chunks`, `get_registry`) stay unchanged

**`src/peer_mcp.rs` (~230 lines → merged into proxy config)**
- `McpServiceRegistry` + `McpTool` impls replace manual `ConnectTo<Conductor>` + `McpServer::builder()`
- `call_peer` HTTP helper stays (used for cross-process peering)

## Verification

After Phase 1:

```bash
# Standard conductor config works
sacp-conductor agent \
  "durable-state-proxy --stream-url ..." \
  "peer-mcp-proxy" \
  "npx @agentclientprotocol/claude-agent-acp"

# Dashboard spawns subprocesses, TUI works
cargo run --bin dashboard

# Agent-to-agent peering works (HTTP)
# Agent A calls prompt_agent("agent-b", "hello") → HTTP → Agent B responds

# Standalone conductor works for editors
cargo run --bin durable-acp-rs -- npx @agentclientprotocol/claude-agent-acp
```

## Why This Order

1. **sacp-proxy migration (1.1)** — reduces conductor.rs, makes 1.2 trivial
2. **Standalone binaries (1.2)** — enables 1.3 (conductor config)
3. **Subprocess dashboard (1.3)** — simplifies dashboard, enables cleanup
4. **Cleanup (1.4)** — remove dead code after architecture change
5. **JSON workaround (1.5)** — minor, do last
