use crate::{
    format_with_line_numbers, resolve_within_workspace, Tool, ToolContext, ToolError, ToolFactory,
};
use async_trait::async_trait;
use evo_policy::Permission;
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Deserialize)]
struct ReadArgs {
    path: String,
}

pub struct ReadFile;

#[async_trait]
impl Tool for ReadFile {
    fn name(&self) -> &str {
        "read_file"
    }
    fn description(&self) -> &str {
        "Read file. Returns lines + numbers. Read before edit."
    }
    fn permission(&self) -> Permission {
        Permission::P0
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": { "path": { "type": "string" } },
            "required": ["path"],
            "additionalProperties": false,
        })
    }
    async fn run(&self, ctx: &ToolContext, args: Value) -> Result<String, ToolError> {
        let a: ReadArgs =
            serde_json::from_value(args).map_err(|e| ToolError::InvalidArgs(e.to_string()))?;
        let path = resolve_within_workspace(&ctx.workspace, &a.path).await?;
        let content = tokio::fs::read_to_string(&path).await?;
        Ok(format_with_line_numbers(&content))
    }
}

inventory::submit!(ToolFactory {
    build: || Box::new(ReadFile)
});

#[cfg(test)]
mod tests {
    use super::*;
    use evo_policy::Permission;
    use serde_json::json;
    use std::path::Path;

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
    async fn read_file_rejects_workspace_escape_via_dotdot() {
        let dir = unique_tmp("read-escape");
        let parent = dir.parent().unwrap();
        let secret = parent.join(format!(
            "secret-{}.txt",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        tokio::fs::write(&secret, "TOPSECRET").await.unwrap();
        let ctx = ctx_in(&dir);
        let escape_arg = format!("../{}", secret.file_name().unwrap().to_string_lossy());
        let err = ReadFile
            .run(&ctx, json!({"path": escape_arg}))
            .await
            .expect_err("must deny escape");
        assert!(matches!(err, ToolError::Denied(_)), "got {err:?}");
        let _ = tokio::fs::remove_file(&secret).await;
    }
}
