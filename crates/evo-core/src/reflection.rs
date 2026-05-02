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
}

pub fn build_reflection_prompt(ctx: &ReflectionCtx) -> String {
    format!(
        "The task has finished. Reflect strictly in JSON. Do NOT include any prose.\n\
\n\
Schema:\n\
{{\n  \"success\": true|false,\n  \"summary\": \"<≤60 chars>\",\n  \"user_real_goal\": \"<≤120 chars>\",\n  \"reusable_steps\": [\"...\"],\n  \"mistakes\": [\"...\"],\n  \"failure_patterns\": [\"...\"],\n  \"verification\": \"<≤80 chars>\",\n  \"skill_update_decision\": \"create|update|merge|deprecate|none\",\n  \"confidence\": 0.0..1.0,\n  \"safety_notes\": [\"...\"],\n  \"next_recommendation\": \"<≤80 chars>\"\n}}\n\
\n\
Rules:\n\
- If success=false, still fill all fields; failure_patterns must be non-empty.\n\
- skill_update_decision=none only if the task is one-off and unlikely to recur.\n\
- Do NOT leak any secret, cookie, key, or path under ~/.evoclaw/secrets.\n\
\n\
Task ID: {task}\n\
Final result: {res}\n",
        task = ctx.task_id,
        res = ctx.final_result_truncated
    )
}

pub fn parse_reflection(model_text: &str) -> Result<ReflectionRecord, String> {
    let trimmed = strip_code_fence(model_text);
    serde_json::from_str(trimmed).map_err(|e| e.to_string())
}

fn strip_code_fence(s: &str) -> &str {
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
