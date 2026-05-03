//! evo-acp-client — Zed Agent Client Protocol (ACP) client.
//!
//! Backed by the official `agent-client-protocol` SDK (v0.11.1) — the same
//! crate Zed itself uses (see `tmp/zed/Cargo.toml:498`).
//!
//! ## Data-driven catalog
//!
//! The list of supported ACP agent backends is **not hardcoded in Rust**.
//! It lives in `registry.json` (embedded at compile time) and is opened up
//! to user customisation:
//!
//! 1. **Built-in defaults** ship in `crates-ext/evo-acp-client/registry.json`
//!    — claude / gemini / codex / cursor / amp / auggie / copilot / aider /
//!    qwen-code at the time of writing. Re-built into the binary; users
//!    don't need to touch this file.
//!
//! 2. **Full override**: writing `~/.evoclaw/agents/registry.json` replaces
//!    the entire catalog. Useful when you want a curated short list.
//!
//! 3. **Per-agent patches**: every JSON file in `~/.evoclaw/agents/registry.d/`
//!    is loaded as a list of agent objects. Agents whose `id` matches an
//!    existing entry **replace** that entry; new ids are appended. This is
//!    the right way to add a single new vendor without touching the rest.
//!
//! Each agent entry declares a `distribution` (`npx` / `command` / `binary`).
//! `Distribution::resolve()` produces the `(cmd, args)` pair the SDK uses to
//! spawn the child — adding a new vendor is a JSON edit, not a code change.

use agent_client_protocol::schema::{
    self as acp, ContentBlock, InitializeRequest, NewSessionRequest, PromptRequest,
    ProtocolVersion, RequestPermissionOutcome, RequestPermissionRequest, RequestPermissionResponse,
    SelectedPermissionOutcome, SessionId, SessionNotification, SessionUpdate, TextContent,
};
use agent_client_protocol::{Agent, ByteStreams, Client, ConnectionTo};
use directories::BaseDirs;
use futures::channel::oneshot;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex as StdMutex, OnceLock};
use tokio::sync::Mutex as TokioMutex;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

#[derive(Debug, thiserror::Error)]
pub enum AcpError {
    #[error("acp sdk: {0}")]
    Sdk(String),
    #[error("config: {0}")]
    Config(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("not connected — call spawn() first")]
    NotConnected,
    #[error("channel closed")]
    ChannelClosed,
}

impl From<acp::Error> for AcpError {
    fn from(e: acp::Error) -> Self {
        AcpError::Sdk(format!("{e}"))
    }
}

// ---------------------------------------------------------------------------
// Catalog data model — entirely data-driven via registry.json.
// ---------------------------------------------------------------------------

/// One ACP-capable agent backend. Loaded from registry JSON files.
///
/// Field types are owned `String` (not `&'static str`) because the catalog
/// is built at runtime from JSON data. Existing call sites that did
/// `profile.id` / `profile.notes` continue to work via deref coercion to
/// `&str`; sites that explicitly named `&'static str` need `.as_str()`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentProfile {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub install_hint: String,
    pub auth_hint: String,
    pub acp_native: bool,
    #[serde(default)]
    pub notes: String,
    pub distribution: Distribution,
}

/// How EvoClaw spawns the agent process.
///
/// New distribution kinds can be added without touching the spawn logic:
/// `Distribution::resolve()` is the only place that needs an arm per kind.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Distribution {
    /// `npx -y <package> <args...>`. Works on any system with Node.js
    /// installed; `npx` will fetch the package on first run.
    Npx {
        package: String,
        #[serde(default)]
        args: Vec<String>,
    },
    /// `<cmd> <args...>` — direct exec. The user must have the binary on
    /// their PATH (catalog supplies `install_hint` to tell them how).
    Command {
        cmd: String,
        #[serde(default)]
        args: Vec<String>,
    },
    /// Pure metadata. The CLI shows `install_hint` and refuses to spawn —
    /// useful for cataloguing agents whose distribution isn't directly
    /// runnable yet (e.g. a binary release the user must download
    /// manually). Switch the entry to `command` once installed.
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
    agents: Vec<AgentProfile>,
    /// Allow `$comment`, `$schema` etc. without parse failure.
    #[serde(flatten, default)]
    _extra: HashMap<String, Value>,
}

