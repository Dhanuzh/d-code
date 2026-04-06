/// Terminal renderer: streaming markdown with inline formatting + tool display.
use crossterm::cursor::MoveToColumn;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use crossterm::style::{
    Attribute, Color, ResetColor, SetAttribute, SetBackgroundColor, SetForegroundColor,
};
use crossterm::terminal::{self, Clear, ClearType};
use crossterm::{execute, style::Print};
use std::io::{stdout, Write};

// ─── Stateful XML filter ──────────────────────────────────────────────────────

pub struct XmlFilter {
    pending: String,
    in_xml: bool,
    close_tag: String,
}

impl XmlFilter {
    pub fn new() -> Self {
        Self {
            pending: String::new(),
            in_xml: false,
            close_tag: String::new(),
        }
    }

    pub fn push(&mut self, delta: &str) -> String {
        let mut out = String::new();
        for ch in delta.chars() {
            if self.in_xml {
                self.pending.push(ch);
                if self.pending.ends_with(&self.close_tag) {
                    self.in_xml = false;
                    self.close_tag.clear();
                    self.pending.clear();
                }
            } else {
                self.pending.push(ch);
                if ch == '>' {
                    if let Some(tag) = parse_open_tag(&self.pending) {
                        self.in_xml = true;
                        self.close_tag = format!("</{}>", tag);
                        self.pending.clear();
                    } else {
                        out.push_str(&self.pending);
                        self.pending.clear();
                    }
                } else if ch == '<' && self.pending.len() > 1 {
                    let prev = self.pending[..self.pending.len() - 1].to_string();
                    out.push_str(&prev);
                    self.pending = "<".to_string();
                }
            }
        }
        out
    }

    pub fn flush(&mut self) -> String {
        if !self.in_xml {
            let s = self.pending.clone();
            self.pending.clear();
            s
        } else {
            String::new()
        }
    }
}

fn parse_open_tag(s: &str) -> Option<&str> {
    let s = s.trim();
    if !s.starts_with('<') || !s.ends_with('>') {
        return None;
    }
    let inner = &s[1..s.len() - 1];
    if inner.is_empty() || inner.starts_with('/') || inner.contains(' ') {
        return None;
    }
    if inner
        .chars()
        .all(|c| c.is_ascii_lowercase() || c == '_' || c == '-')
    {
        Some(inner)
    } else {
        None
    }
}

// ─── Markdown renderer ────────────────────────────────────────────────────────

/// Streaming markdown renderer with typewriter effect.
///
/// Each incoming text delta is printed immediately to the terminal (partial
/// line), giving a token-by-token typewriter feel. When a newline arrives the
/// partial content is cleared and the complete line is re-rendered with full
/// markdown formatting (headers, bullets, code blocks, inline bold/code, etc.).
pub struct MarkdownRenderer {
    /// Incomplete current line accumulating tokens.
    line_buf: String,
    in_code_block: bool,
    /// True when partial line content has already been written to the terminal.
    /// Used to know whether to MoveToColumn(0)+Clear before reprinting.
    has_partial: bool,
}

impl MarkdownRenderer {
    pub fn new() -> Self {
        Self {
            line_buf: String::new(),
            in_code_block: false,
            has_partial: false,
        }
    }

    /// Feed a text delta.
    /// Immediately renders partial content for typewriter effect; re-renders
    /// with full markdown formatting when each line completes.
    pub fn push(&mut self, text: &str) {
        self.line_buf.push_str(text);
        let mut out = stdout();

        // Process every complete line (ends with \n).
        while let Some(nl) = self.line_buf.find('\n') {
            let line = self.line_buf[..nl].to_string();
            self.line_buf.drain(..nl + 1);

            // Clear the partial-line preview we printed earlier.
            if self.has_partial {
                let _ = execute!(out, MoveToColumn(0), Clear(ClearType::CurrentLine));
                self.has_partial = false;
            }

            self.render_line(&line, &mut out);
            let _ = execute!(out, Print("\n"));
        }

        // Typewriter: immediately print whatever partial text is buffered.
        if !self.line_buf.is_empty() {
            if self.has_partial {
                // Overwrite from the start of the current line.
                let _ = execute!(out, MoveToColumn(0), Clear(ClearType::CurrentLine));
            }
            self.render_partial(&self.line_buf.clone(), &mut out);
            self.has_partial = true;
        }

        let _ = out.flush();
    }

    /// Flush any remaining buffered text at end of turn (no trailing newline).
    pub fn flush(&mut self) {
        if !self.line_buf.is_empty() {
            let line = std::mem::take(&mut self.line_buf);
            let mut out = stdout();
            if self.has_partial {
                let _ = execute!(out, MoveToColumn(0), Clear(ClearType::CurrentLine));
            }
            self.has_partial = false;
            self.render_line(&line, &mut out);
            let _ = out.flush();
        } else {
            self.has_partial = false;
        }
    }

    /// Render a partial (incomplete) line — no block-level detection, just
    /// inline formatting so the typewriter text looks styled as it arrives.
    fn render_partial(&self, text: &str, out: &mut impl Write) {
        if self.in_code_block {
            let _ = execute!(
                out,
                SetForegroundColor(Color::Rgb { r: 180, g: 220, b: 180 }),
                Print(format!("  │ {text}")),
                ResetColor,
            );
        } else {
            // Indent to match completed-line rendering, then inline-format.
            let _ = execute!(out, Print("  "));
            render_inline(text, out);
        }
    }

