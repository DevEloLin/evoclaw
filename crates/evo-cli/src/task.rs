//! Provider construction, task spawning, and one-shot execution.

use crate::config::{
    cost_log_path, ensure_layout, evoclaw_dir, load_config, logs_dir, memory_dir, policy_path,
    skills_dir, vault_path, workspace_dir, Config,
};
use crate::slash::get_active_mcp_servers;
use crate::terminal_ui::{Spinner, TerminalUI};
use crate::theme::{short_key_source, Theme};
use crate::{mcp_tools, onboard, ui};
use evo_core::{ConversationRuntime, Memory, Session};
use evo_policy::{BudgetCfg, CostEngine, PolicyConfig, Redactor, Vault};
use evo_providers::{
    AcpProvider, AnthropicProvider, AuthMethod, AzureProvider, BrowserProvider, CopilotProvider,
    Message, OpenAiCompatProvider, Provider,
};
use evo_tools::{ToolContext, ToolRegistry};
use eyre::{Result, WrapErr};
use std::path::Path;
use std::sync::Arc;

const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Conversation history shared across REPL turns. The interactive event loop
/// owns one instance, the `TaskEnv` clones the Arc into every spawned task,
/// and the task runner copies it into the `ConversationRuntime` before
/// `run()` so prior turns are visible to the model. After `run()` the
/// updated history is written back. Cleared by the `/reset` slash command.
pub(crate) type SharedHistory = Arc<tokio::sync::Mutex<Vec<Message>>>;

// ---------------------------------------------------------------------------
// Provider construction
// ---------------------------------------------------------------------------

