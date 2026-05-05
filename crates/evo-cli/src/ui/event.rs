/// Every observable UI change — input editing, task lifecycle, streaming
/// deltas, resize — arrives as a `UiEvent`.
#[derive(Debug)]
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
    /// The agent called `ask_user` — TUI event loop must collect the answer
    /// and send it back via `resp_tx` (raw mode is temporarily disabled).
    AskUser {
        prompt: String,
        resp_tx: tokio::sync::oneshot::Sender<String>,
    },
    /// Input task exited (Ctrl-C or Ctrl-D).
    Shutdown,
}
