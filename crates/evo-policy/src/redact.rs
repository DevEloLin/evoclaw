//! Secret-redaction barrier.
//!
//! Closure rule (PRD §13.4): no API key, password, OAuth token, or other
//! credential the user has entered into EvoClaw is allowed to leave the
//! machine via the model API. This module enforces that boundary.
//!
//! Two complementary defences:
//!
//! 1. **Vault-backed substitution.** When the user runs `evoclaw secret add
//!    NAME VALUE`, the raw value is stored in `~/.evoclaw/secrets/vault.json`
//!    (chmod 600). Any text the runtime is about to hand to the model is
//!    scanned; every occurrence of a vaulted value is replaced with a
//!    placeholder `${SECRET:<NAME>}`. The model only sees the placeholder.
//!
//! 2. **Pattern-based catch-all.** Even if the secret was never registered,
//!    common credential shapes (`sk-...`, `ghp_...`, `eyJ...`, `AKIA...`,
//!    long high-entropy hex / base64 strings) are detected and rewritten as
//!    `[REDACTED:<kind>:<8-char-fingerprint>]`. The fingerprint is a
//!    SHA-256 prefix of the secret — useful for cross-referencing without
//!    ever exposing the raw value.
//!
//! Both defences run on:
//!   - inbound user text (before it joins the message history)
//!   - tool args and tool results (before they go back to the model)
//!   - any string written to memory L1-L5 / session JSONL
//!
//! Tools that genuinely need the raw secret (e.g. `run_shell` with a
//! `$GITHUB_TOKEN` env var) must read it from the process environment;
//! they never get the value via the LLM, so the LLM never carries it.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Persistent file-backed vault. Always lives at `~/.evoclaw/secrets/vault.json`
/// (or whatever path the caller hands to `Vault::load`/`Vault::save`).
/// chmod 600 on Unix.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Vault {
    pub version: u32,
    pub entries: Vec<VaultEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultEntry {
    pub name: String,
    pub value: String,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub fingerprint: String,
    #[serde(default = "Utc::now")]
    pub created_at: DateTime<Utc>,
}

impl Default for Vault {
    fn default() -> Self {
        Self {
            version: 1,
            entries: Vec::new(),
        }
    }
}

impl Vault {
    pub async fn load(path: &Path) -> Result<Self, std::io::Error> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let raw = tokio::fs::read_to_string(path).await?;
        let v: Self = serde_json::from_str(&raw)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        Ok(v)
    }

    pub async fn save(&self, path: &Path) -> Result<(), std::io::Error> {
        if let Some(dir) = path.parent() {
            tokio::fs::create_dir_all(dir).await?;
        }
        let json = serde_json::to_string_pretty(self)?;
        // Atomic write: stage to `<path>.tmp` in the same directory (so the
        // rename is a same-filesystem op), apply secure perms BEFORE rename,
        // then rename — POSIX rename is atomic, so a kill mid-write can never
        // leave the canonical file truncated or empty.
        let tmp = tmp_path_for(path);
        tokio::fs::write(&tmp, json).await?;
        // Set 0600 on the temp file BEFORE rename so the final file inherits
        // secure perms even momentarily — we never want a 0644 vault on disk.
        Self::chmod_600(&tmp).await?;
        // Best-effort fsync: open the file and `sync_all` before the rename.
        if let Ok(file) = tokio::fs::OpenOptions::new().write(true).open(&tmp).await {
            let _ = file.sync_all().await;
        }
        tokio::fs::rename(&tmp, path).await?;
        Ok(())
    }

    pub fn upsert(&mut self, name: &str, value: &str) -> &VaultEntry {
        let kind = classify_secret(value).label();
        let fingerprint = fingerprint_of(value);
        if let Some(idx) = self.entries.iter().position(|e| e.name == name) {
            self.entries[idx].value = value.to_string();
            self.entries[idx].kind = kind.into();
            self.entries[idx].fingerprint = fingerprint;
            return &self.entries[idx];
        }
        self.entries.push(VaultEntry {
            name: name.into(),
            value: value.into(),
            kind: kind.into(),
            fingerprint,
            created_at: Utc::now(),
        });
        self.entries.last().unwrap()
    }

    pub fn remove(&mut self, name: &str) -> bool {
        let before = self.entries.len();
        self.entries.retain(|e| e.name != name);
        before != self.entries.len()
    }

    pub fn get(&self, name: &str) -> Option<&VaultEntry> {
        self.entries.iter().find(|e| e.name == name)
    }

    pub fn list(&self) -> &[VaultEntry] {
        &self.entries
    }

    #[cfg(unix)]
    async fn chmod_600(path: &Path) -> Result<(), std::io::Error> {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o600);
        tokio::fs::set_permissions(path, perms).await
    }

    #[cfg(not(unix))]
    async fn chmod_600(_path: &Path) -> Result<(), std::io::Error> {
        Ok(())
    }
}

