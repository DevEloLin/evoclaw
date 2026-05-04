//! Agent loop: PRD §10.1 + §31 Phase 1 subset of FSM.

use crate::compression::{compress_if_due, CompressionConfig};
use crate::distillation::{
    build_distillation_prompt, parse_distilled_skill, skill_from_reflection_quick, DistillCtx,
};
use crate::memory::{Memory, MemoryLayer, MemoryRecord};
use crate::prompt::{build_system_prompt, PromptCtx};
use crate::reflection::{
    build_reflection_prompt, parse_reflection, ReflectionCtx, ReflectionRecord, SkillUpdateDecision,
};
use crate::session::{
    EndRecord, RecordedToolCall, RecordedUsage, Session, SessionRecord, TaskRecord, TurnRecord,
};
use crate::skill::Skill;
use crate::skill_tree::SkillTree;
use crate::summary::SummaryParser;
use chrono::Utc;
use evo_policy::{
    estimate_usd, is_fully_redacted, BudgetCheck, CostEngine, CostEvent, RedactionMode, Redactor,
};
use evo_providers::{
    ChatRequest, Message, Provider, ProviderError, StreamEvent, ToolCall, ToolFingerprint,
    ToolPayload, ToolResult,
};
use evo_tools::{ToolContext, ToolError, ToolRegistry};
use futures::StreamExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// Upper bound on a single provider streaming call inside the per-turn loop.
/// On timeout the turn is recorded as a failure and the run is aborted with
/// `completed = false` (partial assistant text preserved).
const TURN_TIMEOUT: Duration = Duration::from_secs(300);

/// Upper bound on the reflection / distillation provider call. These are
/// best-effort closeouts; on timeout we fall back to the quick synthesiser.
const REFLECTION_TIMEOUT: Duration = Duration::from_secs(120);

/// Marker text used by `head_tail` to signal the omitted middle section.
const OMIT: &str = " ... ";

/// After this many consecutive `Err(_)` results from the budget engine we
/// treat the cost log as unreadable and hard-stop the loop (no cost
/// visibility = stop spending money).
const BUDGET_ERR_HARD_STOP: u32 = 3;

/// Phase 2.7 — explicit Task FSM (PRD §31).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum TaskState {
    Received,
    Planning,
    ToolExecuting,
    Observing,
    AwaitingUser,
    Reflecting,
    Distilling,
    Completed,
    Failed,
    Archived,
}

#[derive(Debug, thiserror::Error)]
pub enum RuntimeError {
    #[error("provider: {0}")]
    Provider(#[from] ProviderError),
    #[error("tool: {0}")]
    Tool(#[from] ToolError),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("max turns reached: {0}")]
    MaxTurns(u64),
    #[error("budget: {0}")]
    Budget(String),
}

#[derive(Debug, Clone)]
pub struct RunOutcome {
    pub task_id: String,
    pub turns: u64,
    /// Strict (log-mode) scrubbed final text. Safe for on-disk artefacts —
    /// JSONL, audit, memory. Generic high-entropy strings are masked here.
    pub final_text: String,
    /// UI-mode scrubbed final text (`prd/plan/ask.md`). Vault and
    /// known-prefix credentials are masked, but generic high-entropy
    /// fallback is suppressed so identifiers / paths / prose render
    /// cleanly on the user's terminal. CLI renderers should prefer this.
    pub final_text_ui: String,
    pub completed: bool,
    /// Token usage rolled up across every turn that reported a `Usage`
    /// stream event. Defaults to zero for providers that don't report it
    /// (notably ACP, since the upstream agent owns its own metering).
    pub usage: RunUsage,
}

/// Aggregate token usage for an entire `run()` invocation.
#[derive(Debug, Clone, Copy, Default)]
pub struct RunUsage {
    pub input_tokens: u64,
    pub cached_tokens: u64,
    pub output_tokens: u64,
    /// How many turns actually carried a `Usage` event. Useful to
    /// distinguish "0 because the provider doesn't report" from "0 because
    /// the prompt was empty".
    pub turns_with_usage: u64,
}

impl RunUsage {
    pub fn cache_hit_rate(&self) -> f64 {
        if self.input_tokens == 0 {
            0.0
        } else {
            self.cached_tokens as f64 / self.input_tokens as f64
        }
    }
    fn add(&mut self, u: &evo_providers::Usage) {
        self.input_tokens = self.input_tokens.saturating_add(u.input_tokens);
        self.cached_tokens = self.cached_tokens.saturating_add(u.cached_tokens);
        self.output_tokens = self.output_tokens.saturating_add(u.output_tokens);
        self.turns_with_usage = self.turns_with_usage.saturating_add(1);
    }
}

#[derive(Debug, Clone)]
pub struct RuntimeConfig {
    pub max_turns: u64,
    pub max_tokens: u32,
    pub temperature: f32,
    pub model: String,
    /// Run the reflection-and-distillation closeout after a successful task
    /// (PRD §11). Defaults to `true` to preserve memory/skill learning for
    /// regular API providers. ACP-backed providers should turn this off
    /// because the upstream agent IS already a full agent — letting it
    /// double-bill on a "reflection" prompt for every short interaction
    /// can easily double-or-triple the wall time of trivial questions.
    pub reflection_enabled: bool,
    /// Provider ID for logging (e.g., "deepseek", "openai", "acp:claude")
    pub provider_id: Option<String>,
    /// Active MCP server names for logging
    pub mcp_servers: Vec<String>,
    /// Channel-specific formatting instruction forwarded to the system prompt.
    /// Set by `channel_run_one_shot_text` based on the inbound channel kind.
    pub channel_hint: Option<String>,
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            max_turns: 25,
            max_tokens: 1024,
            temperature: 0.2,
            model: "gpt-4o-mini".into(),
            reflection_enabled: true,
            provider_id: None,
            mcp_servers: Vec::new(),
            channel_hint: None,
        }
    }
}

