use std::io::Write as _;

use crossterm::{cursor, execute, queue, style::Print, terminal};

use super::markdown::render_markdown_plain;
use super::state::{ConversationBlock, InputState, Role, TaskPanelState, UiState};
use crate::tui;
use crate::Theme;

/// Manages terminal output per the PRD layout:
///
/// ```text
/// ┌── scroll buffer (past, finished blocks) ────┐
/// │  User block 1                               │
/// │  Assistant block 1 (done)                   │
/// └─────────────────────────────────────────────┘  ← cleared & re-anchored each render
/// ─ EvoClaw · streaming ───────────────────────   ┐
///   live content lines...                         │ bottom zone
/// ─ task ──────────────────────────────────────   │ (cleared +
///   running: task-xxx  ·  elapsed: 5.2s           │  redrawn on
/// ─────────────────────────────────────────────   │  every event)
/// ─ input ─────────────────────────────────────
///   ▷ placeholder                                 │
/// ─────────────────────────────────────────────   │
/// Shortcuts: /status /usage ...                   ┘
/// ```
///
/// ## Invariants
///
/// After every call to `render` or `redraw_bottom`:
/// - `bottom_lines` = number of `\r\n`-terminated rows drawn in the bottom zone.
/// - `cursor_up_from_bottom` = how many rows the cursor has been moved UP from
///   the "at-rest" position (one past the last row of the bottom zone) to put it
///   inside the input box.
///
/// Before any new draw, both invariants are undone first so the cursor returns
/// to the at-rest position, then `MoveUp(bottom_lines)` reaches the top of the
/// old bottom zone for clearing.
pub struct UiRenderer {
    /// Lines currently occupied by the bottom redrawn zone.
    pub bottom_lines: u16,
    /// How many `state.messages` have been flushed to the scroll buffer.
    printed_msg_count: usize,
    theme: Theme,
    /// Rows the cursor was moved UP from "end of bottom zone" into the input box.
    cursor_up_from_bottom: u16,
}

impl UiRenderer {
    pub(crate) fn new(theme: Theme) -> Self {
        Self {
            bottom_lines: 0,
            printed_msg_count: 0,
            theme,
            cursor_up_from_bottom: 0,
        }
    }

    /// Reset bottom-zone tracking (call after slash-command output or raw-mode
    /// toggle where the cursor position is unknown).
    pub fn reset_bottom(&mut self) {
        self.bottom_lines = 0;
        self.cursor_up_from_bottom = 0;
    }

    /// Erase the current bottom zone from the terminal.
    /// Handles the cursor-in-input-box offset before clearing.
    /// Used by the slash-command path in `lib.rs`.
    pub fn clear_bottom(&mut self) {
        let mut stdout = std::io::stdout();
        let _ = execute!(stdout, cursor::Hide);
        // Undo cursor-in-input-box offset first.
        if self.cursor_up_from_bottom > 0 {
            let _ = execute!(
                stdout,
                cursor::MoveDown(self.cursor_up_from_bottom),
                cursor::MoveToColumn(0),
            );
        }
        if self.bottom_lines > 0 {
            let _ = execute!(
                stdout,
                cursor::MoveUp(self.bottom_lines),
                cursor::MoveToColumn(0),
                terminal::Clear(terminal::ClearType::FromCursorDown),
            );
        }
        self.reset_bottom();
    }

    /// Full render: flush any new finished messages into the scroll buffer,
    /// then redraw the bottom zone.
    ///
    /// Call this when `state.messages` may have grown (new user/assistant blocks).
    pub fn render(&mut self, state: &UiState) {
        self.do_render(state, true);
    }

    /// Redraw only the bottom zone without flushing new messages.
    ///
    /// Call this for timer ticks, input-changed events, and resize.
    pub fn redraw_bottom(&mut self, state: &UiState) {
        self.do_render(state, false);
    }

    // ── Core render engine ───────────────────────────────────────────────────

