//! evo-cli — entry function shared by `evo` and `evoclaw` binaries.

pub mod mcp_tools;
pub mod onboard;

use clap::{Parser, Subcommand};
use onboard::ProviderChoice;
use directories::BaseDirs;
use evo_core::channel::ChannelAdapter;
use evo_core::{ConversationRuntime, Memory, MemoryLayer, Session, Skill, SkillTree};
use evo_policy::{default_vault_path, BudgetCfg, CostEngine, Redactor, Vault};
use evo_providers::{
    AcpProvider, AnthropicProvider, AuthMethod, BrowserProvider, CopilotProvider,
    OpenAiCompatProvider, Provider,
};
use evo_tools::{ToolContext, ToolRegistry};
use eyre::{Result, WrapErr};
use rustyline::completion::{Completer, Pair};
use rustyline::highlight::Highlighter;
use rustyline::hint::Hinter;
use rustyline::validate::Validator;
use rustyline::{Context, Helper};
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[derive(Parser, Debug)]
#[command(
    name = "evoclaw",
    bin_name = "evoclaw",
    version,
    about = "EvoClaw — self-evolving local Agent Runtime",
    long_about = "EvoClaw is a Rust-native, local-first agent runtime.\n\
                  Run with no subcommand to enter the interactive shell."
)]
struct Cli {
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// 5-minute setup: model, key, workspace, ~/.evoclaw layout
    Onboard,
    /// Run a one-shot task (non-interactive)
    Run { input: String },
    /// Health check (config, model, fs)
    Doctor,
    /// Doctor sub-checks
    #[command(subcommand)]
    DoctorOf(DoctorCmd),
    /// Start local Gateway daemon (HTTP + WebChat)
    Gateway {
        #[arg(long, default_value = "127.0.0.1:7878")]
        bind: String,
        /// Bearer token for /chat. If omitted, reuse or generate a persistent
        /// 32-hex-char random token at `~/.evoclaw/gateway/token` (mode 0600).
        #[arg(long)]
        token: Option<String>,
    },
    /// Replay a JSONL session
    Replay { path: Option<PathBuf> },
    #[command(subcommand)]
    Skill(SkillCmd),
    #[command(subcommand)]
    Memory(MemoryCmd),
    /// Force interactive REPL (same as no subcommand)
    Shell,
    /// Switch provider / re-enter API key (interactive wizard)
    Login,
    /// External ACP agents (Claude / Codex CLI / Cursor / Copilot, Zed-style)
    #[command(subcommand)]
    Agent(AgentCmd),
    /// MCP servers (filesystem / github / fetch / time / brave / postgres / slack)
    #[command(subcommand)]
    Mcp(McpSubCmd),
    /// Local secret vault — values never reach the model (PRD §13.4)
    #[command(subcommand)]
    Secret(SecretCmd),
    /// Multi-channel adapter scaffolding (Telegram / Slack / Discord, v0.6).
    /// Use `evo channel list` to see registered adapters and
    /// `evo channel run --kind local-pipe` to drive the stdin/stdout
    /// reference adapter end-to-end. See docs/channels.md.
    #[command(subcommand)]
    Channel(ChannelCmd),
}

#[derive(Subcommand, Debug)]
enum ChannelCmd {
    /// List built-in adapters and any external `~/.evoclaw/channels/*.toml`.
    List,
    /// Run a single adapter, fan inbound messages through the agent loop,
    /// and post replies back. Currently only `--kind local-pipe` ships
    /// in-tree; Telegram/Slack/Discord transports land in v0.6.
    Run {
        #[arg(long)]
        kind: String,
    },
}

#[derive(Subcommand, Debug)]
enum SecretCmd {
    /// Add or replace a secret. Reads value from stdin (no echo) if `--stdin`.
    Add {
        name: String,
        #[arg(long)]
        stdin: bool,
        value: Option<String>,
    },
    /// List vault entries (names + fingerprints — never the raw value).
    List,
    /// Remove a vault entry by name.
    Remove { name: String },
    /// Test the redactor against a sample string and show the scrubbed output.
    Test { input: String },
}

#[derive(Subcommand, Debug)]
enum AgentCmd {
    /// List configured agents
    List,
    /// Show built-in catalog
    Catalog,
    /// Add an agent from the catalog (writes ~/.evoclaw/agents/<id>.toml)
    Add { id: String },
    /// Remove a configured agent
    Remove { id: String },
    /// Spawn the agent and run the ACP initialize handshake
    Test { id: String },
}

#[derive(Subcommand, Debug)]
enum McpSubCmd {
    /// List configured MCP servers
    List,
    /// Show built-in catalog
    Catalog,
    /// Add a server from the catalog
    Add { id: String },
    /// Remove a configured server
    Remove { id: String },
    /// Spawn server, initialize handshake, list tools, shutdown
    Test { id: String },
}

#[derive(Subcommand, Debug)]
enum DoctorCmd {
    Tokens,
    Closure,
}

#[derive(Subcommand, Debug)]
enum SkillCmd {
    List,
    Show { id: String },
    Tree,
}

