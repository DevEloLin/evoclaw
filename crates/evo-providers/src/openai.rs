//! OpenAI-compatible client (non-streaming for Phase 1).
//!
//! Phase 1 ships a non-streaming MVP that synthesizes a `BoxStream` of
//! `StreamEvent` from a single response. Real SSE streaming arrives in
//! Phase 3 (PRD §42 token-economy work). One mod covers DeepSeek, Kimi,
//! Qwen, Ollama, vLLM, OpenRouter — anything OpenAI-compat.

use crate::{ChatRequest, Message, ProviderError, Provider, Role, StreamEvent, ToolCall, ToolPayload, ToolSpec, Usage};
use async_trait::async_trait;
use futures::stream::{self, BoxStream, StreamExt};
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Debug, Clone)]
pub struct OpenAiCompatProvider {
    base_url: String,
    api_key: String,
    pub model: String,
    client: reqwest::Client,
}

impl OpenAiCompatProvider {
    pub fn new(base_url: impl Into<String>, api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into().trim_end_matches('/').to_string(),
            api_key: api_key.into(),
            model: model.into(),
            client: reqwest::Client::new(),
        }
    }

    fn endpoint(&self) -> String {
        format!("{}/chat/completions", self.base_url)
    }
}

#[async_trait]
impl Provider for OpenAiCompatProvider {
    async fn stream(
        &self,
        req: ChatRequest,
    ) -> Result<BoxStream<'static, Result<StreamEvent, ProviderError>>, ProviderError> {
        let body = build_body(&req);
        let resp = self
            .client
            .post(self.endpoint())
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await?;
        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::Status { status, body });
        }
        let raw: RawResponse = resp.json().await.map_err(|e| ProviderError::Decode(e.to_string()))?;
        let events = raw.into_events();
        Ok(stream::iter(events.into_iter().map(Ok)).boxed())
    }
}

fn build_body(req: &ChatRequest) -> Value {
    let messages: Vec<Value> = req.messages.iter().map(serialize_message).collect();
    let mut body = json!({
        "model": req.model,
        "messages": messages,
        "stream": false,
        "max_tokens": req.max_tokens,
        "temperature": req.temperature,
    });
    if let ToolPayload::Full(specs) = &req.tools {
        if !specs.is_empty() {
            body["tools"] = Value::Array(specs.iter().map(serialize_tool).collect());
        }
    }
    body
}

fn serialize_message(m: &Message) -> Value {
    let role = match m.role {
        Role::System => "system",
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::Tool => "tool",
    };
    let mut v = json!({ "role": role, "content": m.content });
    if !m.tool_calls.is_empty() {
        v["tool_calls"] = Value::Array(
            m.tool_calls.iter().map(|c| json!({
                "id": c.id,
                "type": "function",
                "function": { "name": c.name, "arguments": c.arguments.to_string() }
            })).collect(),
        );
    }
    if !m.tool_results.is_empty() {
        // OpenAI requires a separate tool message per result; emit the first here.
        if let Some(tr) = m.tool_results.first() {
            v = json!({
                "role": "tool",
                "tool_call_id": tr.call_id,
                "content": tr.content,
            });
        }
    }
    v
}

fn serialize_tool(t: &ToolSpec) -> Value {
    json!({
        "type": "function",
        "function": {
            "name": t.name,
            "description": t.description,
            "parameters": t.schema,
        }
    })
}

#[derive(Debug, Deserialize)]
struct RawResponse {
    choices: Vec<RawChoice>,
    #[serde(default)]
    usage: Option<RawUsage>,
}

#[derive(Debug, Deserialize)]
struct RawChoice {
    message: RawMessage,
}

#[derive(Debug, Deserialize)]
struct RawMessage {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    tool_calls: Vec<RawToolCall>,
}

#[derive(Debug, Deserialize)]
struct RawToolCall {
    id: String,
    function: RawFunction,
}

#[derive(Debug, Deserialize)]
struct RawFunction {
    name: String,
    /// Arguments arrive as a JSON-encoded string in OpenAI-compat APIs.
    arguments: String,
}

#[derive(Debug, Deserialize)]
struct RawUsage {
    prompt_tokens: u64,
    completion_tokens: u64,
    #[serde(default)]
    cached_tokens: u64,
}

impl RawResponse {
    fn into_events(self) -> Vec<StreamEvent> {
        let mut events = Vec::with_capacity(8);
        if let Some(choice) = self.choices.into_iter().next() {
            if let Some(text) = choice.message.content.filter(|s| !s.is_empty()) {
                events.push(StreamEvent::Delta(text));
            }
            for tc in choice.message.tool_calls {
                let arguments: Value = serde_json::from_str(&tc.function.arguments).unwrap_or(Value::Null);
                events.push(StreamEvent::ToolCallStart(ToolCall {
                    id: tc.id,
                    name: tc.function.name,
                    arguments,
                }));
            }
            events.push(StreamEvent::ToolCallFinish);
        }
        if let Some(u) = self.usage {
            events.push(StreamEvent::Usage(Usage {
                input_tokens: u.prompt_tokens,
                cached_tokens: u.cached_tokens,
                output_tokens: u.completion_tokens,
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
    fn endpoint_strips_trailing_slash() {
        let p = OpenAiCompatProvider::new("https://api.example.com/v1/", "k", "m");
        assert_eq!(p.endpoint(), "https://api.example.com/v1/chat/completions");
    }

    #[test]
    fn build_body_omits_tools_when_reuse() {
        let req = ChatRequest {
            model: "m".into(),
            messages: vec![Message::user("hi")],
            tools: ToolPayload::Reuse("…".into()),
            max_tokens: 100,
            temperature: 0.2,
        };
        let body = build_body(&req);
        assert!(body.get("tools").is_none());
    }

    #[test]
    fn build_body_includes_tools_when_full() {
        let spec = ToolSpec { name: "read_file".into(), description: "x".into(), schema: json!({}) };
        let req = ChatRequest {
            model: "m".into(),
            messages: vec![Message::user("hi")],
            tools: ToolPayload::Full(vec![spec]),
            max_tokens: 100,
            temperature: 0.2,
        };
        let body = build_body(&req);
        let tools = body.get("tools").unwrap().as_array().unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["function"]["name"], "read_file");
    }

    #[test]
    fn raw_response_synthesises_events() {
        let raw = RawResponse {
            choices: vec![RawChoice {
                message: RawMessage {
                    content: Some("hello".into()),
                    tool_calls: vec![],
                },
            }],
            usage: Some(RawUsage { prompt_tokens: 100, completion_tokens: 5, cached_tokens: 60 }),
        };
        let events = raw.into_events();
        // Delta, ToolCallFinish, Usage, Done
        assert_eq!(events.len(), 4);
        assert!(matches!(events[0], StreamEvent::Delta(_)));
        assert!(matches!(events.last().unwrap(), StreamEvent::Done));
    }
}
