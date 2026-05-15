//! Main agent loop: `ConversationRuntime::run()`.

use super::util::{emit_provider_debug, SessionEndGuard};
use super::{
    ConversationRuntime, RunOutcome, RunUsage, RuntimeError, TaskState, BUDGET_ERR_HARD_STOP,
    TURN_TIMEOUT,
};
use crate::compression::compress_if_due;
use crate::prompt::build_system_prompt;
use crate::session::{
    EndRecord, RecordedToolCall, RecordedUsage, SessionRecord, TaskRecord, TurnRecord,
};
use crate::skill_tree::SkillTree;
use chrono::Utc;
use evo_policy::{estimate_usd, is_fully_redacted, BudgetCheck, CostEvent, RedactionMode};
use evo_providers::{
    ChatRequest, Message, Provider, ProviderError, StreamEvent, ToolCall, ToolResult,
};
use futures::StreamExt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

impl<P: Provider + ?Sized> ConversationRuntime<P> {
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
                skills_used: Vec::new(), // Reserved for future reflection-stage backfill.
                                          // Today's audit trail relies on RecordedToolCall
                                          // (grep for `"name":"load_skill"` in the JSONL).
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

        // Conversation history is now persisted on `self.history` across
        // `run()` invocations so a REPL session remembers prior turns.
        //
        // ACP providers manage their own history upstream — keeping a
        // parallel copy here would double-bill tokens, so we clear on every
        // run for ACP. For native providers we accumulate.
        // Robust ACP detection: trim whitespace, lowercase, then check prefix.
        // Catches "ACP:claude", " acp:codex", "Acp:Cursor" etc. which would
        // otherwise slip through and double-bill tokens against an upstream
        // agent that maintains its own history.
        let is_acp = self
            .config
            .provider_id
            .as_deref()
            .map(|p| p.trim().to_ascii_lowercase())
            .is_some_and(|p| p.starts_with("acp:"));
        if is_acp {
            self.history.clear();
        }
        if self.history.is_empty() {
            self.history
                .push(Message::system(build_system_prompt(&self.prompt_ctx)));
        }
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
            self.history.push(next_user_payload.clone());

            // PRD §42.5 — periodic tag-level compression of older history.
            compress_if_due(&mut self.history, turn, self.compression_cfg);

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
                messages: self.history.clone(),
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
            self.history.push(Message {
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
}
