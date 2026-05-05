//! TerminalUI rendering, Spinner, and history path helper.

use crate::config::logs_dir;
use crate::theme::{truncate_to, Theme};
use crate::tui;
use eyre::Result;
use std::path::PathBuf;

// ---------------------------------------------------------------------------
// TerminalUI
// ---------------------------------------------------------------------------

/// Terminal UI utilities for adaptive layouts
pub(crate) struct TerminalUI;

impl TerminalUI {
    pub(crate) fn render_top_status_bar(
        theme: &Theme,
        runtime: &str,
        provider: &str,
        model: &str,
        workspace: &str,
        timestamp: &str,
    ) -> String {
        let w = Self::block_width();
        let inner_w = w.saturating_sub(4);
        let left = truncate_to(&format!("{runtime}  •  {provider}  •  {model}"), inner_w);
        let timestamp_w = tui::display_width(timestamp);
        let workspace_line = truncate_to(
            &format!("workspace: {workspace}"),
            inner_w.saturating_sub(timestamp_w + 2),
        );
        let gap = inner_w
            .saturating_sub(tui::display_width(&workspace_line))
            .saturating_sub(timestamp_w);
        let line1 = format!(
            "{primary}{left}{r}",
            primary = theme.frame(),
            r = theme.reset()
        );
        let line2 = format!(
            "{dim}{workspace_line}{}{timestamp}{r}",
            " ".repeat(gap),
            dim = theme.dim(),
            r = theme.reset()
        );
        let sep = format!(
            "{c}{}{r}\n",
            "─".repeat(w),
            c = theme.frame(),
            r = theme.reset(),
        );
        format!("\n{sep}  {line1}\n  {line2}\n{sep}")
    }

    /// `color` is an ANSI escape string (e.g. `theme.frame()`) applied to both
    /// the top title-rule and the bottom closing rule. Pass the same value to
    /// both calls so the pair always shares one colour.
    pub(crate) fn panel(theme: &Theme, title: &str, lines: &[String], color: &str) -> String {
        let w = Self::block_width();
        let inner_w = w.saturating_sub(2);
        let title_plain = format!("─ {title} ");
        let fill = "─".repeat(w.saturating_sub(tui::display_width(&title_plain)));
        let mut out = String::new();
        out.push('\n');
        // Top separator with title
        out.push_str(&format!(
            "{color}─ {title} {fill}{r}\n",
            r = theme.reset(),
            fill = fill,
        ));
        for raw in lines {
            let visible = tui::strip_ansi(raw.as_str());
            if tui::display_width(&visible) <= inner_w {
                // Fits — preserve ANSI colour codes
                out.push_str(&format!("  {raw}\n"));
            } else {
                // Too wide — wrap as plain text
                for line in tui::wrap_text(&visible, inner_w) {
                    out.push_str(&format!("  {line}\n"));
                }
            }
        }
        // Bottom separator — same colour as top
        out.push_str(&format!("{color}{}{r}\n", "─".repeat(w), r = theme.reset(),));
        out
    }

    pub(crate) fn render_markdown(theme: &Theme, text: &str) -> String {
        crate::ui::render_markdown_plain(theme, text, Self::block_width())
    }

    /// Width for answer / status / usage boxes.
    /// Uses `tui::terminal_width()` so separators span the full terminal
    /// window. Already clamped to [60, 220] inside that function.
    pub(crate) fn block_width() -> usize {
        tui::terminal_width()
    }

