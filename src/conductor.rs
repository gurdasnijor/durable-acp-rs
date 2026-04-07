use std::sync::Arc;

use anyhow::Result;
use agent_client_protocol::{
    self as acp, ContentBlock, CreateTerminalRequest, PermissionOption, PromptRequest,
    PromptResponse, RequestPermissionOutcome, RequestPermissionRequest, SessionNotification,
    SessionUpdate, TerminalOutputRequest, ToolCall, ToolCallUpdate,
};
use sacp::{
    Agent, Client, Conductor, ConnectTo, Proxy, on_receive_notification, on_receive_request,
};
use sacp_conductor::{ConductorImpl, McpBridgeMode, ProxiesAndAgent};
use sacp_tokio::AcpAgent;
use serde_json::json;

use crate::app::AppState;
use crate::state::{
    ChunkType, PendingRequestState, PermissionOptionRow, PermissionRow, PromptTurnState,
    TerminalRow, TerminalState,
};

#[derive(Clone)]
pub struct DurableStateProxy {
    pub app: Arc<AppState>,
}

impl ConnectTo<Conductor> for DurableStateProxy {
    async fn connect_to(self, client: impl ConnectTo<Proxy>) -> Result<(), sacp::Error> {
        let app = self.app.clone();
        sacp::Proxy
            .builder()
            .name("durable-state")
            .on_receive_request_from(
                Client,
                {
                    let app = app.clone();
                    async move |req: PromptRequest, responder, cx| {
                        app.set_proxy_connection(cx.clone()).await;
                        app.enqueue_prompt(req, responder)
                            .await
                            .map_err(|e| sacp::util::internal_error(e.to_string()))?;
                        drive_queue(app.clone(), cx.clone())?;
                        Ok(())
                    }
                },
                on_receive_request!(),
            )
            .on_receive_request_from(
                Client,
                {
                    let app = app.clone();
                    async move |req: sacp::schema::NewSessionRequest, responder, cx| {
                        app.set_proxy_connection(cx.clone()).await;
                        cx.send_request_to(Agent, req).on_receiving_result({
                            let app = app.clone();
                            async move |result| {
                                if let Ok(ref response) = result {
                                    let session_id = response.session_id.0.to_string();
                                    app.update_connection(|row| {
                                        row.state = crate::state::ConnectionState::Attached;
                                        row.latest_session_id = Some(session_id.clone());
                                    })
                                    .await
                                    .map_err(|e| sacp::util::internal_error(e.to_string()))?;
                                }
                                responder.respond_with_result(result)
                            }
                        })?;
                        Ok(())
                    }
                },
                on_receive_request!(),
            )
            .on_receive_notification_from(
                Agent,
                {
                    let app = app.clone();
                    async move |notif: SessionNotification, cx| {
                        app.set_proxy_connection(cx.clone()).await;
                        handle_session_notification(&app, &notif)
                            .await
                            .map_err(|e| sacp::util::internal_error(e.to_string()))?;
                        cx.send_notification_to(Client, notif)?;
                        Ok(())
                    }
                },
                on_receive_notification!(),
            )
            .on_receive_request_from(
                Agent,
                {
                    let app = app.clone();
                    async move |req: RequestPermissionRequest, responder, cx| {
                        app.set_proxy_connection(cx.clone()).await;
                        let request_id = responder.id().to_string();
                        let prompt_turn_id = app
                            .session_to_prompt_turn
                            .read()
                            .await
                            .get(req.session_id.0.as_ref())
                            .cloned()
                            .unwrap_or_default();
                        let permission = PermissionRow {
                            request_id: request_id.clone(),
                            jsonrpc_id: responder.id(),
                            logical_connection_id: app.logical_connection_id.clone(),
                            session_id: req.session_id.0.to_string(),
                            prompt_turn_id,
                            title: Some(
                                req.tool_call
                                    .fields
                                    .title
                                    .clone()
                                    .unwrap_or_else(|| "permission".to_string()),
                            ),
                            tool_call_id: Some(req.tool_call.tool_call_id.0.to_string()),
                            options: Some(
                                req.options.iter().map(to_permission_option_row).collect(),
                            ),
                            state: PendingRequestState::Pending,
                            outcome: None,
                            created_at: crate::app::now_ms(),
                            resolved_at: None,
                        };
                        app.write_state_event(
                            "permission",
                            "insert",
                            &request_id,
                            Some(&permission),
                        )
                        .await
                        .map_err(|e| sacp::util::internal_error(e.to_string()))?;

                        cx.send_request_to(Client, req).on_receiving_result({
                            let app = app.clone();
                            async move |result| {
                                let outcome = result.as_ref().ok().map(|r| match &r.outcome {
                                    RequestPermissionOutcome::Cancelled => "cancelled".to_string(),
                                    RequestPermissionOutcome::Selected(sel) => {
                                        sel.option_id.0.to_string()
                                    }
                                    _ => "unknown".to_string(),
                                });
                                let mut snapshot = app.durable_streams.stream_db.snapshot().await;
                                if let Some(row) = snapshot.permissions.get_mut(&request_id) {
                                    row.state = PendingRequestState::Resolved;
                                    row.outcome = outcome;
                                    row.resolved_at = Some(crate::app::now_ms());
                                    let updated = row.clone();
                                    drop(snapshot);
                                    app.write_state_event(
                                        "permission",
                                        "update",
                                        &request_id,
                                        Some(&updated),
                                    )
                                    .await
                                    .map_err(|e| sacp::util::internal_error(e.to_string()))?;
                                }
                                responder.respond_with_result(result)
                            }
                        })?;
                        Ok(())
                    }
                },
                on_receive_request!(),
            )
            .on_receive_request_from(
                Agent,
                {
                    let app = app.clone();
                    async move |req: CreateTerminalRequest, responder, cx| {
                        let terminal_id = uuid::Uuid::new_v4().to_string();
                        let prompt_turn_id = app
                            .session_to_prompt_turn
                            .read()
                            .await
                            .get(req.session_id.0.as_ref())
                            .cloned();
                        let row = TerminalRow {
                            terminal_id: terminal_id.clone(),
                            logical_connection_id: app.logical_connection_id.clone(),
                            session_id: req.session_id.0.to_string(),
                            prompt_turn_id,
                            state: TerminalState::Open,
                            command: Some(req.command.clone()),
                            exit_code: None,
                            signal: None,
                            created_at: crate::app::now_ms(),
                            updated_at: crate::app::now_ms(),
                        };
                        app.write_state_event("terminal", "insert", &terminal_id, Some(&row))
                            .await
                            .map_err(|e| sacp::util::internal_error(e.to_string()))?;
                        cx.send_request_to(Client, req)
                            .forward_response_to(responder)?;
                        Ok(())
                    }
                },
                on_receive_request!(),
            )
            .on_receive_request_from(
                Agent,
                {
                    let app = app.clone();
                    async move |req: TerminalOutputRequest, responder, cx| {
                        let prompt_turn_id = app
                            .session_to_prompt_turn
                            .read()
                            .await
                            .get(req.session_id.0.as_ref())
                            .cloned();
                        if let Some(prompt_turn_id) = prompt_turn_id {
                            app.record_chunk(
                                &prompt_turn_id,
                                ChunkType::ToolResult,
                                serde_json::to_string(&req).unwrap_or_default(),
                            )
                            .await
                            .map_err(|e| sacp::util::internal_error(e.to_string()))?;
                        }
                        cx.send_request_to(Client, req)
                            .forward_response_to(responder)?;
                        Ok(())
                    }
                },
                on_receive_request!(),
            )
            .connect_to(client)
            .await
    }
}

