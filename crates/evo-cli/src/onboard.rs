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
    // ---- Chinese vendors (国内厂商) -------------------------------------
    ProviderProfile {
        id: "deepseek",
        name: "DeepSeek (深度求索)",
        base_url: "https://api.deepseek.com/v1",
        default_model: "deepseek-chat",
        key_url: Some("https://platform.deepseek.com/api_keys"),
        fallback: &["deepseek-reasoner"],
        local: false,
    },
    ProviderProfile {
        id: "kimi",
        name: "Kimi · Moonshot (月之暗面)",
        base_url: "https://api.moonshot.cn/v1",
        default_model: "kimi-k2-0905-preview",
        key_url: Some("https://platform.moonshot.cn/console/api-keys"),
        fallback: &["moonshot-v1-32k", "moonshot-v1-128k"],
        local: false,
    },
    ProviderProfile {
        id: "qwen",
        name: "Qwen · DashScope (阿里通义千问)",
        base_url: "https://dashscope.aliyuncs.com/compatible-mode/v1",
        default_model: "qwen-plus",
        key_url: Some("https://bailian.console.aliyun.com/?apiKey=1"),
        fallback: &["qwen-turbo", "qwen-max"],
        local: false,
    },
    ProviderProfile {
        id: "doubao",
        name: "Doubao · Volcengine (字节豆包)",
        base_url: "https://ark.cn-beijing.volces.com/api/v3",
        default_model: "doubao-seed-1-6-250615",
        key_url: Some("https://console.volcengine.com/ark/region:ark+cn-beijing/apiKey"),
        fallback: &["doubao-1-5-pro-32k-250115"],
        local: false,
    },
    ProviderProfile {
        id: "zhipu",
        name: "Zhipu GLM (智谱)",
        base_url: "https://open.bigmodel.cn/api/paas/v4",
        default_model: "glm-4-plus",
        key_url: Some("https://open.bigmodel.cn/usercenter/apikeys"),
        fallback: &["glm-4-flash", "glm-4-air"],
        local: false,
    },
    ProviderProfile {
        id: "baidu",
        name: "Baidu Qianfan (百度千帆 / 文心一言)",
        base_url: "https://qianfan.baidubce.com/v2",
        default_model: "ernie-4.5-turbo-128k",
        key_url: Some("https://console.bce.baidu.com/iam/#/iam/apikey/list"),
        fallback: &["ernie-4.0-8k", "ernie-speed-128k"],
        local: false,
    },
    ProviderProfile {
        id: "minimax",
        name: "MiniMax (海螺 AI)",
        base_url: "https://api.minimax.chat/v1",
        default_model: "MiniMax-Text-01",
        key_url: Some("https://www.minimaxi.com/user-center/basic-information/interface-key"),
        fallback: &["abab6.5s-chat"],
        local: false,
    },
    ProviderProfile {
        id: "stepfun",
        name: "StepFun (阶跃星辰)",
        base_url: "https://api.stepfun.com/v1",
        default_model: "step-2-16k",
        key_url: Some("https://platform.stepfun.com/interface-key"),
        fallback: &["step-1-flash", "step-1-32k"],
        local: false,
    },
    ProviderProfile {
        id: "tencent",
        name: "Tencent Hunyuan (腾讯混元)",
        base_url: "https://api.hunyuan.cloud.tencent.com/v1",
        default_model: "hunyuan-turbos-latest",
        key_url: Some("https://console.cloud.tencent.com/hunyuan/api-key"),
        fallback: &["hunyuan-large", "hunyuan-standard"],
        local: false,
    },
    // ---- International vendors (国际厂商) -------------------------------
    ProviderProfile {
        id: "openai",
        name: "OpenAI",
        base_url: "https://api.openai.com/v1",
        default_model: "gpt-4o-mini",
        key_url: Some("https://platform.openai.com/api-keys"),
        fallback: &["gpt-4o", "gpt-4-turbo"],
        local: false,
    },
    ProviderProfile {
        id: "anthropic",
        name: "Anthropic (Claude native API)",
        base_url: "https://api.anthropic.com/v1",
        default_model: "claude-3-5-sonnet-20241022",
        key_url: Some("https://console.anthropic.com/settings/keys"),
        fallback: &["claude-3-5-haiku-20241022"],
        local: false,
    },
    ProviderProfile {
        id: "gemini",
        name: "Google Gemini",
        base_url: "https://generativelanguage.googleapis.com/v1beta/openai",
        default_model: "gemini-2.0-flash",
        key_url: Some("https://aistudio.google.com/app/apikey"),
        fallback: &["gemini-1.5-pro", "gemini-1.5-flash"],
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
        id: "mistral",
        name: "Mistral AI",
        base_url: "https://api.mistral.ai/v1",
        default_model: "mistral-large-latest",
        key_url: Some("https://console.mistral.ai/api-keys/"),
        fallback: &["mistral-small-latest", "codestral-latest"],
        local: false,
    },
    ProviderProfile {
        id: "groq",
        name: "Groq (LPU inference)",
        base_url: "https://api.groq.com/openai/v1",
        default_model: "llama-3.3-70b-versatile",
        key_url: Some("https://console.groq.com/keys"),
        fallback: &["llama-3.1-8b-instant"],
        local: false,
    },
    ProviderProfile {
        id: "together",
        name: "Together AI",
        base_url: "https://api.together.xyz/v1",
        default_model: "meta-llama/Meta-Llama-3.1-70B-Instruct-Turbo",
        key_url: Some("https://api.together.xyz/settings/api-keys"),
        fallback: &["Qwen/Qwen2.5-72B-Instruct-Turbo"],
        local: false,
    },
    ProviderProfile {
        id: "fireworks",
        name: "Fireworks AI",
        base_url: "https://api.fireworks.ai/inference/v1",
        default_model: "accounts/fireworks/models/llama-v3p3-70b-instruct",
        key_url: Some("https://fireworks.ai/api-keys"),
        fallback: &["accounts/fireworks/models/qwen2p5-72b-instruct"],
        local: false,
    },
    ProviderProfile {
        id: "openrouter",
        name: "OpenRouter (multi-model gateway)",
        base_url: "https://openrouter.ai/api/v1",
        default_model: "openai/gpt-4o-mini",
        key_url: Some("https://openrouter.ai/keys"),
        fallback: &["anthropic/claude-3.5-sonnet", "deepseek/deepseek-chat"],
        local: false,
    },
    // ---- Local / self-hosted (本地 / 自建) ------------------------------
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
        id: "vllm",
        name: "vLLM (local)",
        base_url: "http://localhost:8000/v1",
        default_model: "Qwen/Qwen2.5-7B-Instruct",
        key_url: None,
        fallback: &[],
        local: true,
    },
    ProviderProfile {
        id: "llamacpp",
        name: "llama.cpp server (local)",
        base_url: "http://localhost:8080/v1",
        default_model: "default",
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
pub fn evoclaw_dir() -> Result<PathBuf> {
    Ok(home()?.join(".evoclaw"))
}
pub fn config_path() -> Result<PathBuf> {
    Ok(evoclaw_dir()?.join("config.toml"))
}
pub fn secrets_dir() -> Result<PathBuf> {
    Ok(evoclaw_dir()?.join("secrets"))
}
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
        let raw = tokio::fs::read_to_string(&path)
            .await
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
    println!(
        "    {})  External ACP agent (Claude / Codex / Cursor / Copilot)",
        acp_index
    );
    println!();
    print!("  > ");
    std::io::stdout().flush().ok();
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    let n: usize = line
        .trim()
        .parse()
        .map_err(|_| eyre::eyre!("not a number; try again with `evoclaw onboard`"))?;
    if n == acp_index {
        return pick_acp_agent().await;
    }
    let profile = PROVIDERS
        .get(n.checked_sub(1).unwrap_or(usize::MAX))
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
    let m: usize = line
        .trim()
        .parse()
        .map_err(|_| eyre::eyre!("not a number"))?;
    let prof = catalog
        .get(m.checked_sub(1).unwrap_or(usize::MAX))
        .ok_or_else(|| eyre::eyre!("choice {m} out of range"))?;
    let cfg = evo_acp_client::AgentConfig::from_profile(prof);
    let saved = evo_acp_client::save_agent(&cfg)
        .await
        .map_err(|e| eyre::eyre!("save agent {}: {e}", prof.id))?;
    println!();
    println!("  ✓ saved agent profile -> {}", saved.display());
    println!(
        "    Make sure '{}' is on PATH; install: {}",
        prof.bin, prof.install_hint
    );
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
        id,
        name: "Custom".into(),
        base_url,
        default_model,
        fallback: vec![],
        key_url: None,
        local: false,
    })
}

