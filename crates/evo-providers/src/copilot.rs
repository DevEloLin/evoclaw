//! GitHub Copilot provider via the public OAuth Device Flow.
//!
//! - Long-lived `ghu_*` token saved at `~/.evoclaw/secrets/copilot.token`
//! - Short-lived `tid=*` session token held in memory, refreshed on demand
//! - Body wire format = OpenAI-compat (Copilot accepts `chat/completions`)

use crate::{ChatRequest, Message, Provider, ProviderError, Role, StreamEvent, ToolCall, ToolPayload, ToolSpec, Usage};
use async_trait::async_trait;
use futures::stream::{self, BoxStream, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::sync::Mutex;

const CLIENT_ID: &str = "Iv1.b507a08c87ecfe98";
const DEVICE_CODE_URL: &str = "https://github.com/login/device/code";
const ACCESS_TOKEN_URL: &str = "https://github.com/login/oauth/access_token";
const COPILOT_TOKEN_URL: &str = "https://api.github.com/copilot_internal/v2/token";
const COPILOT_CHAT_URL: &str = "https://api.githubcopilot.com/chat/completions";
const EDITOR_VERSION: &str = "vscode/1.85.0";
const EDITOR_PLUGIN_VERSION: &str = "copilot-chat/0.20.0";

#[derive(Debug, Deserialize)]
pub struct DeviceCodeResponse {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    #[serde(default = "default_interval")]
    pub interval: u64,
    pub expires_in: u64,
}
fn default_interval() -> u64 { 5 }

#[derive(Debug, Serialize)]
struct DeviceCodeRequest<'a> { client_id: &'a str, scope: &'a str }

#[derive(Debug, Serialize)]
struct AccessTokenRequest<'a> { client_id: &'a str, device_code: &'a str, grant_type: &'a str }

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum AccessTokenResponse {
    Pending { error: String },
    Granted { access_token: String },
}

pub async fn request_device_code(client: &reqwest::Client) -> Result<DeviceCodeResponse, ProviderError> {
    let resp = client.post(DEVICE_CODE_URL)
        .header("Accept", "application/json")
        .json(&DeviceCodeRequest { client_id: CLIENT_ID, scope: "read:user" })
        .send().await?;
    if !resp.status().is_success() {
        let status = resp.status().as_u16();
        let body = resp.text().await.unwrap_or_default();
        return Err(ProviderError::Status { status, body });
    }
    resp.json().await.map_err(|e| ProviderError::Decode(e.to_string()))
}

pub async fn poll_access_token(
    client: &reqwest::Client,
    device_code: &str,
    interval_secs: u64,
    timeout_secs: u64,
) -> Result<String, ProviderError> {
    let started = std::time::Instant::now();
    loop {
        if started.elapsed().as_secs() > timeout_secs {
            return Err(ProviderError::Auth("device flow timed out".into()));
        }
        tokio::time::sleep(std::time::Duration::from_secs(interval_secs.max(2))).await;
        let resp = client.post(ACCESS_TOKEN_URL)
            .header("Accept", "application/json")
            .json(&AccessTokenRequest {
                client_id: CLIENT_ID,
                device_code,
                grant_type: "urn:ietf:params:oauth:grant-type:device_code",
            })
            .send().await?;
        let parsed: AccessTokenResponse = resp.json().await
            .map_err(|e| ProviderError::Decode(e.to_string()))?;
        match parsed {
            AccessTokenResponse::Pending { error } => {
                if error == "authorization_pending" || error == "slow_down" { continue; }
                return Err(ProviderError::Auth(error));
            }
            AccessTokenResponse::Granted { access_token } => return Ok(access_token),
        }
    }
}

#[derive(Debug, Deserialize)]
struct CopilotTokenResponse { token: String, expires_at: u64 }

#[derive(Debug, Clone)]
struct EphemeralToken { token: String, expires_at_unix: u64 }

#[derive(Debug, Clone)]
pub struct CopilotProvider {
    github_token: String,
    pub model: String,
    client: reqwest::Client,
    cache: Arc<Mutex<Option<EphemeralToken>>>,
}

impl CopilotProvider {
    pub fn new(github_token: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            github_token: github_token.into(),
            model: model.into(),
            client: reqwest::Client::new(),
            cache: Arc::new(Mutex::new(None)),
        }
    }

    async fn fresh_session_token(&self) -> Result<String, ProviderError> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs();
        {
            let cache = self.cache.lock().await;
            if let Some(tok) = cache.as_ref() {
                if tok.expires_at_unix > now + 60 {
                    return Ok(tok.token.clone());
                }
            }
        }
        let resp = self.client.get(COPILOT_TOKEN_URL)
            .header("Authorization", format!("token {}", self.github_token))
            .header("Editor-Version", EDITOR_VERSION)
            .header("Editor-Plugin-Version", EDITOR_PLUGIN_VERSION)
            .header("User-Agent", "GitHubCopilot/1.0")
            .send().await?;
        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::Status { status, body });
        }
        let parsed: CopilotTokenResponse = resp.json().await
            .map_err(|e| ProviderError::Decode(e.to_string()))?;
        let tok = EphemeralToken { token: parsed.token, expires_at_unix: parsed.expires_at };
        let copy = tok.token.clone();
        let mut cache = self.cache.lock().await;
        *cache = Some(tok);
        Ok(copy)
    }
}

