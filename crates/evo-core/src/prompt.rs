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
    /// Channel-specific formatting instruction appended to the system prompt
    /// when the agent runs inside a messaging channel (Telegram, Slack, etc.).
    /// `None` for the interactive CLI — no extra instruction is injected.
    pub channel_hint: Option<String>,
}

impl PromptCtx {
    pub fn today_in(workspace: impl Into<String>) -> Self {
        let now: DateTime<Utc> = Utc::now();
        Self {
            date: now.format("%Y-%m-%d").to_string(),
            workspace: workspace.into(),
            l1_index: String::new(),
            tool_names: Vec::new(),
            channel_hint: None,
        }
    }
}

pub fn build_system_prompt(ctx: &PromptCtx) -> String {
    let l1 = if ctx.l1_index.trim().is_empty() {
        "(none)"
    } else {
        ctx.l1_index.as_str()
    };
    let tools = ctx.tool_names.join(", ");
    let base = format!(
        "You are EvoClaw, a self-evolving AI agent. Self-learning loop: task→reflection(LLM)→skill distillation→~/.evoclaw/skills/→loaded into L1 each startup. Today: {date}. Workspace: {ws}. L1 (learned skills): {l1}\n\
         Tools: {tools}. Schema sent separately and cached. When user names a skill/playbook, call load_skill with its id.\n\
         Reply MUST start with <summary>≤30 chars: last result + this intent</summary>. Format ALL responses in Markdown: use ## headers, - lists, **bold**, `code`, and ```fenced blocks```.\n\
         Verify with tools, never assume. Read before edit. Workspace-only writes by default.\n\
         Tool error/denied → call a different tool IMMEDIATELY; never stop to write explanations. ask_user only when every tool has been tried.\n\
         Never log secrets. Cookies stay in Browser Worker — never in this context.",
        date = ctx.date,
        ws = ctx.workspace,
        l1 = l1,
        tools = tools,
    );
    match &ctx.channel_hint {
        Some(hint) => format!("{base}\n{hint}"),
        None => base,
    }
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
            channel_hint: None,
        };
        let p = build_system_prompt(&ctx);
        let lines = p.lines().count();
        assert_eq!(
            lines, 6,
            "system prompt must be 6 lines, got {lines}\n---\n{p}"
        );
    }

    #[test]
    fn under_3200_chars() {
        let ctx = PromptCtx {
            date: "2026-05-02".into(),
            workspace: "/Users/x".into(),
            l1_index: "very long indexing line ".repeat(20),
            tool_names: (0..10).map(|i| format!("tool_{i}")).collect(),
            channel_hint: None,
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
    fn includes_self_learning_description() {
        let ctx = PromptCtx::today_in("/tmp/ws");
        let p = build_system_prompt(&ctx);
        assert!(
            p.contains("self-evolving"),
            "prompt must describe EvoClaw as self-evolving"
        );
        assert!(
            p.contains("reflection"),
            "prompt must mention reflection step"
        );
        assert!(
            p.contains("skill"),
            "prompt must mention skill distillation"
        );
    }

    #[test]
    fn empty_l1_renders_none_marker() {
        let ctx = PromptCtx::today_in("/tmp/ws");
        let p = build_system_prompt(&ctx);
        assert!(p.contains("L1 (learned skills): (none)"));
    }

    #[test]
    fn channel_hint_appends_seventh_line() {
        let mut ctx = PromptCtx::today_in("/tmp/ws");
        ctx.channel_hint = Some("Format: plain text only.".into());
        let p = build_system_prompt(&ctx);
        let lines = p.lines().count();
        assert_eq!(
            lines, 7,
            "with channel_hint expect 7 lines, got {lines}\n{p}"
        );
        assert!(p.contains("Format: plain text only."));
    }
}
