//! evo-cli — entry function shared by `evo` and `evoclaw` binaries.

pub mod mcp_tools;
pub mod onboard;

use clap::{Parser, Subcommand};
use directories::BaseDirs;
use evo_core::{ConversationRuntime, Memory, MemoryLayer, Session, Skill, SkillTree};
use evo_policy::{default_vault_path, BudgetCfg, CostEngine, Redactor, Vault};
use evo_providers::{
    AcpProvider, AnthropicProvider, CopilotProvider, OpenAiCompatProvider, Provider,
};
use evo_tools::{ToolContext, ToolRegistry};
use eyre::{Result, WrapErr};
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::PathBuf;
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
        #[arg(long, default_value = "dev")]
        token: String,
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
    model: ModelCfg,
    budget: ConfigBudget,
    security: SecurityCfg,
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
fn config_path() -> Result<PathBuf> {
    Ok(evoclaw_dir()?.join("config.toml"))
}
fn workspace_dir() -> Result<PathBuf> {
    Ok(evoclaw_dir()?.join("workspace"))
}
fn logs_dir() -> Result<PathBuf> {
    Ok(evoclaw_dir()?.join("logs"))
}

async fn ensure_layout() -> Result<()> {
    for sub in [
        "workspace",
        "logs",
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
        Some(Cmd::Gateway { bind, token }) => gateway(&bind, &token).await,
        Some(Cmd::Replay { path }) => replay(path).await,
        Some(Cmd::Skill(s)) => match s {
            SkillCmd::List => skill_list().await,
            SkillCmd::Show { id } => skill_show(&id).await,
            SkillCmd::Tree => skill_tree().await,
        },
        Some(Cmd::Memory(m)) => match m {
            MemoryCmd::Search { query, limit } => memory_search(&query, limit).await,
        },
    }
}

// ---------------------------------------------------------------------------
// Interactive REPL
// ---------------------------------------------------------------------------

const VERSION: &str = env!("CARGO_PKG_VERSION");

