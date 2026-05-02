//! `<summary>` working-memory protocol: PRD §42.4 + PROMPTS §1/§3.

use std::collections::VecDeque;

const SUMMARY_MAX_CHARS: usize = 30;
const DEFAULT_HISTORY_DEPTH: usize = 40;

pub fn extract_summary(reply: &str) -> Option<String> {
    let start = reply.find("<summary>")?;
    let after = &reply[start + "<summary>".len()..];
    let end = after.find("</summary>")?;
    let raw = after[..end].trim();
    if raw.is_empty() { return None; }
    let mut chars: String = raw.chars().take(SUMMARY_MAX_CHARS).collect();
    if raw.chars().count() > SUMMARY_MAX_CHARS { chars.push('…'); }
    Some(chars)
}

#[derive(Debug, Clone)]
pub struct SummaryParser {
    history: VecDeque<String>,
    depth: usize,
}

impl Default for SummaryParser {
    fn default() -> Self {
        Self { history: VecDeque::with_capacity(DEFAULT_HISTORY_DEPTH), depth: DEFAULT_HISTORY_DEPTH }
    }
}

impl SummaryParser {
    pub fn with_depth(depth: usize) -> Self {
        Self { history: VecDeque::with_capacity(depth), depth }
    }

    pub fn ingest(&mut self, reply: &str) -> Option<String> {
        let s = extract_summary(reply)?;
        while self.history.len() >= self.depth { self.history.pop_front(); }
        self.history.push_back(s.clone());
        Some(s)
    }

    pub fn render_history_block(&self) -> String {
        if self.history.is_empty() { return String::new(); }
        let mut out = String::from("<history>\n");
        for s in &self.history { out.push_str("- "); out.push_str(s); out.push('\n'); }
        out.push_str("</history>");
        out
    }

    pub fn len(&self) -> usize { self.history.len() }
    pub fn is_empty(&self) -> bool { self.history.is_empty() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_basic() {
        assert_eq!(extract_summary("<summary>read ok</summary> rest"), Some("read ok".into()));
    }

    #[test]
    fn extract_missing_returns_none() {
        assert_eq!(extract_summary("no marker"), None);
    }

    #[test]
    fn extract_empty_returns_none() {
        assert_eq!(extract_summary("<summary>   </summary>"), None);
    }

    #[test]
    fn extract_caps_at_30_chars_and_appends_ellipsis() {
        let long = "a".repeat(50);
        let s = format!("<summary>{long}</summary>");
        let out = extract_summary(&s).unwrap();
        assert_eq!(out.chars().count(), SUMMARY_MAX_CHARS + 1);
        assert!(out.ends_with('…'));
    }

    #[test]
    fn parser_keeps_last_n() {
        let mut p = SummaryParser::with_depth(3);
        p.ingest("<summary>one</summary>");
        p.ingest("<summary>two</summary>");
        p.ingest("<summary>three</summary>");
        p.ingest("<summary>four</summary>");
        assert_eq!(p.len(), 3);
        let block = p.render_history_block();
        assert!(block.contains("two"));
        assert!(block.contains("four"));
        assert!(!block.contains("one"));
    }

    #[test]
    fn parser_skips_replies_without_summary() {
        let mut p = SummaryParser::default();
        p.ingest("plain text");
        assert!(p.is_empty());
    }

    #[test]
    fn empty_parser_renders_empty_string() {
        let p = SummaryParser::default();
        assert_eq!(p.render_history_block(), "");
    }
}
