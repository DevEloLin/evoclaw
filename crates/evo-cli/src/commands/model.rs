//! Model selection and listing commands.

use crate::config::{config_path, load_config};
use crate::onboard;
use crate::theme::Theme;
use eyre::{Result, WrapErr};

// ---------------------------------------------------------------------------
// model_show
// ---------------------------------------------------------------------------

pub(crate) async fn model_show() -> Result<()> {
    let theme = Theme::detect();
    let cfg = load_config().await?;

    println!();
    println!(
        "{bold}== Current Model =={reset}",
        bold = theme.bold(),
        reset = theme.reset()
    );
    println!();

    let provider_id = cfg.model.provider.as_deref().unwrap_or("(unknown)");

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

// ---------------------------------------------------------------------------
// model_list
// ---------------------------------------------------------------------------

pub(crate) async fn model_list() -> Result<()> {
    let theme = Theme::detect();
    let cfg = load_config().await?;

    println!();
    println!(
        "{bold}== Available Models =={reset}",
        bold = theme.bold(),
        reset = theme.reset()
    );
    println!();

    let provider_id = cfg.model.provider.as_deref().unwrap_or("deepseek");

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
// model_set
// ---------------------------------------------------------------------------

pub(crate) async fn model_set(model_name: &str) -> Result<()> {
    let theme = Theme::detect();
    let cfg_path = config_path()?;
    let mut cfg = load_config().await?;

    let provider_id = cfg.model.provider.as_deref().unwrap_or("deepseek");

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
            cfg.model.default = model_name.to_string();
            let toml_str = toml::to_string_pretty(&cfg).wrap_err("serialize config")?;
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

    cfg.model.default = model_name.to_string();
    let toml_str = toml::to_string_pretty(&cfg).wrap_err("serialize config")?;
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
