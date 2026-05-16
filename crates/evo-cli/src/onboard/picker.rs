use crate::onboard::catalog::{provider_to_acp_agent, ProviderProfile, PROVIDERS};
use eyre::Result;
use std::io::Write;

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
            if matches!(profile.id, "custom" | "litellm" | "private-gateway") {
                return prompt_gateway(profile.id);
            }
            return Ok(profile_to_choice(profile));
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
            if matches!(
                profile.id,
                "custom" | "litellm" | "private-gateway" | "azure"
            ) {
                return prompt_gateway(profile.id);
            }
            return Ok(profile_to_choice(profile));
        }

        println!("  Choice {} out of range. Please try again.", n);
    }
}

/// ACP agent picker. Result has `id = "acp:<agent>"` so `run_one_shot`
/// dispatches via `AcpProvider::spawn` instead of fetching an API key.
/// Side-effect: writes `~/.evoclaw/agents/<agent>.toml`.
pub(crate) async fn pick_acp_agent() -> Result<ProviderChoice> {
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

pub fn prompt_gateway(kind: &str) -> Result<ProviderChoice> {
    println!();
    match kind {
        "litellm" => {
            println!("  LiteLLM Gateway setup");
            println!("  ─────────────────────────────────────────────────────────────");
            println!("  LiteLLM exposes an OpenAI-compatible endpoint at:");
            println!("    http://<host>:<port>/v1    (default port 4000)");
            println!("  Enter the base URL — everything before /chat/completions.");
            println!("  Example:  https://litellm.mycompany.com/v1");
        }
        "private-gateway" => {
            println!("  Private / Enterprise Gateway setup");
            println!("  ─────────────────────────────────────────────────────────────");
            println!("  If your gateway curl looks like:");
            println!("    curl https://gateway.example.com/api/llm/v1/chat/completions \\");
            println!("         -H \"Authorization: Bearer sk-gw-YOURKEY\"");
            println!("  Then the base_url is the path before /chat/completions:");
            println!("    https://gateway.example.com/api/llm/v1");
            println!();
            println!("  Supports both OpenAI models and Anthropic Claude models —");
            println!("  the correct Messages API endpoint is chosen automatically.");
        }
        "azure" => {
            println!("  Azure AI Foundry / Azure OpenAI setup");
            println!("  ─────────────────────────────────────────────────────────────");
            println!("  Azure OpenAI:  https://<resource>.openai.azure.com");
            println!("    default_model = your deployment name (e.g. gpt-4o-prod)");
            println!("  Azure AI Inference (non-OpenAI models):");
            println!("    https://<resource>.services.ai.azure.com/models");
            println!("    default_model = the model id (e.g. Mistral-large)");
            println!();
            println!("  api-key auth + api-version query are injected automatically.");
        }
        _ => {
            println!("  Custom OpenAI-compatible endpoint setup");
            println!("  ─────────────────────────────────────────────────────────────");
            println!("  Enter the base URL — everything before /chat/completions.");
            println!("  Example:  https://api.example.com/v1");
        }
    }
    println!();
    let id = if kind == "azure" {
        "azure".to_string()
    } else {
        read_nonempty(&format!("provider id (kebab-case, e.g. {kind})"))?
    };
    let base_url = read_nonempty("base_url (e.g. https://gateway.example.com/api/llm/v1)")?;
    let default_model = read_nonempty("default model (e.g. gpt-4o-mini or claude-opus-4-5)")?;
    let name = match kind {
        "litellm" => "LiteLLM Gateway".into(),
        "private-gateway" => "Private Gateway".into(),
        "azure" => "Azure AI Foundry".into(),
        _ => "Custom".into(),
    };
    Ok(ProviderChoice {
        id,
        name,
        base_url,
        default_model,
        fallback: vec![],
        key_url: None,
        local: false,
    })
}

pub fn read_nonempty(label: &str) -> Result<String> {
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
pub fn pick_auth_method(profile: &ProviderChoice) -> Result<evo_providers::AuthMethod> {
    if profile.local {
        return Ok(evo_providers::AuthMethod::ApiKey);
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
            "" | "1" | "api_key" | "apikey" | "key" => {
                return Ok(evo_providers::AuthMethod::ApiKey)
            }
            "2" | "browser" | "web" | "cookie" => return Ok(evo_providers::AuthMethod::Browser),
            "3" | "acp" | "agent" if has_acp => return Ok(evo_providers::AuthMethod::Acp),
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

fn profile_to_choice(profile: &ProviderProfile) -> ProviderChoice {
    ProviderChoice {
        id: profile.id.into(),
        name: profile.name.into(),
        base_url: profile.base_url.into(),
        default_model: profile.default_model.into(),
        fallback: profile.fallback.iter().map(|s| s.to_string()).collect(),
        key_url: profile.key_url.map(String::from),
        local: profile.local,
    }
}
