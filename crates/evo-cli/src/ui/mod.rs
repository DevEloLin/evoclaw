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

pub mod event;
pub mod input;
pub mod markdown;
pub mod renderer;
pub mod state;

pub use event::UiEvent;
pub use input::{load_history, run_input_task_sync, save_history};
pub(crate) use markdown::render_markdown_plain;
pub use renderer::UiRenderer;
pub use state::{BlockStatus, ConversationBlock, InputState, Role, TaskPanelState, UiState};
