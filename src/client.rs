//! Reusable ACP client — connects to a conductor, manages session lifecycle.
//!
//! Decoupled from any UI. The `AcpClientHandler` trait receives events;
//! the caller provides prompts via the returned channel.
//!
//! ```text
//! AcpClient ──transport──► Conductor ──proxy chain──► Agent
//!    ↕ events (on_ready, on_text, on_error)
//!    ↕ prompts (prompt_tx channel)
//! ```

use std::sync::Arc;

use agent_client_protocol::{self as acp};

/// Events emitted during the ACP client lifecycle.
pub trait AcpClientHandler: Send + Sync + 'static {
    /// Session established — ready for prompts.
    fn on_ready(&self, name: &str);
    /// Agent produced text output (complete response from one prompt).
    fn on_text(&self, name: &str, text: &str);
    /// Client encountered an error.
    fn on_error(&self, name: &str, error: &str);
}

/// Configuration for connecting to one conductor.
pub struct AcpClientConfig {
    pub name: String,
    pub transport: sacp::DynConnectTo<sacp::Client>,
}

/// Handle for sending prompts to a running ACP client.
pub struct AcpClientHandle {
    pub prompt_tx: tokio::sync::mpsc::UnboundedSender<String>,
}

/// Spawn an ACP client on the current `LocalSet`.
///
/// Returns a handle for sending prompts. The client runs until the
/// prompt channel closes or the connection drops.
pub fn spawn_acp_client(
    config: AcpClientConfig,
    handler: Arc<dyn AcpClientHandler>,
) -> AcpClientHandle {
    let (prompt_tx, prompt_rx) = tokio::sync::mpsc::unbounded_channel();
    let name = config.name.clone();
    let handler_err = handler.clone();

    tokio::task::spawn_local(async move {
        if let Err(e) = run_acp_client(config, handler, prompt_rx).await {
            handler_err.on_error(&name, &e.to_string());
        }
    });

    AcpClientHandle { prompt_tx }
}

/// Core ACP client lifecycle — testable without `spawn_local`.
///
/// 1. Connect to conductor via transport
/// 2. Initialize (protocol handshake)
/// 3. Create session
/// 4. Loop: receive prompt from channel → send to agent → read response → emit event
pub async fn run_acp_client(
    config: AcpClientConfig,
    handler: Arc<dyn AcpClientHandler>,
    mut prompt_rx: tokio::sync::mpsc::UnboundedReceiver<String>,
) -> Result<(), sacp::Error> {
    let name = config.name.clone();
    let handler_session = handler.clone();

    sacp::Client
        .builder()
        .name(&format!("{}-client", name))
        .on_receive_request(
            async |req: acp::RequestPermissionRequest, responder, _cx| {
                // Auto-approve first permission option
                let outcome = if let Some(opt) = req.options.first() {
                    acp::RequestPermissionOutcome::Selected(
                        acp::SelectedPermissionOutcome::new(opt.option_id.clone()),
                    )
                } else {
                    acp::RequestPermissionOutcome::Cancelled
                };
                responder.respond(acp::RequestPermissionResponse::new(outcome))
            },
            sacp::on_receive_request!(),
        )
        .connect_with(config.transport, async |cx| {
            cx.send_request(acp::InitializeRequest::new(acp::ProtocolVersion::V1))
                .block_task()
                .await?;

            cx.build_session_cwd()?
                .block_task()
                .run_until(async |mut session| {
                    handler_session.on_ready(&name);

                    while let Some(text) = prompt_rx.recv().await {
                        session.send_prompt(&text)?;
                        let response = session.read_to_string().await?;
                        handler_session.on_text(&name, &response);
                    }
                    Ok(())
                })
                .await
        })
        .await
}