#[derive(Subcommand, Debug)]
enum MemoryCmd {
    Search {
        query: String,
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
}

fn skills_dir() -> Result<PathBuf> {
    Ok(evoclaw_dir()?.join("skills"))
}
fn memory_dir() -> Result<PathBuf> {
    Ok(evoclaw_dir()?.join("memory"))
}
fn cost_log_path() -> Result<PathBuf> {
    Ok(evoclaw_dir()?.join("cost.jsonl"))
}
fn vault_path() -> Result<PathBuf> {
    Ok(default_vault_path(&evoclaw_dir()?))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Config {
    #[serde(default)]
    meta: ProfileMeta,
    model: ModelCfg,
    #[serde(default)]
    auth: AuthCfg,
    budget: ConfigBudget,
    security: SecurityCfg,
    /// Optional logging override. Older config.toml files without this
    /// section keep working — `logs_dir()` falls back to the platform
    /// temp dir (`/tmp/evoclaw` on Unix, `%TEMP%\\evoclaw` on Windows).
    #[serde(default)]
    logs: Option<LogsCfg>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct ProfileMeta {
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct LogsCfg {
    /// Directory where session JSONL logs are written. Tilde (`~`) is
    /// expanded against `$HOME`. Missing directories are created on demand.
    #[serde(default)]
    dir: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct AuthCfg {
    /// Selected auth method: `api_key` (default) | `browser` | `acp`.
    /// Old config.toml files without an `[auth]` block decode to default ⇒
    /// `api_key`, preserving backward compatibility with existing installs.
    #[serde(default = "default_auth_method")]
    method: String,
}

fn default_auth_method() -> String {
    AuthMethod::ApiKey.as_str().to_string()
}

impl AuthCfg {
    fn parsed(&self) -> AuthMethod {
        AuthMethod::parse(&self.method).unwrap_or(AuthMethod::ApiKey)
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ModelCfg {
    /// Provider id from the catalog (`deepseek`, `kimi`, ...). When present,
    /// drives api-key resolution. Older configs without this field still work
    /// — `evoclaw login` adds it.
    #[serde(default)]
    provider: Option<String>,
    default: String,
    base_url: String,
    #[serde(default)]
    fallback: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ConfigBudget {
    per_task_usd: f64,
    per_day_usd: f64,
    per_month_usd: f64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
struct SecurityCfg {
    default_permission: String,
    high_risk_intercept: bool,
}

// Config::template is gone — `onboard::save_config` owns the canonical
// rendering of config.toml so there's a single source of truth.

fn home() -> Result<PathBuf> {
    Ok(BaseDirs::new()
        .ok_or_else(|| eyre::eyre!("cannot determine home dir"))?
        .home_dir()
        .to_path_buf())
}
fn evoclaw_dir() -> Result<PathBuf> {
    Ok(home()?.join(".evoclaw"))
}

fn profiles_dir() -> Result<PathBuf> {
    Ok(evoclaw_dir()?.join("profiles"))
}

fn active_profile_file() -> Result<PathBuf> {
    Ok(evoclaw_dir()?.join("active-profile.txt"))
}
fn config_path() -> Result<PathBuf> {
    Ok(evoclaw_dir()?.join("config.toml"))
}
fn workspace_dir() -> Result<PathBuf> {
    Ok(evoclaw_dir()?.join("workspace"))
}
/// Resolution order, evaluated once per process (first call wins):
///   1. env `EVO_LOG_DIR`           — operator override
///   2. config.toml `[logs] dir`    — user override
///   3. platform default            — `/tmp/evoclaw` on Unix,
///                                    `%TEMP%\evoclaw` on Windows
///
/// Initialised by `init_logs_dir(...)` from the entry point. Calling
/// `logs_dir()` before initialisation falls through to the platform
/// default — safe but ignores any `[logs]` block in config.toml.
static LOGS_DIR: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

fn compute_logs_dir(cfg: Option<&Config>) -> PathBuf {
    if let Ok(v) = std::env::var("EVO_LOG_DIR") {
        let trimmed = v.trim();
        if !trimmed.is_empty() {
            return expand_tilde(trimmed);
        }
    }
    if let Some(c) = cfg {
        if let Some(LogsCfg { dir: Some(d) }) = &c.logs {
            let trimmed = d.trim();
            if !trimmed.is_empty() {
                return expand_tilde(trimmed);
            }
        }
    }
    if cfg!(windows) {
        std::env::temp_dir().join("evoclaw")
    } else {
        PathBuf::from("/tmp/evoclaw")
    }
}

fn expand_tilde(raw: &str) -> PathBuf {
    if let Some(rest) = raw.strip_prefix("~/") {
        if let Ok(h) = std::env::var("HOME") {
            return PathBuf::from(h).join(rest);
        }
    }
    PathBuf::from(raw)
}

fn init_logs_dir(cfg: Option<&Config>) {
    let _ = LOGS_DIR.set(compute_logs_dir(cfg));
}

fn logs_dir() -> Result<PathBuf> {
    Ok(LOGS_DIR
        .get()
        .cloned()
        .unwrap_or_else(|| compute_logs_dir(None)))
}

/// One log file per shell session. Inside `interactive()` we compute this
/// once on entry and pass it down to every `run_task_with_provider` call,
/// so all `Task`/`Turn`/`End` records from the same window land in the
/// same JSONL file (instead of one file per `evoclaw>` ask).
fn session_log_path() -> Result<PathBuf> {
    let stamp = chrono::Utc::now().format("%Y%m%dT%H%M%S");
    Ok(logs_dir()?.join(format!("session-{stamp}.jsonl")))
}

async fn ensure_layout() -> Result<()> {
    // Logs no longer live under `~/.evoclaw/`. The destination is decided
    // by `logs_dir()` (env / config / platform default) and created on
    // demand by `Session::open`, so we don't touch it here.
    for sub in [
        "workspace",
        "skills",
        "browser_profiles",
        "secrets",
        "plugins",
        "cache",
    ] {
        tokio::fs::create_dir_all(evoclaw_dir()?.join(sub))
            .await
            .wrap_err_with(|| format!("create {sub}"))?;
    }
    Ok(())
}

async fn load_config() -> Result<Config> {
    let p = config_path()?;
    let text = tokio::fs::read_to_string(&p).await.wrap_err_with(|| {
        format!(
            "read config at {}; run `evoclaw onboard` first",
            p.display()
        )
    })?;
    toml::from_str(&text).wrap_err("parse config.toml")
}

/// Library entry point. Both binaries call this.
pub async fn entry() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .init();
    // Resolve the logs directory once before any subcommand can request it.
    // If config.toml is missing (first run) we fall back to env + platform
    // default — `/tmp/evoclaw` on Unix, `%TEMP%\\evoclaw` on Windows.
    init_logs_dir(load_config().await.ok().as_ref());
    let cli = Cli::parse();
    match cli.cmd {
        None | Some(Cmd::Shell) => interactive().await,
        Some(Cmd::Onboard) => onboard_cmd().await,
        Some(Cmd::Login) => login_cmd().await,
        Some(Cmd::Agent(a)) => match a {
            AgentCmd::List => agent_list().await,
            AgentCmd::Catalog => {
                agent_catalog();
                Ok(())
            }
            AgentCmd::Add { id } => agent_add(&id).await,
            AgentCmd::Remove { id } => agent_remove(&id).await,
            AgentCmd::Test { id } => agent_test(&id).await,
        },
        Some(Cmd::Mcp(m)) => match m {
            McpSubCmd::List => mcp_list().await,
            McpSubCmd::Catalog => {
                mcp_catalog();
                Ok(())
            }
            McpSubCmd::Add { id } => mcp_add(&id).await,
            McpSubCmd::Remove { id } => mcp_remove(&id).await,
            McpSubCmd::Test { id } => mcp_test(&id).await,
        },
        Some(Cmd::Secret(s)) => match s {
            SecretCmd::Add { name, stdin, value } => secret_add(&name, stdin, value).await,
            SecretCmd::List => secret_list().await,
            SecretCmd::Remove { name } => secret_remove(&name).await,
            SecretCmd::Test { input } => secret_test(&input).await,
        },
        Some(Cmd::Run { input }) => run_one_shot(&input).await,
        Some(Cmd::Doctor) => doctor().await,
        Some(Cmd::DoctorOf(d)) => match d {
            DoctorCmd::Tokens => doctor_tokens().await,
            DoctorCmd::Closure => doctor_closure().await,
        },
        Some(Cmd::Gateway { bind, token }) => gateway(&bind, token.as_deref()).await,
        Some(Cmd::Replay { path }) => replay(path).await,
        Some(Cmd::Skill(s)) => match s {
            SkillCmd::List => skill_list().await,
            SkillCmd::Show { id } => skill_show(&id).await,
            SkillCmd::Tree => skill_tree().await,
        },
        Some(Cmd::Memory(m)) => match m {
            MemoryCmd::Search { query, limit } => memory_search(&query, limit).await,
        },
        Some(Cmd::Channel(c)) => channel_handler(c).await,
    }
}

// ---------------------------------------------------------------------------
// Interactive REPL
// ---------------------------------------------------------------------------

/// Slash command completer for rustyline.
/// Provides auto-completion and hints for all slash commands and their subcommands.
#[derive(Default)]
struct SlashCompleter {
    commands: Vec<&'static str>,
}

impl SlashCompleter {
    fn new() -> Self {
        Self {
            commands: vec![
                // Main commands
                "/help", "/login", "/logout", "/exit", "/quit", "/q",
                "/clear", "/doctor", "/tokens", "/usage", "/closure", "/replay",
                "/status",
                // Commands with subcommands
                "/agent", "/agent list", "/agent catalog", "/agent add", "/agent remove", "/agent test",
                "/mcp", "/mcp list", "/mcp catalog", "/mcp add", "/mcp remove", "/mcp test",
                "/secret", "/secret list", "/secret add", "/secret remove", "/secret test",
                "/channel", "/channel list", "/channel run",
                "/skill", "/skill list", "/skill tree", "/skill show",
                "/memory", "/memory search",
                "/model", "/model list", "/model set",
                "/config", "/config show", "/config set", "/config reset",
            ],
        }
    }
}

impl Completer for SlashCompleter {
    type Candidate = Pair;

    fn complete(
        &self,
        line: &str,
        pos: usize,
        _ctx: &Context<'_>,
    ) -> rustyline::Result<(usize, Vec<Pair>)> {
        let line_lower = line[..pos].to_lowercase();

        // Only complete if line starts with /
        if !line_lower.starts_with('/') {
            return Ok((pos, vec![]));
        }

        let matches: Vec<Pair> = self
            .commands
            .iter()
            .filter(|cmd| cmd.to_lowercase().starts_with(&line_lower))
            .map(|cmd| Pair {
                display: cmd.to_string(),
                replacement: cmd.to_string(),
            })
            .collect();

        Ok((0, matches))
    }
}

impl Hinter for SlashCompleter {
    type Hint = String;

    fn hint(&self, line: &str, pos: usize, _ctx: &Context<'_>) -> Option<String> {
        let line_lower = line[..pos].to_lowercase();

        // Only hint if line starts with /
        if !line_lower.starts_with('/') {
            return None;
        }

        // Find first matching command
        self.commands
            .iter()
            .find(|cmd| cmd.to_lowercase().starts_with(&line_lower) && cmd.len() > pos)
            .map(|cmd| cmd[pos..].to_string())
    }
}

impl Highlighter for SlashCompleter {}
impl Validator for SlashCompleter {}
impl Helper for SlashCompleter {}

const VERSION: &str = env!("CARGO_PKG_VERSION");

async fn interactive() -> Result<()> {
    let theme = Theme::detect();
    if !config_path()?.exists() {
        println!();
        println!(
            "  {bold}Welcome to EvoClaw{reset} — let's get you set up.",
            bold = theme.bold(),
            reset = theme.reset(),
        );
        println!();
        println!("  Authentication options (you'll pick after choosing a provider):");
        println!(
            "    {ok}1){reset}  API key             — {dim}preferred · simplest · works for every vendor{reset}",
            ok = theme.ok(),
            dim = theme.dim(),
            reset = theme.reset(),
        );
        println!(
            "    {ok}2){reset}  Browser sign-in     — {dim}paste session token from your browser{reset}",
            ok = theme.ok(),
            dim = theme.dim(),
            reset = theme.reset(),
        );
        ensure_layout().await?;
        run_provider_wizard().await?;
        println!();
    }
    let mut cfg = load_config().await?;
    ensure_layout().await?;
    print_banner(&cfg).await;

    // Build the provider once for the whole shell session. ACP-backed
    // providers can take ~5-10 s to spawn (npx pull + JSON-RPC handshake);
    // doing that on every `evoclaw>` turn was the dominant cost users hit.
    // The (provider, is_acp) pair is rebuilt on `SlashOutcome::Reload` so
    // `/login` and `/agent add` switch the live backend mid-session.
    let (mut provider, mut is_acp) = match build_provider(&cfg).await {
        Ok(pair) => pair,
        Err(e) => {
            eprintln!(
                "{err}[error]{reset} failed to start provider: {e:#}",
                err = theme.err(),
                reset = theme.reset(),
            );
            return Err(e);
        }
    };

    // One log file per shell window. Every `evoclaw>` turn appends Task /
    // Turn / End records to this same JSONL. Default location is
    // `/tmp/evoclaw/session-*.jsonl` on Unix and `%TEMP%\evoclaw\…` on
    // Windows — overridable via env `EVO_LOG_DIR` or config `[logs] dir`.
    let session_log = session_log_path()?;
    println!(
        "{frame}→{reset} session log: {dim}{}{reset}",
        session_log.display(),
        frame = theme.frame(),
        dim = theme.dim(),
        reset = theme.reset(),
    );

    // Build the rustyline editor with slash command auto-completion.
    // Arrow keys, history (Ctrl-P / Ctrl-N), reverse-search (Ctrl-R),
    // Tab completion, and command hints all come courtesy of rustyline.
    // Vim-style keybindings: Ctrl+A (start), Ctrl+E (end), Ctrl+K (kill-end),
    // Ctrl+U (kill-start), Ctrl+W (kill-word) are enabled by default in Emacs mode.
    // History persists across sessions in `<logs_dir>/history.txt`.
    let config = rustyline::Config::builder()
        .edit_mode(rustyline::EditMode::Emacs)  // Emacs mode includes vim-style Ctrl bindings
        .auto_add_history(true)
        .build();
    let helper = SlashCompleter::new();
    let mut editor = rustyline::Editor::with_config(config)
        .map_err(|e| eyre::eyre!("init readline: {e}"))?;
    editor.set_helper(Some(helper));
    let history = history_path()?;
    if let Some(parent) = history.parent() {
        let _ = tokio::fs::create_dir_all(parent).await;
    }
    if history.exists() {
        let _ = editor.load_history(&history);
    }
    // Load MCP servers and track count for status display
    let mut mcp_count = 0;
    let mut registry = evo_tools::ToolRegistry::with_builtins();
    match mcp_tools::install_all(&mut registry).await {
        n if n > 0 => {
            mcp_count = n;
            println!(
                "{ok}✓{reset} MCP: {n} server(s) attached, registry has {} tools",
                registry.names().len(),
                ok = theme.ok(),
                reset = theme.reset(),
            );
        }
        _ => {}
    }

    let prompt = format!(
        "{frame}❯{reset} ",
        frame = theme.frame(),
        reset = theme.reset(),
    );

    // Track consecutive Ctrl+C presses for exit
    let mut ctrl_c_count = 0;
    // Track active skill (None for now, can be enhanced later)
    let active_skill: Option<String> = None;

    loop {
        // Display chat box top border before each prompt
        // Recalculate width each time for terminal resize support
        print!("{}", TerminalUI::chat_box_top(&theme));
        std::io::stdout().flush().ok();

        // rustyline is sync; bounce to a blocking thread so we don't stall
        // the tokio scheduler while waiting on user input.
        let line_res: Result<String, rustyline::error::ReadlineError> = {
            let prompt = prompt.clone();
            // The editor is owned here; we need to move it through the
            // blocking call and get it back.
            let (resp, ed) =
                tokio::task::spawn_blocking(move || (editor.readline(&prompt), editor))
                    .await
                    .map_err(|e| eyre::eyre!("readline join: {e}"))?;
            editor = ed;
            resp
        };
        let line = match line_res {
            Ok(l) => l,
            Err(rustyline::error::ReadlineError::Interrupted) => {
                // Ctrl-C: First press shows hint, second press exits
                ctrl_c_count += 1;
                if ctrl_c_count >= 2 {
                    println!();
                    println!(
                        "{frame}bye.{reset}",
                        frame = theme.frame(),
                        reset = theme.reset()
                    );
                    let _ = editor.save_history(&history);
                    return Ok(());
                } else {
                    println!(
                        "{dim}(Ctrl-C again to exit, or Ctrl-D){reset}",
                        dim = theme.dim(),
                        reset = theme.reset()
                    );
                    continue;
                }
            }
            Err(rustyline::error::ReadlineError::Eof) => {
                // Ctrl-D: clean exit.
                println!(
                    "{frame}bye.{reset}",
                    frame = theme.frame(),
                    reset = theme.reset()
                );
                let _ = editor.save_history(&history);
                return Ok(());
            }
            Err(e) => {
                eprintln!(
                    "{err}[error]{reset} readline: {e}",
                    err = theme.err(),
                    reset = theme.reset(),
                );
                continue;
            }
        };
        // Reset Ctrl+C counter on successful input
        ctrl_c_count = 0;

        // Display chat box bottom border and status after input
        let model_name = &cfg.model.default;
        let provider_id = cfg
            .model
            .provider
            .as_deref()
            .unwrap_or("unknown");
        let acp_status = if is_acp {
            Some(provider_id)
        } else {
            None
        };

        println!(
            "{}",
            TerminalUI::chat_box_bottom(
                &theme,
                model_name,
                provider_id,
                acp_status,
                mcp_count,
                active_skill.as_deref(),
            )
        );

        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        // Add to history (in-memory now, persisted on Drop / save_history).
        let _ = editor.add_history_entry(trimmed);
        // Persist history every line so a `kill -9` doesn't lose it.
        let _ = editor.save_history(&history);
        if let Some(rest) = trimmed.strip_prefix('/') {
            match handle_slash(rest).await? {
                SlashOutcome::Exit => {
                    println!(
                        "{frame}bye.{reset}",
                        frame = theme.frame(),
                        reset = theme.reset()
                    );
                    let _ = editor.save_history(&history);
                    return Ok(());
                }
                SlashOutcome::Reload => {
                    // /login or /agent add wrote new config — pick it up
                    // and rebuild the provider so the next turn uses the
                    // freshly-selected vendor.
                    cfg = load_config().await?;
                    match build_provider(&cfg).await {
                        Ok((p, acp)) => {
                            provider = p;
                            is_acp = acp;
                            let prov_id = cfg
                                .model
                                .provider
                                .clone()
                                .unwrap_or_else(|| "(unknown)".into());
                            println!(
                                "{ok}✓{reset} switched to provider {bold}{prov_id}{reset} {dim}({})",
                                cfg.model.default,
                                ok = theme.ok(),
                                bold = theme.bold(),
                                dim = theme.dim(),
                                reset = theme.reset(),
                            );
                            print!("{}", theme.reset());
                        }
                        Err(e) => {
                            eprintln!(
                                "{err}[error]{reset} failed to switch provider: {e:#}",
                                err = theme.err(),
                                reset = theme.reset(),
                            );
                        }
                    }
                }
                SlashOutcome::Continue => {}
            }
            continue;
        }
        if let Err(e) =
            run_task_with_provider(trimmed, provider.clone(), is_acp, &cfg, &session_log, theme)
                .await
        {
            eprintln!(
                "{err}[error]{reset} {e:#}",
                err = theme.err(),
                reset = theme.reset(),
            );
        }
    }
}

/// Inner width of the welcome box, matching the 63 `═` characters in the top
/// and bottom borders. All content rows pad-or-truncate to this width.
const BOX_INNER_W: usize = 63;
/// Width reserved for the value column. Row layout is
/// `│  <label-8>: <value-W>│` → 2 + 8 + 1 + 1 + W = BOX_INNER_W → W = INNER - 12.
const BOX_VALUE_W: usize = BOX_INNER_W - 12;

async fn print_banner(cfg: &Config) {
    let provider_id = cfg
        .model
        .provider
        .clone()
        .unwrap_or_else(|| "deepseek".into());
    let is_acp = provider_id.starts_with("acp:");
    let auth_method = cfg.auth.parsed();
    let (key_ok, key_status, account_status) = if is_acp {
        (
            true,
            "managed by external agent".into(),
            "external agent".into(),
        )
    } else {
        match auth_method {
            AuthMethod::Browser => match onboard::load_browser_profile(&provider_id).await {
                Ok(p) => {
                    let account = p
                        .account_label
                        .clone()
                        .unwrap_or_else(|| "not recorded — run /login to add it".into());
                    (
                        true,
                        format!("browser · captured {}", short_iso_date(&p.captured_at)),
                        account,
                    )
                }
                Err(_) => (
                    false,
                    "MISSING browser profile — run /login".into(),
                    "unknown".into(),
                ),
            },
            AuthMethod::Acp => (
                true,
                "ACP agent handles authentication".into(),
                "external".into(),
            ),
            AuthMethod::ApiKey => match onboard::resolve_api_key(&provider_id).await {
                Ok((_k, src)) => (
                    true,
                    format!("ok · {}", short_key_source(&src.describe())),
                    "not available for API key auth".into(),
                ),
                Err(_) => (false, "MISSING — run /login".into(), "unknown".into()),
            },
        }
    };
    let skill_count = count_skills().await.unwrap_or(0);
    let vault_count = count_vault_entries().await;
    let home = evoclaw_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    let home_disp = display_home(&home);

    let color = use_color();
    let cyan_b = if color { "\x1b[1;36m" } else { "" };
    let green = if color { "\x1b[32m" } else { "" };
    let red = if color { "\x1b[31m" } else { "" };
    let dim = if color { "\x1b[2m" } else { "" };
    let bold = if color { "\x1b[1m" } else { "" };
    let reset = if color { "\x1b[0m" } else { "" };

    let tagline = format!("local-first · self-evolving · v{VERSION}");
    println!();
    println!("{cyan_b}   ╔═══════════════════════════════════════════════════════════════╗{reset}");
    println!("{cyan_b}   ║                                                               ║{reset}");
    println!("{cyan_b}   ║      ███████╗██╗   ██╗ ██████╗  ██████╗██╗      █████╗ ██╗    ║{reset}");
    println!("{cyan_b}   ║      ██╔════╝██║   ██║██╔═══██╗██╔════╝██║     ██╔══██╗██║    ║{reset}");
    println!("{cyan_b}   ║      █████╗  ██║   ██║██║   ██║██║     ██║     ███████║██║    ║{reset}");
    println!("{cyan_b}   ║      ██╔══╝  ╚██╗ ██╔╝██║   ██║██║     ██║     ██╔══██║██║    ║{reset}");
    println!("{cyan_b}   ║      ███████╗ ╚████╔╝ ╚██████╔╝╚██████╗███████╗██║  ██║██║    ║{reset}");
    println!("{cyan_b}   ║      ╚══════╝  ╚═══╝   ╚═════╝  ╚═════╝╚══════╝╚═╝  ╚═╝╚═╝    ║{reset}");
    println!("{cyan_b}   ║                                                               ║{reset}");
    println!(
        "{cyan_b}   ║{reset}{}{cyan_b}║{reset}",
        center_pad(&format!("{dim}{tagline}{reset}"), BOX_INNER_W)
    );
    println!("{cyan_b}   ╚═══════════════════════════════════════════════════════════════╝{reset}");
    println!();
    println!("{bold}   ┌─ context ─────────────────────────────────────────────────────┐{reset}");
    print_row(
        bold,
        dim,
        reset,
        "home    ",
        &truncate_to(&home_disp, BOX_VALUE_W),
    );
    let prov_value = if is_acp {
        format!("{:<14} {dim}(external ACP agent){reset}", provider_id)
    } else if cfg.model.base_url.is_empty() {
        provider_id.clone()
    } else {
        format!("{:<10} {dim}({}){reset}", provider_id, cfg.model.base_url)
    };
    print_row(bold, dim, reset, "provider", &prov_value);
    let model_value = if is_acp {
        "(remote agent loop)".into()
    } else {
        truncate_to(&cfg.model.default, BOX_VALUE_W)
    };
    print_row(bold, dim, reset, "model   ", &model_value);
    let key_color = if key_ok { green } else { red };
    let key_value = format!("{key_color}{key_status}{reset}");
    let auth_label = if is_acp {
        "auth    "
    } else {
        match auth_method {
            AuthMethod::Browser => "browser ",
            AuthMethod::Acp => "auth    ",
            AuthMethod::ApiKey => "api key ",
        }
    };
    print_row(bold, dim, reset, auth_label, &key_value);
    print_row(
        bold,
        dim,
        reset,
        "account ",
        &truncate_to(&account_status, BOX_VALUE_W),
    );
    let vault_value = if vault_count > 0 {
        format!(
            "{green}{vault_count} entr{}{reset} {dim}· redactor active{reset}",
            if vault_count == 1 { "y" } else { "ies" }
        )
    } else {
        format!("{dim}empty · pattern fallback only{reset}")
    };
    print_row(bold, dim, reset, "vault   ", &vault_value);
    print_row(
        bold,
        dim,
        reset,
        "skills  ",
        &format!("{skill_count} learned"),
    );
    println!("{bold}   └───────────────────────────────────────────────────────────────┘{reset}");
    println!();
    println!("   {bold}Type a task in plain language to run the agent.{reset}");
    println!("   {dim}/help for slash commands  ·  Tab for auto-complete  ·  /exit or Ctrl-D to quit.{reset}");
}

/// Render one `│ label : value ...│` row, padding the visible portion of
/// `value` so the right border lands at the expected column.
fn print_row(bold: &str, dim: &str, reset: &str, label: &str, value: &str) {
    let visible = strip_ansi(value).chars().count();
    let pad = BOX_VALUE_W.saturating_sub(visible);
    let padding: String = " ".repeat(pad);
    println!("{bold}   │{reset}  {dim}{label}:{reset} {value}{padding}{bold}│{reset}",);
}

fn center_pad(s: &str, width: usize) -> String {
    let visible = strip_ansi(s).chars().count();
    if visible >= width {
        return s.to_string();
    }
    let total = width - visible;
    let left = total / 2;
    let right = total - left;
    format!("{}{s}{}", " ".repeat(left), " ".repeat(right))
}

fn use_color() -> bool {
    if std::env::var("NO_COLOR").is_ok() {
        return false;
    }
    if std::env::var("EVO_NO_COLOR").is_ok() {
        return false;
    }
    use std::io::IsTerminal;
    std::io::stdout().is_terminal()
}

/// Centralised colour palette with modern, tech-aesthetic colors.
/// Provides soft, professional colors that are easy on the eyes while maintaining
/// a high-tech feel. All colors are centralized and never hardcoded.
#[derive(Debug, Clone, Copy)]
struct Theme {
    enabled: bool,
}

/// Color definitions - centralized and easy to modify
/// Using 256-color palette for richer, more professional appearance
mod colors {
    // Primary colors - soft teal/cyan for tech aesthetic
    pub const PRIMARY: &str = "\x1b[38;5;51m";      // Soft cyan

    // Status colors - soft and professional
    pub const SUCCESS: &str = "\x1b[38;5;120m";     // Soft green
    pub const ERROR: &str = "\x1b[38;5;204m";       // Soft red
    pub const WARNING: &str = "\x1b[38;5;222m";     // Amber/orange
    pub const INFO: &str = "\x1b[38;5;117m";        // Soft blue

    // Accent colors
    pub const ACCENT: &str = "\x1b[38;5;141m";      // Soft purple
    pub const HIGHLIGHT: &str = "\x1b[38;5;228m";   // Soft yellow

    // Text styles
    pub const DIM: &str = "\x1b[38;5;240m";         // Gray for secondary info
    pub const BOLD: &str = "\x1b[1m";               // Bold
    pub const RESET: &str = "\x1b[0m";              // Reset all

    // Semantic colors for different use cases
    pub const LABEL: &str = "\x1b[38;5;249m";       // Light gray for labels
    pub const VALUE: &str = "\x1b[38;5;253m";       // Bright white for values
    pub const BORDER: &str = "\x1b[38;5;240m";      // Gray for borders
}

impl Theme {
    fn detect() -> Self {
        Self {
            enabled: use_color(),
        }
    }

    fn s(&self, code: &'static str) -> &'static str {
        if self.enabled {
            code
        } else {
            ""
        }
    }

    fn reset(&self) -> &'static str {
        self.s(colors::RESET)
    }

    /// Primary cyan — used for prompts, banners, and primary UI elements
    fn frame(&self) -> &'static str {
        self.s(colors::PRIMARY)
    }

    /// Soft green — success markers and positive feedback
    fn ok(&self) -> &'static str {
        self.s(colors::SUCCESS)
    }

    /// Soft red — error messages
    fn err(&self) -> &'static str {
        self.s(colors::ERROR)
    }

    /// Amber/orange — warnings and spinner
    fn warn(&self) -> &'static str {
        self.s(colors::WARNING)
    }

    /// Soft blue — informational messages
    fn info(&self) -> &'static str {
        self.s(colors::INFO)
    }

    /// Soft purple — system notices and accents
    fn accent(&self) -> &'static str {
        self.s(colors::ACCENT)
    }

    /// Soft yellow — highlights
    fn highlight(&self) -> &'static str {
        self.s(colors::HIGHLIGHT)
    }

    /// Gray — secondary metadata (paths, timing, etc.)
    fn dim(&self) -> &'static str {
        self.s(colors::DIM)
    }

