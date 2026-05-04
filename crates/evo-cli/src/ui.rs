//! EvoClaw CLI — event-driven UI architecture
//!
//! Implements the interaction model from `prd/plan/ui.md`:
//!
//!   * [`UiEvent`]    — all UI state transitions flow through this enum.
//!   * [`UiState`]    — single source of truth for what the terminal shows.
//!   * [`UiRenderer`] — translates `UiState` into terminal output.
//!   * [`run_input_task_sync`] — raw-mode keyboard reader; sends [`UiEvent`]s.
//!
//! Design rules (from PRD):
//!   - Input area never shows submitted questions, streaming output, or task logs.
//!   - Submitted questions immediately appear in the conversation area above.
//!   - Streaming deltas only update the current answer block (not via println!).
//!   - Task status updates in-place — no repeated "processing…" lines.
//!   - Slash commands are instant; their output goes to the conversation area.

use std::collections::VecDeque;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::Instant;

use crossterm::{cursor, execute, queue, style::Print, terminal};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use tokio::sync::mpsc;

use crate::tui;
use crate::Theme;

// ── UiEvent ──────────────────────────────────────────────────────────────────

/// Every observable UI change — input editing, task lifecycle, streaming
/// deltas, resize — arrives as a `UiEvent`.
#[derive(Debug, Clone)]
pub enum UiEvent {
    /// User edited the input field (every keystroke).
    InputChanged { content: String, cursor_char: usize },
    /// User pressed Enter; `content` may be a slash command or a task prompt.
    InputSubmitted {
        task_id: String,
        content: String,
        timestamp: String,
    },
    /// A user message has been appended to the conversation area.
    UserMessageAdded {
        task_id: String,
        content: String,
        timestamp: String,
        /// true when another task is already running (this one is queued).
        queued: bool,
    },
    /// A task was added to the serial queue.
    TaskQueued {
        task_id: String,
        queued_count: usize,
    },
    /// The assistant block for this task was created; streaming begins.
    AssistantStarted {
        task_id: String,
        provider: String,
        timestamp: String,
    },
    /// One streaming chunk arrived from the model.
    AssistantDelta { task_id: String, delta: String },
    /// Streaming finished; final usage/timing metadata attached.
    AssistantDone {
        task_id: String,
        usage_summary: String,
        elapsed_secs: f32,
        model: String,
        provider: String,
    },
    /// Timer tick — re-render elapsed time in the task panel.
    StatusTick,
    /// A slash command produced output for the conversation area.
    SlashCommandOutput { title: String, lines: Vec<String> },
    /// An error from a task or provider.
    Error { message: String },
    /// Terminal was resized.
    Resize { width: u16 },
    /// Explicit full-redraw request.
    Redraw,
    /// Input task exited (Ctrl-C or Ctrl-D).
    Shutdown,
}

// ── State types ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub enum Role {
    User,
    Assistant,
    System,
}

#[derive(Debug, Clone, PartialEq)]
pub enum BlockStatus {
    Submitted,
    Queued,
    Streaming,
    Done,
    Failed,
}

/// One card in the conversation area.
#[derive(Debug, Clone)]
pub struct ConversationBlock {
    pub task_id: Option<String>,
    pub role: Role,
    pub title: String,
    pub content: String,
    pub status: BlockStatus,
}

/// State of the bottom input box.
#[derive(Debug, Clone, Default)]
pub struct InputState {
    pub content: String,
    pub cursor_char: usize,
}

/// State of the live task status panel.
#[derive(Debug, Clone, Default)]
pub struct TaskPanelState {
    pub running_task_id: Option<String>,
    pub queued_count: usize,
    pub provider: String,
    pub started_at: Option<Instant>,
    pub elapsed_ms: u64,
}

/// Complete UI state — single source of truth for the renderer.
pub struct UiState {
    /// Finished conversation blocks (user, assistant, system).
    pub messages: Vec<ConversationBlock>,
    /// The assistant block currently being streamed (not yet in `messages`).
    pub streaming: Option<ConversationBlock>,
    pub input: InputState,
    pub task: TaskPanelState,
    pub terminal_width: u16,
}

impl Default for UiState {
    fn default() -> Self {
        Self::new()
    }
}

impl UiState {
    pub fn new() -> Self {
        let (w, _) = terminal::size().unwrap_or((100, 40));
        Self {
            messages: Vec::new(),
            streaming: None,
            input: InputState::default(),
            task: TaskPanelState::default(),
            terminal_width: w,
        }
    }

