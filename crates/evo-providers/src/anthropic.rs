//! Anthropic native API client (Messages API) — real SSE token streaming.
//!
//! Sends `"stream": true` and parses the Anthropic server-sent event protocol:
//! `content_block_delta` with `text_delta` events are emitted as `Delta` immediately.
//! Tool-use JSON fragments from `input_json_delta` are accumulated per block index
//! and emitted as complete `ToolCallStart` events on `content_block_stop`.
//! Final usage is collected from `message_start` + `message_delta` and emitted
//! as a single `Usage` event on `message_stop`.

use crate::{
    CacheKind, ChatRequest, Message, Provider, ProviderError, Role, StreamEvent, ToolCall,
    ToolPayload, ToolSpec, Usage,
};
use async_trait::async_trait;
use eventsource_stream::Eventsource;
use futures::{channel::mpsc, stream::BoxStream, StreamExt};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;

const ANTHROPIC_VERSION: &str = "2023-06-01";

#[derive(Debug, Clone)]
pub struct AnthropicProvider {
    base_url: String,
    api_key: String,
    pub model: String,
    client: reqwest::Client,
    /// When `true`, authenticate with `Authorization: Bearer <token>` instead of
    /// `x-api-key`.  Set this for third-party gateways that use OpenAI-style auth
    /// but serve the Anthropic Messages API format on `/v1/messages`.
    bearer_auth: bool,
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
            bearer_auth: false,
        }
    }
    /// Switch to `Authorization: Bearer <token>` auth.
    /// Use when the gateway serves Anthropic-format requests but authenticates
    /// via the OpenAI Bearer convention rather than `x-api-key`.
    pub fn with_bearer_auth(mut self) -> Self {
        self.bearer_auth = true;
        self
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
        let mut builder = self.client.post(self.endpoint());
        if self.bearer_auth {
            builder = builder.bearer_auth(&self.api_key);
        } else {
            builder = builder
                .header("x-api-key", &self.api_key)
                .header("anthropic-version", ANTHROPIC_VERSION);
        }
        let resp = builder.json(&body).send().await?;
        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::Status { status, body });
        }

        let (tx, rx) = mpsc::unbounded::<Result<StreamEvent, ProviderError>>();
        let mut sse = resp.bytes_stream().eventsource();

        tokio::spawn(async move {
            // tool_acc[block_index] = (id, name, accumulated_json)
            let mut tool_acc: HashMap<usize, (String, String, String)> = HashMap::new();
            let mut input_tokens: u64 = 0;
            let mut cached_tokens: u64 = 0;
            let mut output_tokens: u64 = 0;

            while let Some(item) = sse.next().await {
                match item {
                    Err(e) => {
                        let _ = tx.unbounded_send(Err(ProviderError::Decode(e.to_string())));
                        return;
                    }
                    Ok(event) => {
                        let ev: SseEvent = match serde_json::from_str(&event.data) {
                            Ok(e) => e,
                            Err(_) => continue,
                        };
                        match ev {
                            SseEvent::MessageStart { message } => {
                                if let Some(u) = message.usage {
                                    input_tokens = u.input_tokens;
                                    cached_tokens = u.cache_read_input_tokens;
                                }
                            }
                            SseEvent::ContentBlockStart { index, content_block } => {
                                if let SseContentBlock::ToolUse { id, name } = content_block {
                                    tool_acc.insert(index, (id, name, String::new()));
                                }
                            }
                            #[allow(clippy::collapsible_match)] // guard can't move `text`
                            SseEvent::ContentBlockDelta { index, delta } => match delta {
                                SseDelta::TextDelta { text } => {
                                    if !text.is_empty()
                                        && tx
                                            .unbounded_send(Ok(StreamEvent::Delta(text)))
                                            .is_err()
                                    {
                                        return;
                                    }
                                }
                                SseDelta::InputJsonDelta { partial_json } => {
                                    if let Some(entry) = tool_acc.get_mut(&index) {
                                        entry.2.push_str(&partial_json);
                                    }
                                }
                                _ => {}
                            },
                            SseEvent::ContentBlockStop { index } => {
                                if let Some((id, name, args_str)) = tool_acc.remove(&index) {
                                    let arguments =
                                        serde_json::from_str(&args_str).unwrap_or(Value::Null);
                                    let _ = tx.unbounded_send(Ok(StreamEvent::ToolCallStart(
                                        ToolCall { id, name, arguments },
                                    )));
                                }
                            }
                            SseEvent::MessageDelta { usage } => {
                                if let Some(u) = usage {
                                    output_tokens = u.output_tokens;
                                }
                            }
                            SseEvent::MessageStop => {
                                let _ = tx.unbounded_send(Ok(StreamEvent::ToolCallFinish));
                                let _ = tx.unbounded_send(Ok(StreamEvent::Usage(Usage {
                                    input_tokens,
                                    cached_tokens,
                                    output_tokens,
                                })));
                                let _ = tx.unbounded_send(Ok(StreamEvent::Done));
                                return;
                            }
                            SseEvent::Ping | SseEvent::Unknown => {}
                        }
                    }
                }
            }

            // Stream ended without message_stop
            let _ = tx.unbounded_send(Ok(StreamEvent::ToolCallFinish));
            let _ = tx.unbounded_send(Ok(StreamEvent::Done));
        });

        Ok(rx.boxed())
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
        "stream": true,
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

