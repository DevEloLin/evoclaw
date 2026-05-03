//! Onboarding wizard — interactive provider picker + auth-method picker.
//!
//! Closure rules:
//! - Key never appears in config.toml; only the **provider id** does.
//! - Key file lives at `~/.evoclaw/secrets/<provider>.key` (chmod 600 on Unix).
//! - Browser-session profile lives at `~/.evoclaw/browser_profiles/<provider>.json`
//!   (chmod 600). Format defined by `evo_providers::BrowserProfile`.
//! - Resolution order at runtime: `EVO_API_KEY` env -> secrets file -> error.
//! - `evoclaw doctor` always reports which source supplied the credential.
//!
//! Auth-method priority shown to the user in the shell entry:
//!   1) API key (preferred — simplest, works for every vendor in the catalog)
//!   2) Browser sign-in (capture session cookie / web token from the browser)
//!   3) ACP agent (TEMPORARILY NOT SUPPORTED — most upstream CLIs don't
//!      implement Zed-ACP natively; gated off until that situation matures)

use directories::BaseDirs;
use evo_providers::{AuthMethod, BrowserAuthShape, BrowserProfile};
use eyre::{Result, WrapErr};
use std::io::Write;
use std::path::{Path, PathBuf};

pub const PROVIDERS: &[ProviderProfile] = &[
    // ---- Top 5 by global popularity (按全球知名度和热度排序) --------
    ProviderProfile {
        id: "openai",
        name: "OpenAI (ChatGPT)",
        base_url: "https://api.openai.com/v1",
        default_model: "gpt-4o-mini",
        key_url: Some("https://platform.openai.com/api-keys"),
        fallback: &["gpt-4o", "gpt-4-turbo"],
        local: false,
    },
    ProviderProfile {
        id: "anthropic",
        name: "Anthropic (Claude)",
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
        id: "deepseek",
        name: "DeepSeek (深度求索)",
        base_url: "https://api.deepseek.com/v1",
        default_model: "deepseek-chat",
        key_url: Some("https://platform.deepseek.com/api_keys"),
        fallback: &["deepseek-reasoner"],
        local: false,
    },
    ProviderProfile {
        id: "copilot",
        name: "GitHub Copilot",
        base_url: "https://api.githubcopilot.com",
        default_model: "gpt-4o",
        key_url: None,
        fallback: &["claude-3.5-sonnet"],
        local: false,
    },
    // ---- Other Chinese vendors (其他国内厂商) -----------------------
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
    // ---- Other International vendors (其他国际厂商) -----------------
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

/// Map a provider ID to its corresponding ACP agent ID (if any).
///
/// This allows users to select ACP mode when choosing a provider that has
/// a corresponding ACP agent available.
///
/// Mappings:
/// - anthropic -> claude
/// - openai -> codex
/// - gemini/google -> gemini
/// - copilot -> copilot
/// - qwen -> qwen-code
/// - cursor -> cursor
pub fn provider_to_acp_agent(provider_id: &str) -> Option<&'static str> {
    match provider_id {
        "anthropic" => Some("claude"),
        "openai" => Some("codex"),
        "gemini" | "google" => Some("gemini"),
        "copilot" => Some("copilot"),
        "qwen" => Some("qwen-code"),
        "cursor" => Some("cursor"),
        _ => None,
    }
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
pub fn browser_profiles_dir() -> Result<PathBuf> {
    Ok(evoclaw_dir()?.join("browser_profiles"))
}
pub fn browser_profile_path(provider_id: &str) -> Result<PathBuf> {
    Ok(browser_profiles_dir()?.join(format!("{provider_id}.json")))
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
    const QUICK_PICK_COUNT: usize = 5;

    loop {
        println!();
        println!("  Select a provider:");

        // Show first 5 providers
        for (i, p) in PROVIDERS.iter().take(QUICK_PICK_COUNT).enumerate() {
            let local_tag = if p.local { "  [local]" } else { "" };
            println!("    {})  {}{}", i + 1, p.name, local_tag);
        }

        // More option
        let more_index = QUICK_PICK_COUNT + 1;
        println!("    {})  More providers...", more_index);

        // ACP agent option
        let acp_index = more_index + 1;
        println!(
            "    {})  External ACP agent (Claude / Codex / Cursor / Copilot)",
            acp_index
        );

        // Cancel option
        println!("    0)  Cancel / Go back");
        println!();
        print!("  > ");
        std::io::stdout().flush().ok();

        let mut line = String::new();
        std::io::stdin().read_line(&mut line)?;
        let input = line.trim();

        // Handle ESC or cancel
        if input == "0" || input.to_lowercase() == "cancel" || input.to_lowercase() == "esc" {
            return Err(eyre::eyre!("Provider selection cancelled by user"));
        }

        let n: usize = match input.parse() {
            Ok(n) => n,
            Err(_) => {
                println!("  Invalid input. Please enter a number.");
                continue;
            }
        };

        // Handle cancel
        if n == 0 {
            return Err(eyre::eyre!("Provider selection cancelled by user"));
        }

        // Handle More option
        if n == more_index {
            return Box::pin(pick_provider_full_list()).await;
        }

        // Handle ACP agent
        if n == acp_index {
            return pick_acp_agent().await;
        }

        // Handle quick pick (1-5)
        if (1..=QUICK_PICK_COUNT).contains(&n) {
            let profile = &PROVIDERS[n - 1];
            if profile.id == "custom" {
                return prompt_custom();
            }
            return Ok(ProviderChoice {
                id: profile.id.into(),
                name: profile.name.into(),
                base_url: profile.base_url.into(),
                default_model: profile.default_model.into(),
                fallback: profile.fallback.iter().map(|s| s.to_string()).collect(),
                key_url: profile.key_url.map(String::from),
                local: profile.local,
            });
        }

        println!("  Choice {} out of range. Please try again.", n);
    }
}

/// Full provider list when user selects "More providers..."
async fn pick_provider_full_list() -> Result<ProviderChoice> {
    loop {
        println!();
        println!("  All providers:");
        for (i, p) in PROVIDERS.iter().enumerate() {
            let local_tag = if p.local { "  [local]" } else { "" };
            println!("    {:>2})  {}{}", i + 1, p.name, local_tag);
        }
        println!("     0)  Cancel / Go back");
        println!();
        print!("  > ");
        std::io::stdout().flush().ok();

        let mut line = String::new();
        std::io::stdin().read_line(&mut line)?;
        let input = line.trim();

        // Handle ESC or cancel
        if input == "0" || input.to_lowercase() == "cancel" || input.to_lowercase() == "esc" {
            // Go back to quick pick
            return Box::pin(pick_provider()).await;
        }

        let n: usize = match input.parse() {
            Ok(n) => n,
            Err(_) => {
                println!("  Invalid input. Please enter a number.");
                continue;
            }
        };

        if n == 0 {
            return Box::pin(pick_provider()).await;
        }

        if let Some(profile) = PROVIDERS.get(n - 1) {
            if profile.id == "custom" {
                return prompt_custom();
            }
            return Ok(ProviderChoice {
                id: profile.id.into(),
                name: profile.name.into(),
                base_url: profile.base_url.into(),
                default_model: profile.default_model.into(),
                fallback: profile.fallback.iter().map(|s| s.to_string()).collect(),
                key_url: profile.key_url.map(String::from),
                local: profile.local,
            });
        }

        println!("  Choice {} out of range. Please try again.", n);
    }
}

/// ACP agent picker. Result has `id = "acp:<agent>"` so `run_one_shot`
/// dispatches via `AcpProvider::spawn` instead of fetching an API key.
/// Side-effect: writes `~/.evoclaw/agents/<agent>.toml`.
async fn pick_acp_agent() -> Result<ProviderChoice> {
    loop {
        println!();
        println!("  Pick an external ACP agent (auth handled by the agent itself):");
        let catalog = evo_acp_client::catalog();
        for (i, a) in catalog.iter().enumerate() {
            let badge = if a.acp_native { "[native]" } else { "[shim]  " };
            println!("    {}) {} {:<10}  — {}", i + 1, badge, a.id, a.name);
            println!("           install: {}", a.install_hint);
            println!("           auth   : {}", a.auth_hint);
        }
        println!("    0)  Cancel / Go back");
        println!();
        print!("  > ");
        std::io::stdout().flush().ok();

        let mut line = String::new();
        std::io::stdin().read_line(&mut line)?;
        let input = line.trim();

        // Handle ESC or cancel - go back to provider selection
        if input == "0" || input.to_lowercase() == "cancel" || input.to_lowercase() == "esc" {
            return Box::pin(pick_provider()).await;
        }

        let m: usize = match input.parse() {
            Ok(n) => n,
            Err(_) => {
                println!("  Invalid input. Please enter a number.");
                continue;
            }
        };

        if m == 0 {
            return Box::pin(pick_provider()).await;
        }

        if let Some(prof) = catalog.get(m - 1) {
            let cfg = evo_acp_client::AgentConfig::from_profile(prof);
            let saved = evo_acp_client::save_agent(&cfg)
                .await
                .map_err(|e| eyre::eyre!("save agent {}: {e}", prof.id))?;
            println!();
            println!("  ✓ saved agent profile -> {}", saved.display());
            println!(
                "    Resolved command: '{} {}'",
                cfg.command,
                cfg.args.join(" ")
            );
            println!("    install: {}", prof.install_hint);
            return Ok(ProviderChoice {
                id: format!("acp:{}", prof.id),
                name: prof.name.clone(),
                base_url: String::new(),
                default_model: format!("acp:{}", prof.id),
                fallback: Vec::new(),
                key_url: None,
                local: true,
            });
        }

        println!("  Choice {} out of range. Please try again.", m);
    }
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
    save_config_with_auth(profile, AuthMethod::ApiKey).await
}

/// Persist `config.toml` with the selected auth method recorded under `[auth]`.
/// Older callers that only know about API-key auth keep working via
/// `save_config` (above), which forwards `AuthMethod::ApiKey`.
pub async fn save_config_with_auth(profile: &ProviderChoice, auth: AuthMethod) -> Result<PathBuf> {
    let path = config_path()?;
    let toml_text = render_config_toml_with_auth(profile, auth);
    tokio::fs::create_dir_all(evoclaw_dir()?).await?;
    tokio::fs::write(&path, toml_text).await?;
    Ok(path)
}

fn render_config_toml_with_auth(p: &ProviderChoice, auth: AuthMethod) -> String {
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
         [auth]\n\
         method = \"{auth}\"\n\
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
        auth = auth.as_str(),
    )
}

