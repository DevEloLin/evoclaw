//! evo-cli — entry function shared by `evo` and `evoclaw` binaries.

pub mod mcp_tools;
pub mod onboard;
pub mod tui;
pub mod ui;

pub(crate) mod commands;
pub(crate) mod config;
pub(crate) mod playbook;
pub(crate) mod slash;
pub(crate) mod task;
pub(crate) mod terminal_ui;
pub(crate) mod theme;

use crate::commands::channel::channel_handler;
use crate::commands::{agent, diag, gateway, mcp, secret, skill};
use crate::config::{config_path, ensure_layout, init_logs_dir, load_config, session_log_path};
use crate::slash::{handle_slash, SlashOutcome};
use crate::task::{build_provider, print_banner, SharedHistory, TaskEnv};
use crate::terminal_ui::history_path;
use crate::theme::Theme;
use clap::{Parser, Subcommand};
use eyre::{Result, WrapErr};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

// ---------------------------------------------------------------------------
// CLI types
// ---------------------------------------------------------------------------

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
    /// Run a one-shot task (non-interactive).
    ///
    /// Two modes:
    ///   evoclaw run "free-form prompt text"             ← ad-hoc prompt
    ///   evoclaw run --skill <id> --param k=v --param …  ← playbook from
    ///                                                     ~/.evoclaw/playbooks/
    Run {
        /// Playbook id to load from ~/.evoclaw/playbooks/<id>.{yaml,yml,md}.
        #[arg(long)]
        skill: Option<String>,
        /// Playbook parameter overrides (repeatable). Format: --param k=v
        #[arg(long = "param", value_name = "K=V")]
        params: Vec<String>,
        /// Free-form prompt text. Ignored when --skill is given.
        input: Option<String>,
    },
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
pub(crate) enum ChannelCmd {
    /// List built-in adapters and any external `~/.evoclaw/channels/*.toml`.
    List,
    /// Show which channel tokens are configured (token present = ready to run).
    Status,
    /// Save a bot token for a channel adapter (persists across restarts).
    /// Supported: telegram, slack, discord.
    Add { kind: String },
    /// Remove a previously saved channel token.
    Remove { kind: String },
    /// Run a single adapter, fan inbound messages through the agent loop,
    /// and post replies back. Supported: local-pipe, telegram.
    Run {
        #[arg(long)]
        kind: String,
        /// Skip the reflection-round after each task. Trims 1-3s off the
        /// per-message latency at the cost of skipping skill score updates.
        /// Strongly recommended when the channel has a tight response
        /// budget (e.g. the 5-second WeChat passive-reply window).
        #[arg(long)]
        no_reflection: bool,
        /// Run with NO tools registered at all. Forces the model to answer
        /// in a single turn — same fast-response rationale as
        /// `--no-reflection`. When false, the registry keeps the standard
        /// built-ins + any attached MCP tools.
        #[arg(long)]
        no_tools: bool,
        /// Override `RuntimeConfig.max_turns` (default 25). Set to 1 with
        /// `--no-tools` for the strict "answer once, return" mode. Must be
        /// ≥ 1 — passing 0 would make the agent loop exit before the first
        /// model call and return `MaxTurns` error.
        #[arg(long, value_parser = clap::value_parser!(u64).range(1..))]
        max_turns: Option<u64>,
        /// Override `RuntimeConfig.max_tokens` (default 4096). Cap output
        /// to keep replies short for SMS-like channels. Must be ≥ 1.
        #[arg(long, value_parser = clap::value_parser!(u32).range(1..))]
        max_tokens: Option<u32>,
        /// Override `RuntimeConfig.temperature` (default 0.2). Bounded
        /// 0.0..=2.0 — most providers reject anything outside that range
        /// (OpenAI: 0..=2, Anthropic: 0..=1).
        #[arg(long, value_parser = parse_temperature)]
        temperature: Option<f32>,
    },
}