#[async_trait]
impl Provider for CopilotProvider {
    async fn stream(
        &self,
        req: ChatRequest,
    ) -> Result<BoxStream<'static, Result<StreamEvent, ProviderError>>, ProviderError> {
        let session_token = self.fresh_session_token().await?;
        let body = build_body(&req);
        let resp = self.client.post(COPILOT_CHAT_URL)
            .header("Authorization", format!("Bearer {session_token}"))
            .header("Editor-Version", EDITOR_VERSION)
            .header("Editor-Plugin-Version", EDITOR_PLUGIN_VERSION)
            .header("Copilot-Integration-Id", "vscode-chat")
            .header("OpenAI-Intent", "conversation-edits")
            .json(&body)
            .send().await?;
        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::Status { status, body });
        }
        let raw: RawOpenAiLikeResponse = resp.json().await
            .map_err(|e| ProviderError::Decode(e.to_string()))?;
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
        Role::System => "system", Role::User => "user",
        Role::Assistant => "assistant", Role::Tool => "tool",
    };
    let mut v = json!({"role": role, "content": m.content});
    if !m.tool_calls.is_empty() {
        v["tool_calls"] = Value::Array(m.tool_calls.iter().map(|c| json!({
            "id": c.id, "type": "function",
            "function": {"name": c.name, "arguments": c.arguments.to_string()}
        })).collect());
    }
    if !m.tool_results.is_empty() {
        if let Some(tr) = m.tool_results.first() {
            v = json!({"role": "tool", "tool_call_id": tr.call_id, "content": tr.content});
        }
    }
    v
}

fn serialize_tool(t: &ToolSpec) -> Value {
    json!({"type": "function", "function": {"name": t.name, "description": t.description, "parameters": t.schema}})
}

#[derive(Debug, Deserialize)]
struct RawOpenAiLikeResponse { choices: Vec<RawChoice>, #[serde(default)] usage: Option<RawUsage> }
#[derive(Debug, Deserialize)]
struct RawChoice { message: RawMsg }
#[derive(Debug, Deserialize)]
struct RawMsg {
    #[serde(default)] content: Option<String>,
    #[serde(default)] tool_calls: Vec<RawToolCall>,
}
#[derive(Debug, Deserialize)]
struct RawToolCall { id: String, function: RawFn }
#[derive(Debug, Deserialize)]
struct RawFn { name: String, arguments: String }
#[derive(Debug, Deserialize)]
struct RawUsage {
    prompt_tokens: u64, completion_tokens: u64,
    #[serde(default)] cached_tokens: u64,
}

impl RawOpenAiLikeResponse {
    fn into_events(self) -> Vec<StreamEvent> {
        let mut events = Vec::new();
        if let Some(c) = self.choices.into_iter().next() {
            if let Some(text) = c.message.content.filter(|s| !s.is_empty()) {
                events.push(StreamEvent::Delta(text));
            }
            for tc in c.message.tool_calls {
                let arguments: Value = serde_json::from_str(&tc.function.arguments).unwrap_or(Value::Null);
                events.push(StreamEvent::ToolCallStart(ToolCall {
                    id: tc.id, name: tc.function.name, arguments,
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
    fn build_body_includes_tools_when_full() {
        let spec = ToolSpec { name: "x".into(), description: "y".into(), schema: json!({}) };
        let req = ChatRequest {
            model: "gpt-4o".into(),
            messages: vec![Message::user("hi")],
            tools: ToolPayload::Full(vec![spec]),
            max_tokens: 100, temperature: 0.0,
        };
        let body = build_body(&req);
        let tools = body.get("tools").unwrap().as_array().unwrap();
        assert_eq!(tools[0]["function"]["name"], "x");
    }

    #[test]
    fn raw_response_synthesises_text_and_finish() {
        let raw = RawOpenAiLikeResponse {
            choices: vec![RawChoice { message: RawMsg { content: Some("ok".into()), tool_calls: vec![] } }],
            usage: Some(RawUsage { prompt_tokens: 10, completion_tokens: 1, cached_tokens: 0 }),
        };
        let events = raw.into_events();
        assert!(matches!(events[0], StreamEvent::Delta(_)));
        assert!(matches!(events.last().unwrap(), StreamEvent::Done));
    }

    #[test]
    fn ephemeral_token_struct_clones() {
        let t = EphemeralToken { token: "tid=abc".into(), expires_at_unix: 9999 };
        let c = t.clone();
        assert_eq!(c.token, "tid=abc");
    }
}