    /// Bold — headings and emphasis
    fn bold(&self) -> &'static str {
        self.s(colors::BOLD)
    }

    /// Light gray — for labels in key-value displays
    fn label(&self) -> &'static str {
        self.s(colors::LABEL)
    }

    /// Bright white — for values in key-value displays
    fn value(&self) -> &'static str {
        self.s(colors::VALUE)
    }

    /// Gray — for borders and separators
    fn border(&self) -> &'static str {
        self.s(colors::BORDER)
    }
}

/// Template-based display utilities for consistent formatting
struct DisplayTemplate;

impl DisplayTemplate {
    /// Format a key-value pair with consistent styling
    fn kv(theme: &Theme, key: &str, value: &str) -> String {
        format!(
            "  {label}{key:.<18}{reset} {value_color}{value}{reset}",
            label = theme.label(),
            key = key,
            reset = theme.reset(),
            value_color = theme.value(),
            value = value
        )
    }

    /// Format a key-value pair with custom value color
    fn kv_colored(theme: &Theme, key: &str, value: &str, color: &str) -> String {
        format!(
            "  {label}{key:.<18}{reset} {color}{value}{reset}",
            label = theme.label(),
            key = key,
            reset = theme.reset(),
            color = color,
            value = value
        )
    }

    /// Format a section header
    fn header(theme: &Theme, title: &str) -> String {
        format!(
            "\n{border}╭─ {primary}{bold}{title}{reset}{border} ─────────────────────────────────────╮{reset}",
            border = theme.border(),
            primary = theme.frame(),
            bold = theme.bold(),
            title = title,
            reset = theme.reset()
        )
    }

    /// Format a section footer
    fn footer(theme: &Theme) -> String {
        format!(
            "{border}╰──────────────────────────────────────────────────────────────╯{reset}",
            border = theme.border(),
            reset = theme.reset()
        )
    }
}

/// Terminal UI utilities for adaptive layouts
struct TerminalUI;

impl TerminalUI {
    /// Get current terminal width, fallback to 100 if detection fails
    /// Checks COLUMNS environment variable first, then uses default
    fn width() -> usize {
        std::env::var("COLUMNS")
            .ok()
            .and_then(|s| s.parse::<usize>().ok())
            .unwrap_or(100) // Modern terminals are typically 100+ cols wide
    }

    /// Draw a simple thin separator line
    fn thin_separator(theme: &Theme) -> String {
        let width = Self::width();
        let line = "─".repeat(width);
        format!("{border}{line}{reset}", border = theme.border(), line = line, reset = theme.reset())
    }

    /// Format status information below the input prompt
    /// Shows: model, account, ACP/MCP status, active skill
    fn format_status(
        theme: &Theme,
        model: &str,
        provider: &str,
        acp_status: Option<&str>,
        mcp_count: usize,
        active_skill: Option<&str>,
    ) -> String {
        let mut parts = Vec::new();

        // Model info
        parts.push(format!(
            "{label}model:{reset} {value}{model}{reset}",
            label = theme.dim(),
            reset = theme.reset(),
            value = theme.frame(),
            model = model
        ));

        // Provider/Account info
        parts.push(format!(
            "{label}provider:{reset} {value}{provider}{reset}",
            label = theme.dim(),
            reset = theme.reset(),
            value = theme.info(),
            provider = provider
        ));

        // ACP status if present
        if let Some(acp) = acp_status {
            parts.push(format!(
                "{label}acp:{reset} {value}{acp}{reset}",
                label = theme.dim(),
                reset = theme.reset(),
                value = theme.accent(),
                acp = acp
            ));
        }

        // MCP servers count
        if mcp_count > 0 {
            parts.push(format!(
                "{label}mcp:{reset} {value}{count} server{s}{reset}",
                label = theme.dim(),
                reset = theme.reset(),
                value = theme.ok(),
                count = mcp_count,
                s = if mcp_count == 1 { "" } else { "s" }
            ));
        }

        // Active skill if any
        if let Some(skill) = active_skill {
            parts.push(format!(
                "{label}skill:{reset} {value}{skill}{reset}",
                label = theme.dim(),
                reset = theme.reset(),
                value = theme.highlight(),
                skill = skill
            ));
        }

        // Join with separator and pad
        let status_line = parts.join(&format!(" {dim}│{reset} ", dim = theme.dim(), reset = theme.reset()));
        format!("  {}", status_line)
    }

    /// Draw the chat box: separator line before prompt
    /// Creates the top border of the input box
    fn chat_box_top(theme: &Theme) -> String {
        format!("\n{}\n", Self::thin_separator(theme))
    }

    /// Draw the chat box bottom: separator + status line
    /// Creates the bottom border and status information
    fn chat_box_bottom(
        theme: &Theme,
        model: &str,
        provider: &str,
        acp_status: Option<&str>,
        mcp_count: usize,
        active_skill: Option<&str>,
    ) -> String {
        let mut output = String::new();

        // Bottom separator - forms the bottom of the chat box
        output.push_str(&Self::thin_separator(theme));
        output.push('\n');

        // Status line - shows current configuration
        output.push_str(&Self::format_status(theme, model, provider, acp_status, mcp_count, active_skill));

        output
    }
}

/// REPL history file. Persisted across sessions so arrow-up resurfaces
/// previous prompts. Lives in the same directory as the JSONL session
/// logs (default `/tmp/evoclaw/history.txt`).
fn history_path() -> Result<PathBuf> {
    Ok(logs_dir()?.join("history.txt"))
}

/// Enhanced terminal spinner with dynamic phase updates. Spawned on a thread
/// so the agent's `await` can run free; the `Drop` impl signals the thread
/// to stop and erases the spinner line so the caller can print clean text after.
struct Spinner {
    handle: Option<std::thread::JoinHandle<()>>,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    phase: std::sync::Arc<std::sync::Mutex<String>>,
}

impl Spinner {
    fn start(theme: Theme, label: &str) -> Self {
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stop_clone = stop.clone();
        let phase = std::sync::Arc::new(std::sync::Mutex::new(label.to_string()));
        let phase_clone = phase.clone();
        let warn = theme.warn().to_string();
        let dim = theme.dim().to_string();
        let reset = theme.reset().to_string();
        let handle = std::thread::spawn(move || {
            let frames: [&str; 10] = [
                "⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏",
            ];
            let started = std::time::Instant::now();
            let mut idx = 0usize;
            while !stop_clone.load(std::sync::atomic::Ordering::SeqCst) {
                let elapsed = started.elapsed().as_secs_f32();
                let current_phase = phase_clone.lock().unwrap().clone();
                eprint!(
                    "\r{warn}{}{reset} {current_phase} {dim}({:.1}s){reset}    ",
                    frames[idx % frames.len()],
                    elapsed,
                );
                use std::io::Write as _;
                std::io::stderr().flush().ok();
                idx = idx.wrapping_add(1);
                std::thread::sleep(std::time::Duration::from_millis(80));
            }
            // Erase the spinner line so the caller's print starts clean.
            eprint!("\r\x1b[2K");
            use std::io::Write as _;
            std::io::stderr().flush().ok();
        });
        Self {
            handle: Some(handle),
            stop,
            phase,
        }
    }

    /// Update the spinner's displayed phase/message
    fn update_phase(&self, new_phase: &str) {
        if let Ok(mut phase) = self.phase.lock() {
            *phase = new_phase.to_string();
        }
    }
}

impl Drop for Spinner {
    fn drop(&mut self) {
        self.stop
            .store(true, std::sync::atomic::Ordering::SeqCst);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

fn display_home(p: &str) -> String {
    if let Ok(home) = std::env::var("HOME") {
        if let Some(rest) = p.strip_prefix(&home) {
            return format!("~{rest}");
        }
    }
    p.to_string()
}

fn truncate_to(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        return s.to_string();
    }
    let mut out: String = s.chars().take(n.saturating_sub(1)).collect();
    out.push('…');
    out
}

fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_esc = false;
    for c in s.chars() {
        if in_esc {
            if c == 'm' {
                in_esc = false;
            }
            continue;
        }
        if c == '\x1b' {
            in_esc = true;
            continue;
        }
        out.push(c);
    }
    out
}

/// Trim an ISO-8601 timestamp (`2026-05-03T09:42:00Z`) to just `2026-05-03`
/// so the banner row stays inside `BOX_VALUE_W`. Falls back to the raw input
/// if it doesn't look like an ISO string.
fn short_iso_date(s: &str) -> String {
    s.split('T').next().unwrap_or(s).to_string()
}

/// Trim the `KeySource` description so the banner row never overflows. We only
/// keep the short tail of long secret-file paths.
fn short_key_source(s: &str) -> String {
    if let Some(rest) = s.strip_prefix("secrets file: ") {
        if let Ok(home) = std::env::var("HOME") {
            if let Some(tail) = rest.strip_prefix(&home) {
                return format!("secrets file: ~{tail}");
            }
        }
        if rest.len() > 32 {
            return format!("secrets file: …{}", &rest[rest.len() - 30..]);
        }
        return format!("secrets file: {rest}");
    }
    s.to_string()
}

async fn count_vault_entries() -> usize {
    let path = match vault_path() {
        Ok(p) => p,
        Err(_) => return 0,
    };
    match Vault::load(&path).await {
        Ok(v) => v.entries.len(),
        Err(_) => 0,
    }
}

async fn count_skills() -> Result<usize> {
    let dir = skills_dir()?;
    if !dir.exists() {
        return Ok(0);
    }
    let mut entries = tokio::fs::read_dir(&dir).await?;
    let mut n = 0;
    while let Some(e) = entries.next_entry().await? {
        if e.path().extension().and_then(|s| s.to_str()) == Some("yaml") {
            n += 1;
        }
    }
    Ok(n)
}

/// Outcome of a slash-command invocation. The interactive loop reads this
/// to decide whether to keep prompting (`Continue`), exit cleanly (`Exit`),
/// or reload `Config` + provider (`Reload`) — the latter is essential for
/// `/login` and `/agent add <id>` to actually take effect within the same
/// shell session (otherwise the cached `provider` keeps using whatever was
/// configured at startup, which was the "switched vendor but still on
/// claude" bug).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SlashOutcome {
    Continue,
    Exit,
    Reload,
}

async fn handle_slash(rest: &str) -> Result<SlashOutcome> {
    let mut parts = rest.split_whitespace();
    let cmd = parts.next().unwrap_or("");
    let args: Vec<&str> = parts.collect();
    match cmd {
        "exit" | "quit" | "q" => {
            return Ok(SlashOutcome::Exit);
        }
        "help" | "?" => print_help(),
        "login" => {
            login_cmd().await?;
            return Ok(SlashOutcome::Reload);
        }
        "agent" => match args.as_slice() {
            [] | ["list"] => agent_list().await?,
            ["catalog"] => agent_catalog(),
            ["add", id] => {
                agent_add(id).await?;
                // Adding an ACP agent doesn't itself flip the active
                // provider, but if the user just added the one they're
                // about to switch to, reloading is the right default.
                return Ok(SlashOutcome::Reload);
            }
            ["remove", id] => agent_remove(id).await?,
            ["test", id] => agent_test(id).await?,
            _ => println!("usage: /agent [list|catalog|add <id>|remove <id>|test <id>]"),
        },
        "mcp" => match args.as_slice() {
            [] | ["list"] => mcp_list().await?,
            ["catalog"] => mcp_catalog(),
            ["add", id] => mcp_add(id).await?,
            ["remove", id] => mcp_remove(id).await?,
            ["test", id] => mcp_test(id).await?,
            _ => println!("usage: /mcp [list|catalog|add <id>|remove <id>|test <id>]"),
        },
        "secret" => match args.as_slice() {
            [] | ["list"] => secret_list().await?,
            ["add", name] => secret_add(name, true, None).await?,
            ["add", name, value] => secret_add(name, false, Some(value.to_string())).await?,
            ["remove", name] => secret_remove(name).await?,
            ["test", rest @ ..] => secret_test(&rest.join(" ")).await?,
            _ => println!("usage: /secret [list|add <name> [value]|remove <name>|test <text>]"),
        },
        "channel" => match args.as_slice() {
            [] | ["list"] => channel_list().await?,
            ["run", kind] => channel_run(kind).await?,
            _ => println!("usage: /channel [list|run <kind>]   (built-in: local-pipe)"),
        },
        "clear" => {
            print!("\x1b[2J\x1b[H");
            std::io::stdout().flush().ok();
        }
        "doctor" => doctor().await?,
        "tokens" => doctor_tokens().await?,
        "closure" => doctor_closure().await?,
        "replay" => replay(args.first().map(PathBuf::from)).await?,
        "skill" => match args.as_slice() {
            [] | ["list"] => skill_list().await?,
            ["tree"] => skill_tree().await?,
            ["show", id] => skill_show(id).await?,
            _ => println!("usage: /skill [list|tree|show <id>]"),
        },
        "memory" => match args.as_slice() {
            [] => println!("usage: /memory <query>"),
            ["search", q @ ..] => memory_search(&q.join(" "), 20).await?,
            q => memory_search(&q.join(" "), 20).await?,
        },
        "logout" => {
            logout_cmd().await?;
            return Ok(SlashOutcome::Reload);
        }
        "usage" => usage_cmd().await?,
        "config" => match args.as_slice() {
            [] | ["show"] => config_show().await?,
            ["set", key, value] => config_set(key, value).await?,
            ["reset"] => config_reset().await?,
            _ => println!("usage: /config [show|set <key> <value>|reset]"),
        },
        "status" => status_cmd().await?,
        "model" => match args.as_slice() {
            [] => model_show().await?,
            ["list"] => model_list().await?,
            ["set", model_name] => {
                model_set(model_name).await?;
                return Ok(SlashOutcome::Reload);
            }
            _ => println!("usage: /model [list|set <model_name>]"),
        },
        "profile" => match args.as_slice() {
            [] | ["show"] => profile_show(None).await?,
            ["show", name] => profile_show(Some(name)).await?,
            ["list"] | ["ls"] => profile_list().await?,
            ["switch" | "use", name] => {
                profile_switch(name).await?;
                return Ok(SlashOutcome::Reload);
            }
            ["add", name] => profile_add(name, args.get(3).copied()).await?,
            ["remove" | "rm", name] => profile_remove(name).await?,
            ["edit", name] => profile_edit(Some(name)).await?,
            ["edit"] => profile_edit(None).await?,
            _ => println!("usage: /profile [list|show [name]|switch <name>|add <name>|remove <name>|edit [name]]"),
        },
        other => println!("unknown command: /{other}  (try /help)"),
    }
    Ok(SlashOutcome::Continue)
}

fn print_help() {
    println!();
    println!("slash commands:");
    println!("  /help                show this help");
    println!("  /login               switch provider / re-enter API key");
    println!("  /logout              clear current auth and return to login");
    println!("  /agent [sub]         ACP external agents (claude/codex/cursor/copilot)");
    println!("  /mcp   [sub]         MCP servers (filesystem/github/fetch/...)");
    println!("  /secret [sub]        local-only key vault (values never reach the model)");
    println!("  /channel [sub]       multi-channel adapters (local-pipe / v0.6 plan)");
    println!("  /skill list          list every skill on disk");
    println!("  /skill tree          rebuild and print skill tree");
    println!("  /skill show <id>     dump one skill's YAML");
    println!("  /memory <query>      grep memory L1/L2/L3");
    println!("  /model [sub]         show/change current model");
    println!("  /profile [sub]       manage configuration profiles");
    println!("  /config [sub]        view/modify configuration");
    println!("  /status              show current session status");
    println!("  /usage               alias for /tokens");
    println!("  /tokens              7-day / 30-day cost & cache stats");
    println!("  /closure             session JSONL audit (PRD §39)");
    println!("  /replay [path]       pretty-print a session (latest by default)");
    println!("  /doctor              health check");
    println!("  /clear               clear screen");
    println!("  /exit  /quit  /q     exit (also Ctrl-D, or Ctrl-C twice)");
    println!();
    println!("keyboard shortcuts:");
    println!("  Tab                  auto-complete slash commands");
    println!("  ↑/↓ or Ctrl-P/N      history navigation");
    println!("  Ctrl-R               reverse search history");
    println!("  Ctrl-A / Ctrl-E      jump to start / end of line");
    println!("  Ctrl-K / Ctrl-U      delete to end / start of line");
    println!("  Ctrl-W               delete previous word");
    println!("  Ctrl-C (twice)       exit");
    println!();
    println!("anything else is treated as a task and runs through the agent loop.");
}