pub struct ConversationRuntime<P: Provider + ?Sized> {
    provider: Arc<P>,
    registry: Arc<ToolRegistry>,
    session: Session,
    tool_ctx: ToolContext,
    prompt_ctx: PromptCtx,
    config: RuntimeConfig,
    fingerprint: ToolFingerprint,
    summaries: SummaryParser,
    compression_cfg: CompressionConfig,
    cost: Option<Arc<CostEngine>>,
    memory: Option<Memory>,
    skills_dir: Option<PathBuf>,
    distill_via_model: bool,
    /// Secret-redaction barrier (PRD §13.4). When `Some`, every user_input,
    /// tool arg and tool result is scrubbed before reaching the model.
    redactor: Option<Redactor>,
    /// Optional channel for forwarding streaming deltas to a UI renderer.
    /// Each message is the raw (pre-scrub) assistant text chunk.
    delta_tx: Option<tokio::sync::mpsc::UnboundedSender<String>>,
}

impl<P: Provider + ?Sized> ConversationRuntime<P> {
    pub fn new(
        provider: Arc<P>,
        registry: Arc<ToolRegistry>,
        session: Session,
        tool_ctx: ToolContext,
        config: RuntimeConfig,
    ) -> Self {
        let mut prompt_ctx = PromptCtx::today_in(tool_ctx.workspace.display().to_string());
        prompt_ctx.tool_names = registry.names();
        prompt_ctx.channel_hint = config.channel_hint.clone();
        Self {
            provider,
            registry,
            session,
            tool_ctx,
            prompt_ctx,
            config,
            fingerprint: ToolFingerprint::default(),
            summaries: SummaryParser::default(),
            compression_cfg: CompressionConfig::default(),
            cost: None,
            memory: None,
            skills_dir: None,
            distill_via_model: true,
            redactor: None,
            delta_tx: None,
        }
    }

    /// Attach a streaming-delta channel. Every assistant text chunk produced
    /// during `run()` is forwarded to this sender before buffering internally.
    /// Used by the interactive UI renderer (`prd/plan/ui.md` §3 / §6).
    pub fn with_delta_sender(mut self, tx: tokio::sync::mpsc::UnboundedSender<String>) -> Self {
        self.delta_tx = Some(tx);
        self
    }

    pub fn with_cost_engine(mut self, cost: Arc<CostEngine>) -> Self {
        self.cost = Some(cost);
        self
    }

    pub fn with_compression(mut self, cfg: CompressionConfig) -> Self {
        self.compression_cfg = cfg;
        self
    }

    pub fn with_memory(mut self, memory: Memory) -> Self {
        self.memory = Some(memory);
        self
    }

