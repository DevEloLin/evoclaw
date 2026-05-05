use crate::{Tool, ToolContext, ToolError, ToolFactory};
use async_trait::async_trait;
use evo_policy::Permission;
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Deserialize)]
struct AskArgs {
    message: String,
}

pub struct AskUser;

#[async_trait]
impl Tool for AskUser {
    fn name(&self) -> &str {
        "ask_user"
    }
    fn description(&self) -> &str {
        "Ask user. Required for high-risk / ambiguous / missing param."
    }
    fn permission(&self) -> Permission {
        Permission::P0
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": { "message": { "type": "string" } },
            "required": ["message"],
            "additionalProperties": false,
        })
    }
    async fn run(&self, ctx: &ToolContext, args: Value) -> Result<String, ToolError> {
        let a: AskArgs =
            serde_json::from_value(args).map_err(|e| ToolError::InvalidArgs(e.to_string()))?;
        if !ctx.allow_user_prompt {
            return Ok(format!("[ask_user-stub] {}", a.message));
        }
        // TUI mode: route through the event loop so raw-mode input isn't disrupted.
        if let Some(tx) = &ctx.ask_tx {
            let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
            tx.send((a.message, resp_tx))
                .map_err(|_| ToolError::Internal("ask_user: TUI channel closed".into()))?;
            return resp_rx
                .await
                .map_err(|_| ToolError::Internal("ask_user: no response from UI".into()));
        }
        // Fallback: direct stdin (one-shot / non-TUI mode).
        let prompt = a.message.clone();
        tokio::task::spawn_blocking(move || {
            use std::io::{self, BufRead, Write};
            let stdout = io::stdout();
            let mut h = stdout.lock();
            let _ = writeln!(h, "\n[evo ask_user] {prompt}");
            let _ = write!(h, "> ");
            let _ = h.flush();
            let stdin = io::stdin();
            let mut line = String::new();
            // EOF means headless (gateway, daemon, CI) — fail loudly.
            match stdin.lock().read_line(&mut line) {
                Ok(0) => Err(ToolError::Internal(
                    "ask_user requires an interactive terminal".into(),
                )),
                Ok(_) => Ok(line.trim().to_string()),
                Err(e) => Err(ToolError::Io(e)),
            }
        })
        .await
        .map_err(|e| ToolError::Internal(e.to_string()))?
    }
}

inventory::submit!(ToolFactory {
    build: || Box::new(AskUser)
});

#[cfg(test)]
mod tests {
    use super::*;
    use evo_policy::Permission;
    use serde_json::json;

    #[tokio::test]
    async fn ask_user_stubs_when_non_interactive() {
        let ctx = ToolContext::default().with_max_permission(Permission::P3);
        let out = AskUser.run(&ctx, json!({"message": "x"})).await.unwrap();
        assert!(out.starts_with("[ask_user-stub]"));
    }
}