    fn render_line(&mut self, line: &str, out: &mut impl Write) {
        // ── Code fence ───────────────────────────────────────────────────────
        if line.trim_start().starts_with("```") {
            self.in_code_block = !self.in_code_block;
            if self.in_code_block {
                let lang = line.trim_start()[3..].trim();
                let _ = execute!(
                    out,
                    SetForegroundColor(Color::Rgb {
                        r: 100,
                        g: 110,
                        b: 130
                    }),
                    SetAttribute(Attribute::Dim),
                    Print(format!(
                        "  ╭─ {}",
                        if lang.is_empty() { "code" } else { lang }
                    )),
                    ResetColor,
                    SetAttribute(Attribute::Reset),
                );
            } else {
                let _ = execute!(
                    out,
                    SetForegroundColor(Color::Rgb {
                        r: 100,
                        g: 110,
                        b: 130
                    }),
                    SetAttribute(Attribute::Dim),
                    Print("  ╰─"),
                    ResetColor,
                    SetAttribute(Attribute::Reset),
                );
            }
            return;
        }

        // ── Inside code block ─────────────────────────────────────────────────
        if self.in_code_block {
            let _ = execute!(
                out,
                SetForegroundColor(Color::Rgb {
                    r: 180,
                    g: 220,
                    b: 180
                }),
                Print(format!("  │ {}", line)),
                ResetColor,
            );
            return;
        }

        // ── Headers ──────────────────────────────────────────────────────────
        if let Some(rest) = line.strip_prefix("### ") {
            let _ = execute!(
                out,
                SetAttribute(Attribute::Bold),
                SetForegroundColor(Color::Rgb {
                    r: 100,
                    g: 180,
                    b: 255
                }),
                Print("  "),
            );
            render_inline(rest, out);
            let _ = execute!(out, ResetColor, SetAttribute(Attribute::Reset));
            return;
        }
        if let Some(rest) = line.strip_prefix("## ") {
            let _ = execute!(
                out,
                SetAttribute(Attribute::Bold),
                SetForegroundColor(Color::Rgb {
                    r: 80,
                    g: 200,
                    b: 200
                }),
                Print("  "),
            );
            render_inline(rest, out);
            let _ = execute!(out, ResetColor, SetAttribute(Attribute::Reset));
            return;
        }
        if let Some(rest) = line.strip_prefix("# ") {
            let _ = execute!(
                out,
                SetAttribute(Attribute::Bold),
                SetForegroundColor(Color::Rgb {
                    r: 80,
                    g: 200,
                    b: 120
                }),
                Print("  "),
            );
            render_inline(rest, out);
            let _ = execute!(out, ResetColor, SetAttribute(Attribute::Reset));
            return;
        }

        // ── Blockquote ───────────────────────────────────────────────────────
        if let Some(rest) = line.strip_prefix("> ") {
            let _ = execute!(
                out,
                SetForegroundColor(Color::Rgb {
                    r: 130,
                    g: 140,
                    b: 160
                }),
                Print("  ▎ "),
            );
            render_inline(rest, out);
            let _ = execute!(out, ResetColor);
            return;
        }

        // ── Unordered list ───────────────────────────────────────────────────
        let stripped_line = line.trim_start();
        let indent = line.len() - stripped_line.len();
        if let Some(rest) = stripped_line
            .strip_prefix("- ")
            .or(stripped_line.strip_prefix("* "))
        {
            let pad = "  ".repeat(indent / 2 + 1);
            let _ = execute!(
                out,
                SetForegroundColor(Color::Rgb {
                    r: 80,
                    g: 200,
                    b: 120
                }),
                Print(format!("{}• ", pad)),
                ResetColor,
            );
            render_inline(rest, out);
            return;
        }

        // ── Ordered list (e.g. "1. ") ────────────────────────────────────────
        if let Some(dot_pos) = stripped_line.find(". ") {
            let prefix = &stripped_line[..dot_pos];
            if prefix.chars().all(|c| c.is_ascii_digit()) && !prefix.is_empty() {
                let rest = &stripped_line[dot_pos + 2..];
                let pad = "  ".repeat(indent / 2 + 1);
                let _ = execute!(
                    out,
                    SetForegroundColor(Color::Rgb {
                        r: 80,
                        g: 200,
                        b: 120
                    }),
                    Print(format!("{}{}. ", pad, prefix)),
                    ResetColor,
                );
                render_inline(rest, out);
                return;
            }
        }

        // ── Horizontal rule ───────────────────────────────────────────────────
        if line.trim() == "---" || line.trim() == "***" {
            let w = terminal::size()
                .map(|(w, _)| w as usize)
                .unwrap_or(80)
                .min(60);
            let _ = execute!(
                out,
                SetForegroundColor(Color::Rgb {
                    r: 70,
                    g: 75,
                    b: 85
                }),
                Print("  "),
                Print("─".repeat(w - 2)),
                ResetColor,
            );
            return;
        }

        // ── Normal text ───────────────────────────────────────────────────────
        let _ = execute!(out, Print("  "));
        render_inline(line, out);
    }
}