pub fn drive_queue(app: Arc<AppState>, cx: sacp::ConnectionTo<Conductor>) -> Result<(), sacp::Error> {
    cx.clone().spawn(async move {
        loop {
            let Some(queued) = app.take_next_prompt().await else {
                break;
            };

            app.session_to_prompt_turn.write().await.insert(
                queued.prompt.session_id.0.to_string(),
                queued.prompt_turn.prompt_turn_id.clone(),
            );

            let mut active_turn = queued.prompt_turn.clone();
            active_turn.state = PromptTurnState::Active;
            app.write_state_event(
                "prompt_turn",
                "update",
                active_turn.prompt_turn_id.clone(),
                Some(&active_turn),
            )
            .await
            .map_err(|e| sacp::util::internal_error(e.to_string()))?;

            let prompt_turn_id = queued.prompt_turn.prompt_turn_id.clone();
            let responder = queued.responder;
            cx.send_request_to(Agent, queued.prompt)
                .on_receiving_result({
                    let app = app.clone();
                    async move |result| {
                        match &result {
                            Ok(response) => {
                                app.record_chunk(
                                    &prompt_turn_id,
                                    ChunkType::Stop,
                                    json!({ "stopReason": response.stop_reason }).to_string(),
                                )
                                .await
                                .map_err(|e| sacp::util::internal_error(e.to_string()))?;
                                app.finish_prompt_turn(
                                    &prompt_turn_id,
                                    format!("{:?}", response.stop_reason).to_lowercase(),
                                    map_stop_reason(response),
                                )
                                .await
                                .map_err(|e| sacp::util::internal_error(e.to_string()))?;
                            }
                            Err(error) => {
                                app.record_chunk(
                                    &prompt_turn_id,
                                    ChunkType::Error,
                                    error.to_string(),
                                )
                                .await
                                .map_err(|e| sacp::util::internal_error(e.to_string()))?;
                                app.finish_prompt_turn(
                                    &prompt_turn_id,
                                    error.to_string(),
                                    PromptTurnState::Broken,
                                )
                                .await
                                .map_err(|e| sacp::util::internal_error(e.to_string()))?;
                            }
                        }
                        responder.respond_with_result(result)
                    }
                })?;
            break;
        }
        Ok(())
    })?;
    Ok(())
}

