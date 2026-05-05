//! Headless browser tools: navigate, screenshot, click, type, eval.
//!
//! A single browser session is launched lazily on first use and reused across
//! all subsequent tool calls. Requires Chrome or Chromium on the host system.
//! All five tools require P3 permission (same as web_fetch).

use crate::tools::secret_inject::{resolve, Resolved};
use crate::{smart_format, Tool, ToolContext, ToolError, ToolFactory};
use async_trait::async_trait;
use chromiumoxide::browser::{Browser, BrowserConfig};
use chromiumoxide::cdp::browser_protocol::page::CaptureScreenshotFormat;
use chromiumoxide::page::ScreenshotParams;
use evo_policy::{Permission, Vault};
use futures::StreamExt as _;
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::LazyLock;
use tokio::sync::Mutex;

// ---------------------------------------------------------------------------
// Shared browser session
// ---------------------------------------------------------------------------

struct BrowserSession {
    _browser: Browser,
    page: chromiumoxide::Page,
    _handle: tokio::task::JoinHandle<()>,
}

impl Drop for BrowserSession {
    fn drop(&mut self) {
        self._handle.abort();
    }
}

static SESSION: LazyLock<Mutex<Option<BrowserSession>>> = LazyLock::new(|| Mutex::new(None));

/// Try well-known Chrome/Chromium absolute paths, then fall back to PATH.
fn find_chrome() -> Option<std::path::PathBuf> {
    let abs: &[&str] = &[
        "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
        "/Applications/Chromium.app/Contents/MacOS/Chromium",
        "/usr/bin/google-chrome",
        "/usr/bin/google-chrome-stable",
        "/usr/bin/chromium-browser",
        "/usr/bin/chromium",
    ];
    for p in abs {
        let path = std::path::Path::new(p);
        if path.exists() {
            return Some(path.to_path_buf());
        }
    }
    let path_var = std::env::var("PATH").unwrap_or_default();
    for name in [
        "google-chrome",
        "google-chrome-stable",
        "chromium-browser",
        "chromium",
    ] {
        for dir in path_var.split(':') {
            let full = std::path::Path::new(dir).join(name);
            if full.exists() {
                return Some(full);
            }
        }
    }
    None
}

async fn ensure_session(slot: &mut Option<BrowserSession>) -> Result<(), ToolError> {
    if slot.is_some() {
        return Ok(());
    }
    let mut builder = BrowserConfig::builder()
        .arg("--no-sandbox")
        .arg("--disable-setuid-sandbox")
        .arg("--disable-dev-shm-usage")
        .arg("--disable-gpu");
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
    *slot = Some(BrowserSession {
        _browser: browser,
        page,
        _handle: handle,
    });
    Ok(())
}

// ---------------------------------------------------------------------------
// browser_navigate
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct NavigateArgs {
    url: String,
}

pub struct BrowserNavigate;

#[async_trait]
impl Tool for BrowserNavigate {
    fn name(&self) -> &str {
        "browser_navigate"
    }
    fn description(&self) -> &str {
        "Navigate headless browser to URL; return page text."
    }
    fn permission(&self) -> Permission {
        Permission::P3
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "url": { "type": "string", "format": "uri" },
            },
            "required": ["url"],
            "additionalProperties": false,
        })
    }
    async fn run(&self, ctx: &ToolContext, args: Value) -> Result<String, ToolError> {
        let a: NavigateArgs =
            serde_json::from_value(args).map_err(|e| ToolError::InvalidArgs(e.to_string()))?;
        if !(a.url.starts_with("http://") || a.url.starts_with("https://")) {
            return Err(ToolError::InvalidArgs(
                "browser_navigate requires an http(s) URL".into(),
            ));
        }
        let mut guard = SESSION.lock().await;
        ensure_session(&mut guard).await?;
        let session = guard.as_ref().unwrap();
        session
            .page
            .goto(a.url.as_str())
            .await
            .map_err(|e| ToolError::Internal(format!("navigate: {e}")))?;
        session
            .page
            .wait_for_navigation()
            .await
            .map_err(|e| ToolError::Internal(format!("wait: {e}")))?;
        let title = session
            .page
            .get_title()
            .await
            .map_err(|e| ToolError::Internal(format!("title: {e}")))?
            .unwrap_or_default();
        let text: String = session
            .page
            .evaluate("document.body ? document.body.innerText : ''")
            .await
            .map_err(|e| ToolError::Internal(format!("text: {e}")))?
            .into_value()
            .unwrap_or_else(|_| String::new());
        let cap = ctx.max_observation_chars.saturating_sub(200);
        Ok(format!(
            "title: {title}\nurl: {}\n\n{}",
            a.url,
            smart_format(&text, cap)
        ))
    }
}

