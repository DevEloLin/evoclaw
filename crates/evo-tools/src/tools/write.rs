use crate::{resolve_within_workspace, Tool, ToolContext, ToolError, ToolFactory};
use async_trait::async_trait;
use evo_policy::Permission;
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Deserialize)]
struct WriteArgs {
    path: String,
    content: String,
}

pub struct WriteFile;

#[async_trait]
impl Tool for WriteFile {
    fn name(&self) -> &str {
        "write_file"
    }
    fn description(&self) -> &str {
        "Write file. Creates if missing. Diff shown before commit."
    }
    fn permission(&self) -> Permission {
        Permission::P1
    }
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
        let a: WriteArgs =
            serde_json::from_value(args).map_err(|e| ToolError::InvalidArgs(e.to_string()))?;
        let path = resolve_within_workspace(&ctx.workspace, &a.path).await?;
        ctx.deny_if_self_write(&path)?;
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let bytes = a.content.len();
        tokio::fs::write(&path, &a.content).await?;
        Ok(format!("wrote {} bytes to {}", bytes, path.display()))
    }
}

inventory::submit!(ToolFactory {
    build: || Box::new(WriteFile)
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

    fn ctx_with_fake_exe(dir: &Path, exe_name: &str) -> ToolContext {
        let exe_path = dir.join(exe_name);
        std::fs::write(&exe_path, b"fake-binary").unwrap();
        let canonical = exe_path.canonicalize().unwrap();
        ToolContext {
            self_exe: Some(Arc::new(canonical)),
            ..ctx_in(dir)
        }
    }

    #[tokio::test]
    async fn write_file_rejects_absolute_outside_workspace() {
        let dir = unique_tmp("write-abs");
        let ctx = ctx_in(&dir);
        let err = WriteFile
            .run(
                &ctx,
                json!({"path": "/etc/evo-test-should-not-exist", "content": "x"}),
            )
            .await
            .expect_err("must deny absolute escape");
        assert!(matches!(err, ToolError::Denied(_)), "got {err:?}");
    }

    #[tokio::test]
    async fn write_file_creates_and_writes() {
        let dir = unique_tmp("write");
        let ctx = ctx_in(&dir);
        let out = WriteFile
            .run(&ctx, json!({"path": "out.txt", "content": "hi"}))
            .await
            .unwrap();
        assert!(out.contains("wrote 2 bytes"));
        let content = tokio::fs::read_to_string(dir.join("out.txt"))
            .await
            .unwrap();
        assert_eq!(content, "hi");
    }

    #[tokio::test]
    async fn write_file_blocked_when_path_is_self_exe() {
        let dir = unique_tmp("self-write");
        let ctx = ctx_with_fake_exe(&dir, "evoclaw");
        let err = WriteFile
            .run(&ctx, json!({"path": "evoclaw", "content": "pwned"}))
            .await
            .expect_err("must deny write to own exe");
        assert!(matches!(err, ToolError::Denied(_)), "got {err:?}");
        // File must not be overwritten.
        let content = std::fs::read(dir.join("evoclaw")).unwrap();
        assert_eq!(content, b"fake-binary");
    }

    #[tokio::test]
    async fn write_file_allowed_when_self_exe_not_set() {
        let dir = unique_tmp("self-none");
        let mut ctx = ctx_in(&dir);
        ctx.self_exe = None;
        let path = dir.join("data.txt");
        std::fs::write(&path, b"ok").unwrap();
        // Should succeed — no guard when self_exe is None.
        let out = WriteFile
            .run(&ctx, json!({"path": "data.txt", "content": "updated"}))
            .await
            .unwrap();
        assert!(out.contains("wrote"));
    }
}