    pub fn with_skills_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.skills_dir = Some(dir.into());
        self
    }

    /// When `false`, distillation skips the second model call and synthesises
    /// a Skill directly from the reflection record (cost-saving fallback).
    pub fn with_distill_via_model(mut self, enabled: bool) -> Self {
        self.distill_via_model = enabled;
        self
    }

    /// Install the secret-redaction barrier. Every text payload that flows
    /// through `run()` — user input, tool args, tool results, assistant
    /// text — is scrubbed via this redactor before it lands in either the
    /// LLM prompt or the JSONL session log.
    pub fn with_redactor(mut self, redactor: Redactor) -> Self {
        self.redactor = Some(redactor);
        self
    }

    /// Strict (log-mode) scrub. Use for anything that hits disk: JSONL,
    /// audit, debug. Equivalent to `redact_for_log`.
    fn scrub(&self, text: &str) -> String {
        self.scrub_with(text, RedactionMode::Log)
    }

    /// Conservative (model-mode) scrub. Use for outbound provider /
    /// ACP / API requests. Suppresses the generic high-entropy fallback
    /// so that ordinary user input (CJK prose, file paths, shell
    /// commands) reaches the upstream agent verbatim. See
    /// `prd/plan/acp.md`.
    fn scrub_for_model(&self, text: &str) -> String {
        self.scrub_with(text, RedactionMode::Model)
    }

    fn scrub_with(&self, text: &str, mode: RedactionMode) -> String {
        if let Some(r) = &self.redactor {
            let (out, _hits) = r.scrub_with(text, mode);
            return out;
        }
        text.to_string()
    }

    /// Mode-aware variant of `scrub_value` for outbound JSON arguments
    /// going to the upstream model / provider. Walks the JSON tree the
    /// same way as `scrub_value` but uses model-mode rules on every
    /// string leaf.
    fn scrub_value_for_model(&self, v: &serde_json::Value) -> serde_json::Value {
        if self.redactor.is_none() {
            return v.clone();
        }
        match v {
            serde_json::Value::String(s) => serde_json::Value::String(self.scrub_for_model(s)),
            serde_json::Value::Array(items) => serde_json::Value::Array(
                items
                    .iter()
                    .map(|x| self.scrub_value_for_model(x))
                    .collect(),
            ),
            serde_json::Value::Object(map) => serde_json::Value::Object(
                map.iter()
                    .map(|(k, val)| (k.clone(), self.scrub_value_for_model(val)))
                    .collect(),
            ),
            _ => v.clone(),
        }
    }

    fn scrub_value(&self, v: &serde_json::Value) -> serde_json::Value {
        if self.redactor.is_none() {
            return v.clone();
        }
        match v {
            serde_json::Value::String(s) => serde_json::Value::String(self.scrub(s)),
            serde_json::Value::Array(items) => {
                serde_json::Value::Array(items.iter().map(|x| self.scrub_value(x)).collect())
            }
            serde_json::Value::Object(map) => serde_json::Value::Object(
                map.iter()
                    .map(|(k, val)| (k.clone(), self.scrub_value(val)))
                    .collect(),
            ),
            _ => v.clone(),
        }
    }

    pub async fn run(&mut self, user_input: &str) -> Result<RunOutcome, RuntimeError> {
        // PRD §13.4 + acp.md — split scrubbing into two channels:
        //
        //   user_input_safe_log   strict scrub for on-disk artefacts
        //                         (JSONL TaskRecord, memory, summaries).
        //                         Generic high-entropy strings are still
        //                         masked here because the disk is a colder
        //                         security boundary than the live model
        //                         request.
        //
        //   user_input_safe_model conservative scrub for outbound
        //                         provider / ACP requests. Vault, PEM, and
        //                         known-prefix credential patterns still
        //                         fire; the high-entropy fallback does
        //                         not. This is what acp.md mandates so
        //                         normal user prose (CJK, paths, shell
        //                         commands) reaches the upstream agent
        //                         verbatim.
        let user_input_safe = self.scrub(user_input);
        let user_input_safe_model = self.scrub_for_model(user_input);

        // Hard guard: if even the conservative scrub erased the entire
        // payload (e.g. the user pasted a single bare token), fail
        // locally instead of sending an unintelligible `[REDACTED:...]`
        // marker upstream — that is exactly the symptom acp.md was filed
        // to fix.
        if is_fully_redacted(&user_input_safe_model) {
            return Err(RuntimeError::Provider(evo_providers::ProviderError::Other(
                "Request was fully redacted before sending. This is likely an EvoClaw \
                     redaction bug. Run with EVOCLAW_DEBUG_PROVIDER=1 to inspect sanitized \
                     payload metadata."
                    .into(),
            )));
        }

        emit_provider_debug(
            self.config.provider_id.as_deref(),
            user_input,
            &user_input_safe,
            &user_input_safe_model,
        );

        let task_id = format!("task-{}", Utc::now().format("%Y%m%dT%H%M%S%.3f"));
        self.session
            .append(&SessionRecord::Task(TaskRecord {
                task_id: task_id.clone(),
                user_input: user_input_safe.clone(),
                source: "cli".into(),
                model: self.config.model.clone(),
                provider: self.config.provider_id.clone(),
                acp_agent: self
                    .config
                    .provider_id
                    .as_ref()
                    .filter(|p| p.starts_with("acp:"))
                    .map(|p| p.strip_prefix("acp:").unwrap_or(p).to_string()),
                mcp_servers: self.config.mcp_servers.clone(),
                skills_used: Vec::new(), // Populated during execution
                started_at: Utc::now(),
            }))
            .await?;

        // PRD §16 — JSONL closure invariant: every task must end with an `End`
        // record. The guard below uses a sync `std::fs::OpenOptions` append on
        // Drop so even a panic inside the loop still seals the log. Flipped to
        // `true` immediately before the normal `End` record is written.
        let end_written = Arc::new(AtomicBool::new(false));
        let _session_guard = SessionEndGuard {
            path: self.session.path().to_path_buf(),
            end_written: end_written.clone(),
        };

        // C3: load active skills into L1 index so every turn sees current skill context.
        if let Some(ref dir) = self.skills_dir {
            if let Ok(tree) = SkillTree::rebuild_from_dir(dir).await {
                let active = tree.active();
                if !active.is_empty() {
                    let mut index = active
                        .iter()
                        .map(|n| format!("{}: {}", n.id, n.name))
                        .collect::<Vec<_>>()
                        .join("; ");
                    if index.len() > 500 {
                        index.truncate(497);
                        index.push_str("...");
                    }
                    self.prompt_ctx.l1_index = index;
                }
            }
        }

        let system_msg = Message::system(build_system_prompt(&self.prompt_ctx));
        let mut history: Vec<Message> = vec![system_msg];
        // Model-mode user input: outbound to provider / ACP. Vault and
        // known-prefix patterns are still redacted; generic high-entropy
        // is not. See acp.md.
        let mut next_user_payload = self.compose_initial_user_msg(&user_input_safe_model);
        let mut completed = false;
        let mut final_text = String::new();
        let mut last_assistant_text_safe = String::new();
        let mut turn = 0u64;
        let mut budget_err_streak: u32 = 0;
        let mut tool_error_count: u32 = 0;
        let mut total_usage = RunUsage::default();

        while turn < self.config.max_turns {
            history.push(next_user_payload.clone());

            // PRD §42.5 — periodic tag-level compression of older history.
            compress_if_due(&mut history, turn, self.compression_cfg);

            // PRD §35 — pre-flight budget check. Soft warns are surfaced via
            // `tracing::warn!`; transient I/O errors are tolerated up to
            // `BUDGET_ERR_HARD_STOP` consecutive failures, after which we
            // hard-stop (no cost visibility = stop spending money).
            if let Some(cost) = &self.cost {
                match cost.check_for_task(&task_id).await {
                    Ok(BudgetCheck::HardStop(level)) => {
                        return Err(RuntimeError::Budget(format!("hard stop: {level:?}")));
                    }
                    Ok(BudgetCheck::SoftWarn(level)) => {
                        budget_err_streak = 0;
                        tracing::warn!(?level, "soft budget warning");
                    }
                    Ok(BudgetCheck::Ok) => {
                        budget_err_streak = 0;
                    }
                    Err(e) => {
                        budget_err_streak = budget_err_streak.saturating_add(1);
                        tracing::warn!(error=?e, streak=budget_err_streak, "budget check failed");
                        if budget_err_streak >= BUDGET_ERR_HARD_STOP {
                            return Err(RuntimeError::Budget(format!(
                                "cost log unreadable for {budget_err_streak} consecutive checks"
                            )));
                        }
                    }
                }
            }

            let tools = self.registry.specs();
            let payload = self.fingerprint.payload_for_turn(turn, tools);

            let req = ChatRequest {
                model: self.config.model.clone(),
                messages: history.clone(),
                tools: payload,
                max_tokens: self.config.max_tokens,
                temperature: self.config.temperature,
            };

            let mut assistant_text = String::new();
            let mut tool_calls: Vec<ToolCall> = Vec::new();
            let mut usage = None;

            // Bound the entire provider stream (open + drain) by `TURN_TIMEOUT`.
            // On timeout the partial assistant text is preserved, a synthetic
            // failed-turn record is appended, and the run ends with
            // `completed = false`.
            let stream_fut = async {
                let mut stream = self.provider.stream(req).await?;
                while let Some(event) = stream.next().await {
                    match event? {
                        StreamEvent::Delta(t) => {
                            // Forward to the UI renderer if a delta channel is attached.
                            if let Some(tx) = &self.delta_tx {
                                let _ = tx.send(t.clone());
                            }
                            assistant_text.push_str(&t);
                        }
                        StreamEvent::ToolCallStart(tc) => tool_calls.push(tc),
                        StreamEvent::ToolCallFinish => {}
                        StreamEvent::Usage(u) => usage = Some(u),
                        StreamEvent::Done => break,
                    }
                }
                Ok::<(), ProviderError>(())
            };
            match tokio::time::timeout(TURN_TIMEOUT, stream_fut).await {
                Ok(Ok(())) => {}
                Ok(Err(e)) => return Err(RuntimeError::Provider(e)),
                Err(_) => {
                    tracing::warn!(turn, "provider stream timed out");
                    let assistant_text_safe = self.scrub(&assistant_text);
                    last_assistant_text_safe = assistant_text_safe.clone();
                    self.session
                        .append(&SessionRecord::Turn(TurnRecord {
                            turn,
                            summary: Some(format!(
                                "[ERROR] provider stream timeout after {}s",
                                TURN_TIMEOUT.as_secs()
                            )),
                            tool_calls: Vec::new(),
                            usage: None,
                            ts: Utc::now(),
                        }))
                        .await?;
                    turn += 1;
                    completed = false;
                    break;
                }
            }

            // Two scrubs of the assistant turn:
            //   * `_safe`        — strict (logs, summaries, JSONL).
            //   * `_safe_model`  — conservative (the message we re-feed
            //                      to the model on the next turn through
            //                      the `history` buffer).
            let assistant_text_safe = self.scrub(&assistant_text);
            let assistant_text_safe_model = self.scrub_for_model(&assistant_text);
            last_assistant_text_safe = assistant_text_safe.clone();
            let safe_calls_for_model: Vec<ToolCall> = tool_calls
                .iter()
                .map(|c| ToolCall {
                    id: c.id.clone(),
                    name: c.name.clone(),
                    arguments: self.scrub_value_for_model(&c.arguments),
                })
                .collect();
            history.push(Message {
                role: evo_providers::Role::Assistant,
                content: assistant_text_safe_model,
                tool_calls: safe_calls_for_model,
                tool_results: Vec::new(),
                cache_control: evo_providers::CacheKind::None,
            });

            let summary = self.summaries.ingest(&assistant_text_safe);

            let mut recorded_calls = Vec::with_capacity(tool_calls.len());
            let mut tool_results: Vec<ToolResult> = Vec::with_capacity(tool_calls.len());
            for call in &tool_calls {
                // PRD §13.4 — dispatch the *scrubbed* args (not the raw model
                // output). Built-in tools and MCP wrappers both consume JSON,
                // so a scrubbed `Value` is a safe substitute and prevents an
                // MCP server from receiving a secret the model echoed back.
                let safe_args = self.scrub_value(&call.arguments);
                match self
                    .registry
                    .invoke(&self.tool_ctx, &call.name, safe_args.clone())
                    .await
                {
                    Ok(out) => {
                        let safe_out_log = self.scrub(&out);
                        let safe_out_model = self.scrub_for_model(&out);
                        recorded_calls.push(RecordedToolCall {
                            name: call.name.clone(),
                            args: safe_args.clone(),
                            result_truncated: safe_out_log,
                            is_error: false,
                        });
                        tool_results.push(ToolResult {
                            call_id: call.id.clone(),
                            content: safe_out_model,
                            is_error: false,
                        });
                    }
                    Err(e) => {
                        tool_error_count = tool_error_count.saturating_add(1);
                        let err_log = self.scrub(&e.to_string());
                        let err_model = self.scrub_for_model(&e.to_string());
                        recorded_calls.push(RecordedToolCall {
                            name: call.name.clone(),
                            args: safe_args,
                            result_truncated: err_log,
                            is_error: true,
                        });
                        tool_results.push(ToolResult {
                            call_id: call.id.clone(),
                            content: err_model,
                            is_error: true,
                        });
                    }
                }
            }

            // PRD §35 — record cost event before persisting turn (so doctor sees it).
            if let (Some(cost), Some(u)) = (&self.cost, usage.as_ref()) {
                let usd = estimate_usd(u.input_tokens, u.cached_tokens, u.output_tokens);
                let _ = cost
                    .record(&CostEvent {
                        ts: Utc::now(),
                        task_id: task_id.clone(),
                        model: self.config.model.clone(),
                        input_tokens: u.input_tokens,
                        cached_tokens: u.cached_tokens,
                        output_tokens: u.output_tokens,
                        usd,
                    })
                    .await;
            }
            if let Some(u) = usage.as_ref() {
                total_usage.add(u);
            }

            self.session
                .append(&SessionRecord::Turn(TurnRecord {
                    turn,
                    summary,
                    tool_calls: recorded_calls,
                    usage: usage.map(|u| RecordedUsage {
                        input: u.input_tokens,
                        cached: u.cached_tokens,
                        output: u.output_tokens,
                    }),
                    ts: Utc::now(),
                }))
                .await?;

            if tool_calls.is_empty() {
                completed = true;
                final_text = assistant_text_safe;
                break;
            }

            next_user_payload = self.compose_next_user_msg(tool_results);
            turn += 1;
        }

        // Fix 3 — preserve partial progress: if the loop exited without
        // `completed = true` but the last assistant turn produced text,
        // surface that to the caller instead of an empty string.
        if !completed && final_text.is_empty() && !last_assistant_text_safe.is_empty() {
            final_text = last_assistant_text_safe.clone();
        }

        // Phase 2 — reflection round before terminal state record. Pass the
        // already-scrubbed user_input so reflection cannot leak secrets even
        // by accident.
        let reflection = if completed
            && self.config.reflection_enabled
            && (self.memory.is_some() || self.skills_dir.is_some())
        {
            self.reflection_round(
                &task_id,
                &final_text,
                // Reflection re-prompts the model, so it must use the
                // conservatively-redacted user input — same rule as the
                // initial provider request.
                &user_input_safe_model,
                completed,
                tool_error_count,
            )
            .await
        } else {
            None
        };

        let terminal = match (completed, reflection.as_ref()) {
            (true, Some(r)) if r.success => TaskState::Completed,
            (true, _) => TaskState::Completed,
            (false, _) => TaskState::Failed,
        };

        // Flip the panic-safety flag *before* appending the real End record so
        // the SessionEndGuard does not double-write on Drop.
        end_written.store(true, Ordering::SeqCst);
        self.session
            .append(&SessionRecord::End(EndRecord {
                state: format!("{terminal:?}").to_uppercase(),
                finished_at: Utc::now(),
            }))
            .await?;

        if !completed {
            return Err(RuntimeError::MaxTurns(self.config.max_turns));
        }
        // ask.md — produce a UI-mode scrubbed twin of `final_text`. The CLI
        // renderer prefers this so generic high-entropy false positives
        // (paths with unusual segments, identifiers, prose) don't show up
        // as `[REDACTED:high_entropy:...]` in the answer block.
        let final_text_ui = self.scrub_with(&final_text, RedactionMode::Ui);
        Ok(RunOutcome {
            task_id,
            turns: turn + 1,
            final_text,
            final_text_ui,
            completed,
            usage: total_usage,
        })
    }

    /// PRD §11 — Reflection + Distillation closeout. Best-effort: any IO or
    /// model error is recorded but not propagated, so the task still succeeds.
    async fn reflection_round(
        &mut self,
        task_id: &str,
        final_text: &str,
        user_input: &str,
        completed: bool,
        tool_error_count: u32,
    ) -> Option<ReflectionRecord> {
        // Collect active skill IDs so the LLM can decide update vs. create.
        let active_skill_ids = if let Some(ref dir) = self.skills_dir {
            SkillTree::rebuild_from_dir(dir)
                .await
                .map(|t| t.active().iter().map(|n| n.id.clone()).collect::<Vec<_>>())
                .unwrap_or_default()
        } else {
            Vec::new()
        };

        // Build a reflection-only ChatRequest (no tools).
        let prompt = build_reflection_prompt(&ReflectionCtx {
            task_id: task_id.into(),
            final_result_truncated: head_tail(final_text, 2000),
            trajectory_truncated: head_tail(final_text, 2000),
            active_skill_ids,
        });
        let messages = vec![
            Message::system(build_system_prompt(&self.prompt_ctx)),
            Message::user(prompt),
        ];
        let req = ChatRequest {
            model: self.config.model.clone(),
            messages,
            tools: ToolPayload::Full(Vec::new()),
            max_tokens: 1024,
            temperature: 0.0,
        };

        // Bound the reflection provider call. On timeout / error we return
        // `None`; the run still completes normally.
        let stream_fut = async {
            let mut text = String::new();
            match self.provider.stream(req).await {
                Ok(mut s) => {
                    while let Some(ev) = s.next().await {
                        match ev {
                            Ok(StreamEvent::Delta(t)) => text.push_str(&t),
                            Ok(StreamEvent::Done) => break,
                            Ok(_) => {}
                            Err(_) => return None,
                        }
                    }
                    Some(text)
                }
                Err(_) => None,
            }
        };
        let text = match tokio::time::timeout(REFLECTION_TIMEOUT, stream_fut).await {
            Ok(Some(t)) => t,
            Ok(None) => return None,
            Err(_) => {
                tracing::warn!(task_id, "reflection provider call timed out");
                return None;
            }
        };

        let refl = match parse_reflection(&text) {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(task_id, error = %e, "reflection parse failed; skipping distillation");
                return None;
            }
        };

        // Memory L3 write (PRD §33). The body lands on disk, so re-scrub
        // with the strict (log) mode in case the caller handed us the
        // model-mode version (which keeps generic high-entropy strings
        // intact).
        if let Some(mem) = self.memory.clone() {
            let user_input_log = self.scrub(user_input);
            let summary_log = self.scrub(&refl.summary);
            let goal_log = self.scrub(&refl.user_real_goal);
            let failures_log: Vec<String> = refl
                .failure_patterns
                .iter()
                .map(|f| self.scrub(f))
                .collect();
            let body = format!(
                "task={task_id}\nuser_input={user_input_log}\nsuccess={}\nsummary={summary_log}\ngoal={goal_log}\nfailures={}",
                refl.success, failures_log.join("; ")
            );
            let mut record =
                MemoryRecord::new(MemoryLayer::L3, body, "reflection", refl.confidence);
            record.tags = vec![
                "reflection".into(),
                if refl.success { "success" } else { "failure" }.into(),
            ];
            if let Err(e) = mem.write(record).await {
                tracing::warn!(task_id, error = %e, "failed to persist L3 memory record");
            }
        }

        // Distillation → Skill (PRD §11.3). Branches on skill_update_decision so
        // Update/Merge/Deprecate act on the existing target rather than creating
        // a parallel duplicate.
        if let Some(dir) = self.skills_dir.clone() {
            match refl.skill_update_decision {
                SkillUpdateDecision::None => {}

                SkillUpdateDecision::Deprecate => {
                    if let Some(ref target_id) = refl.target_skill_id {
                        let path = dir.join(format!("{target_id}.yaml"));
                        match Skill::load_yaml(&path).await {
                            Ok(mut sk) => {
                                let old_state = sk.state;
                                sk.state = crate::skill::SkillState::Deprecated;
                                sk.updated_at = Utc::now();
                                sk.changelog.push(format!(
                                    "v{} {old_state:?} → Deprecated (model requested)",
                                    sk.version
                                ));
                                if let Err(e) = sk.save_yaml(&dir).await {
                                    tracing::warn!(task_id, skill_id = %target_id, error = %e, "failed to save deprecated skill");
                                }
                            }
                            Err(e) => {
                                tracing::warn!(task_id, skill_id = %target_id, error = %e, "deprecate: target skill not found");
                            }
                        }
                    }
                }

                SkillUpdateDecision::Update | SkillUpdateDecision::Merge => {
                    let skill = self
                        .distil_skill(task_id, &refl, user_input, final_text)
                        .await;
                    if let Some(mut sk) = skill {
                        // If the model named a target, merge identity and content.
                        if let Some(ref target_id) = refl.target_skill_id {
                            let path = dir.join(format!("{target_id}.yaml"));
                            if let Ok(existing) = Skill::load_yaml(&path).await {
                                sk.id = existing.id.clone();
                                sk.version = existing.version + 1;
                                sk.parent = Some(existing.id.clone());
                                // Union of triggers, capped at 12.
                                let mut merged = existing.triggers.clone();
                                for t in &sk.triggers {
                                    if !merged.contains(t) {
                                        merged.push(t.clone());
                                    }
                                }
                                merged.truncate(12);
                                sk.triggers = merged;
                                // Prepend existing steps not already covered.
                                for step in existing.steps.iter().rev() {
                                    if !sk.steps.iter().any(|s| s.tool == step.tool) {
                                        sk.steps.insert(0, step.clone());
                                    }
                                }
                            }
                        }
                        if completed && tool_error_count == 0 {
                            sk.record_sandbox_pass();
                        } else {
                            sk.record_sandbox_fail();
                        }
                        if let Err(e) = sk.save_yaml(&dir).await {
                            tracing::warn!(task_id, skill_id = %sk.id, error = %e, "failed to save updated skill");
                        }
                    }
                }

                SkillUpdateDecision::Create => {
                    let skill = self
                        .distil_skill(task_id, &refl, user_input, final_text)
                        .await;
                    if let Some(mut sk) = skill {
                        if completed && tool_error_count == 0 {
                            sk.record_sandbox_pass();
                        } else {
                            sk.record_sandbox_fail();
                        }
                        if let Err(e) = sk.save_yaml(&dir).await {
                            tracing::warn!(task_id, skill_id = %sk.id, error = %e, "failed to save distilled skill");
                        }
                    }
                }
            }
        }

        Some(refl)
    }

    async fn distil_skill(
        &self,
        task_id: &str,
        reflection: &ReflectionRecord,
        user_input: &str,
        trajectory: &str,
    ) -> Option<Skill> {
        if !self.distill_via_model {
            return Some(skill_from_reflection_quick(reflection, task_id, user_input));
        }
        let refl_json = serde_json::to_string(reflection).unwrap_or_default();
        let prompt = build_distillation_prompt(&DistillCtx {
            task_id: task_id.into(),
            reflection_json: refl_json,
            trajectory_truncated: head_tail(trajectory, 4000),
        });
        let req = ChatRequest {
            model: self.config.model.clone(),
            messages: vec![Message::user(prompt)],
            tools: ToolPayload::Full(Vec::new()),
            max_tokens: 1024,
            temperature: 0.0,
        };
        // Bound the distillation provider call. On timeout / error fall back
        // to the local `skill_from_reflection_quick` synthesiser.
        let stream_fut = async {
            let mut text = String::new();
            match self.provider.stream(req).await {
                Ok(mut s) => {
                    while let Some(ev) = s.next().await {
                        match ev {
                            Ok(StreamEvent::Delta(t)) => text.push_str(&t),
                            Ok(StreamEvent::Done) => break,
                            Ok(_) => {}
                            Err(_) => return None,
                        }
                    }
                    Some(text)
                }
                Err(_) => None,
            }
        };
        let text = match tokio::time::timeout(REFLECTION_TIMEOUT, stream_fut).await {
            Ok(Some(t)) => t,
            Ok(None) => return Some(skill_from_reflection_quick(reflection, task_id, user_input)),
            Err(_) => {
                tracing::warn!(task_id, "distillation provider call timed out");
                return Some(skill_from_reflection_quick(reflection, task_id, user_input));
            }
        };
        match parse_distilled_skill(&text, task_id) {
            Ok(sk) => Some(sk),
            Err(_) => Some(skill_from_reflection_quick(reflection, task_id, user_input)),
        }
    }

    fn compose_initial_user_msg(&self, user_input: &str) -> Message {
        let history = self.summaries.render_history_block();
        let prefix = if history.is_empty() {
            String::new()
        } else {
            format!("{history}\n\n")
        };
        Message::user(format!("{prefix}<user_input>\n{user_input}\n</user_input>"))
    }

    fn compose_next_user_msg(&self, tool_results: Vec<ToolResult>) -> Message {
        let history = self.summaries.render_history_block();
        let mut parts = Vec::new();
        if !history.is_empty() {
            parts.push(history);
        }
        parts.push("<tool_results>".into());
        for r in &tool_results {
            parts.push(format!("[{}] {}", r.call_id, r.content));
        }
        parts.push("</tool_results>".into());
        let mut msg = Message::user(parts.join("\n"));
        msg.tool_results = tool_results;
        msg
    }
}

