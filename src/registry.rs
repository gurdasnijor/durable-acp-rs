use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentEntry {
    pub name: String,
    pub api_url: String,
    pub logical_connection_id: String,
    pub registered_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Registry {
    pub agents: Vec<AgentEntry>,
}

fn registry_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("durable-acp")
        .join("registry.json")
}

pub fn read_registry() -> Result<Registry> {
    let path = registry_path();
    if !path.exists() {
        return Ok(Registry::default());
    }
    let data = std::fs::read_to_string(&path).context("read registry")?;
    serde_json::from_str(&data).context("parse registry")
}

fn write_registry(registry: &Registry) -> Result<()> {
    let path = registry_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).context("create registry dir")?;
    }
    let data = serde_json::to_string_pretty(registry)?;
    std::fs::write(&path, data).context("write registry")
}

pub fn register(entry: AgentEntry) -> Result<()> {
    let mut registry = read_registry().unwrap_or_default();
    registry.agents.retain(|a| a.name != entry.name);
    registry.agents.push(entry);
    write_registry(&registry)
}

pub fn unregister(name: &str) -> Result<()> {
    let mut registry = read_registry().unwrap_or_default();
    registry.agents.retain(|a| a.name != name);
    write_registry(&registry)
}
