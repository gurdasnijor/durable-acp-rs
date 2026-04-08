# SDD: Pluggable Transports

> Uses sacp v11's built-in transport abstractions. No custom transport code needed.

## What sacp v11 Already Provides

Three transport types, all implementing `ConnectTo<R>`:

```rust
// 1. Byte streams — stdio, TCP, WebSocket, any AsyncRead+AsyncWrite
sacp::ByteStreams::new(write_half, read_half)

// 2. In-process channels — zero serialization, message passing
let (channel_a, channel_b) = sacp::Channel::duplex();

// 3. Line streams — line-based with interception (debugging)
sacp::Lines::new(outgoing_sink, incoming_stream)
```

`ConductorImpl::run(transport: impl ConnectTo<Host>)` accepts any of them.
No conductor code changes needed for any transport.

## Implementation

### CLI

```rust
#[derive(Debug, Parser)]
struct Cli {
    #[arg(long, default_value_t = 4437)]
    port: u16,
    /// Listen for ACP client connections instead of using stdio.
    /// Accepts WebSocket connections on the REST API server.
    #[arg(long)]
    listen: bool,
    #[arg(long, default_value = "default")]
    name: String,
    #[arg(trailing_var_arg = true, required = true)]
    agent_command: Vec<String>,
}
```

### main.rs — two modes

```rust
let conductor = build_conductor(app, agent);

if cli.listen {
    // Network mode: accept WebSocket on the REST API server
    // Add ws://host:{api_port}/acp endpoint
    // When client connects, bridge WebSocket → ByteStreams → conductor
    let (channel_client, channel_conductor) = sacp::Channel::duplex();

    // Spawn WebSocket acceptor on the API server
    tokio::spawn(accept_ws_client(api_port, channel_client));

    // Run conductor on the channel
    conductor.run(channel_conductor).await
} else {
    // Local mode: stdio (default, editors spawn us)
    conductor.run(ByteStreams::new(stdout, stdin)).await
}
```

### WebSocket endpoint (on existing API server)

Add to the REST API router (already axum):

```rust
use axum::extract::ws::{WebSocket, WebSocketUpgrade};

// In api.rs router:
.route("/acp", get(ws_acp_handler))

async fn ws_acp_handler(
    ws: WebSocketUpgrade,
    State(channel_tx): State<mpsc::Sender<WebSocket>>,
) -> impl IntoResponse {
    ws.on_upgrade(|socket| async {
        channel_tx.send(socket).await.ok();
    })
}
```

The WebSocket is bridged to a `sacp::Channel` which the conductor
consumes. ACP JSON-RPC messages flow over the WebSocket frames.

### Dashboard — use Channel::duplex() instead of tokio::io::duplex

```rust
// Current (byte-level duplex, serialization overhead):
let (client_out, conductor_in) = tokio::io::duplex(64 * 1024);
let transport = ByteStreams::new(client_out.compat_write(), client_in.compat());

// Target (message-level duplex, zero serialization):
let (channel_client, channel_conductor) = sacp::Channel::duplex();
// conductor uses channel_conductor
// ClientSideConnection uses channel_client
```

## Usage

```bash
# Local (default) — editor spawns, connects via stdio
durable-acp-rs npx @agentclientprotocol/claude-agent-acp

# Network — accepts WebSocket client on API server
durable-acp-rs --listen --port 4437 npx @agentclientprotocol/claude-agent-acp
# → REST API at http://host:4438/api/v1/*
# → ACP WebSocket at ws://host:4438/acp
# → Durable streams at http://host:4437/streams/*
```

## Flamecast Integration

Flamecast's runtime provider returns `websocketUrl`. With `--listen`:

```typescript
// Flamecast runtime provider
const session = await runtime.startSession({
  command: "durable-acp-rs",
  args: ["--listen", "--port", port, "npx", "@agentclientprotocol/claude-agent-acp"],
});
// Returns: { websocketUrl: `ws://host:${port+1}/acp` }
// Flamecast connects via existing WebSocket mechanism
```

For Docker/E2B:
```dockerfile
CMD ["durable-acp-rs", "--listen", "--port", "4437", "npx", "@agentclientprotocol/claude-agent-acp"]
EXPOSE 4437 4438
```

## What Changes

| File | Change |
|---|---|
| `src/main.rs` | Add `--listen` flag, branch on stdio vs WebSocket |
| `src/api.rs` | Add `GET /acp` WebSocket route |
| `Cargo.toml` | Add `axum-extra` for WebSocket support (or use axum's built-in) |
| `src/bin/dashboard.rs` | Optional: switch from `tokio::io::duplex` to `Channel::duplex()` |

## What Stays The Same

- `ConductorImpl` — unchanged, `run(transport)` is already generic
- `DurableStateProxy` — unchanged, doesn't know about transport
- `PeerMcpProxy` — unchanged, HTTP peering is transport-independent
- `AppState`, `StreamDB` — unchanged
- REST API read endpoints — unchanged
