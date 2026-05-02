//! Onboarding wizard — interactive provider picker + key entry.
//!
//! Closure rules:
//! - Key never appears in config.toml; only the **provider id** does.
//! - Key file lives at `~/.evoclaw/secrets/<provider>.key` (chmod 600 on Unix).
//! - Resolution order at runtime: `EVO_API_KEY` env -> secrets file -> error.
//! - `evoclaw doctor` always reports which source supplied the key.

use directories::BaseDirs;
use eyre::{Result, WrapErr};
use std::io::Write;
use std::path::{Path, PathBuf};

pub const PROVIDERS: &[ProviderProfile] = &[
    ProviderProfile {
        id: "deepseek",
        name: "DeepSeek",
        base_url: "https://api.deepseek.com/v1",
        default_model: "deepseek-chat",
        key_url: Some("https://platform.deepseek.com/api_keys"),
        fallback: &["deepseek-reasoner"],
        local: false,
    },
    ProviderProfile {
        id: "kimi",
        name: "Kimi (Moonshot)",
        base_url: "https://api.moonshot.cn/v1",
        default_model: "kimi-k2-0905-preview",
        key_url: Some("https://platform.moonshot.cn/console/api-keys"),
        fallback: &["moonshot-v1-32k"],
        local: false,
    },
    ProviderProfile {
        id: "qwen",
        name: "Qwen (DashScope)",
        base_url: "https://dashscope.aliyuncs.com/compatible-mode/v1",
        default_model: "qwen-plus",
        key_url: Some("https://bailian.console.aliyun.com/?apiKey=1"),
        fallback: &["qwen-turbo"],
        local: false,
    },
    ProviderProfile {
        id: "openai",
        name: "OpenAI",
        base_url: "https://api.openai.com/v1",
        default_model: "gpt-4o-mini",
        key_url: Some("https://platform.openai.com/api-keys"),
        fallback: &["gpt-4o"],
        local: false,
    },
    ProviderProfile {
        id: "anthropic",
        name: "Anthropic (native API)",
        base_url: "https://api.anthropic.com/v1",
        default_model: "claude-3-5-sonnet-20241022",
        key_url: Some("https://console.anthropic.com/settings/keys"),
        fallback: &["claude-3-5-haiku-20241022"],
        local: false,
    },
    ProviderProfile {
        id: "copilot",
        name: "GitHub Copilot (OAuth device flow)",
        base_url: "https://api.githubcopilot.com",
        default_model: "gpt-4o",
        key_url: None,
        fallback: &["claude-3.5-sonnet"],
        local: false,
    },
    ProviderProfile {
        id: "openrouter",
        name: "OpenRouter (multi-model)",
        base_url: "https://openrouter.ai/api/v1",
        default_model: "openai/gpt-4o-mini",
        key_url: Some("https://openrouter.ai/keys"),
        fallback: &["anthropic/claude-3.5-sonnet"],
        local: false,
    },
    ProviderProfile {
        id: "ollama",
        name: "Ollama (local)",
        base_url: "http://localhost:11434/v1",
        default_model: "llama3.1",
        key_url: None,
        fallback: &[],
        local: true,
    },
    ProviderProfile {
        id: "custom",
        name: "Custom OpenAI-compatible endpoint",
        base_url: "",
        default_model: "",
        key_url: None,
        fallback: &[],
        local: false,
    },
];

#[derive(Debug, Clone, Copy)]
pub struct ProviderProfile {
    pub id: &'static str,
    pub name: &'static str,
    pub base_url: &'static str,
    pub default_model: &'static str,
    pub key_url: Option<&'static str>,
    pub fallback: &'static [&'static str],
    pub local: bool,
}

pub fn find_provider(id: &str) -> Option<&'static ProviderProfile> {
    PROVIDERS.iter().find(|p| p.id == id)
}

