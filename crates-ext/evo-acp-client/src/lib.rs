//! evo-acp-client — Zed Agent Client Protocol (ACP) client.
//!
//! Auth is delegated to the agent CLI (claude / codex / cursor-agent / gh).

use directories::BaseDirs;
use evo_stdio_rpc::{RpcError, SpawnConfig, StdioRpcClient};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::Arc;

pub const ACP_PROTOCOL_VERSION: &str = "0.1.0";

#[derive(Debug, thiserror::Error)]
pub enum AcpError {
    #[error("rpc: {0}")] Rpc(#[from] RpcError),
    #[error("config: {0}")] Config(String),
    #[error("io: {0}")] Io(#[from] std::io::Error),
}

#[derive(Debug, Clone)]
pub struct AgentProfile {
    pub id: &'static str,
    pub name: &'static str,
    pub bin: &'static str,
    pub args: &'static [&'static str],
    pub install_hint: &'static str,
    pub auth_hint: &'static str,
}

pub const CATALOG: &[AgentProfile] = &[
    AgentProfile {
        id: "claude",
        name: "Claude Agent",
        bin: "claude",
        args: &["--acp"],
        install_hint: "npm install -g @anthropic-ai/claude-code",
        auth_hint: "first run: `claude` opens https://claude.ai/login (your subscription, OAuth)",
    },
    AgentProfile {
        id: "codex",
        name: "Codex CLI",
        bin: "codex",
        args: &["--acp"],
        install_hint: "npm install -g @openai/codex",
        auth_hint: "first run: `codex` opens https://chatgpt.com/oauth (your ChatGPT subscription)",
    },
    AgentProfile {
        id: "cursor",
        name: "Cursor Agent",
        bin: "cursor-agent",
        args: &["--acp"],
        install_hint: "Cursor IDE installs cursor-agent; see https://docs.cursor.com/cli",
        auth_hint: "first run: opens cursor.com/login (your Cursor subscription)",
    },
    AgentProfile {
        id: "copilot",
        name: "GitHub Copilot",
        bin: "gh",
        args: &["copilot", "suggest", "--acp"],
        install_hint: "brew install gh && gh extension install github/gh-copilot",
        auth_hint: "first run: `gh auth login` then `gh copilot` (your Copilot subscription)",
    },
];

pub fn find_agent(id: &str) -> Option<&'static AgentProfile> {
    CATALOG.iter().find(|a| a.id == id)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfig {
    pub id: String,
    pub name: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: Vec<(String, String)>,
    #[serde(default)]
    pub installed_at: Option<chrono::DateTime<chrono::Utc>>,
}

impl AgentConfig {
    pub fn from_profile(p: &AgentProfile) -> Self {
        Self {
            id: p.id.into(), name: p.name.into(), command: p.bin.into(),
            args: p.args.iter().map(|s| s.to_string()).collect(),
            env: Vec::new(),
            installed_at: Some(chrono::Utc::now()),
        }
    }
    pub fn to_spawn(&self) -> SpawnConfig {
        SpawnConfig {
            command: PathBuf::from(&self.command),
            args: self.args.clone(),
            env: self.env.clone(),
            cwd: None,
        }
    }
}

fn home() -> Result<PathBuf, AcpError> {
    Ok(BaseDirs::new()
        .ok_or_else(|| AcpError::Config("no home dir".into()))?
        .home_dir().to_path_buf())
}
pub fn agents_dir() -> Result<PathBuf, AcpError> { Ok(home()?.join(".evoclaw/agents")) }
pub fn agent_config_path(id: &str) -> Result<PathBuf, AcpError> {
    Ok(agents_dir()?.join(format!("{id}.toml")))
}

pub async fn save_agent(cfg: &AgentConfig) -> Result<PathBuf, AcpError> {
    let dir = agents_dir()?;
    tokio::fs::create_dir_all(&dir).await?;
    let path = dir.join(format!("{}.toml", cfg.id));
    let s = toml::to_string_pretty(cfg).map_err(|e| AcpError::Config(e.to_string()))?;
    tokio::fs::write(&path, s).await?;
    Ok(path)
}