// ---------------------------------------------------------------------------
// Auth-method picker (PRD §44 — shell entry rework)
// ---------------------------------------------------------------------------

/// Show the auth-method picker (2 or 3 options depending on provider).
///
/// Priority and labels:
///   1) API key                  ← preferred (returns `AuthMethod::ApiKey`)
///   2) Browser sign-in          ← `AuthMethod::Browser`
///   3) ACP agent                ← only shown if provider has a corresponding ACP agent
///
/// If the provider has a corresponding ACP agent (e.g., anthropic -> claude,
/// openai -> codex), we show option 3 and return `AuthMethod::Acp`.
///
/// Local providers (Ollama / vLLM / llama.cpp) have no auth at all — we
/// short-circuit to `ApiKey` (which then becomes a no-op in `ask_api_key`).
pub fn pick_auth_method(profile: &ProviderChoice) -> Result<AuthMethod> {
    if profile.local {
        return Ok(AuthMethod::ApiKey);
    }

    // Check if this provider has a corresponding ACP agent
    let acp_agent_id = provider_to_acp_agent(&profile.id);
    let has_acp = acp_agent_id.is_some();

    loop {
        println!();
        println!(
            "  How would you like to authenticate with {}?",
            profile.name
        );
        println!("    1)  API key                       (preferred · simplest)");
        println!("    2)  Browser sign-in               (paste session token from your browser)");

        if has_acp {
            let agent_name = acp_agent_id.unwrap();
            // Find the full agent name from catalog
            let agent_display = evo_acp_client::find_agent(agent_name)
                .map(|a| a.name.as_str())
                .unwrap_or(agent_name);
            println!("    3)  ACP agent                     ({})", agent_display);
        }

        println!("    0)  Cancel / Go back");
        print!("  > ");
        std::io::stdout().flush().ok();

        let mut line = String::new();
        std::io::stdin().read_line(&mut line)?;
        let input = line.trim();

        // Handle ESC or cancel
        if input == "0" || input.to_lowercase() == "cancel" || input.to_lowercase() == "esc" {
            return Err(eyre::eyre!(
                "Authentication method selection cancelled by user"
            ));
        }

        match input {
            "" | "1" | "api_key" | "apikey" | "key" => return Ok(AuthMethod::ApiKey),
            "2" | "browser" | "web" | "cookie" => return Ok(AuthMethod::Browser),
            "3" | "acp" | "agent" if has_acp => return Ok(AuthMethod::Acp),
            "0" => {
                return Err(eyre::eyre!(
                    "Authentication method selection cancelled by user"
                ))
            }
            other => {
                if has_acp {
                    println!("  unrecognised choice '{other}', try 1 / 2 / 3 / 0");
                } else {
                    println!("  unrecognised choice '{other}', try 1 / 2 / 0");
                }
                continue;
            }
        }
    }
}