// ── SSE event deserialization ─────────────────────────────────────────────────

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum SseEvent {
    MessageStart {
        message: SseMessage,
    },
    ContentBlockStart {
        index: usize,
        content_block: SseContentBlock,
    },
    ContentBlockDelta {
        index: usize,
        delta: SseDelta,
    },
    ContentBlockStop {
        index: usize,
    },
    MessageDelta {
        #[serde(default)]
        usage: Option<SseOutputUsage>,
    },
    MessageStop,
    Ping,
    #[serde(other)]
    Unknown,
}

#[derive(Deserialize)]
struct SseMessage {
    #[serde(default)]
    usage: Option<SseInputUsage>,
}

#[derive(Deserialize)]
struct SseInputUsage {
    input_tokens: u64,
    #[serde(default)]
    cache_read_input_tokens: u64,
}

#[derive(Deserialize)]
struct SseOutputUsage {
    output_tokens: u64,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum SseContentBlock {
    Text {
        #[allow(dead_code)]
        text: String,
    },
    ToolUse {
        id: String,
        name: String,
    },
    #[serde(other)]
    Other,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum SseDelta {
    TextDelta {
        text: String,
    },
    InputJsonDelta {
        partial_json: String,
    },
    #[serde(other)]
    Other,
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_is_messages() {
        let p = AnthropicProvider::new("k", "claude-3-5-sonnet-20241022");
        assert_eq!(p.endpoint(), "https://api.anthropic.com/v1/messages");
    }

    #[test]
    fn with_bearer_auth_sets_flag() {
        let p = AnthropicProvider::new("k", "m");
        assert!(!p.bearer_auth, "default should be x-api-key auth");
        let p = p.with_bearer_auth();
        assert!(p.bearer_auth, "with_bearer_auth should flip to true");
    }

    #[test]
    fn with_bearer_auth_preserves_base_url() {
        let p = AnthropicProvider::with_base_url("https://gateway.example.com/v1", "token", "claude-opus-4.6")
            .with_bearer_auth();
        assert_eq!(p.endpoint(), "https://gateway.example.com/v1/messages");
        assert!(p.bearer_auth);
    }

    #[test]
    fn build_body_sets_stream_true() {
        let req = ChatRequest {
            model: "claude-3-5-sonnet-20241022".into(),
            messages: vec![Message::user("hi")],
            tools: ToolPayload::Full(Vec::new()),
            max_tokens: 100,
            temperature: 0.2,
        };
        let body = build_body(&req);
        assert_eq!(body["stream"], json!(true));
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
    fn sse_event_deserialises_message_start() {
        let data = r#"{"type":"message_start","message":{"usage":{"input_tokens":42,"cache_read_input_tokens":10}}}"#;
        let ev: SseEvent = serde_json::from_str(data).unwrap();
        let SseEvent::MessageStart { message } = ev else { panic!("wrong variant") };
        let u = message.usage.unwrap();
        assert_eq!(u.input_tokens, 42);
        assert_eq!(u.cache_read_input_tokens, 10);
    }

    #[test]
    fn sse_event_deserialises_text_delta() {
        let data = r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"hello"}}"#;
        let ev: SseEvent = serde_json::from_str(data).unwrap();
        let SseEvent::ContentBlockDelta { index, delta } = ev else { panic!("wrong variant") };
        assert_eq!(index, 0);
        let SseDelta::TextDelta { text } = delta else { panic!("wrong delta") };
        assert_eq!(text, "hello");
    }

    #[test]
    fn sse_event_deserialises_tool_use_block_start() {
        let data = r#"{"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"toolu_01","name":"read_file","input":{}}}"#;
        let ev: SseEvent = serde_json::from_str(data).unwrap();
        let SseEvent::ContentBlockStart { index, content_block } = ev else {
            panic!("wrong variant")
        };
        assert_eq!(index, 1);
        let SseContentBlock::ToolUse { id, name } = content_block else {
            panic!("wrong block type")
        };
        assert_eq!(id, "toolu_01");
        assert_eq!(name, "read_file");
    }

    #[test]
    fn sse_event_deserialises_input_json_delta() {
        let data = r#"{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"path\":"}}"#;
        let ev: SseEvent = serde_json::from_str(data).unwrap();
        let SseEvent::ContentBlockDelta { delta, .. } = ev else { panic!("wrong variant") };
        let SseDelta::InputJsonDelta { partial_json } = delta else { panic!("wrong delta") };
        assert_eq!(partial_json, "{\"path\":");
    }

    #[test]
    fn sse_event_deserialises_message_stop() {
        let data = r#"{"type":"message_stop"}"#;
        let ev: SseEvent = serde_json::from_str(data).unwrap();
        assert!(matches!(ev, SseEvent::MessageStop));
    }

    #[test]
    fn sse_event_deserialises_message_delta_with_usage() {
        let data = r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":15}}"#;
        let ev: SseEvent = serde_json::from_str(data).unwrap();
        let SseEvent::MessageDelta { usage } = ev else { panic!("wrong variant") };
        assert_eq!(usage.unwrap().output_tokens, 15);
    }

    #[test]
    fn sse_ping_is_ignored() {
        let data = r#"{"type":"ping"}"#;
        let ev: SseEvent = serde_json::from_str(data).unwrap();
        assert!(matches!(ev, SseEvent::Ping));
    }
}