async fn interactive() -> Result<()> {
    if !config_path()?.exists() {
        println!();
        println!("  Welcome to EvoClaw — let's get you set up.");
        ensure_layout().await?;
        run_provider_wizard().await?;
        println!();
    }
    let cfg = load_config().await?;
    ensure_layout().await?;
    print_banner(&cfg).await;

    loop {
        std::io::stdout().flush().ok();
        print!("\nevoclaw> ");
        std::io::stdout().flush().ok();
        let mut line = String::new();
        let n = std::io::stdin().read_line(&mut line)?;
        if n == 0 {
            println!("\nbye.");
            return Ok(());
        }
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(rest) = line.strip_prefix('/') {
            if handle_slash(rest).await? {
                return Ok(());
            }
            continue;
        }
        if let Err(e) = run_one_shot(line).await {
            eprintln!("[error] {e:#}");
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
    let (key_ok, key_status) = if is_acp {
        (true, "managed by external agent".into())
    } else {
        match onboard::resolve_api_key(&provider_id).await {
            Ok((_k, src)) => (true, format!("ok · {}", short_key_source(&src.describe()))),
            Err(_) => (false, "MISSING — run /login".into()),
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
    print_row(bold, dim, reset, "api key ", &key_value);
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
    println!("   {dim}/help for slash commands  ·  /exit or Ctrl-D to quit.{reset}");
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

async fn handle_slash(rest: &str) -> Result<bool> {
    let mut parts = rest.split_whitespace();
    let cmd = parts.next().unwrap_or("");
    let args: Vec<&str> = parts.collect();
    match cmd {
        "exit" | "quit" | "q" => {
            println!("bye.");
            return Ok(true);
        }
        "help" | "?" => print_help(),
        "login" => login_cmd().await?,
        "agent" => match args.as_slice() {
            [] | ["list"] => agent_list().await?,
            ["catalog"] => agent_catalog(),
            ["add", id] => agent_add(id).await?,
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
        other => println!("unknown command: /{other}  (try /help)"),
    }
    Ok(false)
}

fn print_help() {
    println!();
    println!("slash commands:");
    println!("  /help                show this help");
    println!("  /login               switch provider / re-enter API key");
    println!("  /agent [sub]         ACP external agents (claude/codex/cursor/copilot)");
    println!("  /mcp   [sub]         MCP servers (filesystem/github/fetch/...)");
    println!("  /secret [sub]        local-only key vault (values never reach the model)");
    println!("  /skill list          list every skill on disk");
    println!("  /skill tree          rebuild and print skill tree");
    println!("  /skill show <id>     dump one skill's YAML");
    println!("  /memory <query>      grep memory L1/L2/L3");
    println!("  /tokens              7-day / 30-day cost & cache stats");
    println!("  /closure             session JSONL audit (PRD §39)");
    println!("  /replay [path]       pretty-print a session (latest by default)");
    println!("  /doctor              health check");
    println!("  /clear               clear screen");
    println!("  /exit  /quit  /q     exit (also Ctrl-D)");
    println!();
    println!("anything else is treated as a task and runs through the agent loop.");
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
    let key_opt = onboard::ask_api_key(&choice).await?;
    if let Some(ref key) = key_opt {
        let path = onboard::save_secret(&choice.id, key).await?;
        println!("  saved key    -> {}", path.display());
    }
    // After login, let the user pick a specific model from the provider's
    // /models endpoint (or accept the catalog default). Best-effort — any
    // error keeps the default and prints one line.
    onboard::pick_model(&mut choice, key_opt.as_deref()).await?;
    let cfg_path = onboard::save_config(&choice).await?;
    println!("  saved config -> {}", cfg_path.display());
    Ok(())
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
    match onboard::resolve_api_key(&provider_id).await {
        Ok((_k, src)) => println!("api_key  : OK ({})", src.describe()),
        Err(e) => println!("api_key  : MISSING — {e:#}\nrun `evoclaw login`"),
    }
    Ok(())
}

async fn run_one_shot(input: &str) -> Result<()> {
    let cfg = load_config().await?;
    ensure_layout().await?;
    let provider_id = cfg
        .model
        .provider
        .clone()
        .unwrap_or_else(|| "deepseek".into());
    let provider: Arc<dyn Provider> = if let Some(agent_id) = provider_id.strip_prefix("acp:") {
        // ACP path: spawn external CLI; auth handled by the agent itself.
        let p = AcpProvider::spawn(agent_id)
            .await
            .map_err(|e| eyre::eyre!("{e:#}"))?;
        Arc::new(p)
    } else {
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
    };
    let mut registry = ToolRegistry::with_builtins();
    let attached_servers = mcp_tools::install_all(&mut registry).await;
    if attached_servers > 0 {
        println!(
            "→ MCP: {attached_servers} server(s) attached, registry now has {} tools",
            registry.names().len()
        );
    }
    let registry = Arc::new(registry);
    let task_id = format!("task-{}", chrono::Utc::now().format("%Y%m%dT%H%M%S%.3f"));
    let log_path = logs_dir()?.join(format!("{task_id}.jsonl"));
    let session = Session::open(&log_path).await?;
    let tool_ctx = ToolContext {
        workspace: workspace_dir()?,
        allow_user_prompt: true,
        ..Default::default()
    };
    let cost_engine = Arc::new(CostEngine::at(cost_log_path()?, BudgetCfg::default()));
    let memory = Memory::at(memory_dir()?);
    // PRD §13.4 — bootstrap secret-redaction barrier from the on-disk vault.
    // No file = empty vault = pattern-only fallback (still scrubs sk-*, ghp_*,
    // eyJ*, AKIA*, high-entropy tokens).
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
            ..Default::default()
        },
    )
    .with_cost_engine(cost_engine)
    .with_memory(memory)
    .with_skills_dir(skills_dir()?)
    .with_redactor(redactor);
    let started = std::time::Instant::now();
    println!("→ running…  log: {}", log_path.display());
    let outcome = runtime.run(input).await?;
    let elapsed = started.elapsed();
    println!(
        "\n=== final ({} turns, {:.1}s) ===\n{}",
        outcome.turns,
        elapsed.as_secs_f32(),
        outcome.final_text
    );
    Ok(())
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

async fn gateway(bind: &str, token: &str) -> Result<()> {
    use std::process::Stdio;
    let mut cmd = tokio::process::Command::new("evo-gateway");
    cmd.env("EVO_GATEWAY_BIND", bind)
        .env("EVO_GATEWAY_ALLOWLIST", token)
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    let mut child = cmd.spawn().map_err(|e| {
        eyre::eyre!(
            "evo-gateway binary not found on PATH: {e}. Build with `cargo build -p evo-gateway`."
        )
    })?;
    println!("→ evo-gateway started, bound to {bind} (token: {token})");
    println!("  WebChat: http://{bind}");
    let status = child.wait().await?;
    if !status.success() {
        return Err(eyre::eyre!("evo-gateway exited: {status}"));
    }
    Ok(())
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
    println!("== Available external agents (Zed-style ACP) ==");
    for p in evo_acp_client::CATALOG {
        println!("  {:<10} {}", p.id, p.name);
        println!("    install: {}", p.install_hint);
        println!("    auth   : {}", p.auth_hint);
    }
    println!("\nadd one with: evoclaw agent add <id>");
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
        println!("  {:<10} bin={} args={:?}", a.id, a.command, a.args);
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
                .map(|p| p.install_hint)
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
    for p in evo_mcp_client::CATALOG {
        println!("  {:<14} {}", p.id, p.name);
        println!("    desc   : {}", p.description);
        println!("    install: {}", p.install_hint);
        if !p.auth_env.is_empty() {
            println!("    env    : {}", p.auth_env.join(", "));
        }
    }
    println!("\nadd one with: evoclaw mcp add <id>");
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
        for var in prof.auth_env {
            if let Ok(v) = std::env::var(var) {
                cfg.env.push((var.to_string(), v));
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
        let missing: Vec<&&str> = prof
            .auth_env
            .iter()
            .filter(|v| !captured.iter().any(|c| c.as_str() == **v))
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
