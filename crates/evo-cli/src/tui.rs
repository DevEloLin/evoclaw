//! Terminal UI utilities: size detection, unicode display width, text wrapping.
//!
//! These helpers are free of business logic and can be reused from both the
//! REPL render path and any future TUI layer.

use unicode_width::UnicodeWidthChar;

// ── Terminal size ─────────────────────────────────────────────────────────────

/// Returns `(cols, rows)` via crossterm. Falls back to `(80, 24)` on failure.
pub fn terminal_size() -> (usize, usize) {
    crossterm::terminal::size()
        .map(|(c, r)| (c as usize, r as usize))
        .unwrap_or((80, 24))
}

/// Terminal column count, clamped to `[60, 220]`.
pub fn terminal_width() -> usize {
    terminal_size().0.clamp(60, 220)
}

// ── Unicode display width ─────────────────────────────────────────────────────

/// Display width of `s` in terminal columns.
///
/// CJK / full-width characters count as 2 columns; ASCII as 1; control
/// characters as 0.  Emoji width follows the Unicode standard (usually 2).
pub fn display_width(s: &str) -> usize {
    s.chars()
        .map(|c| UnicodeWidthChar::width(c).unwrap_or(0))
        .sum()
}

/// Display width of `s`, ignoring embedded ANSI SGR escape sequences.
pub fn display_width_ansi(s: &str) -> usize {
    display_width(&strip_ansi(s))
}

/// Remove ANSI SGR escape sequences (e.g. `\x1b[1m`, `\x1b[0m`) from `s`.
pub fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_esc = false;
    for ch in s.chars() {
        match (in_esc, ch) {
            (false, '\x1b') => in_esc = true,
            (true, 'm') => in_esc = false,
            (true, _) => {}
            (false, c) => out.push(c),
        }
    }
    out
}

// ── Text wrapping ─────────────────────────────────────────────────────────────

/// Wrap `text` so each output line fits within `max_width` terminal columns.
///
/// * Splits at word boundaries when possible.
/// * If a single word exceeds `max_width`, breaks at the character boundary —
///   required for long CJK strings and URLs.
/// * Preserves explicit `\n` newlines in the input.
///
/// Returns one `String` per output line (no trailing `\n`).
pub fn wrap_text(text: &str, max_width: usize) -> Vec<String> {
    if max_width < 4 {
        return vec![text.to_string()];
    }

    let mut lines: Vec<String> = Vec::new();

    for paragraph in text.split('\n') {
        let mut line = String::new();
        let mut line_w: usize = 0;

        for word in paragraph.split_whitespace() {
            let word_w = display_width(word);

            // Word alone exceeds max_width → break at character level
            if word_w > max_width {
                if !line.is_empty() {
                    lines.push(std::mem::take(&mut line));
                    line_w = 0;
                }
                let mut partial = String::new();
                let mut partial_w = 0usize;
                for ch in word.chars() {
                    let cw = UnicodeWidthChar::width(ch).unwrap_or(0);
                    if partial_w + cw > max_width {
                        lines.push(std::mem::take(&mut partial));
                        partial_w = 0;
                    }
                    partial.push(ch);
                    partial_w += cw;
                }
                if !partial.is_empty() {
                    line = partial;
                    line_w = partial_w;
                }
                continue;
            }

            // Normal word: append if it fits, otherwise start a new line
            if line.is_empty() {
                line.push_str(word);
                line_w = word_w;
            } else if line_w + 1 + word_w <= max_width {
                line.push(' ');
                line.push_str(word);
                line_w += 1 + word_w;
            } else {
                lines.push(std::mem::take(&mut line));
                line.push_str(word);
                line_w = word_w;
            }
        }
        lines.push(line);
    }

    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_display_width() {
        assert_eq!(display_width("hello"), 5);
    }

    #[test]
    fn cjk_display_width() {
        assert_eq!(display_width("你好"), 4);
    }

    #[test]
    fn mixed_display_width() {
        assert_eq!(display_width("hi你好"), 6);
    }

    #[test]
    fn strip_ansi_basic() {
        assert_eq!(strip_ansi("\x1b[1mhello\x1b[0m"), "hello");
    }

    #[test]
    fn wrap_english_word_boundary() {
        let lines = wrap_text("hello world foo bar", 11);
        for l in &lines {
            assert!(display_width(l) <= 11, "line too wide: {l:?}");
        }
    }

    #[test]
    fn wrap_cjk_chars() {
        let lines = wrap_text("你好世界朋友啊", 10);
        for l in &lines {
            assert!(display_width(l) <= 10, "cjk line too wide: {l:?}");
        }
    }

    #[test]
    fn wrap_preserves_newlines() {
        let lines = wrap_text("a\nb", 80);
        assert_eq!(lines, vec!["a", "b"]);
    }

    #[test]
    fn wrap_empty() {
        let lines = wrap_text("", 80);
        assert_eq!(lines.len(), 1);
    }
}
