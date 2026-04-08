//! Durable stream subscriber — consumes a remote durable stream and
//! materializes state into a local `StreamDb`.
//!
//! Uses the official `durable-streams` Rust client for transport,
//! reconnection, and offset tracking. This module is a thin adapter
//! that feeds each chunk through `StreamDb::apply_json_message()`.
//!
//! ```text
//! Durable Streams Server
//!   ← durable-streams client (SSE / long-poll, auto-reconnect)
//! StreamSubscriber
//!   → feeds JSON into StreamDb::apply_json_message()
//!   → notifies when up-to-date (preload complete)
//! ```

use std::sync::Arc;

use anyhow::Result;
use durable_streams::{Client, LiveMode, Offset};
use tokio::sync::Notify;
use tracing::{debug, warn};

use crate::state::StreamDb;

/// Subscribes to a remote durable stream and materializes state into
/// a local `StreamDb`.
pub struct StreamSubscriber {
    stream_url: String,
    stream_db: StreamDb,
    up_to_date: Arc<Notify>,
    handle: Option<tokio::task::JoinHandle<()>>,
}

impl StreamSubscriber {
    /// Create a new subscriber pointing at a stream URL.
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

    /// Connect and start consuming events in the background.
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
            consume_stream(url, db, up_to_date).await;
        }));
    }

    /// Connect, consume events, and wait until the initial state is
    /// fully materialized (server sends `up_to_date: true`).
    pub async fn preload(&mut self) -> Result<()> {
        // Register the waiter BEFORE connecting to avoid the race where
        // up_to_date arrives before notified() is polled.
        let notify = self.up_to_date.clone();
        let waiter = notify.notified();
        self.connect();
        waiter.await;
        Ok(())
    }

    /// Disconnect from the stream.
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

/// Core consumer loop using the durable-streams client.
/// The client handles SSE/long-poll transport, reconnection, and offset
/// tracking internally.
async fn consume_stream(url: String, db: StreamDb, up_to_date: Arc<Notify>) {
    let client = Client::new();
    let stream = client.stream(&url);

    let mut reader = match stream
        .read()
        .offset(Offset::Beginning)
        .live(LiveMode::Sse)
        .build()
    {
        Ok(r) => r,
        Err(e) => {
            warn!(error = %e, "failed to build stream reader");
            return;
        }
    };

    loop {
        match reader.next_chunk().await {
            Ok(Some(chunk)) => {
                // Chunk data is a JSON array of events: [{...}, {...}]
                if !chunk.data.is_empty() {
                    match serde_json::from_slice::<Vec<serde_json::Value>>(&chunk.data) {
                        Ok(events) => {
                            for event in events {
                                let bytes = serde_json::to_vec(&event).unwrap();
                                if let Err(e) = db.apply_json_message(&bytes).await {
                                    warn!(error = %e, "failed to apply state event");
                                }
                            }
                        }
                        Err(e) => {
                            debug!(error = %e, "chunk data is not a JSON array, skipping");
                        }
                    }
                }
                if chunk.up_to_date {
                    up_to_date.notify_waiters();
                }
            }
            Ok(None) => {
                // Stream ended (closed) — stop consuming.
                return;
            }
            Err(e) => {
                warn!(error = %e, "durable-streams read error");
                // The client handles retryable errors internally.
                // A non-retryable error means we should stop.
                if !e.is_retryable() {
                    return;
                }
            }
        }
    }
}
