//! Distillation — turn a reflection record + trajectory into a Skill DRAFT.
//! Implements PROMPTS §5.

use crate::reflection::ReflectionRecord;
use crate::skill::{Skill, SkillKind};

#[derive(Debug, Clone)]
pub struct DistillCtx {
    pub task_id: String,
    pub reflection_json: String,
    pub trajectory_truncated: String,
}

pub fn build_distillation_prompt(ctx: &DistillCtx) -> String {
    format!(
        "Distill the following task trajectory into a reusable Skill JSON.\n\
\n\
Mandatory fields:\n\
  id: <auto, kebab-case, ≤32 chars>\n\
  name: <≤40 chars, imperative>\n\
  type: \"Sop\"|\"Diagnostic\"|\"Browser\"|\"Coding\"|\"Ops\"|\"Api\"|\"Workflow\"|\"ToolWrapper\"\n\
  description: <≤120 chars>\n\
  triggers: [keyword1, keyword2, ...]   (≤6 entries)\n\
  preconditions: [...]                  (≤5 entries)\n\
  steps: [{{tool, args_template, check}}]\n\
  verification: <≤120 chars>\n\
  success_criteria: [...]\n\
  failure_patterns: [{{pattern, fix}}]\n\
\n\
Forbidden:\n\
- Specific paths under /home, /Users, ~/.evoclaw/secrets\n\
- Any secret/key/cookie/token literal\n\
- Verbose stdout dumps; cite tool only\n\
\n\
Trajectory: {traj}\n\
Reflection: {refl}\n\
\n\
Output ONLY a JSON object with the fields above (no fences, no commentary).\n\
The runtime will fill in version=1, score=0.5, state=DRAFT, created_from_task, updated_at.\n",
        traj = ctx.trajectory_truncated,
        refl = ctx.reflection_json,
    )
}

pub fn parse_distilled_skill(model_text: &str, task_id: &str) -> Result<Skill, String> {
    let trimmed = strip_code_fence(model_text);
    let mut json: serde_json::Value = serde_json::from_str(trimmed).map_err(|e| e.to_string())?;
    let id = json
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or("skill-untitled")
        .to_string();
    let name = json
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or(&id)
        .to_string();
    let kind: SkillKind = json
        .get("type")
        .and_then(|v| serde_json::from_value::<SkillKind>(v.clone()).ok())
        .unwrap_or(SkillKind::Sop);
    let mut sk = Skill::new_draft(&id, name, kind, task_id);
    if let Some(desc) = json.get("description").and_then(|v| v.as_str()) {
        sk.description = desc.to_string();
    }
    if let Some(arr) = json.get("triggers").and_then(|v| v.as_array()) {
        sk.triggers = arr
            .iter()
            .filter_map(|v| v.as_str().map(String::from))
            .take(6)
            .collect();
    }
    if let Some(arr) = json.get("preconditions").and_then(|v| v.as_array()) {
        sk.preconditions = arr
            .iter()
            .filter_map(|v| v.as_str().map(String::from))
            .take(5)
            .collect();
    }
    if let Some(steps) = json.get_mut("steps").map(std::mem::take) {
        if let Ok(parsed) = serde_json::from_value(steps) {
            sk.steps = parsed;
        }
    }
    if let Some(v) = json.get("verification").and_then(|v| v.as_str()) {
        sk.verification = v.to_string();
    }
    if let Some(arr) = json.get("success_criteria").and_then(|v| v.as_array()) {
        sk.success_criteria = arr
            .iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect();
    }
    if let Some(arr) = json.get("failure_patterns").and_then(|v| v.as_array()) {
        sk.failure_patterns = arr
            .iter()
            .filter_map(|v| serde_json::from_value(v.clone()).ok())
            .collect();
    }
    Ok(sk)
}

/// Cost-saving alternative: derive a Skill DRAFT directly from the reflection
/// without a second model call. Used when budget hits SoftWarn.
pub fn skill_from_reflection_quick(
    reflection: &ReflectionRecord,
    task_id: &str,
    user_input: &str,
) -> Skill {
    let id = format!(
        "skill-{}",
        task_id
            .trim_start_matches("task-")
            .chars()
            .take(12)
            .collect::<String>()
    );
    let name = reflection.summary.chars().take(40).collect::<String>();
    let mut sk = Skill::new_draft(&id, name, SkillKind::Sop, task_id);
    sk.description = reflection.user_real_goal.clone();
    sk.triggers = trigger_keywords(user_input);
    sk.verification = reflection.verification.clone();
    sk.failure_patterns = reflection
        .failure_patterns
        .iter()
        .map(|p| crate::skill::FailurePattern {
            pattern: p.clone(),
            fix: reflection.next_recommendation.clone(),
            count: 0,
            last_seen: None,
        })
        .collect();
    sk
}

fn trigger_keywords(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() >= 3 && w.len() <= 24)
        .map(|w| w.to_lowercase())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .take(6)
        .collect()
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
    use crate::reflection::SkillUpdateDecision;

    fn refl() -> ReflectionRecord {
        ReflectionRecord {
            success: true,
            summary: "diagnose ssh".into(),
            user_real_goal: "verify ssh server reachable".into(),
            reusable_steps: vec!["ssh -v ...".into()],
            mistakes: vec![],
            failure_patterns: vec!["timeout".into()],
            verification: "ssh exit 0".into(),
            skill_update_decision: SkillUpdateDecision::Create,
            confidence: 0.8,
            safety_notes: vec![],
            next_recommendation: "verify hostname".into(),
        }
    }

    #[test]
    fn quick_skill_uses_reflection_summary_and_goal() {
        let sk =
            skill_from_reflection_quick(&refl(), "task-20260502T120000.000", "diagnose ssh hang");
        assert_eq!(sk.name, "diagnose ssh");
        assert_eq!(sk.description, "verify ssh server reachable");
        assert!(sk.triggers.iter().any(|t| t == "ssh"));
        assert_eq!(sk.failure_patterns.len(), 1);
    }

    #[test]
    fn quick_skill_id_starts_with_skill_prefix() {
        let sk = skill_from_reflection_quick(&refl(), "task-abc", "x");
        assert!(sk.id.starts_with("skill-"));
        assert!(sk.id.len() <= 32);
    }

    #[test]
    fn parse_distilled_skill_from_clean_json() {
        let raw = r#"{
            "id": "ssh-diag", "name": "diagnose ssh hang",
            "type": "Diagnostic", "description": "verify ssh server reachable",
            "triggers": ["ssh", "diagnose"], "verification": "exit 0"
        }"#;
        let sk = parse_distilled_skill(raw, "task-1").unwrap();
        assert_eq!(sk.id, "ssh-diag");
        assert!(matches!(sk.kind, SkillKind::Diagnostic));
        assert_eq!(sk.triggers.len(), 2);
    }

    #[test]
    fn parse_distilled_skill_strips_fence() {
        let raw = "```json\n{\"id\":\"x\",\"name\":\"y\",\"type\":\"Sop\"}\n```";
        assert_eq!(parse_distilled_skill(raw, "task-2").unwrap().id, "x");
    }

    #[test]
    fn build_prompt_carries_traj_and_refl() {
        let p = build_distillation_prompt(&DistillCtx {
            task_id: "t-42".into(),
            reflection_json: "{}".into(),
            trajectory_truncated: "...".into(),
        });
        assert!(p.contains("Trajectory:"));
        assert!(p.contains("Reflection:"));
    }
}
