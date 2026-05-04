//! Unified channel adapter configuration.
//!
//! Each adapter stores its settings at `~/.evoclaw/channels/<kind>.toml`.
//! All adapters share the same top-level schema; adapter-specific options can
//! be added under `[options]` in the future.
//!
//! ## Token resolution order (evaluated by the CLI at startup)
//!
//!   1. `${ENV:VAR_NAME}`   — read from environment variable `VAR_NAME`
//!   2. `${SECRET:name}`    — read from `~/.evoclaw/secrets/<name>.key`
//!   3. Literal string      — used directly (dev/testing only)
//!
//! ## Adding a new adapter
//!
//!   1. Create `~/.evoclaw/channels/<kind>.toml` using the schema below.
//!   2. Implement `ChannelAdapter` (see `local_pipe.rs` / `telegram.rs`).
//!   3. Add a match arm in `evo-cli`'s `channel_run()`.

use serde::{Deserialize, Serialize};

/// Top-level configuration for any channel adapter.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelConfig {
    /// Adapter kind identifier (e.g. `"telegram"`, `"slack"`, `"discord"`).
    pub kind: String,
    /// Auth token. Supports `${ENV:VAR}` and `${SECRET:name}` expansion.
    #[serde(default)]
    pub token: String,
    /// Human-readable display name shown in `evo channel list`.
    #[serde(default)]
    pub name: Option<String>,
    /// Controls which group-chat messages are forwarded to the agent.
    /// `always` (default) — every message; `at-mention` — only @-mentions.
    #[serde(default)]
    pub mention_mode: MentionMode,
}

/// Mention-handling policy for group channels.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum MentionMode {
    /// Forward every message (suitable for DMs and small trusted groups).
    #[default]
    Always,
    /// Forward only messages that explicitly @-mention the bot.
    AtMention,
}

impl ChannelConfig {
    /// Resolve token from environment if it uses `${ENV:VAR}` syntax.
    pub fn token_from_env(&self) -> Option<String> {
        let var = self.token.strip_prefix("${ENV:")?.strip_suffix('}')?;
        std::env::var(var).ok().filter(|v| !v.is_empty())
    }

    /// Return the vault key name if the token uses `${SECRET:name}` syntax.
    pub fn secret_key(&self) -> Option<&str> {
        self.token.strip_prefix("${SECRET:")?.strip_suffix('}')
    }

    /// Return the token as a plain literal (not a placeholder).
    pub fn token_literal(&self) -> Option<&str> {
        if self.token.starts_with("${") || self.token.is_empty() {
            None
        } else {
            Some(&self.token)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_env_placeholder() {
        let cfg = ChannelConfig {
            kind: "telegram".into(),
            token: "${ENV:TG_TOKEN}".into(),
            name: None,
            mention_mode: MentionMode::Always,
        };
        assert!(cfg.token_from_env().is_none()); // var not set
        assert!(cfg.secret_key().is_none());
        assert!(cfg.token_literal().is_none());
    }

    #[test]
    fn token_secret_placeholder() {
        let cfg = ChannelConfig {
            kind: "telegram".into(),
            token: "${SECRET:telegram_bot_token}".into(),
            name: None,
            mention_mode: MentionMode::Always,
        };
        assert_eq!(cfg.secret_key(), Some("telegram_bot_token"));
        assert!(cfg.token_from_env().is_none());
        assert!(cfg.token_literal().is_none());
    }

    #[test]
    fn token_literal_value() {
        let cfg = ChannelConfig {
            kind: "telegram".into(),
            token: "123:ABC".into(),
            name: None,
            mention_mode: MentionMode::default(),
        };
        assert_eq!(cfg.token_literal(), Some("123:ABC"));
    }

    #[test]
    fn config_roundtrips_toml() {
        let raw = r#"
kind = "telegram"
token = "${SECRET:telegram_bot_token}"
mention_mode = "always"
"#;
        let cfg: ChannelConfig = toml::from_str(raw).unwrap();
        assert_eq!(cfg.kind, "telegram");
        assert_eq!(cfg.secret_key(), Some("telegram_bot_token"));
    }
}
