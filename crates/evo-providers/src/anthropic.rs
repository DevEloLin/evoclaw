//! Anthropic native API client (Messages API).

use crate::{
    CacheKind, ChatRequest, Message, Provider, ProviderError, Role, StreamEvent, ToolCall,
    ToolPayload, ToolSpec, Usage,
};
use async_trait::async_trait;
use futures::stream::{self, BoxStream, StreamExt};
use serde::Deserialize;
use serde_json::{json, Value};

const ANTHROPIC_VERSION: &str = "2023-06-01";

#[derive(Debug, Clone)]
pub struct AnthropicProvider {
    base_url: String,
    api_key: String,
    pub model: String,
    client: reqwest::Client,
}

impl AnthropicProvider {
    pub fn new(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self::with_base_url("https://api.anthropic.com/v1", api_key, model)
    }
    pub fn with_base_url(
        base_url: impl Into<String>,
        api_key: impl Into<String>,
        model: impl Into<String>,
    ) -> Self {
        Self {
            base_url: base_url.into().trim_end_matches('/').to_string(),
            api_key: api_key.into(),
            model: model.into(),
            client: reqwest::Client::new(),
        }
    }
    fn endpoint(&self) -> String {
        format!("{}/messages", self.base_url)
    }
}

#[async_trait]
impl Provider for AnthropicProvider {
    async fn stream(
        &self,
        req: ChatRequest,
    ) -> Result<BoxStream<'static, Result<StreamEvent, ProviderError>>, ProviderError> {
        let body = build_body(&req);
        let resp = self
            .client
            .post(self.endpoint())
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", ANTHROPIC_VERSION)
            .json(&body)
            .send()
            .await?;
        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::Status { status, body });
        }
        let raw: RawResponse = resp
            .json()
            .await
            .map_err(|e| ProviderError::Decode(e.to_string()))?;
        let events = raw.into_events();
        Ok(stream::iter(events.into_iter().map(Ok)).boxed())
    }
}

fn build_body(req: &ChatRequest) -> Value {
    let mut system_text: Option<String> = None;
    let mut convo: Vec<&Message> = Vec::new();
    for m in &req.messages {
        if m.role == Role::System {
            system_text = Some(m.content.clone());
        } else {
            convo.push(m);
        }
    }
    let messages: Vec<Value> = convo.iter().map(|m| serialize_message(m)).collect();

    let mut body = json!({
        "model": req.model,
        "max_tokens": req.max_tokens,
        "temperature": req.temperature,
        "messages": messages,
    });
    if let Some(s) = system_text {
        body["system"] = system_block(&s, system_cache(req));
    }
    if let ToolPayload::Full(specs) = &req.tools {
        if !specs.is_empty() {
            body["tools"] = Value::Array(specs.iter().map(serialize_tool).collect());
        }
    }
    body
}

fn system_cache(req: &ChatRequest) -> bool {
    req.messages.iter().any(|m| {
        m.role == Role::System
            && matches!(
                m.cache_control,
                CacheKind::Persistent | CacheKind::Ephemeral
            )
    })
}

fn system_block(text: &str, cache: bool) -> Value {
    if cache {
        Value::Array(vec![json!({
            "type": "text",
            "text": text,
            "cache_control": { "type": "ephemeral" }
        })])
    } else {
        Value::String(text.to_string())
    }
}

fn serialize_message(m: &Message) -> Value {
    let role = match m.role {
        Role::User | Role::Tool | Role::System => "user",
        Role::Assistant => "assistant",
    };
    let mut blocks: Vec<Value> = Vec::new();
    if !m.tool_results.is_empty() {
        for tr in &m.tool_results {
            blocks.push(json!({
                "type": "tool_result",
                "tool_use_id": tr.call_id,
                "content": tr.content,
                "is_error": tr.is_error,
            }));
        }
    }
    if !m.content.is_empty() && !blocks.iter().any(|b| b["type"] == "tool_result") {
        let mut text_block = json!({"type": "text", "text": m.content});
        if matches!(m.cache_control, CacheKind::Ephemeral) {
            text_block["cache_control"] = json!({"type": "ephemeral"});
        }
        blocks.push(text_block);
    }
    if !m.tool_calls.is_empty() {
        for tc in &m.tool_calls {
            blocks.push(json!({
                "type": "tool_use",
                "id": tc.id,
                "name": tc.name,
                "input": tc.arguments,
            }));
        }
    }
    if blocks.is_empty() {
        blocks.push(json!({"type": "text", "text": m.content}));
    }
    json!({"role": role, "content": blocks})
}

