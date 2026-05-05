use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

use tokio::sync::mpsc;

use super::event::UiEvent;

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
    // Tab completion state
    let mut tab_active = false;
    let mut tab_candidates: Vec<String> = Vec::new();
    let mut tab_idx = 0usize;
    // Ctrl-R reverse-search state
    let mut rev_in_progress = false;
    let mut rev_upper = 0usize;
    let mut rev_query = String::new();

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
                // Reset completion / reverse-search state on keys that end those modes.
                let is_tab = key.code == KeyCode::Tab;
                let is_ctrl_r =
                    key.code == KeyCode::Char('r') && mods.contains(KeyModifiers::CONTROL);
                if !is_tab {
                    tab_active = false;
                }
                if !is_ctrl_r {
                    rev_in_progress = false;
                }
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

                    // ── Tab completion ───────────────────────────────────────
                    KeyCode::Tab => {
                        if !tab_active {
                            tab_candidates = compute_completions(&input);
                            tab_active = !tab_candidates.is_empty();
                            tab_idx = 0;
                        } else {
                            tab_idx = (tab_idx + 1) % tab_candidates.len();
                        }
                        if let Some(completion) = tab_candidates.get(tab_idx) {
                            input = completion.clone();
                            cursor_char = input.chars().count();
                            let _ = tx.blocking_send(UiEvent::InputChanged {
                                content: input.clone(),
                                cursor_char,
                            });
                        }
                    }

                    // ── Ctrl-R reverse search ────────────────────────────────
                    KeyCode::Char('r') if mods.contains(KeyModifiers::CONTROL) => {
                        if !rev_in_progress {
                            rev_in_progress = true;
                            rev_upper = history.len();
                            rev_query.clone_from(&input);
                        }
                        let found = (0..rev_upper)
                            .rev()
                            .find(|&i| history[i].contains(rev_query.as_str()));
                        if let Some(idx) = found {
                            rev_upper = idx;
                            input.clone_from(&history[idx]);
                            cursor_char = input.chars().count();
                            let _ = tx.blocking_send(UiEvent::InputChanged {
                                content: input.clone(),
                                cursor_char,
                            });
                        }
                    }

                    // ── Ctrl-P / Ctrl-N history ──────────────────────────────
                    KeyCode::Char('p') if mods.contains(KeyModifiers::CONTROL) => {
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
                    KeyCode::Char('n')
                        if mods.contains(KeyModifiers::CONTROL)
                            && hist_idx < history.len() =>
                    {
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

/// Return slash-command completions whose name starts with `input`.
/// Returns an empty vec when `input` does not start with `/`.
fn compute_completions(input: &str) -> Vec<String> {
    if !input.starts_with('/') {
        return Vec::new();
    }
    super::state::SLASH_CMDS
        .iter()
        .filter(|&&c| c.starts_with(input))
        .map(|&c| c.to_string())
        .collect()
}

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

/// Convert a char-index to a byte offset in `s`.
fn char_to_byte(s: &str, char_idx: usize) -> usize {
    s.char_indices()
        .nth(char_idx)
        .map(|(i, _)| i)
        .unwrap_or(s.len())
}
