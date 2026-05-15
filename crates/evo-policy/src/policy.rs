//! User-configurable allow/deny policy loaded from `~/.evoclaw/policy.toml`.
//!
//! Deny rules always take precedence over allow rules. If an allow list is
//! present for a tool, only matching invocations are permitted. If no allow
//! list exists the tool is unrestricted (subject only to deny rules).
//!
//! Pattern syntax: `*` matches any sequence of characters, `?` matches any
//! single character. Patterns are matched case-sensitively.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use tracing::warn;

/// Outcome of a policy check for a single tool invocation.
#[derive(Debug, Clone)]
pub enum PolicyDecision {
    Allow,
    Block(String),
}

/// Per-tool allow/deny lists.  Keys are tool names (`"bash"`, `"write"`,
/// `"read"`, `"web_fetch"`, `"*"` for all tools).
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct RuleSet {
    #[serde(flatten)]
    pub tools: HashMap<String, Vec<String>>,
}

/// Hook definition — a shell command executed before a tool invocation.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct HookDef {
    /// Tool name to match (`"bash"`, `"write"`, `"*"` for all).
    pub tool: String,
    /// Shell command to execute.  Receives tool name + args JSON on stdin.
    pub command: String,
    /// What to do when the hook exits non-zero (default: block).
    #[serde(default = "default_on_fail")]
    pub on_fail: OnFail,
}

fn default_on_fail() -> OnFail {
    OnFail::Block
}

/// Behaviour when a hook exits with a non-zero code.
#[derive(Debug, Clone, Deserialize, Serialize, Default, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum OnFail {
    /// Return `PolicyDecision::Block` with hook stdout as reason.
    #[default]
    Block,
    /// Log a warning but continue execution.
    Warn,
}