    /// Startup welcome screen — two-column layout.
    ///
    /// Left column  : robot mascot · brand · provider/model · auth status
    /// Right column : quick-start commands · live status badges
    pub(crate) fn render_welcome(
        theme: &Theme,
        version: &str,
        provider: &str,
        model: &str,
        workspace: &str,
        auth_ok: bool,
        auth_note: &str,
    ) -> String {
        let w = Self::block_width();
        let c = theme.frame();
        let d = theme.dim();
        let ok = theme.ok();
        let wn = theme.warn();
        let hi = theme.highlight();
        let bd = theme.bold();
        let r = theme.reset();

        // ── mascot: robot spider (13 cols wide, 7 lines) ────────────────────
        // Block chars (▄ ▀ █) are East-Asian-Narrow → width 1 in all terminals.
        // Every line is exactly 13 display columns; all rows are mirror-symmetric
        // around the centre column (position 7).
        let mascot: &[&str] = &[
            r"\\  ▄   ▄  //", // top legs (\\,//) + antennae (▄ at 5,9)
            r"  ▄███████▄  ", // head arch
            r"  █       █  ", // blank forehead
            r"  █ ▀▀ ▀▀ █  ", // eyes: ▀▀ at 5-6 and 8-9 (symmetric)
            r"  ▀█▄▄▄▄▄█▀  ", // jaw: hollow mouth + lower teeth
            r"    ▄▄ ▄▄    ", // chin nubs aligned under eyes (▄▄ at 5-6, 8-9)
            r"//  ██ ██  \\", // bottom legs: full blocks under chin + spider legs
        ];
        let mascot_colors = [d, c, c, c, c, c, d];

        // ── left column lines (no leading spaces — added in render loop) ──
        let mut left: Vec<String> = Vec::new();
        left.push(String::new());
        for (line, col) in mascot.iter().zip(mascot_colors.iter()) {
            left.push(format!("{col}{line}{r}"));
        }
        left.push(String::new());
        left.push(format!("{bd}{c}EvoClaw{r}  {d}v{version}{r}"));
        left.push(format!("{d}self-evolving agent runtime{r}"));
        left.push(String::new());
        left.push(format!("{c}{provider}{r}  {d}·  {model}{r}"));
        left.push(format!("{d}{workspace}{r}"));
        left.push(String::new());
        if auth_ok {
            let note = if auth_note.is_empty() {
                String::new()
            } else {
                format!("  {d}{auth_note}{r}")
            };
            left.push(format!("{ok}✓ ready{r}{note}"));
        } else {
            left.push(format!("{wn}⚠ {auth_note}{r}"));
        }
        left.push(String::new());

        // ── right column lines ────────────────────────────────────────────
        let right_div_w = 26usize;
        let div = format!("{c}{}{r}", "─".repeat(right_div_w));

        let mut right: Vec<String> = Vec::new();
        right.push(String::new());
        right.push(format!("{bd}{hi}Quick start{r}"));
        right.push(div.clone());
        right.push(format!("{c}/help   {r} {d}list all commands{r}"));
        right.push(format!("{c}/login  {r} {d}configure auth{r}"));
        right.push(format!("{c}/doctor {r} {d}health check{r}"));
        right.push(format!("{c}/skill  {r} {d}browse skills{r}"));
        right.push(String::new());
        right.push(format!("{bd}{hi}Status{r}"));
        right.push(div.clone());
        let auth_badge = if auth_ok {
            format!("{ok}✓ ready{r}")
        } else {
            format!("{wn}⚠ run /login{r}")
        };
        right.push(format!("{d}auth   {r}  {auth_badge}"));
        right.push(format!("{d}model  {r}  {d}{model}{r}"));
        right.push(String::new());
        right.push(format!("{d}Ctrl-D to exit  ·  /help for commands{r}"));
        right.push(String::new());

        // ── two-column render ─────────────────────────────────────────────
        // left_col_w = visible width budget for the left column (incl. 2-space margin).
        let left_col_w = (w * 44 / 100).max(28);
        let col_gap = 4usize;
        let sep = format!("{c}{}{r}", "─".repeat(w));

        let max_rows = left.len().max(right.len());
        let mut out = String::new();
        out.push('\n');
        out.push_str(&sep);
        out.push('\n');

        for i in 0..max_rows {
            let l = left.get(i).map(String::as_str).unwrap_or("");
            let rv = right.get(i).map(String::as_str).unwrap_or("");
            let l_vis = tui::display_width_ansi(l);
            // Pad so the right column always starts at the same horizontal pos.
            let pad = left_col_w.saturating_sub(2 + l_vis);
            out.push_str(&format!("  {l}{}{rv}\n", " ".repeat(pad + col_gap)));
        }

        out.push_str(&sep);
        out.push('\n');
        out
    }