/// Render a line with inline markdown: **bold**, *italic*, `code`.
fn render_inline(s: &str, out: &mut impl Write) {
    let bytes = s.as_bytes();
    let len = bytes.len();
    let mut i = 0;

    while i < len {
        // **bold**
        if i + 1 < len && bytes[i] == b'*' && bytes[i + 1] == b'*' {
            if let Some(end) = find_str(s, "**", i + 2) {
                let inner = &s[i + 2..end];
                let _ = execute!(
                    out,
                    SetAttribute(Attribute::Bold),
                    SetForegroundColor(Color::Rgb {
                        r: 255,
                        g: 220,
                        b: 100
                    }),
                    Print(inner),
                    ResetColor,
                    SetAttribute(Attribute::Reset),
                );
                i = end + 2;
                continue;
            }
        }
        // *italic* (not **)
        if bytes[i] == b'*' && (i + 1 >= len || bytes[i + 1] != b'*') {
            if let Some(end) = find_byte(s, b'*', i + 1) {
                let inner = &s[i + 1..end];
                let _ = execute!(
                    out,
                    SetAttribute(Attribute::Italic),
                    SetForegroundColor(Color::Rgb {
                        r: 200,
                        g: 180,
                        b: 255
                    }),
                    Print(inner),
                    ResetColor,
                    SetAttribute(Attribute::Reset),
                );
                i = end + 1;
                continue;
            }
        }
        // `inline code`
        if bytes[i] == b'`' {
            if let Some(end) = find_byte(s, b'`', i + 1) {
                let inner = &s[i + 1..end];
                let _ = execute!(
                    out,
                    SetForegroundColor(Color::Rgb {
                        r: 100,
                        g: 220,
                        b: 140
                    }),
                    Print(inner),
                    ResetColor,
                );
                i = end + 1;
                continue;
            }
        }
        // Regular char — find the next special char to bulk-print.
        let start = i;
        while i < len && bytes[i] != b'*' && bytes[i] != b'`' {
            i += 1;
        }
        let chunk = &s[start..i];
        if !chunk.is_empty() {
            let _ = execute!(
                out,
                SetForegroundColor(Color::Rgb {
                    r: 215,
                    g: 220,
                    b: 230
                }),
                Print(chunk),
                ResetColor
            );
        }
    }
}

fn find_str(s: &str, needle: &str, from: usize) -> Option<usize> {
    s[from..].find(needle).map(|p| p + from)
}

fn find_byte(s: &str, needle: u8, from: usize) -> Option<usize> {
    s[from..]
        .as_bytes()
        .iter()
        .position(|&b| b == needle)
        .map(|p| p + from)
}

// ─── Tool display ─────────────────────────────────────────────────────────────

pub fn print_tool_start(name: &str) {
    let _ = execute!(
        stdout(),
        Print("\n"),
        SetForegroundColor(Color::Rgb { r: 60, g: 68, b: 85 }),
        Print("  ◦ "),
        SetForegroundColor(Color::Rgb { r: 95, g: 105, b: 125 }),
        SetAttribute(Attribute::Dim),
        Print(name),
        SetAttribute(Attribute::Reset),
        ResetColor,
    );
    let _ = stdout().flush();
}

pub fn print_tool_done(name: &str, input: &serde_json::Value, result: &str, is_error: bool, elapsed_ms: u64) {
    let detail = tool_detail(name, input);
    let elapsed_str = if elapsed_ms >= 1000 {
        format!("  · {:.2}s", elapsed_ms as f64 / 1000.0)
    } else if elapsed_ms > 0 {
        format!("  · {}ms", elapsed_ms)
    } else {
        String::new()
    };
    let _ = execute!(stdout(), MoveToColumn(0), Clear(ClearType::CurrentLine));

    if is_error {
        let _ = execute!(
            stdout(),
            SetForegroundColor(Color::Rgb { r: 190, g: 65, b: 65 }),
            Print("  ✗ "),
            SetForegroundColor(Color::Rgb { r: 210, g: 100, b: 100 }),
            Print(name),
            ResetColor,
            SetForegroundColor(Color::Rgb { r: 140, g: 70, b: 70 }),
            Print(if detail.is_empty() { String::new() } else { format!("  {detail}") }),
            SetForegroundColor(Color::Rgb { r: 80, g: 70, b: 70 }),
            SetAttribute(Attribute::Dim),
            Print(&elapsed_str),
            SetAttribute(Attribute::Reset),
            ResetColor,
            Print("\n"),
        );
        if !result.is_empty() {
            print_tool_output_preview(result, true);
        }
    } else {
        let _ = execute!(
            stdout(),
            SetForegroundColor(Color::Rgb { r: 55, g: 150, b: 75 }),
            Print("  ✓ "),
            SetForegroundColor(Color::Rgb { r: 95, g: 105, b: 125 }),
            Print(name),
            SetForegroundColor(Color::Rgb { r: 70, g: 78, b: 95 }),
            Print(if detail.is_empty() { String::new() } else { format!("  {detail}") }),
            SetForegroundColor(Color::Rgb { r: 60, g: 68, b: 82 }),
            SetAttribute(Attribute::Dim),
            Print(&elapsed_str),
            SetAttribute(Attribute::Reset),
            ResetColor,
            Print("\n"),
        );
        if name == "edit_file" {
            if let (Some(old), Some(new)) = (input["old_string"].as_str(), input["new_string"].as_str()) {
                print_inline_diff(old, new);
            }
        } else if name == "write_file" {
            if let Some(content) = input["content"].as_str() {
                let lines = content.lines().count();
                let bytes = content.len();
                let _ = execute!(
                    stdout(),
                    SetForegroundColor(Color::Rgb { r: 55, g: 100, b: 65 }),
                    SetAttribute(Attribute::Dim),
                    Print(format!("  └ {lines} lines · {bytes} bytes\n")),
                    SetAttribute(Attribute::Reset),
                    ResetColor,
                );
            }
        } else if name == "bash" || name == "run_command" {
            if !result.is_empty() {
                print_tool_output_preview(result, false);
            }
        }
    }
    let _ = stdout().flush();
}

