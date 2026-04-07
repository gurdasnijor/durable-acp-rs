use std::collections::BTreeMap;
use std::sync::Arc;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::{RwLock, broadcast};

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
    Queued,
    Active,
    Completed,
    CancelRequested,
    Cancelled,
    Broken,
    TimedOut,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PendingRequestDirection {
    ClientToAgent,
    AgentToClient,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PendingRequestState {
    Pending,
    Resolved,
    Orphaned,
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
pub enum RuntimeStatus {
    Running,
    Paused,
    Stopped,
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
    pub last_error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub position: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop_reason: Option<String>,
    pub started_at: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingRequestRow {
    pub request_id: String,
    pub logical_connection_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_turn_id: Option<String>,
    pub method: String,
    pub direction: PendingRequestDirection,
    pub state: PendingRequestState,
    pub created_at: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolved_at: Option<i64>,
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
pub struct RuntimeInstanceRow {
    pub instance_id: String,
    pub runtime_name: String,
    pub status: RuntimeStatus,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateHeaders {
    pub operation: String,
    #[serde(rename = "type")]
    pub entity_type: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateEnvelope<T> {
    pub headers: StateHeaders,
    pub key: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<T>,
}

#[derive(Debug, Clone)]
pub enum CollectionChange {
    Connections,
    PromptTurns,
    PendingRequests,
    Permissions,
    Terminals,
    RuntimeInstances,
    Chunks,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Collections {
    pub connections: BTreeMap<String, ConnectionRow>,
    pub prompt_turns: BTreeMap<String, PromptTurnRow>,
    pub pending_requests: BTreeMap<String, PendingRequestRow>,
    pub permissions: BTreeMap<String, PermissionRow>,
    pub terminals: BTreeMap<String, TerminalRow>,
    pub runtime_instances: BTreeMap<String, RuntimeInstanceRow>,
    pub chunks: BTreeMap<String, ChunkRow>,
}

#[derive(Clone)]
pub struct StreamDb {
    inner: Arc<RwLock<Collections>>,
    changes_tx: broadcast::Sender<CollectionChange>,
}

impl StreamDb {
    pub fn new() -> Self {
        let (changes_tx, _) = broadcast::channel(256);
        Self {
            inner: Arc::new(RwLock::new(Collections::default())),
            changes_tx,
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
        let entity_type = value
            .get("headers")
            .and_then(|v| v.get("type"))
            .and_then(Value::as_str)
            .context("missing headers.type")?;
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
        match entity_type {
            "connection" => apply_collection::<ConnectionRow>(
                &mut collections.connections,
                key,
                operation,
                value.get("value").cloned(),
            )?,
            "prompt_turn" => apply_collection::<PromptTurnRow>(
                &mut collections.prompt_turns,
                key,
                operation,
                value.get("value").cloned(),
            )?,
            "pending_request" => apply_collection::<PendingRequestRow>(
                &mut collections.pending_requests,
                key,
                operation,
                value.get("value").cloned(),
            )?,
            "permission" => apply_collection::<PermissionRow>(
                &mut collections.permissions,
                key,
                operation,
                value.get("value").cloned(),
            )?,
            "terminal" => apply_collection::<TerminalRow>(
                &mut collections.terminals,
                key,
                operation,
                value.get("value").cloned(),
            )?,
            "runtime_instance" => apply_collection::<RuntimeInstanceRow>(
                &mut collections.runtime_instances,
                key,
                operation,
                value.get("value").cloned(),
            )?,
            "chunk" => apply_collection::<ChunkRow>(
                &mut collections.chunks,
                key,
                operation,
                value.get("value").cloned(),
            )?,
            other => anyhow::bail!("unsupported entity type: {other}"),
        };

        let change = match entity_type {
            "connection" => CollectionChange::Connections,
            "prompt_turn" => CollectionChange::PromptTurns,
            "pending_request" => CollectionChange::PendingRequests,
            "permission" => CollectionChange::Permissions,
            "terminal" => CollectionChange::Terminals,
            "runtime_instance" => CollectionChange::RuntimeInstances,
            "chunk" => CollectionChange::Chunks,
            _ => unreachable!(),
        };
        let _ = self.changes_tx.send(change);
        Ok(())
    }
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
