use directories::BaseDirs;
use eyre::Result;
use std::path::PathBuf;

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
