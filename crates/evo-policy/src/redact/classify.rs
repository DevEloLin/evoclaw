use sha2::{Digest, Sha256};
use std::collections::HashMap;

/// Classification of a likely-secret string.
///
/// Marked `#[non_exhaustive]` so additive variants (new credential families)
/// are non-breaking for downstream `match` sites.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum SecretKind {
    OpenAi,
    Anthropic,
    GitHubPat,
    AwsKeyId,
    AwsSecret,
    AwsSessionToken,
    Jwt,
    Password,
    Slack,
    Stripe,
    PemPrivateKey,
    GenericHighEntropy,
    Unknown,
}

impl SecretKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::OpenAi => "openai_key",
            Self::Anthropic => "anthropic_key",
            Self::GitHubPat => "github_pat",
            Self::AwsKeyId => "aws_key_id",
            Self::AwsSecret => "aws_secret",
            Self::AwsSessionToken => "aws_session_token",
            Self::Jwt => "jwt",
            Self::Password => "password",
            Self::Slack => "slack_token",
            Self::Stripe => "stripe_key",
            Self::PemPrivateKey => "pem_private_key",
            Self::GenericHighEntropy => "high_entropy",
            Self::Unknown => "unknown",
        }
    }
}

/// Cheap classifier — pattern + length + entropy heuristics. Conservative;
/// we'd rather over-redact than leak.
///
/// **High-entropy guardrail (acp.md):** the `GenericHighEntropy` branch only
/// fires on *token-like* strings (ASCII-only, restricted credential charset,
/// not path-like). Natural-language input — CJK sentences, prose with
/// punctuation, file paths, shell commands — is classified as `Unknown`
/// so it survives unredacted on the model path.
pub fn classify_secret(s: &str) -> SecretKind {
    let t = s.trim();
    // Stripe: keep BEFORE generic `sk-` so `sk_live_…` doesn't fall through.
    if is_stripe_key(t) {
        return SecretKind::Stripe;
    }
    if t.starts_with("sk-ant-") {
        return SecretKind::Anthropic;
    }
    if t.starts_with("sk-") && t.len() >= 20 {
        return SecretKind::OpenAi;
    }
    if t.starts_with("ghp_")
        || t.starts_with("gho_")
        || t.starts_with("ghu_")
        || t.starts_with("ghs_")
        || t.starts_with("ghr_")
    {
        return SecretKind::GitHubPat;
    }
    if is_slack_token(t) {
        return SecretKind::Slack;
    }
    if t.starts_with("AKIA") && t.len() == 20 && t.chars().all(|c| c.is_ascii_alphanumeric()) {
        return SecretKind::AwsKeyId;
    }
    if is_aws_session_token(t) {
        return SecretKind::AwsSessionToken;
    }
    if looks_like_jwt(t) {
        return SecretKind::Jwt;
    }
    if looks_like_high_entropy_token(t) {
        return SecretKind::GenericHighEntropy;
    }
    SecretKind::Unknown
}

/// Token-like high-entropy detector.
///
/// Only fires on strings that look like opaque credentials:
///   * char-count ≥ 32
///   * pure ASCII (rules out CJK / accented prose)
///   * every char in the token-credential set
///     `[A-Za-z0-9_\-./+=]` — covers base64 / base64url / base58 / hex /
///     dot-separated payloads, while excluding spaces and prose punctuation
///   * Shannon entropy ≥ 4.0 b/c
///   * not file-path-like (filters absolute Unix / Windows paths even
///     when their lengths would otherwise qualify)
///
/// These rules deliberately reject the cases reported in
/// `prd/plan/acp.md`:
///   * Chinese / Japanese natural language (non-ASCII)
///   * `cargo clippy --workspace --all-targets -- -D warnings` (whitespace)
///   * `/Users/wei.li/devops/gptcli/agent/EvoClaw` (path-like)
///   * `中文 English emoji 🚀 mixed input should wrap correctly.` (whitespace + non-ASCII)
fn looks_like_high_entropy_token(t: &str) -> bool {
    let len = t.chars().count();
    if len < 32 {
        return false;
    }
    if !t.is_ascii() {
        return false;
    }
    if !t
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '/' | '+' | '='))
    {
        return false;
    }
    if is_path_like(t) {
        return false;
    }
    shannon_entropy(t) >= 4.0
}