fn home() -> Result<PathBuf> {
    Ok(BaseDirs::new()
        .ok_or_else(|| eyre::eyre!("cannot determine home dir"))?
        .home_dir()
        .to_path_buf())
}
pub fn evoclaw_dir() -> Result<PathBuf> { Ok(home()?.join(".evoclaw")) }
pub fn config_path() -> Result<PathBuf> { Ok(evoclaw_dir()?.join("config.toml")) }
pub fn secrets_dir() -> Result<PathBuf> { Ok(evoclaw_dir()?.join("secrets")) }
pub fn secret_file(provider_id: &str) -> Result<PathBuf> {
    Ok(secrets_dir()?.join(format!("{provider_id}.key")))
}

#[derive(Debug, Clone)]
pub enum KeySource {
    Env,
    SecretFile(PathBuf),
}

impl KeySource {
    pub fn describe(&self) -> String {
        match self {
            KeySource::Env => "EVO_API_KEY env var".into(),
            KeySource::SecretFile(p) => format!("secrets file: {}", p.display()),
        }
    }
}

/// Resolve API key. Priority: EVO_API_KEY env -> secrets file -> Err.
pub async fn resolve_api_key(provider_id: &str) -> Result<(String, KeySource)> {
    if let Ok(k) = std::env::var("EVO_API_KEY") {
        if !k.is_empty() {
            return Ok((k, KeySource::Env));
        }
    }
    let path = secret_file(provider_id)?;
    if path.exists() {
        let raw = tokio::fs::read_to_string(&path).await
            .wrap_err_with(|| format!("read {}", path.display()))?;
        let key = raw.trim().to_string();
        if !key.is_empty() {
            return Ok((key, KeySource::SecretFile(path)));
        }
    }
    Err(eyre::eyre!(
        "no API key found for provider '{provider_id}'.\n\
         Set EVO_API_KEY env var, or run `evoclaw login` / `evoclaw onboard`.",
    ))
}

#[derive(Debug, Clone)]
pub struct ProviderChoice {
    pub id: String,
    pub name: String,
    pub base_url: String,
    pub default_model: String,
    pub fallback: Vec<String>,
    pub key_url: Option<String>,
    pub local: bool,
}

pub async fn pick_provider() -> Result<ProviderChoice> {
    println!();
    println!("  Select a provider:");
    for (i, p) in PROVIDERS.iter().enumerate() {
        let local_tag = if p.local { "  [local]" } else { "" };
        println!("    {})  {}{}", i + 1, p.name, local_tag);
    }
    let acp_index = PROVIDERS.len() + 1;
    println!("    {})  External ACP agent (Claude / Codex / Cursor / Copilot)", acp_index);
    println!();
    print!("  > ");
    std::io::stdout().flush().ok();
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    let n: usize = line.trim().parse()
        .map_err(|_| eyre::eyre!("not a number; try again with `evoclaw onboard`"))?;
    if n == acp_index {
        return pick_acp_agent().await;
    }
    let profile = PROVIDERS.get(n.checked_sub(1).unwrap_or(usize::MAX))
        .ok_or_else(|| eyre::eyre!("choice {n} out of range"))?;
    if profile.id == "custom" {
        return prompt_custom();
    }
    Ok(ProviderChoice {
        id: profile.id.into(),
        name: profile.name.into(),
        base_url: profile.base_url.into(),
        default_model: profile.default_model.into(),
        fallback: profile.fallback.iter().map(|s| s.to_string()).collect(),
        key_url: profile.key_url.map(String::from),
        local: profile.local,
    })
}

