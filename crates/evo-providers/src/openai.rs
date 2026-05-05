//! OpenAI-compatible client — real SSE token streaming.
//!
//! Sends `"stream": true` and parses server-sent events one token at a time.
//! Tool-call arguments are accumulated across delta events and emitted as
//! complete `ToolCallStart` events when `[DONE]` arrives.
//!
//! Covers: OpenAI, DeepSeek, Kimi, Qwen, Groq, OpenRouter, vLLM, Ollama, and
//! any third-party gateway implementing the OpenAI Chat Completions API.

use crate::{
    ChatRequest, Message, Provider, ProviderError, Role, StreamEvent, ToolCall, ToolPayload,
    ToolSpec, Usage,
};
use async_trait::async_trait;
use eventsource_stream::Eventsource;
use futures::{channel::mpsc, stream::BoxStream, StreamExt};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct OpenAiCompatProvider {
    base_url: String,
    api_key: String,
    pub model: String,
    client: reqwest::Client,
}

impl OpenAiCompatProvider {
    pub fn new(
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

            // Template fix: third-party gateways may serve both OpenAI-compat and
            // Anthropic-format models behind the same base URL.  When the gateway
            // rejects a request with `model_endpoint_mismatch` and the model belongs
            // to `access_group=messages`, it requires the Anthropic Messages API
            // format.  Transparently retry with AnthropicProvider (same base_url /
            // api_key) so the caller needs zero reconfiguration.
            if status == 400 && needs_messages_api(&body) {
                // Third-party gateway: same base_url, same Bearer token, but
                // Anthropic Messages API format on /v1/messages.
                let anthropic = crate::anthropic::AnthropicProvider::with_base_url(
                    &self.base_url,
                    &self.api_key,
                    &self.model,
                )
                .with_bearer_auth();
                return anthropic.stream(req).await;
            }

            return Err(ProviderError::Status { status, body });
        }

        let (tx, rx) = mpsc::unbounded::<Result<StreamEvent, ProviderError>>();
        let mut sse = resp.bytes_stream().eventsource();

        tokio::spawn(async move {
            // tool_acc[index] = (id, name, accumulated_args)
            let mut tool_acc: HashMap<usize, (String, String, String)> = HashMap::new();

            while let Some(item) = sse.next().await {
                match item {
                    Err(e) => {
                        let _ = tx.unbounded_send(Err(ProviderError::Decode(e.to_string())));
                        return;
                    }
                    Ok(event) => {
                        if event.data.trim() == "[DONE]" {
                            flush_tool_calls(&tx, tool_acc);
                            let _ = tx.unbounded_send(Ok(StreamEvent::ToolCallFinish));
                            let _ = tx.unbounded_send(Ok(StreamEvent::Done));
                            return;
                        }

                        let chunk: SseChunk = match serde_json::from_str(&event.data) {
                            Ok(c) => c,
                            Err(_) => continue,
                        };

                        if let Some(choice) = chunk.choices.first() {
                            if let Some(text) = &choice.delta.content {
                                if !text.is_empty()
                                    && tx
                                        .unbounded_send(Ok(StreamEvent::Delta(text.clone())))
                                        .is_err()
                                {
                                    return;
                                }
                            }
                            for tc in &choice.delta.tool_calls {
                                let entry = tool_acc.entry(tc.index).or_insert_with(|| {
                                    (
                                        tc.id.clone().unwrap_or_default(),
                                        tc.function.name.clone().unwrap_or_default(),
                                        String::new(),
                                    )
                                });
                                if let Some(id) = &tc.id {
                                    if !id.is_empty() && entry.0.is_empty() {
                                        entry.0.clone_from(id);
                                    }
                                }
                                if let Some(name) = &tc.function.name {
                                    if !name.is_empty() && entry.1.is_empty() {
                                        entry.1.clone_from(name);
                                    }
                                }
                                if let Some(args) = &tc.function.arguments {
                                    entry.2.push_str(args);
                                }
                            }
                        }

                        if let Some(u) = chunk.usage {
                            let _ = tx.unbounded_send(Ok(StreamEvent::Usage(Usage {
                                input_tokens: u.prompt_tokens,
                                cached_tokens: u.cached_tokens,
                                output_tokens: u.completion_tokens,
                            })));
                        }
                    }
                }
            }

            // Stream ended without [DONE]
            flush_tool_calls(&tx, tool_acc);
            let _ = tx.unbounded_send(Ok(StreamEvent::ToolCallFinish));
            let _ = tx.unbounded_send(Ok(StreamEvent::Done));
        });

        Ok(rx.boxed())
    }
}

fn flush_tool_calls(
    tx: &mpsc::UnboundedSender<Result<StreamEvent, ProviderError>>,
    tool_acc: HashMap<usize, (String, String, String)>,
) {
    let mut calls: Vec<(usize, String, String, String)> = tool_acc
        .into_iter()
        .map(|(idx, (id, name, args))| (idx, id, name, args))
        .collect();
    calls.sort_unstable_by_key(|(idx, ..)| *idx);
    for (_, id, name, args_str) in calls {
        let arguments = serde_json::from_str(&args_str).unwrap_or(Value::Null);
        let _ = tx.unbounded_send(Ok(StreamEvent::ToolCallStart(ToolCall {
            id,
            name,
            arguments,
        })));
    }
}

