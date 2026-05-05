//! Theme, color palette, display utilities, and text helpers.

use crate::tui;

// ---------------------------------------------------------------------------
// Color detection
// ---------------------------------------------------------------------------

pub(crate) fn use_color() -> bool {
    if std::env::var("NO_COLOR").is_ok() {
        return false;
    }
    if std::env::var("EVO_NO_COLOR").is_ok() {
        return false;
    }
    use std::io::IsTerminal;
    std::io::stdout().is_terminal()
}

// ---------------------------------------------------------------------------
// Color definitions — centralized 256-color palette
// ---------------------------------------------------------------------------

pub(crate) mod colors {
    // Primary colors - soft teal/cyan for tech aesthetic
    pub const PRIMARY: &str = "\x1b[38;5;51m"; // Soft cyan

    // Status colors - soft and professional
    pub const SUCCESS: &str = "\x1b[38;5;120m"; // Soft green
    pub const ERROR: &str = "\x1b[38;5;204m"; // Soft red
    pub const WARNING: &str = "\x1b[38;5;222m"; // Amber/orange
    pub const INFO: &str = "\x1b[38;5;117m"; // Soft blue

    // Accent colors
    pub const ACCENT: &str = "\x1b[38;5;141m"; // Soft purple
    pub const HIGHLIGHT: &str = "\x1b[38;5;228m"; // Soft yellow

    // Text styles
    pub const DIM: &str = "\x1b[38;5;240m"; // Gray for secondary info
    pub const BOLD: &str = "\x1b[1m"; // Bold
    pub const ITALIC: &str = "\x1b[3m"; // Italic
    pub const STRIKETHROUGH: &str = "\x1b[9m"; // Strikethrough
    pub const RESET: &str = "\x1b[0m"; // Reset all

    // Semantic colors for different use cases
    pub const LABEL: &str = "\x1b[38;5;249m"; // Light gray for labels
    pub const VALUE: &str = "\x1b[38;5;253m"; // Bright white for values
    pub const BORDER: &str = "\x1b[38;5;240m"; // Gray for borders
}

// ---------------------------------------------------------------------------
// Theme
// ---------------------------------------------------------------------------

/// Centralised colour palette with modern, tech-aesthetic colors.
/// Provides soft, professional colors that are easy on the eyes while maintaining
/// a high-tech feel. All colors are centralized and never hardcoded.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Theme {
    pub(crate) enabled: bool,
}

impl Theme {
    pub(crate) fn detect() -> Self {
        Self {
            enabled: use_color(),
        }
    }

    fn s(&self, code: &'static str) -> &'static str {
        if self.enabled {
            code
        } else {
            ""
        }
    }

    pub(crate) fn reset(&self) -> &'static str {
        self.s(colors::RESET)
    }

    /// Primary cyan — used for prompts, banners, and primary UI elements
    pub(crate) fn frame(&self) -> &'static str {
        self.s(colors::PRIMARY)
    }

    /// Soft green — success markers and positive feedback
    pub(crate) fn ok(&self) -> &'static str {
        self.s(colors::SUCCESS)
    }

    /// Soft red — error messages
    pub(crate) fn err(&self) -> &'static str {
        self.s(colors::ERROR)
    }

    /// Amber/orange — warnings and spinner
    pub(crate) fn warn(&self) -> &'static str {
        self.s(colors::WARNING)
    }

    /// Soft blue — informational messages
    pub(crate) fn info(&self) -> &'static str {
        self.s(colors::INFO)
    }

    /// Soft purple — system notices and accents
    pub(crate) fn accent(&self) -> &'static str {
        self.s(colors::ACCENT)
    }

    /// Soft yellow — highlights
    pub(crate) fn highlight(&self) -> &'static str {
        self.s(colors::HIGHLIGHT)
    }

    /// Gray — secondary metadata (paths, timing, etc.)
    pub(crate) fn dim(&self) -> &'static str {
        self.s(colors::DIM)
    }

    /// Bold — headings and emphasis
    pub(crate) fn bold(&self) -> &'static str {
        self.s(colors::BOLD)
    }

    /// Italic — emphasis in Markdown
    pub(crate) fn italic(&self) -> &'static str {
        self.s(colors::ITALIC)
    }

    /// Strikethrough — ~~deleted~~ text in Markdown
    pub(crate) fn strikethrough(&self) -> &'static str {
        self.s(colors::STRIKETHROUGH)
    }

    /// Light gray — for labels in key-value displays
    pub(crate) fn label(&self) -> &'static str {
        self.s(colors::LABEL)
    }

    /// Bright white — for values in key-value displays
    pub(crate) fn value(&self) -> &'static str {
        self.s(colors::VALUE)
    }

    /// Gray — for borders and separators
    pub(crate) fn border(&self) -> &'static str {
        self.s(colors::BORDER)
    }
}

