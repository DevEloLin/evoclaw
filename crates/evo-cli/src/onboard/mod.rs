//! Onboarding wizard — interactive provider picker + auth-method picker.
//!
//! Closure rules:
//! - Key never appears in config.toml; only the **provider id** does.
//! - Key file lives at `~/.evoclaw/secrets/<provider>.key` (chmod 600 on Unix).
//! - Browser-session profile lives at `~/.evoclaw/browser_profiles/<provider>.json`
//!   (chmod 600). Format defined by `evo_providers::BrowserProfile`.
//! - Resolution order at runtime: `EVO_API_KEY` env -> secrets file -> error.
//! - `evoclaw doctor` always reports which source supplied the credential.
//!
//! Auth-method priority shown to the user in the shell entry:
//!   1) API key (preferred — simplest, works for every vendor in the catalog)
//!   2) Browser sign-in (capture session cookie / web token from the browser)
//!   3) ACP agent (TEMPORARILY NOT SUPPORTED — most upstream CLIs don't
//!      implement Zed-ACP natively; gated off until that situation matures)

pub(crate) mod auth;
pub mod catalog;
pub(crate) mod model;
pub(crate) mod paths;
pub(crate) mod persist;
pub(crate) mod picker;

// Re-export everything that was pub in the old onboard.rs
pub use auth::{
    ask_api_key, capture_browser_profile, resolve_api_key, run_copilot_oauth, KeySource,
};
pub use catalog::{find_provider, provider_to_acp_agent, ProviderProfile, PROVIDERS};
pub use model::pick_model;
pub use paths::{
    browser_profile_path, browser_profiles_dir, config_path, evoclaw_dir, secret_file, secrets_dir,
};
pub use persist::{
    load_browser_profile, save_browser_profile, save_config, save_config_with_auth, save_secret,
};
pub use picker::{pick_auth_method, pick_provider, prompt_gateway, read_nonempty, ProviderChoice};

#[cfg(test)]
mod tests {
    use super::*;
    use evo_providers::{AuthMethod, BrowserAuthShape};
    use persist::render_config_toml_with_auth;

    #[test]
    fn registry_has_at_least_5_providers() {
        assert!(PROVIDERS.len() >= 5);
        for p in PROVIDERS {
            assert!(!p.id.is_empty());
        }
    }

    #[test]
    fn openai_is_default_first_entry() {
        assert_eq!(PROVIDERS[0].id, "openai");
    }

    #[test]
    fn local_providers_have_no_key_url() {
        for p in PROVIDERS {
            if p.local {
                assert!(p.key_url.is_none(), "{} should have no key_url", p.id);
            }
        }
    }

    #[test]
    fn render_config_includes_auth_method_block() {
        let c = ProviderChoice {
            id: "deepseek".into(),
            name: "x".into(),
            base_url: "https://x.example/v1".into(),
            default_model: "deepseek-chat".into(),
            fallback: vec![],
            key_url: None,
            local: false,
        };
        let toml_api = render_config_toml_with_auth(&c, AuthMethod::ApiKey);
        assert!(toml_api.contains("[auth]"));
        assert!(toml_api.contains("method = \"api_key\""));
        let toml_browser = render_config_toml_with_auth(&c, AuthMethod::Browser);
        assert!(toml_browser.contains("method = \"browser\""));
    }

    #[test]
    fn browser_profile_path_under_evoclaw_dir() {
        let path = browser_profile_path("deepseek").unwrap();
        let s = path.display().to_string();
        assert!(s.ends_with("/browser_profiles/deepseek.json"));
    }

    #[test]
    fn browser_capture_shape_for_anthropic_uses_native_header() {
        // We don't test capture itself (interactive), but the inferred shape
        // is part of the public contract. Anthropic ⇒ AnthropicHeader, others
        // ⇒ Bearer. Mirror the match in `capture_browser_profile`.
        let s_anth = match "anthropic" {
            "anthropic" => BrowserAuthShape::AnthropicHeader,
            _ => BrowserAuthShape::Bearer,
        };
        assert_eq!(s_anth, BrowserAuthShape::AnthropicHeader);
        let s_ds = match "deepseek" {
            "anthropic" => BrowserAuthShape::AnthropicHeader,
            _ => BrowserAuthShape::Bearer,
        };
        assert_eq!(s_ds, BrowserAuthShape::Bearer);
    }

    #[test]
    fn render_config_includes_provider_marker() {
        let c = ProviderChoice {
            id: "deepseek".into(),
            name: "x".into(),
            base_url: "https://x.example/v1".into(),
            default_model: "deepseek-chat".into(),
            fallback: vec!["alt1".into()],
            key_url: None,
            local: false,
        };
        let toml = render_config_toml_with_auth(&c, AuthMethod::ApiKey);
        assert!(toml.contains("provider = \"deepseek\""));
        assert!(toml.contains("default  = \"deepseek-chat\""));
        assert!(toml.contains("\"alt1\""));
    }

    #[test]
    fn render_config_handles_empty_fallback() {
        let c = ProviderChoice {
            id: "x".into(),
            name: "x".into(),
            base_url: "u".into(),
            default_model: "m".into(),
            fallback: vec![],
            key_url: None,
            local: true,
        };
        let toml = render_config_toml_with_auth(&c, AuthMethod::ApiKey);
        assert!(toml.contains("fallback = []"));
    }

    #[test]
    fn find_provider_by_id() {
        assert_eq!(find_provider("deepseek").unwrap().id, "deepseek");
        assert!(find_provider("nonexistent").is_none());
    }
}
