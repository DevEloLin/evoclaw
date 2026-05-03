//! evo-mcp-client — Anthropic Model Context Protocol (MCP) client.
//!
//! JSON-RPC 2.0 over stdio. Each MCP server is a child process the user
//! has installed (`npx`, `uvx`, etc.). Auth flows through env vars passed
//! to the subprocess (e.g. `GITHUB_PERSONAL_ACCESS_TOKEN`).
//!
//! ## Data-driven catalog
//!
//! The list of supported MCP servers is **not hardcoded in Rust**. It
//! lives in `registry.json` (embedded at compile time) and is opened up
//! to user customisation just like the ACP catalog:
//!
//! 1. **Built-in defaults** ship in `crates-ext/evo-mcp-client/registry.json`
//!    — filesystem / github / fetch / time / brave-search / postgres /
//!    slack at the time of writing.
//!
//! 2. **Full override**: writing `~/.evoclaw/mcp/registry.json` replaces
//!    the entire catalog.
//!
//! 3. **Per-server patches**: every JSON file in
//!    `~/.evoclaw/mcp/registry.d/` is loaded as a list of server objects.
//!    Servers whose `id` matches an existing entry **replace** that entry;
//!    new ids are appended.
//!
//! Each entry declares a `distribution` (`npx` / `uvx` / `command` /
//! `binary`). `Distribution::resolve()` produces the `(cmd, args)` pair the
//! spawn helper uses — adding a new server is a JSON edit, not a code
//! change.

use directories::BaseDirs;
use evo_stdio_rpc::{RpcError, SpawnConfig, StdioRpcClient};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

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

// ---------------------------------------------------------------------------
// Catalog data model — entirely data-driven via registry.json.
// ---------------------------------------------------------------------------

/// One MCP-capable server backend. Loaded from registry JSON files.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerProfile {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub install_hint: String,
    /// Env vars the server expects (e.g. `GITHUB_PERSONAL_ACCESS_TOKEN`).
    /// `mcp add` auto-captures any of these that are present in the
    /// caller's environment.
    #[serde(default)]
    pub auth_env: Vec<String>,
    pub distribution: Distribution,
}

/// How EvoClaw spawns the MCP server process. New kinds can be added
/// without touching the spawn logic — `Distribution::resolve()` is the
/// only place that needs an arm per kind.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Distribution {
    /// `npx -y <package> <args...>`. Works on any system with Node.js.
    Npx {
        package: String,
        #[serde(default)]
        args: Vec<String>,
    },
    /// `uvx <package> <args...>`. Python via Astral's `uv`.
    Uvx {
        package: String,
        #[serde(default)]
        args: Vec<String>,
    },
    /// `<cmd> <args...>` — direct exec from PATH.
    Command {
        cmd: String,
        #[serde(default)]
        args: Vec<String>,
    },
    /// Pure metadata. The CLI shows `install_hint` and refuses to spawn
    /// (caller switches the entry to `command` once installed).
    Binary {
        #[serde(default)]
        hint: Option<String>,
    },
}

