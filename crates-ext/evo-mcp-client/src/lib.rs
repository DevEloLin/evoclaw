//! evo-mcp-client — Anthropic Model Context Protocol (MCP) client.
//!
//! JSON-RPC 2.0 over stdio. Each MCP server is a child installed by user
//! (`npx`, `uvx`). Auth = env vars passed to subprocess (e.g. `GITHUB_PAT`).

use directories::BaseDirs;
use evo_stdio_rpc::{RpcError, SpawnConfig, StdioRpcClient};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::Arc;

pub const MCP_PROTOCOL_VERSION: &str = "2025-03-26";

#[derive(Debug, thiserror::Error)]
pub enum McpError {
    #[error("rpc: {0}")]
    Rpc(#[from] RpcError),
    #[error("config: {0}")]
    Config(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Clone)]
pub struct ServerProfile {
    pub id: &'static str,
    pub name: &'static str,
    pub command: &'static str,
    pub args: &'static [&'static str],
    pub install_hint: &'static str,
    pub auth_env: &'static [&'static str],
    pub description: &'static str,
}

pub const CATALOG: &[ServerProfile] = &[
    ServerProfile {
        id: "filesystem",
        name: "Filesystem",
        command: "npx",
        args: &["-y", "@modelcontextprotocol/server-filesystem"],
        install_hint: "npx auto-fetches; needs Node ≥18",
        auth_env: &[],
        description: "Read/write files within configured roots",
    },
    ServerProfile {
        id: "github",
        name: "GitHub",
        command: "npx",
        args: &["-y", "@modelcontextprotocol/server-github"],
        install_hint: "npx auto-fetches",
        auth_env: &["GITHUB_PERSONAL_ACCESS_TOKEN"],
        description: "Issues, PRs, repos, search via GitHub API",
    },
    ServerProfile {
        id: "fetch",
        name: "Fetch (web)",
        command: "uvx",
        args: &["mcp-server-fetch"],
        install_hint: "pipx install uv; uvx auto-fetches",
        auth_env: &[],
        description: "Fetch & render web pages as markdown",
    },
    ServerProfile {
        id: "time",
        name: "Time",
        command: "uvx",
        args: &["mcp-server-time"],
        install_hint: "uvx mcp-server-time",
        auth_env: &[],
        description: "Timezone & current-time queries",
    },
    ServerProfile {
        id: "brave-search",
        name: "Brave Search",
        command: "npx",
        args: &["-y", "@modelcontextprotocol/server-brave-search"],
        install_hint: "free tier at brave.com/search/api",
        auth_env: &["BRAVE_API_KEY"],
        description: "Web search via Brave API",
    },
    ServerProfile {
        id: "postgres",
        name: "Postgres",
        command: "npx",
        args: &["-y", "@modelcontextprotocol/server-postgres"],
        install_hint: "needs DATABASE_URL env",
        auth_env: &["DATABASE_URL"],
        description: "Read-only SQL queries",
    },
    ServerProfile {
        id: "slack",
        name: "Slack",
        command: "npx",
        args: &["-y", "@modelcontextprotocol/server-slack"],
        install_hint: "needs SLACK_BOT_TOKEN + SLACK_TEAM_ID",
        auth_env: &["SLACK_BOT_TOKEN", "SLACK_TEAM_ID"],
        description: "Read messages, post to channels",
    },
];

