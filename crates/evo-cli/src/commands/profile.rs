//! Profile management commands.

use crate::config::{
    active_profile_file, config_path, load_config, profiles_dir, AuthCfg, Config, ConfigBudget,
    ModelCfg, ProfileMeta, SecurityCfg,
};
use crate::theme::{DisplayTemplate, Theme};
use eyre::Result;
use std::path::PathBuf;

// ---------------------------------------------------------------------------
// Active profile helpers
// ---------------------------------------------------------------------------

pub(crate) async fn get_active_profile_name() -> Result<String> {
    let active_file = active_profile_file()?;
    if active_file.exists() {
        let name = tokio::fs::read_to_string(&active_file).await?;
        Ok(name.trim().to_string())
    } else {
        Ok("default".to_string())
    }
}

pub(crate) async fn set_active_profile_name(name: &str) -> Result<()> {
    let active_file = active_profile_file()?;
    if let Some(parent) = active_file.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    tokio::fs::write(&active_file, name).await?;
    Ok(())
}

pub(crate) async fn get_profile_path(name: &str) -> Result<PathBuf> {
    Ok(profiles_dir()?.join(format!("{}.toml", name)))
}

// ---------------------------------------------------------------------------
// profile_list
// ---------------------------------------------------------------------------

pub(crate) async fn profile_list() -> Result<()> {
    let theme = Theme::detect();
    let profiles_path = profiles_dir()?;

    println!(
        "{}",
        DisplayTemplate::header(&theme, "Configuration Profiles")
    );

    if !profiles_path.exists() {
        println!(
            "  {}",
            DisplayTemplate::kv(&theme, "Profiles", "None created yet")
        );
        println!("{}", DisplayTemplate::footer(&theme));
        println!();
        println!("  Tip: Use '/profile add <name>' to create a new profile");
        return Ok(());
    }

    let active = get_active_profile_name()
        .await
        .unwrap_or_else(|_| "default".to_string());
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
        println!(
            "  {}{}",
            DisplayTemplate::kv_colored(&theme, &display_name, &desc, theme.dim()),
            marker
        );
    }

    println!("{}", DisplayTemplate::footer(&theme));
    println!();
    Ok(())
}

// ---------------------------------------------------------------------------
// profile_show
// ---------------------------------------------------------------------------

pub(crate) async fn profile_show(name: Option<&str>) -> Result<()> {
    let theme = Theme::detect();
    let profile_name = match name {
        Some(n) => n.to_string(),
        None => get_active_profile_name().await?,
    };

    let profile_path = get_profile_path(&profile_name).await?;
    if !profile_path.exists() {
        println!();
        println!(
            "{err}Profile '{profile_name}' not found{reset}",
            err = theme.err(),
            reset = theme.reset()
        );
        println!();
        return Ok(());
    }

    let cfg_str = tokio::fs::read_to_string(&profile_path).await?;
    let cfg: Config = toml::from_str(&cfg_str)?;

    println!(
        "{}",
        DisplayTemplate::header(&theme, &format!("Profile: {}", profile_name))
    );

    if let Some(desc) = &cfg.meta.description {
        println!("  {}", DisplayTemplate::kv(&theme, "Description", desc));
    }

    let provider_id = cfg.model.provider.as_deref().unwrap_or("(not set)");
    println!("  {}", DisplayTemplate::kv(&theme, "Provider", provider_id));
    println!(
        "  {}",
        DisplayTemplate::kv(&theme, "Model", &cfg.model.default)
    );
    println!(
        "  {}",
        DisplayTemplate::kv(&theme, "Auth Method", cfg.auth.parsed().as_str())
    );
    println!(
        "  {}",
        DisplayTemplate::kv(
            &theme,
            "Budget (task)",
            &format!("${:.2}", cfg.budget.per_task_usd)
        )
    );

    println!("{}", DisplayTemplate::footer(&theme));
    println!();
    Ok(())
}

// ---------------------------------------------------------------------------
// profile_switch
// ---------------------------------------------------------------------------

pub(crate) async fn profile_switch(name: &str) -> Result<()> {
    let theme = Theme::detect();
    let profile_path = get_profile_path(name).await?;

    if !profile_path.exists() {
        println!();
        println!(
            "{err}Profile '{name}' not found{reset}",
            err = theme.err(),
            reset = theme.reset()
        );
        println!();
        println!("Available profiles:");
        profile_list().await?;
        return Ok(());
    }

    set_active_profile_name(name).await?;

    // Copy to config.toml for backward compatibility
    let config_p = config_path()?;
    tokio::fs::copy(&profile_path, &config_p).await?;

    println!();
    println!(
        "{ok}Switched to profile '{name}'{reset}",
        ok = theme.ok(),
        reset = theme.reset()
    );
    println!();
    Ok(())
}

