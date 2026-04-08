//! StreamSubscriber reconnection — verify state materializes after
//! data is written while subscriber is connected.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;

use durable_acp_rs::conductor_state::ConductorState;
use durable_acp_rs::state::{ChunkType, CollectionChange};
use durable_acp_rs::stream_server::StreamServer;
use durable_acp_rs::stream_subscriber::StreamSubscriber;

async fn test_app() -> Arc<ConductorState> {
    let tmp = tempfile::tempdir().unwrap();
    let bind = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0);
    let ds = StreamServer::start_with_dir(bind, "durable-acp-state", tmp.path().to_path_buf())
        .await
        .unwrap();
    std::mem::forget(tmp);
    Arc::new(ConductorState::with_shared_streams(ds).await.unwrap())
}

// ---------------------------------------------------------------------------
// Subscriber sees data written before AND after connect
// ---------------------------------------------------------------------------

#[tokio::test]
async fn subscriber_sees_pre_existing_and_live_data() {
    let app = test_app().await;

    // Write data BEFORE subscriber connects
    app.record_chunk("pt-1", ChunkType::Text, "before connect".into())
        .await
        .unwrap();

    // Connect subscriber
    let mut sub = StreamSubscriber::new(app.state_stream_url());
    sub.preload().await.unwrap();

    // Verify pre-existing data materialized
    let snapshot = sub.stream_db().snapshot().await;
    assert!(
        snapshot.chunks.values().any(|c| c.content == "before connect"),
        "subscriber should see pre-existing chunks"
    );

    // Now write data AFTER subscriber is connected
    let mut rx = sub.stream_db().subscribe_changes();

    app.record_chunk("pt-1", ChunkType::Text, "after connect".into())
        .await
        .unwrap();

    // Wait for live update
    let change = tokio::time::timeout(std::time::Duration::from_secs(3), rx.recv())
        .await
        .expect("timeout waiting for live update")
        .unwrap();
    assert!(matches!(change, CollectionChange::Chunks));

    let snapshot = sub.stream_db().snapshot().await;
    assert!(
        snapshot.chunks.values().any(|c| c.content == "after connect"),
        "subscriber should see live chunks"
    );

    sub.disconnect();
}

// ---------------------------------------------------------------------------
// Multiple subscribers on the same stream see the same state
// ---------------------------------------------------------------------------

#[tokio::test]
async fn multiple_subscribers_see_same_state() {
    let app = test_app().await;
    app.record_chunk("pt-1", ChunkType::Text, "shared".into())
        .await
        .unwrap();

    let url = app.state_stream_url();

    let mut sub_a = StreamSubscriber::new(&url);
    let mut sub_b = StreamSubscriber::new(&url);

    sub_a.preload().await.unwrap();
    sub_b.preload().await.unwrap();

    let snap_a = sub_a.stream_db().snapshot().await;
    let snap_b = sub_b.stream_db().snapshot().await;

    assert_eq!(snap_a.chunks.len(), snap_b.chunks.len());
    assert_eq!(snap_a.connections.len(), snap_b.connections.len());

    sub_a.disconnect();
    sub_b.disconnect();
}

// ---------------------------------------------------------------------------
// Subscriber gracefully handles disconnect
// ---------------------------------------------------------------------------

#[tokio::test]
async fn subscriber_disconnect_is_clean() {
    let app = test_app().await;

    let mut sub = StreamSubscriber::new(app.state_stream_url());
    sub.preload().await.unwrap();

    // Verify it's working
    let snapshot = sub.stream_db().snapshot().await;
    assert!(!snapshot.connections.is_empty());

    // Disconnect — should not panic
    sub.disconnect();

    // StreamDb should still be readable after disconnect (it's in-memory)
    let snapshot = sub.stream_db().snapshot().await;
    assert!(!snapshot.connections.is_empty());
}