    /// Single entry-point for all terminal drawing.
    ///
    /// Invariant contract (see struct doc):
    ///   1. Undo cursor-in-input-box offset → cursor at "end of old bottom zone".
    ///   2. Move up `bottom_lines` → cursor at "top of old bottom zone", clear down.
    ///   3. Print new messages into the now-cleared space (only when `flush_msgs`).
    ///   4. Print the new bottom zone.
    ///   5. Move cursor into the input box; record the offset.
    fn do_render(&mut self, state: &UiState, flush_msgs: bool) {
        let mut stdout = std::io::stdout();

        // ── 1. Hide cursor & undo input-box offset ────────────────────────
        let _ = execute!(stdout, cursor::Hide);
        if self.cursor_up_from_bottom > 0 {
            let _ = execute!(
                stdout,
                cursor::MoveDown(self.cursor_up_from_bottom),
                cursor::MoveToColumn(0),
            );
            self.cursor_up_from_bottom = 0;
        }

        // ── 2. Erase old bottom zone ──────────────────────────────────────
        // Cursor is now at "end of old bottom zone" (one row past last \r\n).
        // Cap MoveUp to the visible terminal height - 1 so we never try to
        // move above the top of the screen. If the old bottom zone was taller
        // than the terminal (streaming block grew large), the excess rows are
        // already in the scrollback and cannot be cleared — but capping here
        // prevents the cursor from stopping at the wrong row and leaving
        // stale lines below it un-cleared.
        if self.bottom_lines > 0 {
            let (_, term_h) = crossterm::terminal::size().unwrap_or((80, 24));
            let safe_up = self.bottom_lines.min(term_h.saturating_sub(1));
            let _ = execute!(
                stdout,
                cursor::MoveUp(safe_up),
                cursor::MoveToColumn(0),
                terminal::Clear(terminal::ClearType::FromCursorDown),
            );
        }
        // Cursor is now at the TOP of the cleared space.

        // ── 3. Flush new finished messages (into the cleared space) ───────
        // This is the critical ordering: messages are printed BEFORE the
        // bottom zone so they appear above it in the scroll buffer.
        if flush_msgs && self.printed_msg_count < state.messages.len() {
            for block in &state.messages[self.printed_msg_count..] {
                let rendered = self.render_block(block, state.terminal_width as usize);
                for line in rendered.lines() {
                    let _ = queue!(stdout, Print(format!("{line}\r\n")));
                }
            }
            let _ = stdout.flush();
            self.printed_msg_count = state.messages.len();
        }

        // ── 4. Draw the bottom zone ───────────────────────────────────────
        let w = state.terminal_width as usize;
        let mut lines = 0u16;

        // Streaming block — shows actual content as it arrives.
        if let Some(streaming) = &state.streaming {
            let rendered = self.render_streaming_block(streaming, &state.task, w);
            for line in rendered.lines() {
                let _ = queue!(stdout, Print(format!("{line}\r\n")));
                lines += 1;
            }
        } else if state.task.running_task_id.is_some() || state.task.queued_count > 0 {
            // Non-streaming busy (tool execution / reflection).
            let rendered = self.render_task_panel(&state.task, w);
            for line in rendered.lines() {
                let _ = queue!(stdout, Print(format!("{line}\r\n")));
                lines += 1;
            }
            // Usage line (if available).
            if let Some(usage) = &state.usage {
                let rendered = self.render_usage_line(usage, w);
                for line in rendered.lines() {
                    let _ = queue!(stdout, Print(format!("{line}\r\n")));
                    lines += 1;
                }
            }
        } else if let Some(usage) = &state.usage {
            // Idle: show usage from last task.
            let rendered = self.render_usage_line(usage, w);
            for line in rendered.lines() {
                let _ = queue!(stdout, Print(format!("{line}\r\n")));
                lines += 1;
            }
        }

        // Input box.
        let input_rendered = self.render_input_box(&state.input, w);
        for line in input_rendered.lines() {
            let _ = queue!(stdout, Print(format!("{line}\r\n")));
            lines += 1;
        }

        // Shortcut hint — NO trailing \r\n.
        let shortcut = self.render_shortcut_line(&state.input, w);
        let _ = queue!(stdout, Print(shortcut));
        // NOTE: `lines` is intentionally NOT incremented for the shortcut row.

        let _ = stdout.flush();
        self.bottom_lines = lines;

        // ── 5. Reposition cursor inside the input box ─────────────────────
        // After printing the shortcut (no \r\n), cursor is on the shortcut row.
        // The input content line is exactly 1 row above the shortcut row.
        // For wrapped input, `extra_up` pushes further up.
        let bw = (state.terminal_width as usize).min(120);
        // Must match render_input_box content inner (bw-6).
        let inner = bw.saturating_sub(6);
        let text_before_byte = char_to_byte(&state.input.content, state.input.cursor_char);
        let text_before = &state.input.content[..text_before_byte];
        let display_before = tui::display_width(text_before);
        let total_display = tui::display_width(&state.input.content);

        let (cursor_col, extra_up): (u16, u16) = if state.input.content.is_empty() {
            (2, 0) // after "  " in placeholder row
        } else if inner == 0 {
            (4, 0)
        } else {
            let total_wraps = total_display.max(1).div_ceil(inner);
            let cursor_wrap = display_before / inner;
            let last_wrap = total_wraps.saturating_sub(1);
            let extra = last_wrap.saturating_sub(cursor_wrap) as u16;
            let col_in_wrap = (display_before % inner) as u16;
            (4 + col_in_wrap, extra)
        };

        // Input content is 2 rows above shortcut (closing rule sits between content and shortcut).
        let up = 2 + extra_up;
        let _ = execute!(
            stdout,
            cursor::MoveUp(up),
            cursor::MoveToColumn(cursor_col),
            cursor::Show,
        );
        self.cursor_up_from_bottom = up;
    }