/// Browser-sign-in capture flow. Open the vendor's web console, ask the user
/// to paste their captured session token, and persist a `BrowserProfile`.
///
/// The shape is inferred from the vendor: Anthropic uses `x-api-key`, every
/// other vendor in our catalog speaks OpenAI-compat which accepts a
/// `Authorization: Bearer …` token. Cookie-string mode is reachable for power
/// users via `BrowserProfile::shape = "cookie"` directly in the JSON file.
pub async fn capture_browser_profile(profile: &ProviderChoice) -> Result<BrowserProfile> {
    let shape = match profile.id.as_str() {
        "anthropic" => BrowserAuthShape::AnthropicHeader,
        _ => BrowserAuthShape::Bearer,
    };
    println!();
    println!("  ─── Browser sign-in for {} ───", profile.name);
    if let Some(url) = &profile.key_url {
        println!("  1) Open the vendor's web console:    {url}");
    } else {
        println!("  1) Open the vendor's web console.");
    }
    println!("  2) Sign in normally (Google / GitHub / SSO / TOTP).");
    println!("  3) Capture your session token:");
    println!("       · most vendors: DevTools → Network → copy `Authorization` header");
    println!("       · cookie-based: DevTools → Application → Cookies → copy session value");
    println!("       · vendor SDKs:  click \"copy access token\" if present");
    if let Some(url) = &profile.key_url {
        print!("  Open {url} now in your browser? [Y/n] ");
        std::io::stdout().flush().ok();
        let mut line = String::new();
        std::io::stdin().read_line(&mut line)?;
        if !line.trim().eq_ignore_ascii_case("n") {
            try_open_browser(url);
        }
    }
    println!();
    print!(
        "  Paste session token (will be saved to ~/.evoclaw/browser_profiles/{}.json, chmod 600): ",
        profile.id
    );
    std::io::stdout().flush().ok();
    let mut tok_line = String::new();
    std::io::stdin().read_line(&mut tok_line)?;
    let token = tok_line.trim().to_string();
    if token.is_empty() {
        return Err(eyre::eyre!(
            "empty session token — aborting; run `evoclaw login` again"
        ));
    }
    print!("  Optional source-hint (e.g. \"DevTools cookie\", press Enter to skip): ");
    std::io::stdout().flush().ok();
    let mut hint_line = String::new();
    std::io::stdin().read_line(&mut hint_line)?;
    let hint = hint_line.trim().to_string();
    print!("  Optional account label (e.g. email / handle, press Enter to skip): ");
    std::io::stdout().flush().ok();
    let mut account_line = String::new();
    std::io::stdin().read_line(&mut account_line)?;
    let account_label = account_line.trim().to_string();
    let captured_at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    Ok(BrowserProfile {
        provider_id: profile.id.clone(),
        base_url: profile.base_url.clone(),
        default_model: profile.default_model.clone(),
        session_token: token,
        shape: shape.into(),
        source_hint: if hint.is_empty() { None } else { Some(hint) },
        account_label: if account_label.is_empty() {
            None
        } else {
            Some(account_label)
        },
        captured_at,
    })
}

