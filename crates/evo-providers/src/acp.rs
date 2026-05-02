//! AcpProvider — adapts an ACP agent CLI to EvoClaw's Provider trait.
//!
//! ACP agents are full agents (claude-code / codex / cursor / gh copilot);
//! they manage their own tool-use loop. EvoClaw's runtime treats them as
//! black-box turn responders: latest user message → ACP `session/prompt` →
//! final text.

use crate::{ChatRequest, Provider, ProviderError, Role, StreamEvent, Usage};
use async_trait::async_trait;
use futures::stream::{self, BoxStream, StreamExt};
use std::sync::Arc;

pub struct AcpProvider {
    client: Arc<evo_acp_client::AcpClient>,
    pub agent_id: String,
}

impl AcpProvider {
    pub async fn spawn(agent_id: &str) -> Result<Self, ProviderError> {
        let cfg = evo_acp_client::load_agent(agent_id).await
            .map_err(|e| ProviderError::Auth(format!(
                "load agent {agent_id}: {e}; run `evoclaw agent add {agent_id}` first")))?;
        let client = Arc::new(evo_acp_client::AcpClient::new());
        client.spawn(&cfg).await
            .map_err(|e| ProviderError::Auth(format!(
                "spawn {} failed: {e}; install: {}",
                cfg.command,
                evo_acp_client::find_agent(agent_id).map(|p| p.install_hint).unwrap_or("(catalog)"),
            )))?;
        client.initialize("evoclaw", env!("CARGO_PKG_VERSION")).await
            .map_err(|e| ProviderError::Auth(format!("ACP initialize: {e}")))?;
        Ok(Self { client, agent_id: agent_id.into() })
    }
}

#[async_trait]
impl Provider for AcpProvider {
    async fn stream(
        &self,
        req: ChatRequest,
    ) -> Result<BoxStream<'static, Result<StreamEvent, ProviderError>>, ProviderError> {
        let prompt_text = req.messages.iter()
            .rev()
            .find(|m| matches!(m.role, Role::User))
            .map(|m| m.content.clone())
            .unwrap_or_default();
        let session_id = self.client.new_session().await
            .map_err(|e| ProviderError::Auth(format!("session/new: {e}")))?;
        let response = self.client.prompt(&session_id, &prompt_text).await
            .map_err(|e| ProviderError::Auth(format!("session/prompt: {e}")))?;
        let text = extract_response_text(&response);
        let mut events: Vec<Result<StreamEvent, ProviderError>> = Vec::new();
        if !text.is_empty() {
            events.push(Ok(StreamEvent::Delta(text)));
        }
        events.push(Ok(StreamEvent::ToolCallFinish));
        events.push(Ok(StreamEvent::Usage(Usage::default())));
        events.push(Ok(StreamEvent::Done));
        Ok(stream::iter(events).boxed())
    }
}

fn extract_response_text(v: &serde_json::Value) -> String {
    if let Some(blocks) = v.get("message").and_then(|m| m.get("content")).and_then(|c| c.as_array()) {
        let mut s = String::new();
        for b in blocks {
            if b.get("type").and_then(|t| t.as_str()) == Some("text") {
                if let Some(t) = b.get("text").and_then(|t| t.as_str()) { s.push_str(t); }
            }
        }
        if !s.is_empty() { return s; }
    }
    if let Some(blocks) = v.get("content").and_then(|c| c.as_array()) {
        let mut s = String::new();
        for b in blocks {
            if b.get("type").and_then(|t| t.as_str()) == Some("text") {
                if let Some(t) = b.get("text").and_then(|t| t.as_str()) { s.push_str(t); }
            }
        }
        if !s.is_empty() { return s; }
    }
    if let Some(t) = v.as_str() { return t.to_string(); }
    if let Some(t) = v.get("text").and_then(|t| t.as_str()) { return t.to_string(); }
    serde_json::to_string(v).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn extract_message_content_blocks() {
        let v = json!({"message": {"content": [
            {"type":"text","text":"hello "}, {"type":"text","text":"world"}
        ]}});
        assert_eq!(extract_response_text(&v), "hello world");
    }
    #[test]
    fn extract_top_level_content_blocks() {
        let v = json!({"content": [{"type":"text","text":"ok"}]});
        assert_eq!(extract_response_text(&v), "ok");
    }
    #[test]
    fn extract_flat_string() {
        let v = json!("plain");
        assert_eq!(extract_response_text(&v), "plain");
    }
    #[test]
    fn extract_text_field() {
        let v = json!({"text": "hi"});
        assert_eq!(extract_response_text(&v), "hi");
    }
    #[test]
    fn extract_unknown_falls_back_to_json() {
        let v = json!({"weird": 42});
        assert!(extract_response_text(&v).contains("weird"));
    }
}
