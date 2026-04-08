//! Bare durable streams server for integration testing.
//! Usage: ds_server [port]

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let port: u16 = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(14000);
    let bind = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    let tmp = std::env::temp_dir().join(format!("ds-test-{}", std::process::id()));
    std::fs::create_dir_all(&tmp)?;

    let ds = durable_acp_rs::stream_server::StreamServer::start_with_dir(
        bind, "durable-acp-state", tmp,
    ).await?;

    eprintln!("Stream URL: {}", ds.stream_url("durable-acp-state"));
    tokio::signal::ctrl_c().await?;
    Ok(())
}
