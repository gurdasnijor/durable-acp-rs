use std::sync::Arc;

use anyhow::Result;
use agent_client_protocol::{
    self as acp, ContentBlock, CreateTerminalRequest, PromptRequest, RequestPermissionOutcome,
    RequestPermissionRequest, SessionNotification, SessionUpdate, TerminalOutputRequest,
};
use sacp::{Agent, Client, Conductor, ConnectTo, Proxy, on_receive_notification, on_receive_request};
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
                        app.capture_proxy_connection(cx.clone()).await;
                        app.enqueue_prompt(req, responder)
                            .await
                            .map_err(|e| sacp::util::internal_error(e.to_string()))?;
                        drive_queue(app.clone(), cx)?;
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
                        app.capture_proxy_connection(cx.clone()).await;
                        let cwd = req.cwd.to_string_lossy().to_string();
                        cx.send_request_to(Agent, req).on_receiving_result({
                            let app = app.clone();
                            async move |result| {
                                if let Ok(ref response) = result {
                                    let session_id = response.session_id.0.to_string();
                                    let _ = app
                                        .update_connection(|row| {
                                            row.state = crate::state::ConnectionState::Attached;
                                            row.latest_session_id = Some(session_id.clone());
                                            row.cwd = Some(cwd.clone());
                                        })
                                        .await;
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
                        let _ = handle_session_notification(&app, &notif).await;
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
                                req.tool_call.fields.title.clone().unwrap_or_else(|| "permission".to_string()),
                            ),
                            tool_call_id: Some(req.tool_call.tool_call_id.0.to_string()),
                            options: Some(req.options.iter().map(|o| PermissionOptionRow {
                                option_id: o.option_id.0.to_string(),
                                name: o.name.clone(),
                                kind: format!("{:?}", o.kind).to_lowercase(),
                            }).collect()),
                            state: PendingRequestState::Pending,
                            outcome: None,
                            created_at: crate::app::now_ms(),
                            resolved_at: None,
                        };
                        let _ = app
                            .write_state_event("permission", "insert", &request_id, Some(&permission))
                            .await;

                        cx.send_request_to(Client, req).on_receiving_result({
                            let app = app.clone();
                            async move |result| {
                                let outcome = result.as_ref().ok().map(|r| match &r.outcome {
                                    RequestPermissionOutcome::Cancelled => "cancelled".to_string(),
                                    RequestPermissionOutcome::Selected(sel) => sel.option_id.0.to_string(),
                                    _ => "unknown".to_string(),
                                });
                                let mut snapshot = app.durable_streams.stream_db.snapshot().await;
                                if let Some(row) = snapshot.permissions.get_mut(&request_id) {
                                    row.state = PendingRequestState::Resolved;
                                    row.outcome = outcome;
                                    row.resolved_at = Some(crate::app::now_ms());
                                    let updated = row.clone();
                                    drop(snapshot);
                                    let _ = app
                                        .write_state_event("permission", "update", &request_id, Some(&updated))
                                        .await;
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
                            .session_to_prompt_turn.read().await
                            .get(req.session_id.0.as_ref()).cloned();
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
                        let _ = app.write_state_event("terminal", "insert", &terminal_id, Some(&row)).await;
                        cx.send_request_to(Client, req).forward_response_to(responder)?;
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
                            .session_to_prompt_turn.read().await
                            .get(req.session_id.0.as_ref()).cloned();
                        if let Some(prompt_turn_id) = prompt_turn_id {
                            let _ = app.record_chunk(
                                &prompt_turn_id,
                                ChunkType::ToolResult,
                                serde_json::to_string(&req).unwrap_or_default(),
                            ).await;
                        }
                        cx.send_request_to(Client, req).forward_response_to(responder)?;
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
        let Some(queued) = app.take_next_prompt().await else {
            return Ok(());
        };

        app.session_to_prompt_turn.write().await.insert(
            queued.prompt.session_id.0.to_string(),
            queued.prompt_turn.prompt_turn_id.clone(),
        );

        let mut active_turn = queued.prompt_turn.clone();
        active_turn.state = PromptTurnState::Active;
        let _ = app
            .write_state_event("prompt_turn", "update", active_turn.prompt_turn_id.clone(), Some(&active_turn))
            .await;

        let prompt_turn_id = queued.prompt_turn.prompt_turn_id.clone();
        cx.send_request_to(Agent, queued.prompt)
            .on_receiving_result({
                let app = app.clone();
                async move |result| {
                    match &result {
                        Ok(response) => {
                            let _ = app.record_chunk(&prompt_turn_id, ChunkType::Stop, json!({ "stopReason": response.stop_reason }).to_string()).await;
                            let state = if response.stop_reason == acp::StopReason::Cancelled { PromptTurnState::Cancelled } else { PromptTurnState::Completed };
                            let _ = app.finish_prompt_turn(&prompt_turn_id, format!("{:?}", response.stop_reason).to_lowercase(), state).await;
                        }
                        Err(error) => {
                            let _ = app.record_chunk(&prompt_turn_id, ChunkType::Error, error.to_string()).await;
                            let _ = app.finish_prompt_turn(&prompt_turn_id, error.to_string(), PromptTurnState::Broken).await;
                        }
                    }
                    queued.responder.respond_with_result(result)
                }
            })?;
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
            app.record_chunk(&prompt_turn_id, ChunkType::Text, content_to_string(&chunk.content)).await?;
        }
        SessionUpdate::AgentThoughtChunk(chunk) => {
            app.record_chunk(&prompt_turn_id, ChunkType::Thinking, content_to_string(&chunk.content)).await?;
        }
        SessionUpdate::ToolCall(call) => {
            app.record_chunk(&prompt_turn_id, ChunkType::ToolCall, serde_json::to_string(call)?).await?;
        }
        SessionUpdate::ToolCallUpdate(update) => {
            app.record_chunk(&prompt_turn_id, ChunkType::ToolResult, serde_json::to_string(update)?).await?;
        }
        other => {
            app.record_chunk(&prompt_turn_id, ChunkType::Text, serde_json::to_string(other)?).await?;
        }
    }
    Ok(())
}

fn content_to_string(block: &ContentBlock) -> String {
    match block {
        ContentBlock::Text(text) => text.text.clone(),
        other => serde_json::to_string(other).unwrap_or_default(),
    }
}
