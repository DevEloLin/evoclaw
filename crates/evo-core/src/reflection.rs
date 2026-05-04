//! Reflection record + prompt builder + parser. PROMPTS §4 + §5.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillUpdateDecision {
    Create,
    Update,
    Merge,
    Deprecate,
    None,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReflectionRecord {
    pub success: bool,
    pub summary: String,
    pub user_real_goal: String,
    #[serde(default)]
    pub reusable_steps: Vec<String>,
    #[serde(default)]
    pub mistakes: Vec<String>,
    #[serde(default)]
    pub failure_patterns: Vec<String>,
    #[serde(default)]
    pub verification: String,
    pub skill_update_decision: SkillUpdateDecision,
    /// For update/merge/deprecate: the ID of the existing skill to act on.
    /// Omit (or set null) when decision is create or none.
    #[serde(default)]
    pub target_skill_id: Option<String>,
    pub confidence: f32,
    #[serde(default)]
    pub safety_notes: Vec<String>,
    #[serde(default)]
    pub next_recommendation: String,
}

#[derive(Debug, Clone)]
pub struct ReflectionCtx {
    pub task_id: String,
    pub final_result_truncated: String,
    /// Truncated tool-call trajectory so the model can identify reusable steps.
    pub trajectory_truncated: String,
    /// IDs of currently active skills — lets the model decide update vs. create.
    pub active_skill_ids: Vec<String>,
}

pub fn build_reflection_prompt(ctx: &ReflectionCtx) -> String {
    let active = if ctx.active_skill_ids.is_empty() {
        "(none)".to_string()
    } else {
        ctx.active_skill_ids.join(", ")
    };
    format!(
        "The task has finished. Reflect strictly in JSON. Do NOT include any prose.\n\
\n\
Schema:\n\
{{\n  \"success\": true|false,\n  \"summary\": \"<≤60 chars>\",\n  \"user_real_goal\": \"<≤120 chars>\",\n  \"reusable_steps\": [\"...\"],\n  \"mistakes\": [\"...\"],\n  \"failure_patterns\": [\"...\"],\n  \"verification\": \"<≤80 chars>\",\n  \"skill_update_decision\": \"create|update|merge|deprecate|none\",\n  \"target_skill_id\": \"<existing skill id, or null>\",\n  \"confidence\": 0.0..1.0,\n  \"safety_notes\": [\"...\"],\n  \"next_recommendation\": \"<≤80 chars>\"\n}}\n\
\n\
Rules:\n\
- If success=false, still fill all fields; failure_patterns must be non-empty.\n\
- skill_update_decision=none only if the task is one-off and unlikely to recur.\n\
- If update/merge/deprecate: set target_skill_id to the matching ID from active_skills.\n\
- Do NOT leak any secret, cookie, key, or path under ~/.evoclaw/secrets.\n\
\n\
Task ID: {task}\n\
Active skills: {active}\n\
Trajectory: {traj}\n\
Final result: {res}\n",
        task = ctx.task_id,
        active = active,
        traj = ctx.trajectory_truncated,
        res = ctx.final_result_truncated
    )
}

pub fn parse_reflection(model_text: &str) -> Result<ReflectionRecord, String> {
    let trimmed = strip_code_fence(model_text);
    serde_json::from_str(trimmed).map_err(|e| e.to_string())
}

pub(crate) fn strip_code_fence(s: &str) -> &str {
    let t = s.trim();
    if let Some(rest) = t.strip_prefix("```json") {
        return rest.trim_end_matches("```").trim();
    }
    if let Some(rest) = t.strip_prefix("```") {
        return rest.trim_end_matches("```").trim();
    }
    t
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_prompt_includes_task_id_and_schema() {
        let p = build_reflection_prompt(&ReflectionCtx {
            task_id: "task-abc".into(),
            final_result_truncated: "ok".into(),
            trajectory_truncated: String::new(),
            active_skill_ids: Vec::new(),
        });
        assert!(p.contains("Task ID: task-abc"));
        assert!(p.contains("skill_update_decision"));
    }

    #[test]
    fn parse_valid_reflection() {
        let raw = r#"{
            "success": true,
            "summary": "ran cargo build",
            "user_real_goal": "verify build green",
            "reusable_steps": ["cargo build --workspace"],
            "mistakes": [],
            "failure_patterns": [],
            "verification": "exit code 0",
            "skill_update_decision": "create",
            "confidence": 0.8,
            "safety_notes": [],
            "next_recommendation": "add CI rule"
        }"#;
        let r = parse_reflection(raw).unwrap();
        assert!(r.success);
        assert!(matches!(
            r.skill_update_decision,
            SkillUpdateDecision::Create
        ));
    }

    #[test]
    fn parse_strips_code_fence() {
        let raw = "```json\n{\"success\":false,\"summary\":\"x\",\"user_real_goal\":\"y\",\"skill_update_decision\":\"none\",\"confidence\":0.4}\n```";
        let r = parse_reflection(raw).unwrap();
        assert!(!r.success);
    }

    #[test]
    fn parse_rejects_missing_required_fields() {
        assert!(parse_reflection("{\"success\": true}").is_err());
    }
}
