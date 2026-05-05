//! Browser profile directory management and multi-session pool.
//!
//! Each `account_id` gets an isolated Chrome profile under
//! `{evoclaw_dir}/browser_profiles/{account_id}/` (chmod 700 on Unix).
//! Sessions are created on first use and reused for all subsequent tool calls
//! within the same process.
//!
//!   account_id = None          → ephemeral session (no profile dir)
//!   account_id = "google_work" → persistent session, profile dir created + chmod 700

use crate::tools::browser_session::{self, BrowserSession};
use crate::ToolError;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use tokio::sync::Mutex;

// ---------------------------------------------------------------------------
// Session pool — keyed by account_id ("__ephemeral__" for no-profile sessions)
// ---------------------------------------------------------------------------

pub(crate) static POOL: LazyLock<Mutex<HashMap<String, BrowserSession>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

// ---------------------------------------------------------------------------
// Profile directory helpers
// ---------------------------------------------------------------------------

/// Resolve the Chrome profile path for `account_id`.
pub(crate) fn profile_dir(evoclaw_dir: &Path, account_id: &str) -> PathBuf {
    evoclaw_dir.join("browser_profiles").join(account_id)
}

/// Create the profile directory (chmod 700 on Unix) and return its path.
pub(crate) async fn ensure_profile_dir(
    evoclaw_dir: &Path,
    account_id: &str,
) -> Result<PathBuf, std::io::Error> {
    let dir = profile_dir(evoclaw_dir, account_id);
    tokio::fs::create_dir_all(&dir).await?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).await?;
    }
    Ok(dir)
}

// ---------------------------------------------------------------------------
// Pool access
// ---------------------------------------------------------------------------

/// Fetch a `BrowserSession` from the pool, launching one if absent.
///
/// `account_id = None`   → ephemeral Chrome session (in-memory only)
/// `account_id = Some(s)` → persistent session; profile dir created under
///                          `evoclaw_dir/browser_profiles/{s}` on first use
pub(crate) async fn get_or_create<'a>(
    pool: &'a mut HashMap<String, BrowserSession>,
    account_id: Option<&str>,
    evoclaw_dir: Option<&Path>,
) -> Result<&'a BrowserSession, ToolError> {
    let key = account_id.unwrap_or("__ephemeral__").to_string();

    if !pool.contains_key(&key) {
        let pdir: Option<PathBuf> = match (account_id, evoclaw_dir) {
            (Some(id), Some(base)) => {
                let dir = ensure_profile_dir(base, id)
                    .await
                    .map_err(|e| ToolError::Internal(format!("profile dir: {e}")))?;
                Some(dir)
            }
            _ => None,
        };
        let session = browser_session::launch(pdir.as_deref()).await?;
        pool.insert(key.clone(), session);
    }

    Ok(pool.get(&key).unwrap())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_dir_path_construction() {
        let base = Path::new("/home/user/.evoclaw");
        let dir = profile_dir(base, "google_work");
        assert_eq!(dir, PathBuf::from("/home/user/.evoclaw/browser_profiles/google_work"));
    }

    #[test]
    fn profile_dir_separates_accounts() {
        let base = Path::new("/home/user/.evoclaw");
        assert_ne!(
            profile_dir(base, "google_work"),
            profile_dir(base, "google_home")
        );
    }
}
