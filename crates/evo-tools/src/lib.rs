//! evo-tools — Phase 1 ships 4 of the 10 PRD §43 tools.
//!
//! `read_file`, `write_file`, `run_shell`, `ask_user` self-register via
//! `inventory::submit!` so adding a tool requires zero changes to dispatch
//! (PRD §45.3).

use async_trait::async_trait;
use evo_policy::Permission;
use evo_providers::ToolSpec;
use serde::Deserialize;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
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
    while idx > 0 && !s.is_char_boundary(idx) { idx -= 1; }
    idx
}
fn ceil_char_boundary(s: &str, mut idx: usize) -> usize {
    while idx < s.len() && !s.is_char_boundary(idx) { idx += 1; }
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
}

impl Default for ToolContext {
    fn default() -> Self {
        Self {
            workspace: PathBuf::from("."),
            allow_user_prompt: false,
            default_shell_timeout: Duration::from_secs(30),
            max_observation_chars: 8000,
        }
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
        self.tools.iter().find(|t| t.name() == name).map(|b| b.as_ref())
    }
    pub async fn invoke(&self, ctx: &ToolContext, name: &str, args: Value) -> Result<String, ToolError> {
        let tool = self.find(name).ok_or_else(|| ToolError::InvalidArgs(format!("unknown tool: {name}")))?;
        let raw = tool.run(ctx, args).await?;
        Ok(smart_format(&raw, ctx.max_observation_chars))
    }
}

fn resolve_path(workspace: &Path, requested: &str) -> PathBuf {
    let p = Path::new(requested);
    if p.is_absolute() { p.to_path_buf() } else { workspace.join(p) }
}

fn format_with_line_numbers(content: &str) -> String {
    let mut out = String::with_capacity(content.len() + content.lines().count() * 6);
    for (i, line) in content.lines().enumerate() {
        out.push_str(&format!("{:5}\t{}\n", i + 1, line));
    }
    out
}

// --- Tool 1: read_file ----------------------------------------------------

#[derive(Deserialize)]
struct ReadArgs { path: String }
pub struct ReadFile;

#[async_trait]
impl Tool for ReadFile {
    fn name(&self) -> &str { "read_file" }
    fn description(&self) -> &str { "Read file. Returns lines + numbers. Read before edit." }
    fn permission(&self) -> Permission { Permission::P0 }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": { "path": { "type": "string" } },
            "required": ["path"],
            "additionalProperties": false,
        })
    }
    async fn run(&self, ctx: &ToolContext, args: Value) -> Result<String, ToolError> {
        let a: ReadArgs = serde_json::from_value(args).map_err(|e| ToolError::InvalidArgs(e.to_string()))?;
        let path = resolve_path(&ctx.workspace, &a.path);
        let content = tokio::fs::read_to_string(&path).await?;
        Ok(format_with_line_numbers(&content))
    }
}
inventory::submit!(ToolFactory { build: || Box::new(ReadFile) });

// --- Tool 2: write_file ---------------------------------------------------

#[derive(Deserialize)]
struct WriteArgs { path: String, content: String }
pub struct WriteFile;

#[async_trait]
impl Tool for WriteFile {
    fn name(&self) -> &str { "write_file" }
    fn description(&self) -> &str { "Write file. Creates if missing. Diff shown before commit." }
    fn permission(&self) -> Permission { Permission::P1 }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string" },
                "content": { "type": "string" },
            },
            "required": ["path", "content"],
            "additionalProperties": false,
        })
    }
    async fn run(&self, ctx: &ToolContext, args: Value) -> Result<String, ToolError> {
        let a: WriteArgs = serde_json::from_value(args).map_err(|e| ToolError::InvalidArgs(e.to_string()))?;
        let path = resolve_path(&ctx.workspace, &a.path);
        if path.is_absolute() && !path.starts_with(&ctx.workspace) {
            return Err(ToolError::Denied(format!("write outside workspace: {}", path.display())));
        }
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let bytes = a.content.len();
        tokio::fs::write(&path, &a.content).await?;
        Ok(format!("wrote {} bytes to {}", bytes, path.display()))
    }
}
inventory::submit!(ToolFactory { build: || Box::new(WriteFile) });

// --- Tool 3: run_shell ----------------------------------------------------

#[derive(Deserialize)]
struct ShellArgs {
    cmd: String,
    #[serde(default)]
    timeout_ms: Option<u64>,
}
pub struct RunShell;

