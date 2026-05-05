//! Slash-command dispatcher and related helpers.

use crate::config::{evoclaw_dir, skills_dir, vault_path};
use eyre::Result;
use evo_policy::Vault;
use std::path::PathBuf;

// ---------------------------------------------------------------------------
// SlashOutcome
// ---------------------------------------------------------------------------

/// Outcome of a slash-command invocation. The interactive loop reads this
/// to decide whether to keep prompting (`Continue`), exit cleanly (`Exit`),
/// or reload `Config` + provider (`Reload`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SlashOutcome {
    Continue,
    Exit,
    Reload,
}

// ---------------------------------------------------------------------------
// handle_slash
// ---------------------------------------------------------------------------

pub(crate) async fn handle_slash(rest: &str) -> Result<SlashOutcome> {
    use crate::commands::{agent, channel, config, diag, mcp, model, profile, secret, skill};
    use crate::commands::onboard::login_cmd;

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
            [] | ["list"] => agent::agent_list().await?,
            ["catalog"] => agent::agent_catalog(),
            ["add", id] => {
                agent::agent_add(id).await?;
                return Ok(SlashOutcome::Reload);
            }
            ["remove", id] => agent::agent_remove(id).await?,
            ["test", id] => agent::agent_test(id).await?,
            _ => println!("usage: /agent [list|catalog|add <id>|remove <id>|test <id>]"),
        },
        "mcp" => match args.as_slice() {
            [] | ["list"] => mcp::mcp_list().await?,
            ["catalog"] => mcp::mcp_catalog(),
            ["add", id] => mcp::mcp_add(id).await?,
            ["remove", id] => mcp::mcp_remove(id).await?,
            ["test", id] => mcp::mcp_test(id).await?,
            _ => println!("usage: /mcp [list|catalog|add <id>|remove <id>|test <id>]"),
        },
        "secret" => match args.as_slice() {
            [] | ["list"] => secret::secret_list().await?,
            ["add", name] => secret::secret_add(name, true, None).await?,
            ["add", name, value] => {
                secret::secret_add(name, false, Some(value.to_string())).await?
            }
            ["remove", name] => secret::secret_remove(name).await?,
            ["test", rest @ ..] => secret::secret_test(&rest.join(" ")).await?,
            _ => println!("usage: /secret [list|add <name> [value]|remove <name>|test <text>]"),
        },
        "channel" => match args.as_slice() {
            [] | ["list"] => channel::channel_list().await?,
            ["run", kind] => channel::channel_run(kind).await?,
            _ => println!("usage: /channel [list|run <kind>]   (built-in: local-pipe)"),
        },
        "clear" => {
            use std::io::Write as _;
            print!("\x1b[2J\x1b[H");
            std::io::stdout().flush().ok();
        }
        "doctor" => diag::doctor().await?,
        "tokens" => diag::doctor_tokens().await?,
        "closure" => diag::doctor_closure().await?,
        "replay" => diag::replay(args.first().map(PathBuf::from)).await?,
        "skill" => match args.as_slice() {
            [] | ["list"] => skill::skill_list().await?,
            ["tree"] => skill::skill_tree().await?,
            ["show", id] => skill::skill_show(id).await?,
            _ => println!("usage: /skill [list|tree|show <id>]"),
        },
        "memory" => match args.as_slice() {
            [] => println!("usage: /memory <query>"),
            ["search", q @ ..] => skill::memory_search(&q.join(" "), 20).await?,
            q => skill::memory_search(&q.join(" "), 20).await?,
        },
        "logout" => {
            diag::logout_cmd().await?;
            return Ok(SlashOutcome::Reload);
        }
        "usage" => diag::usage_cmd().await?,
        "config" => match args.as_slice() {
            [] | ["show"] => config::config_show().await?,
            ["set", key, value] => config::config_set(key, value).await?,
            ["reset"] => config::config_reset().await?,
            _ => println!("usage: /config [show|set <key> <value>|reset]"),
        },
        "status" => config::status_cmd().await?,
        "model" => match args.as_slice() {
            [] => model::model_show().await?,
            ["list"] => model::model_list().await?,
            ["set", model_name] => {
                model::model_set(model_name).await?;
                return Ok(SlashOutcome::Reload);
            }
            _ => println!("usage: /model [list|set <model_name>]"),
        },
        "profile" => match args.as_slice() {
            [] | ["show"] => profile::profile_show(None).await?,
            ["show", name] => profile::profile_show(Some(name)).await?,
            ["list"] | ["ls"] => profile::profile_list().await?,
            ["switch" | "use", name] => {
                profile::profile_switch(name).await?;
                return Ok(SlashOutcome::Reload);
            }
            ["add", name] => profile::profile_add(name, args.get(3).copied()).await?,
            ["remove" | "rm", name] => profile::profile_remove(name).await?,
            ["edit", name] => profile::profile_edit(Some(name)).await?,
            ["edit"] => profile::profile_edit(None).await?,
            _ => println!(
                "usage: /profile [list|show [name]|switch <name>|add <name>|remove <name>|edit [name]]"
            ),
        },
        other => println!("unknown command: /{other}  (try /help)"),
    }
    Ok(SlashOutcome::Continue)
}

// ---------------------------------------------------------------------------
// print_help
// ---------------------------------------------------------------------------

pub(crate) fn print_help() {
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
    println!("  /closure             session JSONL audit (PRD 39)");
    println!("  /replay [path]       pretty-print a session (latest by default)");
    println!("  /doctor              health check");
    println!("  /clear               clear screen");
    println!("  /exit  /quit  /q     exit (also Ctrl-D, or Ctrl-C twice)");
    println!();
    println!("keyboard shortcuts:");
    println!("  Tab                  auto-complete slash commands");
    println!("  Up/Down or Ctrl-P/N  history navigation");
    println!("  Ctrl-R               reverse search history");
    println!("  Ctrl-A / Ctrl-E      jump to start / end of line");
    println!("  Ctrl-K / Ctrl-U      delete to end / start of line");
    println!("  Ctrl-W               delete previous word");
    println!("  Ctrl-C (twice)       exit");
    println!();
    println!("anything else is treated as a task and runs through the agent loop.");
}

// ---------------------------------------------------------------------------
// get_active_mcp_servers
// ---------------------------------------------------------------------------

pub(crate) async fn get_active_mcp_servers() -> Result<Vec<String>> {
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
// count_vault_entries
// ---------------------------------------------------------------------------

pub(crate) async fn count_vault_entries() -> usize {
    let path = match vault_path() {
        Ok(p) => p,
        Err(_) => return 0,
    };
    match Vault::load(&path).await {
        Ok(v) => v.entries.len(),
        Err(_) => 0,
    }
}

// ---------------------------------------------------------------------------
// count_skills
// ---------------------------------------------------------------------------

pub(crate) async fn count_skills() -> Result<usize> {
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
