//! `load_skill` — lets the agent discover and execute user-authored
//! playbooks (under `~/.evoclaw/playbooks/`) directly from natural-language
//! commands like "load uae-events-daily and run it".
//!
//! Behaviour:
//!
//! * Called with **no `id`** (or empty `id`): returns the list of installed
//!   playbooks with their declared parameters, so the agent knows what is
//!   available without the user having to recite filenames.
//!
//! * Called with an `id`: loads the playbook file, validates required
//!   parameters, substitutes `{name}` placeholders in the `steps:` block,
//!   and returns the rendered text. The agent then follows those steps
//!   like any other tool-result, calling `web_fetch` / `run_shell` /
//!   `write_file` / etc. as instructed.

use crate::{Tool, ToolContext, ToolError, ToolFactory};
use async_trait::async_trait;
use evo_policy::Permission;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::PathBuf;

// ---------------------------------------------------------------------------
// Playbook file shape (subset — only what the runner needs)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct PlaybookFile {
    id: String,
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    parameters: Vec<ParamFile>,
    steps: String,
}

#[derive(Debug, Deserialize)]
struct ParamFile {
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default = "default_required")]
    required: bool,
    #[serde(default)]
    example: Option<String>,
}

fn default_required() -> bool {
    true
}

fn playbooks_dir(ctx: &ToolContext) -> Option<PathBuf> {
    ctx.evoclaw_dir.as_ref().map(|d| d.join("playbooks"))
}

/// Parse a playbook file. Accepts both pure YAML and Markdown-with-YAML-
/// frontmatter (delimited by `---`).
fn parse_text(text: &str) -> Result<PlaybookFile, String> {
    let trimmed = text.trim_start_matches('\u{feff}');
    let yaml_body = if let Some(rest) = trimmed
        .strip_prefix("---\n")
        .or_else(|| trimmed.strip_prefix("---\r\n"))
    {
        let end = rest
            .find("\n---\n")
            .or_else(|| rest.find("\n---\r\n"))
            .or_else(|| rest.find("\n---"))
            .ok_or_else(|| "frontmatter missing closing '---'".to_string())?;
        &rest[..end]
    } else {
        trimmed
    };
    serde_yaml::from_str(yaml_body).map_err(|e| e.to_string())
}

/// Resolve and load a skill by either:
///   * a bare name (e.g. `"uae-events-daily"`) — searched under
///     `~/.evoclaw/playbooks/` with `.yaml` / `.yml` / `.md` extensions, or
///   * a file path — absolute (`/abs/path/x.yaml`), home-prefixed
///     (`~/code/skills/x.yaml`), or relative to the workspace
///     (`./local.yaml`).
///
/// Anything containing `/`, starting with `~`, or starting with `.` is
/// treated as a path. Everything else is a bare name.
async fn load_by_id_or_path(ctx: &ToolContext, raw: &str) -> Result<PlaybookFile, ToolError> {
    let looks_like_path = raw.contains('/')
        || raw.starts_with('~')
        || raw.starts_with('.');

    if looks_like_path {
        let expanded = expand_tilde(raw);
        let path = if expanded.is_absolute() {
            expanded
        } else {
            ctx.workspace.join(expanded)
        };
        if !path.exists() {
            return Err(ToolError::Internal(format!(
                "skill file not found: {}",
                path.display()
            )));
        }
        // Security: resolve symlinks then verify the final target is under
        // workspace OR the user's playbooks dir. Anywhere else is denied
        // so the agent cannot be coaxed into reading /etc/passwd, vault
        // files, ~/.ssh/, etc.
        let resolved = tokio::fs::canonicalize(&path).await.map_err(|e| {
            ToolError::Internal(format!(
                "cannot canonicalize {}: {e}",
                path.display()
            ))
        })?;
        let mut allowed_roots: Vec<PathBuf> = Vec::new();
        if let Ok(ws) = tokio::fs::canonicalize(&ctx.workspace).await {
            allowed_roots.push(ws);
        }
        if let Some(pb) = playbooks_dir(ctx) {
            if let Ok(c) = tokio::fs::canonicalize(&pb).await {
                allowed_roots.push(c);
            }
        }
        if !allowed_roots.iter().any(|root| resolved.starts_with(root)) {
            return Err(ToolError::Denied(format!(
                "skill path {} is outside the workspace and ~/.evoclaw/playbooks — \
                 move the file inside one of these directories",
                resolved.display()
            )));
        }
        let text = tokio::fs::read_to_string(&resolved).await?;
        return parse_text(&text)
            .map_err(|e| ToolError::Internal(format!("parse {}: {e}", resolved.display())));
    }

    // Bare name — search the default playbooks dir.
    let dir = playbooks_dir(ctx)
        .ok_or_else(|| ToolError::Internal("ToolContext.evoclaw_dir is not set".into()))?;
    for ext in ["yaml", "yml", "md"] {
        let p = dir.join(format!("{raw}.{ext}"));
        if p.exists() {
            let text = tokio::fs::read_to_string(&p).await?;
            return parse_text(&text)
                .map_err(|e| ToolError::Internal(format!("parse {raw}.{ext}: {e}")));
        }
    }
    Err(ToolError::Internal(format!(
        "skill '{raw}' not found in {} (looked for .yaml/.yml/.md) — \
         pass an absolute path or use ~/path/to/skill.yaml to load from elsewhere",
        dir.display()
    )))
}

