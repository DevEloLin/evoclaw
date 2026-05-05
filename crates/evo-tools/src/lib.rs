//! evo-tools — Phase 1 ships 4 of the 10 PRD §43 tools.
//!
//! `read_file`, `write_file`, `run_shell`, `ask_user` self-register via
//! `inventory::submit!` so adding a tool requires zero changes to dispatch
//! (PRD §45.3).

pub(crate) mod tools;

use async_trait::async_trait;
use evo_policy::{hook, Permission, PolicyConfig, PolicyDecision};
use evo_providers::ToolSpec;
#[cfg(test)]
use serde_json::json;
use serde_json::Value;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

/// PRD §42.3 — head + omit + tail truncation.
pub fn smart_format(s: &str, max_len: usize) -> String {
    let omit = " ... ";
    if s.len() <= max_len + omit.len() * 2 {
        return s.to_string();
    }
    let half = max_len / 2;
    let head_end = floor_char_boundary(s, half);
    let tail_start = ceil_char_boundary(s, s.len().saturating_sub(half));
    format!("{}{}{}", &s[..head_end], omit, &s[tail_start..])
}

fn floor_char_boundary(s: &str, mut idx: usize) -> usize {
    while idx > 0 && !s.is_char_boundary(idx) {
        idx -= 1;
    }
    idx
}
fn ceil_char_boundary(s: &str, mut idx: usize) -> usize {
    while idx < s.len() && !s.is_char_boundary(idx) {
        idx += 1;
    }
    idx
}

#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    #[error("invalid args: {0}")]
    InvalidArgs(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("denied: {0}")]
    Denied(String),
    #[error("timeout after {0:?}")]
    Timeout(Duration),
    #[error("internal: {0}")]
    Internal(String),
}

#[derive(Debug, Clone)]
pub struct ToolContext {
    pub workspace: PathBuf,
    pub allow_user_prompt: bool,
    pub default_shell_timeout: Duration,
    pub max_observation_chars: usize,
    /// Permission ceiling enforced by `ToolRegistry::invoke`. Tools whose
    /// declared `Permission` exceeds this ceiling are denied at dispatch.
    /// PRD §13.1 / README permission ladder. Defaults to `Permission::P2`
    /// (local-safe shell — allows read/write/shell, blocks network and above).
    pub max_permission: Permission,
    /// Canonical path of the running EvoClaw binary. Populated automatically
    /// by `Default`. Write tools and `run_shell` reject any operation that
    /// targets this path or references its filename, preventing the agent from
    /// self-modifying its own executable.
    pub self_exe: Option<Arc<PathBuf>>,
    /// Channel for routing `ask_user` prompts to the TUI event loop instead of
    /// directly reading stdin (which conflicts with raw-mode input handling).
    /// When `None`, `ask_user` falls back to direct stdin (non-TUI / one-shot).
    pub ask_tx:
        Option<tokio::sync::mpsc::UnboundedSender<(String, tokio::sync::oneshot::Sender<String>)>>,
    /// Path to `vault.json` used by `browser_type` to resolve `${SECRET:name}`
    /// and `${TOTP:name}` placeholders at tool-execution time.
    /// When `None`, placeholder syntax passes through unchanged.
    pub vault_path: Option<PathBuf>,
    /// Root of the EvoClaw data directory (`~/.evoclaw`). Used by browser
    /// tools to locate `browser_profiles/{account_id}/` for session persistence.
    /// When `None`, browser sessions are ephemeral (no profile directory).
    pub evoclaw_dir: Option<PathBuf>,
    /// Account identifier for persistent browser sessions (e.g. `"google_work"`).
    /// Maps to `{evoclaw_dir}/browser_profiles/{browser_profile}/`.
    /// When `None`, an ephemeral session is used.
    pub browser_profile: Option<String>,
    /// User-configurable allow/deny rules and pre-exec hooks loaded from
    /// `~/.evoclaw/policy.toml`. `None` disables policy enforcement (default
    /// in tests and one-shot contexts that don't load a config file).
    pub policy: Option<Arc<PolicyConfig>>,
}

impl Default for ToolContext {
    fn default() -> Self {
        let self_exe = std::env::current_exe()
            .ok()
            .and_then(|p| p.canonicalize().ok())
            .map(Arc::new);
        Self {
            workspace: PathBuf::from("."),
            allow_user_prompt: false,
            default_shell_timeout: Duration::from_secs(30),
            max_observation_chars: 8000,
            max_permission: Permission::P2,
            self_exe,
            ask_tx: None,
            vault_path: None,
            evoclaw_dir: None,
            browser_profile: None,
            policy: None,
        }
    }
}

impl ToolContext {
    /// Construct a context rooted at `workspace` with the documented default
    /// permission ceiling (`Permission::P1`). Existing call sites that build
    /// `ToolContext` via `..Default::default()` keep working unchanged because
    /// the new `max_permission` field is populated by `Default`.
    pub fn default_for_workspace(workspace: PathBuf) -> Self {
        Self {
            workspace,
            ..Default::default()
        }
    }

    /// Builder-style override of the permission ceiling.
    pub fn with_max_permission(mut self, p: Permission) -> Self {
        self.max_permission = p;
        self
    }

