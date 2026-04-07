use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Result;
use agent_client_protocol::{PromptRequest, PromptResponse};
use sacp::{Conductor, ConnectionTo, Responder};
use tokio::sync::{Mutex, RwLock};
use uuid::Uuid;

use crate::durable_streams::EmbeddedDurableStreams;
use crate::state::{
    ChunkRow, ChunkType, ConnectionRow, ConnectionState, PendingRequestDirection,
    PendingRequestRow, PendingRequestState, PromptTurnRow, PromptTurnState, StateEnvelope,
    StateHeaders,
};

#[derive(Clone)]
pub struct AppState {
    pub logical_connection_id: String,
    pub durable_streams: EmbeddedDurableStreams,
    pub runtime: Arc<Mutex<RuntimeState>>,
    pub session_to_prompt_turn: Arc<RwLock<HashMap<String, String>>>,
    pub proxy_connection: Arc<Mutex<Option<ConnectionTo<Conductor>>>>,
}

pub struct RuntimeState {
    pub paused: bool,
    pub active_prompt_turn: Option<String>,
    pub queued: VecDeque<QueuedPrompt>,
    pub seq_by_prompt_turn: HashMap<String, i64>,
}

pub struct QueuedPrompt {
    pub prompt_turn: PromptTurnRow,
    pub prompt: PromptRequest,
    pub responder: Responder<PromptResponse>,
}

impl AppState {
    pub async fn new(bind: SocketAddr, state_stream: impl Into<String>) -> Result<Self> {
        let durable_streams = EmbeddedDurableStreams::start(bind, state_stream).await?;
        Self::init(durable_streams).await
    }

    /// Create an AppState that shares an existing durable streams server.
    /// Used by the multi-agent dashboard so all agents write to one state stream.
    pub async fn with_shared_streams(durable_streams: EmbeddedDurableStreams) -> Result<Self> {
        Self::init(durable_streams).await
    }

    async fn init(durable_streams: EmbeddedDurableStreams) -> Result<Self> {
        let logical_connection_id = Uuid::new_v4().to_string();
        let app = Self {
            logical_connection_id: logical_connection_id.clone(),
            durable_streams,
            runtime: Arc::new(Mutex::new(RuntimeState {
                paused: false,
                active_prompt_turn: None,
                queued: VecDeque::new(),
                seq_by_prompt_turn: HashMap::new(),
            })),
            session_to_prompt_turn: Arc::new(RwLock::new(HashMap::new())),
            proxy_connection: Arc::new(Mutex::new(None)),
        };

        let row = ConnectionRow {
            logical_connection_id: logical_connection_id.clone(),
            state: ConnectionState::Created,
            latest_session_id: None,
            last_error: None,
            queue_paused: Some(false),
            created_at: now_ms(),
            updated_at: now_ms(),
        };
        app.write_state_event("connection", "insert", logical_connection_id, Some(&row))
            .await?;
        Ok(app)
    }

    pub fn state_stream_url(&self) -> String {
        self.durable_streams
            .stream_url(&self.durable_streams.state_stream)
    }

    pub async fn write_state_event<T: serde::Serialize>(
        &self,
        entity_type: &str,
        operation: &str,
        key: impl Into<String>,
        value: Option<&T>,
    ) -> Result<()> {
        let envelope = StateEnvelope {
            headers: StateHeaders {
                operation: operation.to_string(),
                entity_type: entity_type.to_string(),
            },
            key: key.into(),
            value,
        };
        self.durable_streams
            .append_json(&self.durable_streams.state_stream, &envelope)
            .await
    }

    pub async fn update_connection(
        &self,
        mut update: impl FnMut(&mut ConnectionRow),
    ) -> Result<()> {
        let mut snapshot = self.durable_streams.stream_db.snapshot().await;
        let row = snapshot
            .connections
            .get_mut(&self.logical_connection_id)
            .expect("connection exists");
        update(row);
        row.updated_at = now_ms();
        let updated = row.clone();
        drop(snapshot);
        self.write_state_event(
            "connection",
            "update",
            &self.logical_connection_id,
            Some(&updated),
        )
        .await
    }

