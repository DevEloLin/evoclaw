use crate::{resolve_within_workspace, Tool, ToolContext, ToolError, ToolFactory};
use async_trait::async_trait;
use evo_policy::Permission;
use serde::Deserialize;
use serde_json::{json, Value};

const LIST_DIR_EXCLUDE: &[&str] = &[
    "node_modules",
    ".git",
    "target",
    ".venv",
    "__pycache__",
    "dist",
    "build",
];

#[derive(Deserialize)]
struct ListDirArgs {
    path: String,
    #[serde(default)]
    max_entries: Option<usize>,
}

pub struct ListDir;

#[async_trait]
impl Tool for ListDir {
    fn name(&self) -> &str {
        "list_dir"
    }
    fn description(&self) -> &str {
        "List dir entries. Excludes node_modules / .git / target."
    }
    fn permission(&self) -> Permission {
        Permission::P0
    }
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
        let a: ListDirArgs =
            serde_json::from_value(args).map_err(|e| ToolError::InvalidArgs(e.to_string()))?;
        let path = resolve_within_workspace(&ctx.workspace, &a.path).await?;
        let max = a.max_entries.unwrap_or(200);
        let mut entries = tokio::fs::read_dir(&path).await?;
        let mut lines = Vec::new();
        let mut count = 0usize;
        while let Some(entry) = entries.next_entry().await? {
            if count >= max {
                lines.push(format!("... (truncated at {max})"));
                break;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            if LIST_DIR_EXCLUDE.contains(&name.as_str()) {
                continue;
            }
            let meta = entry.metadata().await?;
            let kind = if meta.is_dir() {
                "d"
            } else if meta.is_file() {
                "f"
            } else {
                "?"
            };
            lines.push(format!("{kind} {} {} bytes", name, meta.len()));
            count += 1;
        }
        Ok(lines.join("\n"))
    }
}

inventory::submit!(ToolFactory {
    build: || Box::new(ListDir)
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
    async fn list_dir_excludes_target_and_node_modules() {
        let dir = unique_tmp("list");
        for name in ["src", "target", "node_modules", "Cargo.toml"] {
            tokio::fs::create_dir_all(dir.join(name)).await.ok();
        }
        let out = ListDir
            .run(&ctx_in(&dir), json!({"path": "."}))
            .await
            .unwrap();
        assert!(out.contains("src"));
        assert!(!out.contains("target"));
        assert!(!out.contains("node_modules"));
    }
}
