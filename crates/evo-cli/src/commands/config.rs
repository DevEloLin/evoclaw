//! Configuration, status, and model management commands.

use crate::commands::profile::get_active_profile_name;
use crate::config::{config_path, load_config, session_log_path, workspace_dir};
use crate::slash::{count_skills, count_vault_entries};
use crate::terminal_ui::TerminalUI;
use crate::theme::{display_home, Theme};
use crate::onboard;
use eyre::{Result, WrapErr};
use evo_providers::AuthMethod;

const VERSION: &str = env!("CARGO_PKG_VERSION");

// ---------------------------------------------------------------------------
// config_show
// ---------------------------------------------------------------------------

pub(crate) async fn config_show() -> Result<()> {
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

// ---------------------------------------------------------------------------
// config_set
// ---------------------------------------------------------------------------

pub(crate) async fn config_set(key: &str, value: &str) -> Result<()> {
    let theme = Theme::detect();
    let cfg_path = config_path()?;
    let mut cfg = load_config().await?;

    let updated = match key {
        "model.default" | "model" => {
            cfg.model.default = value.to_string();
            true
        }
        "model.base_url" | "base_url" => {
            cfg.model.base_url = value.to_string();
            true
        }
        "budget.per_task_usd" | "budget.per_task" => match value.parse::<f64>() {
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
        },
        "budget.per_day_usd" | "budget.per_day" => match value.parse::<f64>() {
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
        },
        "budget.per_month_usd" | "budget.per_month" => match value.parse::<f64>() {
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
        },
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
        "security.high_risk_intercept" | "high_risk_intercept" => match value.parse::<bool>() {
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
        },
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
        let toml_str = toml::to_string_pretty(&cfg).wrap_err("serialize config")?;
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

// ---------------------------------------------------------------------------
// config_reset
// ---------------------------------------------------------------------------

pub(crate) async fn config_reset() -> Result<()> {
    use std::io::Write as _;

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
        println!(
            "{dim}(cancelled){reset}",
            dim = theme.dim(),
            reset = theme.reset()
        );
    }

    println!();
    Ok(())
}

// ---------------------------------------------------------------------------
// status_cmd
// ---------------------------------------------------------------------------

pub(crate) async fn status_cmd() -> Result<()> {
    let theme = Theme::detect();
    let cfg = load_config().await?;
    let active_profile = get_active_profile_name()
        .await
        .unwrap_or_else(|_| "default".to_string());
    let provider_id = cfg.model.provider.as_deref().unwrap_or("unknown");
    let is_acp = provider_id.starts_with("acp:");
    let (vendor_name, is_local) = if is_acp {
        let agent_name = provider_id.strip_prefix("acp:").unwrap_or(provider_id);
        (format!("External ACP Agent: {}", agent_name), false)
    } else {
        match onboard::find_provider(provider_id) {
            Some(profile) => (
                format!(
                    "{} ({})",
                    profile.name,
                    if profile.local { "Local" } else { "Cloud" }
                ),
                profile.local,
            ),
            None => (format!("Custom: {}", provider_id), false),
        }
    };
    let auth_method = cfg.auth.parsed();
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
                    Ok(p) => p
                        .account_label
                        .unwrap_or_else(|| String::from("Unknown account")),
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
    let session_log = session_log_path()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "(unavailable)".to_string());
    let cfg_path_str = config_path()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "(unavailable)".to_string());
    let workspace = workspace_dir()
        .map(|p| display_home(&p.display().to_string()))
        .unwrap_or_else(|_| "(unavailable)".to_string());
    let vault_count = count_vault_entries().await;
    let skill_count = count_skills().await.unwrap_or(0);
    let timestamp = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();

    print!(
        "{}",
        TerminalUI::render_top_status_bar(
            &theme,
            &format!("evoclaw v{VERSION}"),
            provider_id,
            &cfg.model.default,
            &workspace,
            &timestamp,
        )
    );
    print!(
        "{}",
        TerminalUI::panel(
            &theme,
            "会话历史区",
            &[
                format!("active_profile: {active_profile}"),
                format!("vendor: {vendor_name}"),
                format!("provider: {provider_id}"),
                format!("model: {}", cfg.model.default),
                format!(
                    "endpoint: {}",
                    if cfg.model.base_url.is_empty() {
                        "(not set)"
                    } else {
                        &cfg.model.base_url
                    }
                ),
                format!("auth: {} · {status_text}", auth_method.as_str()),
                format!("account: {account_info}"),
            ],
            theme.frame(),
        )
    );
    print!(
        "{}",
        TerminalUI::panel(
            &theme,
            "任务状态区",
            &[
                "串行执行 — 当前任务完成后接受下一条输入".to_string(),
                format!(
                    "runtime type: {}",
                    if is_local { "local inference" } else { "cloud/external" }
                ),
                format!("session log: {session_log}"),
            ],
            theme.ok(),
        )
    );
    print!(
        "{}",
        TerminalUI::panel(
            &theme,
            "使用信息区",
            &[
                format!("config: {cfg_path_str}"),
                format!("workspace: {workspace}"),
                format!("vault: {vault_count} secrets"),
                format!("skills: {skill_count} learned"),
                "运行 /usage 查看 7d 和 30d token / cost 汇总".to_string(),
            ],
            theme.info(),
        )
    );

    Ok(())
}