// ---------------------------------------------------------------------------
// DisplayTemplate
// ---------------------------------------------------------------------------

/// Template-based display utilities for consistent formatting
pub(crate) struct DisplayTemplate;

impl DisplayTemplate {
    /// Format a key-value pair with consistent styling
    pub(crate) fn kv(theme: &Theme, key: &str, value: &str) -> String {
        format!(
            "  {label}{key:.<18}{reset} {value_color}{value}{reset}",
            label = theme.label(),
            key = key,
            reset = theme.reset(),
            value_color = theme.value(),
            value = value
        )
    }

    /// Format a key-value pair with custom value color
    pub(crate) fn kv_colored(theme: &Theme, key: &str, value: &str, color: &str) -> String {
        format!(
            "  {label}{key:.<18}{reset} {color}{value}{reset}",
            label = theme.label(),
            key = key,
            reset = theme.reset(),
            color = color,
            value = value
        )
    }

    /// Format a section header — horizontal rule with title, no side borders.
    /// Uses `accent` (purple) to match the paired footer.
    pub(crate) fn header(theme: &Theme, title: &str) -> String {
        let plain_title = format!("─ {title} ");
        let fill = "─".repeat(64usize.saturating_sub(tui::display_width(&plain_title)));
        format!(
            "\n{ac}─ {bold}{title}{reset}{ac} {fill}{reset}",
            ac = theme.accent(),
            bold = theme.bold(),
            title = title,
            reset = theme.reset(),
            fill = fill,
        )
    }

    /// Format a section footer — bottom horizontal rule only, no side borders.
    /// Uses `accent` (purple) to match the paired header.
    pub(crate) fn footer(theme: &Theme) -> String {
        format!(
            "{ac}{}{reset}",
            "─".repeat(64),
            ac = theme.accent(),
            reset = theme.reset()
        )
    }
}

// ---------------------------------------------------------------------------
// Text helpers
// ---------------------------------------------------------------------------

pub(crate) fn display_home(p: &str) -> String {
    if let Ok(home) = std::env::var("HOME") {
        if let Some(rest) = p.strip_prefix(&home) {
            return format!("~{rest}");
        }
    }
    p.to_string()
}

pub(crate) fn truncate_to(s: &str, n: usize) -> String {
    if tui::display_width(s) <= n {
        return s.to_string();
    }
    let mut out = String::new();
    let mut used = 0usize;
    let limit = n.saturating_sub(1);
    for ch in s.chars() {
        let cw = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if used + cw > limit {
            break;
        }
        out.push(ch);
        used += cw;
    }
    out.push('…');
    out
}

/// Trim the `KeySource` description so the banner row never overflows. We only
/// keep the short tail of long secret-file paths.
pub(crate) fn short_key_source(s: &str) -> String {
    if let Some(rest) = s.strip_prefix("secrets file: ") {
        if let Ok(home) = std::env::var("HOME") {
            if let Some(tail) = rest.strip_prefix(&home) {
                return format!("secrets file: ~{tail}");
            }
        }
        if rest.len() > 32 {
            return format!("secrets file: …{}", &rest[rest.len() - 30..]);
        }
        return format!("secrets file: {rest}");
    }
    s.to_string()
}
