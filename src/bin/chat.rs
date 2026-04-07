//! Interactive ACP chat client.
//!
//! Spawns the durable-acp conductor as a subprocess (which in turn spawns the agent),
//! then gives you an interactive prompt loop.
//!
//! Usage:
//!   cargo run --bin chat
//!   cargo run --bin chat -- npx @agentclientprotocol/claude-agent-acp

use std::io::Write as _;

use agent_client_protocol::{
    ContentBlock, ContentChunk, RequestPermissionOutcome, RequestPermissionRequest,
    RequestPermissionResponse, SelectedPermissionOutcome, SessionNotification, SessionUpdate,
    StopReason,
};
use sacp::{Client, Dispatch, SessionMessage, on_receive_request};
use sacp_tokio::AcpAgent;

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .without_time()
        .with_writer(std::io::stderr)
        .init();

    let args: Vec<String> = std::env::args().skip(1).collect();
    let agent_args = if args.is_empty() {
        vec![
            "npx".to_string(),
            "@agentclientprotocol/claude-agent-acp".to_string(),
        ]
    } else {
        args
    };

    eprintln!("Agent: {}", agent_args.join(" "));

    let agent = AcpAgent::from_args(agent_args)?;

    let local_set = tokio::task::LocalSet::new();
    local_set
        .run_until(async {
            Client
            .builder()
            .name("durable-acp-chat")
            .on_receive_request(
                async |req: RequestPermissionRequest, responder, cx| {
                    let title = req
                        .tool_call
                        .fields
                        .title
                        .as_deref()
                        .unwrap_or("Permission requested");

                    eprintln!("\n\x1b[33m[permission]\x1b[0m {title}");
                    for (i, opt) in req.options.iter().enumerate() {
                        eprintln!("  \x1b[36m[{}]\x1b[0m {}", i + 1, opt.name);
                    }

                    let options = req.options.clone();

                    // Read user choice on a blocking thread
                    cx.spawn(async move {
                        eprint!("Choose (1-{}, or 'n' to cancel): ", options.len());
                        std::io::Write::flush(&mut std::io::stderr()).ok();

                        let choice = tokio::task::spawn_blocking(|| {
                            let mut input = String::new();
                            std::io::stdin().read_line(&mut input).ok();
                            input.trim().to_string()
                        })
                        .await
                        .unwrap_or_default();

                        let outcome = if choice == "n" || choice == "N" || choice.is_empty() {
                            eprintln!("\x1b[31m[denied]\x1b[0m");
                            RequestPermissionOutcome::Cancelled
                        } else if let Ok(n) = choice.parse::<usize>() {
                            if n >= 1 && n <= options.len() {
                                let opt = &options[n - 1];
                                eprintln!("\x1b[32m[approved: {}]\x1b[0m", opt.name);
                                RequestPermissionOutcome::Selected(
                                    SelectedPermissionOutcome::new(opt.option_id.clone()),
                                )
                            } else {
                                // Default: approve first option
                                let opt = &options[0];
                                eprintln!("\x1b[32m[approved: {}]\x1b[0m", opt.name);
                                RequestPermissionOutcome::Selected(
                                    SelectedPermissionOutcome::new(opt.option_id.clone()),
                                )
                            }
                        } else {
                            // Any other input: approve first option
                            if let Some(opt) = options.first() {
                                eprintln!("\x1b[32m[approved: {}]\x1b[0m", opt.name);
                                RequestPermissionOutcome::Selected(
                                    SelectedPermissionOutcome::new(opt.option_id.clone()),
                                )
                            } else {
                                RequestPermissionOutcome::Cancelled
                            }
                        };

                        responder.respond(RequestPermissionResponse::new(outcome))
                    })?;
                    Ok(())
                },
                on_receive_request!(),
            )
            .connect_with(agent, async |connection| {
                connection
                    .send_request(agent_client_protocol::InitializeRequest::new(
                        agent_client_protocol::ProtocolVersion::V1,
                    ))
                    .block_task()
                    .await?;

                connection
                    .build_session_cwd()?
                    .block_task()
                    .run_until(async |mut session| {
                        eprintln!("Session started. Type your prompts (Ctrl-D to quit):\n");

                        let (line_tx, mut line_rx) = tokio::sync::mpsc::channel::<String>(1);
                        std::thread::spawn(move || {
                            let stdin = std::io::stdin();
                            loop {
                                print!("> ");
                                std::io::stdout().flush().ok();
                                let mut line = String::new();
                                match stdin.read_line(&mut line) {
                                    Ok(0) | Err(_) => break,
                                    Ok(_) => {
                                        let trimmed = line.trim().to_string();
                                        if !trimmed.is_empty() {
                                            if line_tx.blocking_send(trimmed).is_err() {
                                                break;
                                            }
                                        }
                                    }
                                }
                            }
                        });

                        while let Some(line) = line_rx.recv().await {
                            session.send_prompt(&line)?;

                            loop {
                                let message = match session.read_update().await {
                                    Ok(msg) => msg,
                                    Err(e) => {
                                        // Skip parse errors from unknown message types
                                        // (e.g. usage_update) — not fatal
                                        let msg = e.to_string();
                                        if msg.contains("Parse error")
                                            || msg.contains("unknown variant")
                                        {
                                            continue;
                                        }
                                        return Err(e.into());
                                    }
                                };
                                match message {
                                    SessionMessage::SessionMessage(dispatch) => {
                                        // Parse SessionNotification manually to handle
                                        // unknown variants (e.g. usage_update) gracefully
                                        if let Dispatch::Notification(ref msg) = dispatch {
                                            let params = serde_json::to_value(msg.params())
                                                .unwrap_or_default();
                                            match serde_json::from_value::<SessionNotification>(
                                                params,
                                            ) {
                                                Ok(notif) => match notif.update {
                                                    SessionUpdate::AgentMessageChunk(
                                                        ContentChunk {
                                                            content: ContentBlock::Text(text),
                                                            ..
                                                        },
                                                    ) => {
                                                        print!("{}", text.text);
                                                        std::io::stdout().flush().ok();
                                                    }
                                                    SessionUpdate::ToolCall(tc) => {
                                                        eprintln!(
                                                            "\n[tool] {} ({})",
                                                            tc.title,
                                                            tc.tool_call_id.0
                                                        );
                                                    }
                                                    SessionUpdate::ToolCallUpdate(tc) => {
                                                        if let Some(title) = &tc.fields.title {
                                                            eprintln!(
                                                                "[update] {} ({})",
                                                                title,
                                                                tc.tool_call_id.0
                                                            );
                                                        }
                                                        if let Some(status) = &tc.fields.status {
                                                            eprintln!(
                                                                "[status] {:?} ({})",
                                                                status,
                                                                tc.tool_call_id.0
                                                            );
                                                        }
                                                    }
                                                    SessionUpdate::AgentThoughtChunk(
                                                        ContentChunk {
                                                            content: ContentBlock::Text(text),
                                                            ..
                                                        },
                                                    ) => {
                                                        eprint!("{}", text.text);
                                                        std::io::stderr().flush().ok();
                                                    }
                                                    SessionUpdate::AgentThoughtChunk(_) => {}
                                                    SessionUpdate::UserMessageChunk(_) => {}
                                                    _ => {
                                                        eprintln!(
                                                            "[update] {:?}",
                                                            std::mem::discriminant(&notif.update)
                                                        );
                                                    }
                                                },
                                                Err(_) => {
                                                    // Unknown notification variant — skip
                                                }
                                            }
                                        }
                                    }
                                    SessionMessage::StopReason(reason) => {
                                        match reason {
                                            StopReason::EndTurn => println!(),
                                            other => eprintln!("\n[stop: {other:?}]"),
                                        }
                                        break;
                                    }
                                    _ => {}
                                }
                            }
                        }

                        Ok(())
                    })
                    .await
            })
            .await?;

            Ok::<_, anyhow::Error>(())
        })
        .await
}