/// Expand a leading `~/` to the user's home directory. Returns the original
/// path unchanged when `$HOME` is not set or the prefix doesn't apply.
fn expand_tilde(s: &str) -> PathBuf {
    if let Some(rest) = s.strip_prefix("~/") {
        if let Ok(home) = std::env::var("HOME") {
            return PathBuf::from(home).join(rest);
        }
    }
    PathBuf::from(s)
}

async fn list_all(ctx: &ToolContext) -> Result<String, ToolError> {
    let dir = playbooks_dir(ctx)
        .ok_or_else(|| ToolError::Internal("ToolContext.evoclaw_dir is not set".into()))?;
    if !dir.exists() {
        return Ok(format!(
            "no playbooks directory at {} — create one and drop *.yaml or *.md files there",
            dir.display()
        ));
    }
    let mut entries = tokio::fs::read_dir(&dir).await?;
    let mut lines: Vec<String> = Vec::new();
    while let Some(entry) = entries.next_entry().await? {
        let p = entry.path();
        let ext = p.extension().and_then(|s| s.to_str()).unwrap_or("");
        if !matches!(ext, "yaml" | "yml" | "md") {
            continue;
        }
        let Ok(text) = tokio::fs::read_to_string(&p).await else {
            continue;
        };
        let Ok(pb) = parse_text(&text) else {
            continue;
        };
        let summary: Vec<String> = pb
            .parameters
            .iter()
            .map(|pp| {
                let mark = if pp.required { "" } else { "?" };
                format!("{}{}", pp.name, mark)
            })
            .collect();
        lines.push(format!(
            "- {}  params=[{}]  — {}",
            pb.id,
            summary.join(", "),
            pb.name
        ));
    }
    if lines.is_empty() {
        return Ok(format!("no playbooks under {}", dir.display()));
    }
    lines.sort();
    Ok(format!(
        "{} skill(s) available under {}:\n\n{}\n\nTo run one, call load_skill with {{\"id\": \"<id>\", \"params\": {{...}}}}. \
         Params ending in '?' are optional.",
        lines.len(),
        dir.display(),
        lines.join("\n")
    ))
}

// ---------------------------------------------------------------------------
// Tool definition
// ---------------------------------------------------------------------------

#[derive(Deserialize, Default)]
#[serde(default)]
struct LoadSkillArgs {
    /// Playbook id (filename without extension). Omit to list all skills.
    id: Option<String>,
    /// Parameter values for the playbook. Each value is substituted into
    /// `{name}` placeholders inside the `steps:` block.
    params: HashMap<String, String>,
}

pub struct LoadSkillTool;

