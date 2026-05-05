use crate::{resolve_within_workspace, Tool, ToolContext, ToolError, ToolFactory};
use async_trait::async_trait;
use evo_policy::Permission;
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Deserialize)]
struct PatchArgs {
    path: String,
    old_content: String,
    new_content: String,
}

pub struct PatchFile;

#[async_trait]
impl Tool for PatchFile {
    fn name(&self) -> &str {
        "patch_file"
    }
    fn description(&self) -> &str {
        "Replace unique old_content with new. Exact match required."
    }
    fn permission(&self) -> Permission {
        Permission::P1
    }
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
        let a: PatchArgs =
            serde_json::from_value(args).map_err(|e| ToolError::InvalidArgs(e.to_string()))?;
        let path = resolve_within_workspace(&ctx.workspace, &a.path).await?;
        ctx.deny_if_self_write(&path)?;
        let original = tokio::fs::read_to_string(&path).await?;
        let count = original.matches(&a.old_content).count();
        if count == 0 {
            return Err(ToolError::InvalidArgs("old_content not found".into()));
        }
        if count > 1 {
            return Err(ToolError::InvalidArgs(format!(
                "old_content matched {count} times; must be unique"
            )));
        }
        let updated = original.replacen(&a.old_content, &a.new_content, 1);
        tokio::fs::write(&path, &updated).await?;
        Ok(format!(
            "patched {} ({} → {} bytes)",
            path.display(),
            original.len(),
            updated.len()
        ))
    }
}

inventory::submit!(ToolFactory {
    build: || Box::new(PatchFile)
});

#[cfg(test)]
mod tests {
    use super::*;
    use evo_policy::Permission;
    use serde_json::json;
    use std::path::Path;
    use std::sync::Arc;

    fn ctx_in(dir: &Path) -> ToolContext {
        ToolContext::default_for_workspace(dir.to_path_buf()).with_max_permission(Permission::P3)
    }

    fn unique_tmp(name: &str) -> std::path::PathBuf {
        let mut p = std::env::temp_dir();
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        p.push(format!("evo-tools-{name}-{stamp}"));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[tokio::test]
    async fn patch_file_replaces_unique_substring() {
        let dir = unique_tmp("patch");
        let path = dir.join("a.txt");
        tokio::fs::write(&path, "alpha\nbeta\ngamma\n")
            .await
            .unwrap();
        let ctx = ctx_in(&dir);
        let out = PatchFile
            .run(
                &ctx,
                json!({
                    "path": "a.txt",
                    "old_content": "beta",
                    "new_content": "BETA"
                }),
            )
            .await
            .unwrap();
        assert!(out.contains("patched"));
        let content = tokio::fs::read_to_string(&path).await.unwrap();
        assert!(content.contains("BETA"));
    }

    #[tokio::test]
    async fn patch_file_rejects_non_unique() {
        let dir = unique_tmp("patch-dup");
        let path = dir.join("a.txt");
        tokio::fs::write(&path, "x\nx\n").await.unwrap();
        let err = PatchFile
            .run(
                &ctx_in(&dir),
                json!({"path":"a.txt","old_content":"x","new_content":"y"}),
            )
            .await
            .err()
            .unwrap();
        assert!(matches!(err, ToolError::InvalidArgs(_)));
    }

    #[tokio::test]
    async fn patch_file_blocked_when_path_is_self_exe() {
        let dir = unique_tmp("self-patch");
        // patch_file reads the file first, so put recognisable content in it.
        let exe_path = dir.join("evoclaw");
        std::fs::write(&exe_path, "original").unwrap();
        let canonical = exe_path.canonicalize().unwrap();
        let ctx = ToolContext {
            self_exe: Some(Arc::new(canonical)),
            ..ctx_in(&dir)
        };
        let err = PatchFile
            .run(
                &ctx,
                json!({"path": "evoclaw", "old_content": "original", "new_content": "pwned"}),
            )
            .await
            .expect_err("must deny patch to own exe");
        assert!(matches!(err, ToolError::Denied(_)), "got {err:?}");
        assert_eq!(
            std::fs::read_to_string(dir.join("evoclaw")).unwrap(),
            "original"
        );
    }
}