// ---------------------------------------------------------------------------
// Helper functions
// ---------------------------------------------------------------------------

/// Get list of active MCP server names by reading *.toml files in mcp/ directory
async fn get_active_mcp_servers() -> Result<Vec<String>> {
    let mcp_dir = evoclaw_dir()?.join("mcp");
    if !mcp_dir.exists() {
        return Ok(Vec::new());
    }

    let mut servers = Vec::new();
    let mut entries = tokio::fs::read_dir(&mcp_dir).await?;
    while let Some(entry) = entries.next_entry().await? {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) == Some("toml") {
            if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                servers.push(stem.to_string());
            }
        }
    }
    servers.sort();
    Ok(servers)
}

// ---------------------------------------------------------------------------
// Subcommand handlers
// ---------------------------------------------------------------------------

async fn onboard_cmd() -> Result<()> {
    let cfg_path = config_path()?;
    let already = cfg_path.exists();
    if already {
        println!("config.toml exists at {}", cfg_path.display());
        print!("Overwrite with the wizard? [y/N] ");
        std::io::stdout().flush().ok();
        let mut line = String::new();
        std::io::stdin().read_line(&mut line)?;
        if !line.trim().eq_ignore_ascii_case("y") {
            println!("kept existing config; run `evoclaw login` to switch provider only.");
            return Ok(());
        }
    }
    ensure_layout().await?;
    run_provider_wizard().await?;
    println!();
    println!("Done. Run `evoclaw` (no args) to enter the interactive shell.");
    Ok(())
}

async fn login_cmd() -> Result<()> {
    ensure_layout().await?;
    run_provider_wizard().await?;
    println!();
    println!("Login complete. Resume with `evoclaw`.");
    Ok(())
}

async fn run_provider_wizard() -> Result<()> {
    let mut choice = onboard::pick_provider().await?;
    // ACP-prefixed providers (advanced "evoclaw agent add" path) keep the
    // legacy flow — they're not part of the new 3-way auth picker.
    if choice.id.starts_with("acp:") {
        let cfg_path = onboard::save_config(&choice).await?;
        println!("  saved config -> {}", cfg_path.display());
        return Ok(());
    }
    let auth = onboard::pick_auth_method(&choice)?;
    match auth {
        AuthMethod::ApiKey => {
            let key_opt = onboard::ask_api_key(&choice).await?;
            if let Some(ref key) = key_opt {
                let path = onboard::save_secret(&choice.id, key).await?;
                println!("  saved key    -> {}", path.display());
            }
            // Best-effort: pick a specific model from the provider's /models.
            onboard::pick_model(&mut choice, key_opt.as_deref()).await?;
            let cfg_path = onboard::save_config_with_auth(&choice, AuthMethod::ApiKey).await?;
            println!("  saved config -> {}", cfg_path.display());
        }
        AuthMethod::Browser => {
            let profile = onboard::capture_browser_profile(&choice).await?;
            let path = onboard::save_browser_profile(&profile).await?;
            println!("  saved browser profile -> {}", path.display());
            // /models probe with the captured token (best-effort, same code
            // path as API-key flow — the vendor's auth header doesn't change
            // the JSON response shape).
            onboard::pick_model(&mut choice, Some(&profile.session_token)).await?;
            let cfg_path = onboard::save_config_with_auth(&choice, AuthMethod::Browser).await?;
            println!("  saved config -> {}", cfg_path.display());
        }
        AuthMethod::Acp => {
            // User selected ACP agent for this provider. Configure the corresponding
            // ACP agent automatically.
            let agent_id = onboard::provider_to_acp_agent(&choice.id)
                .ok_or_else(|| eyre::eyre!("No ACP agent available for provider '{}'", choice.id))?;

            // Find the agent profile from catalog
            let agent_profile = evo_acp_client::find_agent(agent_id)
                .ok_or_else(|| eyre::eyre!("ACP agent '{}' not found in catalog", agent_id))?;

            // Save the agent configuration
            let agent_config = evo_acp_client::AgentConfig::from_profile(agent_profile);
            let agent_path = evo_acp_client::save_agent(&agent_config)
                .await
                .map_err(|e| eyre::eyre!("save agent {}: {e}", agent_id))?;

            println!();
            println!("  ✓ saved ACP agent profile -> {}", agent_path.display());
            println!("    Agent: {}", agent_profile.name);
            println!("    Command: {} {}", agent_config.command, agent_config.args.join(" "));
            println!("    Install: {}", agent_profile.install_hint);
            println!("    Auth: {}", agent_profile.auth_hint);

            // Save config with acp: prefix
            let acp_choice = ProviderChoice {
                id: format!("acp:{}", agent_id),
                name: agent_profile.name.clone(),
                base_url: String::new(),
                default_model: format!("acp:{}", agent_id),
                fallback: Vec::new(),
                key_url: None,
                local: true,
            };

            let cfg_path = onboard::save_config(&acp_choice).await?;
            println!("  saved config -> {}", cfg_path.display());

            // Test ACP agent connection
            println!();
            println!("  Testing ACP agent connection...");

            match test_acp_connection(&agent_config).await {
                Ok(_) => {
                    println!("  ✓ Connection test PASSED");
                    println!("  ✓ ACP agent '{}' is ready to use", agent_profile.name);
                }
                Err(e) => {
                    println!("  ✗ Connection test FAILED: {}", e);
                    println!();
                    println!("  Troubleshooting:");
                    println!("    1. Check if the agent is installed: {}", agent_profile.install_hint);
                    println!("    2. Verify authentication: {}", agent_profile.auth_hint);
                    println!("    3. Try running the command manually:");
                    println!("       {} {}", agent_config.command, agent_config.args.join(" "));
                    println!();
                    println!("  Configuration saved but connection failed.");
                    println!("  Run `evoclaw doctor` to diagnose or retry with `evoclaw login`.");
                }
            }
        }
    }
    Ok(())
}

/// Test ACP agent connection by spawning the agent and performing a handshake.
///
/// Returns Ok(()) if connection succeeds, Err with details if it fails.
/// Uses a timeout to avoid hanging on unresponsive agents.
async fn test_acp_connection(agent_config: &evo_acp_client::AgentConfig) -> Result<()> {
    use tokio::time::{timeout, Duration};

    // Create a temporary ACP client for testing
    let client = evo_acp_client::AcpClient::new();

    // Step 1: Try to spawn with a 30-second timeout
    let spawn_result = timeout(
        Duration::from_secs(30),
        client.spawn(agent_config)
    ).await;

    match spawn_result {
        Ok(Ok(())) => {
            // Spawn succeeded - now try initialize handshake
        }
        Ok(Err(e)) => {
            return Err(eyre::eyre!("spawn failed: {}", e));
        }
        Err(_) => {
            drop(client);
            return Err(eyre::eyre!(
                "spawn timed out after 30s. Agent may need installation or user input."
            ));
        }
    }

    // Step 2: Try initialize handshake with a 30-second timeout
    let init_result = timeout(
        Duration::from_secs(30),
        client.initialize("evoclaw-test", env!("CARGO_PKG_VERSION"))
    ).await;

    match init_result {
        Ok(Ok(result)) => {
            // Success - display server info if available
            if let Some(info) = result.get("serverInfo") {
                println!("  Server info: {}", info);
            }
            // Clean shutdown
            client.shutdown().await.ok();
            Ok(())
        }
        Ok(Err(e)) => {
            client.shutdown().await.ok();
            Err(eyre::eyre!("initialize handshake failed: {}", e))
        }
        Err(_) => {
            client.shutdown().await.ok();
            Err(eyre::eyre!(
                "initialize timed out after 30s. Agent may require authentication first."
            ))
        }
    }
}

async fn doctor() -> Result<()> {
    println!("== evoclaw doctor ==");
    let dir = evoclaw_dir()?;
    println!("home     : {}", dir.display());
    let cfg = match load_config().await {
        Ok(c) => {
            println!("config   : OK ({})", config_path()?.display());
            c
        }
        Err(e) => {
            println!("config   : MISSING — {e:#}\nrun `evoclaw onboard`");
            return Ok(());
        }
    };
    let provider_id = cfg
        .model
        .provider
        .clone()
        .unwrap_or_else(|| "deepseek".into());
    println!("provider : {provider_id}");
    println!("base_url : {}", cfg.model.base_url);
    println!("model    : {}", cfg.model.default);
    println!("workspace: {}", workspace_dir()?.display());
    println!("logs     : {}", logs_dir()?.display());
    println!("secrets  : {}", onboard::secrets_dir()?.display());
    // ACP-backed provider: auth is delegated to the upstream agent CLI
    // (claude-agent-acp / codex-acp / cursor-agent / amp / auggie / …).
    // We never see the user's vendor credentials, so showing
    // "api_key MISSING" here is misleading. Surface the agent profile
    // instead.
    if let Some(agent_id) = provider_id.strip_prefix("acp:") {
        match evo_acp_client::load_agent(agent_id).await {
            Ok(c) => {
                println!(
                    "acp      : OK (agent='{}', command='{} {}')",
                    c.id,
                    c.command,
                    c.args.join(" ")
                );
            }
            Err(e) => {
                println!(
                    "acp      : MISSING — {e:#}\nrun `evoclaw agent add {agent_id}`"
                );
            }
        }
        return Ok(());
    }
    let auth_method = cfg.auth.parsed();
    println!("auth     : {} ({})", auth_method.label(), auth_method.as_str());
    match auth_method {
        AuthMethod::Browser => match onboard::load_browser_profile(&provider_id).await {
            Ok(p) => println!(
                "browser  : OK ({}, captured {})",
                onboard::browser_profile_path(&provider_id)?.display(),
                p.captured_at
            ),
            Err(e) => println!(
                "browser  : MISSING — {e:#}\nrun `evoclaw login` and pick (2) Browser sign-in"
            ),
        },
        AuthMethod::Acp => {
            // Config shows ACP but it's actually handled via acp: provider prefix
            println!("acp      : configured (auth handled by external agent)")
        },
        AuthMethod::ApiKey => match onboard::resolve_api_key(&provider_id).await {
            Ok((_k, src)) => println!("api_key  : OK ({})", src.describe()),
            Err(e) => println!("api_key  : MISSING — {e:#}\nrun `evoclaw login`"),
        },
    }
    Ok(())
}

/// Build the provider once. ACP-backed providers spawn a child process and
/// run a JSON-RPC handshake — both are slow (npx fetch + SDK initialize),
/// so the interactive shell builds the provider ONCE and reuses it across
/// every `evoclaw>` turn (see `interactive()`). The boolean flag is `true`
/// when this is an ACP backend → caller passes it as
/// `RuntimeConfig.reflection_enabled = false` so the upstream agent isn't
/// asked to write a meta-reflection on every short interaction.
async fn build_provider(cfg: &Config) -> Result<(Arc<dyn Provider>, bool)> {
    let provider_id = cfg
        .model
        .provider
        .clone()
        .unwrap_or_else(|| "deepseek".into());
    if let Some(agent_id) = provider_id.strip_prefix("acp:") {
        let p = AcpProvider::spawn(agent_id)
            .await
            .map_err(|e| eyre::eyre!("{e:#}"))?;
        return Ok((Arc::new(p) as Arc<dyn Provider>, true));
    }
    let provider: Arc<dyn Provider> = match cfg.auth.parsed() {
        AuthMethod::Browser => {
            let profile = onboard::load_browser_profile(&provider_id)
                .await
                .wrap_err_with(|| {
                    format!(
                        "load browser profile for '{provider_id}'. \
                         Run `evoclaw login` and pick (2) Browser sign-in."
                    )
                })?;
            Arc::new(BrowserProvider::from_profile(&profile)) as Arc<dyn Provider>
        }
        AuthMethod::Acp => {
            return Err(eyre::eyre!(
                "config.toml has [auth].method = \"acp\" but provider is not set to an ACP agent. \
                 Run `evoclaw login` and select 'External ACP agent' from the provider list."
            ));
        }
        AuthMethod::ApiKey => {
            let (api_key, _src) = onboard::resolve_api_key(&provider_id).await?;
            match provider_id.as_str() {
                "anthropic" => Arc::new(AnthropicProvider::new(api_key, cfg.model.default.clone()))
                    as Arc<dyn Provider>,
                "copilot" => Arc::new(CopilotProvider::new(api_key, cfg.model.default.clone())),
                _ => Arc::new(OpenAiCompatProvider::new(
                    cfg.model.base_url.clone(),
                    api_key,
                    cfg.model.default.clone(),
                )),
            }
        }
    };
    Ok((provider, false))
}

/// Run a single task with an already-built provider. Each invocation creates
/// a fresh session/redactor (cheap IO) but **reuses the provider** so shell
/// loops don't repay process-spawn cost on every turn.
///
/// `log_path` controls where the JSONL records land. The interactive shell
/// passes the same per-window file every turn (so a whole session lives in
/// one log); the `Cmd::Run` one-shot path passes a dedicated `task-*.jsonl`.
///
/// `theme` controls colour output: drives the spinner and the final result
/// banner. Pass `Theme::detect()` for the standard auto-detected palette.
async fn run_task_with_provider(
    input: &str,
    provider: Arc<dyn Provider>,
    is_acp: bool,
    cfg: &Config,
    log_path: &Path,
    theme: Theme,
) -> Result<()> {
    ensure_layout().await?;
    let mut registry = ToolRegistry::with_builtins();
    let attached_servers = mcp_tools::install_all(&mut registry).await;
    if attached_servers > 0 {
        println!(
            "→ MCP: {attached_servers} server(s) attached, registry now has {} tools",
            registry.names().len()
        );
    }
    let registry = Arc::new(registry);
    let session = Session::open(log_path).await?;
    let tool_ctx = ToolContext {
        workspace: workspace_dir()?,
        allow_user_prompt: true,
        ..Default::default()
    };
    let cost_engine = Arc::new(CostEngine::at(cost_log_path()?, BudgetCfg::default()));
    let memory = Memory::at(memory_dir()?);
    let vault = Vault::load(&vault_path()?).await.wrap_err_with(|| {
        format!(
            "read vault at {}",
            vault_path()
                .map(|p| p.display().to_string())
                .unwrap_or_default()
        )
    })?;
    let redactor = Redactor::from_vault(&vault);
    if !redactor.is_empty() {
        println!(
            "→ secret vault: {} entr{}",
            redactor.entry_count(),
            if redactor.entry_count() == 1 {
                "y"
            } else {
                "ies"
            }
        );
    }
    let mut runtime = ConversationRuntime::new(
        provider,
        registry,
        session,
        tool_ctx,
        evo_core::runtime::RuntimeConfig {
            model: cfg.model.default.clone(),
            // ACP backend = upstream is already a full agent → don't pay for
            // an extra reflection round per task. For API providers we keep
            // the reflection so memory/skill learning still works.
            reflection_enabled: !is_acp,
            provider_id: cfg.model.provider.clone(),
            mcp_servers: get_active_mcp_servers().await.unwrap_or_default(),
            ..Default::default()
        },
    )
    .with_cost_engine(cost_engine)
    .with_memory(memory)
    .with_skills_dir(skills_dir()?)
    .with_redactor(redactor);
    let started = std::time::Instant::now();
    println!(
        "{frame}→{reset} running… {dim}log: {}{reset}",
        log_path.display(),
        frame = theme.frame(),
        dim = theme.dim(),
        reset = theme.reset(),
    );
    // Enhanced spinner with progress updates. Dropped automatically at
    // function exit which clears the line so the answer prints clean.
    let spinner = Spinner::start(theme, "initializing…");

    // Show progress during execution
    spinner.update_phase("connecting to provider…");
    std::thread::sleep(std::time::Duration::from_millis(100)); // brief pause for visibility

    spinner.update_phase("processing request…");
    let outcome = runtime.run(input).await?;
    drop(spinner);
    let elapsed = started.elapsed();
    let usage = &outcome.usage;
    let usage_summary = if usage.turns_with_usage == 0 {
        // ACP backends don't surface per-turn `Usage` events — the
        // upstream agent does its own metering.
        format!(
            "{dim}tokens reported by upstream agent{reset}",
            dim = theme.dim(),
            reset = theme.reset(),
        )
    } else {
        format!(
            "{dim}{}↑ in · {}↓ out · {} cached ({:.0}% hit){reset}",
            usage.input_tokens,
            usage.output_tokens,
            usage.cached_tokens,
            usage.cache_hit_rate() * 100.0,
            dim = theme.dim(),
            reset = theme.reset(),
        )
    };
    println!(
        "\n{frame}╭─{reset} {bold}answer{reset} {dim}· {} turn{} · {:.1}s{reset}",
        outcome.turns,
        if outcome.turns == 1 { "" } else { "s" },
        elapsed.as_secs_f32(),
        frame = theme.frame(),
        bold = theme.bold(),
        dim = theme.dim(),
        reset = theme.reset(),
    );
    println!(
        "{ok}{}{reset}",
        outcome.final_text,
        ok = theme.ok(),
        reset = theme.reset(),
    );
    println!(
        "{frame}╰─{reset} {usage_summary}",
        frame = theme.frame(),
        reset = theme.reset(),
    );
    Ok(())
}

