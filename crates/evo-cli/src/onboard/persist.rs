use crate::onboard::paths::{
    browser_profile_path, browser_profiles_dir, config_path, evoclaw_dir, secrets_dir,
};
use crate::onboard::picker::ProviderChoice;
use evo_providers::{AuthMethod, BrowserProfile};
use eyre::{Result, WrapErr};
use std::path::{Path, PathBuf};

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

pub(crate) fn render_config_toml_with_auth(p: &ProviderChoice, auth: AuthMethod) -> String {
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
