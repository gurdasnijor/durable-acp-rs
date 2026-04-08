use std::io;

use futures::StreamExt;
use futures::channel::mpsc;
use sacp_conductor::trace::{TraceEvent, WriteEvent};

use crate::stream_server::StreamServer;

/// Implements the SDK's `WriteEvent` trait by forwarding `TraceEvent`s
/// through an async channel to a background task that appends them
/// to the durable stream.
pub struct DurableStreamTracer {
    tx: mpsc::UnboundedSender<TraceEvent>,
}

impl WriteEvent for DurableStreamTracer {
    fn write_event(&mut self, event: &TraceEvent) -> io::Result<()> {
        self.tx
            .unbounded_send(event.clone())
            .map_err(|e| io::Error::new(io::ErrorKind::BrokenPipe, e))
    }
}

impl DurableStreamTracer {
    pub fn start(stream_server: StreamServer, stream_name: String) -> Self {
        let (tx, mut rx) = mpsc::unbounded();
        tokio::spawn(async move {
            while let Some(event) = rx.next().await {
                let _ = stream_server.append_json(&stream_name, &event).await;
            }
        });
        Self { tx }
    }
}