const DEFAULT_REGISTRY_JSON: &str = include_str!("../registry.json");

static CATALOG_CELL: OnceLock<Vec<AgentProfile>> = OnceLock::new();

/// All known ACP agent profiles, built from:
///   1. Embedded `registry.json` defaults
///   2. Optional full override at `~/.evoclaw/agents/registry.json`
///   3. Optional per-agent patches in `~/.evoclaw/agents/registry.d/*.json`
///
/// Cached on first call. Restart the process after editing on-disk JSON
/// — there's no live-reload (intentional: catalog stability matters more
/// than convenience).
pub fn catalog() -> &'static [AgentProfile] {
    CATALOG_CELL.get_or_init(load_catalog).as_slice()
}

/// Lookup a profile by id. Returns `None` for unknown ids.
pub fn find_agent(id: &str) -> Option<&'static AgentProfile> {
    catalog().iter().find(|a| a.id == id)
}

/// Backwards-compat shim. Old code reads `evo_acp_client::CATALOG` as a
/// const slice; new code should call `catalog()` directly. This is a
/// `static` reference into the OnceLock.
pub fn catalog_static() -> &'static [AgentProfile] {
    catalog()
}

fn load_catalog() -> Vec<AgentProfile> {
    let mut entries = parse_registry_or_warn(DEFAULT_REGISTRY_JSON, "<embedded registry>");

    // Full override file: replaces the entire list.
    if let Ok(home_path) = home() {
        let user_full = home_path.join(".evoclaw/agents").join("registry.json");
        if user_full.exists() {
            if let Ok(text) = std::fs::read_to_string(&user_full) {
                let user_entries = parse_registry_or_warn(&text, &user_full.display().to_string());
                if !user_entries.is_empty() {
                    entries = user_entries;
                }
            }
        }

        // Per-id patches in registry.d/*.json. Each file is itself either
        // a `RegistryDoc` (`{ "agents": [...] }`) or a bare array.
        let patch_dir = home_path.join(".evoclaw/agents/registry.d");
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

fn parse_registry_or_warn(text: &str, label: &str) -> Vec<AgentProfile> {
    match serde_json::from_str::<RegistryDoc>(text) {
        Ok(doc) => doc.agents,
        Err(e) => {
            eprintln!("[evo-acp-client] failed to parse {label}: {e}");
            Vec::new()
        }
    }
}

fn parse_patch_or_warn(text: &str, label: &str) -> Vec<AgentProfile> {
    if let Ok(doc) = serde_json::from_str::<RegistryDoc>(text) {
        return doc.agents;
    }
    if let Ok(arr) = serde_json::from_str::<Vec<AgentProfile>>(text) {
        return arr;
    }
    eprintln!(
        "[evo-acp-client] {label}: expected `{{\"agents\":[...]}}` or a top-level array of agent objects",
    );
    Vec::new()
}

fn apply_patch(entries: &mut Vec<AgentProfile>, patch: AgentProfile) {
    if let Some(slot) = entries.iter_mut().find(|a| a.id == patch.id) {
        *slot = patch;
    } else {
        entries.push(patch);
    }
}

/// Paths the catalog loader consults. Useful for `evoclaw doctor` /
/// `/agent paths` to show the user where to write overrides.
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
            .map(|h| h.join(".evoclaw/agents/registry.json")),
        user_patch_dir: home_path.map(|h| h.join(".evoclaw/agents/registry.d")),
    }
}