#[async_trait]
impl Tool for LoadSkillTool {
    fn name(&self) -> &str {
        "load_skill"
    }
    fn description(&self) -> &str {
        "Run a skill by id (name or path). Omit id to list available skills."
    }
    fn permission(&self) -> Permission {
        Permission::P1
    }
    fn skip_observation_truncation(&self) -> bool {
        // Skill bodies are instructions the agent must follow in full;
        // the 8 KB observation cap would silently drop most of the steps.
        true
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "id": {
                    "type": "string",
                    "description": "Skill name (looked up in ~/.evoclaw/playbooks/) OR a file path (absolute, ~/-prefixed, or relative to workspace). Omit to list installed skills."
                },
                "params": {
                    "type": "object",
                    "additionalProperties": { "type": "string" },
                    "description": "Parameter values. Required params must be set; optional ones default to empty."
                }
            },
            "additionalProperties": false
        })
    }
    async fn run(&self, ctx: &ToolContext, args: Value) -> Result<String, ToolError> {
        let a: LoadSkillArgs = if args.is_null() {
            LoadSkillArgs::default()
        } else {
            serde_json::from_value(args).map_err(|e| ToolError::InvalidArgs(e.to_string()))?
        };

        let id = match a.id.as_deref() {
            None | Some("") => return list_all(ctx).await,
            Some(s) => s,
        };

        let pb = load_by_id_or_path(ctx, id).await?;

        // Required-param check.
        let missing: Vec<String> = pb
            .parameters
            .iter()
            .filter(|p| p.required && !a.params.contains_key(&p.name))
            .map(|p| {
                let hint = p.example.as_deref().unwrap_or("");
                if hint.is_empty() {
                    format!("'{}' (— {})", p.name, p.description)
                } else {
                    format!("'{}' (example: {hint})", p.name)
                }
            })
            .collect();
        if !missing.is_empty() {
            // Phrase the error so the agent's recovery path is unambiguous:
            // ask the user, then call load_skill AGAIN with the collected
            // values. Without this hint, the agent often gives up after
            // one missing-param error.
            return Err(ToolError::InvalidArgs(format!(
                "skill '{id}' needs these parameters before it can run: {}. \
                 Use the ask_user tool to collect each value, then call \
                 load_skill again with id='{id}' and the params map.",
                missing.join(", ")
            )));
        }

        // Render: substitute declared params first (with empty-string fallback
        // for optionals), then sweep any ad-hoc params not declared.
        let mut rendered = pb.steps.clone();
        for p in &pb.parameters {
            let v = a.params.get(&p.name).map(|s| s.as_str()).unwrap_or("");
            rendered = rendered.replace(&format!("{{{}}}", p.name), v);
        }
        for (k, v) in &a.params {
            rendered = rendered.replace(&format!("{{{}}}", k), v);
        }

        let header = format!(
            "── Skill loaded: {} ({}) ──\n\
             Follow these steps in order to fulfil the user's request. \
             Call other tools (web_fetch, run_shell, write_file, …) as the \
             steps instruct.\n\
             Description: {}\n\n",
            id, pb.name, pb.description
        );
        // Skip the registry's 8 KB observation cap (a loaded skill is the
        // agent's instruction set, not an observation), but still apply a
        // larger safety net at the tool layer: a malicious or careless
        // skill file could otherwise blow past the provider's context
        // window in a single call. 64 KB covers every legitimate skill we
        // ship (UAE events digest is ~40 KB) while bounding the worst case.
        const MAX_SKILL_BODY_BYTES: usize = 64 * 1024;
        if rendered.len() > MAX_SKILL_BODY_BYTES {
            let truncated: String = rendered.chars().take(MAX_SKILL_BODY_BYTES).collect();
            return Ok(format!(
                "{header}{truncated}\n\n\
                 [skill body truncated at {MAX_SKILL_BODY_BYTES} bytes — \
                 original was {} bytes. Split the playbook into smaller \
                 steps or load follow-up sections in a second call.]",
                rendered.len()
            ));
        }
        Ok(format!("{header}{rendered}"))
    }
}

inventory::submit!(ToolFactory {
    build: || Box::new(LoadSkillTool)
});

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn unique_dir(name: &str) -> PathBuf {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("evo-tools-skill-{name}-{stamp}"))
    }

    async fn ctx_with_playbook(name: &str, content: &str) -> (ToolContext, PathBuf) {
        let evoclaw = unique_dir(name);
        let pb_dir = evoclaw.join("playbooks");
        tokio::fs::create_dir_all(&pb_dir).await.unwrap();
        tokio::fs::write(pb_dir.join("t.yaml"), content).await.unwrap();
        let ctx = ToolContext {
            evoclaw_dir: Some(evoclaw.clone()),
            ..ToolContext::default()
        };
        (ctx, evoclaw)
    }

    const SAMPLE: &str = r#"
id: t
name: Test skill
description: hi
parameters:
  - name: out_dir
    description: where
    example: "/tmp/x"
  - name: dry_run
    required: false
steps: |
  write to {out_dir}, dry_run={dry_run}