/// ACP agent picker. Result has `id = "acp:<agent>"` so `run_one_shot`
/// dispatches via `AcpProvider::spawn` instead of fetching an API key.
/// Side-effect: writes `~/.evoclaw/agents/<agent>.toml`.
async fn pick_acp_agent() -> Result<ProviderChoice> {
    println!();
    println!("  Pick an external ACP agent (auth handled by the agent itself):");
    let catalog = evo_acp_client::CATALOG;
    for (i, a) in catalog.iter().enumerate() {
        println!("    {})  {:<10}  — {}", i + 1, a.id, a.name);
        println!("           install: {}", a.install_hint);
        println!("           auth   : {}", a.auth_hint);
    }
    println!();
    print!("  > ");
    std::io::stdout().flush().ok();
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    let m: usize = line.trim().parse()
        .map_err(|_| eyre::eyre!("not a number"))?;
    let prof = catalog.get(m.checked_sub(1).unwrap_or(usize::MAX))
        .ok_or_else(|| eyre::eyre!("choice {m} out of range"))?;
    let cfg = evo_acp_client::AgentConfig::from_profile(prof);
    let saved = evo_acp_client::save_agent(&cfg).await
        .map_err(|e| eyre::eyre!("save agent {}: {e}", prof.id))?;
    println!();
    println!("  ✓ saved agent profile -> {}", saved.display());
    println!("    Make sure '{}' is on PATH; install: {}", prof.bin, prof.install_hint);
    Ok(ProviderChoice {
        id: format!("acp:{}", prof.id),
        name: prof.name.into(),
        base_url: String::new(),
        default_model: format!("acp:{}", prof.id),
        fallback: Vec::new(),
        key_url: None,
        local: true,
    })
}

fn prompt_custom() -> Result<ProviderChoice> {
    let id = read_nonempty("provider id (kebab-case, e.g. my-llm)")?;
    let base_url = read_nonempty("base_url (e.g. https://example.com/v1)")?;
    let default_model = read_nonempty("default model (e.g. my-model)")?;
    Ok(ProviderChoice {
        id, name: "Custom".into(), base_url, default_model,
        fallback: vec![], key_url: None, local: false,
    })
}

fn read_nonempty(label: &str) -> Result<String> {
    print!("  {label}: ");
    std::io::stdout().flush().ok();
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    let s = line.trim().to_string();
    if s.is_empty() { return Err(eyre::eyre!("{label} cannot be empty")); }
    Ok(s)
}

/// Ask for the API key. For Copilot, runs OAuth device flow instead.
/// Optionally opens browser to provider's key page (paste-key flow).
pub async fn ask_api_key(profile: &ProviderChoice) -> Result<Option<String>> {
    if profile.local {
        println!();
        println!("  '{}' is local — no API key needed.", profile.name);
        println!("  Make sure the local server is running (e.g. `ollama serve`).");
        return Ok(None);
    }
    if profile.id == "copilot" {
        return run_copilot_oauth().await.map(Some);
    }
    if let Some(url) = &profile.key_url {
        println!();
        println!("  Get an API key at: {url}");
        print!("  Open this URL in your browser now? [y/N] ");
        std::io::stdout().flush().ok();
        let mut line = String::new();
        std::io::stdin().read_line(&mut line)?;
        if line.trim().eq_ignore_ascii_case("y") {
            try_open_browser(url);
            println!("  (browser opened — paste the key below when ready)");
        }
    }
    println!();
    print!("  Paste API key (will be saved to ~/.evoclaw/secrets/{}.key, chmod 600): ", profile.id);
    std::io::stdout().flush().ok();
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    let key = line.trim().to_string();
    if key.is_empty() {
        return Err(eyre::eyre!("empty key — aborting; run `evoclaw login` again"));
    }
    Ok(Some(key))
}

/// GitHub Copilot OAuth device flow.
async fn run_copilot_oauth() -> Result<String> {
    use evo_providers::copilot;
    let client = reqwest::Client::new();
    println!();
    println!("  GitHub Copilot uses OAuth Device Flow.");
    println!("  Requesting a device code...");
    let dc = copilot::request_device_code(&client).await
        .map_err(|e| eyre::eyre!("device code request failed: {e}"))?;
    println!();
    println!("  ┌──────────────────────────────────────────┐");
    println!("  │  Open this URL in your browser:          │");
    println!("  │    {}                       │", dc.verification_uri);
    println!("  │  Enter this code:                        │");
    println!("  │    {}                              │", dc.user_code);
    println!("  └──────────────────────────────────────────┘");
    print!("  Open the URL now? [Y/n] ");
    std::io::stdout().flush().ok();
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    if !line.trim().eq_ignore_ascii_case("n") {
        try_open_browser(&dc.verification_uri);
    }
    println!();
    println!("  Waiting for authorisation (timeout {}s)...", dc.expires_in.min(900));
    let token = copilot::poll_access_token(&client, &dc.device_code, dc.interval, dc.expires_in.min(900))
        .await
        .map_err(|e| eyre::eyre!("device flow failed: {e}"))?;
    println!("  ✓ authorised. ghu_* token received.");
    Ok(token)
}