    // ── Private rendering helpers ────────────────────────────────────────────

    fn bw(_term_w: usize) -> usize {
        // Always use the live terminal width so separators span the full
        // window. tui::terminal_width() already clamps to [60, 220].
        crate::tui::terminal_width()
    }

    /// Render a finished conversation block — open sides, top/bottom rule only.
    fn render_block(&self, block: &ConversationBlock, term_w: usize) -> String {
        let bw = Self::bw(term_w);
        let theme = &self.theme;

        let tc = match block.role {
            Role::User => theme.ok(),
            Role::Assistant => theme.frame(),
            Role::System => theme.dim(),
        };

        let mut out = format!(
            "\n{tc}{title}{r}\n",
            title = &block.title,
            r = theme.reset(),
        );

        let content = if block.role == Role::Assistant {
            render_markdown_plain(theme, &block.content, bw)
        } else {
            block.content.clone()
        };

        for raw_line in content.lines() {
            let visible = tui::strip_ansi(raw_line);
            if tui::display_width(&visible) <= bw {
                out.push_str(&format!("{raw_line}\n"));
            } else {
                for line in &tui::wrap_text(&visible, bw) {
                    out.push_str(&format!("{line}\n"));
                }
            }
        }

        out.push_str(&format!(
            "{tc}{fill}{r}\n",
            fill = "─".repeat(bw),
            r = theme.reset(),
        ));
        out
    }

    /// Render the streaming assistant block in the bottom zone.
    ///
    /// Shows the actual streaming content so the user can see the response
    /// as it arrives. The entire block is part of the redrawable bottom zone.
    fn render_streaming_block(
        &self,
        block: &ConversationBlock,
        task: &TaskPanelState,
        term_w: usize,
    ) -> String {
        let bw = Self::bw(term_w);
        let theme = &self.theme;

        let elapsed = task.elapsed_ms as f32 / 1000.0;
        let provider = if task.provider.is_empty() {
            "—"
        } else {
            task.provider.as_str()
        };

        let queued_suffix = if task.queued_count > 0 {
            format!(" · queued {}", task.queued_count)
        } else {
            String::new()
        };

        let mut out = format!(
            "{tc}{title} · streaming · {elapsed:.1}s{queued}{r}\n",
            tc = theme.frame(),
            title = &block.title,
            queued = queued_suffix,
            r = theme.reset(),
        );

        if !block.content.is_empty() {
            let rendered = render_markdown_plain(theme, &block.content, bw);
            let (_, term_h) = crossterm::terminal::size().unwrap_or((80, 24));
            // Reserve: header(1) + bottom-rule(1) + task-line(1)
            //        + blank-separator(1) + input-box(3) + shortcut(1) = 8 lines.
            let max_content = (term_h as usize).saturating_sub(8).max(3);

            // Build the physical-line list first (a logical line from the
            // markdown renderer may wrap into multiple terminal rows).  Then
            // take only the tail that fits within the physical-line budget.
            // Without this, wide code lines expand the streaming block past
            // term_h, pushing stale top-separator/header rows into the
            // scrollback buffer on every re-render and producing duplicate
            // cyan separator lines when the user scrolls up.
            let mut phys: Vec<String> = Vec::new();
            for raw_line in rendered.lines() {
                let visible = tui::strip_ansi(raw_line);
                if tui::display_width(&visible) <= bw {
                    phys.push(raw_line.to_string());
                } else {
                    for line in tui::wrap_text(&visible, bw) {
                        phys.push(line);
                    }
                }
            }
            let tail_start = phys.len().saturating_sub(max_content);
            for line in &phys[tail_start..] {
                out.push_str(&format!("{line}\n"));
            }
        }

        out.push_str(&format!(
            "{tc}{fill}{r}\n",
            tc = theme.frame(),
            fill = "─".repeat(bw),
            r = theme.reset(),
        ));
        out.push_str(&format!(
            "{dim}task: running {provider} · elapsed {elapsed:.1}s{r}\n",
            dim = theme.dim(),
            r = theme.reset(),
        ));
        out
    }

