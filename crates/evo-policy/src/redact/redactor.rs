use super::classify::{classify_secret, fingerprint_of, SecretKind};
use super::vault::Vault;

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
pub(crate) fn strip_assignment(token: &str) -> &str {
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