fn try_open_browser(url: &str) {
    let cmd = if cfg!(target_os = "macos") { "open" }
              else if cfg!(target_os = "windows") { "cmd" }
              else { "xdg-open" };
    let args: Vec<&str> = if cfg!(target_os = "windows") {
        vec!["/C", "start", "", url]
    } else {
        vec![url]
    };
    let _ = std::process::Command::new(cmd).args(args).status();
}

pub async fn save_secret(provider_id: &str, key: &str) -> Result<PathBuf> {
    let dir = secrets_dir()?;
    tokio::fs::create_dir_all(&dir).await?;
    let path = dir.join(format!("{provider_id}.key"));
    tokio::fs::write(&path, key).await?;
    chmod_600(&path).await?;
    Ok(path)
}

#[cfg(unix)]
async fn chmod_600(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let perms = std::fs::Permissions::from_mode(0o600);
    tokio::fs::set_permissions(path, perms).await?;
    Ok(())
}

#[cfg(not(unix))]
async fn chmod_600(_path: &Path) -> Result<()> { Ok(()) }

pub async fn save_config(profile: &ProviderChoice) -> Result<PathBuf> {
    let path = config_path()?;
    let toml_text = render_config_toml(profile);
    tokio::fs::create_dir_all(evoclaw_dir()?).await?;
    tokio::fs::write(&path, toml_text).await?;
    Ok(path)
}

fn render_config_toml(p: &ProviderChoice) -> String {
    let fallback_arr = if p.fallback.is_empty() {
        "[]".into()
    } else {
        let inner: Vec<String> = p.fallback.iter().map(|s| format!("\"{s}\"")).collect();
        format!("[{}]", inner.join(", "))
    };
    format!(
        "[model]\n\
         provider = \"{id}\"\n\
         default  = \"{model}\"\n\
         base_url = \"{url}\"\n\
         fallback = {fb}\n\
         \n\
         [budget]\n\
         per_task_usd  = 0.5\n\
         per_day_usd   = 5.0\n\
         per_month_usd = 100.0\n\
         \n\
         [security]\n\
         default_permission  = \"P1\"\n\
         high_risk_intercept = true\n",
        id = p.id, model = p.default_model, url = p.base_url, fb = fallback_arr,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_has_at_least_5_providers() {
        assert!(PROVIDERS.len() >= 5);
        for p in PROVIDERS { assert!(!p.id.is_empty()); }
    }

    #[test]
    fn deepseek_is_default_first_entry() {
        assert_eq!(PROVIDERS[0].id, "deepseek");
    }

    #[test]
    fn local_providers_have_no_key_url() {
        for p in PROVIDERS {
            if p.local { assert!(p.key_url.is_none(), "{} should have no key_url", p.id); }
        }
    }

    #[test]
    fn render_config_includes_provider_marker() {
        let c = ProviderChoice {
            id: "deepseek".into(), name: "x".into(),
            base_url: "https://x.example/v1".into(),
            default_model: "deepseek-chat".into(),
            fallback: vec!["alt1".into()], key_url: None, local: false,
        };
        let toml = render_config_toml(&c);
        assert!(toml.contains("provider = \"deepseek\""));
        assert!(toml.contains("default  = \"deepseek-chat\""));
        assert!(toml.contains("\"alt1\""));
    }

    #[test]
    fn render_config_handles_empty_fallback() {
        let c = ProviderChoice {
            id: "x".into(), name: "x".into(),
            base_url: "u".into(), default_model: "m".into(),
            fallback: vec![], key_url: None, local: true,
        };
        let toml = render_config_toml(&c);
        assert!(toml.contains("fallback = []"));
    }

    #[test]
    fn find_provider_by_id() {
        assert_eq!(find_provider("deepseek").unwrap().id, "deepseek");
        assert!(find_provider("nonexistent").is_none());
    }
}
