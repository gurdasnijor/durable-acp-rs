//! MCP proxy that exposes `list_agents` and `prompt_agent` tools to the agent.
//!
//! Add this proxy to the conductor chain so that any agent running under
//! durable-acp-rs can discover and message peer agents registered in the
//! local registry.

use anyhow::Result;
use futures::StreamExt as _;
use sacp::{Conductor, ConnectTo, Proxy};
use sacp::mcp_server::McpServer;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::registry;

// ---------------------------------------------------------------------------
// MCP tool parameter / return types
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct ListAgentsInput {}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct AgentInfo {
    /// Registered name of the agent
    name: String,
    /// HTTP API base URL (e.g. http://127.0.0.1:4438)
    api_url: String,
    /// Logical connection ID (UUID)
    logical_connection_id: String,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct ListAgentsOutput {
    agents: Vec<AgentInfo>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct PromptAgentInput {
    /// Name of the target agent — use list_agents to discover available names
    name: String,
    /// Text prompt to send to the agent
    text: String,
}

// ---------------------------------------------------------------------------
// Internal HTTP types (mirrors api.rs response shapes)
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ConnectionInfo {
    logical_connection_id: String,
    latest_session_id: Option<String>,
    state: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PromptAccepted {
    prompt_turn_id: String,
}

#[derive(Deserialize)]
struct ChunkEvent {
    #[serde(rename = "type")]
    chunk_type: crate::state::ChunkType,
    content: String,
}

// ---------------------------------------------------------------------------
// HTTP helper
// ---------------------------------------------------------------------------

/// Submit a prompt to a peer agent and return its complete text response.
async fn call_peer(http: &reqwest::Client, api_url: &str, text: &str) -> Result<String> {
    // 1. Find an attached connection with an active session.
    let connections: Vec<ConnectionInfo> = http
        .get(format!("{api_url}/api/v1/connections"))
        .send()
        .await?
        .json()
        .await?;

    let conn = connections
        .iter()
        .find(|c| c.state == "attached")
        .or(connections.first())
        .ok_or_else(|| anyhow::anyhow!("no connections on peer agent at {api_url}"))?;

    let session_id = conn
        .latest_session_id
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("peer at {api_url} has no active session"))?;

    // 2. Submit the prompt.
    let accepted: PromptAccepted = http
        .post(format!(
            "{api_url}/api/v1/connections/{}/prompt",
            conn.logical_connection_id
        ))
        .json(&serde_json::json!({
            "sessionId": session_id,
            "text": text,
        }))
        .send()
        .await?
        .json()
        .await?;

    // 3. Stream the response via SSE until Stop or Error chunk.
    let response = http
        .get(format!(
            "{api_url}/api/v1/prompt-turns/{}/stream",
            accepted.prompt_turn_id
        ))
        .timeout(std::time::Duration::from_secs(120))
        .send()
        .await?;

    let mut buf = String::new();
    let mut output = String::new();
    let mut stream = response.bytes_stream();

    while let Some(chunk) = stream.next().await {
        let bytes = chunk?;
        buf.push_str(&String::from_utf8_lossy(&bytes));

        // Consume complete SSE events (separated by blank lines).
        while let Some(event_end) = buf.find("\n\n") {
            let event = buf[..event_end].to_string();
            buf = buf[event_end + 2..].to_string();

            for line in event.lines() {
                if let Some(data) = line.strip_prefix("data:") {
                    let data = data.trim();
                    if data.is_empty() {
                        continue;
                    }
                    if let Ok(ev) = serde_json::from_str::<ChunkEvent>(data) {
                        use crate::state::ChunkType;
                        match ev.chunk_type {
                            ChunkType::Text => output.push_str(&ev.content),
                            ChunkType::Stop => return Ok(output),
                            ChunkType::Error => {
                                anyhow::bail!("peer agent returned error: {}", ev.content)
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
    }

    Ok(output)
}

// ---------------------------------------------------------------------------
// Proxy component
// ---------------------------------------------------------------------------

/// A conductor proxy that exposes two MCP tools to the wrapped agent:
///
/// - `list_agents` — enumerate peers from the local registry
/// - `prompt_agent` — send a prompt to a named peer and return the response
pub struct PeerMcpProxy;

impl ConnectTo<Conductor> for PeerMcpProxy {
    async fn connect_to(self, client: impl ConnectTo<Proxy>) -> Result<(), sacp::Error> {
        let http = reqwest::Client::new();

        let server = McpServer::builder("peer")
            .instructions(
                "Tools for discovering and messaging peer durable-acp agents running on \
                 this machine. Use list_agents to find peers, then prompt_agent to send \
                 a message and receive its complete response.",
            )
            .tool_fn(
                "list_agents",
                "List all peer agents currently registered on this machine.",
                async |_input: ListAgentsInput, _cx| {
                    let reg = registry::read_registry()
                        .map_err(|e| sacp::util::internal_error(e.to_string()))?;
                    Ok(ListAgentsOutput {
                        agents: reg
                            .agents
                            .into_iter()
                            .map(|e| AgentInfo {
                                name: e.name,
                                api_url: e.api_url,
                                logical_connection_id: e.logical_connection_id,
                            })
                            .collect(),
                    })
                },
                sacp::tool_fn!(),
            )
            .tool_fn(
                "prompt_agent",
                "Send a text prompt to a named peer agent and return its complete text \
                 response. Blocks until the agent finishes its turn (up to 120 s).",
                async move |input: PromptAgentInput, _cx| {
                    let reg = registry::read_registry()
                        .map_err(|e| sacp::util::internal_error(e.to_string()))?;
                    let entry = reg
                        .agents
                        .into_iter()
                        .find(|e| e.name == input.name)
                        .ok_or_else(|| {
                            sacp::util::internal_error(format!(
                                "no agent named '{}' in registry",
                                input.name
                            ))
                        })?;
                    call_peer(&http, &entry.api_url, &input.text)
                        .await
                        .map_err(|e| sacp::util::internal_error(e.to_string()))
                },
                sacp::tool_fn!(),
            )
            .build();

        sacp::Proxy
            .builder()
            .name("peer-mcp")
            .with_mcp_server(server)
            .connect_to(client)
            .await
    }
}
