//! Headless browser tools: navigate, screenshot, click, type, eval.
//!
//! All five tools require P3 permission. A single browser session per
//! account profile is launched lazily on first use and reused across calls.
//! Persistent profiles are stored under `{evoclaw_dir}/browser_profiles/`.

use crate::tools::browser_profile;
use crate::tools::browser_profile::POOL;
use crate::tools::login_detect::{classify, PageKind};
use crate::tools::secret_inject::{load_secrets, resolve, Resolved};
use crate::{smart_format, Tool, ToolContext, ToolError, ToolFactory};
use async_trait::async_trait;
use chromiumoxide::cdp::browser_protocol::page::CaptureScreenshotFormat;
use chromiumoxide::page::ScreenshotParams;
use evo_policy::Permission;
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;

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

        // Ensure session exists, then acquire pool for use.
        {
            let mut pool = POOL.lock().await;
            browser_profile::get_or_create(
                &mut pool,
                ctx.browser_profile.as_deref(),
                ctx.evoclaw_dir.as_deref(),
            )
            .await?;
        }

        let page = {
            let pool = POOL.lock().await;
            let key = ctx.browser_profile.as_deref().unwrap_or("__ephemeral__");
            Arc::clone(&pool.get(key).unwrap().page)
        };

        page.goto(a.url.as_str())
            .await
            .map_err(|e| ToolError::Internal(format!("navigate: {e}")))?;
        page.wait_for_navigation()
            .await
            .map_err(|e| ToolError::Internal(format!("wait: {e}")))?;

        let final_url: String = page
            .evaluate("window.location.href")
            .await
            .map_err(|e| ToolError::Internal(format!("url: {e}")))?
            .into_value()
            .unwrap_or_else(|_| a.url.clone());

        let title = page
            .get_title()
            .await
            .map_err(|e| ToolError::Internal(format!("title: {e}")))?
            .unwrap_or_default();

        let text: String = page
            .evaluate("document.body ? document.body.innerText : ''")
            .await
            .map_err(|e| ToolError::Internal(format!("text: {e}")))?
            .into_value()
            .unwrap_or_else(|_| String::new());

        // Login detection — inform the Skill whether credentials are needed.
        let login_hint = match classify(&final_url, &text) {
            PageKind::LoginRequired => "\nlogin_required: true",
            PageKind::Authenticated => "",
        };

        let cap = ctx.max_observation_chars.saturating_sub(200);
        Ok(format!(
            "url: {final_url}\ntitle: {title}{login_hint}\n\n{}",
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

        {
            let mut pool = POOL.lock().await;
            browser_profile::get_or_create(
                &mut pool,
                ctx.browser_profile.as_deref(),
                ctx.evoclaw_dir.as_deref(),
            )
            .await?;
        }

        let page = {
            let pool = POOL.lock().await;
            let key = ctx.browser_profile.as_deref().unwrap_or("__ephemeral__");
            Arc::clone(&pool.get(key).unwrap().page)
        };

        let params = ScreenshotParams::builder()
            .format(CaptureScreenshotFormat::Png)
            .full_page(a.full_page.unwrap_or(false))
            .build();
        let bytes = page
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
    async fn run(&self, ctx: &ToolContext, args: Value) -> Result<String, ToolError> {
        let a: ClickArgs =
            serde_json::from_value(args).map_err(|e| ToolError::InvalidArgs(e.to_string()))?;

        {
            let mut pool = POOL.lock().await;
            browser_profile::get_or_create(
                &mut pool,
                ctx.browser_profile.as_deref(),
                ctx.evoclaw_dir.as_deref(),
            )
            .await?;
        }

        let page = {
            let pool = POOL.lock().await;
            let key = ctx.browser_profile.as_deref().unwrap_or("__ephemeral__");
            Arc::clone(&pool.get(key).unwrap().page)
        };

        page.find_element(a.selector.as_str())
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

        // Resolve ${SECRET:name} / ${TOTP:name} — real value never echoed.
        let real_text = if let Some(vault_path) = &ctx.vault_path {
            let vault = load_secrets(vault_path)
                .await
                .map_err(|e| ToolError::Internal(format!("credentials load: {e}")))?;
            match resolve(&a.text, &vault) {
                Resolved::Plain => a.text.clone(),
                Resolved::Value(v) => v,
                Resolved::Missing { key } => {
                    return Err(ToolError::Denied(format!(
                        "secret '{key}' not found — add it to ~/.evoclaw/secrets/credentials.toml"
                    )));
                }
            }
        } else {
            a.text.clone()
        };

        {
            let mut pool = POOL.lock().await;
            browser_profile::get_or_create(
                &mut pool,
                ctx.browser_profile.as_deref(),
                ctx.evoclaw_dir.as_deref(),
            )
            .await?;
        }

        let page = {
            let pool = POOL.lock().await;
            let key = ctx.browser_profile.as_deref().unwrap_or("__ephemeral__");
            Arc::clone(&pool.get(key).unwrap().page)
        };

        if a.clear.unwrap_or(true) {
            let sel_json =
                serde_json::to_string(&a.selector).unwrap_or_else(|_| format!("{:?}", a.selector));
            let _ = page
                .evaluate(format!(
                    "(function(){{var el=document.querySelector({sel_json});if(el)el.value='';}})();"
                ))
                .await;
        }
        page.find_element(a.selector.as_str())
            .await
            .map_err(|e| ToolError::Internal(format!("find '{}': {e}", a.selector)))?
            .type_str(real_text.as_str())
            .await
            .map_err(|e| ToolError::Internal(format!("type: {e}")))?;
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
    async fn run(&self, ctx: &ToolContext, args: Value) -> Result<String, ToolError> {
        let a: EvalArgs =
            serde_json::from_value(args).map_err(|e| ToolError::InvalidArgs(e.to_string()))?;

        {
            let mut pool = POOL.lock().await;
            browser_profile::get_or_create(
                &mut pool,
                ctx.browser_profile.as_deref(),
                ctx.evoclaw_dir.as_deref(),
            )
            .await?;
        }

        let page = {
            let pool = POOL.lock().await;
            let key = ctx.browser_profile.as_deref().unwrap_or("__ephemeral__");
            Arc::clone(&pool.get(key).unwrap().page)
        };

        let result: Value = page
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