#[async_trait]
impl Tool for RunShell {
    fn name(&self) -> &str { "run_shell" }
    fn description(&self) -> &str { "Run shell. Sandboxed, 30s default, output truncated 8K." }
    fn permission(&self) -> Permission { Permission::P2 }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "cmd": { "type": "string" },
                "timeout_ms": { "type": "integer", "minimum": 1, "maximum": 600000 },
            },
            "required": ["cmd"],
            "additionalProperties": false,
        })
    }
    async fn run(&self, ctx: &ToolContext, args: Value) -> Result<String, ToolError> {
        let a: ShellArgs = serde_json::from_value(args).map_err(|e| ToolError::InvalidArgs(e.to_string()))?;
        let timeout = a.timeout_ms.map(Duration::from_millis).unwrap_or(ctx.default_shell_timeout);
        let mut cmd = tokio::process::Command::new("sh");
        cmd.arg("-c").arg(&a.cmd).current_dir(&ctx.workspace);
        let output = tokio::time::timeout(timeout, cmd.output())
            .await
            .map_err(|_| ToolError::Timeout(timeout))?
            .map_err(ToolError::Io)?;
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        let code = output.status.code().unwrap_or(-1);
        Ok(format!(
            "exit={code}\n--- stdout ---\n{}\n--- stderr ---\n{}",
            stdout.trim_end(),
            stderr.trim_end()
        ))
    }
}
inventory::submit!(ToolFactory { build: || Box::new(RunShell) });

// --- Tool 4: ask_user -----------------------------------------------------

#[derive(Deserialize)]
struct AskArgs { message: String }
pub struct AskUser;

#[async_trait]
impl Tool for AskUser {
    fn name(&self) -> &str { "ask_user" }
    fn description(&self) -> &str { "Ask user. Required for high-risk / ambiguous / missing param." }
    fn permission(&self) -> Permission { Permission::P0 }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": { "message": { "type": "string" } },
            "required": ["message"],
            "additionalProperties": false,
        })
    }
    async fn run(&self, ctx: &ToolContext, args: Value) -> Result<String, ToolError> {
        let a: AskArgs = serde_json::from_value(args).map_err(|e| ToolError::InvalidArgs(e.to_string()))?;
        if !ctx.allow_user_prompt {
            return Ok(format!("[ask_user-stub] {}", a.message));
        }
        let prompt = a.message.clone();
        let answer = tokio::task::spawn_blocking(move || {
            use std::io::{self, BufRead, Write};
            let stdout = io::stdout();
            let mut h = stdout.lock();
            let _ = writeln!(h, "\n[evo ask_user] {prompt}");
            let _ = write!(h, "> ");
            let _ = h.flush();
            let stdin = io::stdin();
            let mut line = String::new();
            stdin.lock().read_line(&mut line).ok();
            line.trim().to_string()
        })
        .await
        .map_err(|e| ToolError::Internal(e.to_string()))?;
        Ok(answer)
    }
}
inventory::submit!(ToolFactory { build: || Box::new(AskUser) });

// --- Tool 5: patch_file (PRD §43) ----------------------------------------

#[derive(Deserialize)]
struct PatchArgs { path: String, old_content: String, new_content: String }
pub struct PatchFile;

#[async_trait]
impl Tool for PatchFile {
    fn name(&self) -> &str { "patch_file" }
    fn description(&self) -> &str { "Replace unique old_content with new. Exact match required." }
    fn permission(&self) -> Permission { Permission::P1 }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string" },
                "old_content": { "type": "string" },
                "new_content": { "type": "string" },
            },
            "required": ["path", "old_content", "new_content"],
            "additionalProperties": false,
        })
    }
    async fn run(&self, ctx: &ToolContext, args: Value) -> Result<String, ToolError> {
        let a: PatchArgs = serde_json::from_value(args).map_err(|e| ToolError::InvalidArgs(e.to_string()))?;
        let path = resolve_path(&ctx.workspace, &a.path);
        if path.is_absolute() && !path.starts_with(&ctx.workspace) {
            return Err(ToolError::Denied(format!("patch outside workspace: {}", path.display())));
        }
        let original = tokio::fs::read_to_string(&path).await?;
        let count = original.matches(&a.old_content).count();
        if count == 0 {
            return Err(ToolError::InvalidArgs("old_content not found".into()));
        }
        if count > 1 {
            return Err(ToolError::InvalidArgs(format!("old_content matched {count} times; must be unique")));
        }
        let updated = original.replacen(&a.old_content, &a.new_content, 1);
        tokio::fs::write(&path, &updated).await?;
        Ok(format!("patched {} ({} → {} bytes)", path.display(), original.len(), updated.len()))
    }
}
inventory::submit!(ToolFactory { build: || Box::new(PatchFile) });

