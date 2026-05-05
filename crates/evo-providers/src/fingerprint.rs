//! Tool-schema fingerprint: tracks the hash of tool schemas for future use
//! (e.g. Anthropic prompt-cache headers). Tool schemas are always sent in
//! full on every turn — omitting them via a "reuse" shortcut caused providers
//! to report "no tools available" after turn 0, silently breaking multi-turn
//! agentic loops.

use crate::ToolSpec;
use sha2::{Digest, Sha256};

/// What to put in the `tools` slot of an outgoing request.
#[derive(Debug, Clone)]
pub enum ToolPayload {
    Full(Vec<ToolSpec>),
    /// Kept for API compatibility; never produced by `payload_for_turn`.
    Reuse(String),
}

impl ToolPayload {
    pub fn is_reuse(&self) -> bool {
        matches!(self, ToolPayload::Reuse(_))
    }
}

#[derive(Debug, Clone, Default)]
pub struct ToolFingerprint {
    hash: Option<String>,
}

impl ToolFingerprint {
    /// Returns `ToolPayload::Full` on every turn, ensuring the model always
    /// has access to tool schemas regardless of turn number.
    pub fn payload_for_turn(&mut self, _turn: u64, tools: Vec<ToolSpec>) -> ToolPayload {
        let canonical = canonicalise(&tools);
        self.hash = Some(digest(&canonical));
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
    fn all_turns_send_full() {
        // Reuse is disabled — every turn must include full tool schemas so the
        // model can make tool calls on any turn of a multi-turn loop.
        let mut f = ToolFingerprint::default();
        let _ = f.payload_for_turn(0, vec![t("a"), t("b")]);
        assert!(matches!(
            f.payload_for_turn(1, vec![t("a"), t("b")]),
            ToolPayload::Full(_)
        ));
        assert!(matches!(
            f.payload_for_turn(9, vec![t("a"), t("b")]),
            ToolPayload::Full(_)
        ));
    }

    #[test]
    fn changed_tools_sends_full() {
        let mut f = ToolFingerprint::default();
        let _ = f.payload_for_turn(0, vec![t("a")]);
        assert!(matches!(
            f.payload_for_turn(1, vec![t("a"), t("b")]),
            ToolPayload::Full(_)
        ));
    }

    #[test]
    fn tool_order_does_not_affect_behavior() {
        // Both orderings should produce Full (all turns do now).
        let mut f = ToolFingerprint::default();
        let _ = f.payload_for_turn(0, vec![t("a"), t("b")]);
        assert!(matches!(
            f.payload_for_turn(1, vec![t("b"), t("a")]),
            ToolPayload::Full(_)
        ));
    }
}