/// Build the provider once. ACP-backed providers spawn a child process and
/// run a JSON-RPC handshake — both are slow (npx fetch + SDK initialize),
/// so the interactive shell builds the provider ONCE and reuses it across
/// every `evoclaw>` turn (see `interactive()`). The boolean flag is `true`
/// when this is an ACP backend → caller passes it as
/// `RuntimeConfig.reflection_enabled = false` so the upstream agent isn't
/// asked to write a meta-reflection on every short interaction.
pub(crate) async fn build_provider(cfg: &Config) -> Result<(Arc<dyn Provider>, bool)> {
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
                "azure" => Arc::new(AzureProvider::new(
                    cfg.model.base_url.clone(),
                    api_key,
                    cfg.model.default.clone(),
                    None,
                )),
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

// ---------------------------------------------------------------------------
// Banner
// ---------------------------------------------------------------------------

pub(crate) async fn print_banner(cfg: &Config) {
    let theme = Theme::detect();
    let provider_id = cfg
        .model
        .provider
        .clone()
        .unwrap_or_else(|| "deepseek".into());
    let is_acp = provider_id.starts_with("acp:");
    let model_label = if is_acp {
        provider_id.clone()
    } else {
        cfg.model.default.clone()
    };
    let workspace = crate::config::evoclaw_dir()
        .map(|p| crate::theme::display_home(&p.display().to_string()))
        .unwrap_or_else(|_| "~/.evoclaw".into());

    let (auth_ok, auth_note) = if is_acp {
        (true, String::new())
    } else {
        match cfg.auth.parsed() {
            AuthMethod::Browser => match onboard::load_browser_profile(&provider_id).await {
                Ok(_) => (true, String::new()),
                Err(_) => (false, "run /login".into()),
            },
            AuthMethod::Acp => (true, String::new()),
            AuthMethod::ApiKey => match onboard::resolve_api_key(&provider_id).await {
                Ok((_k, src)) => (true, short_key_source(&src.describe())),
                Err(_) => (false, "run /login".into()),
            },
        }
    };

    print!(
        "{}",
        TerminalUI::render_welcome(
            &theme,
            VERSION,
            &provider_id,
            &model_label,
            &workspace,
            auth_ok,
            &auth_note,
        )
    );
}

// ---------------------------------------------------------------------------
// Task environment
// ---------------------------------------------------------------------------

/// Bundle of long-lived REPL state required to spawn or run a task. The
/// interactive event loop constructs one `TaskEnv` after building the provider
/// and clones it (cheap: every field is an `Arc`, a `Sender`, or a small
/// `Config`) into each task it dispatches.
#[derive(Clone)]
pub(crate) struct TaskEnv {
    pub provider: Arc<dyn Provider>,
    pub is_acp: bool,
    pub cfg: Config,
    pub session_log: std::path::PathBuf,
    pub ui_tx: tokio::sync::mpsc::Sender<ui::UiEvent>,
    pub ask_tx: tokio::sync::mpsc::UnboundedSender<(String, tokio::sync::oneshot::Sender<String>)>,
    pub shared_history: SharedHistory,
}

impl TaskEnv {
    /// Spawn a task that runs the AI provider and forwards streaming deltas
    /// and completion as `UiEvent`s. The task runs on the tokio thread pool;
    /// the caller's event loop is never blocked.
    pub(crate) fn spawn(self, task_id: String, content: String) {
        let ts = chrono::Local::now().format("%H:%M:%S").to_string();
        let provider_label = self
            .cfg
            .model
            .provider
            .clone()
            .unwrap_or_else(|| "default".into());

        // Announce task start immediately (PRD §2: user message → task panel).
        let _ = self.ui_tx.try_send(ui::UiEvent::AssistantStarted {
            task_id: task_id.clone(),
            provider: provider_label.clone(),
            timestamp: ts,
        });

        tokio::spawn(async move {
            let started = std::time::Instant::now();

            // Create a delta forwarding channel.
            let (delta_raw_tx, mut delta_raw_rx) =
                tokio::sync::mpsc::unbounded_channel::<String>();
            let fwd_task_id = task_id.clone();
            let fwd_tx = self.ui_tx.clone();
            let fwd_handle = tokio::spawn(async move {
                while let Some(delta) = delta_raw_rx.recv().await {
                    let _ = fwd_tx
                        .send(ui::UiEvent::AssistantDelta {
                            task_id: fwd_task_id.clone(),
                            delta,
                        })
                        .await;
                }
            });

            let result = self.run_interactive(&content, delta_raw_tx).await;

            // Wait for the forwarding task to drain all buffered deltas before
            // sending AssistantDone. Without this, AssistantDone can race ahead
            // of the final delta batch, leaving the streaming block empty or
            // truncated when it is flushed to the scroll buffer.
            let _ = fwd_handle.await;

            let elapsed = started.elapsed().as_secs_f32();

            match result {
                Ok((usage_summary, model)) => {
                    let _ = self
                        .ui_tx
                        .send(ui::UiEvent::AssistantDone {
                            task_id,
                            usage_summary,
                            elapsed_secs: elapsed,
                            model,
                            provider: provider_label,
                        })
                        .await;
                }
                Err(e) => {
                    let _ = self
                        .ui_tx
                        .send(ui::UiEvent::Error {
                            message: format!("{e:#}"),
                        })
                        .await;
                    let _ = self
                        .ui_tx
                        .send(ui::UiEvent::AssistantDone {
                            task_id,
                            usage_summary: "error".into(),
                            elapsed_secs: elapsed,
                            model: self.cfg.model.default.clone(),
                            provider: provider_label,
                        })
                        .await;
                }
            }
        });
    }

    /// Run the AI provider for a single task and return `(usage_summary,
    /// model)`. All output goes through `delta_tx`; nothing is printed to
    /// stdout.
    async fn run_interactive(
        &self,
        input: &str,
        delta_tx: tokio::sync::mpsc::UnboundedSender<String>,
    ) -> Result<(String, String)> {
        ensure_layout().await?;
        let mut registry = ToolRegistry::with_builtins();
        mcp_tools::install_all(&mut registry).await;
        let registry = Arc::new(registry);
        let session = Session::open(self.session_log.as_path()).await?;
        let policy = PolicyConfig::load(&policy_path()?).await;
        let tool_ctx = ToolContext {
            workspace: workspace_dir()?,
            allow_user_prompt: true,
            ask_tx: Some(self.ask_tx.clone()),
            vault_path: vault_path().ok(),
            evoclaw_dir: evoclaw_dir().ok(),
            policy: Some(Arc::new(policy)),
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

        let mut runtime = ConversationRuntime::new(
            self.provider.clone(),
            registry,
            session,
            tool_ctx,
            evo_core::runtime::RuntimeConfig {
                model: self.cfg.model.default.clone(),
                reflection_enabled: !self.is_acp,
                provider_id: self.cfg.model.provider.clone(),
                mcp_servers: get_active_mcp_servers().await.unwrap_or_default(),
                ..Default::default()
            },
        )
        .with_cost_engine(cost_engine)
        .with_memory(memory)
        .with_skills_dir(skills_dir()?)
        .with_redactor(redactor)
        .with_delta_sender(delta_tx);

        // Inject the REPL-wide conversation history so prior turns remain
        // visible to the model. ACP providers manage history upstream — skip
        // sync to avoid double-billing tokens (the runtime also clears on
        // every ACP run as a safety net).
        if !self.is_acp {
            let history_snapshot = self.shared_history.lock().await.clone();
            runtime.set_history(history_snapshot);
        }

        let run_result = runtime.run(input).await;

        // Persist the updated history back to the REPL-wide store BEFORE
        // returning, regardless of success/failure. On error the runtime still
        // holds the partial conversation (user message + any assistant turns
        // that completed before the failure) — losing that would mean the user
        // can't ask "what went wrong?" in a follow-up because the assistant
        // wouldn't even see the original question.
        if !self.is_acp {
            *self.shared_history.lock().await = runtime.take_history();
        }

        let outcome = run_result?;
        let usage = &outcome.usage;
        let usage_summary = if usage.turns_with_usage == 0 {
            "unavailable".to_string()
        } else {
            format!(
                "{}↑ {}↓ {}cached",
                usage.input_tokens, usage.output_tokens, usage.cached_tokens,
            )
        };
        Ok((usage_summary, self.cfg.model.default.clone()))
    }
}

// ---------------------------------------------------------------------------
// Provider-based task runner (one-shot with spinner)
// ---------------------------------------------------------------------------

/// Run a single task with an already-built provider. Each invocation creates
/// a fresh session/redactor (cheap IO) but **reuses the provider** so shell
/// loops don't repay process-spawn cost on every turn.
pub(crate) async fn run_task_with_provider(
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
    let policy = PolicyConfig::load(&policy_path()?).await;
    let tool_ctx = ToolContext {
        workspace: workspace_dir()?,
        allow_user_prompt: true,
        vault_path: vault_path().ok(),
        evoclaw_dir: evoclaw_dir().ok(),
        policy: Some(Arc::new(policy)),
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
    let spinner = Spinner::start(theme, "processing…");
    let outcome = runtime.run(input).await?;
    drop(spinner);
    let elapsed = started.elapsed();
    let usage = &outcome.usage;
    let usage_summary = if usage.turns_with_usage == 0 {
        "unavailable".to_string()
    } else {
        format!(
            "{}↑ in · {}↓ out · {} cached ({:.0}% hit)",
            usage.input_tokens,
            usage.output_tokens,
            usage.cached_tokens,
            usage.cache_hit_rate() * 100.0,
        )
    };
    let model_label = cfg.model.default.as_str();
    let provider_label = cfg
        .model
        .provider
        .as_deref()
        .unwrap_or("(default)")
        .to_string();

    let body = if outcome.final_text_ui.is_empty() {
        &outcome.final_text
    } else {
        &outcome.final_text_ui
    };
    print!(
        "{}",
        TerminalUI::render_answer_block(
            &theme,
            body,
            outcome.turns,
            elapsed.as_secs_f32(),
            model_label,
            &provider_label,
            &usage_summary,
        )
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// One-shot entry point
// ---------------------------------------------------------------------------

pub(crate) async fn run_one_shot(input: &str) -> Result<()> {
    let cfg = load_config().await?;
    ensure_layout().await?;
    let (provider, is_acp) = build_provider(&cfg).await?;
    let task_id = format!("task-{}", chrono::Utc::now().format("%Y%m%dT%H%M%S%.3f"));
    let log_path = logs_dir()?.join(format!("{task_id}.jsonl"));
    run_task_with_provider(input, provider, is_acp, &cfg, &log_path, Theme::detect()).await
}