// --- Tool 6: list_dir (PRD §43) -------------------------------------------

const LIST_DIR_EXCLUDE: &[&str] = &["node_modules", ".git", "target", ".venv", "__pycache__", "dist", "build"];

#[derive(Deserialize)]
struct ListDirArgs {
    path: String,
    #[serde(default)]
    max_entries: Option<usize>,
}
pub struct ListDir;

#[async_trait]
impl Tool for ListDir {
    fn name(&self) -> &str { "list_dir" }
    fn description(&self) -> &str { "List dir entries. Excludes node_modules / .git / target." }
    fn permission(&self) -> Permission { Permission::P0 }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string" },
                "max_entries": { "type": "integer", "minimum": 1, "maximum": 1000 },
            },
            "required": ["path"],
            "additionalProperties": false,
        })
    }
    async fn run(&self, ctx: &ToolContext, args: Value) -> Result<String, ToolError> {
        let a: ListDirArgs = serde_json::from_value(args).map_err(|e| ToolError::InvalidArgs(e.to_string()))?;
        let path = resolve_path(&ctx.workspace, &a.path);
        let max = a.max_entries.unwrap_or(200);
        let mut entries = tokio::fs::read_dir(&path).await?;
        let mut lines = Vec::new();
        let mut count = 0usize;
        while let Some(entry) = entries.next_entry().await? {
            if count >= max { lines.push(format!("... (truncated at {max})")); break; }
            let name = entry.file_name().to_string_lossy().into_owned();
            if LIST_DIR_EXCLUDE.contains(&name.as_str()) { continue; }
            let meta = entry.metadata().await?;
            let kind = if meta.is_dir() { "d" } else if meta.is_file() { "f" } else { "?" };
            lines.push(format!("{kind} {} {} bytes", name, meta.len()));
            count += 1;
        }
        Ok(lines.join("\n"))
    }
}
inventory::submit!(ToolFactory { build: || Box::new(ListDir) });

// --- Tool 7: web_fetch (PRD §43) ------------------------------------------

#[derive(Deserialize)]
struct WebFetchArgs {
    url: String,
    #[serde(default)]
    max_chars: Option<usize>,
}
pub struct WebFetch;

#[async_trait]
impl Tool for WebFetch {
    fn name(&self) -> &str { "web_fetch" }
    fn description(&self) -> &str { "Fetch URL, return body. Cookie excluded from LLM." }
    fn permission(&self) -> Permission { Permission::P3 }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "url": { "type": "string", "format": "uri" },
                "max_chars": { "type": "integer", "minimum": 100, "maximum": 100000 },
            },
            "required": ["url"],
            "additionalProperties": false,
        })
    }
    async fn run(&self, _ctx: &ToolContext, args: Value) -> Result<String, ToolError> {
        let a: WebFetchArgs = serde_json::from_value(args).map_err(|e| ToolError::InvalidArgs(e.to_string()))?;
        if !(a.url.starts_with("http://") || a.url.starts_with("https://")) {
            return Err(ToolError::Denied("web_fetch only supports http(s) URLs".into()));
        }
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::limited(5))
            .timeout(std::time::Duration::from_secs(15))
            .build()
            .map_err(|e| ToolError::Internal(e.to_string()))?;
        let resp = client.get(&a.url).send().await
            .map_err(|e| ToolError::Internal(e.to_string()))?;
        let status = resp.status().as_u16();
        let content_type = resp.headers().get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
        let body = resp.text().await.map_err(|e| ToolError::Internal(e.to_string()))?;
        let cap = a.max_chars.unwrap_or(8000);
        let truncated = smart_format(&body, cap);
        Ok(format!("status={status}\ncontent-type={content_type}\n--- body ---\n{truncated}"))
    }
}
inventory::submit!(ToolFactory { build: || Box::new(WebFetch) });