pub async fn load_agent(id: &str) -> Result<AgentConfig, AcpError> {
    let path = agent_config_path(id)?;
    let s = tokio::fs::read_to_string(&path).await?;
    toml::from_str(&s).map_err(|e| AcpError::Config(e.to_string()))
}

pub async fn list_agents() -> Result<Vec<AgentConfig>, AcpError> {
    let dir = agents_dir()?;
    if !dir.exists() { return Ok(Vec::new()); }
    let mut out = Vec::new();
    let mut entries = tokio::fs::read_dir(&dir).await?;
    while let Some(e) = entries.next_entry().await? {
        let p = e.path();
        if p.extension().and_then(|s| s.to_str()) != Some("toml") { continue; }
        if let Ok(s) = tokio::fs::read_to_string(&p).await {
            if let Ok(cfg) = toml::from_str::<AgentConfig>(&s) { out.push(cfg); }
        }
    }
    Ok(out)
}

pub async fn remove_agent(id: &str) -> Result<(), AcpError> {
    let path = agent_config_path(id)?;
    if path.exists() { tokio::fs::remove_file(&path).await?; }
    Ok(())
}

pub struct AcpClient {
    rpc: Arc<StdioRpcClient>,
    initialized: tokio::sync::Mutex<bool>,
}

impl AcpClient {
    pub fn new() -> Self {
        Self { rpc: Arc::new(StdioRpcClient::new()), initialized: tokio::sync::Mutex::new(false) }
    }

    pub async fn spawn(&self, cfg: &AgentConfig) -> Result<(), AcpError> {
        self.rpc.spawn(cfg.to_spawn()).await?;
        Ok(())
    }

    pub async fn initialize(&self, client_name: &str, client_version: &str) -> Result<Value, AcpError> {
        let mut init = self.initialized.lock().await;
        if *init { return Ok(Value::Null); }
        let result = self.rpc.call("initialize", json!({
            "protocolVersion": ACP_PROTOCOL_VERSION,
            "clientInfo": { "name": client_name, "version": client_version },
            "capabilities": { "fs": false }
        })).await?;
        self.rpc.notify("notifications/initialized", json!({})).await.ok();
        *init = true;
        Ok(result)
    }

    pub async fn new_session(&self) -> Result<String, AcpError> {
        let r = self.rpc.call("session/new", json!({})).await?;
        Ok(r.get("sessionId").and_then(|v| v.as_str()).unwrap_or("").to_string())
    }

    pub async fn prompt(&self, session_id: &str, text: &str) -> Result<Value, AcpError> {
        Ok(self.rpc.call("session/prompt", json!({
            "sessionId": session_id,
            "prompt": [{ "type": "text", "text": text }]
        })).await?)
    }

    pub async fn cancel(&self, session_id: &str) -> Result<(), AcpError> {
        self.rpc.notify("session/cancel", json!({"sessionId": session_id})).await?;
        Ok(())
    }

    pub async fn shutdown(&self) -> Result<(), AcpError> {
        self.rpc.shutdown().await?;
        Ok(())
    }
}

impl Default for AcpClient {
    fn default() -> Self { Self::new() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_has_4_named_agents() {
        let ids: Vec<_> = CATALOG.iter().map(|a| a.id).collect();
        assert_eq!(ids, vec!["claude", "codex", "cursor", "copilot"]);
    }

    #[test]
    fn find_agent_lookup_works() {
        assert_eq!(find_agent("claude").unwrap().bin, "claude");
        assert!(find_agent("nonexistent").is_none());
    }

    #[test]
    fn agent_config_round_trip_via_toml() {
        let cfg = AgentConfig::from_profile(find_agent("claude").unwrap());
        let s = toml::to_string(&cfg).unwrap();
        let back: AgentConfig = toml::from_str(&s).unwrap();
        assert_eq!(back.id, "claude");
        assert!(back.args.contains(&"--acp".to_string()));
    }

    #[tokio::test]
    async fn list_agents_handles_missing_dir() {
        let r = list_agents().await;
        assert!(r.is_ok());
    }
}