// ---------------------------------------------------------------------------
// profile_add
// ---------------------------------------------------------------------------

pub(crate) async fn profile_add(name: &str, template: Option<&str>) -> Result<()> {
    let theme = Theme::detect();
    let profiles_path = profiles_dir()?;
    tokio::fs::create_dir_all(&profiles_path).await?;

    let profile_path = get_profile_path(name).await?;
    if profile_path.exists() {
        println!();
        println!(
            "{warn}Profile '{name}' already exists{reset}",
            warn = theme.warn(),
            reset = theme.reset()
        );
        println!();
        return Ok(());
    }

    let template_cfg = if let Some(tmpl) = template {
        get_profile_template(tmpl)?
    } else {
        load_config().await?
    };

    let mut cfg = template_cfg;
    cfg.meta.name = Some(name.to_string());
    cfg.meta.description = Some(format!("Profile: {}", name));

    let toml_str = toml::to_string_pretty(&cfg)?;
    tokio::fs::write(&profile_path, toml_str).await?;

    println!();
    println!(
        "{ok}Created profile '{name}'{reset}",
        ok = theme.ok(),
        reset = theme.reset()
    );
    println!(
        "  Location: {dim}{}{reset}",
        profile_path.display(),
        dim = theme.dim(),
        reset = theme.reset()
    );
    println!();
    println!("  Use '/profile switch {name}' to activate it");
    println!();
    Ok(())
}

// ---------------------------------------------------------------------------
// profile_remove
// ---------------------------------------------------------------------------

pub(crate) async fn profile_remove(name: &str) -> Result<()> {
    let theme = Theme::detect();

    if name == "default" {
        println!();
        println!(
            "{err}Cannot remove 'default' profile{reset}",
            err = theme.err(),
            reset = theme.reset()
        );
        println!();
        return Ok(());
    }

    let active = get_active_profile_name()
        .await
        .unwrap_or_else(|_| "default".to_string());
    if name == active {
        println!();
        println!(
            "{err}Cannot remove active profile{reset}",
            err = theme.err(),
            reset = theme.reset()
        );
        println!("  Switch to another profile first");
        println!();
        return Ok(());
    }

    let profile_path = get_profile_path(name).await?;
    if !profile_path.exists() {
        println!();
        println!(
            "{warn}Profile '{name}' not found{reset}",
            warn = theme.warn(),
            reset = theme.reset()
        );
        println!();
        return Ok(());
    }

    tokio::fs::remove_file(&profile_path).await?;

    println!();
    println!(
        "{ok}Removed profile '{name}'{reset}",
        ok = theme.ok(),
        reset = theme.reset()
    );
    println!();
    Ok(())
}

// ---------------------------------------------------------------------------
// profile_edit
// ---------------------------------------------------------------------------

pub(crate) async fn profile_edit(name: Option<&str>) -> Result<()> {
    let theme = Theme::detect();
    let profile_name = match name {
        Some(n) => n.to_string(),
        None => get_active_profile_name().await?,
    };

    let profile_path = get_profile_path(&profile_name).await?;
    if !profile_path.exists() {
        println!();
        println!(
            "{err}Profile '{profile_name}' not found{reset}",
            err = theme.err(),
            reset = theme.reset()
        );
        println!();
        return Ok(());
    }

    let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vi".to_string());

    println!();
    println!(
        "{info}Opening profile in {editor}...{reset}",
        info = theme.info(),
        reset = theme.reset()
    );

    std::process::Command::new(&editor)
        .arg(&profile_path)
        .status()?;

    println!();
    println!(
        "{ok}Profile '{profile_name}' saved{reset}",
        ok = theme.ok(),
        reset = theme.reset()
    );
    println!("  Use '/profile switch {profile_name}' to reload if this is not the active profile");
    println!();
    Ok(())
}

// ---------------------------------------------------------------------------
// get_profile_template
// ---------------------------------------------------------------------------

pub(crate) fn get_profile_template(template: &str) -> Result<Config> {
    let (provider, model, description) = match template {
        "deepseek" => ("deepseek", "deepseek-chat", "DeepSeek Chat configuration"),
        "openai" => ("openai", "gpt-4o", "OpenAI GPT-4o configuration"),
        "claude" | "anthropic" => (
            "anthropic",
            "claude-3-5-sonnet-20241022",
            "Claude 3.5 Sonnet configuration",
        ),
        "gemini" | "google" => (
            "google",
            "gemini-2.0-flash-exp",
            "Google Gemini configuration",
        ),
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
