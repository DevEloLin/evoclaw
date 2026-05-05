//! Agent loop: PRD §10.1 + §31 Phase 1 subset of FSM.

mod exec;
mod reflect;
pub(crate) mod util;

use crate::compression::CompressionConfig;
use crate::memory::Memory;
use crate::prompt::PromptCtx;
use crate::session::Session;
use crate::summary::SummaryParser;
use evo_policy::{CostEngine, RedactionMode, Redactor};
use evo_providers::{Message, Provider, ToolFingerprint, ToolResult};
use evo_tools::{ToolContext, ToolRegistry};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

/// Upper bound on a single provider streaming call inside the per-turn loop.
/// On timeout the turn is recorded as a failure and the run is aborted with
/// `completed = false` (partial assistant text preserved).
pub(crate) const TURN_TIMEOUT: Duration = Duration::from_secs(300);

/// Upper bound on the reflection / distillation provider call. These are
/// best-effort closeouts; on timeout we fall back to the quick synthesiser.
pub(crate) const REFLECTION_TIMEOUT: Duration = Duration::from_secs(120);

/// After this many consecutive `Err(_)` results from the budget engine we
/// treat the cost log as unreadable and hard-stop the loop (no cost
/// visibility = stop spending money).
pub(crate) const BUDGET_ERR_HARD_STOP: u32 = 3;

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
    Provider(#[from] evo_providers::ProviderError),
    #[error("tool: {0}")]
    Tool(#[from] evo_tools::ToolError),
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
    pub(crate) fn add(&mut self, u: &evo_providers::Usage) {
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
            max_tokens: 4096,
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
    pub(crate) provider: Arc<P>,
    pub(crate) registry: Arc<ToolRegistry>,
    pub(crate) session: Session,
    pub(crate) tool_ctx: ToolContext,
    pub(crate) prompt_ctx: PromptCtx,
    pub(crate) config: RuntimeConfig,
    pub(crate) fingerprint: ToolFingerprint,
    pub(crate) summaries: SummaryParser,
    pub(crate) compression_cfg: CompressionConfig,
    pub(crate) cost: Option<Arc<CostEngine>>,
    pub(crate) memory: Option<Memory>,
    pub(crate) skills_dir: Option<PathBuf>,
    pub(crate) distill_via_model: bool,
    /// Secret-redaction barrier (PRD §13.4). When `Some`, every user_input,
    /// tool arg and tool result is scrubbed before reaching the model.
    pub(crate) redactor: Option<Redactor>,
    /// Optional channel for forwarding streaming deltas to a UI renderer.
    /// Each message is the raw (pre-scrub) assistant text chunk.
    pub(crate) delta_tx: Option<tokio::sync::mpsc::UnboundedSender<String>>,
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
    pub(crate) fn scrub(&self, text: &str) -> String {
        self.scrub_with(text, RedactionMode::Log)
    }

    /// Conservative (model-mode) scrub. Use for outbound provider /
    /// ACP / API requests. Suppresses the generic high-entropy fallback
    /// so that ordinary user input (CJK prose, file paths, shell
    /// commands) reaches the upstream agent verbatim. See
    /// `prd/plan/acp.md`.
    pub(crate) fn scrub_for_model(&self, text: &str) -> String {
        self.scrub_with(text, RedactionMode::Model)
    }

    pub(crate) fn scrub_with(&self, text: &str, mode: RedactionMode) -> String {
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
    pub(crate) fn scrub_value_for_model(&self, v: &serde_json::Value) -> serde_json::Value {
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

    pub(crate) fn scrub_value(&self, v: &serde_json::Value) -> serde_json::Value {
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

    pub(crate) fn compose_initial_user_msg(&self, user_input: &str) -> Message {
        let history = self.summaries.render_history_block();
        let prefix = if history.is_empty() {
            String::new()
        } else {
            format!("{history}\n\n")
        };
        Message::user(format!(
            "{prefix}<user_input>\n{user_input}\n</user_input>\n\
             <format_requirement>Reply in standard Markdown: \
             ## headings, - lists, **bold**, `inline code`, ```fenced blocks```. \
             Every response MUST use at least one Markdown element.</format_requirement>"
        ))
    }

    pub(crate) fn compose_next_user_msg(&self, tool_results: Vec<ToolResult>) -> Message {
        let history = self.summaries.render_history_block();
        let mut parts = Vec::new();
        if !history.is_empty() {
            parts.push(history);
        }
        parts.push("<tool_results>".into());
        let mut has_error = false;
        for r in &tool_results {
            if r.is_error {
                has_error = true;
                parts.push(format!("[{}] TOOL_ERROR: {}", r.call_id, r.content));
            } else {
                parts.push(format!("[{}] {}", r.call_id, r.content));
            }
        }
        parts.push("</tool_results>".into());
        if has_error {
            let alternatives = self.registry.names().join(", ");
            parts.push(format!(
                "<retry_directive>Tool call failed. IMMEDIATELY call a different tool to \
                 accomplish the same goal — do NOT write explanation text. \
                 Available: {alternatives}</retry_directive>"
            ));
        }
        let mut msg = Message::user(parts.join("\n"));
        msg.tool_results = tool_results;
        msg
    }
}