async fn run_one_shot(input: &str) -> Result<()> {
    let cfg = load_config().await?;
    ensure_layout().await?;
    let (provider, is_acp) = build_provider(&cfg).await?;
    // One-shot CLI invocations get a dedicated `task-*.jsonl` so each run
    // is individually replayable; the interactive shell uses one
    // `session-*.jsonl` per window (see `interactive()`).
    let task_id = format!("task-{}", chrono::Utc::now().format("%Y%m%dT%H%M%S%.3f"));
    let log_path = logs_dir()?.join(format!("{task_id}.jsonl"));
    run_task_with_provider(input, provider, is_acp, &cfg, &log_path, Theme::detect()).await
}

async fn replay(path: Option<PathBuf>) -> Result<()> {
    let chosen = match path {
        Some(p) => p,
        None => most_recent_session().await?,
    };
    let records = Session::read_all(&chosen).await?;
    println!(
        "== replay {} ({} records) ==\n",
        chosen.display(),
        records.len()
    );
    for r in records {
        match r {
            evo_core::session::SessionRecord::Task(t) => {
                println!(
                    "[TASK] {}\n  input : {}\n  source: {}\n  model : {}\n  start : {}\n",
                    t.task_id, t.user_input, t.source, t.model, t.started_at
                );
            }
            evo_core::session::SessionRecord::Turn(t) => {
                println!("[TURN {}] {} tool_calls", t.turn, t.tool_calls.len());
                if let Some(s) = &t.summary {
                    println!("  summary: {s}");
                }
                for tc in &t.tool_calls {
                    let preview = tc
                        .result_truncated
                        .lines()
                        .take(2)
                        .collect::<Vec<_>>()
                        .join(" | ");
                    let flag = if tc.is_error { "x" } else { "ok" };
                    println!("  [{flag}] {} args={} -> {}", tc.name, tc.args, preview);
                }
                if let Some(u) = &t.usage {
                    let hit = if u.input == 0 {
                        0.0
                    } else {
                        u.cached as f64 / u.input as f64 * 100.0
                    };
                    println!(
                        "  usage: in={} cached={} ({:.0}% hit) out={}",
                        u.input, u.cached, hit, u.output
                    );
                }
                println!();
            }
            evo_core::session::SessionRecord::End(e) => {
                println!("[END] {} @ {}\n", e.state, e.finished_at);
            }
        }
    }
    Ok(())
}

/// Path to the persistent gateway token. We co-locate it with the rest of
/// `~/.evoclaw` runtime state, in its own subdirectory so the file's chmod
/// 600 is meaningful (not shared with public artefacts).
fn gateway_token_path() -> Result<PathBuf> {
    Ok(evoclaw_dir()?.join("gateway").join("token"))
}

/// SHA-256 fingerprint of the bearer token, first 8 hex chars. Safe to log /
/// print: an attacker cannot recover the token from this — but operators can
/// confirm the same value is in use across processes. Reuses
/// `evo_policy::fingerprint_of` so we add no new top-level dependency.
fn token_fingerprint(s: &str) -> String {
    evo_policy::fingerprint_of(s)
}

/// Resolve the bearer token to use for `evo gateway`:
///   * `--token <T>` provided   → use as-is (operator override)
///   * persisted file present   → read it, validate, reuse
///   * neither                  → generate a fresh 32-hex random token,
///     write it (mode 0600), print it ONCE.
async fn resolve_gateway_token(cli_override: Option<&str>) -> Result<(String, bool)> {
    if let Some(t) = cli_override {
        let t = t.trim();
        if t.is_empty() {
            return Err(eyre::eyre!("--token may not be empty"));
        }
        return Ok((t.to_string(), false));
    }
    let path = gateway_token_path()?;
    if let Ok(raw) = tokio::fs::read_to_string(&path).await {
        let trimmed = raw.trim().to_string();
        if !trimmed.is_empty() && trimmed.chars().all(|c| c.is_ascii_alphanumeric()) {
            return Ok((trimmed, false));
        }
        // Corrupt or empty file — fall through and regenerate.
    }
    let fresh = generate_token_hex(16).await?;
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    tokio::fs::write(&path, &fresh).await?;
    set_file_mode_600(&path).await.ok();
    Ok((fresh, true))
}

/// Read `n_bytes` bytes from `/dev/urandom` (Unix) and hex-encode them. On
/// non-Unix platforms, fall back to a SHA-256 of high-entropy process state —
/// still vastly better than the previous default of the literal `"dev"`.
async fn generate_token_hex(n_bytes: usize) -> Result<String> {
    #[cfg(unix)]
    {
        use tokio::io::AsyncReadExt;
        if let Ok(mut f) = tokio::fs::File::open("/dev/urandom").await {
            let mut buf = vec![0u8; n_bytes];
            f.read_exact(&mut buf).await?;
            return Ok(bytes_to_hex(&buf));
        }
    }
    let _ = n_bytes; // signature consistency on non-Unix fallback
    let now = chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0);
    let pid = std::process::id();
    let stack_addr = &now as *const _ as usize;
    let env_hash: usize = std::env::vars()
        .map(|(k, v)| k.len().wrapping_mul(31).wrapping_add(v.len()))
        .fold(0usize, |a, b| a.wrapping_add(b));
    // `fingerprint_of` returns 8 hex chars; concatenate four seeded hashes
    // to reach the 32-hex shape promised by the docstring above.
    let seed = format!("{now}-{pid}-{stack_addr}-{env_hash}");
    let a = evo_policy::fingerprint_of(&format!("{seed}-A"));
    let b = evo_policy::fingerprint_of(&format!("{seed}-B"));
    let c = evo_policy::fingerprint_of(&format!("{seed}-C"));
    let d = evo_policy::fingerprint_of(&format!("{seed}-D"));
    Ok(format!("{a}{b}{c}{d}"))
}

/// Inline lower-case hex encoder — avoids pulling the `hex` crate in as a new
/// dep of `evo-cli`. (`hex` is already used by `evo-policy`, but this crate's
/// Cargo.toml doesn't list it and the workspace constraint forbids new deps.)
fn bytes_to_hex(bytes: &[u8]) -> String {
    const TABLE: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(TABLE[(b >> 4) as usize] as char);
        out.push(TABLE[(b & 0x0f) as usize] as char);
    }
    out
}

#[cfg(unix)]
async fn set_file_mode_600(path: &std::path::Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let perms = std::fs::Permissions::from_mode(0o600);
    tokio::fs::set_permissions(path, perms).await
}
#[cfg(not(unix))]
async fn set_file_mode_600(_path: &std::path::Path) -> std::io::Result<()> {
    Ok(())
}

async fn gateway(bind: &str, token_arg: Option<&str>) -> Result<()> {
    use std::process::Stdio;
    ensure_layout().await?;
    let (token, freshly_generated) = resolve_gateway_token(token_arg).await?;
    let fp = token_fingerprint(&token);
    let token_path = gateway_token_path()?;

    let mut cmd = tokio::process::Command::new("evo-gateway");
    cmd.env("EVO_GATEWAY_BIND", bind)
        .env("EVO_GATEWAY_ALLOWLIST", &token)
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    let mut child = cmd.spawn().map_err(|e| {
        eyre::eyre!(
            "evo-gateway binary not found on PATH: {e}. Build with `cargo build -p evo-gateway`."
        )
    })?;
    // Never echo the raw token after this point. Operators who need it can
    // either pass `--token` themselves or read the chmod-600 file directly.
    println!("→ evo-gateway started, bound to {bind} (token fingerprint: {fp})");
    println!("  WebChat: http://{bind}");
    if freshly_generated {
        println!();
        println!("  ╔══════════════════════════════════════════════════════════════╗");
        println!("  ║  A NEW gateway token has been generated and saved to disk.   ║");
        println!("  ║  Save this — it WILL NOT be shown again.                     ║");
        println!("  ║                                                              ║");
        println!("  ║    token: {token:<50}║");
        println!("  ║    file : {:<50}║", token_path.display());
        println!("  ║    chmod: 0600 (owner read/write only)                       ║");
        println!("  ╚══════════════════════════════════════════════════════════════╝");
        println!();
    } else if token_arg.is_none() {
        println!("  (token loaded from {})", token_path.display());
    }
    let status = child.wait().await?;
    if !status.success() {
        return Err(eyre::eyre!("evo-gateway exited: {status}"));
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::items_after_test_module)]
mod gateway_tests {
    use super::*;

