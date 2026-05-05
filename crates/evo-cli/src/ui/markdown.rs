use crate::tui;
use crate::Theme;

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
pub(crate) fn render_markdown_plain(theme: &Theme, text: &str, term_w: usize) -> String {
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
            let rule = "─".repeat(term_w.min(60));
            if in_code {
                out.push(format!(
                    "{bc}{rule}{r}",
                    bc = theme.border(),
                    r = theme.reset()
                ));
                in_code = false;
            } else {
                let label = if lang.trim().is_empty() {
                    String::new()
                } else {
                    format!("code: {}", lang.trim())
                };
                if !label.is_empty() {
                    out.push(format!(
                        "{ac}{label}{r}",
                        ac = theme.accent(),
                        r = theme.reset()
                    ));
                }
                out.push(format!(
                    "{bc}{rule}{r}",
                    bc = theme.border(),
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
                "  {hl}{line}{r}",
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
                let plain_w = tui::display_width(rest).min(term_w.min(60));
                let underline = if depth <= 2 {
                    "─".repeat(plain_w)
                } else {
                    String::new()
                };
                out.push(format!(
                    "{bold}{frame}{heading_text}{r}",
                    bold = theme.bold(),
                    frame = theme.frame(),
                    r = theme.reset()
                ));
                if !underline.is_empty() {
                    out.push(format!(
                        "{dim}{underline}{r}",
                        dim = theme.dim(),
                        r = theme.reset()
                    ));
                }
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
                    "{dim}{rule}{r}",
                    dim = theme.dim(),
                    rule = "─".repeat(term_w.min(60)),
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
                "{dim}  {rendered}{r}",
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
