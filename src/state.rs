use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use anyhow::{Context, Result};
use sacp_conductor::trace::{NotificationEvent, RequestEvent, ResponseEvent, TraceEvent};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::{Mutex, RwLock, broadcast};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionState {
    Created,
    Attached,
    Broken,
    Closed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PromptTurnState {
    Active,
    Completed,
    Cancelled,
    Broken,
    // Legacy variants — may exist in old streams, not created by new code
    Queued,
    CancelRequested,
    TimedOut,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PendingRequestState {
    Pending,
    Resolved,
    Orphaned, // legacy
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TerminalState {
    Open,
    Exited,
    Released,
    Broken,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ChunkType {
    Text,
    ToolCall,
    Thinking,
    ToolResult,
    Error,
    Stop,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionRow {
    pub logical_connection_id: String,
    pub state: ConnectionState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latest_session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    /// Legacy field — exists in old streams, ignored by new code.
    #[serde(default, skip_serializing)]
    pub queue_paused: Option<bool>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptTurnRow {
    pub prompt_turn_id: String,
    pub logical_connection_id: String,
    pub session_id: String,
    pub request_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    pub state: PromptTurnState,
    /// Legacy field — exists in old streams, ignored by new code.
    #[serde(default, skip_serializing)]
    pub position: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop_reason: Option<String>,
    pub started_at: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionOptionRow {
    pub option_id: String,
    pub name: String,
    pub kind: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionRow {
    pub request_id: String,
    pub jsonrpc_id: Value,
    pub logical_connection_id: String,
    pub session_id: String,
    pub prompt_turn_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub options: Option<Vec<PermissionOptionRow>>,
    pub state: PendingRequestState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
    pub created_at: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolved_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalRow {
    pub terminal_id: String,
    pub logical_connection_id: String,
    pub session_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_turn_id: Option<String>,
    pub state: TerminalState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signal: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChunkRow {
    pub chunk_id: String,
    pub prompt_turn_id: String,
    pub logical_connection_id: String,
    #[serde(rename = "type")]
    pub chunk_type: ChunkType,
    pub content: String,
    pub seq: i64,
    pub created_at: i64,
}

/// Legacy STATE-PROTOCOL envelope format (used for seeding connection rows).
/// New events use TraceEvent format via DurableStreamTracer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateEnvelope<T> {
    #[serde(rename = "type")]
    pub entity_type: String,
    pub key: String,
    pub headers: StateHeaders,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<T>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateHeaders {
    pub operation: String,
}

#[derive(Debug, Clone)]
pub enum CollectionChange {
    Connections,
    PromptTurns,
    Permissions,
    Terminals,
    Chunks,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Collections {
    pub connections: BTreeMap<String, ConnectionRow>,
    pub prompt_turns: BTreeMap<String, PromptTurnRow>,
    pub permissions: BTreeMap<String, PermissionRow>,
    pub terminals: BTreeMap<String, TerminalRow>,
    pub chunks: BTreeMap<String, ChunkRow>,
}

/// Internal state for correlating TraceEvent requests with their responses.
#[derive(Debug, Default)]
struct TraceCorrelation {
    /// Maps JSON-RPC request ID → prompt_turn_id for in-flight prompt requests.
    prompt_request_to_turn: HashMap<String, String>,
    /// Tracks in-flight session/new requests (request ID → cwd) for response correlation.
    pending_session_new: HashMap<String, String>,
    /// Maps session_id → prompt_turn_id for the currently active prompt in each session.
    session_active_turn: HashMap<String, String>,
    /// Per-turn chunk sequence counter.
    chunk_seq: HashMap<String, i64>,
    /// Connection ID — set once from the host.
    connection_id: Option<String>,
    /// Monotonic counter for generating prompt_turn_ids.
    turn_counter: u64,
}

#[derive(Clone)]
pub struct StreamDb {
    inner: Arc<RwLock<Collections>>,
    changes_tx: broadcast::Sender<CollectionChange>,
    correlation: Arc<Mutex<TraceCorrelation>>,
}

impl StreamDb {
    pub fn new() -> Self {
        let (changes_tx, _) = broadcast::channel(256);
        Self {
            inner: Arc::new(RwLock::new(Collections::default())),
            changes_tx,
            correlation: Arc::new(Mutex::new(TraceCorrelation::default())),
        }
    }

    pub fn subscribe_changes(&self) -> broadcast::Receiver<CollectionChange> {
        self.changes_tx.subscribe()
    }

    pub async fn snapshot(&self) -> Collections {
        self.inner.read().await.clone()
    }

    pub async fn apply_json_message(&self, message: &[u8]) -> Result<()> {
        let value: Value = serde_json::from_slice(message).context("decode state event")?;
        let msg_type = value
            .get("type")
            .and_then(Value::as_str)
            .context("missing type")?;

        // Detect format: TraceEvent uses "request"/"response"/"notification",
        // StateEnvelope uses entity types like "connection"/"prompt_turn"/"chunk".
        match msg_type {
            "request" | "response" | "notification" => {
                let trace_event: TraceEvent =
                    serde_json::from_value(value).context("decode TraceEvent")?;
                return self.apply_trace_event(&trace_event).await;
            }
            _ => {} // fall through to StateEnvelope handling
        }

        let entity_type = msg_type;
        let operation = value
            .get("headers")
            .and_then(|v| v.get("operation"))
            .and_then(Value::as_str)
            .context("missing headers.operation")?;
        let key = value
            .get("key")
            .and_then(Value::as_str)
            .context("missing key")?
            .to_string();

        let mut collections = self.inner.write().await;
        let change = match entity_type {
            "connection" => {
                apply_collection::<ConnectionRow>(
                    &mut collections.connections, key, operation, value.get("value").cloned(),
                )?;
                CollectionChange::Connections
            }
            "prompt_turn" => {
                apply_collection::<PromptTurnRow>(
                    &mut collections.prompt_turns, key, operation, value.get("value").cloned(),
                )?;
                CollectionChange::PromptTurns
            }
            "permission" => {
                apply_collection::<PermissionRow>(
                    &mut collections.permissions, key, operation, value.get("value").cloned(),
                )?;
                CollectionChange::Permissions
            }
            "terminal" => {
                apply_collection::<TerminalRow>(
                    &mut collections.terminals, key, operation, value.get("value").cloned(),
                )?;
                CollectionChange::Terminals
            }
            "chunk" => {
                apply_collection::<ChunkRow>(
                    &mut collections.chunks, key, operation, value.get("value").cloned(),
                )?;
                CollectionChange::Chunks
            }
            // Legacy entity types from old streams — skip silently
            "pending_request" | "runtime_instance" => return Ok(()),
            other => anyhow::bail!("unsupported entity type: {other}"),
        };
        drop(collections);
        let _ = self.changes_tx.send(change);
        Ok(())
    }

    /// Set the connection ID used by trace event materialization.
    pub async fn set_connection_id(&self, id: String) {
        self.correlation.lock().await.connection_id = Some(id);
    }

    /// Materialize app state from an SDK TraceEvent.
    pub async fn apply_trace_event(&self, event: &TraceEvent) -> Result<()> {
        match event {
            TraceEvent::Request(req) => self.handle_trace_request(req).await,
            TraceEvent::Response(resp) => self.handle_trace_response(resp).await,
            TraceEvent::Notification(notif) => self.handle_trace_notification(notif).await,
            _ => Ok(()),
        }
    }

    async fn handle_trace_request(&self, req: &RequestEvent) -> Result<()> {
        match req.method.as_str() {
            "session/new" => {
                let cwd = req.params.get("cwd").and_then(Value::as_str)
                    .unwrap_or("").to_string();
                let request_id = req.id.to_string();
                let mut corr = self.correlation.lock().await;
                corr.pending_session_new.insert(request_id, cwd.clone());
                let conn_id = corr.connection_id.clone().unwrap_or_else(|| "unknown".to_string());
                drop(corr);

                let mut collections = self.inner.write().await;
                if let Some(conn) = collections.connections.get_mut(&conn_id) {
                    if !cwd.is_empty() {
                        conn.cwd = Some(cwd);
                    }
                    conn.updated_at = now_ms();
                }
                drop(collections);
                let _ = self.changes_tx.send(CollectionChange::Connections);
            }
            "session/prompt" => {
                let session_id = req.params.get("sessionId")
                    .or_else(|| req.params.get("session_id"))
                    .and_then(Value::as_str)
                    .unwrap_or("unknown");
                let request_id = req.id.to_string();

                let text = req.params.get("prompt")
                    .and_then(Value::as_array)
                    .and_then(|blocks| {
                        blocks.iter().find_map(|b| {
                            if b.get("type").and_then(Value::as_str) == Some("text") {
                                b.get("text").and_then(Value::as_str).map(String::from)
                            } else {
                                None
                            }
                        })
                    });

                let mut corr = self.correlation.lock().await;
                corr.turn_counter += 1;
                let prompt_turn_id = format!("trace-turn-{}", corr.turn_counter);
                let conn_id = corr.connection_id.clone().unwrap_or_else(|| "unknown".to_string());

                corr.prompt_request_to_turn.insert(request_id.clone(), prompt_turn_id.clone());
                corr.session_active_turn.insert(session_id.to_string(), prompt_turn_id.clone());
                drop(corr);

                let row = PromptTurnRow {
                    prompt_turn_id: prompt_turn_id.clone(),
                    logical_connection_id: conn_id,
                    session_id: session_id.to_string(),
                    request_id,
                    text,
                    state: PromptTurnState::Active,
                    position: None,
                    stop_reason: None,
                    started_at: now_ms(),
                    completed_at: None,
                };
                let mut collections = self.inner.write().await;
                collections.prompt_turns.insert(prompt_turn_id, row);
                drop(collections);
                let _ = self.changes_tx.send(CollectionChange::PromptTurns);
            }
            _ => {}
        }
        Ok(())
    }

    async fn handle_trace_response(&self, resp: &ResponseEvent) -> Result<()> {
        let request_id = resp.id.to_string();
        let mut corr = self.correlation.lock().await;

        // Handle session/new response
        if let Some(_cwd) = corr.pending_session_new.remove(&request_id) {
            if !resp.is_error {
                let session_id = resp.payload.get("sessionId")
                    .or_else(|| resp.payload.get("session_id"))
                    .and_then(Value::as_str);
                let conn_id = corr.connection_id.clone().unwrap_or_else(|| "unknown".to_string());
                drop(corr);

                if let Some(sid) = session_id {
                    let mut collections = self.inner.write().await;
                    if let Some(conn) = collections.connections.get_mut(&conn_id) {
                        conn.state = ConnectionState::Attached;
                        conn.latest_session_id = Some(sid.to_string());
                        conn.updated_at = now_ms();
                    }
                    drop(collections);
                    let _ = self.changes_tx.send(CollectionChange::Connections);
                }
                return Ok(());
            }
            drop(corr);
            return Ok(());
        }

        // Handle session/prompt response
        if let Some(prompt_turn_id) = corr.prompt_request_to_turn.remove(&request_id) {
            let stop_reason = if resp.is_error {
                Some("error".to_string())
            } else {
                resp.payload.get("stopReason")
                    .or_else(|| resp.payload.get("stop_reason"))
                    .and_then(Value::as_str)
                    .map(String::from)
            };

            let state = if resp.is_error {
                PromptTurnState::Broken
            } else {
                PromptTurnState::Completed
            };

            corr.session_active_turn.retain(|_, v| *v != prompt_turn_id);
            corr.chunk_seq.remove(&prompt_turn_id);
            drop(corr);

            let mut collections = self.inner.write().await;
            if let Some(turn) = collections.prompt_turns.get_mut(&prompt_turn_id) {
                turn.state = state;
                turn.stop_reason = stop_reason;
                turn.completed_at = Some(now_ms());
            }
            drop(collections);
            let _ = self.changes_tx.send(CollectionChange::PromptTurns);
        }
        Ok(())
    }

    async fn handle_trace_notification(&self, notif: &NotificationEvent) -> Result<()> {
        if notif.method != "session/update" {
            return Ok(());
        }

        let session_id = notif.session.as_deref()
            .or_else(|| notif.params.get("sessionId").and_then(Value::as_str))
            .or_else(|| notif.params.get("session_id").and_then(Value::as_str));
        let Some(session_id) = session_id else { return Ok(()) };

        let corr = self.correlation.lock().await;
        let prompt_turn_id = corr.session_active_turn.get(session_id).cloned();
        let conn_id = corr.connection_id.clone().unwrap_or_else(|| "unknown".to_string());
        drop(corr);
        let Some(prompt_turn_id) = prompt_turn_id else { return Ok(()) };

        let update = notif.params.get("update");
        let (chunk_type, content) = match update.and_then(|u| u.get("type")).and_then(Value::as_str) {
            Some("agentMessageChunk") => {
                let text = update
                    .and_then(|u| u.get("content"))
                    .and_then(|c| c.get("text"))
                    .and_then(Value::as_str)
                    .unwrap_or("");
                (ChunkType::Text, text.to_string())
            }
            Some("agentThoughtChunk") => {
                let text = update
                    .and_then(|u| u.get("content"))
                    .and_then(|c| c.get("text"))
                    .and_then(Value::as_str)
                    .unwrap_or("");
                (ChunkType::Thinking, text.to_string())
            }
            Some("toolCall") => {
                let content = update.map(|u| u.to_string()).unwrap_or_default();
                (ChunkType::ToolCall, content)
            }
            Some("toolCallUpdate") => {
                let content = update.map(|u| u.to_string()).unwrap_or_default();
                (ChunkType::ToolResult, content)
            }
            _ => return Ok(()),
        };

        let mut corr = self.correlation.lock().await;
        let seq = corr.chunk_seq.entry(prompt_turn_id.clone()).or_insert(0);
        let current_seq = *seq;
        *seq += 1;
        drop(corr);

        let chunk = ChunkRow {
            chunk_id: uuid::Uuid::new_v4().to_string(),
            prompt_turn_id,
            logical_connection_id: conn_id,
            chunk_type,
            content,
            seq: current_seq,
            created_at: now_ms(),
        };

        let mut collections = self.inner.write().await;
        collections.chunks.insert(chunk.chunk_id.clone(), chunk);
        drop(collections);
        let _ = self.changes_tx.send(CollectionChange::Chunks);
        Ok(())
    }
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("time went backwards")
        .as_millis() as i64
}

fn apply_collection<T: for<'de> Deserialize<'de>>(
    collection: &mut BTreeMap<String, T>,
    key: String,
    operation: &str,
    value: Option<Value>,
) -> Result<()> {
    match operation {
        "insert" | "update" => {
            let value = value.context("missing value for insert/update")?;
            collection.insert(key, serde_json::from_value(value)?);
        }
        "delete" => {
            collection.remove(&key);
        }
        other => anyhow::bail!("unsupported operation: {other}"),
    }
    Ok(())
}