    #[test]
    fn token_fingerprint_is_8_hex() {
        let fp = token_fingerprint("hello");
        assert_eq!(fp.len(), 8);
        assert!(fp.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn token_fingerprint_is_stable_and_distinguishes_inputs() {
        assert_eq!(token_fingerprint("same"), token_fingerprint("same"));
        assert_ne!(token_fingerprint("a"), token_fingerprint("b"));
    }

    #[test]
    fn bytes_to_hex_matches_known_vectors() {
        assert_eq!(bytes_to_hex(&[]), "");
        assert_eq!(bytes_to_hex(&[0x00, 0xff]), "00ff");
        assert_eq!(bytes_to_hex(&[0xde, 0xad, 0xbe, 0xef]), "deadbeef");
    }

    #[tokio::test]
    async fn generate_token_hex_returns_32_hex_chars() {
        let t = generate_token_hex(16).await.expect("generate");
        assert_eq!(t.len(), 32);
        assert!(t.chars().all(|c| c.is_ascii_hexdigit()));
    }
}

async fn doctor_closure() -> Result<()> {
    let dir = logs_dir()?;
    if !dir.exists() {
        println!("(no logs yet)");
        return Ok(());
    }
    let mut entries = tokio::fs::read_dir(&dir).await?;
    let mut total = 0;
    let mut with_task = 0;
    let mut with_turns = 0;
    let mut with_end = 0;
    let mut completed = 0;
    let mut failed = 0;
    while let Some(entry) = entries.next_entry().await? {
        let p = entry.path();
        if p.extension().and_then(|s| s.to_str()) != Some("jsonl") {
            continue;
        }
        total += 1;
        let records = Session::read_all(&p).await.unwrap_or_default();
        let mut has_task = false;
        let mut has_turn = false;
        let mut end_state: Option<String> = None;
        for r in records {
            match r {
                evo_core::session::SessionRecord::Task(_) => has_task = true,
                evo_core::session::SessionRecord::Turn(_) => has_turn = true,
                evo_core::session::SessionRecord::End(e) => end_state = Some(e.state),
            }
        }
        if has_task {
            with_task += 1;
        }
        if has_turn {
            with_turns += 1;
        }
        if let Some(s) = &end_state {
            with_end += 1;
            if s.contains("COMPLETED") {
                completed += 1;
            } else if s.contains("FAILED") {
                failed += 1;
            }
        }
    }
    println!("== evoclaw doctor closure ==");
    println!("path: {}", dir.display());
    println!("{:<28} {:>6}", "metric", "count");
    println!("{:<28} {:>6}", "session files", total);
    println!("{:<28} {:>6}", "TaskRecord present", with_task);
    println!("{:<28} {:>6}", "TurnRecord present", with_turns);
    println!("{:<28} {:>6}", "EndRecord present", with_end);
    println!("{:<28} {:>6}", "  COMPLETED end-state", completed);
    println!("{:<28} {:>6}", "  FAILED end-state", failed);
    if total > 0 && with_task == total && with_end == total {
        println!("\nclosure: OK (PRD §39 #1, #4)");
    } else {
        println!("\nclosure: WARN — some sessions missing TaskRecord or EndRecord");
    }
    Ok(())
}

async fn doctor_tokens() -> Result<()> {
    let path = cost_log_path()?;
    let engine = CostEngine::at(&path, BudgetCfg::default());
    let events = engine.read_events().await?;
    if events.is_empty() {
        println!("(no cost events recorded yet)");
        return Ok(());
    }
    let now = chrono::Utc::now();
    let day_cut = now - chrono::Duration::days(7);
    let month_cut = now - chrono::Duration::days(30);
    let mut s7 = (0u64, 0u64, 0u64, 0.0f64, 0u64);
    let mut s30 = (0u64, 0u64, 0u64, 0.0f64, 0u64);
    for ev in &events {
        if ev.ts >= day_cut {
            s7.0 += ev.input_tokens;
            s7.1 += ev.cached_tokens;
            s7.2 += ev.output_tokens;
            s7.3 += ev.usd;
            s7.4 += 1;
        }
        if ev.ts >= month_cut {
            s30.0 += ev.input_tokens;
            s30.1 += ev.cached_tokens;
            s30.2 += ev.output_tokens;
            s30.3 += ev.usd;
            s30.4 += 1;
        }
    }
    let hr = |c: u64, t: u64| -> f64 {
        if t == 0 {
            0.0
        } else {
            c as f64 / t as f64
        }
    };
    println!("== evoclaw doctor tokens ==");
    println!("path: {}", path.display());
    println!();
    println!("{:<14} {:>12} {:>12}", "metric", "7d", "30d");
    println!("{:<14} {:>12} {:>12}", "events", s7.4, s30.4);
    println!("{:<14} {:>12} {:>12}", "input_tokens", s7.0, s30.0);
    println!("{:<14} {:>12} {:>12}", "cached_tokens", s7.1, s30.1);
    println!("{:<14} {:>12} {:>12}", "output_tokens", s7.2, s30.2);
    println!(
        "{:<14} {:>11.2}% {:>11.2}%",
        "cache_hit",
        hr(s7.1, s7.0) * 100.0,
        hr(s30.1, s30.0) * 100.0
    );
    println!("{:<14} {:>11.4}$ {:>11.4}$", "usd_total", s7.3, s30.3);
    println!();
    println!(
        "budget: per_task ≤ ${:.2}, per_day ≤ ${:.2} (soft) / ${:.2} (hard), per_month ≤ ${:.0}",
        engine.cfg().per_task_usd,
        engine.cfg().per_day_soft_usd,
        engine.cfg().per_day_hard_usd,
        engine.cfg().per_month_usd
    );
    Ok(())
}

async fn logout_cmd() -> Result<()> {
    let theme = Theme::detect();
    println!();
    println!(
        "{warn}logout:{reset} clearing current authentication...",
        warn = theme.warn(),
        reset = theme.reset()
    );

    let cfg = load_config().await?;
    let auth_method = cfg.auth.parsed();

    // Clear auth based on method
    match auth_method {
        AuthMethod::ApiKey => {
            if let Some(provider_id) = &cfg.model.provider {
                let secret_path = onboard::secret_file(provider_id)?;
                if secret_path.exists() {
                    tokio::fs::remove_file(&secret_path).await.ok();
                    println!(
                        "  {ok}✓{reset} removed API key for {bold}{provider_id}{reset}",
                        ok = theme.ok(),
                        bold = theme.bold(),
                        reset = theme.reset()
                    );
                }
            }
        }
        AuthMethod::Browser => {
            if let Some(provider_id) = &cfg.model.provider {
                let profile_path = onboard::browser_profile_path(provider_id)?;
                if profile_path.exists() {
                    tokio::fs::remove_dir_all(&profile_path).await.ok();
                    println!(
                        "  {ok}✓{reset} removed browser profile for {bold}{provider_id}{reset}",
                        ok = theme.ok(),
                        bold = theme.bold(),
                        reset = theme.reset()
                    );
                }
            }
        }
        AuthMethod::Acp => {
            println!(
                "  {dim}(ACP agent-based auth — no local credentials to clear){reset}",
                dim = theme.dim(),
                reset = theme.reset()
            );
        }
    }

    println!();
    println!(
        "{frame}→{reset} Run {bold}/login{reset} to re-authenticate or Ctrl-D to exit.",
        frame = theme.frame(),
        bold = theme.bold(),
        reset = theme.reset()
    );
    Ok(())
}

async fn usage_cmd() -> Result<()> {
    // Alias for /tokens command
    doctor_tokens().await
}

async fn config_show() -> Result<()> {
    let theme = Theme::detect();
    let cfg = load_config().await?;
    let cfg_path = config_path()?;

    println!();
    println!(
        "{bold}== Configuration =={reset}",
        bold = theme.bold(),
        reset = theme.reset()
    );
    println!(
        "{frame}path:{reset} {dim}{}{reset}",
        cfg_path.display(),
        frame = theme.frame(),
        dim = theme.dim(),
        reset = theme.reset()
    );
    println!();

    println!("[model]");
    println!(
        "  provider     : {}",
        cfg.model.provider.as_deref().unwrap_or("(not set)")
    );
    println!("  default      : {}", cfg.model.default);
    println!("  base_url     : {}", cfg.model.base_url);
    if !cfg.model.fallback.is_empty() {
        println!("  fallback     : {:?}", cfg.model.fallback);
    }

    println!();
    println!("[auth]");
    println!("  method       : {}", cfg.auth.method);

    println!();
    println!("[budget]");
    println!("  per_task_usd : ${:.2}", cfg.budget.per_task_usd);
    println!("  per_day_usd  : ${:.2}", cfg.budget.per_day_usd);
    println!("  per_month_usd: ${:.0}", cfg.budget.per_month_usd);

    println!();
    println!("[security]");
    println!(
        "  default_permission  : {}",
        cfg.security.default_permission
    );
    println!(
        "  high_risk_intercept : {}",
        cfg.security.high_risk_intercept
    );

    if let Some(logs) = &cfg.logs {
        println!();
        println!("[logs]");
        if let Some(dir) = &logs.dir {
            println!("  dir          : {}", dir);
        }
    }

    println!();
    Ok(())
}

async fn config_set(key: &str, value: &str) -> Result<()> {
    let theme = Theme::detect();
    let cfg_path = config_path()?;
    let mut cfg = load_config().await?;

    // Support common configuration changes
    let updated = match key {
        "model.default" | "model" => {
            cfg.model.default = value.to_string();
            true
        }
        "model.base_url" | "base_url" => {
            cfg.model.base_url = value.to_string();
            true
        }
        "budget.per_task_usd" | "budget.per_task" => {
            match value.parse::<f64>() {
                Ok(v) => {
                    cfg.budget.per_task_usd = v;
                    true
                }
                Err(_) => {
                    println!(
                        "{err}Invalid number: {value}{reset}",
                        err = theme.err(),
                        reset = theme.reset()
                    );
                    false
                }
            }
        }
        "budget.per_day_usd" | "budget.per_day" => {
            match value.parse::<f64>() {
                Ok(v) => {
                    cfg.budget.per_day_usd = v;
                    true
                }
                Err(_) => {
                    println!(
                        "{err}Invalid number: {value}{reset}",
                        err = theme.err(),
                        reset = theme.reset()
                    );
                    false
                }
            }
        }
        "budget.per_month_usd" | "budget.per_month" => {
            match value.parse::<f64>() {
                Ok(v) => {
                    cfg.budget.per_month_usd = v;
                    true
                }
                Err(_) => {
                    println!(
                        "{err}Invalid number: {value}{reset}",
                        err = theme.err(),
                        reset = theme.reset()
                    );
                    false
                }
            }
        }
        "security.default_permission" | "default_permission" => {
            if ["ask", "allow", "deny"].contains(&value) {
                cfg.security.default_permission = value.to_string();
                true
            } else {
                println!(
                    "{err}Invalid permission value. Use: ask, allow, or deny{reset}",
                    err = theme.err(),
                    reset = theme.reset()
                );
                false
            }
        }
        "security.high_risk_intercept" | "high_risk_intercept" => {
            match value.parse::<bool>() {
                Ok(v) => {
                    cfg.security.high_risk_intercept = v;
                    true
                }
                Err(_) => {
                    println!(
                        "{err}Invalid boolean: {value}. Use: true or false{reset}",
                        err = theme.err(),
                        reset = theme.reset()
                    );
                    false
                }
            }
        }
        _ => {
            println!();
            println!(
                "{warn}Unsupported config key: {key}{reset}",
                warn = theme.warn(),
                reset = theme.reset()
            );
            println!();
            println!("Supported keys:");
            println!("  model.default         - Current model name");
            println!("  model.base_url        - API base URL");
            println!("  budget.per_task_usd   - Per-task budget limit");
            println!("  budget.per_day_usd    - Per-day budget limit");
            println!("  budget.per_month_usd  - Per-month budget limit");
            println!("  security.default_permission - ask|allow|deny");
            println!("  security.high_risk_intercept - true|false");
            println!();
            println!(
                "{dim}For other changes, edit {} manually{reset}",
                cfg_path.display(),
                dim = theme.dim(),
                reset = theme.reset()
            );
            println!();
            return Ok(());
        }
    };

    if updated {
        // Write back the config
        let toml_str = toml::to_string_pretty(&cfg)
            .wrap_err("serialize config")?;
        tokio::fs::write(&cfg_path, toml_str)
            .await
            .wrap_err("write config")?;

        println!();
        println!(
            "{ok}✓{reset} Set {bold}{key}{reset} = {value}",
            ok = theme.ok(),
            bold = theme.bold(),
            reset = theme.reset()
        );
        println!();
    }

    Ok(())
}

async fn config_reset() -> Result<()> {
    let theme = Theme::detect();
    let cfg_path = config_path()?;

    println!();
    print!(
        "{warn}Reset configuration?{reset} This will remove {} [y/N] ",
        cfg_path.display(),
        warn = theme.warn(),
        reset = theme.reset()
    );
    std::io::stdout().flush().ok();

    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;

    if line.trim().eq_ignore_ascii_case("y") {
        tokio::fs::remove_file(&cfg_path).await?;
        println!(
            "{ok}✓{reset} Configuration reset. Run {bold}evoclaw onboard{reset} to reconfigure.",
            ok = theme.ok(),
            bold = theme.bold(),
            reset = theme.reset()
        );
    } else {
        println!("{dim}(cancelled){reset}", dim = theme.dim(), reset = theme.reset());
    }

    println!();
    Ok(())
}

async fn status_cmd() -> Result<()> {
    let theme = Theme::detect();
    let cfg = load_config().await?;

    // ═══════════════════════════════════════════════════════════
    // Provider & Model Section
    // ═══════════════════════════════════════════════════════════
    println!("{}", DisplayTemplate::header(&theme, "Provider & Model"));

    // Show active profile name
    let active_profile = get_active_profile_name()
        .await
        .unwrap_or_else(|_| "default".to_string());
    println!("{}", DisplayTemplate::kv(&theme, "Active Profile", &active_profile));

    let provider_id = cfg.model.provider.as_deref().unwrap_or("unknown");
    let is_acp = provider_id.starts_with("acp:");

    // Get provider details from catalog
    let (vendor_name, is_local) = if is_acp {
        let agent_name = provider_id.strip_prefix("acp:").unwrap_or(provider_id);
        (format!("External ACP Agent: {}", agent_name), false)
    } else {
        match onboard::find_provider(provider_id) {
            Some(profile) => (
                format!("{} ({})", profile.name, if profile.local { "Local" } else { "Cloud" }),
                profile.local
            ),
            None => (format!("Custom: {}", provider_id), false),
        }
    };

    println!("{}", DisplayTemplate::kv(&theme, "Vendor", &vendor_name));
    println!("{}", DisplayTemplate::kv(&theme, "Provider ID", provider_id));

    if !cfg.model.base_url.is_empty() {
        println!(
            "{}",
            DisplayTemplate::kv_colored(&theme, "API Endpoint", &cfg.model.base_url, theme.info())
        );
    }

    println!("{}", DisplayTemplate::kv(&theme, "Model", &cfg.model.default));

    if is_local {
        println!(
            "{}",
            DisplayTemplate::kv_colored(&theme, "Type", "Local Inference", theme.accent())
        );
    }

    println!("{}", DisplayTemplate::footer(&theme));

    // ═══════════════════════════════════════════════════════════
    // Authentication Section
    // ═══════════════════════════════════════════════════════════
    println!("{}", DisplayTemplate::header(&theme, "Authentication"));

    let auth_method = cfg.auth.parsed();
    println!("{}", DisplayTemplate::kv(&theme, "Method", auth_method.as_str()));

    // Check auth status and get account info
    let (auth_ok, account_info) = match auth_method {
        AuthMethod::ApiKey => {
            let exists = onboard::secret_file(provider_id)
                .ok()
                .map(|p| p.exists())
                .unwrap_or(false);
            (exists, String::from("API Key authentication"))
        }
        AuthMethod::Browser => {
            let exists = onboard::browser_profile_path(provider_id)
                .ok()
                .map(|p| p.exists())
                .unwrap_or(false);
            let account = if exists {
                match onboard::load_browser_profile(provider_id).await {
                    Ok(p) => p.account_label.unwrap_or_else(|| String::from("Unknown account")),
                    Err(_) => String::from("Profile exists but cannot be read"),
                }
            } else {
                String::from("No browser profile found")
            };
            (exists, account)
        }
        AuthMethod::Acp => (true, format!("Managed by external agent: {}", provider_id)),
    };

    let status_text = if auth_ok { "Authenticated" } else { "Not Authenticated" };
    let status_color = if auth_ok { theme.ok() } else { theme.err() };
    println!(
        "{}",
        DisplayTemplate::kv_colored(&theme, "Status", status_text, status_color)
    );

    if matches!(auth_method, AuthMethod::Browser | AuthMethod::Acp) || !auth_ok {
        println!("{}", DisplayTemplate::kv(&theme, "Account", &account_info));
    }

    println!("{}", DisplayTemplate::footer(&theme));

    // ═══════════════════════════════════════════════════════════
    // Session & Paths Section
    // ═══════════════════════════════════════════════════════════
    println!("{}", DisplayTemplate::header(&theme, "Session & Paths"));

    if let Ok(session_log) = session_log_path() {
        println!(
            "{}",
            DisplayTemplate::kv_colored(
                &theme,
                "Session Log",
                &session_log.display().to_string(),
                theme.dim()
            )
        );
    }

    if let Ok(cfg_path) = config_path() {
        println!(
            "{}",
            DisplayTemplate::kv_colored(
                &theme,
                "Config",
                &cfg_path.display().to_string(),
                theme.dim()
            )
        );
    }

    if let Ok(ws) = workspace_dir() {
        println!(
            "{}",
            DisplayTemplate::kv_colored(
                &theme,
                "Workspace",
                &ws.display().to_string(),
                theme.dim()
            )
        );
    }

    // Vault info
    let vault_count = count_vault_entries().await;
    println!(
        "{}",
        DisplayTemplate::kv(&theme, "Vault Entries", &format!("{} secrets", vault_count))
    );

    // Skills info
    if let Ok(skill_count) = count_skills().await {
        println!(
            "{}",
            DisplayTemplate::kv(&theme, "Learned Skills", &format!("{} skills", skill_count))
        );
    }

    println!("{}", DisplayTemplate::footer(&theme));
    println!();

    Ok(())
}

async fn model_show() -> Result<()> {
    let theme = Theme::detect();
    let cfg = load_config().await?;

    println!();
    println!(
        "{bold}== Current Model =={reset}",
        bold = theme.bold(),
        reset = theme.reset()
    );
    println!();

    let provider_id = cfg
        .model
        .provider
        .as_deref()
        .unwrap_or("(unknown)");

    println!(
        "{frame}Provider:{reset}     {bold}{}{reset}",
        provider_id,
        frame = theme.frame(),
        bold = theme.bold(),
        reset = theme.reset()
    );
    println!(
        "{frame}Current model:{reset} {bold}{}{reset}",
        cfg.model.default,
        frame = theme.frame(),
        bold = theme.bold(),
        reset = theme.reset()
    );
    println!(
        "{frame}Base URL:{reset}      {}",
        cfg.model.base_url,
        frame = theme.frame(),
        reset = theme.reset()
    );

    if !cfg.model.fallback.is_empty() {
        println!(
            "{frame}Fallback:{reset}      {}",
            cfg.model.fallback.join(", "),
            frame = theme.frame(),
            reset = theme.reset()
        );
    }

    println!();
    println!(
        "{dim}Use {reset}{bold}/model list{reset}{dim} to see available models{reset}",
        dim = theme.dim(),
        bold = theme.bold(),
        reset = theme.reset()
    );
    println!(
        "{dim}Use {reset}{bold}/model set <name>{reset}{dim} to switch models{reset}",
        dim = theme.dim(),
        bold = theme.bold(),
        reset = theme.reset()
    );
    println!();

    Ok(())
}

async fn model_list() -> Result<()> {
    let theme = Theme::detect();
    let cfg = load_config().await?;

    println!();
    println!(
        "{bold}== Available Models =={reset}",
        bold = theme.bold(),
        reset = theme.reset()
    );
    println!();

    let provider_id = cfg
        .model
        .provider
        .as_deref()
        .unwrap_or("deepseek");

    // Try to find the provider profile
    if let Some(profile) = onboard::find_provider(provider_id) {
        println!(
            "{frame}Provider:{reset} {bold}{}{reset} ({})",
            profile.id,
            profile.name,
            frame = theme.frame(),
            bold = theme.bold(),
            reset = theme.reset()
        );
        println!();

        // Show default model (current)
        let is_current_default = cfg.model.default == profile.default_model;
        println!(
            "  {} {bold}{}{reset}  {dim}(default){reset}",
            if is_current_default {
                format!("{ok}●{reset}", ok = theme.ok(), reset = theme.reset())
            } else {
                format!("{dim}○{reset}", dim = theme.dim(), reset = theme.reset())
            },
            profile.default_model,
            bold = theme.bold(),
            dim = theme.dim(),
            reset = theme.reset()
        );

        // Show fallback models
        for model in profile.fallback {
            let is_current = cfg.model.default == *model;
            println!(
                "  {} {}",
                if is_current {
                    format!("{ok}●{reset}", ok = theme.ok(), reset = theme.reset())
                } else {
                    format!("{dim}○{reset}", dim = theme.dim(), reset = theme.reset())
                },
                model
            );
        }

        println!();
        println!(
            "{dim}Use {reset}{bold}/model set <name>{reset}{dim} to switch{reset}",
            dim = theme.dim(),
            bold = theme.bold(),
            reset = theme.reset()
        );
    } else {
        println!(
            "{warn}Provider '{provider_id}' not found in catalog{reset}",
            warn = theme.warn(),
            reset = theme.reset()
        );
        println!();
        println!(
            "{dim}Current model: {reset}{bold}{}{reset}",
            cfg.model.default,
            dim = theme.dim(),
            bold = theme.bold(),
            reset = theme.reset()
        );
        println!(
            "{dim}Use {reset}{bold}/login{reset}{dim} to change provider{reset}",
            dim = theme.dim(),
            bold = theme.bold(),
            reset = theme.reset()
        );
    }

    println!();
    Ok(())
}

// ---------------------------------------------------------------------------
// Profile management commands
// ---------------------------------------------------------------------------

async fn get_active_profile_name() -> Result<String> {
    let active_file = active_profile_file()?;
    if active_file.exists() {
        let name = tokio::fs::read_to_string(&active_file).await?;
        Ok(name.trim().to_string())
    } else {
        Ok("default".to_string())
    }
}

async fn set_active_profile_name(name: &str) -> Result<()> {
    let active_file = active_profile_file()?;
    if let Some(parent) = active_file.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    tokio::fs::write(&active_file, name).await?;
    Ok(())
}

async fn get_profile_path(name: &str) -> Result<PathBuf> {
    Ok(profiles_dir()?.join(format!("{}.toml", name)))
}

async fn profile_list() -> Result<()> {
    let theme = Theme::detect();
    let profiles_path = profiles_dir()?;

    println!("{}", DisplayTemplate::header(&theme, "Configuration Profiles"));

    if !profiles_path.exists() {
        println!("  {}", DisplayTemplate::kv(&theme, "Profiles", "None created yet"));
        println!("{}", DisplayTemplate::footer(&theme));
        println!();
        println!("  Tip: Use '/profile add <name>' to create a new profile");
        return Ok(());
    }

    let active = get_active_profile_name().await.unwrap_or_else(|_| "default".to_string());
    let mut profiles = Vec::new();

    let mut entries = tokio::fs::read_dir(&profiles_path).await?;
    while let Some(entry) = entries.next_entry().await? {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) == Some("toml") {
            if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                let is_active = stem == active;
                let cfg_str = tokio::fs::read_to_string(&path).await.ok();
                let description = cfg_str
                    .and_then(|s| toml::from_str::<Config>(&s).ok())
                    .and_then(|c| c.meta.description.or(c.meta.name))
                    .unwrap_or_else(|| "No description".to_string());

                profiles.push((stem.to_string(), description, is_active));
            }
        }
    }

    profiles.sort_by(|a, b| a.0.cmp(&b.0));