// `browser_action` was removed in v0.5.1. EvoClaw no longer drives browser
// sessions. Phase 4.5 pivoted to ACP (external agent CLIs like claude-code /
// codex / cursor / copilot) — see `evo-acp-client` — and to standard MCP for
// dynamic tools — see `evo-mcp-client`.

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx_in(dir: &Path) -> ToolContext {
        ToolContext { workspace: dir.to_path_buf(), ..Default::default() }
    }

    fn unique_tmp(name: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
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

    #[tokio::test]
    async fn read_file_returns_numbered_lines() {
        let dir = unique_tmp("read");
        let path = dir.join("a.txt");
        tokio::fs::write(&path, "alpha\nbeta\n").await.unwrap();
        let ctx = ctx_in(&dir);
        let out = ReadFile.run(&ctx, json!({"path": "a.txt"})).await.unwrap();
        assert!(out.contains("1\talpha"));
        assert!(out.contains("2\tbeta"));
    }

    #[tokio::test]
    async fn write_file_creates_and_writes() {
        let dir = unique_tmp("write");
        let ctx = ctx_in(&dir);
        let out = WriteFile.run(&ctx, json!({"path": "out.txt", "content": "hi"})).await.unwrap();
        assert!(out.contains("wrote 2 bytes"));
        let content = tokio::fs::read_to_string(dir.join("out.txt")).await.unwrap();
        assert_eq!(content, "hi");
    }

    #[tokio::test]
    async fn run_shell_captures_exit() {
        let dir = unique_tmp("shell");
        let ctx = ctx_in(&dir);
        let out = RunShell.run(&ctx, json!({"cmd": "echo hello"})).await.unwrap();
        assert!(out.contains("exit=0"));
        assert!(out.contains("hello"));
    }

    #[tokio::test]
    async fn ask_user_stubs_when_non_interactive() {
        let ctx = ToolContext { allow_user_prompt: false, ..Default::default() };
        let out = AskUser.run(&ctx, json!({"message": "x"})).await.unwrap();
        assert!(out.starts_with("[ask_user-stub]"));
    }

    #[test]
    fn registry_includes_all_seven_builtins() {
        let r = ToolRegistry::with_builtins();
        let names = r.names();
        for must in ["read_file", "write_file", "patch_file", "list_dir", "run_shell", "ask_user", "web_fetch"] {
            assert!(names.iter().any(|n| n == must), "missing {must}");
        }
    }

    #[tokio::test]
    async fn patch_file_replaces_unique_substring() {
        let dir = unique_tmp("patch");
        let path = dir.join("a.txt");
        tokio::fs::write(&path, "alpha\nbeta\ngamma\n").await.unwrap();
        let ctx = ctx_in(&dir);
        let out = PatchFile.run(&ctx, json!({
            "path": "a.txt",
            "old_content": "beta",
            "new_content": "BETA"
        })).await.unwrap();
        assert!(out.contains("patched"));
        let content = tokio::fs::read_to_string(&path).await.unwrap();
        assert!(content.contains("BETA"));
    }

    #[tokio::test]
    async fn patch_file_rejects_non_unique() {
        let dir = unique_tmp("patch-dup");
        let path = dir.join("a.txt");
        tokio::fs::write(&path, "x\nx\n").await.unwrap();
        let err = PatchFile.run(&ctx_in(&dir), json!({"path":"a.txt","old_content":"x","new_content":"y"})).await.err().unwrap();
        assert!(matches!(err, ToolError::InvalidArgs(_)));
    }

    #[tokio::test]
    async fn list_dir_excludes_target_and_node_modules() {
        let dir = unique_tmp("list");
        for name in ["src", "target", "node_modules", "Cargo.toml"] {
            tokio::fs::create_dir_all(dir.join(name)).await.ok();
        }
        let out = ListDir.run(&ctx_in(&dir), json!({"path": "."})).await.unwrap();
        assert!(out.contains("src"));
        assert!(!out.contains("target"));
        assert!(!out.contains("node_modules"));
    }

    #[tokio::test]
    async fn web_fetch_rejects_non_http() {
        let err = WebFetch.run(&ToolContext::default(), json!({"url": "ftp://example.com"})).await.err().unwrap();
        assert!(matches!(err, ToolError::Denied(_)));
    }

    #[test]
    fn descriptions_under_80_chars() {
        let r = ToolRegistry::with_builtins();
        for spec in r.specs() {
            assert!(spec.description.len() <= 80, "{}: {} chars", spec.name, spec.description.len());
        }
    }
}