    /// Render the post-task dashboard: conversation card, task state, usage.
    pub(crate) fn render_answer_block(
        theme: &Theme,
        body: &str,
        turns: u64,
        elapsed_secs: f32,
        model: &str,
        provider: &str,
        usage_summary: &str,
    ) -> String {
        let mut conversation = vec![format!(
            "{}EvoClaw ({provider}){}    {elapsed_secs:.1}s · {turns} turn{}",
            theme.ok(),
            theme.reset(),
            if turns == 1 { "" } else { "s" },
        )];
        let rendered = Self::render_markdown(theme, body);
        for line in rendered.split('\n') {
            conversation.push(line.to_string());
        }

        let mut out = Self::panel(theme, "会话历史区", &conversation, theme.frame());
        out.push_str(&Self::panel(
            theme,
            "任务状态区",
            &[
                format!("任务: task-complete ({provider})"),
                "状态: 已完成".to_string(),
                format!("已用时: {elapsed_secs:.1}s"),
            ],
            theme.ok(),
        ));
        out.push_str(&Self::panel(
            theme,
            "使用信息区",
            &[
                format!("model: {model}"),
                format!("provider: {provider}"),
                format!("usage: {usage_summary}"),
                "30d 总计: 查看 /usage 获取完整汇总".to_string(),
            ],
            theme.info(),
        ));
        out
    }
}

// ---------------------------------------------------------------------------
// History path
// ---------------------------------------------------------------------------

/// REPL history file. Persisted across sessions so arrow-up resurfaces
/// previous prompts. Lives in the same directory as the JSONL session
/// logs (default `/tmp/evoclaw/history.txt`).
pub(crate) fn history_path() -> Result<PathBuf> {
    Ok(logs_dir()?.join("history.txt"))
}

// ---------------------------------------------------------------------------
// Spinner
// ---------------------------------------------------------------------------

/// Enhanced terminal spinner with dynamic phase updates. Spawned on a thread
/// so the agent's `await` can run free; the `Drop` impl signals the thread
/// to stop and erases the spinner line so the caller can print clean text after.
pub(crate) struct Spinner {
    handle: Option<std::thread::JoinHandle<()>>,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl Spinner {
    /// In-place spinner anchored with crossterm `SavePosition` /
    /// `RestorePosition`. Even if a stray `println!` from the runtime
    /// or a tool sneaks between two frames, the next frame still
    /// rewrites the **same** anchor line instead of cascading down.
    pub(crate) fn start(theme: Theme, label: &str) -> Self {
        use crossterm::cursor::{RestorePosition, SavePosition};
        use crossterm::terminal::{Clear, ClearType};
        use std::io::Write as _;

        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stop_clone = stop.clone();
        let warn = theme.warn().to_string();
        let dim = theme.dim().to_string();
        let reset = theme.reset().to_string();
        let label = label.to_string();

        // Reserve an anchor row before saving cursor position. We
        // print a newline (advances cursor + scrolls if at bottom),
        // move back up onto that fresh row, then `SavePosition`. If
        // we just `SavePosition` at the bottom of the scroll region
        // a later `RestorePosition` would target an already-scrolled-
        // away line.
        let mut stderr = std::io::stderr();
        let _ = crossterm::execute!(stderr, crossterm::style::Print("\n"));
        let _ = crossterm::execute!(stderr, crossterm::cursor::MoveUp(1));
        let _ = crossterm::execute!(stderr, SavePosition);
        let _ = stderr.flush();

        let handle = std::thread::spawn(move || {
            let frames: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
            let started = std::time::Instant::now();
            let mut idx = 0usize;
            while !stop_clone.load(std::sync::atomic::Ordering::SeqCst) {
                let elapsed = started.elapsed().as_secs_f32();
                let mut err = std::io::stderr();
                let _ = crossterm::execute!(
                    err,
                    RestorePosition,
                    Clear(ClearType::CurrentLine),
                    crossterm::style::Print(format!(
                        "{warn}{}{reset} {label} {dim}({:.1}s){reset}",
                        frames[idx % frames.len()],
                        elapsed,
                    )),
                );
                let _ = err.flush();
                idx = idx.wrapping_add(1);
                std::thread::sleep(std::time::Duration::from_millis(80));
            }
            // Drop path: clear the anchor line and leave the cursor
            // sitting on it so the next caller starts fresh.
            let mut err = std::io::stderr();
            let _ = crossterm::execute!(err, RestorePosition, Clear(ClearType::CurrentLine));
            let _ = err.flush();
        });
        Self {
            handle: Some(handle),
            stop,
        }
    }
}

impl Drop for Spinner {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}
