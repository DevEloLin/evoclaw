//! Reflection and distillation closeout: `reflection_round()` + `distil_skill()`.

use super::util::head_tail;
use super::{ConversationRuntime, REFLECTION_TIMEOUT};
use crate::distillation::{
    build_distillation_prompt, parse_distilled_skill, skill_from_reflection_quick, DistillCtx,
};
use crate::memory::{MemoryLayer, MemoryRecord};
use crate::prompt::build_system_prompt;
use crate::reflection::{
    build_reflection_prompt, parse_reflection, ReflectionCtx, ReflectionRecord, SkillUpdateDecision,
};
use crate::skill::Skill;
use crate::skill_tree::SkillTree;
use chrono::Utc;
use evo_providers::{ChatRequest, Message, Provider, StreamEvent, ToolPayload};
use futures::StreamExt;

impl<P: Provider + ?Sized> ConversationRuntime<P> {
    /// PRD §11 — Reflection + Distillation closeout. Best-effort: any IO or
    /// model error is recorded but not propagated, so the task still succeeds.
    pub(crate) async fn reflection_round(
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

    pub(crate) async fn distil_skill(
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
}
