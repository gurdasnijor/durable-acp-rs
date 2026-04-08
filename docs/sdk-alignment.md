# SDD: SDK Alignment — Paved Roads First

> **Status: 🔄 IN PROGRESS**
> W2 ✅ Done, W3 ✅ Done, W4 🔄 In progress (session.connection() approach)

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

### 1.2 Package proxies as standalone binaries — ✅ DONE

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

### 1.3 Move dashboard to subprocess-per-agent — ✅ DONE

Dashboard spawns conductor subprocesses and communicates via
REST API + SSE (531 lines). Removed in-process `ConductorImpl`,
`AgentRouter`, `LocalSet` wiring. Deleted `src/agent_router.rs`.

### 1.4 Clean up stale code — ✅ DONE

These have been deleted:
- `src/agent_router.rs` — replaced by HTTP peering
- `src/bin/chat.rs` — removed
- `src/bin/run.rs` — removed
- `src/bin/peer.rs` — removed
- `AgentRouter`, `TuiState`, `HeadlessClient` — removed from dashboard

### 1.5 API Bypass Fix — 🔄 IN PROGRESS (session.connection() approach)

**Problem:** `POST /connections/{id}/prompt` bypasses the proxy chain.

**Fix:** Use `session.connection()` — the SDK's paved road. The
`ClientSideConnection` owns the session. `session.connection()` returns
the client-side `ConnectionTo` handle. Sending a `PromptRequest` through
it enters from the Client side → proxy chain fires → `DurableStateProxy`
records state automatically.

**Challenge:** `ClientSideConnection` is `!Send`, so it can't be shared
directly with the REST API (which runs on a multi-threaded tokio runtime).
The fix uses a channel bridge: the dashboard spawns a task on the
`LocalSet` that holds the connection and receives prompt requests from
the API via an `mpsc` channel.

```rust
// Channel bridge: API → dashboard LocalSet → session.connection()
let (prompt_tx, prompt_rx) = mpsc::channel(32);
api_state.set_prompt_sender(prompt_tx);

// On LocalSet thread:
while let Some(req) = prompt_rx.recv().await {
    session.connection()
        .send_request(PromptRequest::new(req.session_id, req.text))
        .block_task().await?;
}
```

**Delete after this fix:**
- `proxy_connection` from `AppState`
- `capture_proxy_connection` from `conductor.rs`
- All manual state recording from `api.rs`

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
