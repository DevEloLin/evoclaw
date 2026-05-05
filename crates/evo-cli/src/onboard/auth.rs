use crate::onboard::paths::secret_file;
use crate::onboard::picker::ProviderChoice;
use evo_providers::{BrowserAuthShape, BrowserProfile};
use eyre::{Result, WrapErr};
use std::path::PathBuf;

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

/// Ask for the API key. For Copilot, runs OAuth device flow instead.
/// Optionally opens browser to provider's key page (paste-key flow).
pub async fn ask_api_key(profile: &ProviderChoice) -> Result<Option<String>> {
    use std::io::Write;

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
pub async fn run_copilot_oauth() -> Result<String> {
    use evo_providers::copilot;
    use std::io::Write;

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

pub fn try_open_browser(url: &str) {
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

/// Browser-sign-in capture flow. Open the vendor's web console, ask the user
/// to paste their captured session token, and persist a `BrowserProfile`.
///
/// The shape is inferred from the vendor: Anthropic uses `x-api-key`, every
/// other vendor in our catalog speaks OpenAI-compat which accepts a
/// `Authorization: Bearer …` token. Cookie-string mode is reachable for power
/// users via `BrowserProfile::shape = "cookie"` directly in the JSON file.
pub async fn capture_browser_profile(profile: &ProviderChoice) -> Result<BrowserProfile> {
    use std::io::Write;

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
