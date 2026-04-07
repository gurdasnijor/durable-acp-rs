# SDD: Unified Event Subscribers — WebSocket + Webhooks

> **Status: 🔜 READY FOR EXECUTION**
> - Implements [multi-session WebSocket RFC](https://flamecast.mintlify.app/rfcs/multi-session-websocket)
> - Implements [webhooks RFC](https://flamecast.mintlify.app/rfcs/webhooks)
> - Estimated: ~2-3 days total
>   - `EventSubscriber` trait + `SubscriberManager`: ~0.5 day
>   - `WsSubscriber` (Flamecast channel protocol): ~1-1.5 days
>   - `WebhookSubscriber` (HTTP POST + HMAC): ~0.5 day
>   - Generalized SSE endpoints: ~0.5 day

## Insight

The [multi-session WebSocket RFC](https://flamecast.mintlify.app/rfcs/multi-session-websocket)
and [webhooks RFC](https://flamecast.mintlify.app/rfcs/webhooks) describe
the same fundamental primitive: **subscribe to session events, dispatch
them somewhere**.

- WebSocket: dispatch events to a connected client over a persistent connection
- Webhook: dispatch events to an HTTP endpoint on each change
- SSE: dispatch events to an HTTP client via Server-Sent Events (we already have this)

All three are subscribers on the same event source: `StreamDb::subscribe_changes()`.

## Design: Subscriber Trait

```rust
/// A subscriber receives session events and dispatches them somewhere.
#[async_trait]
trait EventSubscriber: Send + Sync {
    /// Called for each event that matches the subscriber's filter.
    async fn dispatch(&self, event: &SessionEvent) -> Result<()>;

    /// Called when the subscriber should shut down.
    fn close(&self);
}

struct SessionEvent {
    session_id: String,
    agent_id: String,
    seq: u64,
    event_type: String,           // "chunk", "prompt_turn", "permission", "terminal", etc.
    data: serde_json::Value,
    timestamp: i64,
}
```

### Three implementations, one subscriber loop

```rust
/// WebSocket subscriber — dispatches events over a persistent WS connection
struct WsSubscriber {
    sink: SplitSink<WebSocketStream, Message>,
    channels: HashSet<String>,     // subscribed channels ("session:abc", "agents", etc.)
}

/// Webhook subscriber — POSTs events to an HTTP endpoint
struct WebhookSubscriber {
    url: String,
    secret: Option<String>,        // HMAC signing
    filter: EventFilter,           // which event types to send
    client: reqwest::Client,
}

/// SSE subscriber — streams events to an HTTP response (already exists in api.rs)
struct SseSubscriber {
    tx: mpsc::Sender<Event>,
    filter: EventFilter,
}
```

### Subscriber Manager

```rust
struct SubscriberManager {
    subscribers: Vec<Box<dyn EventSubscriber>>,
    rx: broadcast::Receiver<CollectionChange>,  // from StreamDb
}

impl SubscriberManager {
    async fn run(&mut self) {
        while let Ok(change) = self.rx.recv().await {
            let events = self.materialize_events(&change).await;
            for event in &events {
                for subscriber in &self.subscribers {
                    subscriber.dispatch(event).await.ok();
                }
            }
        }
    }
}
```

## WebSocket — Matching the Flamecast Protocol

The [multi-session WebSocket RFC](https://flamecast.mintlify.app/rfcs/multi-session-websocket)
defines a channel-based multiplexing protocol. Map it to our subscriber model:

### Server → Client (read path — subscriber dispatches)

| Flamecast WS Message | Source in durable-acp-rs |
|---|---|
| `connected` | WebSocket open handler |
| `subscribed` / `unsubscribed` | Channel management on WsSubscriber |
| `event` (session chunk, tool call, etc.) | `StreamDb::subscribe_changes()` → `CollectionChange::Chunks` |
| `session.created` | `CollectionChange::Connections` (state = "created") |
| `session.terminated` | `CollectionChange::Connections` (state = "closed") |

### Client → Server (write path — commands)

| Flamecast WS Action | Handler in durable-acp-rs |
|---|---|
| `subscribe` / `unsubscribe` | Add/remove channels on WsSubscriber |
| `prompt` | Route to agent's `prompt_tx` channel (in-process) or REST API |
| `permission.respond` | Route to permission `perm_tx` channel |
| `cancel` | Send `CancelNotification` via conductor connection |
| `terminate` | Drop conductor task |
| `queue.*` | `AppState::set_paused()`, queue manipulation |
| `terminal.*` | Forward to agent via conductor |
| `ping` / `pong` | Keepalive |

### Implementation

```rust
// In api.rs, add WebSocket endpoint
async fn ws_handler(
    ws: WebSocketUpgrade,
    State(app): State<Arc<AppState>>,
    State(manager): State<Arc<AgentManager>>,
) -> impl IntoResponse {
    ws.on_upgrade(|socket| handle_ws(socket, app, manager))
}

async fn handle_ws(
    socket: WebSocket,
    app: Arc<AppState>,
    manager: Arc<AgentManager>,
) {
    let (sink, stream) = socket.split();
    let subscriber = WsSubscriber::new(sink);

    // Read commands from client
    tokio::spawn(async move {
        while let Some(msg) = stream.next().await {
            match parse_action(msg) {
                Action::Subscribe(channel) => subscriber.subscribe(channel),
                Action::Prompt { session_id, text } => {
                    manager.send_prompt(&session_id, text).await;
                }
                Action::PermissionRespond { session_id, request_id, body } => {
                    manager.resolve_permission(&session_id, &request_id, body).await;
                }
                // ... other actions
            }
        }
    });

    // Register subscriber for event dispatch
    manager.add_subscriber(Box::new(subscriber));
}
```

## Webhooks — Same Subscriber, HTTP Dispatch

The [webhooks RFC](https://flamecast.mintlify.app/rfcs/webhooks) is even
simpler — it's a subscriber that POSTs events:

```rust
impl EventSubscriber for WebhookSubscriber {
    async fn dispatch(&self, event: &SessionEvent) -> Result<()> {
        if !self.filter.matches(event) {
            return Ok(());
        }

        let payload = serde_json::to_string(event)?;

        let mut req = self.client
            .post(&self.url)
            .header("Content-Type", "application/json")
            .body(payload);

        // HMAC signing (if secret configured)
        if let Some(secret) = &self.secret {
            let signature = hmac_sha256(secret, &payload);
            req = req.header("X-Webhook-Signature", signature);
        }

        req.send().await?;
        Ok(())
    }
}
```

### Webhook registration

```rust
// REST API
POST /api/v1/webhooks
{
    "url": "https://my-service.com/webhook",
    "events": ["chunk", "prompt_turn.completed", "permission"],
    "sessions": ["*"],  // or specific session IDs
    "secret": "optional-hmac-secret"
}

GET  /api/v1/webhooks          → list registered webhooks
DELETE /api/v1/webhooks/:id    → unregister
```

### In agents.toml

```toml
[[webhook]]
url = "https://slack.com/webhook/..."
events = ["prompt_turn.completed"]

[[webhook]]
url = "https://my-dashboard.com/events"
events = ["*"]
secret = "hmac-secret-123"
```

## SSE — Already a Subscriber

Our existing `GET /api/v1/prompt-turns/{id}/stream` is already a subscriber.
It subscribes to `StreamDb::subscribe_changes()` and dispatches via SSE.
Generalize it:

```rust
// Session-scoped SSE (all events for a session)
GET /api/v1/agents/:id/stream?since=42

// Global SSE (all events for all sessions)
GET /api/v1/stream?since=0
```

## The Unified Model

```
StreamDb::subscribe_changes()
         │
         ▼
  SubscriberManager
         │
    ┌────┼────────────────┐
    │    │                │
    ▼    ▼                ▼
  WsSubscriber   WebhookSubscriber   SseSubscriber
  (persistent    (HTTP POST per      (HTTP SSE
   connection)    event)              stream)
```

All three read from the same broadcast channel. All three filter by
session/event type. All three deliver the same `SessionEvent` payload.
The only difference is the transport.

## Relationship to `DurableACPClient` (TypeScript)

This SDD is the **server side**. The `DurableACPClient` from
`~/gurdasnijor/distributed-acp/packages/durable-acp-client/` is the
**client side**. They're two halves of the same system:

```
DurableACPClient (TS)              durable-acp-rs (Rust)
─────────────────────              ─────────────────────
                                   StreamDb::subscribe_changes()
                                          │
                                   SubscriberManager
                                   ┌──────┼──────────┐
subscribes via WS  ◄────────────── WsSubscriber      │
subscribes via SSE ◄──────────────────────────── SseSubscriber
                                          │
Slack/PagerDuty    ◄────────────── WebhookSubscriber
```

The `DurableACPClient` currently supports SSE (read-only) and HTTP POST
(commands). The WsSubscriber adds the WebSocket transport that Flamecast's
React UI expects — bidirectional, with channel multiplexing, prompt
submission, and permission resolution over one connection.

**Dependency chain:**
1. This SDD → implements WsSubscriber (Flamecast's WS channel protocol)
2. `DurableACPClient` → connects via WS (already has `WsTransport` class)
3. Flamecast React UI → uses `DurableACPClient` hooks
4. Flamecast cuts event bus + custom WS server

Without this SDD, Flamecast can only use SSE (read-only). With it,
Flamecast gets the full bidirectional protocol it already expects.

## What This Gives Flamecast

1. **Drop the custom WebSocket server** — use the unified subscriber model
2. **Drop the custom event bus** — `StreamDb::subscribe_changes()` is the bus
3. **Webhooks for free** — same subscriber, different dispatch
4. **SSE for free** — already working, just generalize the filter
5. **Replay from offset** — durable stream supports `?since=N`, all three
   subscriber types can replay missed events

## Alignment with ACP Proxy Chains RFD

The [Proxy Chains RFD](https://agentclientprotocol.com/rfds/proxy-chains)
defines proxies as composable message interceptors for context injection,
tool coordination, response filtering, and session orchestration. It does
NOT cover observability, event streaming, or webhooks — those are outside
the proxy's responsibility.

Our architecture respects this boundary:

```
ACP Proxy Chain (per spec)              Subscriber Layer (our addition)
──────────────────────────              ────────────────────────────────
DurableStateProxy                       WebSocket subscriber
  intercepts → persists to stream ───►  Webhook subscriber
PeerMcpProxy                            SSE subscriber
  intercepts → injects MCP tools        (all consume the stream)
```

- **Proxies intercept** — they sit in the message chain, transform or
  record, and forward. This is the spec's model.
- **Subscribers consume** — they read from the durable stream and dispatch
  to external systems. This is our addition on top.
- **The durable stream bridges them** — the proxy writes, the subscriber
  reads. Clean separation.

This means our proxy chain is spec-compliant (context injection via MCP,
response interception via durable state), and the event subscriber system
is a complementary layer that doesn't require changes to the ACP spec.

## Files to Change

| File | Change |
|---|---|
| `src/subscriber.rs` | **New** — `EventSubscriber` trait, `SubscriberManager`, `SessionEvent` |
| `src/ws_subscriber.rs` | **New** — WebSocket subscriber matching Flamecast protocol |
| `src/webhook_subscriber.rs` | **New** — HTTP POST subscriber with HMAC signing |
| `src/api.rs` | Add `WS /ws`, `POST /webhooks`, generalized SSE endpoints |
| `agents.toml` | Add `[[webhook]]` section |
| `Cargo.toml` | Add `axum-extra` (WebSocket), `hmac`/`sha2` (signing) |