inventory::submit!(ToolFactory {
    build: || Box::new(BrowserNavigate)
});

// ---------------------------------------------------------------------------
// browser_screenshot
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct ScreenshotArgs {
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    full_page: Option<bool>,
}

pub struct BrowserScreenshot;

#[async_trait]
impl Tool for BrowserScreenshot {
    fn name(&self) -> &str {
        "browser_screenshot"
    }
    fn description(&self) -> &str {
        "Screenshot current page; save PNG to path, return path."
    }
    fn permission(&self) -> Permission {
        Permission::P3
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Output PNG path (default: workspace/screenshot.png)"
                },
                "full_page": { "type": "boolean" },
            },
            "additionalProperties": false,
        })
    }
    async fn run(&self, ctx: &ToolContext, args: Value) -> Result<String, ToolError> {
        let a: ScreenshotArgs =
            serde_json::from_value(args).map_err(|e| ToolError::InvalidArgs(e.to_string()))?;
        let save_path = match a.path {
            Some(p) => {
                let pb = std::path::PathBuf::from(&p);
                if pb.is_absolute() {
                    pb
                } else {
                    ctx.workspace.join(pb)
                }
            }
            None => ctx.workspace.join("screenshot.png"),
        };
        let mut guard = SESSION.lock().await;
        ensure_session(&mut guard).await?;
        let session = guard.as_ref().unwrap();
        let params = ScreenshotParams::builder()
            .format(CaptureScreenshotFormat::Png)
            .full_page(a.full_page.unwrap_or(false))
            .build();
        let bytes = session
            .page
            .screenshot(params)
            .await
            .map_err(|e| ToolError::Internal(format!("screenshot: {e}")))?;
        std::fs::write(&save_path, &bytes).map_err(ToolError::Io)?;
        Ok(format!("screenshot saved: {}", save_path.display()))
    }
}

inventory::submit!(ToolFactory {
    build: || Box::new(BrowserScreenshot)
});

// ---------------------------------------------------------------------------
// browser_click
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct ClickArgs {
    selector: String,
}

pub struct BrowserClick;

#[async_trait]
impl Tool for BrowserClick {
    fn name(&self) -> &str {
        "browser_click"
    }
    fn description(&self) -> &str {
        "Click element matching CSS selector in browser."
    }
    fn permission(&self) -> Permission {
        Permission::P3
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "selector": { "type": "string" },
            },
            "required": ["selector"],
            "additionalProperties": false,
        })
    }
    async fn run(&self, _ctx: &ToolContext, args: Value) -> Result<String, ToolError> {
        let a: ClickArgs =
            serde_json::from_value(args).map_err(|e| ToolError::InvalidArgs(e.to_string()))?;
        let mut guard = SESSION.lock().await;
        ensure_session(&mut guard).await?;
        let session = guard.as_ref().unwrap();
        session
            .page
            .find_element(a.selector.as_str())
            .await
            .map_err(|e| ToolError::Internal(format!("find '{}': {e}", a.selector)))?
            .click()
            .await
            .map_err(|e| ToolError::Internal(format!("click: {e}")))?;
        Ok(format!("clicked: {}", a.selector))
    }
}

inventory::submit!(ToolFactory {
    build: || Box::new(BrowserClick)
});

// ---------------------------------------------------------------------------
// browser_type
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct TypeArgs {
    selector: String,
    text: String,
    #[serde(default)]
    clear: Option<bool>,
}

pub struct BrowserType;

