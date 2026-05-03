//! evo-gateway binary.

use directories::BaseDirs;
use evo_gateway::{serve, GatewayConfig, DEFAULT_MAX_CONCURRENT};
use evo_providers::{AuthMethod, BrowserProfile, OpenAiCompatProvider};
use evo_tools::ToolRegistry;
use eyre::{Result, WrapErr};
use serde::Deserialize;
use std::sync::Arc;

#[derive(Debug, Deserialize)]
struct Config {
    model: ModelCfg,
    #[serde(default)]
    auth: AuthCfg,
}

#[derive(Debug, Deserialize)]
struct ModelCfg {
    #[serde(default)]
    provider: Option<String>,
    default: String,
    base_url: String,
}

#[derive(Debug, Default, Deserialize)]
struct AuthCfg {
    #[serde(default = "default_auth_method")]
    method: String,
}

fn default_auth_method() -> String {
    AuthMethod::ApiKey.as_str().to_string()
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();
    let base = BaseDirs::new().ok_or_else(|| eyre::eyre!("no home"))?;
    let home = base.home_dir();
    let cfg_path = home.join(".evoclaw/config.toml");
    let cfg_text = tokio::fs::read_to_string(&cfg_path)
        .await
        .wrap_err_with(|| format!("read {}; run `evo onboard` first", cfg_path.display()))?;
    let cfg: Config = toml::from_str(&cfg_text)?;
    let identity_summary = load_identity_summary(home, &cfg).await;
    let api_key =
        std::env::var("EVO_API_KEY").map_err(|_| eyre::eyre!("EVO_API_KEY env var not set"))?;
    let bind = std::env::var("EVO_GATEWAY_BIND").unwrap_or_else(|_| "127.0.0.1:7878".into());
    let allowlist: Vec<String> = std::env::var("EVO_GATEWAY_ALLOWLIST")
        .unwrap_or_else(|_| "dev".into())
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    let max_concurrent: usize = std::env::var("EVO_GATEWAY_MAX_CONCURRENT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_MAX_CONCURRENT);

    let provider = Arc::new(OpenAiCompatProvider::new(
        cfg.model.base_url.clone(),
        api_key,
        cfg.model.default.clone(),
    ));

    // TODO(Fix 9 / consolidate): install MCP tools into the gateway registry.
    // The canonical implementation lives in `evo_cli::mcp_tools::install_all`
    // (see crates/evo-cli/src/mcp_tools.rs). Surfacing it cross-crate would
    // require adding `evo-cli` (or replicating `mcp_tools` plus pulling in
    // `evo-mcp-client`) as a Cargo dependency of `evo-gateway`, which is out
    // of scope for this hardening pass — the security review constraints
    // fence edits to source files only. Until that consolidation lands, the
    // gateway ships with `ToolRegistry::with_builtins()` exactly as before.
    // The critical half of Fix 9 — wiring the vault-backed `Redactor` into
    // every WebChat task — IS done in `evo_gateway::lib::handle_chat` so
    // unredacted secrets never reach the model.
    let registry = Arc::new(ToolRegistry::with_builtins());

    let mut gw_cfg = GatewayConfig::local_default(home);
    gw_cfg.bind = bind;
    gw_cfg.allowlist = allowlist;
    gw_cfg.max_concurrent = max_concurrent;

    serve(
        gw_cfg,
        provider,
        registry,
        cfg.model.default,
        identity_summary,
    )
    .await
}

async fn load_identity_summary(home: &std::path::Path, cfg: &Config) -> String {
    let provider_id = cfg
        .model
        .provider
        .clone()
        .unwrap_or_else(|| "unknown".into());
    match AuthMethod::parse(&cfg.auth.method).unwrap_or(AuthMethod::ApiKey) {
        AuthMethod::Browser => {
            let path = home
                .join(".evoclaw")
                .join("browser_profiles")
                .join(format!("{provider_id}.json"));
            match tokio::fs::read_to_string(&path).await {
                Ok(raw) => match serde_json::from_str::<BrowserProfile>(&raw) {
                    Ok(profile) => match profile.account_label {
                        Some(account) if !account.trim().is_empty() => {
                            format!("{provider_id} · {account}")
                        }
                        _ => format!("{provider_id} · browser session"),
                    },
                    Err(_) => format!("{provider_id} · browser session"),
                },
                Err(_) => format!("{provider_id} · browser session"),
            }
        }
        AuthMethod::Acp => format!("{provider_id} · external agent"),
        AuthMethod::ApiKey => format!("{provider_id} · API key auth"),
    }
}
