//! Shared test helpers — replaces ConductorState for test setup.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;

use tokio::sync::Mutex;

use durable_acp_rs::state::*;
use durable_acp_rs::stream_server::StreamServer;

/// Lightweight replacement for ConductorState in tests.
/// Owns a StreamServer and connection identity, provides helpers
/// for writing StateEnvelope events to the durable stream.
pub struct TestApp {
    pub stream_server: StreamServer,
    pub connection_id: String,
    seq: Mutex<HashMap<String, i64>>,
}

impl TestApp {
    pub async fn new(ds: StreamServer) -> Self {
        let connection_id = uuid::Uuid::new_v4().to_string();
        let conn_row = ConnectionRow {
            logical_connection_id: connection_id.clone(),
            state: ConnectionState::Created,
            latest_session_id: None,
            cwd: None,
            last_error: None,
            queue_paused: None,
            created_at: now_ms(),
            updated_at: now_ms(),
        };
        let envelope = StateEnvelope {
            entity_type: "connection".to_string(),
            key: connection_id.clone(),
            headers: StateHeaders { operation: "insert".to_string() },
            value: Some(&conn_row),
        };
        ds.append_json(&ds.state_stream, &envelope).await.unwrap();
        ds.stream_db.set_connection_id(connection_id.clone()).await;
        Self {
            stream_server: ds,
            connection_id,
            seq: Mutex::new(HashMap::new()),
        }
    }

    pub async fn write_state_event<T: serde::Serialize>(
        &self, entity_type: &str, operation: &str, key: &str, value: Option<&T>,
    ) -> anyhow::Result<()> {
        let envelope = StateEnvelope {
            entity_type: entity_type.to_string(),
            key: key.to_string(),
            headers: StateHeaders { operation: operation.to_string() },
            value,
        };
        self.stream_server.append_json(&self.stream_server.state_stream, &envelope).await
    }

    pub async fn record_chunk(
        &self, prompt_turn_id: &str, chunk_type: ChunkType, content: String,
    ) -> anyhow::Result<()> {
        let mut seq_map = self.seq.lock().await;
        let seq_val = *seq_map
            .entry(prompt_turn_id.to_string())
            .and_modify(|v| *v += 1)
            .or_insert(0);
        drop(seq_map);

        let chunk_id = uuid::Uuid::new_v4().to_string();
        let row = ChunkRow {
            chunk_id: chunk_id.clone(),
            prompt_turn_id: prompt_turn_id.to_string(),
            logical_connection_id: self.connection_id.clone(),
            chunk_type,
            content,
            seq: seq_val,
            created_at: now_ms(),
        };
        self.write_state_event("chunk", "insert", &chunk_id, Some(&row)).await
    }

    pub async fn finish_prompt_turn(
        &self, turn_id: &str, stop_reason: String, state: PromptTurnState,
    ) -> anyhow::Result<()> {
        let snapshot = self.stream_server.stream_db.snapshot().await;
        if let Some(turn) = snapshot.prompt_turns.get(turn_id) {
            let mut updated = turn.clone();
            updated.state = state;
            updated.stop_reason = Some(stop_reason);
            updated.completed_at = Some(now_ms());
            drop(snapshot);
            self.write_state_event("prompt_turn", "update", turn_id, Some(&updated)).await
        } else {
            Ok(())
        }
    }

    pub async fn update_connection(
        &self, f: impl FnOnce(&mut ConnectionRow),
    ) -> anyhow::Result<()> {
        let snapshot = self.stream_server.stream_db.snapshot().await;
        if let Some(conn) = snapshot.connections.get(&self.connection_id) {
            let mut updated = conn.clone();
            f(&mut updated);
            updated.updated_at = now_ms();
            drop(snapshot);
            self.write_state_event("connection", "update", &self.connection_id, Some(&updated)).await
        } else {
            anyhow::bail!("connection not found")
        }
    }

    pub fn state_stream_url(&self) -> String {
        self.stream_server.stream_url(&self.stream_server.state_stream)
    }
}

pub async fn test_ds() -> StreamServer {
    let tmp = tempfile::tempdir().unwrap();
    let bind = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0);
    let ds = StreamServer::start_with_dir(bind, "durable-acp-state", tmp.path().to_path_buf())
        .await
        .unwrap();
    std::mem::forget(tmp);
    ds
}

pub async fn test_app() -> Arc<TestApp> {
    let ds = test_ds().await;
    Arc::new(TestApp::new(ds).await)
}
