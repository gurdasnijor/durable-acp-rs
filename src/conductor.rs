//! Conductor composition — assembles the proxy chain.
//!
//! This is the "running_proxies_with_conductor" pattern from the ACP cookbook.
//! Each proxy is a reusable `ConnectTo<Conductor>` component. This module
//! just composes them into a `ConductorImpl`.

use std::sync::Arc;

use sacp_conductor::{ConductorImpl, McpBridgeMode, ProxiesAndAgent};
use sacp_tokio::AcpAgent;

use crate::conductor_state::ConductorState;
use crate::durable_state_proxy::DurableStateProxy;
use crate::peer_mcp::PeerMcpProxy;

/// Build a conductor with the durable state + peer MCP proxy chain.
///
/// Chain: Client → DurableStateProxy → PeerMcpProxy → Agent
pub fn build_conductor(app: Arc<ConductorState>, agent: AcpAgent) -> ConductorImpl<sacp::Agent> {
    ConductorImpl::new_agent(
        "durable-acp".to_string(),
        ProxiesAndAgent::new(agent)
            .proxy(DurableStateProxy { app })
            .proxy(PeerMcpProxy),
        McpBridgeMode::default(),
    )
}