/// Full policy configuration loaded from `~/.evoclaw/policy.toml`.
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct PolicyConfig {
    /// Patterns that must match for a tool call to proceed (whitelist).
    /// If empty for a tool, all invocations of that tool are permitted.
    #[serde(default)]
    pub allow: RuleSet,
    /// Patterns that unconditionally block a tool call (blacklist).
    #[serde(default)]
    pub deny: RuleSet,
    /// Pre-execution hooks.
    #[serde(default)]
    pub hooks: HooksConfig,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct HooksConfig {
    #[serde(default)]
    pub pre_exec: Vec<HookDef>,
}

impl PolicyConfig {
    /// Load from a TOML file.  Returns `Default` when the file does not
    /// exist — callers always get a usable config without extra error handling.
    pub async fn load(path: &Path) -> Self {
        match tokio::fs::read_to_string(path).await {
            Err(_) => Self::default(),
            Ok(text) => match toml::from_str::<Self>(&text) {
                Ok(cfg) => cfg,
                Err(e) => {
                    warn!("policy.toml parse error (using defaults): {e}");
                    Self::default()
                }
            },
        }
    }

    /// Evaluate deny/allow rules for a tool invocation.
    ///
    /// `subject` is the value rules match against: for `bash` it is the full
    /// command string; for `write`/`read`/`patch` it is the file path; for
    /// `web_fetch` it is the URL; for other tools it is empty (rules still
    /// apply when the tool name matches a `deny`/`allow` key with `"*"`).
    pub fn check_rules(&self, tool: &str, subject: &str) -> PolicyDecision {
        let subject_exp = expand_home(subject);

        // Deny takes precedence — check deny lists for this tool and for "*".
        for key in [tool, "*"] {
            if let Some(patterns) = self.deny.tools.get(key) {
                for pat in patterns {
                    if glob_match(pat, &subject_exp) {
                        return PolicyDecision::Block(format!(
                            "policy: denied by rule '{pat}' for tool '{tool}'"
                        ));
                    }
                }
            }
        }

        // Allow list: if any allow list exists for this tool (or "*"), the
        // subject must match at least one pattern to proceed.
        let allow_patterns: Vec<&String> = [tool, "*"]
            .iter()
            .filter_map(|k| self.allow.tools.get(*k))
            .flatten()
            .collect();

        if !allow_patterns.is_empty() {
            let matched = allow_patterns.iter().any(|p| glob_match(p, &subject_exp));
            if !matched {
                return PolicyDecision::Block(format!(
                    "policy: '{}' not in allow list for tool '{tool}'",
                    subject_exp
                ));
            }
        }

        PolicyDecision::Allow
    }

    /// Return hooks whose `tool` field matches `tool_name` or `"*"`.
    pub fn hooks_for(&self, tool_name: &str) -> Vec<&HookDef> {
        self.hooks
            .pre_exec
            .iter()
            .filter(|h| h.tool == tool_name || h.tool == "*")
            .collect()
    }
}

/// Expand a leading `~/` to the actual home directory.
fn expand_home(s: &str) -> String {
    if let Some(rest) = s.strip_prefix("~/") {
        if let Ok(home) = std::env::var("HOME") {
            return format!("{home}/{rest}");
        }
    }
    s.to_string()
}

/// Simple glob matching: `*` matches any sequence, `?` matches one char.
/// O(m×n) DP — safe against adversarial patterns like `"***...***"`.
pub fn glob_match(pattern: &str, text: &str) -> bool {
    let pat: Vec<char> = pattern.chars().collect();
    let txt: Vec<char> = text.chars().collect();
    let m = pat.len();
    let n = txt.len();

    // dp[i][j] = pat[..i] matches txt[..j]
    let mut dp = vec![vec![false; n + 1]; m + 1];
    dp[0][0] = true;

    // A leading run of '*' matches empty text.
    for i in 1..=m {
        if pat[i - 1] == '*' {
            dp[i][0] = dp[i - 1][0];
        }
    }

    for i in 1..=m {
        for j in 1..=n {
            dp[i][j] = match pat[i - 1] {
                '*' => dp[i - 1][j] || dp[i][j - 1],
                '?' => dp[i - 1][j - 1],
                c => dp[i - 1][j - 1] && c == txt[j - 1],
            };
        }
    }

    dp[m][n]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glob_literal() {
        assert!(glob_match("cargo build", "cargo build"));
        assert!(!glob_match("cargo build", "cargo test"));
    }

    #[test]
    fn glob_star() {
        assert!(glob_match("cargo *", "cargo build --release"));
        assert!(glob_match("*.ssh/*", "cat ~/.ssh/id_rsa"));
        assert!(glob_match("*.ssh*", "cat ~/.ssh/id_rsa"));
        assert!(!glob_match("cargo *", "git status"));
        // Whitespace IS significant inside a pattern — " .ssh" demands a
        // literal space before ".ssh", which the SSH paths below do NOT have.
        assert!(!glob_match("* .ssh*", "cat ~/.ssh/id_rsa"));
    }

    #[test]
    fn glob_question() {
        assert!(glob_match("git ?", "git s"));
        assert!(!glob_match("git ?", "git st"));
    }

    #[test]
    fn deny_beats_allow() {
        let mut cfg = PolicyConfig::default();
        cfg.deny.tools.insert("bash".into(), vec!["*.ssh*".into()]);
        cfg.allow.tools.insert("bash".into(), vec!["*".into()]);
        assert!(matches!(
            cfg.check_rules("bash", "cat ~/.ssh/id_rsa"),
            PolicyDecision::Block(_)
        ));
    }

    #[test]
    fn allow_list_restricts() {
        let mut cfg = PolicyConfig::default();
        cfg.allow
            .tools
            .insert("bash".into(), vec!["cargo *".into(), "git *".into()]);
        assert!(matches!(
            cfg.check_rules("bash", "cargo build"),
            PolicyDecision::Allow
        ));
        assert!(matches!(
            cfg.check_rules("bash", "rm -rf /"),
            PolicyDecision::Block(_)
        ));
    }

    #[test]
    fn no_rules_allows_all() {
        let cfg = PolicyConfig::default();
        assert!(matches!(
            cfg.check_rules("bash", "anything"),
            PolicyDecision::Allow
        ));
    }
}