pub fn default_vault_path(evoclaw_dir: &Path) -> PathBuf {
    evoclaw_dir.join("secrets").join("vault.json")
}

/// Same-directory `<path>.tmp` companion path used by `Vault::save` for the
/// atomic write-and-rename cycle. Kept private but `pub(crate)` so future
/// modules can reuse it.
pub(crate) fn tmp_path_for(path: &Path) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(".tmp");
    PathBuf::from(s)
}

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
    if t.len() >= 32 && shannon_entropy(t) >= 4.0 {
        return SecretKind::GenericHighEntropy;
    }
    SecretKind::Unknown
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

/// Stateless redactor built from a `Vault` snapshot. Cheap to clone and pass
/// across threads (it owns its own `Vec`s of mappings).
#[derive(Debug, Clone, Default)]
pub struct Redactor {
    /// Sorted longest-first so longer secrets win over shorter substrings.
    mappings: Vec<(String, String)>,
}

impl Redactor {
    pub fn from_vault(v: &Vault) -> Self {
        // Tighter filter: skip entries whose value is empty OR whitespace-only.
        // A `"   "` value would otherwise match every triple-space in normal
        // text and corrupt formatting catastrophically.
        let mut mappings: Vec<(String, String)> = v
            .entries
            .iter()
            .filter(|e| !e.value.trim().is_empty())
            .map(|e| (e.value.clone(), format!("${{SECRET:{}}}", e.name)))
            .collect();
        mappings.sort_by_key(|m| std::cmp::Reverse(m.0.len()));
        Self { mappings }
    }

    /// Vault substitution + PEM block scrub + pattern fallback. Returns the
    /// scrubbed text and the count of substitutions made. Idempotent:
    /// re-running on already-scrubbed text is a no-op.
    pub fn scrub(&self, text: &str) -> (String, usize) {
        let mut out = text.to_string();
        let mut hits = 0usize;
        for (raw, placeholder) in &self.mappings {
            if out.contains(raw) {
                let n = out.matches(raw.as_str()).count();
                out = out.replace(raw.as_str(), placeholder);
                hits += n;
            }
        }
        // Multi-line PEM private-key blocks must be redacted before the
        // tokenizer runs — they contain whitespace and would be split across
        // tokens otherwise.
        let (out, pem_hits) = scrub_pem_blocks(&out);
        let (out, pattern_hits) = scrub_patterns(&out);
        (out, hits + pem_hits + pattern_hits)
    }

    pub fn is_empty(&self) -> bool {
        self.mappings.is_empty()
    }
    pub fn entry_count(&self) -> usize {
        self.mappings.len()
    }
}

/// Redact `-----BEGIN ... PRIVATE KEY-----` ... `-----END ... PRIVATE KEY-----`
/// blocks (any envelope variant: RSA, EC, OPENSSH, ENCRYPTED, plain PRIVATE
/// KEY). The body is replaced with `[REDACTED:pem_private_key]`. Returns the
/// scrubbed text and the number of blocks redacted. Pure string scan — no
/// regex dep, no allocations beyond the rebuilt string.
pub fn scrub_pem_blocks(text: &str) -> (String, usize) {
    const BEGIN: &str = "-----BEGIN ";
    const END: &str = "-----END ";
    const TAIL: &str = "-----";
    let mut out = String::with_capacity(text.len());
    let mut hits = 0usize;
    let mut cursor = 0usize;
    while cursor < text.len() {
        let rest = &text[cursor..];
        if let Some(begin_off) = rest.find(BEGIN) {
            let begin_abs = cursor + begin_off;
            if let Some(end_rel) = text[begin_abs + BEGIN.len()..].find(END) {
                let end_abs = begin_abs + BEGIN.len() + end_rel;
                if let Some(tail_rel) = text[end_abs + END.len()..].find(TAIL) {
                    let block_end = end_abs + END.len() + tail_rel + TAIL.len();
                    out.push_str(&text[cursor..begin_abs]);
                    out.push_str("[REDACTED:pem_private_key]");
                    hits += 1;
                    cursor = block_end;
                    continue;
                }
            }
            // Unterminated BEGIN — copy verbatim and stop scanning.
            out.push_str(&text[cursor..]);
            return (out, hits);
        }
        out.push_str(&text[cursor..]);
        return (out, hits);
    }
    (out, hits)
}