pub async fn save_browser_profile(profile: &BrowserProfile) -> Result<PathBuf> {
    let dir = browser_profiles_dir()?;
    tokio::fs::create_dir_all(&dir).await?;
    let path = browser_profile_path(&profile.provider_id)?;
    let json =
        serde_json::to_string_pretty(profile).wrap_err("serialise BrowserProfile to JSON")?;
    tokio::fs::write(&path, json).await?;
    chmod_600(&path).await?;
    Ok(path)
}

pub async fn load_browser_profile(provider_id: &str) -> Result<BrowserProfile> {
    let path = browser_profile_path(provider_id)?;
    let raw = tokio::fs::read_to_string(&path)
        .await
        .wrap_err_with(|| format!("read {}", path.display()))?;
    serde_json::from_str(&raw).wrap_err("parse BrowserProfile JSON")
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
    fn openai_is_default_first_entry() {
        assert_eq!(PROVIDERS[0].id, "openai");
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
    fn render_config_includes_auth_method_block() {
        let c = ProviderChoice {
            id: "deepseek".into(),
            name: "x".into(),
            base_url: "https://x.example/v1".into(),
            default_model: "deepseek-chat".into(),
            fallback: vec![],
            key_url: None,
            local: false,
        };
        let toml_api = render_config_toml_with_auth(&c, AuthMethod::ApiKey);
        assert!(toml_api.contains("[auth]"));
        assert!(toml_api.contains("method = \"api_key\""));
        let toml_browser = render_config_toml_with_auth(&c, AuthMethod::Browser);
        assert!(toml_browser.contains("method = \"browser\""));
    }

    #[test]
    fn browser_profile_path_under_evoclaw_dir() {
        let path = browser_profile_path("deepseek").unwrap();
        let s = path.display().to_string();
        assert!(s.ends_with("/browser_profiles/deepseek.json"));
    }

    #[test]
    fn browser_capture_shape_for_anthropic_uses_native_header() {
        // We don't test capture itself (interactive), but the inferred shape
        // is part of the public contract. Anthropic ⇒ AnthropicHeader, others
        // ⇒ Bearer. Mirror the match in `capture_browser_profile`.
        let s_anth = match "anthropic" {
            "anthropic" => BrowserAuthShape::AnthropicHeader,
            _ => BrowserAuthShape::Bearer,
        };
        assert_eq!(s_anth, BrowserAuthShape::AnthropicHeader);
        let s_ds = match "deepseek" {
            "anthropic" => BrowserAuthShape::AnthropicHeader,
            _ => BrowserAuthShape::Bearer,
        };
        assert_eq!(s_ds, BrowserAuthShape::Bearer);
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
        let toml = render_config_toml_with_auth(&c, AuthMethod::ApiKey);
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
        let toml = render_config_toml_with_auth(&c, AuthMethod::ApiKey);
        assert!(toml.contains("fallback = []"));
    }

    #[test]
    fn find_provider_by_id() {
        assert_eq!(find_provider("deepseek").unwrap().id, "deepseek");
        assert!(find_provider("nonexistent").is_none());
    }
}