    for (name, desc, is_active) in profiles {
        let marker = if is_active {
            format!(" {}", theme.ok())
        } else {
            String::new()
        };
        let display_name = if is_active {
            format!("{} (active){}", name, theme.reset())
        } else {
            name.clone()
        };
        println!("  {}{}", DisplayTemplate::kv_colored(&theme, &display_name, &desc, theme.dim()), marker);
    }

    println!("{}", DisplayTemplate::footer(&theme));
    println!();
    Ok(())
}

async fn profile_show(name: Option<&str>) -> Result<()> {
    let theme = Theme::detect();
    let profile_name = match name {
        Some(n) => n.to_string(),
        None => get_active_profile_name().await?,
    };

    let profile_path = get_profile_path(&profile_name).await?;
    if !profile_path.exists() {
        println!();
        println!("{err}Profile '{profile_name}' not found{reset}",
            err = theme.err(), reset = theme.reset());
        println!();
        return Ok(());
    }

    let cfg_str = tokio::fs::read_to_string(&profile_path).await?;
    let cfg: Config = toml::from_str(&cfg_str)?;

    println!("{}", DisplayTemplate::header(&theme, &format!("Profile: {}", profile_name)));

    if let Some(desc) = &cfg.meta.description {
        println!("  {}", DisplayTemplate::kv(&theme, "Description", desc));
    }

    let provider_id = cfg.model.provider.as_deref().unwrap_or("(not set)");
    println!("  {}", DisplayTemplate::kv(&theme, "Provider", provider_id));
    println!("  {}", DisplayTemplate::kv(&theme, "Model", &cfg.model.default));
    println!("  {}", DisplayTemplate::kv(&theme, "Auth Method", cfg.auth.parsed().as_str()));
    println!("  {}", DisplayTemplate::kv(&theme, "Budget (task)", &format!("${:.2}", cfg.budget.per_task_usd)));

    println!("{}", DisplayTemplate::footer(&theme));
    println!();
    Ok(())
}

async fn profile_switch(name: &str) -> Result<()> {
    let theme = Theme::detect();
    let profile_path = get_profile_path(name).await?;

    if !profile_path.exists() {
        println!();
        println!("{err}Profile '{name}' not found{reset}",
            err = theme.err(), reset = theme.reset());
        println!();
        println!("Available profiles:");
        profile_list().await?;
        return Ok(());
    }

    // Set as active profile
    set_active_profile_name(name).await?;

    // Copy to config.toml for backward compatibility
    let config_path = config_path()?;
    tokio::fs::copy(&profile_path, &config_path).await?;

    println!();
    println!("{ok}Switched to profile '{name}'{reset}",
        ok = theme.ok(), reset = theme.reset());
    println!();
    Ok(())
}

async fn profile_add(name: &str, template: Option<&str>) -> Result<()> {
    let theme = Theme::detect();
    let profiles_path = profiles_dir()?;
    tokio::fs::create_dir_all(&profiles_path).await?;

    let profile_path = get_profile_path(name).await?;
    if profile_path.exists() {
        println!();
        println!("{warn}Profile '{name}' already exists{reset}",
            warn = theme.warn(), reset = theme.reset());
        println!();
        return Ok(());
    }

    // Create profile from template or copy current config
    let template_cfg = if let Some(tmpl) = template {
        get_profile_template(tmpl)?
    } else {
        // Copy current config as template
        load_config().await?
    };

    let mut cfg = template_cfg;
    cfg.meta.name = Some(name.to_string());
    cfg.meta.description = Some(format!("Profile: {}", name));

    let toml_str = toml::to_string_pretty(&cfg)?;
    tokio::fs::write(&profile_path, toml_str).await?;

    println!();
    println!("{ok}Created profile '{name}'{reset}",
        ok = theme.ok(), reset = theme.reset());
    println!("  Location: {dim}{}{reset}",
        profile_path.display(), dim = theme.dim(), reset = theme.reset());
    println!();
    println!("  Use '/profile switch {name}' to activate it");
    println!();
    Ok(())
}

async fn profile_remove(name: &str) -> Result<()> {
    let theme = Theme::detect();

    if name == "default" {
        println!();
        println!("{err}Cannot remove 'default' profile{reset}",
            err = theme.err(), reset = theme.reset());
        println!();
        return Ok(());
    }

    let active = get_active_profile_name().await.unwrap_or_else(|_| "default".to_string());
    if name == active {
        println!();
        println!("{err}Cannot remove active profile{reset}",
            err = theme.err(), reset = theme.reset());
        println!("  Switch to another profile first");
        println!();
        return Ok(());
    }

    let profile_path = get_profile_path(name).await?;
    if !profile_path.exists() {
        println!();
        println!("{warn}Profile '{name}' not found{reset}",
            warn = theme.warn(), reset = theme.reset());
        println!();
        return Ok(());
    }

    tokio::fs::remove_file(&profile_path).await?;

    println!();
    println!("{ok}Removed profile '{name}'{reset}",
        ok = theme.ok(), reset = theme.reset());
    println!();
    Ok(())
}

async fn profile_edit(name: Option<&str>) -> Result<()> {
    let theme = Theme::detect();
    let profile_name = match name {
        Some(n) => n.to_string(),
        None => get_active_profile_name().await?,
    };

    let profile_path = get_profile_path(&profile_name).await?;
    if !profile_path.exists() {
        println!();
        println!("{err}Profile '{profile_name}' not found{reset}",
            err = theme.err(), reset = theme.reset());
        println!();
        return Ok(());
    }

    let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vi".to_string());

    println!();
    println!("{info}Opening profile in {editor}...{reset}",
        info = theme.info(), reset = theme.reset());

    std::process::Command::new(&editor)
        .arg(&profile_path)
        .status()?;

    println!();
    println!("{ok}Profile '{profile_name}' saved{reset}",
        ok = theme.ok(), reset = theme.reset());
    println!("  Use '/profile switch {profile_name}' to reload if this is not the active profile");
    println!();
    Ok(())
}

fn get_profile_template(template: &str) -> Result<Config> {
    // Provide common templates
    let (provider, model, description) = match template {
        "deepseek" => ("deepseek", "deepseek-chat", "DeepSeek Chat configuration"),
        "openai" => ("openai", "gpt-4o", "OpenAI GPT-4o configuration"),
        "claude" | "anthropic" => ("anthropic", "claude-3-5-sonnet-20241022", "Claude 3.5 Sonnet configuration"),
        "gemini" | "google" => ("google", "gemini-2.0-flash-exp", "Google Gemini configuration"),
        "ollama" => ("ollama", "llama3.2", "Ollama local model configuration"),
        _ => return Err(eyre::eyre!("Unknown template: {}", template)),
    };

    Ok(Config {
        meta: ProfileMeta {
            name: Some(template.to_string()),
            description: Some(description.to_string()),
        },
        model: ModelCfg {
            provider: Some(provider.to_string()),
            default: model.to_string(),
            base_url: String::new(),
            fallback: Vec::new(),
        },
        auth: AuthCfg::default(),
        budget: ConfigBudget {
            per_task_usd: 0.5,
            per_day_usd: 5.0,
            per_month_usd: 100.0,
        },
        security: SecurityCfg {
            default_permission: "P1".to_string(),
            high_risk_intercept: true,
        },
        logs: None,
    })
}

async fn model_set(model_name: &str) -> Result<()> {
    let theme = Theme::detect();
    let cfg_path = config_path()?;
    let mut cfg = load_config().await?;

    let provider_id = cfg
        .model
        .provider
        .as_deref()
        .unwrap_or("deepseek");

    // Validate the model exists for this provider
    let profile = match onboard::find_provider(provider_id) {
        Some(p) => p,
        None => {
            println!();
            println!(
                "{warn}Warning:{reset} Provider '{provider_id}' not found in catalog",
                warn = theme.warn(),
                reset = theme.reset()
            );
            println!(
                "{dim}  Allowing model change anyway (custom provider?){reset}",
                dim = theme.dim(),
                reset = theme.reset()
            );
            println!();
            // Allow setting any model for custom providers
            cfg.model.default = model_name.to_string();
            let toml_str = toml::to_string_pretty(&cfg)
                .wrap_err("serialize config")?;
            tokio::fs::write(&cfg_path, toml_str)
                .await
                .wrap_err("write config")?;
            println!(
                "{ok}✓{reset} Set model to: {bold}{model_name}{reset}",
                ok = theme.ok(),
                bold = theme.bold(),
                reset = theme.reset()
            );
            println!();
            return Ok(());
        }
    };

    let valid_models: Vec<&str> = std::iter::once(profile.default_model)
        .chain(profile.fallback.iter().copied())
        .collect();

    if !valid_models.contains(&model_name) {
        println!();
        println!(
            "{err}Model '{model_name}' not available for provider '{provider_id}'{reset}",
            err = theme.err(),
            reset = theme.reset()
        );
        println!();
        println!("Available models:");
        for m in valid_models {
            println!("  - {}", m);
        }
        println!();
        return Ok(());
    }

    // Update the config
    cfg.model.default = model_name.to_string();

    // Write back the config
    let toml_str = toml::to_string_pretty(&cfg)
        .wrap_err("serialize config")?;
    tokio::fs::write(&cfg_path, toml_str)
        .await
        .wrap_err("write config")?;

    println!();
    println!(
        "{ok}✓{reset} Switched to model: {bold}{model_name}{reset}",
        ok = theme.ok(),
        bold = theme.bold(),
        reset = theme.reset()
    );
    println!(
        "{dim}  Provider will reload on next interaction{reset}",
        dim = theme.dim(),
        reset = theme.reset()
    );
    println!();

    Ok(())
}

async fn skill_list() -> Result<()> {
    let dir = skills_dir()?;
    if !dir.exists() {
        println!("(no skills yet — run a task first)");
        return Ok(());
    }
    let mut entries = tokio::fs::read_dir(&dir).await?;
    println!(
        "{:<24} {:<10} {:>5} {:<8} NAME",
        "ID", "STATE", "SCORE", "VER"
    );
    while let Some(entry) = entries.next_entry().await? {
        let p = entry.path();
        if p.extension().and_then(|s| s.to_str()) != Some("yaml") {
            continue;
        }
        match Skill::load_yaml(&p).await {
            Ok(sk) => println!(
                "{:<24} {:<10} {:>5.2} v{:<7} {}",
                sk.id,
                format!("{:?}", sk.state).to_uppercase(),
                sk.score,
                sk.version,
                sk.name
            ),
            Err(e) => println!("ERR {}: {e}", p.display()),
        }
    }
    Ok(())
}

async fn skill_show(id: &str) -> Result<()> {
    let path = skills_dir()?.join(format!("{id}.yaml"));
    let content = tokio::fs::read_to_string(&path)
        .await
        .wrap_err_with(|| format!("read {}", path.display()))?;
    println!("{content}");
    Ok(())
}

async fn skill_tree() -> Result<()> {
    let dir = skills_dir()?;
    let tree = SkillTree::rebuild_from_dir(&dir).await?;
    let index_path = SkillTree::default_index_path(&dir);
    tree.save(&index_path).await?;
    println!("{}", tree.render_tree());
    println!("(index: {})", index_path.display());
    Ok(())
}

async fn memory_search(query: &str, limit: usize) -> Result<()> {
    let mem = Memory::at(memory_dir()?);
    let hits = mem
        .search(
            query,
            &[MemoryLayer::L1, MemoryLayer::L2, MemoryLayer::L3],
            limit,
        )
        .await?;
    if hits.is_empty() {
        println!("(no matches for '{query}')");
        return Ok(());
    }
    for r in hits {
        println!(
            "[{:?}] {} (conf={:.2}, src={})\n  {}",
            r.layer, r.id, r.confidence, r.source, r.content
        );
    }
    Ok(())
}

// --- ACP agents ----------------------------------------------------------

fn agent_catalog() {
    println!("== Available external agents ==");
    println!("  [native] = speaks Zed Agent Client Protocol out of the box");
    println!(
        "  [shim]   = does NOT speak Zed ACP; needs a custom shim or use a different provider"
    );
    println!();
    for p in evo_acp_client::catalog() {
        let badge = if p.acp_native { "[native]" } else { "[shim]  " };
        println!("  {} {:<10} {}", badge, p.id, p.name);
        println!("    install: {}", p.install_hint);
        println!("    auth   : {}", p.auth_hint);
        println!("    note   : {}", p.notes);
    }
    let paths = evo_acp_client::registry_paths();
    println!();
    println!("add one with: evoclaw agent add <id>");
    if let Some(p) = paths.user_full {
        println!(
            "customise:    write {} (full override) or drop *.json into {} (per-id patches)",
            p.display(),
            paths
                .user_patch_dir
                .map(|d| d.display().to_string())
                .unwrap_or_default(),
        );
    }
}

async fn agent_list() -> Result<()> {
    let agents = evo_acp_client::list_agents()
        .await
        .map_err(|e| eyre::eyre!("{e:#}"))?;
    if agents.is_empty() {
        println!("(no agents configured — try `evoclaw agent catalog`)");
        return Ok(());
    }
    println!("== Configured ACP agents ==");
    for a in agents {
        let badge = match evo_acp_client::find_agent(&a.id) {
            Some(p) if p.acp_native => "[native]",
            Some(_) => "[shim]  ",
            None => "[custom]",
        };
        println!(
            "  {} {:<10} bin={} args={:?}",
            badge, a.id, a.command, a.args
        );
    }
    Ok(())
}

async fn agent_add(id: &str) -> Result<()> {
    let prof = evo_acp_client::find_agent(id).ok_or_else(|| {
        eyre::eyre!("unknown agent '{id}' — run `evoclaw agent catalog` to see options")
    })?;
    let cfg = evo_acp_client::AgentConfig::from_profile(prof);
    let path = evo_acp_client::save_agent(&cfg)
        .await
        .map_err(|e| eyre::eyre!("{e:#}"))?;
    println!("✓ added agent '{id}' → {}", path.display());
    println!("  install : {}", prof.install_hint);
    println!("  auth    : {}", prof.auth_hint);
    println!("  test it : evoclaw agent test {id}");
    Ok(())
}

async fn agent_remove(id: &str) -> Result<()> {
    evo_acp_client::remove_agent(id)
        .await
        .map_err(|e| eyre::eyre!("{e:#}"))?;
    println!("removed agent '{id}'");
    Ok(())
}

async fn agent_test(id: &str) -> Result<()> {
    let cfg = evo_acp_client::load_agent(id)
        .await
        .map_err(|e| eyre::eyre!("{e:#}; did you `evoclaw agent add {id}` first?"))?;
    println!("→ spawning '{}' ({} {:?})", cfg.id, cfg.command, cfg.args);
    let client = evo_acp_client::AcpClient::new();
    client.spawn(&cfg).await.map_err(|e| {
        eyre::eyre!(
            "{e}; install with: {}",
            evo_acp_client::find_agent(id)
                .map(|p| p.install_hint.as_str())
                .unwrap_or("see catalog")
        )
    })?;
    let result = client
        .initialize("evoclaw", env!("CARGO_PKG_VERSION"))
        .await
        .map_err(|e| eyre::eyre!("initialize failed: {e}"))?;
    println!("✓ initialize OK");
    if let Some(info) = result.get("serverInfo") {
        println!("  serverInfo: {}", info);
    }
    client.shutdown().await.ok();
    Ok(())
}

// --- MCP servers ---------------------------------------------------------

fn mcp_catalog() {
    println!("== Available MCP servers ==");
    for p in evo_mcp_client::catalog() {
        println!("  {:<14} {}", p.id, p.name);
        println!("    desc   : {}", p.description);
        println!("    install: {}", p.install_hint);
        if !p.auth_env.is_empty() {
            println!("    env    : {}", p.auth_env.join(", "));
        }
    }
    let paths = evo_mcp_client::registry_paths();
    println!();
    println!("add one with: evoclaw mcp add <id>");
    if let Some(p) = paths.user_full {
        println!(
            "customise:    write {} (full override) or drop *.json into {} (per-id patches)",
            p.display(),
            paths
                .user_patch_dir
                .map(|d| d.display().to_string())
                .unwrap_or_default(),
        );
    }
}

async fn mcp_list() -> Result<()> {
    let servers = evo_mcp_client::list_servers()
        .await
        .map_err(|e| eyre::eyre!("{e:#}"))?;
    if servers.is_empty() {
        println!("(no MCP servers — try `evoclaw mcp catalog`)");
        return Ok(());
    }
    println!("== Configured MCP servers ==");
    for s in servers {
        println!("  {:<14} cmd={} args={:?}", s.id, s.command, s.args);
    }
    Ok(())
}

