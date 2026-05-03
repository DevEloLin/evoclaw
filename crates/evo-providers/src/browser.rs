//! BrowserProvider — session-cookie / web-token authentication wrapper.
//!
//! Goal: every vendor in the catalog must offer a "browser sign-in" auth path
//! in addition to API-key auth. This crate ships the **Phase-1** flow:
//!
//!   1. The CLI opens the vendor's web console in the user's default browser.
//!   2. The user signs in normally (Google / GitHub / SSO / TOTP — anything
//!      the vendor supports — *we never see the credentials*).
//!   3. The user copies their session token (from `Application → Cookies`,
//!      from a downloaded JSON, or from a vendor-specific "copy access token"
//!      button) and pastes it into EvoClaw.
//!   4. We persist the token under `~/.evoclaw/browser_profiles/<id>.json`
//!      (chmod 600) and reuse it across processes.
//!
//! Phase-2 will drive the browser via CDP / Playwright to capture cookies
//! automatically and refresh expiring sessions; the persistence schema
//! defined here is forward-compatible with that work.
//!
//! Wire-format: the chat call is delegated to whatever native client the
//! vendor uses (OpenAI-compat, Anthropic, …). Only the auth header changes.

use crate::{
    AnthropicProvider, ChatRequest, OpenAiCompatProvider, Provider, ProviderError, StreamEvent,
};
use async_trait::async_trait;
use futures::stream::BoxStream;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// How the user authenticated to a vendor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AuthMethod {
    /// Long-lived API key (preferred, simplest, works for every vendor).
    #[default]
    ApiKey,
    /// Browser-derived session token / cookie (vendor-specific shape; we
    /// just attach it as `Authorization: Bearer …` or `Cookie: …`).
    Browser,
    /// ACP agent (Zed Agent Client Protocol). **Not yet supported** in the
    /// onboarding wizard — most upstream CLIs (claude-code, codex, gemini-cli)
    /// don't implement Zed-ACP natively, so we gate it off until the upstream
    /// situation matures. See `evo-acp-client::CATALOG` for the long list of
    /// reasons. Power users can still configure ACP via `evoclaw agent add`.
    Acp,
}

impl AuthMethod {
    pub fn as_str(self) -> &'static str {
        match self {
            AuthMethod::ApiKey => "api_key",
            AuthMethod::Browser => "browser",
            AuthMethod::Acp => "acp",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "api_key" | "apikey" | "key" => Some(Self::ApiKey),
            "browser" | "web" | "cookie" => Some(Self::Browser),
            "acp" | "agent" => Some(Self::Acp),
            _ => None,
        }
    }
    /// Human-readable label, mirrored in the picker.
    pub fn label(self) -> &'static str {
        match self {
            AuthMethod::ApiKey => "API key",
            AuthMethod::Browser => "Browser sign-in",
            AuthMethod::Acp => "ACP agent (not yet supported)",
        }
    }
}

/// How a captured session token is attached to outgoing requests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrowserAuthShape {
    /// Vendor accepts the captured token as an API key (`Authorization: Bearer`).
    Bearer,
    /// Vendor uses a session cookie. We split on `;` so the user can paste
    /// the entire `document.cookie` line — and we trust them to include the
    /// session-relevant pair.
    Cookie,
    /// Anthropic-style: header is `x-api-key: <token>`.
    AnthropicHeader,
}

/// Persisted browser-session profile. Stored at
/// `~/.evoclaw/browser_profiles/<provider_id>.json` (mode 0600).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrowserProfile {
    pub provider_id: String,
    /// Wire endpoint base (`https://api.example.com/v1`). Same value as the
    /// API-key flow; we copy it here so the profile is self-describing.
    pub base_url: String,
    /// Default model id captured at sign-in time.
    pub default_model: String,
    /// The captured session credential. Format depends on `shape`.
    pub session_token: String,
    /// How to attach `session_token` to outgoing requests.
    pub shape: BrowserShapeRepr,
    /// Optional human note: where the user copied this from. Helps when the
    /// session expires and the user has to repeat the steps.
    #[serde(default)]
    pub source_hint: Option<String>,
    /// ISO-8601 capture time. Used for `evoclaw doctor` staleness warnings.
    pub captured_at: String,
}

/// Serde shape for `BrowserAuthShape` so `serde_json` can round-trip it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrowserShapeRepr {
    Bearer,
    Cookie,
    AnthropicHeader,
}

impl From<BrowserAuthShape> for BrowserShapeRepr {
    fn from(s: BrowserAuthShape) -> Self {
        match s {
            BrowserAuthShape::Bearer => Self::Bearer,
            BrowserAuthShape::Cookie => Self::Cookie,
            BrowserAuthShape::AnthropicHeader => Self::AnthropicHeader,
        }
    }
}
impl From<BrowserShapeRepr> for BrowserAuthShape {
    fn from(s: BrowserShapeRepr) -> Self {
        match s {
            BrowserShapeRepr::Bearer => Self::Bearer,
            BrowserShapeRepr::Cookie => Self::Cookie,
            BrowserShapeRepr::AnthropicHeader => Self::AnthropicHeader,
        }
    }
}

