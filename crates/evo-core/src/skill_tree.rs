//! Skill Tree Index — PRD §11.5 + §43.

use crate::skill::{Skill, SkillKind, SkillState};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillTreeNode {
    pub id: String,
    pub name: String,
    #[serde(rename = "type")]
    pub kind: SkillKind,
    pub state: SkillState,
    pub score: f32,
    #[serde(default)]
    pub triggers: Vec<String>,
    #[serde(default)]
    pub parent: Option<String>,
    pub updated_at: DateTime<Utc>,
}

impl From<&Skill> for SkillTreeNode {
    fn from(s: &Skill) -> Self {
        Self {
            id: s.id.clone(), name: s.name.clone(), kind: s.kind,
            state: s.state, score: s.score,
            triggers: s.triggers.clone(), parent: s.parent.clone(),
            updated_at: s.updated_at,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillTree {
    pub nodes: Vec<SkillTreeNode>,
    pub updated_at: DateTime<Utc>,
}

impl SkillTree {
    pub fn empty() -> Self { Self { nodes: Vec::new(), updated_at: Utc::now() } }

    pub async fn rebuild_from_dir(skills_dir: impl AsRef<Path>) -> std::io::Result<Self> {
        let dir = skills_dir.as_ref();
        let mut nodes = Vec::new();
        if !dir.exists() { return Ok(SkillTree::empty()); }
        let mut entries = tokio::fs::read_dir(dir).await?;
        while let Some(entry) = entries.next_entry().await? {
            let p = entry.path();
            if p.extension().and_then(|s| s.to_str()) != Some("yaml") { continue; }
            if let Ok(sk) = Skill::load_yaml(&p).await {
                nodes.push(SkillTreeNode::from(&sk));
            }
        }
        nodes.sort_by(|a, b| {
            order_state(b.state).cmp(&order_state(a.state))
                .then_with(|| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal))
                .then_with(|| a.name.cmp(&b.name))
        });
        Ok(SkillTree { nodes, updated_at: Utc::now() })
    }

    pub async fn save(&self, path: impl AsRef<Path>) -> std::io::Result<()> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let json = serde_json::to_string_pretty(self)?;
        tokio::fs::write(path, json).await
    }

    pub async fn load(path: impl AsRef<Path>) -> std::io::Result<Self> {
        let text = tokio::fs::read_to_string(path).await?;
        serde_json::from_str(&text).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
    }

    pub fn search(&self, query: &str, limit: usize) -> Vec<&SkillTreeNode> {
        let q = query.to_lowercase();
        let mut hits: Vec<&SkillTreeNode> = self.nodes.iter()
            .filter(|n| !matches!(n.state, SkillState::Archived))
            .filter(|n| n.name.to_lowercase().contains(&q)
                || n.id.to_lowercase().contains(&q)
                || n.triggers.iter().any(|t| t.to_lowercase().contains(&q)))
            .collect();
        hits.sort_by(|a, b| {
            order_state(b.state).cmp(&order_state(a.state))
                .then_with(|| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal))
        });
        hits.truncate(limit);
        hits
    }

    pub fn active(&self) -> Vec<&SkillTreeNode> {
        self.nodes.iter().filter(|n| matches!(n.state, SkillState::Active)).collect()
    }

    pub fn render_tree(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!("== skill tree ({} nodes, {} active) ==\n",
            self.nodes.len(), self.active().len()));
        let mut by_kind: std::collections::BTreeMap<String, Vec<&SkillTreeNode>> = Default::default();
        for n in &self.nodes {
            by_kind.entry(format!("{:?}", n.kind)).or_default().push(n);
        }
        for (kind, items) in by_kind {
            out.push_str(&format!("\n[{kind}]\n"));
            for n in items {
                out.push_str(&format!("  {:<24} {:<10} score={:.2} {} (triggers: {})\n",
                    n.id, format!("{:?}", n.state).to_uppercase(),
                    n.score, n.name, n.triggers.join(", ")));
            }
        }
        out
    }

    pub fn default_index_path(skills_dir: impl Into<PathBuf>) -> PathBuf {
        skills_dir.into().join("index.json")
    }
}

fn order_state(s: SkillState) -> u8 {
    match s {
        SkillState::Active => 5,
        SkillState::Candidate => 4,
        SkillState::Degraded => 3,
        SkillState::Draft => 2,
        SkillState::Deprecated => 1,
        SkillState::Archived => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unique_dir(name: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        p.push(format!("evo-tree-{name}-{stamp}"));
        p
    }

    fn mk_skill(id: &str, name: &str, score: f32, state: SkillState, triggers: Vec<&str>) -> Skill {
        let mut s = Skill::new_draft(id, name, SkillKind::Sop, "task-1");
        s.score = score;
        s.state = state;
        s.triggers = triggers.into_iter().map(String::from).collect();
        s
    }

    #[tokio::test]
    async fn rebuild_from_empty_dir_yields_empty_tree() {
        let t = SkillTree::rebuild_from_dir(unique_dir("empty")).await.unwrap();
        assert!(t.nodes.is_empty());
    }

    #[tokio::test]
    async fn rebuild_picks_up_yaml_files() {
        let dir = unique_dir("pickup");
        let s = mk_skill("s1", "diagnose ssh", 0.85, SkillState::Active, vec!["ssh", "diagnose"]);
        s.save_yaml(&dir).await.unwrap();
        let t = SkillTree::rebuild_from_dir(&dir).await.unwrap();
        assert_eq!(t.nodes.len(), 1);
        assert_eq!(t.nodes[0].id, "s1");
    }

    #[tokio::test]
    async fn search_matches_trigger() {
        let dir = unique_dir("search");
        for sk in [
            mk_skill("ssh-diag", "diagnose ssh", 0.85, SkillState::Active, vec!["ssh"]),
            mk_skill("docker-diag", "docker logs", 0.7, SkillState::Active, vec!["docker"]),
        ] {
            sk.save_yaml(&dir).await.unwrap();
        }
        let t = SkillTree::rebuild_from_dir(&dir).await.unwrap();
        let hits = t.search("ssh", 10);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, "ssh-diag");
    }

    #[tokio::test]
    async fn save_load_round_trip() {
        let dir = unique_dir("rtrip");
        let sk = mk_skill("s1", "x", 0.5, SkillState::Draft, vec![]);
        sk.save_yaml(&dir).await.unwrap();
        let t = SkillTree::rebuild_from_dir(&dir).await.unwrap();
        let p = SkillTree::default_index_path(&dir);
        t.save(&p).await.unwrap();
        let back = SkillTree::load(&p).await.unwrap();
        assert_eq!(back.nodes.len(), 1);
    }

    #[test]
    fn ordering_active_above_draft() {
        assert!(order_state(SkillState::Active) > order_state(SkillState::Draft));
    }
}