fn read_nonempty(label: &str) -> Result<String> {
    print!("  {label}: ");
    std::io::stdout().flush().ok();
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    let s = line.trim().to_string();
    if s.is_empty() {
        return Err(eyre::eyre!("{label} cannot be empty"));
    }
    Ok(s)
}

/// After the API key is captured, fetch `<base_url>/models` and let the user
/// pick one of the models the key actually entitles them to.
///
/// - Local providers (Ollama / vLLM / llama.cpp) skip the network probe and
///   keep `profile.default_model`.
/// - ACP providers (`acp:*`) skip — model is irrelevant; the upstream CLI
///   manages that itself.
/// - On any error (no network, 401, parsing) we print one line and keep the
///   catalog default. This step is **best-effort**: never fail the wizard.
pub async fn pick_model(profile: &mut ProviderChoice, api_key: Option<&str>) -> Result<()> {
    if profile.local || profile.id.starts_with("acp:") || profile.base_url.is_empty() {
        return Ok(());
    }
    let url = format!("{}/models", profile.base_url.trim_end_matches('/'));
    let mut req = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|e| eyre::eyre!("build http client: {e}"))?
        .get(&url);
    if let Some(k) = api_key {
        req = req.bearer_auth(k);
    }
    println!();
    println!("  fetching available models from {url} …");
    let models = match req.send().await {
        Ok(resp) if resp.status().is_success() => match resp.json::<ModelList>().await {
            Ok(list) => list.data,
            Err(e) => {
                eprintln!(
                    "  (could not parse /models response: {e}; keeping default '{}')",
                    profile.default_model
                );
                return Ok(());
            }
        },
        Ok(resp) => {
            eprintln!(
                "  (provider returned HTTP {}; keeping default '{}')",
                resp.status(),
                profile.default_model
            );
            return Ok(());
        }
        Err(e) => {
            eprintln!(
                "  (could not reach {url}: {e}; keeping default '{}')",
                profile.default_model
            );
            return Ok(());
        }
    };
    if models.is_empty() {
        eprintln!(
            "  (provider returned 0 models; keeping default '{}')",
            profile.default_model
        );
        return Ok(());
    }
    println!();
    println!(
        "  Available models ({} total). Type a number, or press Enter for the default.",
        models.len()
    );
    let preview: Vec<&ModelEntry> = models.iter().take(30).collect();
    for (i, m) in preview.iter().enumerate() {
        let marker = if m.id == profile.default_model {
            " (default)"
        } else {
            ""
        };
        println!("    {:>2})  {}{}", i + 1, m.id, marker);
    }
    if models.len() > preview.len() {
        println!(
            "    … ({} more models hidden — type the model id directly to pick one)",
            models.len() - preview.len()
        );
    }
    println!();
    print!("  model [{}]: ", profile.default_model);
    std::io::stdout().flush().ok();
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    let line = line.trim();
    if line.is_empty() {
        println!("  → keeping default '{}'", profile.default_model);
        return Ok(());
    }
    if let Ok(n) = line.parse::<usize>() {
        if let Some(m) = preview.get(n.checked_sub(1).unwrap_or(usize::MAX)) {
            profile.default_model = m.id.clone();
            println!("  → selected '{}'", profile.default_model);
            return Ok(());
        }
    }
    // user typed a model id directly
    if models.iter().any(|m| m.id == line) {
        profile.default_model = line.to_string();
        println!("  → selected '{}'", profile.default_model);
    } else {
        println!(
            "  (no exact match for '{line}'; keeping default '{}')",
            profile.default_model
        );
    }
    Ok(())
}