pub fn find_server(id: &str) -> Option<&'static ServerProfile> {
    CATALOG.iter().find(|s| s.id == id)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerConfig {
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

impl ServerConfig {
    pub fn from_profile(p: &ServerProfile) -> Self {
        Self {
            id: p.id.into(),
            name: p.name.into(),
            command: p.command.into(),
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

fn home() -> Result<PathBuf, McpError> {
    Ok(BaseDirs::new()
        .ok_or_else(|| McpError::Config("no home dir".into()))?
        .home_dir()
        .to_path_buf())
}
pub fn servers_dir() -> Result<PathBuf, McpError> {
    Ok(home()?.join(".evoclaw/mcp"))
}
pub fn server_config_path(id: &str) -> Result<PathBuf, McpError> {
    Ok(servers_dir()?.join(format!("{id}.toml")))
}

pub async fn save_server(cfg: &ServerConfig) -> Result<PathBuf, McpError> {
    let dir = servers_dir()?;
    tokio::fs::create_dir_all(&dir).await?;
    let path = dir.join(format!("{}.toml", cfg.id));
    let s = toml::to_string_pretty(cfg).map_err(|e| McpError::Config(e.to_string()))?;
    tokio::fs::write(&path, s).await?;
    Ok(path)
}

pub async fn load_server(id: &str) -> Result<ServerConfig, McpError> {
    let path = server_config_path(id)?;
    let s = tokio::fs::read_to_string(&path).await?;
    toml::from_str(&s).map_err(|e| McpError::Config(e.to_string()))
}

pub async fn list_servers() -> Result<Vec<ServerConfig>, McpError> {
    let dir = servers_dir()?;
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    let mut entries = tokio::fs::read_dir(&dir).await?;
    while let Some(e) = entries.next_entry().await? {
        let p = e.path();
        if p.extension().and_then(|s| s.to_str()) != Some("toml") {
            continue;
        }
        if let Ok(s) = tokio::fs::read_to_string(&p).await {
            if let Ok(cfg) = toml::from_str::<ServerConfig>(&s) {
                out.push(cfg);
            }
        }
    }
    Ok(out)
}

pub async fn remove_server(id: &str) -> Result<(), McpError> {
    let path = server_config_path(id)?;
    if path.exists() {
        tokio::fs::remove_file(&path).await?;
    }
    Ok(())
}

#[derive(Debug, Clone, Deserialize)]
pub struct McpTool {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default, rename = "inputSchema")]
    pub input_schema: Value,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ToolCallResult {
    #[serde(default)]
    pub content: Vec<ToolContent>,
    #[serde(default, rename = "isError")]
    pub is_error: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type")]
pub enum ToolContent {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "image")]
    Image {
        #[serde(default)]
        data: String,
        #[serde(default, rename = "mimeType")]
        mime_type: String,
    },
    #[serde(other)]
    Other,
}

pub struct McpClient {
    rpc: Arc<StdioRpcClient>,
    initialized: tokio::sync::Mutex<bool>,
}

impl McpClient {
    pub fn new() -> Self {
        Self {
            rpc: Arc::new(StdioRpcClient::new()),
            initialized: tokio::sync::Mutex::new(false),
        }
    }

    pub async fn spawn(&self, cfg: &ServerConfig) -> Result<(), McpError> {
        self.rpc.spawn(cfg.to_spawn()).await?;
        Ok(())
    }

    pub async fn initialize(
        &self,
        client_name: &str,
        client_version: &str,
    ) -> Result<Value, McpError> {
        let mut init = self.initialized.lock().await;
        if *init {
            return Ok(Value::Null);
        }
        let result = self
            .rpc
            .call(
                "initialize",
                json!({
                    "protocolVersion": MCP_PROTOCOL_VERSION,
                    "capabilities": { "tools": {}, "resources": {}, "prompts": {} },
                    "clientInfo": { "name": client_name, "version": client_version }
                }),
            )
            .await?;
        self.rpc
            .notify("notifications/initialized", json!({}))
            .await
            .ok();
        *init = true;
        Ok(result)
    }

    pub async fn list_tools(&self) -> Result<Vec<McpTool>, McpError> {
        let r = self.rpc.call("tools/list", json!({})).await?;
        let tools_val = r.get("tools").cloned().unwrap_or(json!([]));
        serde_json::from_value(tools_val).map_err(|e| McpError::Config(e.to_string()))
    }

    pub async fn call_tool(
        &self,
        name: &str,
        arguments: Value,
    ) -> Result<ToolCallResult, McpError> {
        let r = self
            .rpc
            .call("tools/call", json!({"name": name, "arguments": arguments}))
            .await?;
        serde_json::from_value(r).map_err(|e| McpError::Config(e.to_string()))
    }

    pub async fn shutdown(&self) -> Result<(), McpError> {
        self.rpc.shutdown().await?;
        Ok(())
    }
}

impl Default for McpClient {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_has_seven_servers() {
        assert_eq!(CATALOG.len(), 7);
        let ids: Vec<_> = CATALOG.iter().map(|s| s.id).collect();
        assert!(ids.contains(&"filesystem"));
        assert!(ids.contains(&"github"));
    }

    #[test]
    fn find_server_works() {
        assert_eq!(find_server("github").unwrap().command, "npx");
        assert!(find_server("nope").is_none());
    }

    #[test]
    fn server_config_round_trip_toml() {
        let cfg = ServerConfig::from_profile(find_server("filesystem").unwrap());
        let s = toml::to_string(&cfg).unwrap();
        let back: ServerConfig = toml::from_str(&s).unwrap();
        assert_eq!(back.id, "filesystem");
    }

    #[test]
    fn auth_env_listed_for_secret_servers() {
        let gh = find_server("github").unwrap();
        assert!(gh.auth_env.contains(&"GITHUB_PERSONAL_ACCESS_TOKEN"));
        let pg = find_server("postgres").unwrap();
        assert!(pg.auth_env.contains(&"DATABASE_URL"));
    }

    #[test]
    fn local_servers_have_no_auth_env() {
        let fs = find_server("filesystem").unwrap();
        assert!(fs.auth_env.is_empty());
    }

    #[tokio::test]
    async fn list_servers_handles_missing_dir() {
        assert!(list_servers().await.is_ok());
    }

    #[test]
    fn tool_content_parses_text_and_unknown() {
        let s = r#"[{"type":"text","text":"hello"},{"type":"weird","x":1}]"#;
        let v: Vec<ToolContent> = serde_json::from_str(s).unwrap();
        assert_eq!(v.len(), 2);
    }
}
