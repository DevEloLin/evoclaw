use std::time::Instant;

use crossterm::terminal;

use super::event::UiEvent;

/// All slash commands supported by the REPL — used for Tab completion.
pub const SLASH_CMDS: &[&str] = &[
    "/agent", "/channel", "/clear", "/closure", "/config",
    "/doctor", "/exit", "/help", "/login", "/logout",
    "/mcp", "/memory", "/model", "/profile", "/quit",
    "/replay", "/secret", "/skill", "/status", "/tokens",
    "/usage",
];

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
    /// Slash-command candidates matching the current input prefix, for Tab completion hint.
    pub completions: Vec<String>,
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
    pub usage: Option<String>,
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
            usage: None,
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
                self.input.completions = if content.starts_with('/') {
                    SLASH_CMDS
                        .iter()
                        .filter(|&&c| c.starts_with(content.as_str()))
                        .map(|&c| c.to_string())
                        .collect()
                } else {
                    Vec::new()
                };
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
                self.usage = Some(usage_summary.clone());
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