/// Returns `true` when a 400 error body signals that the model belongs to
/// `access_group=messages` and therefore requires the Anthropic Messages API
/// endpoint (`/v1/messages`) rather than OpenAI Chat Completions
/// (`/v1/chat/completions`).
///
/// Matches the `model_endpoint_mismatch` error code emitted by third-party
/// gateways (e.g. AstraGateway, LiteLLM, OpenRouter custom deployments) that
/// multiplex both API formats behind a single base URL.
fn needs_messages_api(body: &str) -> bool {
    let v: Value = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(_) => return false,
    };
    let code = v["error"]["code"].as_str().unwrap_or("");
    let message = v["error"]["message"].as_str().unwrap_or("");
    code == "model_endpoint_mismatch" && message.contains("access_group=messages")
}

fn build_body(req: &ChatRequest) -> Value {
    let messages: Vec<Value> = req.messages.iter().map(serialize_message).collect();
    let mut body = json!({
        "model": req.model,
        "messages": messages,
        "stream": true,
        "stream_options": { "include_usage": true },
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

// ── SSE chunk deserialization ─────────────────────────────────────────────────

#[derive(Deserialize)]
struct SseChunk {
    #[serde(default)]
    choices: Vec<SseChoice>,
    #[serde(default)]
    usage: Option<SseUsage>,
}

#[derive(Deserialize)]
struct SseChoice {
    delta: SseDelta,
}

#[derive(Deserialize, Default)]
struct SseDelta {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    tool_calls: Vec<SseToolCallDelta>,
}

#[derive(Deserialize)]
struct SseToolCallDelta {
    index: usize,
    #[serde(default)]
    id: Option<String>,
    function: SseFunctionDelta,
}

#[derive(Deserialize, Default)]
struct SseFunctionDelta {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    arguments: Option<String>,
}

#[derive(Deserialize)]
struct SseUsage {
    prompt_tokens: u64,
    completion_tokens: u64,
    #[serde(default)]
    cached_tokens: u64,
}

// ── Message / tool serializers ────────────────────────────────────────────────

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
            m.tool_calls
                .iter()
                .map(|c| {
                    json!({
                        "id": c.id,
                        "type": "function",
                        "function": { "name": c.name, "arguments": c.arguments.to_string() }
                    })
                })
                .collect(),
        );
    }
    if !m.tool_results.is_empty() {
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

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_strips_trailing_slash() {
        let p = OpenAiCompatProvider::new("https://api.example.com/v1/", "k", "m");
        assert_eq!(p.endpoint(), "https://api.example.com/v1/chat/completions");
    }

    #[test]
    fn build_body_uses_stream_true() {
        let req = ChatRequest {
            model: "gpt-4o".into(),
            messages: vec![Message::user("hi")],
            tools: ToolPayload::Reuse("…".into()),
            max_tokens: 100,
            temperature: 0.2,
        };
        let body = build_body(&req);
        assert_eq!(body["stream"], json!(true));
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
        let spec = ToolSpec {
            name: "read_file".into(),
            description: "x".into(),
            schema: json!({}),
        };
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
    fn sse_chunk_deserialises_text_delta() {
        let data = r#"{"choices":[{"delta":{"content":"hello"}}]}"#;
        let chunk: SseChunk = serde_json::from_str(data).unwrap();
        assert_eq!(chunk.choices[0].delta.content.as_deref(), Some("hello"));
    }

    #[test]
    fn sse_chunk_deserialises_tool_call_fragment() {
        let data = r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"get_weather","arguments":""}}]}}]}"#;
        let chunk: SseChunk = serde_json::from_str(data).unwrap();
        let tc = &chunk.choices[0].delta.tool_calls[0];
        assert_eq!(tc.index, 0);
        assert_eq!(tc.id.as_deref(), Some("call_1"));
        assert_eq!(tc.function.name.as_deref(), Some("get_weather"));
    }

    #[test]
    fn sse_chunk_deserialises_usage() {
        let data = r#"{"choices":[],"usage":{"prompt_tokens":10,"completion_tokens":5,"cached_tokens":3}}"#;
        let chunk: SseChunk = serde_json::from_str(data).unwrap();
        let u = chunk.usage.unwrap();
        assert_eq!(u.prompt_tokens, 10);
        assert_eq!(u.completion_tokens, 5);
        assert_eq!(u.cached_tokens, 3);
    }

    // ── needs_messages_api ───────────────────────────────────────────────────

    #[test]
    fn detects_messages_api_mismatch() {
        let body = r#"{"error":{"message":"Model 'claude-opus-4.6' (access_group=messages) cannot be used with endpoint /v1/chat/completions. Expected one of: chat, chat_responses","type":"invalid_request_error","code":"model_endpoint_mismatch"}}"#;
        assert!(
            needs_messages_api(body),
            "should detect messages access_group"
        );
    }

    #[test]
    fn ignores_different_error_code() {
        let body = r#"{"error":{"message":"invalid model","type":"invalid_request_error","code":"model_not_found"}}"#;
        assert!(!needs_messages_api(body));
    }

    #[test]
    fn ignores_mismatch_without_messages_access_group() {
        // model belongs to 'chat' group — should NOT trigger Anthropic fallback
        let body = r#"{"error":{"message":"Model 'gpt-4o' (access_group=chat) cannot be used with endpoint /v1/messages. Expected one of: messages","type":"invalid_request_error","code":"model_endpoint_mismatch"}}"#;
        assert!(!needs_messages_api(body));
    }

    #[test]
    fn ignores_non_json_body() {
        assert!(!needs_messages_api("Bad Gateway"));
        assert!(!needs_messages_api(""));
    }
}
