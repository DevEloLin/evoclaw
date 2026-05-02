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
use crate::summary::SummaryParser;
use chrono::Utc;
use evo_policy::{estimate_usd, BudgetCheck, CostEngine, CostEvent, Redactor};
use evo_providers::{
    ChatRequest, Message, Provider, ProviderError, StreamEvent, ToolCall, ToolFingerprint,
    ToolPayload, ToolResult,
};
use evo_tools::{ToolContext, ToolError, ToolRegistry};
use futures::StreamExt;
use std::path::PathBuf;
use std::sync::Arc;

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
    pub final_text: String,
    pub completed: bool,
}

#[derive(Debug, Clone)]
pub struct RuntimeConfig {
    pub max_turns: u64,
    pub max_tokens: u32,
    pub temperature: f32,
    pub model: String,
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            max_turns: 25,
            max_tokens: 1024,
            temperature: 0.2,
            model: "gpt-4o-mini".into(),
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
        }
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

    fn scrub(&self, text: &str) -> String {
        if let Some(r) = &self.redactor {
            let (out, _hits) = r.scrub(text);
            return out;
        }
        text.to_string()
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
        // PRD §13.4 — first thing we do with user_input is scrub it.
        // Anything past this point (history, JSONL, memory, distillation) sees
        // only placeholders, never the raw secret.
        let user_input_safe = self.scrub(user_input);
        let task_id = format!("task-{}", Utc::now().format("%Y%m%dT%H%M%S%.3f"));
        self.session
            .append(&SessionRecord::Task(TaskRecord {
                task_id: task_id.clone(),
                user_input: user_input_safe.clone(),
                source: "cli".into(),
                model: self.config.model.clone(),
                started_at: Utc::now(),
            }))
            .await?;

        let system_msg = Message::system(build_system_prompt(&self.prompt_ctx));
        let mut history: Vec<Message> = vec![system_msg];
        let mut next_user_payload = self.compose_initial_user_msg(&user_input_safe);
        let mut completed = false;
        let mut final_text = String::new();
        let mut turn = 0u64;

        while turn < self.config.max_turns {
            history.push(next_user_payload.clone());

            // PRD §42.5 — periodic tag-level compression of older history.
            compress_if_due(&mut history, turn, self.compression_cfg);

            // PRD §35 — pre-flight budget check.
            if let Some(cost) = &self.cost {
                match cost.check_for_task(&task_id).await {
                    Ok(BudgetCheck::HardStop(level)) => {
                        return Err(RuntimeError::Budget(format!("hard stop: {level:?}")));
                    }
                    Ok(_) | Err(_) => { /* SoftWarn / IO error: continue */ }
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

            let mut stream = self.provider.stream(req).await?;
            let mut assistant_text = String::new();
            let mut tool_calls: Vec<ToolCall> = Vec::new();
            let mut usage = None;

            while let Some(event) = stream.next().await {
                match event? {
                    StreamEvent::Delta(t) => assistant_text.push_str(&t),
                    StreamEvent::ToolCallStart(tc) => tool_calls.push(tc),
                    StreamEvent::ToolCallFinish => {}
                    StreamEvent::Usage(u) => usage = Some(u),
                    StreamEvent::Done => break,
                }
            }

            // Scrub assistant text in case the model echoed a registered secret
            // (it shouldn't, but treat the boundary as untrusted).
            let assistant_text_safe = self.scrub(&assistant_text);
            let safe_calls: Vec<ToolCall> = tool_calls
                .iter()
                .map(|c| ToolCall {
                    id: c.id.clone(),
                    name: c.name.clone(),
                    arguments: self.scrub_value(&c.arguments),
                })
                .collect();
            history.push(Message {
                role: evo_providers::Role::Assistant,
                content: assistant_text_safe.clone(),
                tool_calls: safe_calls,
                tool_results: Vec::new(),
                cache_control: evo_providers::CacheKind::None,
            });

            let summary = self.summaries.ingest(&assistant_text_safe);

            let mut recorded_calls = Vec::with_capacity(tool_calls.len());
            let mut tool_results: Vec<ToolResult> = Vec::with_capacity(tool_calls.len());
            for call in &tool_calls {
                let safe_args = self.scrub_value(&call.arguments);
                match self
                    .registry
                    .invoke(&self.tool_ctx, &call.name, call.arguments.clone())
                    .await
                {
                    Ok(out) => {
                        let safe_out = self.scrub(&out);
                        recorded_calls.push(RecordedToolCall {
                            name: call.name.clone(),
                            args: safe_args.clone(),
                            result_truncated: safe_out.clone(),
                            is_error: false,
                        });
                        tool_results.push(ToolResult {
                            call_id: call.id.clone(),
                            content: safe_out,
                            is_error: false,
                        });
                    }
                    Err(e) => {
                        let err = self.scrub(&e.to_string());
                        recorded_calls.push(RecordedToolCall {
                            name: call.name.clone(),
                            args: safe_args,
                            result_truncated: err.clone(),
                            is_error: true,
                        });
                        tool_results.push(ToolResult {
                            call_id: call.id.clone(),
                            content: err,
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

        // Phase 2 — reflection round before terminal state record. Pass the
        // already-scrubbed user_input so reflection cannot leak secrets even
        // by accident.
        let reflection = if completed && (self.memory.is_some() || self.skills_dir.is_some()) {
            self.reflection_round(&task_id, &final_text, &user_input_safe)
                .await
        } else {
            None
        };

        let terminal = match (completed, reflection.as_ref()) {
            (true, Some(r)) if r.success => TaskState::Completed,
            (true, _) => TaskState::Completed,
            (false, _) => TaskState::Failed,
        };

        self.session
            .append(&SessionRecord::End(EndRecord {
                state: format!("{terminal:?}").to_uppercase(),
                finished_at: Utc::now(),
            }))
            .await?;

        if !completed {
            return Err(RuntimeError::MaxTurns(self.config.max_turns));
        }
        Ok(RunOutcome {
            task_id,
            turns: turn + 1,
            final_text,
            completed,
        })
    }

    /// PRD §11 — Reflection + Distillation closeout. Best-effort: any IO or
    /// model error is recorded but not propagated, so the task still succeeds.
    async fn reflection_round(
        &mut self,
        task_id: &str,
        final_text: &str,
        user_input: &str,
    ) -> Option<ReflectionRecord> {
        // Build a reflection-only ChatRequest (no tools).
        let prompt = build_reflection_prompt(&ReflectionCtx {
            task_id: task_id.into(),
            final_result_truncated: head_tail(final_text, 4000),
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
            }
            Err(_) => return None,
        }

        let refl = match parse_reflection(&text) {
            Ok(r) => r,
            Err(_) => return None,
        };

        // Memory L3 write (PRD §33).
        if let Some(mem) = self.memory.clone() {
            let body = format!(
                "task={task_id}\nuser_input={user_input}\nsuccess={}\nsummary={}\ngoal={}\nfailures={}",
                refl.success, refl.summary, refl.user_real_goal, refl.failure_patterns.join("; ")
            );
            let mut record =
                MemoryRecord::new(MemoryLayer::L3, body, "reflection", refl.confidence);
            record.tags = vec![
                "reflection".into(),
                if refl.success { "success" } else { "failure" }.into(),
            ];
            let _ = mem.write(record).await;
        }

        // Distillation → Skill DRAFT (PRD §11.3).
        if let Some(dir) = self.skills_dir.clone() {
            if !matches!(refl.skill_update_decision, SkillUpdateDecision::None) {
                let skill = self
                    .distil_skill(task_id, &refl, user_input, final_text)
                    .await;
                if let Some(sk) = skill {
                    let _ = sk.save_yaml(&dir).await;
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
        let mut text = String::new();
        if let Ok(mut s) = self.provider.stream(req).await {
            while let Some(ev) = s.next().await {
                match ev {
                    Ok(StreamEvent::Delta(t)) => text.push_str(&t),
                    Ok(StreamEvent::Done) => break,
                    Ok(_) => {}
                    Err(_) => {
                        return Some(skill_from_reflection_quick(reflection, task_id, user_input))
                    }
                }
            }
        } else {
            return Some(skill_from_reflection_quick(reflection, task_id, user_input));
        }
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
    if s.len() <= max + 8 {
        return s.to_string();
    }
    let half = max / 2;
    let head_end = floor_char_boundary(s, half);
    let tail_start = ceil_char_boundary(s, s.len().saturating_sub(half));
    format!("{} ... {}", &s[..head_end], &s[tail_start..])
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
