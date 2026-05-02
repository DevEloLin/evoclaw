//! MCP tool bridge — wraps each tool advertised by a configured MCP server
//! as an `evo_tools::Tool` so the agent loop can call it just like a built-in.
//!
//! Tool naming: `mcp__<server_id>__<tool>` (Claude-Code convention) prevents
//! collisions across servers and makes provenance obvious in transcripts.

use async_trait::async_trait;
use evo_mcp_client::{McpClient, ServerConfig, ToolContent};
use evo_policy::Permission;
use evo_tools::{Tool, ToolContext, ToolError, ToolRegistry};
use serde_json::Value;
use std::sync::Arc;

/// One MCP-exposed tool, adapted to the `Tool` trait. Heap-allocates name +
/// description so they outlive the registry and satisfy `Tool: 'static`.
pub struct McpToolWrapper {
    qualified_name: String,
    remote_name: String,
    description: String,
    schema: Value,
    server_id: String,
    client: Arc<McpClient>,
}

impl McpToolWrapper {
    pub fn new(
        server_id: impl Into<String>,
        remote_name: impl Into<String>,
        description: impl Into<String>,
        schema: Value,
        client: Arc<McpClient>,
    ) -> Self {
        let server_id = server_id.into();
        let remote_name = remote_name.into();
        let qualified_name = format!("mcp__{server_id}__{remote_name}");
        Self {
            qualified_name,
            remote_name,
            description: description.into(),
            schema,
            server_id,
            client,
        }
    }

    pub fn server_id(&self) -> &str { &self.server_id }
    pub fn remote_name(&self) -> &str { &self.remote_name }
}

#[async_trait]
impl Tool for McpToolWrapper {
    fn name(&self) -> &str { &self.qualified_name }
    fn description(&self) -> &str { &self.description }
    fn permission(&self) -> Permission { Permission::P3 }
    fn schema(&self) -> Value { self.schema.clone() }

    async fn run(&self, _ctx: &ToolContext, args: Value) -> Result<String, ToolError> {
        let result = self.client.call_tool(&self.remote_name, args).await
            .map_err(|e| ToolError::Internal(format!("mcp[{}].{}: {e}", self.server_id, self.remote_name)))?;
        let text = render_content(&result.content);
        if result.is_error {
            return Err(ToolError::Internal(text));
        }
        Ok(text)
    }
}

fn render_content(blocks: &[ToolContent]) -> String {
    let mut out = String::new();
    for b in blocks {
        match b {
            ToolContent::Text { text } => {
                if !out.is_empty() { out.push('\n'); }
                out.push_str(text);
            }
            ToolContent::Image { mime_type, .. } => {
                if !out.is_empty() { out.push('\n'); }
                out.push_str(&format!("[image: {mime_type}]"));
            }
            ToolContent::Other => { /* skip unknown */ }
        }
    }
    out
}

/// Spawn one MCP server, run the initialize handshake, list its tools, and
/// register every one as a wrapper in `registry`. On failure, returns the
/// error so the caller can decide whether to skip or abort.
pub async fn install_server(
    registry: &mut ToolRegistry,
    cfg: &ServerConfig,
) -> Result<usize, evo_mcp_client::McpError> {
    let client = Arc::new(McpClient::new());
    client.spawn(cfg).await?;
    client.initialize("evoclaw", env!("CARGO_PKG_VERSION")).await?;
    let tools = client.list_tools().await?;
    let n = tools.len();
    for t in tools {
        let w = McpToolWrapper::new(
            cfg.id.clone(),
            t.name,
            t.description,
            t.input_schema,
            client.clone(),
        );
        registry.push(Box::new(w));
    }
    Ok(n)
}

/// Walk every server in `~/.evoclaw/mcp/`, install all tools they expose.
/// Per-server failures are logged and skipped — a missing `npx` should not
/// prevent the agent from starting.
pub async fn install_all(registry: &mut ToolRegistry) -> usize {
    let servers = match evo_mcp_client::list_servers().await {
        Ok(s) => s,
        Err(e) => { tracing::warn!(error=%e, "list_servers failed"); return 0; }
    };
    let mut attached = 0usize;
    for cfg in servers {
        match install_server(registry, &cfg).await {
            Ok(n) => {
                tracing::info!(server=%cfg.id, tools=n, "MCP server attached");
                attached += 1;
            }
            Err(e) => {
                tracing::warn!(server=%cfg.id, error=%e, "MCP server skipped");
            }
        }
    }
    attached
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn render_content_concatenates_text_blocks() {
        let blocks = vec![
            ToolContent::Text { text: "alpha".into() },
            ToolContent::Text { text: "beta".into() },
        ];
        assert_eq!(render_content(&blocks), "alpha\nbeta");
    }

    #[test]
    fn render_content_includes_image_marker() {
        let blocks = vec![
            ToolContent::Image { data: "ignored".into(), mime_type: "image/png".into() },
        ];
        assert!(render_content(&blocks).contains("image/png"));
    }

    #[test]
    fn qualified_name_uses_double_underscore() {
        let c = Arc::new(McpClient::new());
        let w = McpToolWrapper::new("github", "list_issues", "desc", json!({}), c);
        assert_eq!(w.name(), "mcp__github__list_issues");
    }

    #[test]
    fn description_is_passed_through() {
        let c = Arc::new(McpClient::new());
        let w = McpToolWrapper::new("fs", "read", "Read a file via MCP", json!({}), c);
        assert_eq!(w.description(), "Read a file via MCP");
    }
}
