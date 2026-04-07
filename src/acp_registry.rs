//! Integration with the ACP Agent Registry.
//!
//! Fetches agent metadata from <https://cdn.agentclientprotocol.com/registry/v1/latest/registry.json>
//! and resolves agent IDs to runnable commands for the current platform.

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::collections::HashMap;

const REGISTRY_URL: &str =
    "https://cdn.agentclientprotocol.com/registry/v1/latest/registry.json";

#[derive(Debug, Deserialize)]
pub struct RemoteRegistry {
    pub agents: Vec<RemoteAgent>,
}

#[derive(Debug, Deserialize)]
pub struct RemoteAgent {
    pub id: String,
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub distribution: Distribution,
}

#[derive(Debug, Default, Deserialize)]
pub struct Distribution {
    pub npx: Option<NpxDist>,
    pub uvx: Option<UvxDist>,
    pub binary: Option<HashMap<String, BinaryDist>>,
}

#[derive(Debug, Deserialize)]
pub struct NpxDist {
    pub package: String,
    #[serde(default)]
    pub args: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct UvxDist {
    pub package: String,
    #[serde(default)]
    pub args: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct BinaryDist {
    pub cmd: String,
    #[serde(default)]
    pub args: Vec<String>,
    pub archive: Option<String>,
}

impl RemoteAgent {
    /// Resolve this agent to a command + args for the current platform.
    pub fn resolve_command(&self) -> Result<Vec<String>> {
        // Prefer npx > uvx > binary
        if let Some(npx) = &self.distribution.npx {
            let mut cmd = vec!["npx".to_string(), npx.package.clone()];
            cmd.extend(npx.args.iter().cloned());
            return Ok(cmd);
        }
        if let Some(uvx) = &self.distribution.uvx {
            let mut cmd = vec!["uvx".to_string(), uvx.package.clone()];
            cmd.extend(uvx.args.iter().cloned());
            return Ok(cmd);
        }
        if let Some(binaries) = &self.distribution.binary {
            let platform = current_platform();
            if let Some(bin) = binaries.get(&platform) {
                let mut cmd = vec![bin.cmd.clone()];
                cmd.extend(bin.args.iter().cloned());
                return Ok(cmd);
            }
            bail!(
                "No binary for platform '{}'. Available: {:?}",
                platform,
                binaries.keys().collect::<Vec<_>>()
            );
        }
        bail!("Agent '{}' has no supported distribution", self.id)
    }
}

pub async fn fetch_registry() -> Result<RemoteRegistry> {
    let client = reqwest::Client::new();
    client
        .get(REGISTRY_URL)
        .send()
        .await
        .context("fetch ACP registry")?
        .json()
        .await
        .context("parse ACP registry")
}

pub fn current_platform() -> String {
    let arch = if cfg!(target_arch = "aarch64") {
        "aarch64"
    } else {
        "x86_64"
    };
    let os = if cfg!(target_os = "macos") {
        "darwin"
    } else if cfg!(target_os = "windows") {
        "windows"
    } else {
        "linux"
    };
    format!("{os}-{arch}")
}