/// Heuristic: looks like a filesystem path rather than a credential. Fires
/// when the token starts with `/`, `~`, or matches `<drive>:\` / `<drive>:/`,
/// or contains multiple path separators with at least one segment that is a
/// recognisable filesystem token.
pub(crate) fn is_path_like(t: &str) -> bool {
    if t.starts_with('/') || t.starts_with("~/") || t.starts_with("./") || t.starts_with("../") {
        return true;
    }
    let bytes = t.as_bytes();
    if bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && (bytes[2] == b'/' || bytes[2] == b'\\')
    {
        return true;
    }
    let segs: Vec<&str> = t.split(['/', '\\']).collect();
    if segs.len() >= 3 {
        const FS_TOKENS: &[&str] = &[
            "Users",
            "home",
            "var",
            "etc",
            "tmp",
            "opt",
            "usr",
            "bin",
            "sbin",
            "private",
            "mnt",
            "dev",
            "Library",
            "Applications",
            "Volumes",
            "System",
        ];
        if segs.iter().any(|s| FS_TOKENS.contains(s)) {
            return true;
        }
    }
    false
}

/// Slack OAuth/bot/user/refresh tokens. Slack's spec uses `xoxb-`, `xoxp-`,
/// `xoxs-`, `xoxa-`, `xoxr-` followed by digit/letter segments separated by
/// `-`. We require at least one `-` after the prefix and a min length to
/// reduce false positives on harmless strings like `"xoxb-test"`.
fn is_slack_token(t: &str) -> bool {
    const PREFIXES: &[&str] = &["xoxb-", "xoxp-", "xoxs-", "xoxa-", "xoxr-"];
    for p in PREFIXES {
        if let Some(rest) = t.strip_prefix(p) {
            // Slack tokens are typically >= 24 chars after the prefix and
            // contain at least one dash.
            if rest.len() >= 10 && rest.contains('-') {
                return true;
            }
        }
    }
    false
}

/// Stripe keys: `pk_live_`, `sk_live_`, `pk_test_`, `sk_test_`, `rk_live_`,
/// `rk_test_`. Length floor of 24 (prefix + at least 16 random chars).
fn is_stripe_key(t: &str) -> bool {
    const PREFIXES: &[&str] = &[
        "pk_live_", "sk_live_", "pk_test_", "sk_test_", "rk_live_", "rk_test_",
    ];
    for p in PREFIXES {
        if let Some(rest) = t.strip_prefix(p) {
            if rest.len() >= 16 && rest.chars().all(|c| c.is_ascii_alphanumeric()) {
                return true;
            }
        }
    }
    false
}

/// AWS Session Token heuristic. AWS STS session tokens are very long (300+
/// chars typical), base64-ish, and frequently start with `FQoG` (the literal
/// magic prefix of the v4 token format). Returns true when EITHER:
///   - token starts with `FQoG` and length >= 100, OR
///   - token length >= 200 and is pure `[A-Za-z0-9+/=]` (very strong signal).
fn is_aws_session_token(t: &str) -> bool {
    if t.starts_with("FQoG") && t.len() >= 100 {
        return true;
    }
    if t.len() >= 200
        && t.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '/' || c == '=')
    {
        return true;
    }
    false
}

fn looks_like_jwt(s: &str) -> bool {
    if !s.starts_with("eyJ") {
        return false;
    }
    s.split('.').count() == 3 && s.len() >= 40
}

/// Shannon entropy in bits per character.
pub fn shannon_entropy(s: &str) -> f64 {
    if s.is_empty() {
        return 0.0;
    }
    let mut counts = HashMap::new();
    for c in s.chars() {
        *counts.entry(c).or_insert(0u32) += 1;
    }
    let len = s.chars().count() as f64;
    let mut h = 0.0;
    for &c in counts.values() {
        let p = c as f64 / len;
        h -= p * p.log2();
    }
    h
}

pub fn fingerprint_of(s: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(s.as_bytes());
    let digest = hasher.finalize();
    hex::encode(&digest[..4])
}