impl Distribution {
    /// Resolve to the concrete `(command, args)` pair to spawn. Returns
    /// `None` for `Binary` distributions, which can't be spawned directly.
    pub fn resolve(&self) -> Option<(String, Vec<String>)> {
        match self {
            Distribution::Npx { package, args } => {
                let mut full = Vec::with_capacity(args.len() + 2);
                full.push("-y".to_string());
                full.push(package.clone());
                full.extend(args.iter().cloned());
                Some(("npx".to_string(), full))
            }
            Distribution::Uvx { package, args } => {
                let mut full = Vec::with_capacity(args.len() + 1);
                full.push(package.clone());
                full.extend(args.iter().cloned());
                Some(("uvx".to_string(), full))
            }
            Distribution::Command { cmd, args } => Some((cmd.clone(), args.clone())),
            Distribution::Binary { .. } => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RegistryDoc {
    #[serde(default)]
    version: Option<String>,
    #[serde(default)]
    servers: Vec<ServerProfile>,
    #[serde(flatten, default)]
    _extra: HashMap<String, Value>,
}

const DEFAULT_REGISTRY_JSON: &str = include_str!("../registry.json");

static CATALOG_CELL: OnceLock<Vec<ServerProfile>> = OnceLock::new();

/// All known MCP server profiles, built from:
///   1. Embedded `registry.json` defaults
///   2. Optional full override at `~/.evoclaw/mcp/registry.json`
///   3. Optional per-server patches in `~/.evoclaw/mcp/registry.d/*.json`
pub fn catalog() -> &'static [ServerProfile] {
    CATALOG_CELL.get_or_init(load_catalog).as_slice()
}

pub fn find_server(id: &str) -> Option<&'static ServerProfile> {
    catalog().iter().find(|s| s.id == id)
}

fn load_catalog() -> Vec<ServerProfile> {
    let mut entries = parse_registry_or_warn(DEFAULT_REGISTRY_JSON, "<embedded mcp registry>");
    if let Ok(home_path) = home() {
        let user_full = home_path.join(".evoclaw/mcp").join("registry.json");
        if user_full.exists() {
            if let Ok(text) = std::fs::read_to_string(&user_full) {
                let user_entries = parse_registry_or_warn(&text, &user_full.display().to_string());
                if !user_entries.is_empty() {
                    entries = user_entries;
                }
            }
        }
        let patch_dir = home_path.join(".evoclaw/mcp/registry.d");
        if patch_dir.is_dir() {
            if let Ok(rd) = std::fs::read_dir(&patch_dir) {
                let mut paths: Vec<_> = rd
                    .filter_map(|e| e.ok().map(|e| e.path()))
                    .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("json"))
                    .collect();
                paths.sort();
                for path in paths {
                    if let Ok(text) = std::fs::read_to_string(&path) {
                        for patch in parse_patch_or_warn(&text, &path.display().to_string()) {
                            apply_patch(&mut entries, patch);
                        }
                    }
                }
            }
        }
    }
    entries
}

fn parse_registry_or_warn(text: &str, label: &str) -> Vec<ServerProfile> {
    match serde_json::from_str::<RegistryDoc>(text) {
        Ok(doc) => doc.servers,
        Err(e) => {
            eprintln!("[evo-mcp-client] failed to parse {label}: {e}");
            Vec::new()
        }
    }
}

fn parse_patch_or_warn(text: &str, label: &str) -> Vec<ServerProfile> {
    if let Ok(doc) = serde_json::from_str::<RegistryDoc>(text) {
        return doc.servers;
    }
    if let Ok(arr) = serde_json::from_str::<Vec<ServerProfile>>(text) {
        return arr;
    }
    eprintln!(
        "[evo-mcp-client] {label}: expected `{{\"servers\":[...]}}` or a top-level array of server objects",
    );
    Vec::new()
}

fn apply_patch(entries: &mut Vec<ServerProfile>, patch: ServerProfile) {
    if let Some(slot) = entries.iter_mut().find(|s| s.id == patch.id) {
        *slot = patch;
    } else {
        entries.push(patch);
    }
}

/// Paths the catalog loader consults. Useful for `evoclaw mcp catalog` /
/// doctor to show the user where to write overrides.
#[derive(Debug, Clone)]
pub struct RegistryPaths {
    pub user_full: Option<PathBuf>,
    pub user_patch_dir: Option<PathBuf>,
}

pub fn registry_paths() -> RegistryPaths {
    let home_path = home().ok();
    RegistryPaths {
        user_full: home_path
            .clone()
            .map(|h| h.join(".evoclaw/mcp/registry.json")),
        user_patch_dir: home_path.map(|h| h.join(".evoclaw/mcp/registry.d")),
    }
}