async fn mcp_add(id: &str) -> Result<()> {
    let prof = evo_mcp_client::find_server(id)
        .ok_or_else(|| eyre::eyre!("unknown server '{id}' — run `evoclaw mcp catalog`"))?;
    let mut cfg = evo_mcp_client::ServerConfig::from_profile(prof);
    if !prof.auth_env.is_empty() {
        for var in &prof.auth_env {
            if let Ok(v) = std::env::var(var) {
                cfg.env.push((var.clone(), v));
            }
        }
    }
    let path = evo_mcp_client::save_server(&cfg)
        .await
        .map_err(|e| eyre::eyre!("{e:#}"))?;
    println!("✓ added MCP server '{id}' → {}", path.display());
    println!("  install: {}", prof.install_hint);
    if !prof.auth_env.is_empty() {
        let captured: Vec<&String> = cfg.env.iter().map(|(k, _)| k).collect();
        let missing: Vec<&String> = prof
            .auth_env
            .iter()
            .filter(|v| !captured.iter().any(|c| c.as_str() == v.as_str()))
            .collect();
        if !missing.is_empty() {
            println!("  ⚠ missing env vars: {:?}", missing);
            println!("  set them and re-run `evoclaw mcp add {id}` to capture");
        }
    }
    println!("  test it: evoclaw mcp test {id}");
    Ok(())
}

async fn mcp_remove(id: &str) -> Result<()> {
    evo_mcp_client::remove_server(id)
        .await
        .map_err(|e| eyre::eyre!("{e:#}"))?;
    println!("removed MCP server '{id}'");
    Ok(())
}

async fn mcp_test(id: &str) -> Result<()> {
    let cfg = evo_mcp_client::load_server(id)
        .await
        .map_err(|e| eyre::eyre!("{e:#}; did you `evoclaw mcp add {id}` first?"))?;
    println!("→ spawning '{}' ({} {:?})", cfg.id, cfg.command, cfg.args);
    let client = evo_mcp_client::McpClient::new();
    client.spawn(&cfg).await.map_err(|e| eyre::eyre!("{e}"))?;
    let _ = client
        .initialize("evoclaw", env!("CARGO_PKG_VERSION"))
        .await
        .map_err(|e| eyre::eyre!("initialize failed: {e}"))?;
    println!("✓ initialize OK");
    let tools = client
        .list_tools()
        .await
        .map_err(|e| eyre::eyre!("tools/list failed: {e}"))?;
    println!("  exposed tools: {}", tools.len());
    for t in tools.iter().take(20) {
        println!(
            "    - {} : {}",
            t.name,
            t.description.lines().next().unwrap_or("")
        );
    }
    client.shutdown().await.ok();
    Ok(())
}

// --- Secret vault --------------------------------------------------------

async fn read_secret_from_stdin() -> Result<String> {
    print!("  paste value (input is echoed; clear scrollback after): ");
    std::io::stdout().flush().ok();
    let mut buf = String::new();
    std::io::stdin().read_line(&mut buf)?;
    let v = buf.trim().to_string();
    if v.is_empty() {
        return Err(eyre::eyre!("empty value — aborted"));
    }
    Ok(v)
}

async fn secret_add(name: &str, from_stdin: bool, value: Option<String>) -> Result<()> {
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err(eyre::eyre!("name must be [A-Za-z0-9_-]+"));
    }
    let raw = match (from_stdin, value) {
        (true, _) => read_secret_from_stdin().await?,
        (false, Some(v)) => v,
        (false, None) => return Err(eyre::eyre!("either pass a value or use --stdin")),
    };
    let path = vault_path()?;
    let mut vault = Vault::load(&path).await.unwrap_or_default();
    vault.upsert(name, &raw);
    vault.save(&path).await?;
    let entry = vault
        .get(name)
        .ok_or_else(|| eyre::eyre!("upsert vanished"))?;
    println!(
        "✓ stored '{name}' (kind={}, fingerprint={}) at {}",
        entry.kind,
        entry.fingerprint,
        path.display()
    );
    println!("  the model will never see the raw value — only ${{SECRET:{name}}}");
    Ok(())
}

async fn secret_list() -> Result<()> {
    let vault = Vault::load(&vault_path()?).await.unwrap_or_default();
    if vault.entries.is_empty() {
        println!("(vault is empty — try `evoclaw secret add NAME --stdin`)");
        return Ok(());
    }
    println!("{:<24} {:<14} {:<10} CREATED", "NAME", "KIND", "FINGER");
    for e in vault.list() {
        println!(
            "{:<24} {:<14} {:<10} {}",
            e.name,
            e.kind,
            e.fingerprint,
            e.created_at.format("%Y-%m-%d %H:%M")
        );
    }
    println!(
        "\n(values are stored at {} — chmod 600)",
        vault_path()?.display()
    );
    Ok(())
}

async fn secret_remove(name: &str) -> Result<()> {
    let path = vault_path()?;
    let mut vault = Vault::load(&path).await.unwrap_or_default();
    if !vault.remove(name) {
        return Err(eyre::eyre!("no such secret: {name}"));
    }
    vault.save(&path).await?;
    println!("removed '{name}'");
    Ok(())
}

async fn secret_test(input: &str) -> Result<()> {
    let vault = Vault::load(&vault_path()?).await.unwrap_or_default();
    let r = Redactor::from_vault(&vault);
    let (out, hits) = r.scrub(input);
    println!("input  : {input}");
    println!("output : {out}");
    println!("hits   : {hits} substitution(s)");
    Ok(())
}

async fn most_recent_session() -> Result<PathBuf> {
    let dir = logs_dir()?;
    let mut entries = tokio::fs::read_dir(&dir)
        .await
        .wrap_err_with(|| format!("read {}", dir.display()))?;
    let mut newest: Option<(std::time::SystemTime, PathBuf)> = None;
    while let Some(entry) = entries.next_entry().await? {
        let p = entry.path();
        if p.extension().and_then(|s| s.to_str()) != Some("jsonl") {
            continue;
        }
        let meta = entry.metadata().await?;
        let mtime = meta.modified()?;
        if newest.as_ref().map(|(t, _)| mtime > *t).unwrap_or(true) {
            newest = Some((mtime, p));
        }
    }
    newest
        .map(|(_, p)| p)
        .ok_or_else(|| eyre::eyre!("no JSONL sessions in {}", dir.display()))
}

// ---------------------------------------------------------------------------
// Channel adapters (v0.6 scaffolding — see docs/channels.md)
// ---------------------------------------------------------------------------

async fn channel_handler(sub: ChannelCmd) -> Result<()> {
    match sub {
        ChannelCmd::List => channel_list().await,
        ChannelCmd::Run { kind } => channel_run(&kind).await,
    }
}

fn channels_dir() -> Result<PathBuf> {
    Ok(evoclaw_dir()?.join("channels"))
}

async fn channel_list() -> Result<()> {
    println!("== EvoClaw channel adapters ==");
    println!();
    println!("built-in:");
    println!(
        "  {:<14} stdin/stdout JSON (reference adapter)",
        "local-pipe"
    );
    println!();

    let dir = channels_dir()?;
    let external = if dir.exists() {
        let mut entries = tokio::fs::read_dir(&dir).await?;
        let mut found = Vec::new();
        while let Some(entry) = entries.next_entry().await? {
            let p = entry.path();
            if p.extension().and_then(|s| s.to_str()) == Some("toml") {
                if let Some(stem) = p.file_stem().and_then(|s| s.to_str()) {
                    found.push((stem.to_string(), p));
                }
            }
        }
        found.sort();
        found
    } else {
        Vec::new()
    };

    if external.is_empty() {
        println!("external (~/.evoclaw/channels/*.toml): (none yet)");
        println!();
        println!("v0.6 plan: telegram, slack, discord — see docs/channels.md");
    } else {
        println!("external (~/.evoclaw/channels/*.toml):");
        for (name, path) in external {
            println!("  {:<14} {}", name, path.display());
        }
    }
    Ok(())
}

async fn channel_run(kind: &str) -> Result<()> {
    use evo_core::channel::{OutboundKind, OutboundMessage};
    use evo_core::channel_router::{self, ChannelRouter};
    use evo_core::local_pipe::LocalPipe;

    if kind != "local-pipe" {
        return Err(eyre::eyre!(
            "unknown adapter '{kind}'. Built-in adapters: local-pipe. \
             Telegram/Slack/Discord ship in v0.6 — see docs/channels.md."
        ));
    }

    ensure_layout().await?;
    tokio::fs::create_dir_all(channels_dir()?).await.ok();

    let adapter = Arc::new(LocalPipe);
    let mut router = ChannelRouter::new();
    router.register(adapter.clone());

    let (inbound_tx, mut inbound_rx) = tokio::sync::mpsc::channel(64);

    // Adapters run in the background. We keep a separate `Arc<LocalPipe>`
    // for outbound replies so the dispatch loop doesn't have to round-trip
    // through `router.send_via`.
    let router_handle = tokio::spawn(router.run_all(inbound_tx));

    eprintln!(
        "→ channel: local-pipe adapter ready. Send line-delimited \
         InboundMessage JSON on stdin; replies stream to stdout."
    );

    while let Some(msg) = inbound_rx.recv().await {
        if !channel_router::should_handle(&msg) {
            tracing::debug!(
                conversation_id = %msg.conversation_id,
                "channel: skipping un-mentioned message"
            );
            continue;
        }
        let conv_id = msg.conversation_id.clone();
        let reply = match channel_run_one_shot_text(&msg.text).await {
            Ok(text) => OutboundMessage {
                conversation_id: conv_id,
                text,
                kind: OutboundKind::Reply,
            },
            Err(e) => OutboundMessage {
                conversation_id: conv_id,
                text: format!("[error] {e:#}"),
                kind: OutboundKind::Error,
            },
        };
        if let Err(e) = adapter.send(reply).await {
            tracing::warn!(error=?e, "channel: failed to send reply");
        }
    }

    let _ = router_handle.await;
    Ok(())
}

/// Thin wrapper around the conversation runtime that returns the final
/// text instead of printing it. Used by the channel dispatch loop so the
/// reply travels through the adapter rather than stdout-as-CLI.
async fn channel_run_one_shot_text(input: &str) -> Result<String> {
    let cfg = load_config().await?;
    ensure_layout().await?;
    let provider_id = cfg
        .model
        .provider
        .clone()
        .unwrap_or_else(|| "deepseek".into());
    let provider: Arc<dyn Provider> = if let Some(agent_id) = provider_id.strip_prefix("acp:") {
        let p = AcpProvider::spawn(agent_id)
            .await
            .map_err(|e| eyre::eyre!("{e:#}"))?;
        Arc::new(p)
    } else {
        match cfg.auth.parsed() {
            AuthMethod::Browser => {
                let profile = onboard::load_browser_profile(&provider_id).await.wrap_err_with(
                    || {
                        format!(
                            "load browser profile for '{provider_id}'. \
                             Run `evoclaw login` and pick (2) Browser sign-in."
                        )
                    },
                )?;
                Arc::new(BrowserProvider::from_profile(&profile)) as Arc<dyn Provider>
            }
            AuthMethod::Acp => {
                return Err(eyre::eyre!(
                    "config.toml has [auth].method = \"acp\" but provider is not set to an ACP agent. \
                     Run `evoclaw login` and select 'External ACP agent' from the provider list."
                ));
            }
            AuthMethod::ApiKey => {
                let (api_key, _src) = onboard::resolve_api_key(&provider_id).await?;
                match provider_id.as_str() {
                    "anthropic" => Arc::new(AnthropicProvider::new(
                        api_key,
                        cfg.model.default.clone(),
                    )) as Arc<dyn Provider>,
                    "copilot" => Arc::new(CopilotProvider::new(
                        api_key,
                        cfg.model.default.clone(),
                    )),
                    _ => Arc::new(OpenAiCompatProvider::new(
                        cfg.model.base_url.clone(),
                        api_key,
                        cfg.model.default.clone(),
                    )),
                }
            }
        }
    };
    let mut registry = ToolRegistry::with_builtins();
    let _attached = mcp_tools::install_all(&mut registry).await;
    let registry = Arc::new(registry);
    let task_id = format!("task-{}", chrono::Utc::now().format("%Y%m%dT%H%M%S%.3f"));
    let log_path = logs_dir()?.join(format!("{task_id}.jsonl"));
    let session = Session::open(&log_path).await?;
    let tool_ctx = ToolContext {
        workspace: workspace_dir()?,
        // Channel-driven runs are non-interactive — never block on a tty
        // prompt for permission upgrades.
        allow_user_prompt: false,
        ..Default::default()
    };
    let cost_engine = Arc::new(CostEngine::at(cost_log_path()?, BudgetCfg::default()));
    let memory = Memory::at(memory_dir()?);
    let vault = Vault::load(&vault_path()?).await.unwrap_or_default();
    let redactor = Redactor::from_vault(&vault);
    let mut runtime = ConversationRuntime::new(
        provider,
        registry,
        session,
        tool_ctx,
        evo_core::runtime::RuntimeConfig {
            model: cfg.model.default.clone(),
            provider_id: cfg.model.provider.clone(),
            mcp_servers: get_active_mcp_servers().await.unwrap_or_default(),
            ..Default::default()
        },
    )
    .with_cost_engine(cost_engine)
    .with_memory(memory)
    .with_skills_dir(skills_dir()?)
    .with_redactor(redactor);
    let outcome = runtime.run(input).await?;
    Ok(outcome.final_text)
}

#[cfg(test)]
mod ui_tests {
    use super::*;

    #[test]
    fn test_terminal_width_detection() {
        // Should return a reasonable width (100 default or from COLUMNS env)
        let width = TerminalUI::width();
        assert!(width >= 40, "Terminal width should be at least 40 columns");
        assert!(width <= 500, "Terminal width should be reasonable (<= 500)");
    }

    #[test]
    fn test_thin_separator() {
        let theme = Theme::detect();
        let separator = TerminalUI::thin_separator(&theme);

        // Should contain box drawing characters
        assert!(separator.contains("─"), "Separator should contain horizontal line character");

        // Should not be empty
        assert!(!separator.is_empty(), "Separator should not be empty");
    }

    #[test]
    fn test_format_status() {
        let theme = Theme::detect();

        // Test basic status with model and provider
        let status = TerminalUI::format_status(
            &theme,
            "gpt-4",
            "openai",
            None,
            0,
            None,
        );
        assert!(status.contains("gpt-4"), "Status should contain model name");
        assert!(status.contains("openai"), "Status should contain provider name");

        // Test with ACP
        let status_with_acp = TerminalUI::format_status(
            &theme,
            "claude-3",
            "anthropic",
            Some("claude-desktop"),
            0,
            None,
        );
        assert!(status_with_acp.contains("claude-3"), "Status should contain model");
        assert!(status_with_acp.contains("acp"), "Status should mention ACP");
        assert!(status_with_acp.contains("claude-desktop"), "Status should contain ACP name");

        // Test with MCP servers
        let status_with_mcp = TerminalUI::format_status(
            &theme,
            "gpt-4",
            "openai",
            None,
            3,
            None,
        );
        assert!(status_with_mcp.contains("mcp"), "Status should mention MCP");
        assert!(status_with_mcp.contains("3"), "Status should show MCP count");

        // Test with active skill
        let status_with_skill = TerminalUI::format_status(
            &theme,
            "gpt-4",
            "openai",
            None,
            0,
            Some("code-review"),
        );
        assert!(status_with_skill.contains("skill"), "Status should mention skill");
        assert!(status_with_skill.contains("code-review"), "Status should show skill name");
    }

    #[test]
    fn test_chat_box_top() {
        let theme = Theme::detect();

        let header = TerminalUI::chat_box_top(&theme);

        // Should contain separator
        assert!(header.contains("─"), "Should contain separator line");

        // Should have newlines
        assert!(header.contains('\n'), "Should be multi-line");

        // Should start and end with newline for proper spacing
        assert!(header.starts_with('\n'), "Should start with newline");
        assert!(header.ends_with('\n'), "Should end with newline");
    }

    #[test]
    fn test_chat_box_bottom() {
        let theme = Theme::detect();

        let footer = TerminalUI::chat_box_bottom(
            &theme,
            "gpt-4-turbo",
            "openai",
            Some("copilot"),
            3,
            Some("refactor"),
        );

        // Should contain separator
        assert!(footer.contains("─"), "Should contain separator line");

        // Should contain all status information
        assert!(footer.contains("gpt-4-turbo"), "Should show model name");
        assert!(footer.contains("openai"), "Should show provider");
        assert!(footer.contains("copilot"), "Should show ACP status");
        assert!(footer.contains("3"), "Should show MCP count");
        assert!(footer.contains("refactor"), "Should show skill name");

        // Should have multiple lines
        assert!(footer.contains('\n'), "Should be multi-line");
    }

    #[test]
    fn test_width_with_columns_env() {
        // Test that COLUMNS environment variable is respected
        std::env::set_var("COLUMNS", "120");
        let width = TerminalUI::width();
        assert_eq!(width, 120, "Should use COLUMNS env variable");

        // Clean up
        std::env::remove_var("COLUMNS");
    }
}