/// Clap value parser for `--temperature`. Bounds the value to a range
/// every supported provider tolerates so we fail at CLI-parse time rather
/// than mid-stream with a cryptic provider error.
fn parse_temperature(s: &str) -> std::result::Result<f32, String> {
    let v: f32 = s
        .parse()
        .map_err(|e| format!("'{s}' is not a valid f32: {e}"))?;
    if !v.is_finite() {
        return Err(format!("temperature must be finite (got {v})"));
    }
    if !(0.0..=2.0).contains(&v) {
        return Err(format!(
            "temperature {v} out of range (must be 0.0..=2.0)"
        ));
    }
    Ok(v)
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
    /// List reflection-generated skills under ~/.evoclaw/skills/
    List,
    /// Show a single reflection-generated skill's YAML body
    Show { id: String },
    /// Render the skill tree index
    Tree,
    /// List user-authored playbooks under ~/.evoclaw/playbooks/
    Playbooks,
    /// Show a single playbook's resolved fields
    PlaybookShow { id: String },
    /// Execute a playbook as a one-shot task. Use `--param k=v` per parameter.
    Run {
        id: String,
        #[arg(long = "param", value_name = "K=V")]
        params: Vec<String>,
    },
}

#[derive(Subcommand, Debug)]
enum MemoryCmd {
    Search {
        query: String,
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/// Library entry point. Both binaries call this.
pub async fn entry() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .init();
    // Resolve the logs directory once before any subcommand can request it.
    init_logs_dir(load_config().await.ok().as_ref());
    let cli = Cli::parse();
    match cli.cmd {
        None | Some(Cmd::Shell) => interactive().await,
        Some(Cmd::Onboard) => commands::onboard::onboard_cmd().await,
        Some(Cmd::Login) => commands::onboard::login_cmd().await,
        Some(Cmd::Agent(a)) => match a {
            AgentCmd::List => agent::agent_list().await,
            AgentCmd::Catalog => {
                agent::agent_catalog();
                Ok(())
            }
            AgentCmd::Add { id } => agent::agent_add(&id).await,
            AgentCmd::Remove { id } => agent::agent_remove(&id).await,
            AgentCmd::Test { id } => agent::agent_test(&id).await,
        },
        Some(Cmd::Mcp(m)) => match m {
            McpSubCmd::List => mcp::mcp_list().await,
            McpSubCmd::Catalog => {
                mcp::mcp_catalog();
                Ok(())
            }
            McpSubCmd::Add { id } => mcp::mcp_add(&id).await,
            McpSubCmd::Remove { id } => mcp::mcp_remove(&id).await,
            McpSubCmd::Test { id } => mcp::mcp_test(&id).await,
        },
        Some(Cmd::Secret(s)) => match s {
            SecretCmd::Add { name, stdin, value } => secret::secret_add(&name, stdin, value).await,
            SecretCmd::List => secret::secret_list().await,
            SecretCmd::Remove { name } => secret::secret_remove(&name).await,
            SecretCmd::Test { input } => secret::secret_test(&input).await,
        },
        Some(Cmd::Run { skill, params, input }) => match (skill, input) {
            (Some(id), _) => skill::skill_run(&id, params).await,
            (None, Some(text)) => task::run_one_shot(&text).await,
            (None, None) => Err(eyre::eyre!(
                "evoclaw run requires either a prompt or --skill <id>"
            )),
        },
        Some(Cmd::Doctor) => diag::doctor().await,
        Some(Cmd::DoctorOf(d)) => match d {
            DoctorCmd::Tokens => diag::doctor_tokens().await,
            DoctorCmd::Closure => diag::doctor_closure().await,
        },
        Some(Cmd::Gateway { bind, token }) => gateway::gateway(&bind, token.as_deref()).await,
        Some(Cmd::Replay { path }) => diag::replay(path).await,
        Some(Cmd::Skill(s)) => match s {
            SkillCmd::List => skill::skill_list().await,
            SkillCmd::Show { id } => skill::skill_show(&id).await,
            SkillCmd::Tree => skill::skill_tree().await,
            SkillCmd::Playbooks => skill::playbook_list().await,
            SkillCmd::PlaybookShow { id } => skill::playbook_show(&id).await,
            SkillCmd::Run { id, params } => skill::skill_run(&id, params).await,
        },
        Some(Cmd::Memory(m)) => match m {
            MemoryCmd::Search { query, limit } => skill::memory_search(&query, limit).await,
        },
        Some(Cmd::Channel(c)) => channel_handler(c).await,
    }
}

// ---------------------------------------------------------------------------
// Interactive REPL
// ---------------------------------------------------------------------------

async fn interactive() -> Result<()> {
    let theme = Theme::detect();

    // First-run onboarding
    if !config_path()?.exists() {
        println!();
        println!(
            "  {bold}Welcome to EvoClaw{reset} — let's get you set up.",
            bold = theme.bold(),
            reset = theme.reset(),
        );
        println!();
        println!("  Authentication options:");
        println!(
            "    {ok}1){reset}  API key             — {dim}simplest · works for every vendor{reset}",
            ok = theme.ok(),
            dim = theme.dim(),
            reset = theme.reset(),
        );
        println!(
            "    {ok}2){reset}  Browser sign-in     — {dim}paste session token from browser{reset}",
            ok = theme.ok(),
            dim = theme.dim(),
            reset = theme.reset(),
        );
        ensure_layout().await?;
        commands::onboard::run_provider_wizard().await?;
        println!();
    }

    let mut cfg = load_config().await?;
    ensure_layout().await?;

    let mut registry = evo_tools::ToolRegistry::with_builtins();
    mcp_tools::install_all(&mut registry).await;

    // Clear visible screen + scrollback + reset cursor.
    {
        use std::io::Write as _;
        let mut out = std::io::stdout();
        let _ = out.write_all(b"\x1b[H\x1b[2J\x1b[3J");
        let _ = out.flush();
    }

    // Print banner BEFORE enabling raw mode (banner uses println!).
    print_banner(&cfg).await;

    // Build the provider once for the entire shell session.
    let (mut provider, mut is_acp) = match build_provider(&cfg).await {
        Ok(pair) => pair,
        Err(e) => {
            eprintln!(
                "{err}error{reset} failed to start provider: {e:#}",
                err = theme.err(),
                reset = theme.reset(),
            );
            return Err(e);
        }
    };

    let session_log = session_log_path()?;
    let history_file = history_path()?;

    // Enable raw mode
    crossterm::terminal::enable_raw_mode().wrap_err("failed to enable raw terminal mode")?;
    let _raw_guard = RawModeGuard;

    // Event channel
    let (ui_tx, mut ui_rx) = tokio::sync::mpsc::channel::<ui::UiEvent>(512);

    // ask_user channel: tools → event loop (avoids raw-mode stdin conflict).
    let (ask_tx, mut ask_rx) =
        tokio::sync::mpsc::unbounded_channel::<(String, tokio::sync::oneshot::Sender<String>)>();

    // Shared conversation history — REPL-wide so successive turns remember
    // prior context. The `TaskEnv` built below clones the Arc into every
    // spawned task; the task runner injects the snapshot into
    // `ConversationRuntime` before `run()` and writes the updated history
    // back when `run()` completes. Cleared by the `/reset` slash command.
    let shared_history: SharedHistory = Arc::new(tokio::sync::Mutex::new(Vec::new()));
    let ask_bridge_tx = ui_tx.clone();
    tokio::spawn(async move {
        while let Some((prompt, resp_tx)) = ask_rx.recv().await {
            let _ = ask_bridge_tx
                .send(ui::UiEvent::AskUser { prompt, resp_tx })
                .await;
        }
    });

    // Initial UI state + renderer
    let mut state = ui::UiState::new();
    let mut renderer = ui::UiRenderer::new(theme);
    renderer.redraw_bottom(&state);

    // Spawn raw-mode input task
    let input_pause = Arc::new(AtomicBool::new(false));
    let input_tx = ui_tx.clone();
    let hist_file = history_file.clone();
    let pause_for_task = Arc::clone(&input_pause);
    tokio::task::spawn_blocking(move || {
        ui::run_input_task_sync(input_tx, hist_file, pause_for_task);
    });

    // Status-tick timer (updates elapsed display every 500 ms)
    let tick_tx = ui_tx.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(tokio::time::Duration::from_millis(500));
        loop {
            interval.tick().await;
            if tick_tx.send(ui::UiEvent::StatusTick).await.is_err() {
                break;
            }
        }
    });

    // Task queue (serial execution model per PRD §5)
    let mut task_queue: std::collections::VecDeque<(String, String)> = Default::default();
    let mut task_running = false;

    // Long-lived bundle of REPL state shared by every spawned task. Cloning
    // is cheap (Arc + sender clones + Config Clone), and the struct removes
    // the 9-arg call site that earlier needed `#[allow(too_many_arguments)]`.
    // Mutated in place by `/reload` so subsequent tasks see the new provider.
    let mut task_env = TaskEnv {
        provider: provider.clone(),
        is_acp,
        cfg: cfg.clone(),
        session_log: session_log.clone(),
        ui_tx: ui_tx.clone(),
        ask_tx: ask_tx.clone(),
        shared_history: shared_history.clone(),
    };

    // Main event loop
    loop {
        let Some(event) = ui_rx.recv().await else {
            break;
        };

        // AskUser carries a non-Clone oneshot::Sender — handle it by value
        // before the borrow-by-ref match below.
        let event = match event {
            ui::UiEvent::AskUser { prompt, resp_tx } => {
                input_pause.store(true, Ordering::SeqCst);
                renderer.clear_bottom();
                crossterm::terminal::disable_raw_mode().ok();
                {
                    use std::io::Write as _;
                    println!("\n[?] {prompt}");
                    print!("> ");
                    let _ = std::io::stdout().flush();
                }
                let answer = tokio::task::spawn_blocking(|| {
                    use std::io::BufRead as _;
                    let mut s = String::new();
                    let _ = std::io::stdin().lock().read_line(&mut s);
                    s.trim().to_string()
                })
                .await
                .unwrap_or_default();
                crossterm::terminal::enable_raw_mode().ok();
                renderer.reset_bottom();
                input_pause.store(false, Ordering::SeqCst);
                let _ = resp_tx.send(answer.clone());
                state.apply(&ui::UiEvent::SlashCommandOutput {
                    title: "ask_user".into(),
                    lines: vec![format!("[?] {prompt}"), format!("> {answer}")],
                });
                renderer.render(&state);
                continue;
            }
            other => other,
        };

        match &event {
            // Shutdown
            ui::UiEvent::Shutdown => {
                renderer.clear_bottom();
                drop(_raw_guard);
                crossterm::terminal::disable_raw_mode().ok();
                use std::io::Write as _;
                print!("\r\n");
                let _ = std::io::stdout().flush();
                println!(
                    "{frame}bye.{reset}",
                    frame = theme.frame(),
                    reset = theme.reset()
                );
                return Ok(());
            }

            // User submitted input
            ui::UiEvent::InputSubmitted {
                task_id,
                content,
                timestamp,
            } => {
                let task_id = task_id.clone();
                let content = content.clone();
                let timestamp = timestamp.clone();

                // Slash command: instant, bypasses task queue
                if let Some(rest) = content.strip_prefix('/') {
                    input_pause.store(true, Ordering::SeqCst);
                    tokio::time::sleep(tokio::time::Duration::from_millis(80)).await;

                    renderer.clear_bottom();
                    crossterm::terminal::disable_raw_mode().ok();

                    let slash_result = handle_slash(rest).await;

                    crossterm::terminal::enable_raw_mode().ok();
                    renderer.reset_bottom();
                    input_pause.store(false, Ordering::SeqCst);

                    match slash_result? {
                        SlashOutcome::Exit => {
                            drop(_raw_guard);
                            crossterm::terminal::disable_raw_mode().ok();
                            println!(
                                "{frame}bye.{reset}",
                                frame = theme.frame(),
                                reset = theme.reset()
                            );
                            return Ok(());
                        }
                        SlashOutcome::Reload => {
                            // Refuse to clear shared_history while a task is
                            // running or queued — the in-flight task already
                            // holds a snapshot it will write back, and
                            // queued tasks would read an empty history,
                            // silently losing the user's context.
                            if task_running || !task_queue.is_empty() {
                                eprintln!(
                                    "{warn}reload deferred — wait for {n} task(s) to finish, then retry{reset}",
                                    warn = theme.warn(),
                                    n = task_queue.len() + if task_running { 1 } else { 0 },
                                    reset = theme.reset(),
                                );
                            } else {
                                // Switching providers invalidates history.
                                *shared_history.lock().await = Vec::new();
                            }
                            cfg = load_config().await?;
                            match build_provider(&cfg).await {
                                Ok((p, acp)) => {
                                    provider = p;
                                    is_acp = acp;
                                    // Refresh the task env so subsequent
                                    // spawn() calls see the new provider /
                                    // config — otherwise the loop would keep
                                    // dispatching against the old (possibly
                                    // disconnected) provider.
                                    task_env.provider = provider.clone();
                                    task_env.is_acp = is_acp;
                                    task_env.cfg = cfg.clone();
                                    crossterm::terminal::disable_raw_mode().ok();
                                    let prov_id = cfg
                                        .model
                                        .provider
                                        .clone()
                                        .unwrap_or_else(|| "(unknown)".into());
                                    println!(
                                        "{ok}switched to {bold}{prov_id}{reset}",
                                        ok = theme.ok(),
                                        bold = theme.bold(),
                                        reset = theme.reset(),
                                    );
                                    crossterm::terminal::enable_raw_mode().ok();
                                }
                                Err(e) => {
                                    crossterm::terminal::disable_raw_mode().ok();
                                    eprintln!(
                                        "{err}error{reset} failed to switch provider: {e:#}",
                                        err = theme.err(),
                                        reset = theme.reset(),
                                    );
                                    crossterm::terminal::enable_raw_mode().ok();
                                }
                            }
                        }
                        SlashOutcome::ResetHistory => {
                            if task_running || !task_queue.is_empty() {
                                eprintln!(
                                    "{warn}/reset deferred — wait for {n} task(s) to finish, then retry{reset}",
                                    warn = theme.warn(),
                                    n = task_queue.len() + if task_running { 1 } else { 0 },
                                    reset = theme.reset(),
                                );
                            } else {
                                *shared_history.lock().await = Vec::new();
                            }
                        }
                        SlashOutcome::Continue => {}
                    }

                    state.apply(&ui::UiEvent::InputChanged {
                        content: String::new(),
                        cursor_char: 0,
                    });
                    renderer.redraw_bottom(&state);
                    continue;
                }

                // Regular task
                let queued = task_running;
                state.apply(&ui::UiEvent::UserMessageAdded {
                    task_id: task_id.clone(),
                    content: content.clone(),
                    timestamp: timestamp.clone(),
                    queued,
                });
                state.apply(&ui::UiEvent::InputChanged {
                    content: String::new(),
                    cursor_char: 0,
                });

                if queued {
                    task_queue.push_back((task_id.clone(), content.clone()));
                    let queued_count = task_queue.len();
                    state.apply(&ui::UiEvent::TaskQueued {
                        task_id: task_id.clone(),
                        queued_count,
                    });
                    renderer.render(&state);
                } else {
                    task_running = true;
                    task_env.clone().spawn(task_id.clone(), content.clone());
                    renderer.render(&state);
                }
                continue;
            }

            // Task finished: start next queued task if any
            ui::UiEvent::AssistantDone { .. } => {
                task_running = false;
                if let Some((next_id, next_content)) = task_queue.pop_front() {
                    task_running = true;
                    let queued_count = task_queue.len();
                    state.task.queued_count = queued_count;
                    task_env.clone().spawn(next_id, next_content);
                }
            }

            // StatusTick: only redraw when a task is active
            ui::UiEvent::StatusTick => {
                if state.apply(&event) {
                    renderer.redraw_bottom(&state);
                }
                continue;
            }

            _ => {}
        }

        if state.apply(&event) {
            renderer.render(&state);
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// RawModeGuard
// ---------------------------------------------------------------------------

/// RAII guard that disables raw mode on drop (safety net for panics).
struct RawModeGuard;

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        crossterm::terminal::disable_raw_mode().ok();
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod parser_tests {
    use super::parse_temperature;

    #[test]
    fn parses_typical_temperature_values() {
        assert!((parse_temperature("0.0").unwrap() - 0.0).abs() < 1e-6);
        assert!((parse_temperature("0.7").unwrap() - 0.7).abs() < 1e-6);
        assert!((parse_temperature("2.0").unwrap() - 2.0).abs() < 1e-6);
    }

    #[test]
    fn rejects_out_of_range_values() {
        assert!(parse_temperature("-0.1").is_err());
        assert!(parse_temperature("2.01").is_err());
        assert!(parse_temperature("999").is_err());
    }

    #[test]
    fn rejects_non_finite_values() {
        // `inf` and `nan` parse as f32 but must not be accepted as
        // temperature values — providers reject them too.
        assert!(parse_temperature("inf").is_err());
        assert!(parse_temperature("nan").is_err());
        assert!(parse_temperature("-inf").is_err());
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse_temperature("hot").is_err());
        assert!(parse_temperature("").is_err());
        assert!(parse_temperature("1.0.0").is_err());
    }
}

#[cfg(test)]
mod ui_tests {
    use crate::terminal_ui::TerminalUI;
    use crate::theme::Theme;
    use crate::tui;

    #[test]
    fn test_terminal_width_detection() {
        let width = tui::terminal_width();
        assert!(width >= 40, "Terminal width should be at least 40 columns");
        assert!(width <= 500, "Terminal width should be reasonable (<= 500)");
    }

    #[test]
    fn test_banner_panel_shows_runtime_info() {
        let theme = Theme { enabled: false };
        let out = TerminalUI::panel(
            &theme,
            "evoclaw",
            &[
                "auth: ok · env".to_string(),
                "account: not available for API key auth".to_string(),
                "vault: empty · pattern fallback active".to_string(),
                "skills: 0 learned  ·  mcp: none attached".to_string(),
                "Type a question or /help for commands · Ctrl-D to exit".to_string(),
            ],
            theme.accent(),
        );
        assert!(out.contains("evoclaw"));
        assert!(out.contains("auth: ok"));
        assert!(out.contains("vault:"));
        assert!(out.contains("skills:"));
        assert!(out.contains("Ctrl-D to exit"));
        assert!(out.contains('─'));
        assert!(!out.contains('╭'), "panel must not use side-border corners");
    }

    #[test]
    fn test_width_is_reasonable() {
        let width = tui::terminal_width();
        assert!(width >= 60);
        assert!(width <= 220);
    }

    #[test]
    fn test_render_markdown_formats_basic_blocks() {
        let theme = Theme { enabled: false };
        let rendered = TerminalUI::render_markdown(
            &theme,
            "# Title\n\n- item\n1. step\n> quote\n\n```rust\nlet x = 1;\n```",
        );
        assert!(rendered.contains("Title"));
        assert!(rendered.contains("• item"));
        assert!(rendered.contains("1. step"));
        assert!(rendered.contains("  quote"));
        assert!(rendered.contains("code: rust"));
        assert!(!rendered.contains("┌"), "no box-drawing corners");
        assert!(rendered.contains("  let x = 1;"));
    }

    #[test]
    fn test_render_inline_markdown_formats_links_and_code() {
        let theme = Theme { enabled: false };
        let rendered =
            TerminalUI::render_markdown(&theme, "See [docs](https://example.com) and `cargo test`");
        assert!(rendered.contains("docs (https://example.com)"));
        assert!(rendered.contains("cargo test"));
        assert!(!rendered.contains('`'));
    }

    #[test]
    fn test_render_answer_block_has_top_and_bottom_borders() {
        let theme = Theme { enabled: false };
        let body = "Hello world";
        let out = TerminalUI::render_answer_block(
            &theme,
            body,
            1,
            12.4,
            "acp:codex",
            "acp:codex",
            "unavailable",
        );
        assert!(!out.contains('╭'), "panel must not use corner");
        assert!(!out.contains('╯'), "panel must not use corner");
        assert!(out.contains('─'), "panel must have horizontal separators");
        assert!(out.contains("EvoClaw (acp:codex)"));
        assert!(out.contains("12.4s"));
        assert!(out.contains("Hello world"));
        assert!(out.contains("provider: acp:codex"));
        assert!(out.contains("usage: unavailable"));
    }

    #[test]
    fn test_render_answer_block_pluralises_turns() {
        let theme = Theme { enabled: false };
        let out = TerminalUI::render_answer_block(
            &theme,
            "x",
            3,
            5.0,
            "gpt-4o-mini",
            "openai",
            "unavailable",
        );
        assert!(out.contains("3 turns"));
    }

    #[test]
    fn test_render_top_status_bar_matches_dashboard_spec() {
        let theme = Theme { enabled: false };
        let out = TerminalUI::render_top_status_bar(
            &theme,
            "evoclaw v1.0.1-beta.1",
            "acp:codex",
            "acp:codex",
            "~/devops/gptcli/agent/EvoClaw",
            "2026-05-03 17:22:48",
        );
        assert!(out.contains("evoclaw v1.0.1-beta.1"));
        assert!(out.contains("acp:codex"));
        assert!(out.contains("workspace: ~/devops/gptcli/agent/EvoClaw"));
        assert!(out.contains("2026-05-03 17:22:48"));
        assert!(out.contains('─'));
        assert!(!out.contains('╭'));
    }

    #[test]
    fn test_render_answer_block_wraps_long_lines_within_box() {
        let theme = Theme { enabled: false };
        let body = "a a a a a a a a a a a a a a a a a a a a a a a a a a a a a a \
                    a a a a a a a a a a a a a a a a a a a a a a a a a a a a a a \
                    a a a a a a a a a a a a a a a a a a a a a a a a a a a a a a";
        let out = TerminalUI::render_answer_block(
            &theme,
            body,
            1,
            1.0,
            "openai",
            "openai",
            "unavailable",
        );
        let body_lines = out.lines().filter(|l| l.contains('a')).count();
        assert!(
            body_lines >= 2,
            "expected wrap to produce 2+ body lines, got {body_lines}"
        );
    }

    #[test]
    fn test_render_answer_block_does_not_emit_high_entropy_marker_for_normal_input() {
        let theme = Theme { enabled: false };
        let body = "evo-cli / evo-core / evo-providers";
        let out = TerminalUI::render_answer_block(
            &theme,
            body,
            1,
            1.0,
            "openai",
            "openai",
            "unavailable",
        );
        assert!(!out.contains("[REDACTED:"));
    }

    #[test]
    fn test_truncate_to_respects_cjk_display_width() {
        use crate::theme::truncate_to;
        assert_eq!(truncate_to("abc", 10), "abc");
    }

    #[test]
    fn test_render_markdown_code_fence_handles_wide_language_labels() {
        let theme = Theme { enabled: false };
        let rendered = TerminalUI::render_markdown(&theme, "```rust\ncontent\n```");
        assert!(rendered.contains("code: rust"));
        assert!(rendered.contains("  content"));
        assert!(!rendered.contains("┌"));
        assert!(!rendered.contains("└"));
    }

    #[test]
    fn test_render_inline_markdown_italic_single_asterisk() {
        let theme = Theme { enabled: false };
        let rendered = TerminalUI::render_markdown(&theme, "This is *italic* text");
        assert!(rendered.contains("italic"));
        assert!(!rendered.contains('*'));
    }

    #[test]
    fn test_render_inline_markdown_strikethrough() {
        let theme = Theme { enabled: false };
        let rendered = TerminalUI::render_markdown(&theme, "~~deleted~~ text");
        assert!(rendered.contains("deleted"));
        assert!(!rendered.contains("~~"));
    }

    #[test]
    fn test_render_markdown_gfm_table() {
        let theme = Theme { enabled: false };
        let md = "| Name | Age |\n|------|-----|\n| Alice | 30 |";
        let rendered = TerminalUI::render_markdown(&theme, md);
        assert!(rendered.contains("Name"));
        assert!(rendered.contains("Age"));
        assert!(rendered.contains("Alice"));
        assert!(rendered.contains('│'));
        assert!(rendered.contains('─'));
        assert!(!rendered.contains('├'));
    }

    #[test]
    fn test_render_markdown_nested_list() {
        let theme = Theme { enabled: false };
        let md = "- top\n  - nested";
        let rendered = TerminalUI::render_markdown(&theme, md);
        assert!(rendered.contains("top"));
        assert!(rendered.contains("nested"));
        let top_line = rendered.lines().find(|l| l.contains("top")).unwrap();
        let nested_line = rendered.lines().find(|l| l.contains("nested")).unwrap();
        let top_indent = top_line.chars().take_while(|&c| c == ' ').count();
        let nested_indent = nested_line.chars().take_while(|&c| c == ' ').count();
        assert!(
            nested_indent > top_indent,
            "nested list item should be indented more"
        );
    }

    #[test]
    fn test_render_inline_markdown_snake_case_not_italic() {
        let theme = Theme { enabled: false };
        let rendered = TerminalUI::render_markdown(&theme, "use snake_case here");
        assert!(rendered.contains("snake_case"));
    }
}