/// Print first 5 lines of tool output with `│`/`└` tree prefix.
fn print_tool_output_preview(output: &str, is_error: bool) {
    const MAX_PREVIEW: usize = 5;
    // Skip blank prefix lines for cleaner look.
    let all_lines: Vec<&str> = output.lines().filter(|l| !l.trim().is_empty()).collect();
    let total = all_lines.len();
    if total == 0 { return; }
    let show = total.min(MAX_PREVIEW);
    let (fg_r, fg_g, fg_b) = if is_error { (140, 80, 80) } else { (80, 90, 108) };
    for (i, line) in all_lines[..show].iter().enumerate() {
        let is_last = i + 1 == show;
        let prefix = if is_last && total <= MAX_PREVIEW { "  └ " } else { "  │ " };
        let truncated = truncate_line(line, 100);
        let _ = execute!(
            stdout(),
            SetForegroundColor(Color::Rgb { r: fg_r, g: fg_g, b: fg_b }),
            SetAttribute(Attribute::Dim),
            Print(format!("{prefix}{truncated}\n")),
            SetAttribute(Attribute::Reset),
            ResetColor,
        );
    }
    if total > MAX_PREVIEW {
        let remaining = total - MAX_PREVIEW;
        let _ = execute!(
            stdout(),
            SetForegroundColor(Color::Rgb { r: fg_r, g: fg_g, b: fg_b }),
            SetAttribute(Attribute::Dim),
            Print(format!("  └ … +{remaining} lines\n")),
            SetAttribute(Attribute::Reset),
            ResetColor,
        );
    }
    let _ = stdout().flush();
}

// ─── Inline diff ──────────────────────────────────────────────────────────────

/// Show a colored unified-style diff between old and new strings.
/// Uses LCS to compute the diff; caps display at 12 diff lines.
pub fn print_inline_diff(old: &str, new: &str) {
    const MAX_DIFF_LINES: usize = 12;
    let old_lines: Vec<&str> = old.lines().collect();
    let new_lines: Vec<&str> = new.lines().collect();

    let diff = lcs_diff(&old_lines, &new_lines);
    let out = &mut stdout();

    let changes: Vec<_> = diff.iter().filter(|d| !matches!(d, DiffOp::Same(_))).collect();
    if changes.is_empty() {
        return; // no visible change
    }

    let mut shown = 0usize;
    let mut hidden = 0usize;
    for op in &diff {
        match op {
            DiffOp::Remove(line) => {
                if shown < MAX_DIFF_LINES {
                    let _ = execute!(
                        out,
                        SetBackgroundColor(Color::Rgb { r: 55, g: 18, b: 18 }),
                        SetForegroundColor(Color::Rgb { r: 185, g: 75, b: 75 }),
                        Print("  − "),
                        SetForegroundColor(Color::Rgb { r: 205, g: 125, b: 125 }),
                        Print(truncate_line(line, 100)),
                        ResetColor,
                        Print("\n"),
                    );
                    shown += 1;
                } else {
                    hidden += 1;
                }
            }
            DiffOp::Add(line) => {
                if shown < MAX_DIFF_LINES {
                    let _ = execute!(
                        out,
                        SetBackgroundColor(Color::Rgb { r: 18, g: 48, b: 26 }),
                        SetForegroundColor(Color::Rgb { r: 55, g: 155, b: 78 }),
                        Print("  + "),
                        SetForegroundColor(Color::Rgb { r: 125, g: 195, b: 148 }),
                        Print(truncate_line(line, 100)),
                        ResetColor,
                        Print("\n"),
                    );
                    shown += 1;
                } else {
                    hidden += 1;
                }
            }
            DiffOp::Same(_) => {}
        }
    }
    if hidden > 0 {
        let _ = execute!(
            out,
            SetForegroundColor(Color::Rgb { r: 68, g: 76, b: 92 }),
            SetAttribute(Attribute::Dim),
            Print(format!("  … {hidden} more lines\n")),
            SetAttribute(Attribute::Reset),
            ResetColor,
        );
    }
    let _ = out.flush();
}

