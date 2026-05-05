//! Channel adapter commands: list, status, add, remove, run.

use crate::config::{
    cost_log_path, ensure_layout, evoclaw_dir, load_config, logs_dir, memory_dir, skills_dir,
    vault_path, workspace_dir,
};
use crate::mcp_tools;
use crate::onboard;
use evo_core::{ConversationRuntime, Memory, Session};
use evo_policy::{BudgetCfg, CostEngine, Redactor, Vault};
use evo_providers::{
    AcpProvider, AnthropicProvider, AuthMethod, BrowserProvider, CopilotProvider,
    OpenAiCompatProvider, Provider,
};
use evo_tools::{ToolContext, ToolRegistry};
use eyre::{Result, WrapErr};
use std::path::PathBuf;
use std::sync::Arc;

// ---------------------------------------------------------------------------
// Directory helper
// ---------------------------------------------------------------------------

pub(crate) fn channels_dir() -> Result<PathBuf> {
    Ok(evoclaw_dir()?.join("channels"))
}

// ---------------------------------------------------------------------------
// Known channels catalog
// ---------------------------------------------------------------------------

/// (kind, env-var, setup instructions)
pub(crate) const KNOWN_CHANNELS: &[(&str, &str, &str)] = &[
    (
        "telegram",
        "TELEGRAM_BOT_TOKEN",
        "Create a bot via @BotFather on Telegram and copy the token it gives you.",
    ),
    (
        "slack",
        "SLACK_BOT_TOKEN",
        "Create an app at api.slack.com/apps, add Bot Token Scopes, install to workspace.",
    ),
    (
        "discord",
        "DISCORD_BOT_TOKEN",
        "Create an application at discord.com/developers/applications, add a Bot, copy token.",
    ),
];

// ---------------------------------------------------------------------------
// channel_list
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// channel_handler (entry dispatch)
// ---------------------------------------------------------------------------

pub(crate) async fn channel_handler(sub: crate::ChannelCmd) -> Result<()> {
    use crate::ChannelCmd;
    match sub {
        ChannelCmd::List => channel_list().await,
        ChannelCmd::Status => channel_status().await,
        ChannelCmd::Add { kind } => channel_add(&kind).await,
        ChannelCmd::Remove { kind } => channel_remove(&kind).await,
        ChannelCmd::Run { kind } => channel_run(&kind).await,
    }
}

// ---------------------------------------------------------------------------
// channel_list
// ---------------------------------------------------------------------------

