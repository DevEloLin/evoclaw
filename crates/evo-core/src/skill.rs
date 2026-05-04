//! Skill entity, YAML serde, FSM, EWMA score: PRD §17.2 + §32 + PROMPTS §5.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum SkillState {
    Draft,
    Candidate,
    Active,
    Degraded,
    Deprecated,
    Archived,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum SkillKind {
    Sop,
    Diagnostic,
    Browser,
    Coding,
    Ops,
    Api,
    Workflow,
    ToolWrapper,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillStep {
    pub tool: String,
    pub args_template: serde_json::Value,
    #[serde(default)]
    pub check: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SkillStats {
    pub success: u32,
    pub failure: u32,
    pub consecutive_failures: u32,
    pub last_used: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FailurePattern {
    pub pattern: String,
    pub fix: String,
    #[serde(default)]
    pub count: u32,
    #[serde(default)]
    pub last_seen: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Skill {
    pub id: String,
    pub name: String,
    #[serde(rename = "type")]
    pub kind: SkillKind,
    pub version: u32,
    pub description: String,
    #[serde(default)]
    pub triggers: Vec<String>,
    #[serde(default)]
    pub environment: serde_json::Value,
    #[serde(default)]
    pub preconditions: Vec<String>,
    #[serde(default)]
    pub steps: Vec<SkillStep>,
    #[serde(default)]
    pub verification: String,
    #[serde(default)]
    pub success_criteria: Vec<String>,
    #[serde(default)]
    pub failure_patterns: Vec<FailurePattern>,
    pub score: f32,
    pub state: SkillState,
    #[serde(default)]
    pub parent: Option<String>,
    pub created_from_task: String,
    pub updated_at: DateTime<Utc>,
    #[serde(default)]
    pub stats: SkillStats,
    #[serde(default)]
    pub changelog: Vec<String>,
}

impl Skill {
    pub fn new_draft(
        id: impl Into<String>,
        name: impl Into<String>,
        kind: SkillKind,
        task_id: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            kind,
            version: 1,
            description: String::new(),
            triggers: Vec::new(),
            environment: serde_json::Value::Null,
            preconditions: Vec::new(),
            steps: Vec::new(),
            verification: String::new(),
            success_criteria: Vec::new(),
            failure_patterns: Vec::new(),
            score: 0.5,
            state: SkillState::Draft,
            parent: None,
            created_from_task: task_id.into(),
            updated_at: Utc::now(),
            stats: SkillStats::default(),
            changelog: vec!["v1: created from task".into()],
        }
    }

    pub fn record_success(&mut self) {
        self.score = 0.9 * self.score + 0.1;
        self.stats.success += 1;
        self.stats.consecutive_failures = 0;
        self.stats.last_used = Some(Utc::now());
        self.maybe_transition_after_score_change();
    }

    pub fn record_failure(&mut self) {
        self.score *= 0.9;
        self.stats.failure += 1;
        self.stats.consecutive_failures += 1;
        self.stats.last_used = Some(Utc::now());
        self.maybe_transition_after_score_change();
    }

    pub fn record_thumbs_up(&mut self) {
        self.score = (self.score + 0.1).min(1.0);
        self.maybe_transition_after_score_change();
    }

    pub fn record_thumbs_down(&mut self) {
        self.score *= 0.7;
        self.maybe_transition_after_score_change();
    }

    pub fn record_correction(&mut self) {
        self.score *= 0.5;
        self.maybe_transition_after_score_change();
    }

    pub fn record_sandbox_pass(&mut self) {
        self.score = self.score.max(0.6);
        if self.state == SkillState::Draft {
            self.transition(SkillState::Candidate, "sandbox pass");
        }
    }

    pub fn record_sandbox_fail(&mut self) {
        self.score *= 0.5;
        self.maybe_transition_after_score_change();
    }

    fn maybe_transition_after_score_change(&mut self) {
        let s = self.score;
        match self.state {
            SkillState::Candidate if self.stats.success >= 3 && s >= 0.7 => {
                self.transition(SkillState::Active, "promoted")
            }
            SkillState::Candidate | SkillState::Active if (0.3..0.7).contains(&s) => {
                self.transition(SkillState::Degraded, "score in [0.3,0.7)")
            }
            SkillState::Degraded
                if s >= 0.7 && self.stats.consecutive_failures == 0 && self.stats.success >= 3 =>
            {
                self.transition(SkillState::Active, "recovered")
            }
            SkillState::Degraded if s < 0.3 || self.stats.consecutive_failures >= 5 => {
                self.transition(SkillState::Deprecated, "deprecated")
            }
            SkillState::Draft if s < 0.1 => self.transition(SkillState::Deprecated, "draft died"),
            _ => {}
        }
    }

    fn transition(&mut self, new_state: SkillState, note: &str) {
        if self.state == new_state {
            return;
        }
        self.changelog.push(format!(
            "v{} {:?} → {:?} ({note})",
            self.version, self.state, new_state
        ));
        self.state = new_state;
        self.updated_at = Utc::now();
    }

    pub fn auto_enable(&self) -> bool {
        matches!(self.state, SkillState::Active)
    }
    pub fn can_be_searched(&self) -> bool {
        !matches!(self.state, SkillState::Archived)
    }

    pub async fn save_yaml(&self, dir: impl AsRef<Path>) -> std::io::Result<std::path::PathBuf> {
        let dir = dir.as_ref();
        tokio::fs::create_dir_all(dir).await?;
        let path = dir.join(format!("{}.yaml", self.id));
        // Atomic write: render to a unique tmp sibling, then rename. POSIX
        // `rename` is atomic on the same filesystem, so concurrent reflections
        // for the same skill ID can no longer truncate each other's output.
        let tmp = dir.join(format!(
            "{}.yaml.tmp.{}.{}",
            self.id,
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap_or(0)
        ));
        let yaml_str = render_yaml(self)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        tokio::fs::write(&tmp, yaml_str).await?;
        tokio::fs::rename(&tmp, &path).await?;
        Ok(path)
    }

    pub async fn load_yaml(path: impl AsRef<Path>) -> std::io::Result<Self> {
        let text = tokio::fs::read_to_string(path).await?;
        parse_yaml(&text).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
    }
}

/// JSON is a subset of YAML so we serialise as JSON for now (Phase 2 keeps deps small).
pub fn render_yaml(skill: &Skill) -> Result<String, serde_json::Error> {
    Ok(serde_json::to_string_pretty(skill)? + "\n")
}

pub fn parse_yaml(text: &str) -> Result<Skill, String> {
    serde_json::from_str(text).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s() -> Skill {
        Skill::new_draft("skill-1", "test skill", SkillKind::Sop, "task-1")
    }

    #[test]
    fn new_skill_starts_draft_with_score_0_5() {
        let sk = s();
        assert!(matches!(sk.state, SkillState::Draft));
        assert!((sk.score - 0.5).abs() < 1e-6);
    }

    #[test]
    fn sandbox_pass_promotes_draft_to_candidate() {
        let mut sk = s();
        sk.record_sandbox_pass();
        assert!(matches!(sk.state, SkillState::Candidate));
        assert!(sk.score >= 0.6);
    }

    #[test]
    fn three_successes_promote_candidate_to_active() {
        let mut sk = s();
        sk.record_sandbox_pass();
        sk.record_success();
        sk.record_success();
        sk.record_success();
        assert!(
            matches!(sk.state, SkillState::Active),
            "got {:?} score={}",
            sk.state,
            sk.score
        );
    }

    #[test]
    fn correction_drops_score_to_half() {
        let mut sk = s();
        sk.record_correction();
        assert!((sk.score - 0.25).abs() < 1e-6);
    }

    #[test]
    fn five_failures_deprecate_through_degraded() {
        let mut sk = s();
        sk.state = SkillState::Active;
        sk.score = 0.8;
        for _ in 0..5 {
            sk.record_failure();
        }
        assert!(matches!(
            sk.state,
            SkillState::Deprecated | SkillState::Degraded
        ));
        assert!(sk.score < 0.7);
    }

    #[test]
    fn auto_enable_only_when_active() {
        let mut sk = s();
        assert!(!sk.auto_enable());
        sk.state = SkillState::Active;
        assert!(sk.auto_enable());
    }

    #[tokio::test]
    async fn yaml_round_trip() {
        let mut p = std::env::temp_dir();
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        p.push(format!("evo-skill-{stamp}"));
        let sk = s();
        let path = sk.save_yaml(&p).await.unwrap();
        let back = Skill::load_yaml(&path).await.unwrap();
        assert_eq!(back.id, sk.id);
        assert!(matches!(back.state, SkillState::Draft));
    }

    #[test]
    fn changelog_entry_per_transition() {
        let mut sk = s();
        sk.record_sandbox_pass();
        let n = sk.changelog.len();
        sk.record_success();
        sk.record_success();
        sk.record_success();
        assert!(sk.changelog.len() > n, "expected changelog entry on Active");
    }
}
