//! Diagnostic, usage, and replay commands.

use crate::commands::secret::most_recent_session;
use crate::config::{config_path, cost_log_path, load_config, logs_dir, workspace_dir};
use crate::terminal_ui::TerminalUI;
use crate::theme::Theme;
use evo_core::Session;
use evo_policy::{BudgetCfg, CostEngine};
use evo_providers::AuthMethod;
use eyre::Result;
use std::path::PathBuf;

// ---------------------------------------------------------------------------
// doctor
// ---------------------------------------------------------------------------

pub(crate) async fn doctor() -> Result<()> {
    use crate::config::evoclaw_dir;
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
    println!("secrets  : {}", crate::onboard::secrets_dir()?.display());

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
                println!("acp      : MISSING — {e:#}\nrun `evoclaw agent add {agent_id}`");
            }
        }
        return Ok(());
    }
    let auth_method = cfg.auth.parsed();
    println!(
        "auth     : {} ({})",
        auth_method.label(),
        auth_method.as_str()
    );
    match auth_method {
        AuthMethod::Browser => match crate::onboard::load_browser_profile(&provider_id).await {
            Ok(p) => println!(
                "browser  : OK ({}, captured {})",
                crate::onboard::browser_profile_path(&provider_id)?.display(),
                p.captured_at
            ),
            Err(e) => println!(
                "browser  : MISSING — {e:#}\nrun `evoclaw login` and pick (2) Browser sign-in"
            ),
        },
        AuthMethod::Acp => {
            println!("acp      : configured (auth handled by external agent)")
        }
        AuthMethod::ApiKey => match crate::onboard::resolve_api_key(&provider_id).await {
            Ok((_k, src)) => println!("api_key  : OK ({})", src.describe()),
            Err(e) => println!("api_key  : MISSING — {e:#}\nrun `evoclaw login`"),
        },
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// doctor_closure
// ---------------------------------------------------------------------------

pub(crate) async fn doctor_closure() -> Result<()> {
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
        println!("\nclosure: OK (PRD 39 #1, #4)");
    } else {
        println!("\nclosure: WARN — some sessions missing TaskRecord or EndRecord");
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// doctor_tokens
// ---------------------------------------------------------------------------

pub(crate) async fn doctor_tokens() -> Result<()> {
    let theme = Theme::detect();
    let path = cost_log_path()?;
    let engine = CostEngine::at(&path, BudgetCfg::default());
    let events = engine.read_events().await?;
    if events.is_empty() {
        print!(
            "{}",
            TerminalUI::panel(
                &theme,
                "Usage Info",
                &["no cost events recorded yet".to_string()],
                theme.info(),
            )
        );
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
    print!(
        "{}",
        TerminalUI::panel(
            &theme,
            "Usage Info",
            &[
                format!("path: {}", path.display()),
                format!(
                    "7d  events={} input_tokens={} output_tokens={}",
                    s7.4, s7.0, s7.2
                ),
                format!(
                    "7d  cached_tokens={} cache_hit={:.2}% usd_total=${:.4}",
                    s7.1,
                    hr(s7.1, s7.0) * 100.0,
                    s7.3
                ),
                format!(
                    "30d events={} input_tokens={} output_tokens={}",
                    s30.4, s30.0, s30.2
                ),
                format!(
                    "30d cached_tokens={} cache_hit={:.2}% usd_total=${:.4}",
                    s30.1,
                    hr(s30.1, s30.0) * 100.0,
                    s30.3
                ),
                format!(
                    "budget: per_task <= ${:.2}, per_day <= ${:.2} soft / ${:.2} hard, per_month <= ${:.0}",
                    engine.cfg().per_task_usd,
                    engine.cfg().per_day_soft_usd,
                    engine.cfg().per_day_hard_usd,
                    engine.cfg().per_month_usd
                ),
            ],
            theme.info(),
        )
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// logout_cmd
// ---------------------------------------------------------------------------

pub(crate) async fn logout_cmd() -> Result<()> {
    let theme = Theme::detect();
    println!();
    println!(
        "{warn}logout:{reset} clearing current authentication...",
        warn = theme.warn(),
        reset = theme.reset()
    );
    let cfg = load_config().await?;
    let auth_method = cfg.auth.parsed();
    match auth_method {
        AuthMethod::ApiKey => {
            if let Some(provider_id) = &cfg.model.provider {
                let secret_path = crate::onboard::secret_file(provider_id)?;
                if secret_path.exists() {
                    tokio::fs::remove_file(&secret_path).await.ok();
                    println!(
                        "  {ok}removed API key for {bold}{provider_id}{reset}",
                        ok = theme.ok(),
                        bold = theme.bold(),
                        reset = theme.reset()
                    );
                }
            }
        }
        AuthMethod::Browser => {
            if let Some(provider_id) = &cfg.model.provider {
                let profile_path = crate::onboard::browser_profile_path(provider_id)?;
                if profile_path.exists() {
                    tokio::fs::remove_dir_all(&profile_path).await.ok();
                    println!(
                        "  {ok}removed browser profile for {bold}{provider_id}{reset}",
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
        "{frame}Run {bold}/login{reset}{frame} to re-authenticate or Ctrl-D to exit.{reset}",
        frame = theme.frame(),
        bold = theme.bold(),
        reset = theme.reset()
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// usage_cmd
// ---------------------------------------------------------------------------

pub(crate) async fn usage_cmd() -> Result<()> {
    doctor_tokens().await
}

// ---------------------------------------------------------------------------
// replay
// ---------------------------------------------------------------------------

pub(crate) async fn replay(path: Option<PathBuf>) -> Result<()> {
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