/// Walk the text token-by-token (whitespace + simple-punctuation split) and
/// replace any token that classifies as a secret. Preserves leading and
/// trailing punctuation.
pub fn scrub_patterns(text: &str) -> (String, usize) {
    let mut out = String::with_capacity(text.len());
    let mut hits = 0usize;
    let mut buf = String::new();
    let mut in_token = false;
    for c in text.chars() {
        if c.is_whitespace()
            || c == ','
            || c == ';'
            || c == ')'
            || c == ']'
            || c == '}'
            || c == '"'
            || c == '\''
        {
            if in_token {
                let (replaced, hit) = redact_token(&buf);
                out.push_str(&replaced);
                if hit {
                    hits += 1;
                }
                buf.clear();
                in_token = false;
            }
            out.push(c);
        } else {
            in_token = true;
            buf.push(c);
        }
    }
    if in_token {
        let (replaced, hit) = redact_token(&buf);
        out.push_str(&replaced);
        if hit {
            hits += 1;
        }
    }
    (out, hits)
}

fn redact_token(token: &str) -> (String, bool) {
    let inner = strip_assignment(token);
    let kind = classify_secret(inner);
    if matches!(kind, SecretKind::Unknown) {
        return (token.to_string(), false);
    }
    let placeholder = format!("[REDACTED:{}:{}]", kind.label(), fingerprint_of(inner));
    let replaced = token.replace(inner, &placeholder);
    (replaced, true)
}

