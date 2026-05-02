//! Tool-schema fingerprint: PRD §42.1 招式 1.

use crate::ToolSpec;
use sha2::{Digest, Sha256};

const DEFAULT_RESET_EVERY: u64 = 10;
const REUSE_TEMPLATE: &str =
    "### Tools: still active, protocol unchanged. Use the same tools as the prior turn.";

/// What to put in the `tools` slot of an outgoing request.
#[derive(Debug, Clone)]
pub enum ToolPayload {
    Full(Vec<ToolSpec>),
    Reuse(String),
}

impl ToolPayload {
    pub fn is_reuse(&self) -> bool {
        matches!(self, ToolPayload::Reuse(_))
    }
}

#[derive(Debug, Clone)]
pub struct ToolFingerprint {
    hash: Option<String>,
    last_full_send_turn: u64,
    reset_every: u64,
}

impl Default for ToolFingerprint {
    fn default() -> Self {
        Self {
            hash: None,
            last_full_send_turn: 0,
            reset_every: DEFAULT_RESET_EVERY,
        }
    }
}

impl ToolFingerprint {
    pub fn with_reset_every(reset_every: u64) -> Self {
        Self {
            reset_every,
            ..Self::default()
        }
    }

    pub fn payload_for_turn(&mut self, turn: u64, tools: Vec<ToolSpec>) -> ToolPayload {
        let canonical = canonicalise(&tools);
        let new_hash = digest(&canonical);
        let forced_reset = turn == 0
            || self.hash.is_none()
            || turn.saturating_sub(self.last_full_send_turn) >= self.reset_every;
        let hash_match = self.hash.as_deref() == Some(new_hash.as_str());
        if hash_match && !forced_reset {
            return ToolPayload::Reuse(REUSE_TEMPLATE.into());
        }
        self.hash = Some(new_hash);
        self.last_full_send_turn = turn;
        ToolPayload::Full(tools)
    }
}

fn canonicalise(tools: &[ToolSpec]) -> String {
    let mut sorted: Vec<&ToolSpec> = tools.iter().collect();
    sorted.sort_by(|a, b| a.name.cmp(&b.name));
    serde_json::to_string(&sorted).unwrap_or_default()
}

fn digest(s: &str) -> String {
    let mut h = Sha256::new();
    h.update(s.as_bytes());
    hex::encode(h.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn t(name: &str) -> ToolSpec {
        ToolSpec {
            name: name.into(),
            description: format!("desc {name}"),
            schema: json!({}),
        }
    }

    #[test]
    fn first_turn_sends_full() {
        let mut f = ToolFingerprint::default();
        assert!(matches!(
            f.payload_for_turn(0, vec![t("read_file")]),
            ToolPayload::Full(_)
        ));
    }

    #[test]
    fn unchanged_tools_reused_after_first() {
        let mut f = ToolFingerprint::default();
        let _ = f.payload_for_turn(0, vec![t("a"), t("b")]);
        assert!(matches!(
            f.payload_for_turn(1, vec![t("a"), t("b")]),
            ToolPayload::Reuse(_)
        ));
    }

    #[test]
    fn reset_every_n_resends_full() {
        let mut f = ToolFingerprint::with_reset_every(5);
        let _ = f.payload_for_turn(0, vec![t("a")]);
        assert!(matches!(
            f.payload_for_turn(5, vec![t("a")]),
            ToolPayload::Full(_)
        ));
    }

    #[test]
    fn changed_tools_resends_full() {
        let mut f = ToolFingerprint::default();
        let _ = f.payload_for_turn(0, vec![t("a")]);
        assert!(matches!(
            f.payload_for_turn(1, vec![t("a"), t("b")]),
            ToolPayload::Full(_)
        ));
    }

    #[test]
    fn order_does_not_matter_for_hash() {
        let mut f = ToolFingerprint::default();
        let _ = f.payload_for_turn(0, vec![t("a"), t("b")]);
        assert!(matches!(
            f.payload_for_turn(1, vec![t("b"), t("a")]),
            ToolPayload::Reuse(_)
        ));
    }
}
