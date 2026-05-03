//! evo-providers — model adapter trait + OpenAI-compatible client + tool-schema fingerprint.
//!
//! PRD §10.3 Model Router foundations + §42.1/§42.2 token-economy hooks.

use async_trait::async_trait;
use futures::stream::BoxStream;
use serde::{Deserialize, Serialize};

pub mod acp;
pub mod anthropic;
pub mod browser;
pub mod copilot;
pub mod fingerprint;
pub mod openai;

pub use acp::AcpProvider;
pub use anthropic::AnthropicProvider;
pub use browser::{
    AuthMethod, BrowserAuthShape, BrowserProfile, BrowserProvider, BrowserShapeRepr,
};
pub use copilot::CopilotProvider;
pub use fingerprint::{ToolFingerprint, ToolPayload};
pub use openai::OpenAiCompatProvider;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

/// Cache hint per PRD §42.2.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CacheKind {
    #[default]
    None,
    Ephemeral,
    Persistent,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_results: Vec<ToolResult>,
    #[serde(default, skip_serializing_if = "is_default_cache")]
    pub cache_control: CacheKind,
}

fn is_default_cache(c: &CacheKind) -> bool {
    matches!(c, CacheKind::None)
}

impl Message {
    pub fn user(text: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: text.into(),
            tool_calls: Vec::new(),
            tool_results: Vec::new(),
            cache_control: CacheKind::None,
        }
    }
    pub fn system(text: impl Into<String>) -> Self {
        Self {
            role: Role::System,
            content: text.into(),
            tool_calls: Vec::new(),
            tool_results: Vec::new(),
            cache_control: CacheKind::Persistent,
        }
    }
    pub fn assistant(text: impl Into<String>) -> Self {
        Self {
            role: Role::Assistant,
            content: text.into(),
            tool_calls: Vec::new(),
            tool_results: Vec::new(),
            cache_control: CacheKind::None,
        }
    }
}

/// Tool advertisement to the model. `description` ≤80 chars per PRD §43.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub schema: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    pub call_id: String,
    pub content: String,
    #[serde(default)]
    pub is_error: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Usage {
    pub input_tokens: u64,
    pub cached_tokens: u64,
    pub output_tokens: u64,
}

impl Usage {
    pub fn cache_hit_rate(&self) -> f64 {
        if self.input_tokens == 0 {
            0.0
        } else {
            self.cached_tokens as f64 / self.input_tokens as f64
        }
    }
}

#[derive(Debug, Clone)]
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<Message>,
    pub tools: ToolPayload,
    pub max_tokens: u32,
    pub temperature: f32,
}

#[derive(Debug, Clone)]
pub enum StreamEvent {
    Delta(String),
    ToolCallStart(ToolCall),
    ToolCallFinish,
    Usage(Usage),
    Done,
}

#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    #[error("http: {0}")]
    Http(#[from] reqwest::Error),
    #[error("decode: {0}")]
    Decode(String),
    #[error("status {status}: {body}")]
    Status { status: u16, body: String },
    #[error("auth: {0}")]
    Auth(String),
    #[error("budget: {0}")]
    Budget(String),
    /// Local pre-flight failure that has nothing to do with the upstream
    /// provider. Used when EvoClaw refuses to dispatch a request because
    /// of an internal invariant (e.g. fully-redacted prompt detected
    /// before send — see `prd/plan/acp.md`).
    #[error("{0}")]
    Other(String),
}

#[async_trait]
pub trait Provider: Send + Sync {
    async fn stream(
        &self,
        req: ChatRequest,
    ) -> Result<BoxStream<'static, Result<StreamEvent, ProviderError>>, ProviderError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_user_defaults_no_cache() {
        assert!(matches!(Message::user("hi").cache_control, CacheKind::None));
    }
    #[test]
    fn message_system_is_persistent() {
        assert!(matches!(
            Message::system("x").cache_control,
            CacheKind::Persistent
        ));
    }
    #[test]
    fn cache_hit_rate_zero_safe() {
        assert_eq!(Usage::default().cache_hit_rate(), 0.0);
    }
    #[test]
    fn cache_hit_rate_typical() {
        let u = Usage {
            input_tokens: 1000,
            cached_tokens: 700,
            output_tokens: 200,
        };
        assert!((u.cache_hit_rate() - 0.7).abs() < 1e-9);
    }
}
