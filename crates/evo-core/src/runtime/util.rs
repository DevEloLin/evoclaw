//! Standalone utility functions, RAII guards, and tests for the runtime module.

use crate::session::{EndRecord, SessionRecord};
use chrono::Utc;
use std::path::Path;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// Marker text used by `head_tail` to signal the omitted middle section.
pub(crate) const OMIT: &str = " ... ";

/// Truncate `s` to at most `max` bytes, keeping the head and tail with an
/// omission marker in the middle. Respects UTF-8 char boundaries.
pub(crate) fn head_tail(s: &str, max: usize) -> String {
    if s.len() <= max + OMIT.len() * 2 {
        return s.to_string();
    }
    let half = max / 2;
    let head_end = floor_char_boundary(s, half);
    let tail_start = ceil_char_boundary(s, s.len().saturating_sub(half));
    format!("{}{OMIT}{}", &s[..head_end], &s[tail_start..])
}

fn floor_char_boundary(s: &str, mut i: usize) -> usize {
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

fn ceil_char_boundary(s: &str, mut i: usize) -> usize {
    while i < s.len() && !s.is_char_boundary(i) {
        i += 1;
    }
    i
}

/// PRD §16 — JSONL closure invariant guard.
///
/// If `run()` returns / panics without writing the terminal `End` record
/// (`end_written` still `false`), the guard's `Drop` synchronously appends a
/// synthetic `state = "FAILED"` record using `std::fs` so downstream tools
/// (`evoclaw doctor closure`) still see a sealed log. We use sync I/O here
/// because `Drop` cannot be async.
pub(crate) struct SessionEndGuard {
    pub(crate) path: PathBuf,
    pub(crate) end_written: Arc<AtomicBool>,
}

impl Drop for SessionEndGuard {
    fn drop(&mut self) {
        if self.end_written.load(Ordering::SeqCst) {
            return;
        }
        let _ = append_synthetic_end(&self.path);
    }
}

/// Provider-payload debug instrumentation (acp.md).
///
/// When the user opts in via `EVOCLAW_DEBUG_PROVIDER=1` (any truthy value:
/// `1`, `true`, `yes`, case-insensitive), this prints metadata about the
/// outbound model request to stderr. **Never prints raw user input or raw
/// secrets** — only char-counts, redaction counts, a head/tail preview, and
/// a stable 8-char SHA-256 fingerprint of the raw input.
///
/// The helper is intentionally a free function (no `&self`) so it can be
/// called before any redactor work happens, and it is gated by the env
/// var so a user must explicitly opt in.
pub(crate) fn emit_provider_debug(
    provider_id: Option<&str>,
    raw_user_input: &str,
    sanitized_for_log: &str,
    sanitized_for_model: &str,
) {
    if !provider_debug_enabled() {
        return;
    }
    let raw_chars = raw_user_input.chars().count();
    let log_chars = sanitized_for_log.chars().count();
    let model_chars = sanitized_for_model.chars().count();
    let redaction_count = sanitized_for_model.matches("[REDACTED:").count()
        + sanitized_for_model.matches("${SECRET:").count();
    let source = if sanitized_for_model == raw_user_input {
        "raw_user_input"
    } else {
        "sanitized_for_model"
    };
    let preview = preview_for_debug(sanitized_for_model);
    let fp = {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        h.update(raw_user_input.as_bytes());
        let d = h.finalize();
        hex::encode(&d[..4])
    };
    eprintln!("== Provider Payload Debug ==");
    eprintln!("provider: {}", provider_id.unwrap_or("<none>"));
    eprintln!("raw_user_input_len: {raw_chars}");
    eprintln!("sanitized_for_log_len: {log_chars}");
    eprintln!("sanitized_for_model_len: {model_chars}");
    eprintln!("redaction_count: {redaction_count}");
    eprintln!("model_request_source: {source}");
    eprintln!("model_request_preview: {preview}");
    eprintln!("raw_user_input_fingerprint: {fp}");
    eprintln!("============================");
}

pub(crate) fn provider_debug_enabled() -> bool {
    matches!(
        std::env::var("EVOCLAW_DEBUG_PROVIDER")
            .ok()
            .as_deref()
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("1") | Some("true") | Some("yes") | Some("on")
    )
}

/// Compact head/tail preview that never crosses a UTF-8 char boundary.
/// Caps the visible body at 160 chars total so noisy console output
/// doesn't drown the user.
fn preview_for_debug(text: &str) -> String {
    const CAP: usize = 160;
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= CAP {
        return text.to_string();
    }
    let head: String = chars.iter().take(CAP / 2).collect();
    let tail: String = chars
        .iter()
        .skip(chars.len().saturating_sub(CAP / 2))
        .collect();
    format!("{head} … {tail}")
}

pub(crate) fn append_synthetic_end(path: &Path) -> std::io::Result<()> {
    use std::io::Write;
    let synthetic = SessionRecord::End(EndRecord {
        state: "FAILED".to_string(),
        finished_at: Utc::now(),
    });
    let mut line = serde_json::to_string(&synthetic)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    line.push('\n');
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    f.write_all(line.as_bytes())?;
    f.flush()
}

#[cfg(test)]
mod tests {
    use crate::runtime::{ConversationRuntime, RuntimeConfig, RuntimeError};
    use crate::session::Session;
    use evo_tools::{ToolContext, ToolRegistry};
    use std::sync::Arc;

    fn unique_log() -> std::path::PathBuf {
        let mut p = std::env::temp_dir();
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        p.push(format!("evo-runtime-{stamp}.jsonl"));
        p
    }

    fn unique_ws() -> std::path::PathBuf {
        let mut p = std::env::temp_dir();
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        p.push(format!("evo-rt-ws-{stamp}"));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[tokio::test]
    async fn single_turn_no_tools_completes() {
        let provider = Arc::new(evo_mock_provider::MockProvider::scripted(vec![
            evo_mock_provider::Turn::final_text("<summary>done</summary> hello world"),
        ]));
        let registry = Arc::new(ToolRegistry::with_builtins());
        let session = Session::open(unique_log()).await.unwrap();
        let mut rt = ConversationRuntime::new(
            provider,
            registry,
            session,
            ToolContext::default(),
            RuntimeConfig::default(),
        );
        let out = rt.run("hi").await.unwrap();
        assert!(out.completed);
        assert!(out.final_text.contains("hello world"));
    }

    // ── acp.md regression — provider receives un-redacted user prompt ────
    //
    // These tests pin the contract from `prd/plan/acp.md`: ordinary user
    // input must reach the provider verbatim (vault + known-prefix
    // credential patterns aside), and a fully-redacted prompt must be
    // refused locally rather than dispatched upstream.

    /// A minimal `Provider` that records the latest user message of every
    /// inbound `ChatRequest` so tests can inspect what would have been sent
    /// to a real upstream agent.
    struct RecordingProvider {
        last_user_message: Arc<std::sync::Mutex<Option<String>>>,
    }

    impl RecordingProvider {
        fn new() -> (Arc<Self>, Arc<std::sync::Mutex<Option<String>>>) {
            let slot = Arc::new(std::sync::Mutex::new(None));
            (
                Arc::new(Self {
                    last_user_message: slot.clone(),
                }),
                slot,
            )
        }
    }

    #[async_trait::async_trait]
    impl evo_providers::Provider for RecordingProvider {
        async fn stream(
            &self,
            req: evo_providers::ChatRequest,
        ) -> Result<
            futures::stream::BoxStream<
                'static,
                Result<evo_providers::StreamEvent, evo_providers::ProviderError>,
            >,
            evo_providers::ProviderError,
        > {
            let last = req
                .messages
                .iter()
                .rev()
                .find(|m| matches!(m.role, evo_providers::Role::User))
                .map(|m| m.content.clone())
                .unwrap_or_default();
            *self.last_user_message.lock().unwrap() = Some(last);
            use evo_providers::{StreamEvent, Usage};
            use futures::StreamExt;
            let events: Vec<Result<StreamEvent, evo_providers::ProviderError>> = vec![
                Ok(StreamEvent::Delta(
                    "<summary>done</summary> ack".to_string(),
                )),
                Ok(StreamEvent::ToolCallFinish),
                Ok(StreamEvent::Usage(Usage::default())),
                Ok(StreamEvent::Done),
            ];
            Ok(futures::stream::iter(events).boxed())
        }
    }

    /// Run a single user turn against `RecordingProvider` with the given
    /// vault contents (always `Some(Redactor)`) and return the captured
    /// outbound user payload.
    async fn run_capture_outbound(input: &str, vault: evo_policy::Vault) -> String {
        let (provider, slot) = RecordingProvider::new();
        let registry = Arc::new(ToolRegistry::with_builtins());
        let session = Session::open(unique_log()).await.unwrap();
        let redactor = evo_policy::Redactor::from_vault(&vault);
        let mut rt = ConversationRuntime::new(
            provider,
            registry,
            session,
            ToolContext::default(),
            RuntimeConfig::default(),
        )
        .with_redactor(redactor);
        rt.run(input).await.unwrap();
        let captured = slot.lock().unwrap().clone();
        captured.expect("provider not called")
    }

    #[tokio::test]
    async fn cjk_question_reaches_provider_verbatim() {
        let s = "没有自适应终端大小，横线分割的文本框，没有被两个横线包起来";
        let outbound = run_capture_outbound(s, evo_policy::Vault::default()).await;
        assert!(outbound.contains(s), "missing user text in: {outbound}");
        assert!(
            !outbound.contains("[REDACTED:high_entropy"),
            "high_entropy false positive: {outbound}"
        );
    }

    #[tokio::test]
    async fn english_account_question_reaches_provider_verbatim() {
        let s = "如何确定当前是哪个账户";
        let outbound = run_capture_outbound(s, evo_policy::Vault::default()).await;
        assert!(outbound.contains(s));
        assert!(!outbound.contains("[REDACTED:"));
    }

    #[tokio::test]
    async fn mixed_language_input_reaches_provider_verbatim() {
        let s = "中文 English emoji 🚀 mixed input should wrap correctly.";
        let outbound = run_capture_outbound(s, evo_policy::Vault::default()).await;
        assert!(outbound.contains(s));
        assert!(!outbound.contains("[REDACTED:"));
    }

    #[tokio::test]
    async fn workspace_path_reaches_provider_verbatim() {
        let s = "/Users/wei.li/devops/gptcli/agent/EvoClaw";
        let outbound = run_capture_outbound(s, evo_policy::Vault::default()).await;
        assert!(outbound.contains(s));
    }

    #[tokio::test]
    async fn shell_command_reaches_provider_verbatim() {
        let s = "cargo clippy --workspace --all-targets -- -D warnings";
        let outbound = run_capture_outbound(s, evo_policy::Vault::default()).await;
        assert!(outbound.contains(s));
        assert!(!outbound.contains("[REDACTED:"));
    }

    #[tokio::test]
    async fn openai_key_is_redacted_but_surrounding_prose_survives() {
        let s = "我的 API key 是 sk-1234567890abcdefghijklmnopqrstuvwxyz，帮我检查配置";
        let outbound = run_capture_outbound(s, evo_policy::Vault::default()).await;
        assert!(
            !outbound.contains("sk-1234567890abcdefghijklmnopqrstuvwxyz"),
            "raw key leaked: {outbound}"
        );
        assert!(outbound.contains("[REDACTED:openai_key:"));
        assert!(outbound.contains("我的 API key 是"));
        assert!(outbound.contains("，帮我检查配置"));
    }

    #[tokio::test]
    async fn vault_substitution_still_runs_on_outbound_path() {
        let mut v = evo_policy::Vault::default();
        v.upsert("gh_token", "ghp_1234567890abcdefghijklmnopqrstuvwxyz");
        let s = "deploy with ghp_1234567890abcdefghijklmnopqrstuvwxyz please";
        let outbound = run_capture_outbound(s, v).await;
        assert!(outbound.contains("${SECRET:gh_token}"));
        assert!(!outbound.contains("ghp_1234567890abcdefghijklmnopqrstuvwxyz"));
    }

    #[tokio::test]
    async fn fully_redacted_prompt_is_refused_locally() {
        let (provider, slot) = RecordingProvider::new();
        let registry = Arc::new(ToolRegistry::with_builtins());
        let session = Session::open(unique_log()).await.unwrap();
        let redactor = evo_policy::Redactor::from_vault(&evo_policy::Vault::default());
        let mut rt = ConversationRuntime::new(
            provider,
            registry,
            session,
            ToolContext::default(),
            RuntimeConfig::default(),
        )
        .with_redactor(redactor);
        let err = rt
            .run("sk-1234567890abcdefghijklmnopqrstuvwxyz")
            .await
            .expect_err("must refuse fully-redacted prompt");
        match err {
            RuntimeError::Provider(evo_providers::ProviderError::Other(msg)) => {
                assert!(msg.contains("fully redacted"));
                assert!(msg.contains("EVOCLAW_DEBUG_PROVIDER"));
            }
            other => panic!("expected Provider(Other(..)), got {other:?}"),
        }
        assert!(
            slot.lock().unwrap().is_none(),
            "provider must not be invoked"
        );
    }

    #[tokio::test]
    async fn history_persists_across_runs_for_native_providers() {
        let (provider, _slot) = RecordingProvider::new();
        let registry = Arc::new(ToolRegistry::with_builtins());
        let session = Session::open(unique_log()).await.unwrap();
        let redactor = evo_policy::Redactor::from_vault(&evo_policy::Vault::default());
        let mut rt = ConversationRuntime::new(
            provider,
            registry,
            session,
            ToolContext::default(),
            RuntimeConfig::default(),
        )
        .with_redactor(redactor);
        assert_eq!(rt.history_len(), 0, "fresh runtime has empty history");

        rt.run("我叫李伟").await.unwrap();
        let after_first = rt.history_len();
        assert!(
            after_first >= 3,
            "after one run history should hold system+user+assistant (got {after_first})"
        );

        rt.run("我叫什么").await.unwrap();
        let after_second = rt.history_len();
        assert!(
            after_second > after_first,
            "second run must extend history, not reset it ({after_first} → {after_second})"
        );
        let serialised = serde_json::to_string(&rt.history).unwrap();
        assert!(
            serialised.contains("我叫李伟"),
            "first user msg must persist"
        );
        assert!(
            serialised.contains("我叫什么"),
            "second user msg must persist"
        );
    }

    #[tokio::test]
    async fn history_resets_via_reset_history() {
        let (provider, _slot) = RecordingProvider::new();
        let registry = Arc::new(ToolRegistry::with_builtins());
        let session = Session::open(unique_log()).await.unwrap();
        let redactor = evo_policy::Redactor::from_vault(&evo_policy::Vault::default());
        let mut rt = ConversationRuntime::new(
            provider,
            registry,
            session,
            ToolContext::default(),
            RuntimeConfig::default(),
        )
        .with_redactor(redactor);

        rt.run("first").await.unwrap();
        assert!(rt.history_len() > 0);
        rt.reset_history();
        assert_eq!(rt.history_len(), 0, "reset_history clears all entries");
    }

    #[tokio::test]
    async fn acp_provider_detection_is_case_insensitive_and_trims() {
        // Regression: previous `starts_with("acp:")` missed "ACP:claude",
        // " acp:codex", and "Acp:Cursor" — those slipped through and
        // accumulated history that the upstream ACP agent already tracks,
        // double-billing tokens on every turn.
        for pid in ["ACP:claude", " acp:codex", "Acp:Cursor", "  acp:gemini  "] {
            let (provider, _slot) = RecordingProvider::new();
            let registry = Arc::new(ToolRegistry::with_builtins());
            let session = Session::open(unique_log()).await.unwrap();
            let redactor = evo_policy::Redactor::from_vault(&evo_policy::Vault::default());
            let mut rt = ConversationRuntime::new(
                provider,
                registry,
                session,
                ToolContext::default(),
                RuntimeConfig {
                    provider_id: Some(pid.into()),
                    ..Default::default()
                },
            )
            .with_redactor(redactor);
            rt.run("first").await.unwrap();
            let first = rt.history_len();
            rt.run("second").await.unwrap();
            assert_eq!(
                first,
                rt.history_len(),
                "ACP provider id '{pid}' should not accumulate history",
            );
        }
    }

    #[tokio::test]
    async fn acp_provider_does_not_accumulate_history() {
        let (provider, _slot) = RecordingProvider::new();
        let registry = Arc::new(ToolRegistry::with_builtins());
        let session = Session::open(unique_log()).await.unwrap();
        let redactor = evo_policy::Redactor::from_vault(&evo_policy::Vault::default());
        let mut rt = ConversationRuntime::new(
            provider,
            registry,
            session,
            ToolContext::default(),
            RuntimeConfig {
                provider_id: Some("acp:claude".into()),
                ..Default::default()
            },
        )
        .with_redactor(redactor);

        rt.run("first").await.unwrap();
        let len_after_first = rt.history_len();
        rt.run("second").await.unwrap();
        let len_after_second = rt.history_len();
        assert_eq!(
            len_after_first, len_after_second,
            "ACP runs should not accumulate (each run resets to fresh)"
        );
    }

    #[tokio::test]
    async fn max_turns_yields_error() {
        let provider = Arc::new(evo_mock_provider::MockProvider::looping_tool_call(
            "read_file",
            serde_json::json!({"path": "x"}),
        ));
        let registry = Arc::new(ToolRegistry::with_builtins());
        let session = Session::open(unique_log()).await.unwrap();
        let mut rt = ConversationRuntime::new(
            provider,
            registry,
            session,
            ToolContext {
                workspace: unique_ws(),
                ..Default::default()
            },
            RuntimeConfig {
                max_turns: 3,
                ..Default::default()
            },
        );
        let err = rt.run("loop").await.expect_err("should hit max turns");
        assert!(matches!(err, RuntimeError::MaxTurns(3)));
    }
}
