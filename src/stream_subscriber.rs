//! Durable session client — SSE subscriber to a durable stream.
//!
//! Connects to a durable streams server via SSE (`?live=sse`), tracks
//! offsets, and feeds events into a `StreamDb` for materialization.
//! Handles reconnection from the last offset automatically.
//!
//! This is the Rust equivalent of `@durable-streams/state` StreamDB —
//! the missing transport primitive that turns a remote durable stream
//! into a local materialized state.
//!
//! ```text
//! Durable Streams Server (:4437)
//!   GET /streams/durable-acp-state?live=sse
//!       ↓ SSE: event: data / event: control
//! StreamSubscriber
//!   → parses SSE frames
//!   → feeds JSON into StreamDb::apply_json_message()
//!   → tracks offset for reconnection
//!   → notifies when up-to-date (preload complete)
//! ```

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use serde::Deserialize;
use tokio::sync::Notify;
use tracing::{debug, warn};

use crate::state::StreamDb;

/// A durable session that subscribes to a remote durable stream via SSE
/// and materializes state into a local `StreamDb`.
pub struct StreamSubscriber {
    stream_url: String,
    stream_db: StreamDb,
    up_to_date: Arc<Notify>,
    handle: Option<tokio::task::JoinHandle<()>>,
}

/// SSE control event payload from the durable streams server.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ControlPayload {
    stream_next_offset: String,
    #[serde(default)]
    up_to_date: Option<bool>,
    #[serde(default)]
    stream_closed: Option<bool>,
}

impl StreamSubscriber {
    /// Create a new durable session pointing at a stream URL.
    ///
    /// Does not connect until `connect()` or `preload()` is called.
    pub fn new(stream_url: impl Into<String>) -> Self {
        Self {
            stream_url: stream_url.into(),
            stream_db: StreamDb::new(),
            up_to_date: Arc::new(Notify::new()),
            handle: None,
        }
    }

    /// Get a reference to the `StreamDb` for snapshots and subscriptions.
    pub fn stream_db(&self) -> &StreamDb {
        &self.stream_db
    }

    /// Connect and start consuming SSE events in the background.
    ///
    /// Events are applied to the `StreamDb` as they arrive. Call
    /// `preload()` instead if you need to wait for initial sync.
    pub fn connect(&mut self) {
        if self.handle.is_some() {
            return;
        }
        let url = self.stream_url.clone();
        let db = self.stream_db.clone();
        let up_to_date = self.up_to_date.clone();

        self.handle = Some(tokio::spawn(async move {
            run_sse_loop(url, db, up_to_date).await;
        }));
    }

    /// Connect, consume events, and wait until the initial state is
    /// fully materialized (server sends `upToDate: true`).
    pub async fn preload(&mut self) -> Result<()> {
        // Register the waiter BEFORE connecting to avoid the race where
        // upToDate arrives before notified() is polled.
        let notify = self.up_to_date.clone();
        let waiter = notify.notified();
        self.connect();
        waiter.await;
        Ok(())
    }

    /// Disconnect from the SSE stream.
    pub fn disconnect(&mut self) {
        if let Some(handle) = self.handle.take() {
            handle.abort();
        }
    }
}

impl Drop for StreamSubscriber {
    fn drop(&mut self) {
        self.disconnect();
    }
}

/// Core SSE consumer loop. Reconnects on error with exponential backoff.
async fn run_sse_loop(url: String, db: StreamDb, up_to_date: Arc<Notify>) {
    let client = reqwest::Client::new();
    let mut offset: Option<String> = None;
    let mut backoff = Duration::from_millis(100);
    let max_backoff = Duration::from_secs(30);

    loop {
        let off = offset.as_deref().unwrap_or("-1");
        let req_url = format!("{url}?live=sse&offset={off}");

        debug!(url = %req_url, "SSE connecting");

        match consume_sse_stream(&client, &req_url, &db, &up_to_date, &mut offset).await {
            Ok(closed) => {
                if closed {
                    debug!("SSE stream closed by server");
                    return;
                }
                // Stream ended without close — reconnect immediately
                backoff = Duration::from_millis(100);
            }
            Err(e) => {
                warn!(error = %e, backoff_ms = backoff.as_millis(), "SSE error, reconnecting");
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(max_backoff);
            }
        }
    }
}