#[async_trait]
impl Tool for BrowserType {
    fn name(&self) -> &str {
        "browser_type"
    }
    fn description(&self) -> &str {
        "Type text into form element matching CSS selector."
    }
    fn permission(&self) -> Permission {
        Permission::P3
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "selector": { "type": "string" },
                "text": { "type": "string" },
                "clear": {
                    "type": "boolean",
                    "description": "Clear existing value first (default true)"
                },
            },
            "required": ["selector", "text"],
            "additionalProperties": false,
        })
    }
    async fn run(&self, ctx: &ToolContext, args: Value) -> Result<String, ToolError> {
        let a: TypeArgs =
            serde_json::from_value(args).map_err(|e| ToolError::InvalidArgs(e.to_string()))?;

        // Resolve ${SECRET:name} / ${TOTP:name} at execution boundary.
        // The real value is used only here and never returned in the observation.
        let real_text = if let Some(vault_path) = &ctx.vault_path {
            let vault = Vault::load(vault_path)
                .await
                .map_err(|e| ToolError::Internal(format!("vault load: {e}")))?;
            match resolve(&a.text, &vault) {
                Resolved::Plain => a.text.clone(),
                Resolved::Value(v) => v,
                Resolved::Missing { key } => {
                    return Err(ToolError::Denied(format!(
                        "secret '{key}' not found in vault — \
                         run: evoclaw secret add {key} <value>"
                    )));
                }
            }
        } else {
            a.text.clone()
        };

        let mut guard = SESSION.lock().await;
        ensure_session(&mut guard).await?;
        let session = guard.as_ref().unwrap();
        if a.clear.unwrap_or(true) {
            let sel_json =
                serde_json::to_string(&a.selector).unwrap_or_else(|_| format!("{:?}", a.selector));
            let _ = session
                .page
                .evaluate(format!(
                    "(function(){{var el=document.querySelector({sel_json});if(el)el.value='';}})();"
                ))
                .await;
        }
        session
            .page
            .find_element(a.selector.as_str())
            .await
            .map_err(|e| ToolError::Internal(format!("find '{}': {e}", a.selector)))?
            .type_str(real_text.as_str())
            .await
            .map_err(|e| ToolError::Internal(format!("type: {e}")))?;
        // Never echo real_text — observation only names the selector.
        Ok(format!("typed into: {}", a.selector))
    }
}

inventory::submit!(ToolFactory {
    build: || Box::new(BrowserType)
});

// ---------------------------------------------------------------------------
// browser_eval
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct EvalArgs {
    js: String,
}

pub struct BrowserEval;

#[async_trait]
impl Tool for BrowserEval {
    fn name(&self) -> &str {
        "browser_eval"
    }
    fn description(&self) -> &str {
        "Evaluate JavaScript in browser; return the result."
    }
    fn permission(&self) -> Permission {
        Permission::P3
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "js": { "type": "string" },
            },
            "required": ["js"],
            "additionalProperties": false,
        })
    }
    async fn run(&self, _ctx: &ToolContext, args: Value) -> Result<String, ToolError> {
        let a: EvalArgs =
            serde_json::from_value(args).map_err(|e| ToolError::InvalidArgs(e.to_string()))?;
        let mut guard = SESSION.lock().await;
        ensure_session(&mut guard).await?;
        let session = guard.as_ref().unwrap();
        let result: Value = session
            .page
            .evaluate(a.js.as_str())
            .await
            .map_err(|e| ToolError::Internal(format!("eval: {e}")))?
            .into_value()
            .unwrap_or(Value::Null);
        Ok(serde_json::to_string_pretty(&result).unwrap_or_else(|_| "null".into()))
    }
}

inventory::submit!(ToolFactory {
    build: || Box::new(BrowserEval)
});

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn browser_tool_descriptions_under_80_chars() {
        let tools: &[&dyn Tool] = &[
            &BrowserNavigate,
            &BrowserScreenshot,
            &BrowserClick,
            &BrowserType,
            &BrowserEval,
        ];
        for t in tools {
            assert!(
                t.description().len() <= 80,
                "{}: {} chars",
                t.name(),
                t.description().len()
            );
        }
    }

    #[test]
    fn browser_tools_require_p3() {
        let tools: &[&dyn Tool] = &[
            &BrowserNavigate,
            &BrowserScreenshot,
            &BrowserClick,
            &BrowserType,
            &BrowserEval,
        ];
        for t in tools {
            assert_eq!(
                t.permission(),
                Permission::P3,
                "{} should require P3",
                t.name()
            );
        }
    }
}
