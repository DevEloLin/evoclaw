//! Tag-level periodic compression: PRD §42.5 招式 5.

use evo_providers::Message;

const TAGS: &[(&str, &str)] = &[
    ("<thinking>", "</thinking>"),
    ("<tool_use>", "</tool_use>"),
    ("<tool_result>", "</tool_result>"),
];

const TRUNCATE_MARKER: &str = " ... [Truncated] ... ";

#[derive(Debug, Clone, Copy)]
pub struct CompressionConfig {
    pub turn_period: u64,
    pub keep_recent: usize,
    pub max_segment: usize,
}

impl Default for CompressionConfig {
    fn default() -> Self { Self { turn_period: 5, keep_recent: 10, max_segment: 400 } }
}

pub fn compress_if_due(messages: &mut [Message], turn: u64, cfg: CompressionConfig) -> bool {
    if cfg.turn_period == 0 || turn % cfg.turn_period != 0 || turn == 0 { return false; }
    let len = messages.len();
    if len <= cfg.keep_recent { return false; }
    let cutoff = len - cfg.keep_recent;
    let mut changed = false;
    for msg in messages.iter_mut().take(cutoff) {
        let new_content = compress_tags(&msg.content, cfg.max_segment);
        if new_content != msg.content {
            msg.content = new_content;
            changed = true;
        }
    }
    changed
}

pub fn compress_tags(content: &str, max_segment: usize) -> String {
    let mut out = content.to_string();
    for (open, close) in TAGS {
        out = compress_one_tag(&out, open, close, max_segment);
    }
    out
}

fn compress_one_tag(haystack: &str, open: &str, close: &str, max_segment: usize) -> String {
    let mut out = String::with_capacity(haystack.len());
    let mut idx = 0;
    while let Some(open_pos) = haystack[idx..].find(open) {
        let abs_open = idx + open_pos;
        out.push_str(&haystack[idx..abs_open + open.len()]);
        let after = abs_open + open.len();
        let Some(close_rel) = haystack[after..].find(close) else {
            out.push_str(&haystack[after..]);
            return out;
        };
        let abs_close = after + close_rel;
        let inner = &haystack[after..abs_close];
        out.push_str(&truncate(inner, max_segment));
        out.push_str(close);
        idx = abs_close + close.len();
    }
    out.push_str(&haystack[idx..]);
    out
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max + TRUNCATE_MARKER.len() * 2 { return s.to_string(); }
    let half = max / 2;
    let head_end = floor_char_boundary(s, half);
    let tail_start = ceil_char_boundary(s, s.len().saturating_sub(half));
    format!("{}{}{}", &s[..head_end], TRUNCATE_MARKER, &s[tail_start..])
}

fn floor_char_boundary(s: &str, mut i: usize) -> usize {
    while i > 0 && !s.is_char_boundary(i) { i -= 1; }
    i
}
fn ceil_char_boundary(s: &str, mut i: usize) -> usize {
    while i < s.len() && !s.is_char_boundary(i) { i += 1; }
    i
}

#[cfg(test)]
mod tests {
    use super::*;
    use evo_providers::{CacheKind, Role};

    fn msg(body: &str) -> Message {
        Message { role: Role::User, content: body.into(), tool_calls: Vec::new(), tool_results: Vec::new(), cache_control: CacheKind::None }
    }

    #[test]
    fn no_op_at_turn_0() {
        let mut v = vec![msg("<thinking>xxx</thinking>")];
        assert!(!compress_if_due(&mut v, 0, CompressionConfig::default()));
    }

    #[test]
    fn skip_when_turn_not_multiple_of_period() {
        let big = format!("<thinking>{}</thinking>", "x".repeat(2000));
        let mut v: Vec<Message> = (0..15).map(|_| msg(&big)).collect();
        assert!(!compress_if_due(&mut v, 7, CompressionConfig::default()));
    }

    #[test]
    fn compresses_old_messages_only() {
        let big = format!("<thinking>{}</thinking>", "x".repeat(2000));
        let mut v: Vec<Message> = (0..15).map(|_| msg(&big)).collect();
        let changed = compress_if_due(&mut v, 5, CompressionConfig::default());
        assert!(changed);
        for (i, m) in v.iter().enumerate().take(15).skip(5) {
            assert_eq!(m.content.len(), big.len(), "msg {i} should be untouched");
        }
        for (i, m) in v.iter().enumerate().take(5) {
            assert!(m.content.contains(TRUNCATE_MARKER), "msg {i} should be truncated");
            assert!(m.content.len() < big.len() / 2);
        }
    }

    #[test]
    fn ignores_messages_below_keep_recent_threshold() {
        let big = format!("<thinking>{}</thinking>", "x".repeat(2000));
        let mut v: Vec<Message> = (0..5).map(|_| msg(&big)).collect();
        assert!(!compress_if_due(&mut v, 5, CompressionConfig::default()));
    }

    #[test]
    fn compress_tags_handles_multiple_tags_in_one_message() {
        let s = format!("<thinking>{}</thinking><tool_use>{}</tool_use>", "x".repeat(1000), "y".repeat(1000));
        let out = compress_tags(&s, 100);
        assert_eq!(out.matches(TRUNCATE_MARKER).count(), 2);
    }

    #[test]
    fn compress_tags_passes_through_short_segments() {
        let s = "<thinking>quick</thinking>".to_string();
        assert_eq!(compress_tags(&s, 1000), s);
    }
}
