//! System prompt builder. Source-of-truth template lives in PROMPTS.md §1.
//!
//! Hard constraint per PRD §44.1 / DEV_PLAN §0: 6 lines, ≤800 tokens.

use chrono::{DateTime, Utc};

#[derive(Debug, Clone)]
pub struct PromptCtx {
    pub date: String,
    pub workspace: String,
    pub l1_index: String,
    pub tool_names: Vec<String>,
}

impl PromptCtx {
    pub fn today_in(workspace: impl Into<String>) -> Self {
        let now: DateTime<Utc> = Utc::now();
        Self {
            date: now.format("%Y-%m-%d").to_string(),
            workspace: workspace.into(),
            l1_index: String::new(),
            tool_names: Vec::new(),
        }
    }
}

pub fn build_system_prompt(ctx: &PromptCtx) -> String {
    let l1 = if ctx.l1_index.trim().is_empty() { "(none)" } else { ctx.l1_index.as_str() };
    let tools = ctx.tool_names.join(", ");
    format!(
        "You are EvoClaw. Today: {date}. Workspace: {ws}. Memory L1: {l1}\n\
         Tools: {tools}. Schema sent separately and cached.\n\
         Reply MUST start with <summary>≤30 chars: last result + this intent</summary>.\n\
         Verify with tools, never assume. Read before edit. Workspace-only writes by default.\n\
         On failure: 1st read stderr; 2nd analyze; 3rd switch tool; 4th ask_user.\n\
         Never log secrets. Cookies stay in Browser Worker — never in this context.",
        date = ctx.date, ws = ctx.workspace, l1 = l1, tools = tools,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_six_lines() {
        let ctx = PromptCtx {
            date: "2026-05-02".into(),
            workspace: "/tmp/ws".into(),
            l1_index: "user prefers fish shell".into(),
            tool_names: vec!["read_file".into(), "run_shell".into()],
        };
        let p = build_system_prompt(&ctx);
        let lines = p.lines().count();
        assert_eq!(lines, 6, "system prompt must be 6 lines, got {lines}\n---\n{p}");
    }

    #[test]
    fn under_3200_chars() {
        let ctx = PromptCtx {
            date: "2026-05-02".into(),
            workspace: "/Users/x".into(),
            l1_index: "very long indexing line ".repeat(20),
            tool_names: (0..10).map(|i| format!("tool_{i}")).collect(),
        };
        let p = build_system_prompt(&ctx);
        assert!(p.len() < 3200, "prompt too long: {} chars", p.len());
    }

    #[test]
    fn includes_summary_protocol() {
        let ctx = PromptCtx::today_in("/tmp/ws");
        let p = build_system_prompt(&ctx);
        assert!(p.contains("<summary>"));
    }

    #[test]
    fn empty_l1_renders_none_marker() {
        let ctx = PromptCtx::today_in("/tmp/ws");
        let p = build_system_prompt(&ctx);
        assert!(p.contains("Memory L1: (none)"));
    }
}