// ---------------------------------------------------------------------------
// Per-server saved config (`~/.evoclaw/mcp/<id>.toml`).
// ---------------------------------------------------------------------------

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
    /// Derive a runnable config from a catalog profile. `Binary`
    /// distributions produce a stub config whose `command` is empty —
    /// `McpClient::spawn` will reject it with a readable error rather
    /// than mis-spawning.
    pub fn from_profile(p: &ServerProfile) -> Self {
        let (command, args) = p.distribution.resolve().unwrap_or_default();
        Self {
            id: p.id.clone(),
            name: p.name.clone(),
            command,
            args,
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

// ---------------------------------------------------------------------------
// MCP client (JSON-RPC over stdio) — unchanged from prior implementation.
// ---------------------------------------------------------------------------

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
    /// Server identifier captured at `spawn` time. Used to prefix diagnostic
    /// errors so a multi-server setup can attribute failures unambiguously.
    server_id: tokio::sync::Mutex<String>,
}

impl McpClient {
    pub fn new() -> Self {
        Self {
            rpc: Arc::new(StdioRpcClient::new()),
            initialized: tokio::sync::Mutex::new(false),
            server_id: tokio::sync::Mutex::new(String::new()),
        }
    }

    pub async fn spawn(&self, cfg: &ServerConfig) -> Result<(), McpError> {
        if cfg.command.trim().is_empty() {
            return Err(McpError::Config(format!(
                "server '{}' has no runnable distribution. \
                 Edit ~/.evoclaw/mcp/{}.toml or supply a registry.d entry \
                 with kind=command/npx/uvx.",
                cfg.id, cfg.id
            )));
        }
        {
            let mut sid = self.server_id.lock().await;
            *sid = cfg.id.clone();
        }
        self.rpc.spawn(cfg.to_spawn()).await?;
        Ok(())
    }

    /// Snapshot of the server id for use in error messages. Returns
    /// `<unknown>` if `spawn` has not been called yet.
    async fn sid(&self) -> String {
        let g = self.server_id.lock().await;
        if g.is_empty() {
            "<unknown>".to_string()
        } else {
            g.clone()
        }
    }

    /// Wrap a free-form message in a `McpError::Config` that includes the
    /// owning server id for diagnostics: `[<server_id>]: <msg>`.
    async fn config_err(&self, msg: impl Into<String>) -> McpError {
        let sid = self.sid().await;
        McpError::Config(format!("[{sid}]: {}", msg.into()))
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
        let tools_val = match r.get("tools") {
            Some(v) => v.clone(),
            None => {
                return Err(self
                    .config_err(
                        "server returned no `tools` field in tools/list response".to_string(),
                    )
                    .await);
            }
        };
        match serde_json::from_value::<Vec<McpTool>>(tools_val) {
            Ok(tools) => Ok(tools),
            Err(e) => Err(self
                .config_err(format!("failed to parse tools/list `tools` array: {e}"))
                .await),
        }
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
        match serde_json::from_value::<ToolCallResult>(r) {
            Ok(out) => Ok(out),
            Err(e) => Err(self
                .config_err(format!("failed to parse tools/call response: {e}"))
                .await),
        }
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
    fn embedded_catalog_parses_clean() {
        let entries = parse_registry_or_warn(DEFAULT_REGISTRY_JSON, "<test>");
        assert!(!entries.is_empty(), "embedded mcp registry must parse");
        let ids: Vec<_> = entries.iter().map(|s| s.id.as_str()).collect();
        assert!(ids.contains(&"filesystem"));
        assert!(ids.contains(&"github"));
        assert!(ids.contains(&"fetch"));
    }

    #[test]
    fn catalog_is_cached() {
        let a = catalog().as_ptr();
        let b = catalog().as_ptr();
        assert_eq!(a, b, "catalog() should return the same slice every call");
    }

    #[test]
    fn find_server_works() {
        let gh = find_server("github").unwrap();
        assert_eq!(gh.id, "github");
        assert!(find_server("nope").is_none());
    }

    #[test]
    fn npx_distribution_resolves_to_npx_y() {
        let d = Distribution::Npx {
            package: "@scope/pkg".into(),
            args: vec!["--flag".into()],
        };
        assert_eq!(
            d.resolve().unwrap(),
            (
                "npx".into(),
                vec!["-y".into(), "@scope/pkg".into(), "--flag".into()]
            )
        );
    }

    #[test]
    fn uvx_distribution_resolves_to_uvx_package() {
        let d = Distribution::Uvx {
            package: "mcp-server-time".into(),
            args: vec![],
        };
        assert_eq!(
            d.resolve().unwrap(),
            ("uvx".into(), vec!["mcp-server-time".into()])
        );
    }

    #[test]
    fn command_distribution_passes_through() {
        let d = Distribution::Command {
            cmd: "/usr/local/bin/my-mcp".into(),
            args: vec!["--port".into(), "0".into()],
        };
        assert_eq!(
            d.resolve().unwrap(),
            (
                "/usr/local/bin/my-mcp".into(),
                vec!["--port".into(), "0".into()]
            )
        );
    }

    #[test]
    fn binary_distribution_returns_none() {
        assert!(Distribution::Binary { hint: None }.resolve().is_none());
    }

    #[test]
    fn server_config_from_profile_npx() {
        let p = find_server("github").unwrap();
        let cfg = ServerConfig::from_profile(p);
        assert_eq!(cfg.command, "npx");
        assert!(cfg
            .args
            .iter()
            .any(|a| a == "@modelcontextprotocol/server-github"));
    }

    #[test]
    fn server_config_from_profile_uvx() {
        let p = find_server("fetch").unwrap();
        let cfg = ServerConfig::from_profile(p);
        assert_eq!(cfg.command, "uvx");
        assert_eq!(cfg.args, vec!["mcp-server-fetch"]);
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
        assert!(gh
            .auth_env
            .iter()
            .any(|v| v == "GITHUB_PERSONAL_ACCESS_TOKEN"));
        let pg = find_server("postgres").unwrap();
        assert!(pg.auth_env.iter().any(|v| v == "DATABASE_URL"));
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

    #[tokio::test]
    async fn config_err_prefixes_with_server_id() {
        let cli = McpClient::new();
        {
            let mut g = cli.server_id.lock().await;
            *g = "github".into();
        }
        let err = cli.config_err("boom").await;
        match err {
            McpError::Config(s) => {
                assert!(s.starts_with("[github]:"), "got: {s}");
                assert!(s.contains("boom"));
            }
            _ => panic!("expected Config"),
        }
    }

    #[tokio::test]
    async fn config_err_uses_unknown_when_not_spawned() {
        let cli = McpClient::new();
        let err = cli.config_err("oops").await;
        match err {
            McpError::Config(s) => assert!(s.starts_with("[<unknown>]:"), "got: {s}"),
            _ => panic!("expected Config"),
        }
    }

    #[test]
    fn patch_appends_unknown_id() {
        let mut entries: Vec<ServerProfile> = vec![];
        let patch = ServerProfile {
            id: "myserver".into(),
            name: "My Server".into(),
            description: "Custom".into(),
            install_hint: "manual".into(),
            auth_env: vec![],
            distribution: Distribution::Command {
                cmd: "myserver".into(),
                args: vec![],
            },
        };
        apply_patch(&mut entries, patch);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].id, "myserver");
    }

    #[test]
    fn patch_replaces_existing_id() {
        let mut entries: Vec<ServerProfile> = vec![ServerProfile {
            id: "github".into(),
            name: "old".into(),
            description: "".into(),
            install_hint: "".into(),
            auth_env: vec![],
            distribution: Distribution::Command {
                cmd: "old".into(),
                args: vec![],
            },
        }];
        let patch = ServerProfile {
            id: "github".into(),
            name: "new".into(),
            description: "".into(),
            install_hint: "".into(),
            auth_env: vec![],
            distribution: Distribution::Command {
                cmd: "new".into(),
                args: vec![],
            },
        };
        apply_patch(&mut entries, patch);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "new");
    }
}
