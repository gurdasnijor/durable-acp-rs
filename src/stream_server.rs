use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use axum::Extension;
use axum::Router;
use axum::routing::get;
use bytes::Bytes;
use durable_streams_server::config::Config;
use durable_streams_server::handlers;
use durable_streams_server::middleware;
use durable_streams_server::router::build_router;
use durable_streams_server::storage::Storage;
use durable_streams_server::storage::StreamConfig;
use durable_streams_server::storage::file::FileStorage;
use tokio::net::TcpListener;

use crate::state::StreamDb;

const MAX_TOTAL_BYTES: u64 = 128 * 1024 * 1024;
const MAX_STREAM_BYTES: u64 = 32 * 1024 * 1024;

/// Default storage directory: ~/.local/share/durable-acp/streams/
fn default_storage_dir() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("durable-acp")
        .join("streams")
}

#[derive(Clone)]
pub struct StreamServer {
    pub storage: Arc<FileStorage>,
    pub state_stream: String,
    pub stream_db: StreamDb,
    pub base_url: Arc<str>,
}

impl StreamServer {
    pub async fn start(bind: SocketAddr, state_stream: impl Into<String>) -> Result<Self> {
        Self::start_with_dir(bind, state_stream, default_storage_dir()).await
    }

    pub async fn start_with_dir(
        bind: SocketAddr,
        state_stream: impl Into<String>,
        storage_dir: PathBuf,
    ) -> Result<Self> {
        let state_stream = state_stream.into();
        let storage = Arc::new(
            FileStorage::new(&storage_dir, MAX_TOTAL_BYTES, MAX_STREAM_BYTES, true)
                .map_err(|e| anyhow::anyhow!("create file storage at {}: {e}", storage_dir.display()))?,
        );

        // Create state stream if it doesn't exist (idempotent)
        let _ = storage.create_stream(
            &state_stream,
            StreamConfig::new("application/json".to_string()),
        );

        let stream_db = StreamDb::new();

        let router = build_app_router(storage.clone());
        let listener = TcpListener::bind(bind).await?;
        let local_addr = listener.local_addr()?;
        let base_url: Arc<str> = format!("http://{local_addr}").into();

        tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });

        let ds = Self {
            storage,
            state_stream,
            stream_db,
            base_url,
        };

        // Replay existing state from disk into StreamDb
        ds.rebuild_state().await?;

        Ok(ds)
    }

    pub fn stream_url(&self, stream_name: &str) -> String {
        format!("{}/streams/{stream_name}", self.base_url)
    }

    pub async fn append_json<T: serde::Serialize>(
        &self,
        stream_name: &str,
        value: &T,
    ) -> Result<()> {
        let payload = serde_json::to_vec(value)?;
        self.storage
            .append(
                stream_name,
                Bytes::from(payload.clone()),
                "application/json",
            )
            .context("append to durable stream")?;
        if stream_name == self.state_stream {
            self.stream_db.apply_json_message(&payload).await?;
        }
        Ok(())
    }

    pub async fn rebuild_state(&self) -> Result<()> {
        let read = self
            .storage
            .read(
                &self.state_stream,
                &durable_streams_server::protocol::offset::Offset::start(),
            )
            .context("read state stream")?;
        for message in read.messages {
            self.stream_db.apply_json_message(&message).await?;
        }
        Ok(())
    }
}

fn build_app_router(storage: Arc<FileStorage>) -> Router {
    let config = Config::default();
    let ds_router = build_router(storage.clone(), &config);
    Router::new().merge(streams_alias(storage)).merge(ds_router)
}

fn streams_alias(storage: Arc<FileStorage>) -> Router {
    let config = Config::default();
    Router::new()
        .route(
            "/streams/{name}",
            get(handlers::get::read_stream::<FileStorage>)
                .put(handlers::put::create_stream::<FileStorage>)
                .head(handlers::head::stream_metadata::<FileStorage>)
                .post(handlers::post::append_data::<FileStorage>)
                .delete(handlers::delete::delete_stream::<FileStorage>),
        )
        .layer(Extension(
            durable_streams_server::config::SseReconnectInterval(
                config.sse_reconnect_interval_secs,
            ),
        ))
        .layer(Extension(durable_streams_server::config::LongPollTimeout(
            config.long_poll_timeout,
        )))
        .layer(axum::middleware::from_fn(
            middleware::security::add_security_headers,
        ))
        .with_state(storage)
}