    /// Apply a `UiEvent`. Returns `true` when a redraw is warranted.
    pub fn apply(&mut self, event: &UiEvent) -> bool {
        match event {
            UiEvent::InputChanged {
                content,
                cursor_char,
            } => {
                self.input.content.clone_from(content);
                self.input.cursor_char = *cursor_char;
                true
            }
            UiEvent::UserMessageAdded {
                task_id,
                content,
                timestamp,
                queued,
            } => {
                let title = if *queued {
                    format!("You · {timestamp} · queued")
                } else {
                    format!("You · {timestamp}")
                };
                self.messages.push(ConversationBlock {
                    task_id: Some(task_id.clone()),
                    role: Role::User,
                    title,
                    content: content.clone(),
                    status: if *queued {
                        BlockStatus::Queued
                    } else {
                        BlockStatus::Submitted
                    },
                });
                true
            }
            UiEvent::TaskQueued {
                task_id,
                queued_count,
            } => {
                self.task.queued_count = *queued_count;
                for block in &mut self.messages {
                    if block.task_id.as_deref() == Some(task_id.as_str()) {
                        block.status = BlockStatus::Queued;
                    }
                }
                true
            }
            UiEvent::AssistantStarted {
                task_id,
                provider,
                timestamp,
            } => {
                self.streaming = Some(ConversationBlock {
                    task_id: Some(task_id.clone()),
                    role: Role::Assistant,
                    title: format!("EvoClaw · {provider} · {timestamp}"),
                    content: String::new(),
                    status: BlockStatus::Streaming,
                });
                self.task.running_task_id = Some(task_id.clone());
                self.task.provider.clone_from(provider);
                self.task.started_at = Some(Instant::now());
                self.task.elapsed_ms = 0;
                self.task.queued_count = self.task.queued_count.saturating_sub(1);
                true
            }
            UiEvent::AssistantDelta { task_id, delta } => {
                if let Some(block) = &mut self.streaming {
                    if block.task_id.as_deref() == Some(task_id.as_str()) {
                        block.content.push_str(delta);
                    }
                }
                if let Some(t) = self.task.started_at {
                    self.task.elapsed_ms = t.elapsed().as_millis() as u64;
                }
                true
            }
            UiEvent::AssistantDone {
                task_id,
                usage_summary,
                elapsed_secs,
                provider: _,
                ..
            } => {
                if let Some(mut block) = self.streaming.take() {
                    if block.task_id.as_deref() == Some(task_id.as_str()) {
                        block.status = BlockStatus::Done;
                        block.title =
                            format!("{} · {elapsed_secs:.1}s · {usage_summary}", block.title);
                        self.messages.push(block);
                    } else {
                        self.streaming = Some(block);
                    }
                }
                self.task.running_task_id = None;
                self.task.elapsed_ms = (elapsed_secs * 1000.0) as u64;
                true
            }
            UiEvent::StatusTick => {
                if let Some(t) = self.task.started_at {
                    self.task.elapsed_ms = t.elapsed().as_millis() as u64;
                }
                self.task.running_task_id.is_some() || self.streaming.is_some()
            }
            UiEvent::SlashCommandOutput { title, lines } => {
                self.messages.push(ConversationBlock {
                    task_id: None,
                    role: Role::System,
                    title: title.clone(),
                    content: lines.join("\n"),
                    status: BlockStatus::Done,
                });
                true
            }
            UiEvent::Error { message } => {
                self.messages.push(ConversationBlock {
                    task_id: None,
                    role: Role::System,
                    title: "error".to_string(),
                    content: message.clone(),
                    status: BlockStatus::Failed,
                });
                true
            }
            UiEvent::Resize { width } => {
                self.terminal_width = *width;
                true
            }
            UiEvent::Redraw => true,
            _ => false,
        }
    }
}