/// Given `password=hunter2` return `hunter2`. Accepts `=`, `:`, JSON-style
/// `"key":"value"` (returns the value substring).
fn strip_assignment(token: &str) -> &str {
    if let Some(idx) = token.find('=') {
        let v = &token[idx + 1..];
        return v.trim_matches('"').trim_matches('\'');
    }
    if let Some(idx) = token.find(':') {
        let v = &token[idx + 1..];
        return v.trim_matches('"').trim_matches('\'');
    }
    token
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_openai_sk() {
        assert_eq!(
            classify_secret("sk-1234567890ABCDEFghij"),
            SecretKind::OpenAi
        );
    }
    #[test]
    fn classify_anthropic_overrides_openai() {
        assert_eq!(classify_secret("sk-ant-api03-XXXX"), SecretKind::Anthropic);
    }
    #[test]
    fn classify_github_pats() {
        for prefix in ["ghp_", "gho_", "ghu_", "ghs_", "ghr_"] {
            assert_eq!(
                classify_secret(&format!("{prefix}abcdef0123456789")),
                SecretKind::GitHubPat
            );
        }
    }
    #[test]
    fn classify_aws_key_id() {
        assert_eq!(
            classify_secret("AKIAIOSFODNN7EXAMPLE"),
            SecretKind::AwsKeyId
        );
    }
    #[test]
    fn classify_jwt() {
        let jwt = "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjMifQ.signature_part_here";
        assert_eq!(classify_secret(jwt), SecretKind::Jwt);
    }
    #[test]
    fn classify_high_entropy() {
        let s = "K9f4Lq2pZ8xV3wT7yU0nB6mC1aS5dG2eH4jR8bN0";
        assert_eq!(classify_secret(s), SecretKind::GenericHighEntropy);
    }
    #[test]
    fn classify_normal_words_as_unknown() {
        assert_eq!(classify_secret("hello"), SecretKind::Unknown);
        assert_eq!(classify_secret("the quick brown fox"), SecretKind::Unknown);
    }

    #[test]
    fn fingerprint_is_stable_8_chars() {
        let f1 = fingerprint_of("ghp_xxxxx");
        let f2 = fingerprint_of("ghp_xxxxx");
        assert_eq!(f1, f2);
        assert_eq!(f1.len(), 8);
    }

    #[test]
    fn vault_upsert_overwrites_same_name() {
        let mut v = Vault::default();
        v.upsert("gh", "ghp_old00000000000000000");
        v.upsert("gh", "ghp_new00000000000000000");
        assert_eq!(v.entries.len(), 1);
        assert_eq!(v.entries[0].value, "ghp_new00000000000000000");
    }

    #[test]
    fn vault_remove_returns_true_when_present() {
        let mut v = Vault::default();
        v.upsert("gh", "ghp_xxxxxxxxxxxxxxxxxxxx");
        assert!(v.remove("gh"));
        assert!(!v.remove("gh"));
    }

    #[test]
    fn redactor_substitutes_vault_value() {
        let mut v = Vault::default();
        v.upsert("gh", "ghp_secret_value_xxxxx");
        let r = Redactor::from_vault(&v);
        let (out, n) = r.scrub("please use ghp_secret_value_xxxxx now");
        assert_eq!(out, "please use ${SECRET:gh} now");
        assert_eq!(n, 1);
    }

    #[test]
    fn redactor_scrubs_unknown_secret_via_pattern() {
        let r = Redactor::default();
        let (out, n) = r.scrub("token=ghp_unregistered_aaaaaaaaaaaaaaaaaaa end");
        assert!(out.contains("[REDACTED:github_pat:"));
        assert!(!out.contains("ghp_unregistered"));
        assert_eq!(n, 1);
    }

    #[test]
    fn redactor_scrubs_jwt_in_authorization_header() {
        let r = Redactor::default();
        let (out, _) =
            r.scrub("Authorization: Bearer eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiJ4In0.sig_value_x");
        assert!(out.contains("[REDACTED:jwt:"));
    }

    #[test]
    fn redactor_idempotent() {
        let mut v = Vault::default();
        v.upsert("gh", "ghp_xxxxxxxxxxxxxxxxxxxx");
        let r = Redactor::from_vault(&v);
        let (once, _) = r.scrub("hello ghp_xxxxxxxxxxxxxxxxxxxx world");
        let (twice, hits2) = r.scrub(&once);
        assert_eq!(once, twice);
        assert_eq!(hits2, 0);
    }

    #[test]
    fn longest_value_wins_when_overlapping() {
        let mut v = Vault::default();
        v.upsert("short", "abc");
        v.upsert("long", "abcdef_long_value");
        let r = Redactor::from_vault(&v);
        let (out, _) = r.scrub("see abcdef_long_value here");
        assert!(out.contains("${SECRET:long}"));
        assert!(!out.contains("${SECRET:short}"));
    }

    #[test]
    fn entropy_distinguishes_random_from_words() {
        let words = "the quick brown fox jumps over";
        let random = "K9f4Lq2pZ8xV3wT7yU0nB6mC1aS5";
        assert!(shannon_entropy(random) > shannon_entropy(words));
    }

    #[test]
    fn strip_assignment_handles_quoted_values() {
        assert_eq!(strip_assignment("password=\"hunter2\""), "hunter2");
        assert_eq!(strip_assignment("token:'abc'"), "abc");
    }

    #[test]
    fn empty_vault_only_uses_pattern_matching() {
        let r = Redactor::default();
        let (out, _) = r.scrub("hello world");
        assert_eq!(out, "hello world");
    }

    #[tokio::test]
    async fn vault_save_then_load_round_trip() {
        let dir = std::env::temp_dir().join(format!(
            "evo-vault-{}",
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
        ));
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let path = dir.join("vault.json");
        let mut v = Vault::default();
        v.upsert("gh", "ghp_xxxxxxxxxxxxxxxxxxxx");
        v.save(&path).await.unwrap();
        let back = Vault::load(&path).await.unwrap();
        assert_eq!(back.entries.len(), 1);
        assert_eq!(back.entries[0].name, "gh");
        assert_eq!(back.entries[0].value, "ghp_xxxxxxxxxxxxxxxxxxxx");
        // Cleanup ignored on error.
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }
}