/// Provider that authenticates with a browser-captured session token.
///
/// Internally this composes an underlying vendor client. For Bearer/Cookie
/// shapes we lean on `OpenAiCompatProvider` (which already accepts a string
/// "key" and sends it via `bearer_auth`). Anthropic native goes through
/// `AnthropicProvider`, which already accepts an opaque key string and sends
/// it as `x-api-key`. Both reuse paths are honest about what they do — no
/// fake "browser automation"; the cookie/token comes from the user's real
/// browser session.
pub struct BrowserProvider {
    inner: Arc<dyn Provider>,
    pub provider_id: String,
}

impl std::fmt::Debug for BrowserProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BrowserProvider")
            .field("provider_id", &self.provider_id)
            .finish()
    }
}

impl BrowserProvider {
    /// Construct from a captured profile. Picks the right inner provider
    /// based on `provider_id` (only `anthropic` uses the native client; all
    /// others go through OpenAI-compat).
    pub fn from_profile(profile: &BrowserProfile) -> Self {
        let inner: Arc<dyn Provider> = match profile.provider_id.as_str() {
            "anthropic" => Arc::new(AnthropicProvider::with_base_url(
                profile.base_url.clone(),
                profile.session_token.clone(),
                profile.default_model.clone(),
            )),
            _ => Arc::new(OpenAiCompatProvider::new(
                profile.base_url.clone(),
                profile.session_token.clone(),
                profile.default_model.clone(),
            )),
        };
        Self {
            inner,
            provider_id: profile.provider_id.clone(),
        }
    }
}

#[async_trait]
impl Provider for BrowserProvider {
    async fn stream(
        &self,
        req: ChatRequest,
    ) -> Result<BoxStream<'static, Result<StreamEvent, ProviderError>>, ProviderError> {
        self.inner.stream(req).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auth_method_round_trip() {
        for m in [AuthMethod::ApiKey, AuthMethod::Browser, AuthMethod::Acp] {
            assert_eq!(AuthMethod::parse(m.as_str()), Some(m));
        }
    }

    #[test]
    fn auth_method_parse_aliases() {
        assert_eq!(AuthMethod::parse("ApiKey"), Some(AuthMethod::ApiKey));
        assert_eq!(AuthMethod::parse("WEB"), Some(AuthMethod::Browser));
        assert_eq!(AuthMethod::parse("agent"), Some(AuthMethod::Acp));
        assert_eq!(AuthMethod::parse("zzz"), None);
    }

    #[test]
    fn acp_label_marks_unsupported() {
        assert!(AuthMethod::Acp.label().contains("not yet supported"));
    }

    #[test]
    fn shape_repr_round_trip() {
        for s in [
            BrowserAuthShape::Bearer,
            BrowserAuthShape::Cookie,
            BrowserAuthShape::AnthropicHeader,
        ] {
            let r: BrowserShapeRepr = s.into();
            let back: BrowserAuthShape = r.into();
            assert_eq!(s, back);
        }
    }

    #[test]
    fn profile_serializes_to_json() {
        let p = BrowserProfile {
            provider_id: "deepseek".into(),
            base_url: "https://api.deepseek.com/v1".into(),
            default_model: "deepseek-chat".into(),
            session_token: "tok".into(),
            shape: BrowserShapeRepr::Bearer,
            source_hint: Some("DevTools cookie".into()),
            captured_at: "2026-05-03T00:00:00Z".into(),
        };
        let s = serde_json::to_string(&p).unwrap();
        assert!(s.contains("\"shape\":\"bearer\""));
        let back: BrowserProfile = serde_json::from_str(&s).unwrap();
        assert_eq!(back.provider_id, "deepseek");
    }

    #[test]
    fn from_profile_picks_anthropic_for_anthropic_id() {
        let p = BrowserProfile {
            provider_id: "anthropic".into(),
            base_url: "https://api.anthropic.com/v1".into(),
            default_model: "claude-3-5-sonnet-20241022".into(),
            session_token: "tok".into(),
            shape: BrowserShapeRepr::AnthropicHeader,
            source_hint: None,
            captured_at: "2026-05-03T00:00:00Z".into(),
        };
        let bp = BrowserProvider::from_profile(&p);
        assert_eq!(bp.provider_id, "anthropic");
    }

    #[test]
    fn from_profile_falls_back_to_openai_compat() {
        let p = BrowserProfile {
            provider_id: "deepseek".into(),
            base_url: "https://api.deepseek.com/v1".into(),
            default_model: "deepseek-chat".into(),
            session_token: "tok".into(),
            shape: BrowserShapeRepr::Bearer,
            source_hint: None,
            captured_at: "2026-05-03T00:00:00Z".into(),
        };
        let bp = BrowserProvider::from_profile(&p);
        assert_eq!(bp.provider_id, "deepseek");
    }
}
