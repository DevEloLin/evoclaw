//! Headless browser session lifecycle.
//!
//! A `BrowserSession` wraps one Chrome tab. Sessions can be launched with or
//! without a persistent profile directory:
//!   - `None`    — ephemeral; cookies are lost when the process exits
//!   - `Some(p)` — persistent; cookies/storage survive process restarts

use crate::ToolError;
use chromiumoxide::browser::{Browser, BrowserConfig};
use futures::StreamExt as _;
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// Session handle
// ---------------------------------------------------------------------------

pub(crate) struct BrowserSession {
    pub(crate) _browser: Browser,
    pub(crate) page: chromiumoxide::Page,
    /// Non-None when Chrome was started with a persistent `--user-data-dir`.
    #[allow(dead_code)]
    pub(crate) profile_dir: Option<PathBuf>,
    _handle: tokio::task::JoinHandle<()>,
}

impl Drop for BrowserSession {
    fn drop(&mut self) {
        self._handle.abort();
    }
}

// ---------------------------------------------------------------------------
// Launcher
// ---------------------------------------------------------------------------

/// Launch a new `BrowserSession`.
///
/// When `profile_dir` is `Some`, Chrome is started with `--user-data-dir`
/// pointing at that path so cookies and session storage survive restarts.
pub(crate) async fn launch(profile_dir: Option<&Path>) -> Result<BrowserSession, ToolError> {
    let mut builder = BrowserConfig::builder()
        .arg("--no-sandbox")
        .arg("--disable-setuid-sandbox")
        .arg("--disable-dev-shm-usage")
        .arg("--disable-gpu");

    if let Some(dir) = profile_dir {
        builder = builder.arg(format!("--user-data-dir={}", dir.to_string_lossy()));
    }

    if let Some(exe) = find_chrome() {
        builder = builder.chrome_executable(exe);
    }

    let config = builder
        .build()
        .map_err(|e| ToolError::Internal(format!("browser config: {e}")))?;

    let (browser, mut handler) = Browser::launch(config).await.map_err(|e| {
        ToolError::Internal(format!(
            "browser launch failed: {e} — Chrome/Chromium must be installed"
        ))
    })?;

    let handle = tokio::spawn(async move { while handler.next().await.is_some() {} });

    let page = browser
        .new_page("about:blank")
        .await
        .map_err(|e| ToolError::Internal(format!("new page: {e}")))?;

    Ok(BrowserSession {
        _browser: browser,
        page,
        profile_dir: profile_dir.map(Path::to_path_buf),
        _handle: handle,
    })
}

// ---------------------------------------------------------------------------
// Chrome binary discovery
// ---------------------------------------------------------------------------

fn find_chrome() -> Option<PathBuf> {
    const ABS: &[&str] = &[
        "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
        "/Applications/Chromium.app/Contents/MacOS/Chromium",
        "/usr/bin/google-chrome",
        "/usr/bin/google-chrome-stable",
        "/usr/bin/chromium-browser",
        "/usr/bin/chromium",
    ];
    for p in ABS {
        let path = Path::new(p);
        if path.exists() {
            return Some(path.to_path_buf());
        }
    }
    let path_var = std::env::var("PATH").unwrap_or_default();
    for name in ["google-chrome", "google-chrome-stable", "chromium-browser", "chromium"] {
        for dir in path_var.split(':') {
            let full = Path::new(dir).join(name);
            if full.exists() {
                return Some(full);
            }
        }
    }
    None
}