// ── UiRenderer ────────────────────────────────────────────────────────────────

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
        if self.bottom_lines > 0 {
            let _ = execute!(
                stdout,
                cursor::MoveUp(self.bottom_lines),
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

        // Streaming indicator — exactly ONE row by design (see
        // `render_streaming_oneline` for rationale).
        if let Some(streaming) = &state.streaming {
            let line = self.render_streaming_oneline(streaming, &state.task, w);
            let _ = queue!(stdout, Print(format!("{line}\r\n")));
            lines += 1;
        } else if state.task.running_task_id.is_some() || state.task.queued_count > 0 {
            // Non-streaming busy (tool execution / reflection).
            let rendered = self.render_task_panel(&state.task, w);
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

        // Shortcut hint — NO trailing \r\n.  Printing \r\n when the cursor is
        // already on the last terminal row scrolls the viewport on every
        // keystroke, causing the content area above to shift.  We leave the
        // cursor sitting on the shortcut row; `bottom_lines` counts only the
        // \r\n-terminated rows above it, and the cursor-reposition math uses
        // `up = 2` (not 3) to compensate.
        let shortcut = self.render_shortcut_line(w);
        let _ = queue!(stdout, Print(shortcut));
        // NOTE: `lines` is intentionally NOT incremented for the shortcut row.

        let _ = stdout.flush();
        self.bottom_lines = lines;

        // ── 5. Reposition cursor inside the input box ─────────────────────
        // After `lines` \r\n rows the cursor is at row S+lines.  The shortcut
        // was printed WITHOUT \r\n so cursor is still on row S+lines.
        // The input text line is 2 rows above the shortcut:
        //   S+lines   : shortcut              ← cursor after draw
        //   S+lines-1 : ╰── (input box bottom border)
        //   S+lines-2 : │ text │              ← cursor target
        //
        // For wrapped input, `extra_up` pushes further up when the logical
        // cursor is on an earlier wrap line than the last one.
        let bw = (state.terminal_width as usize).min(120);
        // Must match render_input_box content inner (bw-6, not bw-4).
        let inner = bw.saturating_sub(6);
        let text_before_byte = char_to_byte(&state.input.content, state.input.cursor_char);
        let text_before = &state.input.content[..text_before_byte];
        let display_before = tui::display_width(text_before);
        let total_display = tui::display_width(&state.input.content);

        let (cursor_col, extra_up): (u16, u16) = if state.input.content.is_empty() {
            (2, 0) // after "│ " in placeholder row
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

    #[allow(dead_code)]
    fn bw(term_w: usize) -> usize {
        term_w.min(120)
    }

    /// Render a finished conversation block — open sides, top/bottom rule only.
    fn render_block(&self, block: &ConversationBlock, term_w: usize) -> String {
        let bw = Self::bw(term_w);
        let inner = bw.saturating_sub(2);
        let theme = &self.theme;

        // Role colour used for both the opening and closing rules so each
        // block's pair of separators is visually tied together.
        let tc = match block.role {
            Role::User => theme.ok(),
            Role::Assistant => theme.frame(),
            Role::System => theme.dim(),
        };

        let title_w = tui::display_width(&block.title);
        let dash = "─".repeat(bw.saturating_sub(3 + title_w));
        let mut out = format!(
            "\n{tc}─ {title} {dash}{r}\n",
            title = &block.title,
            r = theme.reset(),
        );

        let content = if block.role == Role::Assistant {
            render_markdown_plain(theme, &block.content)
        } else {
            block.content.clone()
        };

        for raw_line in content.lines() {
            let visible = tui::strip_ansi(raw_line);
            if tui::display_width(&visible) <= inner {
                // Fits — preserve ANSI colour codes from the Markdown renderer
                out.push_str(&format!("  {raw_line}\n"));
            } else {
                // Too wide — wrap plain text (colour lost, but layout correct)
                for line in &tui::wrap_text(&visible, inner) {
                    out.push_str(&format!("  {line}\n"));
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

    /// Render the live streaming indicator as a SINGLE row.
    ///
    /// PRD constraint: while the assistant is streaming, the bottom zone
    /// must NOT grow line-by-line as content arrives — a growing panel
    /// causes the scroll-buffer above it to drift, which the user perceives
    /// as a "jumping" terminal.  Instead we show one in-place status line:
    ///
    /// ```text
    /// ⠹ generating · openai · 5.2s · 1234 chars   queued: 0
    /// ```
    ///
    /// The full assistant content is rendered as a normal block when
    /// `AssistantDone` fires (it scrolls in above the bottom zone).
    fn render_streaming_oneline(
        &self,
        block: &ConversationBlock,
        task: &TaskPanelState,
        term_w: usize,
    ) -> String {
        let theme = &self.theme;
        let elapsed = task.elapsed_ms as f32 / 1000.0;
        let chars = block.content.chars().count();
        let provider = if task.provider.is_empty() {
            "—"
        } else {
            task.provider.as_str()
        };

        // Braille spinner — 10 frames @ 80 ms each.
        const FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
        let frame = FRAMES[((task.elapsed_ms / 80) as usize) % FRAMES.len()];

        let mut left = format!("generating · {provider} · {elapsed:.1}s · {chars} chars");
        let right = if task.queued_count > 0 {
            format!("queued: {}", task.queued_count)
        } else {
            String::new()
        };

        // Total visible budget: term_w − 1 for the magic-margin (some
        // terminals wrap when column == term_w).  Layout is:
        //   <frame> <space> <left> <gap> <right>
        let frame_w = tui::display_width(frame);
        let max = term_w.saturating_sub(1);
        let fixed = frame_w + 1; // frame + one space
        let right_w = tui::display_width(&right);

        // If left+right doesn't fit, truncate left so the line stays one row.
        if fixed + tui::display_width(&left) + right_w > max {
            let avail = max.saturating_sub(fixed + right_w + 1); // +1 buffer
            left = truncate_display(&left, avail).to_string();
        }
        let left_w = tui::display_width(&left);
        let gap = max.saturating_sub(fixed + left_w + right_w);

        format!(
            "{ac}{frame}{r} {dim}{left}{spaces}{right}{r}",
            ac = theme.accent(),
            r = theme.reset(),
            dim = theme.dim(),
            spaces = " ".repeat(gap),
        )
    }

    /// Render the task panel when not streaming (tool execution / reflection).
    fn render_task_panel(&self, task: &TaskPanelState, term_w: usize) -> String {
        let bw = Self::bw(term_w);
        let theme = &self.theme;
        let elapsed = task.elapsed_ms as f32 / 1000.0;

        let header = "─ task ";
        let dash = "─".repeat(bw.saturating_sub(tui::display_width(header)));
        let mut out = format!(
            "\n{wn}{header}{dash}{r}\n",
            wn = theme.warn(),
            r = theme.reset(),
        );

        let running_id = task.running_task_id.as_deref().unwrap_or("(processing)");
        let line1 = format!("running: {running_id} · provider: {}", task.provider);
        let line2 = format!("queued: {}  ·  elapsed: {elapsed:.1}s", task.queued_count);

        out.push_str(&format!("  {dim}{line1}{r}\n", dim = theme.dim(), r = theme.reset()));
        out.push_str(&format!("  {dim}{line2}{r}\n", dim = theme.dim(), r = theme.reset()));
        out.push_str(&format!(
            "{wn}{fill}{r}\n",
            wn = theme.warn(),
            fill = "─".repeat(bw),
            r = theme.reset(),
        ));
        out
    }

    /// Render the bottom input box — open sides, top/bottom rule only.
    ///
    /// Cursor-column offsets are preserved:
    ///   placeholder row: "  ▷ …"  → col 2  (matches old "│ " = 2)
    ///   content row:     "  › …"  → col 4  (matches old "│ › " = 4)
    fn render_input_box(&self, input: &InputState, term_w: usize) -> String {
        let bw = Self::bw(term_w);
        // "  " indent = 2 cols overhead for placeholder
        // "  › " indent = 4 cols overhead for content (keeps cursor_col = 4 + col_in_wrap)
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
    fn render_shortcut_line(&self, term_w: usize) -> String {
        let bw = Self::bw(term_w);
        let hint =
            "Shortcuts: /status  /usage  /help  /clear  /queue  /cancel  /exit  |  Ctrl-C quit";
        let truncated = truncate_display(hint, bw);
        format!(
            "{dim}{truncated}{r}",
            dim = self.theme.dim(),
            r = self.theme.reset()
        )
    }
}

// ── Markdown renderer for finished assistant blocks ───────────────────────────

/// Multi-pass GFM table renderer with column-aligned padding.
/// Separator rows become plain rules; data cells are padded to the widest
/// value in each column so `│` borders stay vertically aligned (CJK-aware).
fn render_table(theme: &Theme, rows: &[String]) -> Vec<String> {
    // Parse: None = separator row, Some(cells) = data row
    let parsed: Vec<Option<Vec<String>>> = rows
        .iter()
        .filter_map(|row| {
            let t = row.trim();
            if t.starts_with('|') && t.ends_with('|') && t.len() > 2 {
                Some(t)
            } else {
                None
            }
        })
        .map(|t| {
            let inner = &t[1..t.len() - 1];
            let is_sep = inner
                .chars()
                .all(|c| c == '-' || c == ':' || c == '|' || c == ' ');
            if is_sep {
                None
            } else {
                Some(inner.split('|').map(|c| c.trim().to_string()).collect())
            }
        })
        .collect();

    // Max display width per column (from plain text, not ANSI-rendered)
    let mut col_widths: Vec<usize> = Vec::new();
    for cells in parsed.iter().flatten() {
        for (ci, cell) in cells.iter().enumerate() {
            let w = tui::display_width(cell);
            if ci >= col_widths.len() {
                col_widths.push(w);
            } else if w > col_widths[ci] {
                col_widths[ci] = w;
            }
        }
    }

    let dim = theme.dim();
    let r = theme.reset();
    let n = col_widths.len();
    let mut out = Vec::new();
    for opt in &parsed {
        match opt {
            None => {
                // Separator: plain horizontal rule aligned to the data row width.
                // Width = sum(col_widths) + 3*n + 1  (matches "│ cell │ cell │" format)
                let rule_w = if n == 0 {
                    32
                } else {
                    col_widths.iter().sum::<usize>() + 3 * n + 1
                };
                out.push(format!("  {dim}{fill}{r}", fill = "─".repeat(rule_w)));
            }
            Some(cells) => {
                let rendered: Vec<String> = cells
                    .iter()
                    .enumerate()
                    .map(|(ci, cell)| {
                        let plain_w = tui::display_width(cell);
                        let col_w = col_widths.get(ci).copied().unwrap_or(plain_w);
                        let pad = " ".repeat(col_w.saturating_sub(plain_w));
                        format!("{}{pad}", render_inline(theme, cell))
                    })
                    .collect();
                let sep = format!(" {dim}│{r} ");
                let row_str = rendered.join(&sep);
                out.push(format!("  {dim}│{r} {row_str} {dim}│{r}"));
            }
        }
    }
    out
}

/// Markdown → ANSI renderer used by both the interactive REPL and one-shot mode.
///
/// Handles: code fences, headings (h1–h6), horizontal rules, blockquotes,
/// nested unordered/ordered lists, GFM tables (column-aligned via `render_table`),
/// and inline formatting (bold, italic, strikethrough, links, inline code).
pub(crate) fn render_markdown_plain(theme: &Theme, text: &str) -> String {
    let mut out = Vec::new();
    let mut in_code = false;
    let mut table_buf: Vec<String> = Vec::new();
    // Suppress protocol tags the agent emits for internal bookkeeping.
    // These must never reach the user's terminal as raw text.
    let mut in_protocol_tag = false;

    for raw_line in text.lines() {
        let line = raw_line.trim_end();
        let trimmed = line.trim();

        // Strip <summary>, <thinking>, <reflection> protocol blocks.
        // Opening and closing tags may appear on the same line or span lines.
        if !in_code {
            if in_protocol_tag {
                if trimmed.contains("</summary>")
                    || trimmed.contains("</thinking>")
                    || trimmed.contains("</reflection>")
                {
                    in_protocol_tag = false;
                }
                continue;
            }
            if trimmed.starts_with("<summary>")
                || trimmed.starts_with("<thinking>")
                || trimmed.starts_with("<reflection>")
            {
                let same_line = trimmed.contains("</summary>")
                    || trimmed.contains("</thinking>")
                    || trimmed.contains("</reflection>");
                if !same_line {
                    in_protocol_tag = true;
                }
                continue;
            }
        }

        // Code fence
        if let Some(lang) = trimmed.strip_prefix("```") {
            if in_code {
                out.push(format!(
                    "{bc}  └─────────────────────────────────{r}",
                    bc = theme.border(),
                    r = theme.reset()
                ));
                in_code = false;
            } else {
                let label = if lang.trim().is_empty() {
                    "code".to_string()
                } else {
                    format!("code: {}", lang.trim())
                };
                out.push(format!(
                    "{bc}  ┌─ {ac}{label}{r} {bc}──────────────────────────────{r}",
                    bc = theme.border(),
                    ac = theme.accent(),
                    r = theme.reset()
                ));
                in_code = true;
            }
            continue;
        }

        if in_code {
            // Flush any buffered table before entering code content
            if !table_buf.is_empty() {
                out.extend(render_table(theme, &table_buf));
                table_buf.clear();
            }
            out.push(format!(
                "{bc}  │{r} {hl}{line}{r}",
                bc = theme.border(),
                hl = theme.highlight(),
                r = theme.reset()
            ));
            continue;
        }

        // Buffer GFM table rows for multi-pass aligned rendering
        if trimmed.starts_with('|') && trimmed.ends_with('|') && trimmed.len() > 2 {
            table_buf.push(raw_line.to_string());
            continue;
        }

        // Non-table line: flush any pending table buffer first
        if !table_buf.is_empty() {
            out.extend(render_table(theme, &table_buf));
            table_buf.clear();
        }

        // Heading
        let depth = trimmed.chars().take_while(|&c| c == '#').count();
        if depth > 0 && depth <= 6 {
            let rest = trimmed[depth..].trim();
            if !rest.is_empty() {
                let heading_text = render_inline(theme, rest);
                out.push(format!(
                    "{bold}{frame}{prefix} {heading_text}{r}",
                    bold = theme.bold(),
                    frame = theme.frame(),
                    prefix = "#".repeat(depth),
                    r = theme.reset()
                ));
                continue;
            }
        }

        // Horizontal rule — all dashes, stars, or underscores (with optional spaces)
        {
            let non_space: Vec<char> = trimmed.chars().filter(|c| !c.is_whitespace()).collect();
            if non_space.len() >= 3
                && non_space
                    .iter()
                    .all(|&c| c == '-' || c == '*' || c == '_')
                && non_space.iter().all(|&c| c == non_space[0])
            {
                out.push(format!(
                    "{dim}────────────────────────────────{r}",
                    dim = theme.dim(),
                    r = theme.reset()
                ));
                continue;
            }
        }

        // Blockquote (supports `> ` prefix; strips leading `>` chars for nesting)
        if trimmed.starts_with('>') {
            let rest = trimmed
                .trim_start_matches('>')
                .trim_start_matches(' ');
            let rendered = render_inline(theme, rest);
            out.push(format!(
                "{dim}│{r} {rendered}",
                dim = theme.dim(),
                r = theme.reset()
            ));
            continue;
        }

        // Leading-space count for nested list indentation (2 spaces = 1 level)
        let indent_spaces = raw_line.chars().take_while(|&c| c == ' ').count();
        let nest = indent_spaces / 2;

        // Unordered list
        if let Some(rest) = trimmed
            .strip_prefix("- ")
            .or_else(|| trimmed.strip_prefix("* "))
        {
            let pad = "  ".repeat(nest);
            let bullet = if nest == 0 { "•" } else { "◦" };
            let rendered = render_inline(theme, rest);
            out.push(format!(
                "{pad}  {ok}{bullet}{r} {rendered}",
                ok = theme.ok(),
                r = theme.reset()
            ));
            continue;
        }

        // Ordered list
        if let Some(dot) = trimmed.find(". ") {
            if dot > 0 && dot <= 3 && trimmed[..dot].chars().all(|c| c.is_ascii_digit()) {
                let num = &trimmed[..dot];
                let rest = &trimmed[dot + 2..];
                let pad = "  ".repeat(nest);
                let rendered = render_inline(theme, rest);
                out.push(format!(
                    "{pad}  {ok}{num}.{r} {rendered}",
                    ok = theme.ok(),
                    r = theme.reset()
                ));
                continue;
            }
        }

        if trimmed.is_empty() {
            out.push(String::new());
        } else {
            out.push(render_inline(theme, trimmed));
        }
    }

    // Flush any table that ends at end-of-text
    if !table_buf.is_empty() {
        out.extend(render_table(theme, &table_buf));
    }

    out.join("\n")
}

/// Parse `[link text](url)` starting at `chars[start]` (which must be `[`).
/// Returns `(display_text, url, end_index)` where `end_index` is one past `)`.
fn parse_inline_link(chars: &[char], start: usize) -> Option<(String, String, usize)> {
    let mut i = start + 1;
    let mut text = String::new();
    let mut depth = 1usize;
    while i < chars.len() {
        match chars[i] {
            '[' => {
                depth += 1;
                text.push('[');
            }
            ']' => {
                depth -= 1;
                if depth == 0 {
                    break;
                }
                text.push(']');
            }
            ch => text.push(ch),
        }
        i += 1;
    }
    if i >= chars.len() {
        return None;
    }
    // Expect `(` immediately after `]`
    if i + 1 >= chars.len() || chars[i + 1] != '(' {
        return None;
    }
    i += 2; // skip `](`
    let mut url = String::new();
    while i < chars.len() && chars[i] != ')' {
        url.push(chars[i]);
        i += 1;
    }
    if i >= chars.len() {
        return None;
    }
    // Strip optional title attribute: `url "title"` → keep only the URL token
    let url_clean = url
        .split(' ')
        .next()
        .unwrap_or(url.as_str())
        .to_string();
    Some((text, url_clean, i + 1))
}

fn render_inline(theme: &Theme, text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let n = chars.len();
    let mut out = String::new();
    let mut i = 0;
    let mut in_code = false;
    let mut in_bold = false;
    let mut in_italic = false;
    let mut in_strike = false;

    while i < n {
        let ch = chars[i];

        // Inline code: `...`
        if ch == '`' {
            out.push_str(if in_code {
                theme.reset()
            } else {
                theme.highlight()
            });
            in_code = !in_code;
            i += 1;
            continue;
        }

        if in_code {
            out.push(ch);
            i += 1;
            continue;
        }

        // Link: [text](url)
        if ch == '[' {
            if let Some((link_text, url, end)) = parse_inline_link(&chars, i) {
                out.push_str(theme.info());
                out.push_str(&link_text);
                out.push_str(theme.reset());
                out.push_str(theme.dim());
                out.push_str(&format!(" ({url})"));
                out.push_str(theme.reset());
                i = end;
                continue;
            }
        }

        // Strikethrough: ~~...~~
        if ch == '~' && i + 1 < n && chars[i + 1] == '~' {
            out.push_str(if in_strike {
                theme.reset()
            } else {
                theme.strikethrough()
            });
            in_strike = !in_strike;
            i += 2;
            continue;
        }

        // Bold: **...**  (check before single-* italic)
        if ch == '*' && i + 1 < n && chars[i + 1] == '*' {
            out.push_str(if in_bold {
                theme.reset()
            } else {
                theme.bold()
            });
            in_bold = !in_bold;
            i += 2;
            continue;
        }

        // Italic: *...* (single asterisk)
        if ch == '*' {
            out.push_str(if in_italic {
                theme.reset()
            } else {
                theme.italic()
            });
            in_italic = !in_italic;
            i += 1;
            continue;
        }

        // Italic: _..._ — only at non-identifier boundaries to avoid `snake_case` false positives
        if ch == '_' {
            let prev_word = i > 0 && (chars[i - 1].is_alphanumeric() || chars[i - 1] == '_');
            let next_word =
                i + 1 < n && (chars[i + 1].is_alphanumeric() || chars[i + 1] == '_');
            let is_boundary = if in_italic {
                !next_word
            } else {
                !prev_word && !next_word
            };
            if is_boundary {
                out.push_str(if in_italic {
                    theme.reset()
                } else {
                    theme.italic()
                });
                in_italic = !in_italic;
                i += 1;
                continue;
            }
        }

        out.push(ch);
        i += 1;
    }

    // Close any unclosed spans
    if in_code || in_bold || in_italic || in_strike {
        out.push_str(theme.reset());
    }
    out
}

// ── Input task (sync, called from spawn_blocking) ────────────────────────────

/// Runs inside `tokio::task::spawn_blocking`. Reads raw terminal events and
/// sends [`UiEvent`]s via `blocking_send`.
///
/// Supported: printable characters, Backspace/Delete, ←/→/↑/↓ navigation,
/// Home/End, Ctrl-A/E/U/K/W/C/D.
///
/// `paused` is set to `true` by the main loop before running a slash command
/// that needs exclusive stdin access (e.g. `/login`). The task stops calling
/// `crossterm::event::read()` while paused so it does not race with `read_line`.
#[allow(clippy::collapsible_if)]
pub fn run_input_task_sync(
    tx: mpsc::Sender<UiEvent>,
    history_path: PathBuf,
    paused: Arc<AtomicBool>,
) {
    use crossterm::event::{Event, KeyCode, KeyModifiers};
    use std::time::Duration;

    let mut history: VecDeque<String> = load_history(&history_path);
    let mut input = String::new();
    let mut cursor_char = 0usize;
    let mut hist_idx = history.len();
    let mut hist_snapshot = String::new();

    loop {
        // Yield the stdin fd to the main loop while a slash command runs.
        if paused.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(20));
            continue;
        }

        // Non-blocking poll so the pause flag is checked regularly.
        let event = match crossterm::event::poll(Duration::from_millis(50)) {
            Ok(true) => match crossterm::event::read() {
                Ok(e) => e,
                Err(_) => break,
            },
            Ok(false) => continue,
            Err(_) => break,
        };

        match event {
            Event::Resize(w, _) => {
                let _ = tx.blocking_send(UiEvent::Resize { width: w });
                continue;
            }
            Event::Key(key) => {
                let mods = key.modifiers;
                match key.code {
                    // ── Submit ──────────────────────────────────────────────
                    KeyCode::Enter => {
                        let content = input.trim().to_string();
                        if content.is_empty() {
                            continue;
                        }
                        if history.back().map(|s| s.as_str()) != Some(content.as_str()) {
                            history.push_back(content.clone());
                            if history.len() > 1000 {
                                history.pop_front();
                            }
                            save_history(&history_path, &history);
                        }
                        hist_idx = history.len();
                        input.clear();
                        cursor_char = 0;

                        let ts = chrono::Local::now().format("%H:%M:%S").to_string();
                        let task_id =
                            format!("task-{}", chrono::Utc::now().format("%Y%m%dT%H%M%S%.3fZ"));
                        if tx
                            .blocking_send(UiEvent::InputSubmitted {
                                task_id,
                                content,
                                timestamp: ts,
                            })
                            .is_err()
                        {
                            break;
                        }
                        let _ = tx.blocking_send(UiEvent::InputChanged {
                            content: String::new(),
                            cursor_char: 0,
                        });
                    }

                    // ── Exit ────────────────────────────────────────────────
                    KeyCode::Char('c') if mods.contains(KeyModifiers::CONTROL) => {
                        let _ = tx.blocking_send(UiEvent::Shutdown);
                        break;
                    }
                    KeyCode::Char('d') if mods.contains(KeyModifiers::CONTROL) => {
                        let _ = tx.blocking_send(UiEvent::Shutdown);
                        break;
                    }

                    // ── Ctrl line-editing ────────────────────────────────────
                    KeyCode::Char('a') if mods.contains(KeyModifiers::CONTROL) => {
                        cursor_char = 0;
                        let _ = tx.blocking_send(UiEvent::InputChanged {
                            content: input.clone(),
                            cursor_char,
                        });
                    }
                    KeyCode::Char('e') if mods.contains(KeyModifiers::CONTROL) => {
                        cursor_char = input.chars().count();
                        let _ = tx.blocking_send(UiEvent::InputChanged {
                            content: input.clone(),
                            cursor_char,
                        });
                    }
                    KeyCode::Char('u') if mods.contains(KeyModifiers::CONTROL) => {
                        input.drain(..char_to_byte(&input, cursor_char));
                        cursor_char = 0;
                        let _ = tx.blocking_send(UiEvent::InputChanged {
                            content: input.clone(),
                            cursor_char,
                        });
                    }
                    KeyCode::Char('k') if mods.contains(KeyModifiers::CONTROL) => {
                        input.truncate(char_to_byte(&input, cursor_char));
                        let _ = tx.blocking_send(UiEvent::InputChanged {
                            content: input.clone(),
                            cursor_char,
                        });
                    }
                    KeyCode::Char('w') if mods.contains(KeyModifiers::CONTROL) => {
                        let end = char_to_byte(&input, cursor_char);
                        let trimmed = input[..end].trim_end();
                        let word_start = trimmed
                            .rfind(|c: char| c.is_whitespace())
                            .map(|i| i + 1)
                            .unwrap_or(0);
                        input.replace_range(word_start..end, "");
                        cursor_char = input[..word_start].chars().count();
                        let _ = tx.blocking_send(UiEvent::InputChanged {
                            content: input.clone(),
                            cursor_char,
                        });
                    }

                    // ── Printable characters ─────────────────────────────────
                    KeyCode::Char(c)
                        if mods == KeyModifiers::NONE || mods == KeyModifiers::SHIFT =>
                    {
                        let byte_pos = char_to_byte(&input, cursor_char);
                        input.insert(byte_pos, c);
                        cursor_char += 1;
                        let _ = tx.blocking_send(UiEvent::InputChanged {
                            content: input.clone(),
                            cursor_char,
                        });
                    }

                    // ── Editing ──────────────────────────────────────────────
                    KeyCode::Backspace if cursor_char > 0 => {
                        cursor_char -= 1;
                        input.remove(char_to_byte(&input, cursor_char));
                        let _ = tx.blocking_send(UiEvent::InputChanged {
                            content: input.clone(),
                            cursor_char,
                        });
                    }
                    KeyCode::Delete if cursor_char < input.chars().count() => {
                        input.remove(char_to_byte(&input, cursor_char));
                        let _ = tx.blocking_send(UiEvent::InputChanged {
                            content: input.clone(),
                            cursor_char,
                        });
                    }

                    // ── Cursor movement ──────────────────────────────────────
                    KeyCode::Left if cursor_char > 0 => {
                        cursor_char -= 1;
                        let _ = tx.blocking_send(UiEvent::InputChanged {
                            content: input.clone(),
                            cursor_char,
                        });
                    }
                    KeyCode::Right if cursor_char < input.chars().count() => {
                        cursor_char += 1;
                        let _ = tx.blocking_send(UiEvent::InputChanged {
                            content: input.clone(),
                            cursor_char,
                        });
                    }
                    KeyCode::Home => {
                        cursor_char = 0;
                        let _ = tx.blocking_send(UiEvent::InputChanged {
                            content: input.clone(),
                            cursor_char,
                        });
                    }
                    KeyCode::End => {
                        cursor_char = input.chars().count();
                        let _ = tx.blocking_send(UiEvent::InputChanged {
                            content: input.clone(),
                            cursor_char,
                        });
                    }

                    // ── History ──────────────────────────────────────────────
                    KeyCode::Up => {
                        if hist_idx == history.len() {
                            hist_snapshot.clone_from(&input);
                        }
                        if hist_idx > 0 {
                            hist_idx -= 1;
                            input.clone_from(&history[hist_idx]);
                            cursor_char = input.chars().count();
                            let _ = tx.blocking_send(UiEvent::InputChanged {
                                content: input.clone(),
                                cursor_char,
                            });
                        }
                    }
                    KeyCode::Down if hist_idx < history.len() => {
                        hist_idx += 1;
                        input = if hist_idx == history.len() {
                            hist_snapshot.clone()
                        } else {
                            history[hist_idx].clone()
                        };
                        cursor_char = input.chars().count();
                        let _ = tx.blocking_send(UiEvent::InputChanged {
                            content: input.clone(),
                            cursor_char,
                        });
                    }

                    _ => {}
                }
            }
            _ => {}
        }
    }
}

// ── History file helpers ──────────────────────────────────────────────────────

pub fn load_history(path: &Path) -> VecDeque<String> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| l.to_string())
        .collect()
}

pub fn save_history(path: &Path, history: &VecDeque<String>) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(path, history.iter().cloned().collect::<Vec<_>>().join("\n"));
}

// ── Internal utilities ────────────────────────────────────────────────────────

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