"#;

    #[tokio::test]
    async fn list_returns_known_id() {
        let (ctx, _) = ctx_with_playbook("list", SAMPLE).await;
        let out = LoadSkillTool.run(&ctx, json!({})).await.unwrap();
        assert!(out.contains("- t"), "list output: {out}");
        assert!(out.contains("out_dir"));
        assert!(out.contains("dry_run?"));
    }

    #[tokio::test]
    async fn load_substitutes_params() {
        let (ctx, _) = ctx_with_playbook("load", SAMPLE).await;
        let out = LoadSkillTool
            .run(&ctx, json!({"id": "t", "params": {"out_dir": "/abc"}}))
            .await
            .unwrap();
        assert!(out.contains("/abc"), "missing /abc: {out}");
        assert!(!out.contains("{out_dir}"));
        assert!(out.contains("Skill loaded"));
    }

    #[tokio::test]
    async fn missing_required_param_errors() {
        let (ctx, _) = ctx_with_playbook("missing", SAMPLE).await;
        let err = LoadSkillTool
            .run(&ctx, json!({"id": "t"}))
            .await
            .unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("out_dir"), "err: {msg}");
        assert!(msg.contains("/tmp/x"), "err should mention example: {msg}");
    }

    #[tokio::test]
    async fn unknown_id_errors() {
        let (ctx, _) = ctx_with_playbook("unknown", SAMPLE).await;
        let err = LoadSkillTool
            .run(&ctx, json!({"id": "nope"}))
            .await
            .unwrap_err();
        assert!(format!("{err}").contains("not found"));
    }

    #[tokio::test]
    async fn loads_skill_from_absolute_path_inside_workspace() {
        // Absolute path is allowed only when the resolved canonical path
        // is under the workspace or the playbooks dir. This test places
        // a skill in a sub-directory of the workspace and confirms the
        // tool accepts the full absolute path.
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let ws = std::env::temp_dir().join(format!("evo-skill-abs-{stamp}"));
        let sub = ws.join("nested");
        tokio::fs::create_dir_all(&sub).await.unwrap();
        let file = sub.join("custom.yaml");
        tokio::fs::write(&file, SAMPLE).await.unwrap();

        let mut ctx = ToolContext::default_for_workspace(ws);
        ctx.evoclaw_dir = Some(std::env::temp_dir().join(format!("evo-abs-evo-{stamp}")));
        let out = LoadSkillTool
            .run(
                &ctx,
                json!({"id": file.to_str().unwrap(), "params": {"out_dir": "/abc"}}),
            )
            .await
            .unwrap();
        assert!(out.contains("/abc"));
        assert!(out.contains("Skill loaded"));
    }

    #[tokio::test]
    async fn loads_skill_from_relative_workspace_path() {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let ws = std::env::temp_dir().join(format!("evo-skill-ws-{stamp}"));
        tokio::fs::create_dir_all(ws.join("my-skills")).await.unwrap();
        tokio::fs::write(ws.join("my-skills").join("rel.yaml"), SAMPLE)
            .await
            .unwrap();

        let mut ctx = ToolContext::default_for_workspace(ws.clone());
        ctx.evoclaw_dir = Some(ws.join(".evoclaw"));
        let out = LoadSkillTool
            .run(
                &ctx,
                json!({"id": "./my-skills/rel.yaml", "params": {"out_dir": "/q"}}),
            )
            .await
            .unwrap();
        assert!(out.contains("/q"));
    }

    #[test]
    fn expand_tilde_resolves_home() {
        std::env::set_var("HOME", "/Users/test");
        let p = expand_tilde("~/foo/bar.yaml");
        assert_eq!(p, PathBuf::from("/Users/test/foo/bar.yaml"));
    }

    #[test]
    fn expand_tilde_passes_through_non_tilde() {
        let p = expand_tilde("/abs/path");
        assert_eq!(p, PathBuf::from("/abs/path"));
    }

    #[tokio::test]
    async fn missing_path_reports_clearly() {
        let ctx = ToolContext {
            evoclaw_dir: Some(std::env::temp_dir()),
            ..ToolContext::default()
        };
        let err = LoadSkillTool
            .run(&ctx, json!({"id": "/nonexistent/skill.yaml"}))
            .await
            .unwrap_err();
        assert!(format!("{err}").contains("not found"));
    }

    #[tokio::test]
    async fn rejects_path_outside_workspace_and_playbooks() {
        // Create a skill OUTSIDE both workspace and the playbooks dir.
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let escape_dir = std::env::temp_dir().join(format!("evo-skill-escape-{stamp}"));
        tokio::fs::create_dir_all(&escape_dir).await.unwrap();
        let escape_file = escape_dir.join("evil.yaml");
        tokio::fs::write(&escape_file, SAMPLE).await.unwrap();

        let ws = std::env::temp_dir().join(format!("evo-skill-ws-secure-{stamp}"));
        let evoclaw = std::env::temp_dir().join(format!("evo-skill-evo-secure-{stamp}"));
        tokio::fs::create_dir_all(&ws).await.unwrap();
        tokio::fs::create_dir_all(evoclaw.join("playbooks")).await.unwrap();
        let mut ctx = ToolContext::default_for_workspace(ws);
        ctx.evoclaw_dir = Some(evoclaw);

        let err = LoadSkillTool
            .run(
                &ctx,
                json!({"id": escape_file.to_str().unwrap(), "params": {"out_dir": "/x"}}),
            )
            .await
            .unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("outside"), "expected escape denied, got: {msg}");
    }

    #[tokio::test]
    async fn allows_explicit_path_inside_playbooks_dir() {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let evoclaw = std::env::temp_dir().join(format!("evo-skill-allow-{stamp}"));
        let pb_dir = evoclaw.join("playbooks");
        tokio::fs::create_dir_all(&pb_dir).await.unwrap();
        let file = pb_dir.join("nested.yaml");
        tokio::fs::write(&file, SAMPLE).await.unwrap();
        let mut ctx = ToolContext::default_for_workspace(
            std::env::temp_dir().join(format!("evo-skill-ws-allow-{stamp}")),
        );
        tokio::fs::create_dir_all(&ctx.workspace).await.unwrap();
        ctx.evoclaw_dir = Some(evoclaw);
        let out = LoadSkillTool
            .run(
                &ctx,
                json!({"id": file.to_str().unwrap(), "params": {"out_dir": "/q"}}),
            )
            .await
            .unwrap();
        assert!(out.contains("/q"));
    }

    #[tokio::test]
    async fn skip_observation_truncation_is_true() {
        // Lock the contract: load_skill must NOT be truncated by the
        // registry. Otherwise a 40 KB skill body loses STEP 1B..STEP 7.
        assert!(LoadSkillTool.skip_observation_truncation());
    }

    #[tokio::test]
    async fn oversize_skill_body_is_truncated() {
        // Build a synthetic skill whose rendered body exceeds the 64 KB cap.
        // The tool should return a body capped at 64 KB plus a clear marker.
        let big_step: String = "x".repeat(70 * 1024);
        let yaml = format!(
            "id: big\nname: Big skill\ndescription: oversized\nparameters: []\nsteps: |\n  {big_step}\n"
        );
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let evoclaw = std::env::temp_dir().join(format!("evo-tools-skill-big-{stamp}"));
        let pb_dir = evoclaw.join("playbooks");
        tokio::fs::create_dir_all(&pb_dir).await.unwrap();
        tokio::fs::write(pb_dir.join("big.yaml"), yaml).await.unwrap();
        let ctx = ToolContext {
            evoclaw_dir: Some(evoclaw),
            ..ToolContext::default()
        };
        let out = LoadSkillTool.run(&ctx, json!({"id": "big"})).await.unwrap();
        assert!(out.contains("[skill body truncated"));
        // Header (~200 bytes) + 64 KB body + truncation marker (~120 bytes).
        // Hard upper bound generous enough to ignore header/marker drift.
        assert!(out.len() < 64 * 1024 + 1024);
    }

    #[tokio::test]
    async fn frontmatter_md_parses() {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let evoclaw = std::env::temp_dir().join(format!("evo-tools-skill-md-{stamp}"));
        let pb_dir = evoclaw.join("playbooks");
        tokio::fs::create_dir_all(&pb_dir).await.unwrap();
        let md = format!("---\n{}\n---\n# body ignored\n", SAMPLE);
        tokio::fs::write(pb_dir.join("t.md"), md).await.unwrap();
        let ctx = ToolContext {
            evoclaw_dir: Some(evoclaw),
            ..ToolContext::default()
        };
        let out = LoadSkillTool.run(&ctx, json!({"id": "t", "params": {"out_dir": "/x"}}))
            .await
            .unwrap();
        assert!(out.contains("/x"));
    }
}