fn serialize_tool(t: &ToolSpec) -> Value {
    json!({
        "name": t.name,
        "description": t.description,
        "input_schema": t.schema,
    })
}

#[derive(Debug, Deserialize)]
struct RawResponse {
    content: Vec<RawContentBlock>,
    #[serde(default)]
    usage: Option<RawUsage>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum RawContentBlock {
    Text {
        text: String,
    },
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
    #[serde(other)]
    Other,
}

#[derive(Debug, Deserialize)]
struct RawUsage {
    input_tokens: u64,
    output_tokens: u64,
    #[serde(default)]
    cache_read_input_tokens: u64,
}

impl RawResponse {
    fn into_events(self) -> Vec<StreamEvent> {
        let mut events = Vec::new();
        for block in self.content {
            match block {
                RawContentBlock::Text { text } if !text.is_empty() => {
                    events.push(StreamEvent::Delta(text));
                }
                RawContentBlock::ToolUse { id, name, input } => {
                    events.push(StreamEvent::ToolCallStart(ToolCall {
                        id,
                        name,
                        arguments: input,
                    }));
                }
                _ => {}
            }
        }
        events.push(StreamEvent::ToolCallFinish);
        if let Some(u) = self.usage {
            events.push(StreamEvent::Usage(Usage {
                input_tokens: u.input_tokens,
                cached_tokens: u.cache_read_input_tokens,
                output_tokens: u.output_tokens,
            }));
        }
        events.push(StreamEvent::Done);
        events
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_is_messages() {
        let p = AnthropicProvider::new("k", "claude-3-5-sonnet-20241022");
        assert_eq!(p.endpoint(), "https://api.anthropic.com/v1/messages");
    }

    #[test]
    fn build_body_extracts_system_field() {
        let req = ChatRequest {
            model: "claude-3-5-sonnet-20241022".into(),
            messages: vec![Message::system("be brief"), Message::user("hi")],
            tools: ToolPayload::Full(Vec::new()),
            max_tokens: 100,
            temperature: 0.2,
        };
        let body = build_body(&req);
        assert!(body.get("system").is_some(), "system field missing");
        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 1, "system should be removed from messages");
    }

    #[test]
    fn build_body_includes_cache_control_when_persistent() {
        let mut sys = Message::system("cached system");
        sys.cache_control = CacheKind::Persistent;
        let req = ChatRequest {
            model: "claude".into(),
            messages: vec![sys, Message::user("hi")],
            tools: ToolPayload::Full(Vec::new()),
            max_tokens: 100,
            temperature: 0.0,
        };
        let body = build_body(&req);
        let sysv = &body["system"];
        assert!(sysv.is_array(), "system should be array when cached");
        assert_eq!(sysv[0]["cache_control"]["type"], "ephemeral");
    }

    #[test]
    fn serialize_tool_uses_input_schema_not_parameters() {
        let t = ToolSpec {
            name: "x".into(),
            description: "y".into(),
            schema: json!({"type": "object"}),
        };
        let s = serialize_tool(&t);
        assert!(s.get("input_schema").is_some());
        assert!(s.get("parameters").is_none());
    }

    #[test]
    fn raw_response_synthesises_events_with_tool_use() {
        let raw = RawResponse {
            content: vec![
                RawContentBlock::Text {
                    text: "thinking".into(),
                },
                RawContentBlock::ToolUse {
                    id: "toolu_1".into(),
                    name: "read_file".into(),
                    input: json!({"path": "x"}),
                },
            ],
            usage: Some(RawUsage {
                input_tokens: 100,
                output_tokens: 5,
                cache_read_input_tokens: 60,
            }),
        };
        let events = raw.into_events();
        assert!(matches!(events[0], StreamEvent::Delta(_)));
        assert!(matches!(events[1], StreamEvent::ToolCallStart(_)));
        assert!(matches!(events.last().unwrap(), StreamEvent::Done));
    }

    #[test]
    fn raw_response_no_tool_call_terminates_uniformly() {
        let raw = RawResponse {
            content: vec![RawContentBlock::Text {
                text: "hello".into(),
            }],
            usage: None,
        };
        let events = raw.into_events();
        assert!(events
            .iter()
            .any(|e| matches!(e, StreamEvent::ToolCallFinish)));
        assert!(matches!(events.last().unwrap(), StreamEvent::Done));
    }
}