async fn handle_session_notification(app: &AppState, notif: &SessionNotification) -> Result<()> {
    let prompt_turn_id = app
        .session_to_prompt_turn
        .read()
        .await
        .get(notif.session_id.0.as_ref())
        .cloned();
    let Some(prompt_turn_id) = prompt_turn_id else {
        return Ok(());
    };

    match &notif.update {
        SessionUpdate::UserMessageChunk(chunk) | SessionUpdate::AgentMessageChunk(chunk) => {
            app.record_chunk(
                &prompt_turn_id,
                ChunkType::Text,
                content_to_string(&chunk.content),
            )
            .await?;
        }
        SessionUpdate::AgentThoughtChunk(chunk) => {
            app.record_chunk(
                &prompt_turn_id,
                ChunkType::Thinking,
                content_to_string(&chunk.content),
            )
            .await?;
        }
        SessionUpdate::ToolCall(call) => {
            persist_tool_call(app, &prompt_turn_id, call).await?;
        }
        SessionUpdate::ToolCallUpdate(update) => {
            persist_tool_call_update(app, &prompt_turn_id, update).await?;
        }
        other => {
            app.record_chunk(
                &prompt_turn_id,
                ChunkType::Text,
                serde_json::to_string(other)?,
            )
            .await?;
        }
    }

    Ok(())
}

async fn persist_tool_call(app: &AppState, prompt_turn_id: &str, call: &ToolCall) -> Result<()> {
    app.record_chunk(
        prompt_turn_id,
        ChunkType::ToolCall,
        serde_json::to_string(call)?,
    )
    .await
}

async fn persist_tool_call_update(
    app: &AppState,
    prompt_turn_id: &str,
    update: &ToolCallUpdate,
) -> Result<()> {
    app.record_chunk(
        prompt_turn_id,
        ChunkType::ToolResult,
        serde_json::to_string(update)?,
    )
    .await
}

fn content_to_string(block: &ContentBlock) -> String {
    match block {
        ContentBlock::Text(text) => text.text.clone(),
        other => serde_json::to_string(other).unwrap_or_default(),
    }
}

fn to_permission_option_row(option: &PermissionOption) -> PermissionOptionRow {
    PermissionOptionRow {
        option_id: option.option_id.0.to_string(),
        name: option.name.clone(),
        kind: format!("{:?}", option.kind).to_lowercase(),
    }
}

fn map_stop_reason(response: &PromptResponse) -> PromptTurnState {
    match response.stop_reason {
        acp::StopReason::Cancelled => PromptTurnState::Cancelled,
        _ => PromptTurnState::Completed,
    }
}

pub fn build_conductor(app: Arc<AppState>, agent: AcpAgent) -> ConductorImpl<sacp::Agent> {
    ConductorImpl::new_agent(
        "durable-acp".to_string(),
        ProxiesAndAgent::new(agent).proxy(DurableStateProxy { app }),
        McpBridgeMode::default(),
    )
}

/// Like `build_conductor` but also injects the `list_agents` / `prompt_agent`
/// MCP tools so the wrapped agent can discover and message peer conductors.
pub fn build_conductor_with_peer_mcp(
    app: Arc<AppState>,
    agent: AcpAgent,
) -> ConductorImpl<sacp::Agent> {
    ConductorImpl::new_agent(
        "durable-acp".to_string(),
        ProxiesAndAgent::new(agent)
            .proxy(DurableStateProxy { app })
            .proxy(crate::peer_mcp::PeerMcpProxy),
        McpBridgeMode::default(),
    )
}