enum DiffOp<'a> {
    Same(()),
    Remove(&'a str),
    Add(&'a str),
}

/// O(mn) LCS-based diff. Fine for typical edit_file inputs (< few hundred lines).
fn lcs_diff<'a>(old: &[&'a str], new: &[&'a str]) -> Vec<DiffOp<'a>> {
    let m = old.len();
    let n = new.len();
    // Build LCS table.
    let mut dp = vec![vec![0u16; n + 1]; m + 1];
    for i in 1..=m {
        for j in 1..=n {
            dp[i][j] = if old[i - 1] == new[j - 1] { dp[i-1][j-1] + 1 } else { dp[i-1][j].max(dp[i][j-1]) };
        }
    }
    // Backtrack.
    let mut ops: Vec<DiffOp<'a>> = Vec::new();
    let mut i = m;
    let mut j = n;
    while i > 0 || j > 0 {
        if i > 0 && j > 0 && old[i - 1] == new[j - 1] {
            ops.push(DiffOp::Same(()));
            i -= 1;
            j -= 1;
        } else if j > 0 && (i == 0 || dp[i][j - 1] >= dp[i - 1][j]) {
            ops.push(DiffOp::Add(new[j - 1]));
            j -= 1;
        } else {
            ops.push(DiffOp::Remove(old[i - 1]));
            i -= 1;
        }
    }
    ops.reverse();
    ops
}

fn truncate_line(s: &str, max: usize) -> String {
    let trimmed = s.trim_end();
    if trimmed.chars().count() <= max {
        trimmed.to_string()
    } else {
        format!("{}…", &trimmed[..trimmed.char_indices().nth(max).map(|(i,_)| i).unwrap_or(trimmed.len())])
    }
}

/// Extract a short human-readable detail from tool input.
fn tool_detail(name: &str, input: &serde_json::Value) -> String {
    // Shorten a path to last 3 components for display.
    fn short_path(p: &str) -> String {
        let parts: Vec<&str> = p.trim_start_matches('/').split('/').collect();
        if parts.len() <= 3 {
            p.to_string()
        } else {
            format!("…/{}", parts[parts.len() - 3..].join("/"))
        }
    }
    match name {
        "read_file" => input["path"].as_str().map(short_path).unwrap_or_default(),
        "write_file" | "edit_file" => input["path"].as_str().map(short_path).unwrap_or_default(),
        "list_dir" => input["path"].as_str().map(short_path).unwrap_or_default(),
        "bash" | "run_command" => {
            let cmd = input["command"].as_str().unwrap_or("");
            if cmd.chars().count() > 64 {
                format!("{}…", &cmd[..cmd.char_indices().nth(61).map(|(i,_)|i).unwrap_or(61)])
            } else {
                cmd.to_string()
            }
        }
        "grep" => {
            let pat = input["pattern"].as_str().unwrap_or("");
            let path = input["path"].as_str().unwrap_or(".");
            format!("/{pat}/  {}", short_path(path))
        }
        "glob" => input["pattern"].as_str().unwrap_or("").to_string(),
        "read_image" => input["path"].as_str().map(short_path).unwrap_or_default(),
        _ => String::new(),
    }
}

// ─── Status / error messages ──────────────────────────────────────────────────

pub fn print_info(msg: &str) {
    let _ = execute!(
        stdout(),
        SetForegroundColor(Color::Rgb { r: 95, g: 105, b: 125 }),
        Print("  · "),
        SetForegroundColor(Color::Rgb { r: 155, g: 162, b: 178 }),
        Print(msg),
        ResetColor,
        Print("\n"),
    );
}

pub fn print_success(msg: &str) {
    let _ = execute!(
        stdout(),
        SetForegroundColor(Color::Rgb { r: 55, g: 155, b: 78 }),
        Print("  ✓ "),
        SetForegroundColor(Color::Rgb { r: 160, g: 205, b: 175 }),
        Print(msg),
        ResetColor,
        Print("\n"),
    );
}

pub fn print_warning(msg: &str) {
    let _ = execute!(
        stdout(),
        SetForegroundColor(Color::Rgb { r: 200, g: 145, b: 40 }),
        Print("  ⚠  "),
        SetForegroundColor(Color::Rgb { r: 200, g: 185, b: 130 }),
        Print(msg),
        ResetColor,
        Print("\n"),
    );
}

pub fn print_error(msg: &str) {
    let _ = execute!(
        stdout(),
        SetForegroundColor(Color::Rgb { r: 185, g: 65, b: 65 }),
        Print("  ✗ "),
        SetForegroundColor(Color::Rgb { r: 210, g: 120, b: 120 }),
        Print(msg),
        ResetColor,
        Print("\n"),
    );
}

/// Thin separator printed before each assistant response starts.
pub fn print_turn_divider() {
    let w = terminal::size().map(|(w, _)| w as usize).unwrap_or(80).min(72);
    let _ = execute!(
        stdout(),
        SetForegroundColor(Color::Rgb { r: 45, g: 50, b: 62 }),
        Print(format!("  {}\n", "─".repeat(w.saturating_sub(4)))),
        ResetColor,
    );
}

pub fn print_section_header(title: &str) {
    let w = terminal::size().map(|(w, _)| w as usize).unwrap_or(80).min(60);
    let line = "─".repeat(w.saturating_sub(4));
    let _ = execute!(
        stdout(),
        SetForegroundColor(Color::Rgb { r: 80, g: 200, b: 120 }),
        SetAttribute(Attribute::Bold),
        Print(format!("  {title}")),
        ResetColor,
        SetAttribute(Attribute::Reset),
        Print("\n"),
        SetForegroundColor(Color::Rgb { r: 60, g: 65, b: 75 }),
        Print(format!("  {line}")),
        ResetColor,
        Print("\n"),
    );
}

/// Print a welcome banner with provider status.
pub fn print_welcome_banner(provider_info: &str, auth_store: &dcode_providers::AuthStore) {
    let w = terminal::size().map(|(w, _)| w as usize).unwrap_or(80).min(72);
    let sep = "─".repeat(w.saturating_sub(2));

    // Top separator.
    let _ = execute!(
        stdout(),
        Print("\n"),
        SetForegroundColor(Color::Rgb { r: 45, g: 50, b: 62 }),
        Print(format!("  {sep}\n")),
        ResetColor,
    );

    // Title row: "  d-code  ·  provider/model  ─────  /help"
    // Right-align hints against terminal width.
    let hints = "/help · /model · Tab";
    let middle_vis = 8 + visible_str_len(provider_info); // "d-code  ·  " + provider
    let fill = w.saturating_sub(2 + middle_vis + 2 + hints.len());
    let _ = execute!(
        stdout(),
        Print("  "),
        SetForegroundColor(Color::Rgb { r: 75, g: 195, b: 115 }),
        SetAttribute(Attribute::Bold),
        Print("d-code"),
        SetAttribute(Attribute::Reset),
        ResetColor,
        SetForegroundColor(Color::Rgb { r: 55, g: 60, b: 75 }),
        Print("  ·  "),
        ResetColor,
        SetForegroundColor(Color::Rgb { r: 155, g: 175, b: 205 }),
        Print(provider_info),
        ResetColor,
        SetForegroundColor(Color::Rgb { r: 45, g: 50, b: 62 }),
        Print(format!("  {}", "─".repeat(fill.saturating_sub(2)))),
        ResetColor,
        SetForegroundColor(Color::Rgb { r: 65, g: 72, b: 88 }),
        SetAttribute(Attribute::Dim),
        Print(format!("  {hints}")),
        SetAttribute(Attribute::Reset),
        ResetColor,
        Print("\n"),
    );

    // Provider auth status row.
    let dot_on  = "\x1b[38;2;55;155;78m●\x1b[0m";
    let dot_off = "\x1b[2m○\x1b[0m";
    let anth_dot = if auth_store.anthropic.is_some() { dot_on } else { dot_off };
    let cop_dot  = if auth_store.copilot.is_some()   { dot_on } else { dot_off };
    let oai_dot  = if auth_store.openai.is_some() || auth_store.openai_oauth.is_some() { dot_on } else { dot_off };

    let _ = execute!(
        stdout(),
        Print("  "),
        SetForegroundColor(Color::Rgb { r: 80, g: 88, b: 108 }),
        SetAttribute(Attribute::Dim),
        Print("providers  "),
        SetAttribute(Attribute::Reset),
        ResetColor,
    );
    // Each provider with colored dot.
    for (dot, label) in [
        (anth_dot, "anthropic"),
        (cop_dot, "copilot"),
        (oai_dot, "openai"),
    ] {
        let active = !dot.contains("2m○"); // dim = inactive
        print!("{dot} ");
        let _ = execute!(
            stdout(),
            if active {
                SetForegroundColor(Color::Rgb { r: 130, g: 145, b: 165 })
            } else {
                SetForegroundColor(Color::Rgb { r: 60, g: 65, b: 80 })
            },
            SetAttribute(if active { Attribute::Reset } else { Attribute::Dim }),
            Print(format!("{label}  ")),
            SetAttribute(Attribute::Reset),
            ResetColor,
        );
    }

    // Bottom separator.
    let _ = execute!(
        stdout(),
        Print("\n"),
        SetForegroundColor(Color::Rgb { r: 45, g: 50, b: 62 }),
        Print(format!("  {sep}\n")),
        ResetColor,
        Print("\n"),
    );
}

/// Print a condensed replay of a session's conversation for context on resume.
/// Shows up to `max_turns` turns, each message truncated to fit the terminal.
pub fn print_session_recap(messages: &[dcode_providers::Message], max_turns: usize) {
    use dcode_providers::{ContentBlock, Role};

    let w = terminal::size().map(|(w, _)| w as usize).unwrap_or(80).min(80);
    let sep = format!("  \x1b[2m{}\x1b[0m", "─".repeat(w.saturating_sub(4)));

    // Collect turns: pairs of (user_text, assistant_text)
    let mut turns: Vec<(String, String)> = vec![];
    let mut user_text = String::new();
    let mut asst_text = String::new();

    for msg in messages {
        match msg.role {
            Role::User => {
                if !user_text.is_empty() && !asst_text.is_empty() {
                    turns.push((user_text.clone(), asst_text.clone()));
                    user_text.clear();
                    asst_text.clear();
                }
                for block in &msg.content {
                    if let ContentBlock::Text { text } = block {
                        let t = text.trim();
                        if !t.is_empty() && !t.starts_with('[') {
                            if !user_text.is_empty() { user_text.push(' '); }
                            user_text.push_str(t);
                        }
                    }
                }
            }
            Role::Assistant => {
                for block in &msg.content {
                    if let ContentBlock::Text { text } = block {
                        let t = text.trim();
                        if !t.is_empty() && !t.starts_with('[') {
                            if !asst_text.is_empty() { asst_text.push(' '); }
                            asst_text.push_str(t);
                        }
                    }
                }
            }
        }
    }
    if !user_text.is_empty() && !asst_text.is_empty() {
        turns.push((user_text, asst_text));
    }

    if turns.is_empty() {
        return;
    }

    println!("{sep}");

    let start = turns.len().saturating_sub(max_turns);
    if start > 0 {
        println!("  \x1b[2m… {} earlier turn(s) not shown\x1b[0m", start);
    }

    let msg_width = w.saturating_sub(12); // account for label prefix

    for (user, asst) in &turns[start..] {
        // User line.
        let user_trunc = truncate_to(user, msg_width);
        let _ = execute!(
            stdout(),
            SetForegroundColor(Color::Rgb { r: 100, g: 160, b: 240 }),
            Print("  You  "),
            ResetColor,
            SetForegroundColor(Color::Rgb { r: 200, g: 210, b: 225 }),
            Print(format!("{user_trunc}\n")),
            ResetColor,
        );

        // Assistant line.
        let asst_trunc = truncate_to(asst, msg_width);
        let _ = execute!(
            stdout(),
            SetForegroundColor(Color::Rgb { r: 80, g: 200, b: 120 }),
            Print("  d-code  "),
            ResetColor,
            SetForegroundColor(Color::Rgb { r: 160, g: 170, b: 185 }),
            Print(format!("{asst_trunc}\n")),
            ResetColor,
        );
        println!();
    }
    println!("{sep}");
}

/// Prompt the user to confirm a dangerous bash command.
/// Returns true if approved. Called synchronously from inside the agent loop.
pub fn confirm_dangerous_bash(cmd: &str) -> bool {
    use crossterm::style::{Color, ResetColor, SetForegroundColor};
    let _ = execute!(
        stdout(),
        Print("\n"),
        SetForegroundColor(Color::Rgb { r: 255, g: 180, b: 50 }),
        Print("  ⚠  Dangerous command detected\n"),
        ResetColor,
        SetForegroundColor(Color::Rgb { r: 220, g: 230, b: 240 }),
        Print(format!("  $ {cmd}\n")),
        ResetColor,
        SetForegroundColor(Color::Rgb { r: 140, g: 150, b: 165 }),
        Print("  Run it? [y/N] "),
        ResetColor,
    );
    let _ = stdout().flush();
    let mut input = String::new();
    let _ = std::io::stdin().read_line(&mut input);
    let approved = input.trim().eq_ignore_ascii_case("y");
    if !approved {
        let _ = execute!(
            stdout(),
            SetForegroundColor(Color::Rgb { r: 200, g: 70, b: 70 }),
            Print("  Blocked.\n"),
            ResetColor,
        );
    }
    approved
}

/// Prompt the user with a question from the AI's ask_user tool.
/// Returns the user's text answer.
pub fn prompt_user_question(question: &str, choices: &[String]) -> String {
    use crossterm::style::{Color, ResetColor, SetForegroundColor, SetAttribute, Attribute};
    println!();
    let _ = execute!(
        stdout(),
        SetForegroundColor(Color::Rgb { r: 80, g: 200, b: 200 }),
        SetAttribute(Attribute::Bold),
        Print("  ? "),
        ResetColor,
        SetForegroundColor(Color::Rgb { r: 215, g: 220, b: 230 }),
        Print(question),
        Print("\n"),
        ResetColor,
        SetAttribute(Attribute::Reset),
    );
    if !choices.is_empty() {
        for (i, choice) in choices.iter().enumerate() {
            let _ = execute!(
                stdout(),
                SetForegroundColor(Color::Rgb { r: 100, g: 130, b: 160 }),
                Print(format!("    {}. ", i + 1)),
                ResetColor,
                SetForegroundColor(Color::Rgb { r: 180, g: 190, b: 210 }),
                Print(format!("{choice}\n")),
                ResetColor,
            );
        }
        let _ = execute!(
            stdout(),
            SetForegroundColor(Color::Rgb { r: 140, g: 150, b: 165 }),
            Print(format!("  Choice [1-{}] or type answer: ", choices.len())),
            ResetColor,
        );
    } else {
        let _ = execute!(
            stdout(),
            SetForegroundColor(Color::Rgb { r: 140, g: 150, b: 165 }),
            Print("  Answer: "),
            ResetColor,
        );
    }
    let _ = stdout().flush();
    let mut input = String::new();
    let _ = std::io::stdin().read_line(&mut input);
    let input = input.trim().to_string();
    // Map number to choice text if applicable.
    if !choices.is_empty() {
        if let Ok(n) = input.parse::<usize>() {
            if n >= 1 && n <= choices.len() {
                return choices[n - 1].clone();
            }
        }
    }
    input
}

fn truncate_to(s: &str, max_chars: usize) -> String {
    let first_line = s.lines().next().unwrap_or(s);
    let count = first_line.chars().count();
    if count <= max_chars {
        first_line.to_string()
    } else {
        let end: usize = first_line
            .char_indices()
            .nth(max_chars.saturating_sub(1))
            .map(|(i, _)| i)
            .unwrap_or(first_line.len());
        format!("{}…", &first_line[..end])
    }
}

fn visible_str_len(s: &str) -> usize {
    let mut len = 0usize;
    let mut esc = false;
    for ch in s.chars() {
        if esc {
            if ch == 'm' { esc = false; }
        } else if ch == '\x1b' {
            esc = true;
        } else {
            len += 1;
        }
    }
    len
}

// ─── Interactive selection dropdown ──────────────────────────────────────────

/// Interactive selector with type-to-filter search.
/// `current_marker` optionally marks one item as "(current)".
pub fn select_interactive(title: &str, items: &[String]) -> Option<usize> {
    select_interactive_with_current(title, items, None)
}

pub fn select_interactive_with_current(
    title: &str,
    items: &[String],
    current_idx: Option<usize>,
) -> Option<usize> {
    let max_visible: usize = terminal::size()
        .map(|(_, h)| (h as usize).saturating_sub(6).min(16))
        .unwrap_or(12);

    let mut query = String::new();
    let mut selected: usize = 0;
    let mut scroll_offset: usize = 0;

    // Build filtered indices.
    let filter = |q: &str| -> Vec<usize> {
        if q.is_empty() {
            return (0..items.len()).collect();
        }
        let q_lower = q.to_lowercase();
        items
            .iter()
            .enumerate()
            .filter(|(_, item)| {
                let item_lower = item.to_lowercase();
                // Fuzzy: all query chars appear in order.
                let mut chars = q_lower.chars();
                let mut current = chars.next();
                for ch in item_lower.chars() {
                    if let Some(c) = current {
                        if ch == c {
                            current = chars.next();
                        }
                    } else {
                        break;
                    }
                }
                current.is_none()
            })
            .map(|(i, _)| i)
            .collect()
    };

    let mut filtered = filter(&query);

    // Start selection on current model if present.
    if let Some(cur) = current_idx {
        if let Some(pos) = filtered.iter().position(|&i| i == cur) {
            selected = pos;
            if selected >= max_visible {
                scroll_offset = selected.saturating_sub(max_visible / 2);
            }
        }
    }

    terminal::enable_raw_mode().ok()?;
    let mut out = stdout();

    // Draw header + search + list.
    let mut header_lines = draw_picker_full(
        &mut out,
        title,
        &query,
        &filtered,
        items,
        selected,
        scroll_offset,
        max_visible,
        current_idx,
    );

    let result = loop {
        let Ok(Event::Key(KeyEvent {
            code,
            modifiers,
            kind,
            ..
        })) = event::read()
        else {
            continue;
        };
        if !matches!(
            kind,
            crossterm::event::KeyEventKind::Press | crossterm::event::KeyEventKind::Repeat
        ) {
            continue;
        }

        let mut needs_redraw = false;

        match code {
            KeyCode::Up => {
                if selected > 0 {
                    selected -= 1;
                    if selected < scroll_offset {
                        scroll_offset = selected;
                    }
                    needs_redraw = true;
                }
            }
            KeyCode::Down => {
                if !filtered.is_empty() && selected + 1 < filtered.len() {
                    selected += 1;
                    if selected >= scroll_offset + max_visible {
                        scroll_offset = selected + 1 - max_visible;
                    }
                    needs_redraw = true;
                }
            }
            KeyCode::Enter => {
                if !filtered.is_empty() {
                    break Some(filtered[selected]);
                }
            }
            KeyCode::Esc => break None,
            KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => break None,
            KeyCode::Backspace => {
                if !query.is_empty() {
                    query.pop();
                    filtered = filter(&query);
                    selected = 0;
                    scroll_offset = 0;
                    needs_redraw = true;
                }
            }
            KeyCode::Char(ch) if !modifiers.contains(KeyModifiers::CONTROL) => {
                query.push(ch);
                filtered = filter(&query);
                selected = 0;
                scroll_offset = 0;
                needs_redraw = true;
            }
            _ => {}
        }

        if needs_redraw {
            erase_picker(&mut out, header_lines);
            header_lines = draw_picker_full(
                &mut out,
                title,
                &query,
                &filtered,
                items,
                selected,
                scroll_offset,
                max_visible,
                current_idx,
            );
        }
    };

    terminal::disable_raw_mode().ok();
    erase_picker(&mut out, header_lines);

    result
}

/// Returns total lines rendered.
fn draw_picker_full(
    out: &mut impl Write,
    title: &str,
    query: &str,
    filtered: &[usize],
    items: &[String],
    selected: usize,
    scroll_offset: usize,
    max_visible: usize,
    current_idx: Option<usize>,
) -> usize {
    let mut lines = 0;

    // Title.
    for line in title.lines() {
        let _ = execute!(out, MoveToColumn(0), Print(line), Print("\r\n"));
        lines += 1;
    }

    // Search input.
    let _ = execute!(
        out,
        MoveToColumn(0),
        SetForegroundColor(Color::Rgb {
            r: 100,
            g: 110,
            b: 130
        }),
        Print("  🔍 "),
        ResetColor,
        SetForegroundColor(Color::Rgb {
            r: 220,
            g: 225,
            b: 235
        }),
        Print(if query.is_empty() {
            "type to filter..."
        } else {
            query
        }),
        ResetColor,
        Clear(ClearType::UntilNewLine),
        Print("\r\n"),
    );
    lines += 1;

    // Separator.
    let _ = execute!(
        out,
        MoveToColumn(0),
        SetForegroundColor(Color::Rgb {
            r: 60,
            g: 65,
            b: 75
        }),
        Print("  ─────────────────────────────"),
        ResetColor,
        Print("\r\n"),
    );
    lines += 1;

    if filtered.is_empty() {
        let _ = execute!(
            out,
            MoveToColumn(0),
            SetForegroundColor(Color::Rgb {
                r: 130,
                g: 140,
                b: 150
            }),
            Print("    No matches"),
            ResetColor,
            Clear(ClearType::UntilNewLine),
            Print("\r\n"),
        );
        lines += 1;
    } else {
        let visible_end = (scroll_offset + max_visible).min(filtered.len());

        // Scroll-up indicator.
        if scroll_offset > 0 {
            let _ = execute!(
                out,
                MoveToColumn(0),
                SetForegroundColor(Color::Rgb {
                    r: 100,
                    g: 110,
                    b: 130
                }),
                Print(format!("    ↑ {} more", scroll_offset)),
                ResetColor,
                Clear(ClearType::UntilNewLine),
                Print("\r\n"),
            );
            lines += 1;
        }

        for vis_i in scroll_offset..visible_end {
            let item_idx = filtered[vis_i];
            let item = &items[item_idx];
            let is_sel = vis_i == selected;
            let is_current = current_idx == Some(item_idx);

            if is_sel {
                let _ = execute!(
                    out,
                    MoveToColumn(0),
                    SetForegroundColor(Color::Rgb {
                        r: 80,
                        g: 200,
                        b: 120
                    }),
                    SetAttribute(Attribute::Bold),
                    Print(format!("  ❯ {}", item)),
                );
                if is_current {
                    let _ = execute!(
                        out,
                        SetForegroundColor(Color::Rgb {
                            r: 80,
                            g: 160,
                            b: 200
                        }),
                        Print(" (current)"),
                    );
                }
                let _ = execute!(
                    out,
                    ResetColor,
                    SetAttribute(Attribute::Reset),
                    Clear(ClearType::UntilNewLine),
                    Print("\r\n")
                );
            } else {
                let _ = execute!(
                    out,
                    MoveToColumn(0),
                    SetForegroundColor(if is_current {
                        Color::Rgb {
                            r: 80,
                            g: 160,
                            b: 200,
                        }
                    } else {
                        Color::Rgb {
                            r: 130,
                            g: 140,
                            b: 150,
                        }
                    }),
                    Print(format!("    {}", item)),
                );
                if is_current {
                    let _ = execute!(
                        out,
                        SetForegroundColor(Color::Rgb {
                            r: 80,
                            g: 130,
                            b: 160
                        }),
                        Print(" (current)"),
                    );
                }
                let _ = execute!(
                    out,
                    ResetColor,
                    Clear(ClearType::UntilNewLine),
                    Print("\r\n")
                );
            }
            lines += 1;
        }

        // Scroll-down indicator.
        if visible_end < filtered.len() {
            let _ = execute!(
                out,
                MoveToColumn(0),
                SetForegroundColor(Color::Rgb {
                    r: 100,
                    g: 110,
                    b: 130
                }),
                Print(format!("    ↓ {} more", filtered.len() - visible_end)),
                ResetColor,
                Clear(ClearType::UntilNewLine),
                Print("\r\n"),
            );
            lines += 1;
        }
    }

    // Footer hint.
    let _ = execute!(
        out,
        MoveToColumn(0),
        SetForegroundColor(Color::Rgb {
            r: 80,
            g: 85,
            b: 100
        }),
        Print("  ↑↓ navigate · enter select · esc cancel"),
        ResetColor,
        Clear(ClearType::UntilNewLine),
        Print("\r\n"),
    );
    lines += 1;

    let _ = out.flush();
    lines
}

fn erase_picker(out: &mut impl Write, total_lines: usize) {
    if total_lines > 0 {
        for _ in 0..total_lines {
            let _ = execute!(
                out,
                crossterm::cursor::MoveToPreviousLine(1),
                Clear(ClearType::CurrentLine)
            );
        }
        let _ = execute!(out, MoveToColumn(0), Clear(ClearType::CurrentLine));
    }
    let _ = out.flush();
}
