//! evo-gateway binary.

use directories::BaseDirs;
use evo_gateway::{serve, GatewayConfig};
use evo_providers::OpenAiCompatProvider;
use evo_tools::ToolRegistry;
use eyre::{Result, WrapErr};
use serde::Deserialize;
use std::sync::Arc;

#[derive(Debug, Deserialize)]
struct Config { model: ModelCfg }

#[derive(Debug, Deserialize)]
struct ModelCfg { default: String, base_url: String }

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")))
        .init();
    let base = BaseDirs::new().ok_or_else(|| eyre::eyre!("no home"))?;
    let home = base.home_dir();
    let cfg_path = home.join(".evoclaw/config.toml");
    let cfg_text = tokio::fs::read_to_string(&cfg_path).await
        .wrap_err_with(|| format!("read {}; run `evo onboard` first", cfg_path.display()))?;
    let cfg: Config = toml::from_str(&cfg_text)?;
    let api_key = std::env::var("EVO_API_KEY")
        .map_err(|_| eyre::eyre!("EVO_API_KEY env var not set"))?;
    let bind = std::env::var("EVO_GATEWAY_BIND").unwrap_or_else(|_| "127.0.0.1:7878".into());
    let allowlist: Vec<String> = std::env::var("EVO_GATEWAY_ALLOWLIST")
        .unwrap_or_else(|_| "dev".into())
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();

    let provider = Arc::new(OpenAiCompatProvider::new(cfg.model.base_url.clone(), api_key, cfg.model.default.clone()));
    let registry = Arc::new(ToolRegistry::with_builtins());

    let mut gw_cfg = GatewayConfig::local_default(home);
    gw_cfg.bind = bind;
    gw_cfg.allowlist = allowlist;

    serve(gw_cfg, provider, registry, cfg.model.default).await
}