pub(crate) async fn channel_list() -> Result<()> {
    println!("== EvoClaw channel adapters ==");
    println!();
    println!("built-in:");
    println!(
        "  {:<14} stdin/stdout JSON (reference adapter)",
        "local-pipe"
    );
    println!(
        "  {:<14} Telegram Bot API long-polling  (token: TELEGRAM_BOT_TOKEN or vault)",
        "telegram"
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
        println!("planned: slack, discord, line, messenger — see docs/channels.md");
    } else {
        println!("external (~/.evoclaw/channels/*.toml):");
        for (name, path) in external {
            println!("  {:<14} {}", name, path.display());
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// channel_status
// ---------------------------------------------------------------------------

/// Show which channel tokens are currently saved (env var or key file).
pub(crate) async fn channel_status() -> Result<()> {
    println!("== channel token status ==");
    println!();
    println!("{:<12} {:<12} SOURCE", "KIND", "STATUS");
    println!("{}", "-".repeat(60));
    for (kind, env_var, _) in KNOWN_CHANNELS {
        let (status, source) = channel_token_source(kind, env_var);
        println!("{:<12} {:<12} {}", kind, status, source);
    }
    println!();
    println!("To add:    evo channel add <kind>");
    println!("To remove: evo channel remove <kind>");
    println!("To start:  evo channel run --kind <kind>");
    Ok(())
}

// ---------------------------------------------------------------------------
// channel_add
// ---------------------------------------------------------------------------

/// Save a bot token for a channel adapter to `~/.evoclaw/secrets/<kind>_bot_token.key`.
pub(crate) async fn channel_add(kind: &str) -> Result<()> {
    use std::io::Write as _;

    ensure_layout().await?;
    let kind_lower = kind.to_lowercase();
    let entry = KNOWN_CHANNELS
        .iter()
        .find(|(k, _, _)| *k == kind_lower.as_str())
        .ok_or_else(|| {
            eyre::eyre!(
                "unknown channel '{kind}'. Supported: {}",
                KNOWN_CHANNELS
                    .iter()
                    .map(|(k, _, _)| *k)
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })?;
    let (_, env_var, instructions) = entry;

    println!();
    println!("  Register {} bot token.", kind_lower);
    println!("  {instructions}");
    println!("  (alternatively, set env var {env_var} — it takes precedence over the file)");
    println!();
    print!("  Token (input visible — clear scrollback after): ");
    std::io::stdout().flush().ok();
    let mut buf = String::new();
    std::io::stdin().read_line(&mut buf)?;
    let token = buf.trim().to_string();
    if token.is_empty() {
        return Err(eyre::eyre!("empty token — aborted"));
    }

    let secret_name = format!("{kind_lower}_bot_token");
    let path = evoclaw_dir()?
        .join("secrets")
        .join(format!("{secret_name}.key"));
    tokio::fs::write(&path, format!("{token}\n")).await?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    }

    println!();
    println!("  ✓ {} token saved ({})", kind_lower, path.display());
    println!("  Run `evo channel run --kind {kind_lower}` to start the adapter.");
    Ok(())
}

// ---------------------------------------------------------------------------
// channel_remove
// ---------------------------------------------------------------------------

/// Remove a previously saved channel bot token.
pub(crate) async fn channel_remove(kind: &str) -> Result<()> {
    let kind_lower = kind.to_lowercase();
    let secret_name = format!("{kind_lower}_bot_token");
    let path = evoclaw_dir()?
        .join("secrets")
        .join(format!("{secret_name}.key"));
    if path.exists() {
        tokio::fs::remove_file(&path).await?;
        println!("✓ {kind_lower} token removed.");
    } else {
        println!("{kind_lower} token not found — nothing to remove.");
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// channel_run
// ---------------------------------------------------------------------------

pub(crate) async fn channel_run(kind: &str) -> Result<()> {
    use evo_core::channel::{ChannelAdapter, ChannelKind, OutboundKind, OutboundMessage};
    use evo_core::channel_router::{self, ChannelRouter};
    use evo_core::local_pipe::LocalPipe;
    use evo_core::telegram::TelegramAdapter;

    ensure_layout().await?;
    tokio::fs::create_dir_all(channels_dir()?).await.ok();

    let (adapter, channel_kind): (Arc<dyn ChannelAdapter>, ChannelKind) = match kind {
        "local-pipe" => {
            eprintln!(
                "→ channel: local-pipe adapter ready. Send line-delimited \
                 InboundMessage JSON on stdin; replies stream to stdout."
            );
            (Arc::new(LocalPipe), ChannelKind::LocalPipe)
        }
        "telegram" => {
            let token = resolve_channel_token("telegram", "TELEGRAM_BOT_TOKEN").await?;
            eprintln!("→ channel: telegram adapter ready (long-polling).");
            (Arc::new(TelegramAdapter::new(token)), ChannelKind::Telegram)
        }
        other => {
            return Err(eyre::eyre!(
                "unknown adapter '{other}'. Built-in: local-pipe, telegram. \
                 Slack/Discord planned — see docs/channels.md."
            ));
        }
    };

    let mut router = ChannelRouter::new();
    router.register(adapter.clone());

    let (inbound_tx, mut inbound_rx) = tokio::sync::mpsc::channel(64);
    let router_handle = tokio::spawn(router.run_all(inbound_tx));

    while let Some(msg) = inbound_rx.recv().await {
        if !channel_router::should_handle(&msg) {
            tracing::debug!(
                conversation_id = %msg.conversation_id,
                "channel: skipping un-mentioned message"
            );
            continue;
        }
        let conv_id = msg.conversation_id.clone();
        let ck = channel_kind.clone();
        let reply = match channel_run_one_shot_text(&msg.text, &ck).await {
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

// ---------------------------------------------------------------------------
// channel_run_one_shot_text
// ---------------------------------------------------------------------------

/// Thin wrapper around the conversation runtime that returns the final
/// text instead of printing it. Used by the channel dispatch loop so the
/// reply travels through the adapter rather than stdout-as-CLI.
pub(crate) async fn channel_run_one_shot_text(
    input: &str,
    channel_kind: &evo_core::channel::ChannelKind,
) -> Result<String> {
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
                    "config.toml has [auth].method = \"acp\" but provider is not set to an ACP \
                     agent. Run `evoclaw login` and select 'External ACP agent'."
                ));
            }
            AuthMethod::ApiKey => {
                let (api_key, _src) = onboard::resolve_api_key(&provider_id).await?;
                match provider_id.as_str() {
                    "anthropic" => {
                        Arc::new(AnthropicProvider::new(api_key, cfg.model.default.clone()))
                            as Arc<dyn Provider>
                    }
                    "copilot" => Arc::new(CopilotProvider::new(api_key, cfg.model.default.clone())),
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
            mcp_servers: crate::slash::get_active_mcp_servers()
                .await
                .unwrap_or_default(),
            channel_hint: Some(channel_format_hint(channel_kind)),
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

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Resolve a channel bot token from environment or the EvoClaw secrets dir.
///
/// Resolution order:
///   1. Environment variable `env_var` (e.g. `TELEGRAM_BOT_TOKEN`)
///   2. `~/.evoclaw/secrets/<kind>_bot_token.key`
pub(crate) async fn resolve_channel_token(kind: &str, env_var: &str) -> Result<String> {
    if let Ok(v) = std::env::var(env_var) {
        if !v.trim().is_empty() {
            return Ok(v.trim().to_string());
        }
    }
    let secret_name = format!("{kind}_bot_token");
    let path = evoclaw_dir()?
        .join("secrets")
        .join(format!("{secret_name}.key"));
    if path.exists() {
        let token = tokio::fs::read_to_string(&path).await?.trim().to_string();
        if !token.is_empty() {
            return Ok(token);
        }
    }
    Err(eyre::eyre!(
        "No token found for '{kind}' channel.\n\
         Set env var {env_var}, or store it with:\n  \
         evoclaw channel add {kind}"
    ))
}

/// Return (status_label, source_description) for a channel's token.
pub(crate) fn channel_token_source(kind: &str, env_var: &str) -> (&'static str, String) {
    if std::env::var(env_var)
        .map(|v| !v.trim().is_empty())
        .unwrap_or(false)
    {
        return ("configured", format!("env: {env_var}"));
    }
    let secret_name = format!("{kind}_bot_token");
    let path = evoclaw_dir()
        .ok()
        .map(|d| d.join("secrets").join(format!("{secret_name}.key")));
    match path {
        Some(p) if p.exists() => (
            "configured",
            format!("file: ~/.evoclaw/secrets/{secret_name}.key"),
        ),
        _ => ("missing", format!("run: evo channel add {kind}")),
    }
}

/// Per-channel Markdown formatting instruction injected into the system prompt
/// so the model layouts its reply for the target platform automatically.
pub(crate) fn channel_format_hint(kind: &evo_core::channel::ChannelKind) -> String {
    use evo_core::channel::ChannelKind;
    match kind {
        ChannelKind::Telegram => concat!(
            "Output format (Telegram Markdown): use *bold* for key terms and headings, ",
            "`inline code` for commands/paths/values, triple-backtick blocks for code. ",
            "Bullet points with - for lists. Keep answers concise and well-structured. ",
            "Do NOT include the <summary> XML tag in your reply — output the answer directly."
        )
        .into(),
        ChannelKind::Slack | ChannelKind::Discord => concat!(
            "Output format (Slack/Discord Markdown): use **bold** for headings, ",
            "`inline code`, and triple-backtick code blocks. Bullet points with -."
        )
        .into(),
        _ => concat!(
            "Output format: clear Markdown with bold headings, bullet points, ",
            "and code blocks where appropriate. Keep answers focused and structured."
        )
        .into(),
    }
}