/// Consume one SSE connection. Returns Ok(true) if stream closed normally.
async fn consume_sse_stream(
    client: &reqwest::Client,
    url: &str,
    db: &StreamDb,
    up_to_date: &Notify,
    offset: &mut Option<String>,
) -> Result<bool> {
    let response = client
        .get(url)
        .header("Accept", "text/event-stream")
        .send()
        .await
        .context("SSE connect")?;

    if !response.status().is_success() {
        anyhow::bail!("SSE HTTP {}", response.status());
    }

    let mut buf = String::new();
    let mut stream = response.bytes_stream();
    use futures::StreamExt;

    while let Some(chunk) = stream.next().await {
        let bytes = chunk.context("SSE read")?;
        buf.push_str(&String::from_utf8_lossy(&bytes));

        // Parse complete SSE frames (terminated by \n\n)
        while let Some(end) = buf.find("\n\n") {
            let frame = buf[..end].to_string();
            buf = buf[end + 2..].to_string();

            if let Some(closed) = process_sse_frame(&frame, db, up_to_date, offset).await? {
                if closed {
                    return Ok(true);
                }
            }
        }
    }

    Ok(false)
}

/// Process a single SSE frame. Returns Some(true) if stream is closed.
async fn process_sse_frame(
    frame: &str,
    db: &StreamDb,
    up_to_date: &Notify,
    offset: &mut Option<String>,
) -> Result<Option<bool>> {
    let mut event_type = None;
    let mut data_lines = Vec::new();

    for line in frame.lines() {
        if line.starts_with(':') {
            // Comment / keepalive — skip
            continue;
        }
        if let Some(rest) = line.strip_prefix("event:") {
            event_type = Some(rest.trim().to_string());
        } else if let Some(rest) = line.strip_prefix("data:") {
            data_lines.push(rest.to_string());
        }
    }

    if data_lines.is_empty() {
        return Ok(None);
    }

    let data = data_lines.join("\n");

    match event_type.as_deref() {
        Some("control") => {
            let control: ControlPayload =
                serde_json::from_str(&data).context("parse control payload")?;
            *offset = Some(control.stream_next_offset);

            if control.up_to_date == Some(true) {
                up_to_date.notify_waiters();
            }
            if control.stream_closed == Some(true) {
                return Ok(Some(true));
            }
        }
        Some("data") => {
            // JSON streams batch events as a JSON array: [{...}, {...}]
            let events: Vec<serde_json::Value> =
                serde_json::from_str(&data).context("parse data batch")?;
            for event in events {
                let bytes = serde_json::to_vec(&event)?;
                if let Err(e) = db.apply_json_message(&bytes).await {
                    warn!(error = %e, "failed to apply state event");
                }
            }
        }
        _ => {
            // Unknown event type — skip
        }
    }

    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::ConnectionState;

    #[tokio::test]
    async fn process_control_frame_updates_offset() {
        let db = StreamDb::new();
        let notify = Notify::new();
        let mut offset = None;

        let frame = r#"event: control
data:{"streamNextOffset":"abc_123","upToDate":true}"#;

        let result = process_sse_frame(frame, &db, &notify, &mut offset)
            .await
            .unwrap();

        assert!(result.is_none()); // not closed
        assert_eq!(offset, Some("abc_123".to_string()));
    }

    #[tokio::test]
    async fn process_data_frame_applies_state_events() {
        let db = StreamDb::new();
        let notify = Notify::new();
        let mut offset = None;

        let frame = r#"event: data
data:[{"headers":{"operation":"insert","type":"connection"},"key":"conn-1","value":{"logicalConnectionId":"conn-1","state":"created","queuePaused":false,"createdAt":1,"updatedAt":1}}]"#;

        process_sse_frame(frame, &db, &notify, &mut offset)
            .await
            .unwrap();

        let snapshot = db.snapshot().await;
        assert_eq!(snapshot.connections.len(), 1);
        assert_eq!(
            snapshot.connections["conn-1"].state,
            ConnectionState::Created
        );
    }

    #[tokio::test]
    async fn process_closed_frame_returns_true() {
        let db = StreamDb::new();
        let notify = Notify::new();
        let mut offset = None;

        let frame = r#"event: control
data:{"streamNextOffset":"end","streamClosed":true}"#;

        let result = process_sse_frame(frame, &db, &notify, &mut offset)
            .await
            .unwrap();

        assert_eq!(result, Some(true));
    }

    #[tokio::test]
    async fn keepalive_comments_are_skipped() {
        let db = StreamDb::new();
        let notify = Notify::new();
        let mut offset = None;

        let frame = ":keepalive";

        let result = process_sse_frame(frame, &db, &notify, &mut offset)
            .await
            .unwrap();

        assert!(result.is_none());
        assert!(offset.is_none());
    }
}
