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
fn is_path_like(t: &str) -> bool {
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
    ///
    /// Equivalent to `scrub_with(text, RedactionMode::Log)` — the strict mode
    /// previously used everywhere. Existing call sites that *should* be on
    /// the strict mode (logs, JSONL, audit, debug) keep their behaviour.
    pub fn scrub(&self, text: &str) -> (String, usize) {
        self.scrub_with(text, RedactionMode::Log)
    }

    /// Mode-aware scrub. See [`RedactionMode`].
    pub fn scrub_with(&self, text: &str, mode: RedactionMode) -> (String, usize) {
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
        // tokens otherwise. Always run, regardless of mode: a private-key
        // block is unambiguously a credential.
        let (out, pem_hits) = scrub_pem_blocks(&out);
        let (out, pattern_hits) = scrub_patterns_with(&out, mode);
        (out, hits + pem_hits + pattern_hits)
    }

    /// Strict redaction for logs / JSONL / audit / on-disk traces.
    /// Every classifier rule is active including the high-entropy fallback.
    pub fn redact_for_log(&self, text: &str) -> (String, usize) {
        self.scrub_with(text, RedactionMode::Log)
    }

    /// Conservative redaction for **outbound model / provider requests**.
    /// Vault substitution + PEM blocks + *known-prefix* secret patterns
    /// (sk-, ghp_, AKIA, JWT, Slack, Stripe, AWS session token). The
    /// generic high-entropy fallback is **disabled** so that natural-language
    /// user input (CJK sentences, prose, file paths, shell commands) reaches
    /// the upstream agent verbatim. See `prd/plan/acp.md`.
    pub fn redact_for_model(&self, text: &str) -> (String, usize) {
        self.scrub_with(text, RedactionMode::Model)
    }

    /// Moderate redaction for UI display (`prd/plan/ask.md`). Vault, PEM,
    /// and known-prefix credential patterns still fire; the generic
    /// high-entropy fallback is suppressed so ordinary identifiers
    /// (`evo-cli`, `tokio::spawn`), file paths, and CJK / English prose
    /// survive verbatim. The user looking at the screen never wants to
    /// see `[REDACTED:high_entropy:...]` plastered over normal output.
    pub fn redact_for_ui(&self, text: &str) -> (String, usize) {
        self.scrub_with(text, RedactionMode::Ui)
    }

    pub fn is_empty(&self) -> bool {
        self.mappings.is_empty()
    }
    pub fn entry_count(&self) -> usize {
        self.mappings.len()
    }
}

/// Selects how aggressively the redactor should scrub a payload.
///
/// * `Log`  — strict. Vault, PEM, all credential prefixes, and the generic
///   high-entropy fallback.
/// * `Model` — conservative. Vault, PEM, known-prefix credential patterns
///   **only**. Generic high-entropy is disabled so prose / CJK / paths /
///   commands survive verbatim.
/// * `Ui`   — same as `Log`. Named separately so call sites document intent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RedactionMode {
    Log,
    Model,
    Ui,
}

/// True if `text` contains nothing but `[REDACTED:...]` / `${SECRET:...}`
/// markers and surrounding whitespace — i.e. the user message has been
/// completely redacted away. Runtime can use this as a guardrail to refuse
/// to send an empty prompt upstream.
pub fn is_fully_redacted(text: &str) -> bool {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return false;
    }
    let mut cursor = 0usize;
    let bytes = trimmed.as_bytes();
    let mut saw_any_marker = false;
    while cursor < bytes.len() {
        let rest = &trimmed[cursor..];
        let next = rest
            .find("[REDACTED:")
            .map(|i| (i, "[REDACTED:", ']'))
            .into_iter()
            .chain(rest.find("${SECRET:").map(|i| (i, "${SECRET:", '}')))
            .min_by_key(|(i, _, _)| *i);
        let Some((rel, prefix, close)) = next else {
            return rest.chars().all(char::is_whitespace) && saw_any_marker;
        };
        // Anything between cursor and the next marker must be whitespace.
        if !rest[..rel].chars().all(char::is_whitespace) {
            return false;
        }
        let abs_start = cursor + rel + prefix.len();
        let Some(close_rel) = trimmed[abs_start..].find(close) else {
            return false;
        };
        cursor = abs_start + close_rel + 1;
        saw_any_marker = true;
    }
    saw_any_marker
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
///
/// **Splitter note:** in addition to ASCII whitespace and ASCII punctuation,
/// we also split on common CJK punctuation (`，。、；：「」『』（）！？—…` and
/// other unicode punctuation classes). Without this, a single Chinese
/// sentence becomes one giant "token" and could trip the high-entropy
/// classifier — that was the root cause of the bug fixed in
/// `prd/plan/acp.md`. We additionally split on any non-ASCII char so prose
/// is naturally chopped into runs of ASCII tokens.
pub fn scrub_patterns(text: &str) -> (String, usize) {
    scrub_patterns_with(text, RedactionMode::Log)
}

