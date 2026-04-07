# How durable-acp-rs Unlocks Flamecast Capabilities

> Maps each Flamecast guide to what durable-acp-rs provides as the
> infrastructure layer. These guides are aspirational — durable-acp-rs
> is the reference implementation that makes them real.

## 1. Local Agents → Already Working

**Guide:** [Orchestrate local agents](https://flamecast.mintlify.app/guides/local-agents)
**What it describes:** Register agent templates, spawn local agents, setup scripts

**What durable-acp-rs provides:**
- `agents.toml` + ACP registry = agent templates
- `sacp-conductor` spawns agents as subprocesses = local orchestration
- `DurableStateProxy` = durable session state for free
- `PeerMcpProxy` = agent-to-agent messaging for free

**Status: ✅ Working today.** `cargo run --bin dashboard` does all of this.

The Flamecast guide's `flamecast.registerAgentTemplate()` maps to our
`agents.toml`:

```toml
# Flamecast template equivalent
[[agent]]
name = "codex"
agent = "codex-acp"  # resolved from ACP registry
```

---

## 2. Cloud Agents → Needs Pluggable Transports (W9)

**Guide:** [Orchestrate cloud agents](https://flamecast.mintlify.app/guides/cloud-agents)
**What it describes:** Docker runtime, custom runtimes, transport interface

**What durable-acp-rs provides:**
- `ConductorImpl::run(ByteStreams)` accepts any `AsyncRead + AsyncWrite`
- TCP/WebSocket transports = connect to remote containers
- Conductor writes to shared DS server regardless of where agent runs

**Status: 🔜 W9 (pluggable transports) + W10 (runtime providers)**

```toml
# agents.toml with remote agent
[[agent]]
name = "cloud-codex"
transport = "tcp"
host = "gpu-server.internal"
port = 9000
```

The Flamecast guide's `DockerRuntime` maps to our `RuntimeProvider` trait:
```rust
trait RuntimeProvider {
    async fn start(&self, config: &AgentConfig) -> Result<Box<dyn Transport>>;
    async fn stop(&self, id: &str) -> Result<()>;
}
```

---

## 3. Build Your Own Agent → Already Supported

**Guide:** [Build your own agent](https://flamecast.mintlify.app/guides/build-your-own-agent)
**What it describes:** Implement ACP Agent interface, connect via stdio/TCP

**What durable-acp-rs provides:**
- Any ACP-compatible agent works unmodified
- Conductor wraps it transparently — agent doesn't know about durable state
- Streaming responses, tool calls, permission requests all intercepted and persisted

**Status: ✅ Working today.** Any agent from the ACP registry or custom-built
works with our conductor.

```toml
[[agent]]
name = "my-agent"
command = ["node", "my-agent.js"]  # custom agent
```

---

## 4. Subagents → Unlocked by PeerMcpProxy

**Guide:** [Spin off subagents](https://flamecast.mintlify.app/guides/subagents)
**What it describes:** Parent agent spawns children via `POST /api/agents`

**What durable-acp-rs provides — two approaches:**

### Approach A: MCP peering (automatic, zero API calls)

Every agent already has `list_agents` + `prompt_agent` MCP tools.
A "parent" agent naturally delegates to peers:

```
Parent agent (claude-acp):
  "I need to fix the frontend CSS and the backend validation.
   Let me ask agent-b to handle the CSS while I do the backend."
  
  → calls list_agents() → finds agent-b
  → calls prompt_agent("agent-b", "fix the CSS in src/app.css")
  → agent-b responds with the fix
  → parent incorporates the result
```

No `POST /api/agents`, no session management, no WebSocket wiring.
The MCP tools handle it. **This is simpler than Flamecast's approach.**

### Approach B: REST API (Flamecast-compatible)

For the Flamecast guide's pattern (dynamic child spawning), we need
the session CRUD API (W8 in flamecast-integration-sdd.md):

```
POST /api/v1/agents → spawn conductor subprocess → return session
POST /api/v1/agents/{id}/prompts → send prompt
DELETE /api/v1/agents/{id} → terminate
```

**Status:** Approach A ✅ working. Approach B 🔜 needs session CRUD API.

---

## 5. Slackbot → Unlocked by Webhooks + SSE

**Guide:** [Set up a Slackbot](https://flamecast.mintlify.app/guides/slackbot)
**What it describes:** Slack → Flamecast → Agent → Slack. Requires persistent
WebSocket (problematic for serverless).

**The key pain point from the guide:**
> "Flamecast currently requires a persistent WebSocket connection to
> receive session events... incompatible with serverless platforms."

**What durable-acp-rs solves:**

The durable stream + `@durable-acp/client` eliminates the WebSocket
requirement. The Slackbot can be **fully stateless**:

```
1. Slack mention → serverless function
2. POST /api/v1/connections/{id}/prompt (fire and forget)
3. Register webhook: POST /api/v1/webhooks
     { url: "https://my-slack-bot.com/callback", events: ["chunk"] }
4. Webhook fires on each chunk → serverless function → Slack message
```

No persistent WebSocket. No long-running process. The DS stream + webhook
worker handles delivery. The bot is a pair of serverless functions:
- One receives Slack events → POSTs prompts to REST API
- One receives webhook callbacks → posts chunks to Slack

**Status: 🔜 Needs webhook worker (W7b, ~50 lines) + session CRUD API**

Alternatively, the `@durable-acp/client` can subscribe via SSE (no WebSocket):
```typescript
// Serverless-friendly: poll SSE from a worker
const db = createDurableACPDB({ stateStreamUrl });
db.collections.chunks.subscribe((chunks) => {
  // Post new chunks to Slack
});
```

---

## 6. Custom UI → Unlocked by StreamDB + DurableACPClient

**Guide:** [Build a custom UI](https://flamecast.mintlify.app/guides/custom-ui)
**What it describes:** `useFlamecastSession` hook, render conversations,
handle permissions, browse files

**What durable-acp-rs provides:**

The `@durable-acp/client` + `@durable-acp/state` packages ARE the
custom UI integration layer. They provide:

| Flamecast Hook | durable-acp-rs Equivalent |
|---|---|
| `useFlamecastSession(id)` | `useDurableACPSession(id)` — wraps `createDurableACPDB` |
| `events` | `db.collections.chunks` + `.promptTurns` (reactive TanStack DB) |
| `prompt(text)` | `client.prompt(text)` (REST POST) |
| `respondToPermission()` | `client.resolvePermission(id, optionId)` |
| `cancel()` / `terminate()` | `client.cancel()` |
| `requestFilePreview()` | `GET /api/v1/agents/{id}/files?path=...` (W11) |
| `connectionState` | StreamDB connection state |
| `sessionLogsToSegments()` | Derive from `chunks` collection (text/tool/permission) |

The `useFlamecastSession` hook maps directly to a React hook wrapping
`DurableACPClient`:

```typescript
function useDurableACPSession(sessionId: string) {
  const client = useMemo(() => new DurableACPClient({
    stateStreamUrl: `${DS_URL}/streams/durable-acp-state`,
    serverUrl: API_URL,
    connectionId: sessionId,
  }), [sessionId]);

  const chunks = useLiveQuery(client.collections.chunks);
  const turns = useLiveQuery(client.collections.promptTurns);
  const permissions = useLiveQuery(client.pendingPermissions);

  return {
    chunks,
    turns,
    permissions,
    prompt: (text) => client.prompt(text),
    resolvePermission: (id, optionId) => client.resolvePermission(id, optionId),
    cancel: () => client.cancel(),
  };
}
```

**Status: ✅ Client packages exist.** 🔜 Needs file system API (W11) and
schema compatibility verification (W8).

---

## Summary: What Unlocks What

| Flamecast Guide | What Unlocks It | Status |
|---|---|---|
| Local agents | `agents.toml` + conductor + dashboard | ✅ Working |
| Build your own agent | Any ACP agent works unmodified | ✅ Working |
| Subagents (MCP approach) | `PeerMcpProxy` list/prompt tools | ✅ Working |
| Custom UI | `@durable-acp/client` + StreamDB | ✅ Client exists, needs schema verify |
| Subagents (API approach) | Session CRUD API | 🔜 W8 |
| Cloud agents | Pluggable transports + runtime providers | 🔜 W9 + W10 |
| Slackbot (stateless) | Webhook worker + REST API | 🔜 W7b + W8 |

**The key unlock that durable-acp-rs provides over Flamecast today:**

1. **No persistent WebSocket required** — DS stream + SSE/webhooks enables
   stateless integrations (Slackbot, serverless functions)
2. **MCP peering is automatic** — subagents don't need API calls, just
   `prompt_agent("peer-name", "do this")`
3. **Durable sessions for free** — any agent wrapped in the conductor gets
   history, replay, and observability without code changes
4. **Reactive TypeScript client exists** — `@durable-acp/state` StreamDB
   with TanStack DB collections, not custom WebSocket event parsing
