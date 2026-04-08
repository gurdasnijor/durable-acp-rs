//! Minimal ACP test agent over stdio.
//!
//! Handles Initialize, NewSession, and Prompt requests.
//! Used by /acp WebSocket endpoint integration tests.

use agent_client_protocol::{self as acp};
use sacp::{Client, ConnectTo};

struct TestAgent;

impl ConnectTo<Client> for TestAgent {
    async fn connect_to(
        self,
        client: impl ConnectTo<sacp::Agent>,
    ) -> Result<(), sacp::Error> {
        sacp::Agent
            .builder()
            .name("testy")
            .on_receive_request(
                async |req: acp::InitializeRequest, responder, _cx| {
                    responder.respond(acp::InitializeResponse::new(req.protocol_version))
                },
                sacp::on_receive_request!(),
            )
            .on_receive_request(
                async |_req: sacp::schema::NewSessionRequest, responder, _cx| {
                    responder.respond(sacp::schema::NewSessionResponse::new(
                        acp::SessionId::new(uuid::Uuid::new_v4().to_string()),
                    ))
                },
                sacp::on_receive_request!(),
            )
            .on_receive_request(
                async |_req: acp::PromptRequest, responder, _cx| {
                    responder.respond(acp::PromptResponse::new(acp::StopReason::EndTurn))
                },
                sacp::on_receive_request!(),
            )
            .connect_to(client)
            .await
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    TestAgent
        .connect_to(sacp_tokio::Stdio::new())
        .await?;
    Ok(())
}