#[derive(Debug, serde::Deserialize)]
struct ModelList {
    #[serde(default)]
    data: Vec<ModelEntry>,
}

#[derive(Debug, serde::Deserialize)]
struct ModelEntry {
    id: String,
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
    print!(
        "  Paste API key (will be saved to ~/.evoclaw/secrets/{}.key, chmod 600): ",
        profile.id
    );
    std::io::stdout().flush().ok();
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    let key = line.trim().to_string();
    if key.is_empty() {
        return Err(eyre::eyre!(
            "empty key — aborting; run `evoclaw login` again"
        ));
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
    let dc = copilot::request_device_code(&client)
        .await
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
    println!(
        "  Waiting for authorisation (timeout {}s)...",
        dc.expires_in.min(900)
    );
    let token = copilot::poll_access_token(
        &client,
        &dc.device_code,
        dc.interval,
        dc.expires_in.min(900),
    )
    .await
    .map_err(|e| eyre::eyre!("device flow failed: {e}"))?;
    println!("  ✓ authorised. ghu_* token received.");
    Ok(token)
}

fn try_open_browser(url: &str) {
    let cmd = if cfg!(target_os = "macos") {
        "open"
    } else if cfg!(target_os = "windows") {
        "cmd"
    } else {
        "xdg-open"
    };
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
async fn chmod_600(_path: &Path) -> Result<()> {
    Ok(())
}

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
        id = p.id,
        model = p.default_model,
        url = p.base_url,
        fb = fallback_arr,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_has_at_least_5_providers() {
        assert!(PROVIDERS.len() >= 5);
        for p in PROVIDERS {
            assert!(!p.id.is_empty());
        }
    }

    #[test]
    fn deepseek_is_default_first_entry() {
        assert_eq!(PROVIDERS[0].id, "deepseek");
    }

    #[test]
    fn local_providers_have_no_key_url() {
        for p in PROVIDERS {
            if p.local {
                assert!(p.key_url.is_none(), "{} should have no key_url", p.id);
            }
        }
    }

    #[test]
    fn render_config_includes_provider_marker() {
        let c = ProviderChoice {
            id: "deepseek".into(),
            name: "x".into(),
            base_url: "https://x.example/v1".into(),
            default_model: "deepseek-chat".into(),
            fallback: vec!["alt1".into()],
            key_url: None,
            local: false,
        };
        let toml = render_config_toml(&c);
        assert!(toml.contains("provider = \"deepseek\""));
        assert!(toml.contains("default  = \"deepseek-chat\""));
        assert!(toml.contains("\"alt1\""));
    }

    #[test]
    fn render_config_handles_empty_fallback() {
        let c = ProviderChoice {
            id: "x".into(),
            name: "x".into(),
            base_url: "u".into(),
            default_model: "m".into(),
            fallback: vec![],
            key_url: None,
            local: true,
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