// ---------------------------------------------------------------------------
// Per-agent saved config (`~/.evoclaw/agents/<id>.toml`).
// ---------------------------------------------------------------------------

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
    /// Derive a runnable config from a catalog profile.
    ///
    /// `Distribution::Binary` profiles produce a stub config whose
    /// `command` is empty — `AcpClient::spawn` will reject it with a
    /// readable error rather than mis-spawning.
    pub fn from_profile(p: &AgentProfile) -> Self {
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
}

fn home() -> Result<PathBuf, AcpError> {
    Ok(BaseDirs::new()
        .ok_or_else(|| AcpError::Config("no home dir".into()))?
        .home_dir()
        .to_path_buf())
}
pub fn agents_dir() -> Result<PathBuf, AcpError> {
    Ok(home()?.join(".evoclaw/agents"))
}
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
            if let Ok(cfg) = toml::from_str::<AgentConfig>(&s) {
                out.push(cfg);
            }
        }
    }
    Ok(out)
}

pub async fn remove_agent(id: &str) -> Result<(), AcpError> {
    let path = agent_config_path(id)?;
    if path.exists() {
        tokio::fs::remove_file(&path).await?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// AcpClient — SDK-backed ACP client. Spawns child + drives JSON-RPC.
// ---------------------------------------------------------------------------

type SessionBuffers = Arc<StdMutex<HashMap<String, String>>>;

struct AcpInner {
    connection: ConnectionTo<Agent>,
    buffers: SessionBuffers,
    initialized: bool,
    _child: tokio::process::Child,
    _io_task: tokio::task::JoinHandle<()>,
    shutdown_tx: Option<oneshot::Sender<()>>,
}

pub struct AcpClient {
    inner: TokioMutex<Option<AcpInner>>,
}

impl AcpClient {
    pub fn new() -> Self {
        Self {
            inner: TokioMutex::new(None),
        }
    }

    /// Spawn the agent CLI and bring up an ACP connection. Pipeline:
    ///   1. tokio::process spawn with piped stdio
    ///   2. wrap stdio in tokio↔futures compat shims
    ///   3. ByteStreams transport
    ///   4. Client.builder() with notification + permission handlers
    ///   5. connect_with on a tokio task; capture ConnectionTo<Agent>
    pub async fn spawn(&self, cfg: &AgentConfig) -> Result<(), AcpError> {
        if cfg.command.trim().is_empty() {
            return Err(AcpError::Config(format!(
                "agent '{}' has no runnable distribution. \
                 Edit ~/.evoclaw/agents/{}.toml or supply a registry.d entry \
                 with kind=command/npx.",
                cfg.id, cfg.id
            )));
        }
        let mut command = tokio::process::Command::new(&cfg.command);
        command.args(&cfg.args);
        for (k, v) in &cfg.env {
            command.env(k, v);
        }
        command
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);

        let mut child = command.spawn().map_err(|e| {
            AcpError::Config(format!(
                "spawn '{}' failed: {e}; install hint: see `evoclaw agent catalog`",
                cfg.command
            ))
        })?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| AcpError::Config("child stdin not piped".into()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| AcpError::Config("child stdout not piped".into()))?;
        if let Some(stderr) = child.stderr.take() {
            tokio::spawn(forward_stderr(stderr));
        }

        let transport = ByteStreams::new(stdin.compat_write(), stdout.compat());

        let buffers: SessionBuffers = Arc::new(StdMutex::new(HashMap::new()));
        let buffers_for_handler = buffers.clone();

        let (conn_tx, conn_rx) = oneshot::channel::<ConnectionTo<Agent>>();
        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();

        let agent_id_for_log = cfg.id.clone();
        let io_task = tokio::spawn(async move {
            let result = Client
                .builder()
                .name("evoclaw")
                .on_receive_notification(
                    {
                        let buffers = buffers_for_handler.clone();
                        async move |notification: SessionNotification, _cx| {
                            accumulate_session_text(&buffers, &notification);
                            Ok(())
                        }
                    },
                    agent_client_protocol::on_receive_notification!(),
                )
                .on_receive_request(
                    async move |req: RequestPermissionRequest, responder, _cx| {
                        let outcome = match req.options.first() {
                            Some(opt) => RequestPermissionOutcome::Selected(
                                SelectedPermissionOutcome::new(opt.option_id.clone()),
                            ),
                            None => RequestPermissionOutcome::Cancelled,
                        };
                        responder.respond(RequestPermissionResponse::new(outcome))
                    },
                    agent_client_protocol::on_receive_request!(),
                )
                .connect_with(transport, async move |connection: ConnectionTo<Agent>| {
                    if conn_tx.send(connection).is_err() {
                        return Err(acp::Error::internal_error());
                    }
                    let _ = shutdown_rx.await;
                    Ok::<(), acp::Error>(())
                })
                .await;
            if let Err(e) = result {
                tracing::warn!(agent = %agent_id_for_log, "ACP connection driver exited: {e}");
            }
        });

        let connection = conn_rx.await.map_err(|_| AcpError::ChannelClosed)?;

        let mut guard = self.inner.lock().await;
        *guard = Some(AcpInner {
            connection,
            buffers,
            initialized: false,
            _child: child,
            _io_task: io_task,
            shutdown_tx: Some(shutdown_tx),
        });
        Ok(())
    }

    pub async fn initialize(
        &self,
        client_name: &str,
        client_version: &str,
    ) -> Result<Value, AcpError> {
        let mut guard = self.inner.lock().await;
        let inner = guard.as_mut().ok_or(AcpError::NotConnected)?;
        if inner.initialized {
            return Ok(Value::Null);
        }
        let connection = inner.connection.clone();
        let _ = (client_name, client_version);
        let response = connection
            .send_request(InitializeRequest::new(ProtocolVersion::V1))
            .block_task()
            .await?;
        inner.initialized = true;
        Ok(json!({
            "protocolVersion": format!("{:?}", response.protocol_version),
            "agentInfo": response.agent_info.as_ref().map(|i| json!({
                "name": i.name,
                "title": i.title,
            })),
        }))
    }

    pub async fn new_session(&self) -> Result<String, AcpError> {
        let connection = self.connection_clone().await?;
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
        let response = connection
            .send_request(NewSessionRequest::new(cwd))
            .block_task()
            .await?;
        Ok(response.session_id.0.to_string())
    }

    pub async fn prompt(&self, session_id: &str, text: &str) -> Result<Value, AcpError> {
        let (connection, buffers) = {
            let guard = self.inner.lock().await;
            let inner = guard.as_ref().ok_or(AcpError::NotConnected)?;
            (inner.connection.clone(), inner.buffers.clone())
        };
        if let Ok(mut map) = buffers.lock() {
            map.insert(session_id.to_string(), String::new());
        }
        let session = SessionId::new(session_id.to_string());
        let blocks = vec![ContentBlock::Text(TextContent::new(text.to_string()))];
        let response = connection
            .send_request(PromptRequest::new(session.clone(), blocks))
            .block_task()
            .await?;
        let accumulated = buffers
            .lock()
            .ok()
            .and_then(|map| map.get(session_id).cloned())
            .unwrap_or_default();
        Ok(json!({
            "text": accumulated,
            "stopReason": format!("{:?}", response.stop_reason),
        }))
    }

    pub async fn cancel(&self, session_id: &str) -> Result<(), AcpError> {
        let connection = self.connection_clone().await?;
        let session = SessionId::new(session_id.to_string());
        connection.send_notification(acp::CancelNotification::new(session))?;
        Ok(())
    }

    pub async fn shutdown(&self) -> Result<(), AcpError> {
        let mut guard = self.inner.lock().await;
        if let Some(mut inner) = guard.take() {
            if let Some(tx) = inner.shutdown_tx.take() {
                let _ = tx.send(());
            }
        }
        Ok(())
    }

    async fn connection_clone(&self) -> Result<ConnectionTo<Agent>, AcpError> {
        let guard = self.inner.lock().await;
        guard
            .as_ref()
            .map(|i| i.connection.clone())
            .ok_or(AcpError::NotConnected)
    }
}

impl Default for AcpClient {
    fn default() -> Self {
        Self::new()
    }
}

fn accumulate_session_text(buffers: &SessionBuffers, n: &SessionNotification) {
    let SessionUpdate::AgentMessageChunk(ref chunk) = n.update else {
        return;
    };
    let ContentBlock::Text(ref tc) = chunk.content else {
        return;
    };
    let key = n.session_id.0.to_string();
    if let Ok(mut map) = buffers.lock() {
        map.entry(key).or_default().push_str(&tc.text);
    }
}

async fn forward_stderr(stderr: tokio::process::ChildStderr) {
    use tokio::io::{AsyncBufReadExt, BufReader};
    let mut reader = BufReader::new(stderr).lines();
    while let Ok(Some(line)) = reader.next_line().await {
        tracing::debug!("agent stderr: {line}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_catalog_parses_clean() {
        let entries = parse_registry_or_warn(DEFAULT_REGISTRY_JSON, "<test>");
        assert!(!entries.is_empty(), "embedded registry must parse");
        // Native agents come first.
        assert_eq!(entries[0].id, "claude");
        assert!(entries[0].acp_native);
    }

    #[test]
    fn catalog_is_cached() {
        let a = catalog().as_ptr();
        let b = catalog().as_ptr();
        assert_eq!(a, b, "catalog() should return the same slice every call");
    }

    #[test]
    fn find_agent_by_id() {
        assert_eq!(find_agent("claude").map(|p| p.id.as_str()), Some("claude"));
        assert!(find_agent("nonexistent").is_none());
    }

    #[test]
    fn npx_distribution_resolves_to_npx_y() {
        let d = Distribution::Npx {
            package: "@scope/pkg@1".into(),
            args: vec!["--flag".into()],
        };
        let (cmd, args) = d.resolve().unwrap();
        assert_eq!(cmd, "npx");
        assert_eq!(args, vec!["-y", "@scope/pkg@1", "--flag"]);
    }

    #[test]
    fn command_distribution_passes_through() {
        let d = Distribution::Command {
            cmd: "gemini".into(),
            args: vec!["--experimental-acp".into()],
        };
        assert_eq!(
            d.resolve().unwrap(),
            ("gemini".into(), vec!["--experimental-acp".into()])
        );
    }

    #[test]
    fn binary_distribution_returns_none() {
        let d = Distribution::Binary { hint: None };
        assert!(d.resolve().is_none());
    }

    #[test]
    fn agent_config_from_profile_npx() {
        let p = find_agent("claude").unwrap();
        let cfg = AgentConfig::from_profile(p);
        assert_eq!(cfg.command, "npx");
        assert!(cfg
            .args
            .iter()
            .any(|a| a.contains("@agentclientprotocol/claude-agent-acp")));
    }

    #[test]
    fn agent_config_from_profile_command() {
        let p = find_agent("gemini").unwrap();
        let cfg = AgentConfig::from_profile(p);
        assert_eq!(cfg.command, "gemini");
        assert_eq!(cfg.args, vec!["--experimental-acp"]);
    }

    #[test]
    fn acp_native_set_matches_zed_registry() {
        let native: Vec<&str> = catalog()
            .iter()
            .filter(|p| p.acp_native)
            .map(|p| p.id.as_str())
            .collect();
        assert_eq!(
            native,
            vec!["claude", "gemini", "codex", "cursor", "amp", "auggie"]
        );
    }

    #[test]
    fn patch_appends_unknown_id() {
        let mut entries: Vec<AgentProfile> = vec![];
        let patch = AgentProfile {
            id: "myagent".into(),
            name: "My Agent".into(),
            description: "Custom".into(),
            install_hint: "manual".into(),
            auth_hint: "none".into(),
            acp_native: true,
            notes: "".into(),
            distribution: Distribution::Command {
                cmd: "myacp".into(),
                args: vec![],
            },
        };
        apply_patch(&mut entries, patch);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].id, "myagent");
    }

    #[test]
    fn patch_replaces_existing_id() {
        let mut entries: Vec<AgentProfile> = vec![AgentProfile {
            id: "claude".into(),
            name: "old".into(),
            description: "".into(),
            install_hint: "".into(),
            auth_hint: "".into(),
            acp_native: true,
            notes: "".into(),
            distribution: Distribution::Command {
                cmd: "old".into(),
                args: vec![],
            },
        }];
        let patch = AgentProfile {
            id: "claude".into(),
            name: "new".into(),
            description: "".into(),
            install_hint: "".into(),
            auth_hint: "".into(),
            acp_native: true,
            notes: "".into(),
            distribution: Distribution::Command {
                cmd: "new".into(),
                args: vec![],
            },
        };
        apply_patch(&mut entries, patch);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "new");
    }

    #[test]
    fn patch_parser_accepts_bare_array() {
        let arr = serde_json::to_string(&vec![AgentProfile {
            id: "x".into(),
            name: "X".into(),
            description: "".into(),
            install_hint: "".into(),
            auth_hint: "".into(),
            acp_native: true,
            notes: "".into(),
            distribution: Distribution::Command {
                cmd: "x".into(),
                args: vec![],
            },
        }])
        .unwrap();
        let parsed = parse_patch_or_warn(&arr, "<test>");
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].id, "x");
    }

    #[test]
    fn agent_config_round_trips_via_toml() {
        let cfg = AgentConfig {
            id: "claude".into(),
            name: "Claude".into(),
            command: "npx".into(),
            args: vec![
                "-y".into(),
                "@agentclientprotocol/claude-agent-acp@latest".into(),
            ],
            env: vec![],
            installed_at: None,
        };
        let s = toml::to_string(&cfg).unwrap();
        let back: AgentConfig = toml::from_str(&s).unwrap();
        assert_eq!(back.id, "claude");
        assert_eq!(back.command, "npx");
    }

    #[test]
    fn agent_config_path_uses_evoclaw_dir() {
        let p = agent_config_path("claude").unwrap();
        let s = p.display().to_string();
        assert!(s.ends_with("/.evoclaw/agents/claude.toml"));
    }

    #[test]
    fn accumulate_session_text_only_appends_agent_chunks() {
        use agent_client_protocol::schema::{ContentChunk, TextContent};
        let buffers: SessionBuffers = Arc::new(StdMutex::new(HashMap::new()));
        let n_agent = SessionNotification::new(
            SessionId::new("s1"),
            SessionUpdate::AgentMessageChunk(ContentChunk::new(ContentBlock::Text(
                TextContent::new("hello "),
            ))),
        );
        accumulate_session_text(&buffers, &n_agent);
        let n_user = SessionNotification::new(
            SessionId::new("s1"),
            SessionUpdate::UserMessageChunk(ContentChunk::new(ContentBlock::Text(
                TextContent::new("user-echo "),
            ))),
        );
        accumulate_session_text(&buffers, &n_user);
        let n_agent2 = SessionNotification::new(
            SessionId::new("s1"),
            SessionUpdate::AgentMessageChunk(ContentChunk::new(ContentBlock::Text(
                TextContent::new("world"),
            ))),
        );
        accumulate_session_text(&buffers, &n_agent2);
        let map = buffers.lock().unwrap();
        assert_eq!(map.get("s1").map(String::as_str), Some("hello world"));
    }
}