    pub async fn enqueue_prompt(
        &self,
        prompt: PromptRequest,
        responder: Responder<PromptResponse>,
    ) -> Result<String> {
        let request_id = responder.id().to_string();
        let prompt_turn_id = Uuid::new_v4().to_string();
        let text = prompt_text(&prompt);
        let row = PromptTurnRow {
            prompt_turn_id: prompt_turn_id.clone(),
            logical_connection_id: self.logical_connection_id.clone(),
            session_id: prompt.session_id.0.to_string(),
            request_id: request_id.clone(),
            text,
            state: PromptTurnState::Queued,
            position: None,
            stop_reason: None,
            started_at: now_ms(),
            completed_at: None,
        };
        self.write_state_event("prompt_turn", "insert", &prompt_turn_id, Some(&row))
            .await?;

        let pending = PendingRequestRow {
            request_id: request_id.clone(),
            logical_connection_id: self.logical_connection_id.clone(),
            session_id: Some(prompt.session_id.0.to_string()),
            prompt_turn_id: Some(prompt_turn_id.clone()),
            method: "session/prompt".to_string(),
            direction: PendingRequestDirection::ClientToAgent,
            state: PendingRequestState::Pending,
            created_at: now_ms(),
            resolved_at: None,
        };
        self.write_state_event("pending_request", "insert", &request_id, Some(&pending))
            .await?;

        let mut runtime = self.runtime.lock().await;
        runtime.queued.push_back(QueuedPrompt {
            prompt_turn: row,
            prompt,
            responder,
        });
        Ok(prompt_turn_id)
    }

    pub async fn record_chunk(
        &self,
        prompt_turn_id: &str,
        chunk_type: ChunkType,
        content: String,
    ) -> Result<()> {
        let mut runtime = self.runtime.lock().await;
        let next_seq = runtime
            .seq_by_prompt_turn
            .entry(prompt_turn_id.to_string())
            .and_modify(|v| *v += 1)
            .or_insert(0);
        let row = ChunkRow {
            chunk_id: Uuid::new_v4().to_string(),
            prompt_turn_id: prompt_turn_id.to_string(),
            logical_connection_id: self.logical_connection_id.clone(),
            chunk_type,
            content,
            seq: *next_seq,
            created_at: now_ms(),
        };
        drop(runtime);
        let key = row.chunk_id.clone();
        self.write_state_event("chunk", "insert", key, Some(&row))
            .await
    }

    /// Capture the proxy connection on first use. Subsequent calls are no-ops.
    pub async fn capture_proxy_connection(&self, cx: ConnectionTo<Conductor>) {
        let mut guard = self.proxy_connection.lock().await;
        if guard.is_none() {
            *guard = Some(cx);
        }
    }

    pub async fn take_next_prompt(&self) -> Option<QueuedPrompt> {
        let mut runtime = self.runtime.lock().await;
        if runtime.paused || runtime.active_prompt_turn.is_some() {
            return None;
        }
        let queued = runtime.queued.pop_front();
        if let Some(queued) = &queued {
            runtime.active_prompt_turn = Some(queued.prompt_turn.prompt_turn_id.clone());
        }
        queued
    }

    pub async fn finish_prompt_turn(
        &self,
        prompt_turn_id: &str,
        stop_reason: String,
        state: PromptTurnState,
    ) -> Result<()> {
        let mut snapshot = self.durable_streams.stream_db.snapshot().await;
        if let Some(row) = snapshot.prompt_turns.get_mut(prompt_turn_id) {
            row.state = state;
            row.stop_reason = Some(stop_reason);
            row.completed_at = Some(now_ms());
            let updated = row.clone();
            drop(snapshot);
            self.write_state_event(
                "prompt_turn",
                "update",
                prompt_turn_id.to_string(),
                Some(&updated),
            )
            .await?;
            let mut runtime = self.runtime.lock().await;
            runtime.active_prompt_turn = None;
        }
        Ok(())
    }

    pub async fn set_paused(&self, paused: bool) -> Result<()> {
        {
            let mut runtime = self.runtime.lock().await;
            runtime.paused = paused;
        }
        self.update_connection(|row| row.queue_paused = Some(paused))
            .await
    }
}

pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("time went backwards")
        .as_millis() as i64
}

fn prompt_text(prompt: &PromptRequest) -> Option<String> {
    let mut text = String::new();
    for block in &prompt.prompt {
        if let agent_client_protocol::ContentBlock::Text(text_block) = block {
            if !text.is_empty() {
                text.push('\n');
            }
            text.push_str(&text_block.text);
        }
    }
    if text.is_empty() { None } else { Some(text) }
}
