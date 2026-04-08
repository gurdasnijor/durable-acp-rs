# SDD: Pluggable Transports — Unified ConnectTo Model

> All participants — dashboard, editors, Flamecast, remote clients — are
> ACP Clients using the same `ConnectTo` abstraction. The conductor is
> an ACP Agent. Transport is just plumbing.

## Principle

From `index.md`: "All prompt submission goes through ACP. The REST API
is read-only state observation + queue management."

This means **every client** — dashboard TUI, Flamecast React UI, CLI,
remote automation — connects as an ACP Client via `ConnectTo`. The
conductor doesn't know or care which transport the client uses.

## What sacp Provides

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

## Server Side: Always Available

The conductor already runs a REST API server on `:port+1`. The WebSocket
ACP endpoint is just another route — no flag, no mode:

```rust
// In api.rs router — add alongside existing routes:
.route("/acp", get(ws_acp_handler))
```

The conductor accepts clients on **both** transports simultaneously:
- **stdio** — editors spawn it as a subprocess, connect via stdin/stdout
- **WebSocket at `/acp`** — remote clients connect via `ws://host:port+1/acp`

This works because the conductor's internal `ConductorMessage` queue
serializes all messages regardless of source — the same mechanism that
handles multiple MCP bridge connections (see `run_tcp_listener` in
`sacp-conductor`). No single-client constraint.

```bash
# All connections always available:
durable-acp-rs --port 4437 npx @agentclientprotocol/claude-agent-acp
# → stdio:           editor connects via stdin/stdout
# → ACP WebSocket:   ws://host:4438/acp (always available)
# → REST API:        http://host:4438/api/v1/* (always available)
# → Durable streams: http://host:4437/streams/* (always available)
```

## Client Side: Dashboard as ACP Client

The dashboard is an ACP Client. It should use the same `ConnectTo`
abstraction, not custom subprocess management:

```rust
// Current (custom glue, ~30 lines per agent):
let mut child = Command::new(&conductor_bin).stdin(piped).stdout(piped).spawn()?;
let outgoing = child.stdin.take().unwrap().compat_write();
let incoming = child.stdout.take().unwrap().compat();
let (conn, handle_io) = acp::ClientSideConnection::new(client, outgoing, incoming, spawn_fn);

// Target (ConnectTo, ~5 lines per agent):
let transport = resolve_transport(&config);
Client.builder()
    .name(&config.name)
    .connect_with(transport, |connection| {
        // Initialize, create session, prompt loop — same code for all transports
    })
    .await
```

### Transport resolution from agents.toml

```toml
# Local agent — spawns subprocess, connects via stdio
[[agent]]
name = "claude-local"
agent = "claude-acp"
# transport = "stdio"  (default, implicit)

# Remote agent — connects via WebSocket
[[agent]]
name = "claude-gpu"
transport = { type = "ws", url = "ws://gpu-server:4438/acp" }

# Remote agent — connects via TCP
[[agent]]
name = "claude-tcp"
transport = { type = "tcp", host = "10.0.0.5", port = 9000 }
```

```rust
fn resolve_transport(config: &AgentConfig) -> Box<dyn ConnectTo<Client>> {
    match &config.transport {
        None | Some(Transport::Stdio) => {
            // Default: spawn conductor subprocess, stdio
            let command = resolve_command(&config);
            Box::new(AcpAgent::from_args(command))
        }
        Some(Transport::Ws { url }) => {
            // WebSocket: connect to remote conductor
            Box::new(WebSocketTransport::new(url))
        }
        Some(Transport::Tcp { host, port }) => {
            // TCP: connect to remote conductor
            Box::new(TcpTransport::new(host, *port))
        }
    }
}
```

### WebSocket transport (new, ~30 lines)

```rust
struct WebSocketTransport { url: String }

impl ConnectTo<Client> for WebSocketTransport {
    async fn connect_to(self, client: impl ConnectTo<Agent>) -> Result<(), sacp::Error> {
        let (ws, _) = tokio_tungstenite::connect_async(&self.url).await?;
        let (write, read) = ws.split();
        ByteStreams::new(write, read).connect_to(client).await
    }
}
```

### TCP transport (new, ~15 lines)

```rust
struct TcpTransport { host: String, port: u16 }

impl ConnectTo<Client> for TcpTransport {
    async fn connect_to(self, client: impl ConnectTo<Agent>) -> Result<(), sacp::Error> {
        let stream = TcpStream::connect((self.host, self.port)).await?;
        let (read, write) = stream.into_split();
        ByteStreams::new(write.compat_write(), read.compat()).connect_to(client).await
    }
}
```

## The Full Picture

```
                    ┌─────────────────────────────────────┐
                    │         ACP Clients                  │
                    │  (all use ConnectTo abstraction)      │
                    │                                     │
                    │  Dashboard TUI    → stdio/Channel    │
                    │  Flamecast UI     → WebSocket        │
                    │  Editor (Zed)     → stdio            │
                    │  Remote script    → TCP/WebSocket    │
                    └──────────┬──────────────────────────┘
                               │
                    ConnectTo<Client> (any transport)
                               │
                    ┌──────────▼──────────────────────────┐
                    │     ConductorImpl                    │
                    │     (transport-agnostic)             │
                    │                                     │
                    │  DurableStateProxy → PeerMcpProxy   │
                    │            → Agent subprocess        │
                    └─────────────────────────────────────┘
```

Every client connects the same way. The conductor doesn't know if it's
stdio, WebSocket, TCP, or in-process channels. Transport is config.

## Flamecast Integration

Flamecast's runtime returns `websocketUrl`. The `/acp` endpoint is
always available — no special flags:

```typescript
// Flamecast runtime provider spawns:
//   durable-acp-rs --port 4437 npx claude-agent-acp
// Returns: { websocketUrl: "ws://host:4438/acp" }
// Flamecast connects via existing WebSocket mechanism
```

For Docker/E2B:
```dockerfile
CMD ["durable-acp-rs", "--port", "4437", "npx", "claude-agent-acp"]
EXPOSE 4437 4438
```

## Dashboard Optimization: Channel::duplex()

For in-process connections (dashboard spawning conductors), use
`sacp::Channel::duplex()` instead of `tokio::io::duplex` — zero
serialization, messages passed by value:

```rust
let (channel_client, channel_conductor) = sacp::Channel::duplex();
// Conductor: conductor.run(channel_conductor)
// Client: Client.builder().connect_with(channel_client, |cx| { ... })
```

## What Changes

| File | Change |
|---|---|
| `src/main.rs` | No changes — stdio always works, WebSocket is on API server |
| `src/api.rs` | Add `GET /acp` WebSocket upgrade route |
| `src/bin/dashboard.rs` | Replace custom subprocess glue with `resolve_transport()` + `ConnectTo` |
| `agents.toml` | Add optional `transport` field |
| New: `src/transport.rs` | `WebSocketTransport`, `TcpTransport`, `resolve_transport()` (~60 lines) |

## What Stays The Same

- `ConductorImpl` — unchanged, `run(transport)` is already generic
- `DurableStateProxy` — unchanged, transport-agnostic
- `PeerMcpProxy` — unchanged, HTTP peering is transport-independent
- `AppState`, `StreamDB`, REST API — unchanged