    /// Deny a write operation if `path` resolves to the running binary.
    fn deny_if_self_write(&self, path: &Path) -> Result<(), ToolError> {
        if let Some(exe) = &self.self_exe {
            if path == exe.as_ref() {
                return Err(ToolError::Denied(
                    "self-guard: cannot write to own executable".into(),
                ));
            }
        }
        Ok(())
    }

    /// Deny a shell command if it references the running binary's filename,
    /// blocking replacement, deletion, or chmod of the executable.
    fn deny_if_shell_targets_self(&self, cmd: &str) -> Result<(), ToolError> {
        if let Some(exe) = &self.self_exe {
            let stem = exe
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("evoclaw");
            if cmd.contains(stem) {
                return Err(ToolError::Denied(format!(
                    "self-guard: shell command references own executable '{stem}'"
                )));
            }
        }
        Ok(())
    }

    /// Hard-deny any command or path that targets the user's SSH directory.
    /// Matches `~/.ssh`, `$HOME/.ssh`, and any absolute path containing `/.ssh`.
    /// This guard is unconditional and cannot be overridden by permission level.
    pub(crate) fn deny_if_targets_ssh_dir(&self, input: &str) -> Result<(), ToolError> {
        if input.contains("/.ssh") {
            return Err(ToolError::Denied(
                "security: access to ~/.ssh is prohibited".into(),
            ));
        }
        Ok(())
    }
}

#[async_trait]
pub trait Tool: Send + Sync + 'static {
    /// Tool identifier shown to the model. Built-ins return string literals;
    /// dynamic tools (MCP wrappers) return field references — both are valid
    /// `&str`. Keep stable across a registry's lifetime.
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn permission(&self) -> Permission;
    fn schema(&self) -> Value;
    async fn run(&self, ctx: &ToolContext, args: Value) -> Result<String, ToolError>;
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: self.name().to_string(),
            description: self.description().to_string(),
            schema: self.schema(),
        }
    }
}

pub struct ToolFactory {
    pub build: fn() -> Box<dyn Tool>,
}
inventory::collect!(ToolFactory);

pub struct ToolRegistry {
    tools: Vec<Box<dyn Tool>>,
}

impl ToolRegistry {
    pub fn with_builtins() -> Self {
        let tools = inventory::iter::<ToolFactory>()
            .map(|f| (f.build)())
            .collect();
        Self { tools }
    }
    /// Add a dynamically-constructed tool (e.g. an MCP wrapper). Returns the
    /// registry for builder-style chaining.
    pub fn push(&mut self, tool: Box<dyn Tool>) -> &mut Self {
        self.tools.push(tool);
        self
    }
    pub fn names(&self) -> Vec<String> {
        self.tools.iter().map(|t| t.name().to_string()).collect()
    }
    pub fn specs(&self) -> Vec<ToolSpec> {
        self.tools.iter().map(|t| t.spec()).collect()
    }
    pub fn find(&self, name: &str) -> Option<&dyn Tool> {
        self.tools
            .iter()
            .find(|t| t.name() == name)
            .map(|b| b.as_ref())
    }
    pub async fn invoke(
        &self,
        ctx: &ToolContext,
        name: &str,
        args: Value,
    ) -> Result<String, ToolError> {
        let tool = self
            .find(name)
            .ok_or_else(|| ToolError::InvalidArgs(format!("unknown tool: {name}")))?;
        // Permission ladder enforcement at dispatch (PRD §13.1).
        let required = tool.permission();
        if !ctx.max_permission.allows(required) {
            return Err(ToolError::Denied(format!(
                "tool '{}' requires {:?}, ceiling is {:?}",
                name, required, ctx.max_permission
            )));
        }
        // User-configurable policy: allow/deny rules then pre-exec hooks.
        if let Some(policy) = &ctx.policy {
            let subject = extract_subject(name, &args);
            match policy.check_rules(name, &subject) {
                PolicyDecision::Block(reason) => return Err(ToolError::Denied(reason)),
                PolicyDecision::Allow => {}
            }
            for h in policy.hooks_for(name) {
                match hook::run_pre_exec(h, name, &args).await {
                    PolicyDecision::Block(reason) => return Err(ToolError::Denied(reason)),
                    PolicyDecision::Allow => {}
                }
            }
        }
        let raw = tool.run(ctx, args).await?;
        Ok(smart_format(&raw, ctx.max_observation_chars))
    }
}

/// Extract the policy-matching subject from tool args.
///
/// For `bash`/`run_shell`: the `command` field.
/// For `write_file`/`read_file`/`patch_file`/`list_dir`: the `path` field.
/// For `web_fetch`: the `url` field.
/// For all other tools: empty string (rule still matches on tool name alone).
fn extract_subject(tool: &str, args: &Value) -> String {
    let field = match tool {
        "run_shell" | "bash" => "command",
        "write_file" | "read_file" | "patch_file" | "list_dir" => "path",
        "web_fetch" => "url",
        _ => return String::new(),
    };
    args.get(field)
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

fn resolve_path(workspace: &Path, requested: &str) -> PathBuf {
    let p = Path::new(requested);
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        workspace.join(p)
    }
}

