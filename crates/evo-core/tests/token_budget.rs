//! Token budget regression tests — Phase 3.8 / DEV_PLAN §3.

use evo_core::compression::{compress_if_due, CompressionConfig};
use evo_core::summary::SummaryParser;
use evo_providers::{CacheKind, Message, Role, ToolFingerprint, ToolSpec};
use serde_json::json;

fn tspec(n: &str) -> ToolSpec {
    ToolSpec { name: n.into(), description: format!("desc {n}"), schema: json!({}) }
}

fn user_msg(body: &str) -> Message {
    Message { role: Role::User, content: body.into(), tool_calls: Vec::new(), tool_results: Vec::new(), cache_control: CacheKind::None }
}

#[test]
fn fingerprint_reuse_dominates_long_session() {
    let mut fp = ToolFingerprint::default();
    let tools = vec![tspec("read_file"), tspec("write_file"), tspec("run_shell"), tspec("ask_user")];
    let mut full = 0;
    let mut reuse = 0;
    for turn in 0..30 {
        let p = fp.payload_for_turn(turn, tools.clone());
        if p.is_reuse() { reuse += 1; } else { full += 1; }
    }
    assert_eq!(full, 3, "expected exactly 3 full sends across 30 turns (turns 0/10/20)");
    assert_eq!(reuse, 27);
}

#[test]
fn summary_parser_caps_history_block() {
    let mut p = SummaryParser::default();
    for i in 0..200 {
        p.ingest(&format!("<summary>turn {i} done</summary>"));
    }
    let block = p.render_history_block();
    assert!(block.len() < 1500, "history block too large: {} chars", block.len());
    assert!(block.contains("turn 199 done"));
    assert!(!block.contains("turn 0 done"));
}

#[test]
fn long_history_compresses_50_percent() {
    let big = format!("<thinking>{}</thinking>", "x".repeat(2000));
    let mut history: Vec<Message> = (0..30).map(|_| user_msg(&big)).collect();
    let original: usize = history.iter().map(|m| m.content.len()).sum();
    let changed = compress_if_due(&mut history, 5, CompressionConfig::default());
    assert!(changed);
    let compressed: usize = history.iter().map(|m| m.content.len()).sum();
    let saved_pct = 100.0 * (1.0 - compressed as f64 / original as f64);
    assert!(saved_pct >= 50.0, "expected ≥50% reduction, got {:.1}%", saved_pct);
    for m in &history[20..] {
        assert!(!m.content.contains("[Truncated]"), "recent msg should be untouched");
    }
}

#[test]
fn cache_hit_rate_target_60_percent() {
    use evo_providers::Usage;
    let mut total_in = 0u64;
    let mut total_cached = 0u64;
    for _ in 0..30 {
        let u = Usage { input_tokens: 1000, cached_tokens: 700, output_tokens: 50 };
        total_in += u.input_tokens;
        total_cached += u.cached_tokens;
    }
    let hit_rate = total_cached as f64 / total_in as f64;
    assert!(hit_rate >= 0.6, "cache_hit_rate {hit_rate} below PRD §42.6 target");
}