/// Mode-aware variant of [`scrub_patterns`]. In `RedactionMode::Model` the
/// generic high-entropy fallback is suppressed.
pub fn scrub_patterns_with(text: &str, mode: RedactionMode) -> (String, usize) {
    let mut out = String::with_capacity(text.len());
    let mut hits = 0usize;
    let mut buf = String::new();
    let mut in_token = false;
    for c in text.chars() {
        if is_token_separator(c) {
            if in_token {
                let (replaced, hit) = redact_token_with(&buf, mode);
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
        let (replaced, hit) = redact_token_with(&buf, mode);
        out.push_str(&replaced);
        if hit {
            hits += 1;
        }
    }
    (out, hits)
}

/// True if `c` should end a token-scan run. We split aggressively so the
/// classifier only sees compact ASCII strings.
fn is_token_separator(c: char) -> bool {
    if c.is_whitespace() {
        return true;
    }
    if matches!(
        c,
        ',' | ';' | ')' | ']' | '}' | '(' | '[' | '{' | '"' | '\'' | '<' | '>' | '`'
    ) {
        return true;
    }
    // Common CJK punctuation that does not have an ASCII equivalent in our
    // splitter. Keep this list small and unambiguous — anything that is a
    // legitimate run separator in user prose qualifies.
    if matches!(
        c,
        '，' | '。'
            | '、'
            | '；'
            | '：'
            | '「'
            | '」'
            | '『'
            | '』'
            | '（'
            | '）'
            | '！'
            | '？'
            | '…'
            | '—'
            | '·'
            | '《'
            | '》'
            | '【'
            | '】'
    ) {
        return true;
    }
    // Defence in depth: anything outside the token-credential charset is a
    // separator. Keeps natural-language runs from ever being scanned as a
    // single token. This includes accented Latin, emoji, and any other
    // non-ASCII glyph.
    if !(c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '/' | '+' | '=' | ':' | '@')) {
        return true;
    }
    false
}

fn redact_token_with(token: &str, mode: RedactionMode) -> (String, bool) {
    let inner = strip_assignment(token);
    let kind = classify_secret(inner);
    if matches!(kind, SecretKind::Unknown) {
        return (token.to_string(), false);
    }
    // On the model path, suppress the generic high-entropy fallback. Only
    // strong-signal credential prefixes (sk-, ghp_, AKIA, JWT, Slack,
    // Stripe, AWS session token) cross the redaction barrier outbound.
    // Both `Model` (outbound to provider) and `Ui` (rendered on-screen for
    // the human user) are conservative paths — the generic high-entropy
    // fallback is suppressed so ordinary identifiers, paths, and prose
    // survive verbatim. Only `Log` keeps it. See `prd/plan/ask.md`.
    if matches!(mode, RedactionMode::Model | RedactionMode::Ui)
        && matches!(kind, SecretKind::GenericHighEntropy)
    {
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

    // ── acp.md regression tests ───────────────────────────────────────────
    //
    // These tests pin the contract from `prd/plan/acp.md`: ordinary user
    // input (CJK prose, file paths, shell commands, mixed-language text)
    // must NOT be classified as a secret, and the model-mode redactor must
    // pass them through verbatim. Real credentials must still be redacted
    // locally.

    #[test]
    fn cjk_prose_is_not_high_entropy() {
        let s = "没有自适应终端大小，横线分割的文本框，没有被两个横线包起来";
        assert_ne!(classify_secret(s), SecretKind::GenericHighEntropy);
        assert_eq!(classify_secret(s), SecretKind::Unknown);
    }

    #[test]
    fn cjk_prose_survives_model_redactor() {
        let r = Redactor::default();
        let s = "没有自适应终端大小，横线分割的文本框，没有被两个横线包起来";
        let (out, n) = r.redact_for_model(s);
        assert_eq!(out, s);
        assert_eq!(n, 0);
    }

    #[test]
    fn english_question_survives_model_redactor() {
        let r = Redactor::default();
        let s = "如何确定当前是哪个账户";
        let (out, n) = r.redact_for_model(s);
        assert_eq!(out, s);
        assert_eq!(n, 0);
    }

    #[test]
    fn mixed_input_with_emoji_survives_model_redactor() {
        let r = Redactor::default();
        let s = "中文 English emoji 🚀 mixed input should wrap correctly.";
        let (out, n) = r.redact_for_model(s);
        assert_eq!(out, s);
        assert_eq!(n, 0);
    }

    #[test]
    fn unix_path_is_not_redacted() {
        let r = Redactor::default();
        let s = "/Users/wei.li/devops/gptcli/agent/EvoClaw";
        let (out, n) = r.redact_for_model(s);
        assert_eq!(out, s);
        assert_eq!(n, 0);
        // Even strict log mode should not redact a plain path.
        let (out_log, n_log) = r.redact_for_log(s);
        assert_eq!(out_log, s);
        assert_eq!(n_log, 0);
    }

    #[test]
    fn shell_command_is_not_redacted() {
        let r = Redactor::default();
        let s = "cargo clippy --workspace --all-targets -- -D warnings";
        let (out, n) = r.redact_for_model(s);
        assert_eq!(out, s);
        assert_eq!(n, 0);
    }

    #[test]
    fn real_openai_key_is_still_redacted_in_model_mode() {
        let r = Redactor::default();
        let s = "我的 API key 是 sk-1234567890abcdefghijklmnopqrstuvwxyz，帮我检查配置";
        let (out, n) = r.redact_for_model(s);
        assert!(
            !out.contains("sk-1234567890abcdefghijklmnopqrstuvwxyz"),
            "raw key leaked: {out}"
        );
        assert!(out.contains("[REDACTED:openai_key:"), "no marker: {out}");
        // Surrounding prose must be preserved verbatim.
        assert!(out.contains("我的 API key 是 "));
        assert!(out.contains("，帮我检查配置"));
        assert_eq!(n, 1);
    }

    #[test]
    fn github_pat_is_still_redacted_in_model_mode() {
        let r = Redactor::default();
        let s = "use ghp_1234567890abcdefghijklmnopqrstuvwxyz here";
        let (out, n) = r.redact_for_model(s);
        assert!(out.contains("[REDACTED:github_pat:"));
        assert!(out.starts_with("use "));
        assert!(out.ends_with(" here"));
        assert_eq!(n, 1);
    }

    #[test]
    fn jwt_is_still_redacted_in_model_mode() {
        let r = Redactor::default();
        let s =
            "token eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiIxMjMifQ.signature_part_here end";
        let (out, _) = r.redact_for_model(s);
        assert!(out.contains("[REDACTED:jwt:"));
        assert!(out.starts_with("token "));
        assert!(out.ends_with(" end"));
    }

    #[test]
    fn pem_block_is_redacted_in_both_modes() {
        let r = Redactor::default();
        let s = "before -----BEGIN PRIVATE KEY-----\nabc\n-----END PRIVATE KEY----- after";
        let (model_out, _) = r.redact_for_model(s);
        let (log_out, _) = r.redact_for_log(s);
        assert!(model_out.contains("[REDACTED:pem_private_key]"));
        assert!(log_out.contains("[REDACTED:pem_private_key]"));
        assert!(!model_out.contains("BEGIN PRIVATE KEY"));
    }

    #[test]
    fn high_entropy_token_redacted_in_log_but_not_in_model() {
        let r = Redactor::default();
        let s = "ref K9f4Lq2pZ8xV3wT7yU0nB6mC1aS5dG2eH4jR8bN0 ok";
        let (log_out, log_n) = r.redact_for_log(s);
        let (model_out, model_n) = r.redact_for_model(s);
        assert!(log_out.contains("[REDACTED:high_entropy:"));
        assert_eq!(log_n, 1);
        // Model path is conservative — generic high-entropy passes through.
        assert_eq!(model_out, s);
        assert_eq!(model_n, 0);
    }

    #[test]
    fn vault_substitution_runs_in_every_mode() {
        let mut v = Vault::default();
        v.upsert("gh", "ghp_secret_value_xxxxx");
        let r = Redactor::from_vault(&v);
        for mode in [RedactionMode::Log, RedactionMode::Model, RedactionMode::Ui] {
            let (out, n) = r.scrub_with("token=ghp_secret_value_xxxxx end", mode);
            assert!(out.contains("${SECRET:gh}"), "mode {mode:?} -> {out}");
            assert!(n >= 1);
        }
    }

    #[test]
    fn fully_redacted_detector_recognises_marker_only_payloads() {
        assert!(is_fully_redacted("[REDACTED:high_entropy:abcdef00]"));
        assert!(is_fully_redacted("   [REDACTED:openai_key:11223344]   "));
        assert!(is_fully_redacted("${SECRET:gh}"));
        assert!(is_fully_redacted(
            "[REDACTED:high_entropy:aa] [REDACTED:openai_key:bb]"
        ));
    }

    #[test]
    fn fully_redacted_detector_rejects_normal_text() {
        assert!(!is_fully_redacted(""));
        assert!(!is_fully_redacted("   "));
        assert!(!is_fully_redacted("hello world"));
        assert!(!is_fully_redacted(
            "我的 API key 是 [REDACTED:openai_key:abcd] 帮我检查配置"
        ));
        assert!(!is_fully_redacted("ok [REDACTED:openai_key:abcd]"));
    }

    #[test]
    fn path_like_detector_excludes_paths_from_classifier() {
        assert!(is_path_like("/Users/wei.li/devops/gptcli/agent/EvoClaw"));
        assert!(is_path_like("/etc/passwd"));
        assert!(is_path_like("~/Library/Caches"));
        assert!(is_path_like("./scripts/build.sh"));
        assert!(is_path_like("../foo/bar/baz"));
        assert!(is_path_like("C:\\Users\\foo\\bar"));
        assert!(!is_path_like("ghp_xxxxxxxxxxxxxxxx"));
        assert!(!is_path_like("eyJhbGc.eyJzdWI.sig"));
    }

    // ── ask.md regression — identifiers, paths, commands ────────────────
    //
    // Every string in `prd/plan/ask.md`'s "must NOT be redacted" list is
    // pinned here. The CLI screen (`Redactor::redact_for_ui`) and the
    // outbound model path (`Redactor::redact_for_model`) must both leave
    // these untouched.

    fn assert_passthrough_in_ui_and_model(input: &str) {
        let r = Redactor::default();
        let (ui_out, ui_n) = r.redact_for_ui(input);
        let (model_out, model_n) = r.redact_for_model(input);
        assert_eq!(ui_out, input, "UI mode mutated `{input}` → `{ui_out}`");
        assert_eq!(
            model_out, input,
            "Model mode mutated `{input}` → `{model_out}`"
        );
        assert_eq!(ui_n, 0);
        assert_eq!(model_n, 0);
    }

    #[test]
    fn crate_names_pass_through() {
        for s in [
            "evo-cli",
            "evo-core",
            "evo-providers",
            "crates-ext",
            "evo-gateway",
            "evo-policy",
            "evo-tools",
        ] {
            assert_passthrough_in_ui_and_model(s);
        }
    }

    #[test]
    fn rust_identifiers_pass_through() {
        for s in [
            "tokio::spawn",
            "tokio::sync::Semaphore",
            "queued_n",
            "fetch_sub",
            "swap(0)",
            "usize::MAX",
            "rustyline",
            "tui.rs",
            "lib.rs",
            "Cargo.toml",
            "semaphore",
        ] {
            assert_passthrough_in_ui_and_model(s);
        }
    }

    #[test]
    fn slash_commands_pass_through() {
        for s in ["/cancel", "/queue", "/help", "/exit", "/status", "/usage"] {
            assert_passthrough_in_ui_and_model(s);
        }
    }

    #[test]
    fn absolute_paths_pass_through() {
        for s in [
            "/Users/wei.li/devops/gptcli/agent/EvoClaw/crates/evo-cli/src/lib.rs",
            "/Users/wei.li/.evoclaw/config.toml",
            "/tmp/evoclaw/session-20260503T143523.jsonl",
        ] {
            assert_passthrough_in_ui_and_model(s);
        }
    }

    #[test]
    fn path_with_line_number_passes_through() {
        // `path:line` — the `:` is a non-separator so the whole thing is
        // one token; it must still pass through both paths.
        let s = "/Users/wei.li/devops/gptcli/agent/EvoClaw/crates/evo-cli/src/lib.rs:681";
        assert_passthrough_in_ui_and_model(s);
    }

    #[test]
    fn relative_path_with_line_number_passes_through() {
        // `evo-cli/src/lib.rs:681` is short (under 32 chars) so it falls
        // out at the length gate, but pin the contract anyway.
        let s = "evo-cli/src/lib.rs:681";
        assert_passthrough_in_ui_and_model(s);
    }

    #[test]
    fn cli_commands_pass_through_each_token() {
        let r = Redactor::default();
        let cmd = "cargo clippy --workspace --all-targets -- -D warnings";
        let (out, n) = r.redact_for_ui(cmd);
        assert_eq!(out, cmd);
        assert_eq!(n, 0);
        let (out_m, n_m) = r.redact_for_model(cmd);
        assert_eq!(out_m, cmd);
        assert_eq!(n_m, 0);
    }

    #[test]
    fn cjk_sentences_from_ask_md_pass_through() {
        for s in [
            "这些输出都没有格式化",
            "没有自适应终端大小",
            "横线分割的文本框没有被两个横线包起来",
            "现在我提问好像 ACP 没有直接发送到",
        ] {
            assert_passthrough_in_ui_and_model(s);
        }
    }

    #[test]
    fn ui_mode_does_not_emit_high_entropy_marker_on_real_assistant_output() {
        // Simulated assistant reply mixing prose, code names, paths,
        // and a bullet list. None of it is a credential, none of it
        // should produce `[REDACTED:high_entropy:...]`.
        let r = Redactor::default();
        let s = "这次修改主要涉及 evo-cli、evo-core、evo-providers。\n\
                 路径 /Users/wei.li/devops/gptcli/agent/EvoClaw/crates/evo-cli/src/lib.rs:681 \
                 调用 tokio::spawn + Semaphore::new(1)。\n\
                 - 风险 1: queued_n.fetch_sub 之后 worker 仍然继续。\n\
                 - 风险 2: /cancel 只清空队列。\n\
                 cargo clippy --workspace --all-targets -- -D warnings 通过。";
        let (out, n) = r.redact_for_ui(s);
        assert_eq!(out, s, "ui out diverged: {out}");
        assert_eq!(n, 0);
        assert!(
            !out.contains("[REDACTED:high_entropy"),
            "ui output contained high_entropy marker"
        );
    }

    #[test]
    fn ui_mode_still_redacts_real_keys() {
        let r = Redactor::default();
        let s = "我的 key 是 sk-1234567890abcdefghijklmnopqrstuvwxyz，帮我检查配置";
        let (out, _) = r.redact_for_ui(s);
        assert!(!out.contains("sk-1234567890abcdefghijklmnopqrstuvwxyz"));
        assert!(out.contains("[REDACTED:openai_key:"));
        assert!(out.contains("我的 key 是"));
        assert!(out.contains("，帮我检查配置"));
    }

    #[test]
    fn ui_mode_still_redacts_pem_blocks() {
        let r = Redactor::default();
        let s = "前 -----BEGIN PRIVATE KEY-----\nabc\n-----END PRIVATE KEY----- 后";
        let (out, _) = r.redact_for_ui(s);
        assert!(out.contains("[REDACTED:pem_private_key]"));
        assert!(!out.contains("BEGIN PRIVATE KEY"));
    }

    #[test]
    fn ui_mode_still_redacts_jwt_and_github_pat() {
        let r = Redactor::default();
        let s = "use ghp_1234567890abcdefghijklmnopqrstuvwxyz and \
                 eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiIxMjMifQ.signature_part_here";
        let (out, _) = r.redact_for_ui(s);
        assert!(out.contains("[REDACTED:github_pat:"));
        assert!(out.contains("[REDACTED:jwt:"));
        assert!(!out.contains("ghp_1234567890abcdefghijklmnopqrstuvwxyz"));
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