/// Lexically normalize a path: collapse `.` and `..` components without
/// touching the filesystem. Used as a fallback when `canonicalize` cannot run
/// (e.g. the target file does not exist yet for `write_file`).
fn lexical_normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in p.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                // Pop the last real component if any; otherwise keep the
                // ParentDir token so the boundary check downstream catches
                // escapes that pop past the workspace root.
                if !out.pop() {
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Canonicalize when the path exists; otherwise canonicalize the deepest
/// existing ancestor and re-attach the missing tail. Returns a normalized,
/// absolute path with all `..` and symlinks resolved up to the existing
/// portion. The remaining tail is lexically clean.
async fn canonical_or_lexical(p: &Path) -> std::io::Result<PathBuf> {
    if let Ok(real) = tokio::fs::canonicalize(p).await {
        return Ok(real);
    }
    // Walk up to find the deepest existing ancestor.
    let mut anc = p;
    let mut tail: Vec<&std::ffi::OsStr> = Vec::new();
    loop {
        match tokio::fs::canonicalize(anc).await {
            Ok(real) => {
                let mut out = real;
                for seg in tail.iter().rev() {
                    out.push(seg);
                }
                return Ok(lexical_normalize(&out));
            }
            Err(_) => match anc.parent() {
                Some(parent) => {
                    if let Some(name) = anc.file_name() {
                        tail.push(name);
                    }
                    anc = parent;
                }
                None => {
                    // No ancestor exists — fall back to lexical normalization
                    // of the original path. Boundary check downstream still
                    // rejects out-of-workspace results.
                    return Ok(lexical_normalize(p));
                }
            },
        }
    }
}

/// Resolve `requested` against `workspace` and verify the result stays inside
/// the workspace after `..` collapse and symlink resolution. Returns the
/// canonicalized (or lexically normalized) path on success.
async fn resolve_within_workspace(workspace: &Path, requested: &str) -> Result<PathBuf, ToolError> {
    let raw = resolve_path(workspace, requested);
    let resolved = canonical_or_lexical(&raw).await.map_err(ToolError::Io)?;
    let ws_canon = canonical_or_lexical(workspace)
        .await
        .map_err(ToolError::Io)?;
    if !resolved.starts_with(&ws_canon) {
        return Err(ToolError::Denied(format!(
            "path escapes workspace: {}",
            resolved.display()
        )));
    }
    Ok(resolved)
}

fn format_with_line_numbers(content: &str) -> String {
    let mut out = String::with_capacity(content.len() + content.lines().count() * 6);
    for (i, line) in content.lines().enumerate() {
        out.push_str(&format!("{:5}\t{}\n", i + 1, line));
    }
    out
}

// `browser_action` was removed in v0.5.1. EvoClaw no longer drives browser
// sessions. Phase 4.5 pivoted to ACP (external agent CLIs like claude-code /
// codex / cursor / copilot) — see `evo-acp-client` — and to standard MCP for
// dynamic tools — see `evo-mcp-client`.

#[cfg(test)]
mod tests {
    use super::*;

    fn unique_tmp(name: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        p.push(format!("evo-tools-{name}-{stamp}"));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn smart_format_preserves_short_strings() {
        assert_eq!(smart_format("hello", 100), "hello");
    }
    #[test]
    fn smart_format_truncates_long_strings() {
        let s = "abcdefghij".repeat(100);
        let out = smart_format(&s, 20);
        assert!(out.contains(" ... "));
        assert!(out.len() < s.len() / 2);
    }
    #[test]
    fn smart_format_handles_utf8() {
        let s = "中文测试".repeat(50);
        let out = smart_format(&s, 20);
        let pos = out.find(" ... ").unwrap();
        assert!(out.is_char_boundary(pos));
    }

    #[test]
    fn registry_includes_all_builtins() {
        let r = ToolRegistry::with_builtins();
        let names = r.names();
        for must in [
            "read_file",
            "write_file",
            "patch_file",
            "list_dir",
            "run_shell",
            "ask_user",
            "web_fetch",
            "browser_navigate",
            "browser_screenshot",
            "browser_click",
            "browser_type",
            "browser_eval",
        ] {
            assert!(names.iter().any(|n| n == must), "missing {must}");
        }
    }

    #[tokio::test]
    async fn registry_invoke_denies_when_ceiling_too_low() {
        // Default ceiling is P2; web_fetch requires P3.
        let r = ToolRegistry::with_builtins();
        let dir = unique_tmp("perm");
        let ctx = ToolContext::default_for_workspace(dir);
        let err = r
            .invoke(&ctx, "web_fetch", json!({"url": "https://example.com"}))
            .await
            .expect_err("must be denied");
        assert!(matches!(err, ToolError::Denied(_)), "got {err:?}");
    }

    #[test]
    fn descriptions_under_80_chars() {
        let r = ToolRegistry::with_builtins();
        for spec in r.specs() {
            assert!(
                spec.description.len() <= 80,
                "{}: {} chars",
                spec.name,
                spec.description.len()
            );
        }
    }
}