fn head_tail(s: &str, max: usize) -> String {
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
struct SessionEndGuard {
    path: PathBuf,
    end_written: Arc<AtomicBool>,
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
/// secrets** — only:
///   * the provider id
///   * char-counts for raw / log-mode / model-mode versions
///   * the redaction count (model-mode minus log-mode pass-throughs)
///   * the source channel (`raw_user_input` if no scrub touched it,
///     `sanitized_for_model` otherwise)
///   * a short head-tail preview of the model-mode version, with all
///     internal `[REDACTED:...]` markers preserved (markers are safe;
///     real secret values were already removed before this point)
///   * a stable 8-char SHA-256 fingerprint of the raw input, for
///     correlation across log lines without exposing the value
///
/// The helper is intentionally a free function (no `&self`) so it can be
/// called before any redactor work happens, and it is gated by the env
/// var so a user must explicitly opt in.
fn emit_provider_debug(
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

fn provider_debug_enabled() -> bool {
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

fn append_synthetic_end(path: &Path) -> std::io::Result<()> {
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
    use super::*;
    use evo_mock_provider::MockProvider;

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
        let provider = Arc::new(MockProvider::scripted(vec![
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
    /// outbound user payload. Asserting on `<user_input> ... </user_input>`
    /// content confirms what the upstream agent actually sees.
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
        // A bare OpenAI key as the entire prompt — every byte should be
        // rewritten into a `[REDACTED:openai_key:...]` marker, leaving
        // nothing for the upstream agent. The runtime must refuse rather
        // than dispatch.
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
    async fn max_turns_yields_error() {
        let provider = Arc::new(MockProvider::looping_tool_call(
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
