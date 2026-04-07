//! In-process agent-to-agent routing.
//!
//! When multiple agents run in the same process, peer prompts route through
//! channels instead of HTTP. Each agent registers a prompt handler; the
//! `prompt_agent` MCP tool checks the router before falling back to HTTP.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tokio::sync::{mpsc, oneshot};

/// A request to prompt a peer agent.
pub struct PeerPromptRequest {
    pub text: String,
    pub response_tx: oneshot::Sender<Result<String, String>>,
}

/// Shared router for in-process agent communication.
#[derive(Clone, Default)]
pub struct AgentRouter {
    agents: Arc<Mutex<HashMap<String, mpsc::UnboundedSender<PeerPromptRequest>>>>,
}

impl AgentRouter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register an agent's prompt handler.
    pub fn register(&self, name: String, tx: mpsc::UnboundedSender<PeerPromptRequest>) {
        self.agents.lock().unwrap().insert(name, tx);
    }

    /// Unregister an agent.
    pub fn unregister(&self, name: &str) {
        self.agents.lock().unwrap().remove(name);
    }

    /// Send a prompt to a peer agent. Returns None if the agent isn't registered
    /// (caller should fall back to HTTP).
    pub async fn prompt(&self, name: &str, text: &str) -> Option<Result<String, String>> {
        let tx = {
            let agents = self.agents.lock().unwrap();
            agents.get(name).cloned()
        };

        let tx = tx?;
        let (response_tx, response_rx) = oneshot::channel();
        let req = PeerPromptRequest {
            text: text.to_string(),
            response_tx,
        };

        if tx.send(req).is_err() {
            return Some(Err(format!("agent '{}' channel closed", name)));
        }

        match response_rx.await {
            Ok(result) => Some(result),
            Err(_) => Some(Err(format!("agent '{}' dropped response", name))),
        }
    }

    /// List registered agent names.
    pub fn list(&self) -> Vec<String> {
        self.agents.lock().unwrap().keys().cloned().collect()
    }
}

/// Global router instance. Set by the dashboard before spawning agents.
static GLOBAL_ROUTER: std::sync::OnceLock<AgentRouter> = std::sync::OnceLock::new();

pub fn set_global_router(router: AgentRouter) {
    let _ = GLOBAL_ROUTER.set(router);
}

pub fn global_router() -> Option<&'static AgentRouter> {
    GLOBAL_ROUTER.get()
}