    /// Render the task status line when not streaming (tool execution / reflection).
    fn render_task_panel(&self, task: &TaskPanelState, term_w: usize) -> String {
        let bw = Self::bw(term_w);
        let theme = &self.theme;
        let elapsed = task.elapsed_ms as f32 / 1000.0;

        let running_id = task.running_task_id.as_deref().unwrap_or("processing");
        let provider = if task.provider.is_empty() {
            "—"
        } else {
            task.provider.as_str()
        };

        let mut out = format!(
            "{tc}{fill}{r}\n",
            tc = theme.warn(),
            fill = "─".repeat(bw),
            r = theme.reset(),
        );
        out.push_str(&format!(
            "{dim}task: running {id} · provider {provider} · queued {q} · elapsed {elapsed:.1}s{r}\n",
            dim = theme.dim(),
            id = running_id,
            q = task.queued_count,
            r = theme.reset(),
        ));
        out
    }

    /// Render the usage summary line.
    fn render_usage_line(&self, usage: &str, term_w: usize) -> String {
        let bw = Self::bw(term_w);
        let theme = &self.theme;
        let mut out = format!(
            "{ac}{fill}{r}\n",
            ac = theme.accent(),
            fill = "─".repeat(bw),
            r = theme.reset(),
        );
        out.push_str(&format!(
            "{dim}usage: {usage}{r}\n",
            dim = theme.dim(),
            r = theme.reset(),
        ));
        out
    }

    /// Render the bottom input line — simple rule + prompt line.
    fn render_input_box(&self, input: &InputState, term_w: usize) -> String {
        let bw = Self::bw(term_w);
        let inner = bw.saturating_sub(6);
        let theme = &self.theme;

        let header = "─ input ";
        let dash = "─".repeat(bw.saturating_sub(tui::display_width(header)));
        let mut out = format!(
            "\n{ac}{header}{dash}{r}\n",
            ac = theme.accent(),
            r = theme.reset(),
        );

        if input.content.is_empty() {
            let placeholder = "▷ Type your message and press Enter to send  ·  /help for commands";
            out.push_str(&format!(
                "  {dim}{placeholder}{r}\n",
                dim = theme.dim(),
                r = theme.reset(),
            ));
        } else {
            let wrapped = if tui::display_width(&input.content) <= inner {
                vec![input.content.clone()]
            } else {
                tui::wrap_text(&input.content, inner)
            };
            for line in &wrapped {
                out.push_str(&format!(
                    "  {ac}›{r} {line}\n",
                    ac = theme.accent(),
                    r = theme.reset(),
                ));
            }
        }

        out.push_str(&format!(
            "{ac}{fill}{r}\n",
            ac = theme.accent(),
            fill = "─".repeat(bw),
            r = theme.reset(),
        ));
        out
    }

    /// Single-line shortcut hint below the input box.
    /// Shows Tab-completion candidates when `input.completions` is non-empty;
    /// otherwise shows the static keyboard shortcut reminder.
    fn render_shortcut_line(&self, input: &InputState, term_w: usize) -> String {
        let bw = Self::bw(term_w);
        if !input.completions.is_empty() {
            let hint = format!("⇥  {}", input.completions.join("  "));
            let truncated = truncate_display(&hint, bw);
            format!(
                "{hi}{truncated}{r}",
                hi = self.theme.highlight(),
                r = self.theme.reset()
            )
        } else {
            let hint =
                "shortcuts: Tab /cmd  ·  ↑↓/Ctrl-P/N history  ·  Ctrl-R search  ·  Ctrl-C quit";
            let truncated = truncate_display(hint, bw);
            format!(
                "{dim}{truncated}{r}",
                dim = self.theme.dim(),
                r = self.theme.reset()
            )
        }
    }
}

/// Convert a char-index to a byte offset in `s`.
fn char_to_byte(s: &str, char_idx: usize) -> usize {
    s.char_indices()
        .nth(char_idx)
        .map(|(i, _)| i)
        .unwrap_or(s.len())
}

/// Truncate `s` to at most `max_cols` display columns (UTF-8 aware).
fn truncate_display(s: &str, max_cols: usize) -> &str {
    let mut used = 0usize;
    for (byte_i, ch) in s.char_indices() {
        let cw = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if used + cw > max_cols {
            return &s[..byte_i];
        }
        used += cw;
    }
    s
}
