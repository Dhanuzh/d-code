//! AssistantMessage component — renders the streaming AI response.
//!
//! Mirrors pi-mono's assistant-message.ts component.
//! Accumulates text deltas, renders markdown-styled lines.
//! Marks dirty on each delta so the TUI re-renders only when content changes.

use crate::{Component, Line};

// Pi-mono dark theme colors
const C_TEXT: &str = "\x1b[38;2;212;215;222m";
const C_BOLD: &str = "\x1b[38;2;240;198;116m"; // heading gold
const C_CODE: &str = "\x1b[38;2;138;190;183m"; // teal
const C_BULLET: &str = "\x1b[38;2;138;190;183m"; // teal
const C_HEADING: &str = "\x1b[38;2;240;198;116m"; // gold
const C_SUCCESS: &str = "\x1b[38;2;181;189;104m"; // green
const C_MUTED: &str = "\x1b[38;2;128;128;128m";
const C_THINKING: &str = "\x1b[38;2;102;102;120m"; // dim purple-gray for thinking
const RESET: &str = "\x1b[0m";
const ITALIC: &str = "\x1b[3m";

/// Renders a streaming assistant message with markdown formatting.
///
/// Accumulates text deltas, processes complete lines, keeps last partial
/// line for live typewriter preview. Also renders extended thinking content
/// in a dim/italic style above the main text.
pub struct AssistantMessage {
    /// Complete lines already processed (stable, won't change).
    complete_lines: Vec<String>,
    /// Current incomplete line buffer.
    partial: String,
    /// Whether we're inside a ``` code block.
    in_code_block: bool,
    /// Language tag of current code fence.
    code_lang: String,
    /// Content changed since last render.
    dirty: bool,
    /// Whether the message is finalized (no more deltas expected).
    finalized: bool,
    /// Accumulated thinking content (rendered dim/italic above main text).
    thinking_buf: String,
    /// Whether we're currently receiving thinking content.
    pub in_thinking: bool,
}

impl AssistantMessage {
    pub fn new() -> Self {
        Self {
            complete_lines: Vec::new(),
            partial: String::new(),
            in_code_block: false,
            code_lang: String::new(),
            dirty: true,
            finalized: false,
            thinking_buf: String::new(),
            in_thinking: false,
        }
    }

    /// Feed a thinking delta (extended reasoning content).
    pub fn push_thinking(&mut self, delta: &str) {
        self.thinking_buf.push_str(delta);
        self.in_thinking = true;
        self.dirty = true;
    }

    /// Mark thinking as complete (next text delta starts the real response).
    pub fn end_thinking(&mut self) {
        self.in_thinking = false;
        self.dirty = true;
    }

    /// Feed a text delta from the stream.
    pub fn push(&mut self, delta: &str) {
        self.partial.push_str(delta);
        self.dirty = true;

        // Process complete lines.
        while let Some(nl) = self.partial.find('\n') {
            let line = self.partial[..nl].to_string();
            self.partial.drain(..nl + 1);
            let rendered = self.render_complete_line(&line);
            self.complete_lines.push(rendered);
        }
    }

    /// Mark the message as complete (flush partial).
    pub fn finalize(&mut self) {
        if !self.partial.is_empty() {
            let line = std::mem::take(&mut self.partial);
            let rendered = self.render_complete_line(&line);
            self.complete_lines.push(rendered);
        }
        self.finalized = true;
        self.dirty = true;
    }

    /// Number of complete lines rendered.
    pub fn line_count(&self) -> usize {
        self.complete_lines.len() + if self.partial.is_empty() { 0 } else { 1 }
    }

    // ── Rendering ─────────────────────────────────────────────────────────────

    /// Render a complete line with full markdown processing.
    fn render_complete_line(&mut self, line: &str) -> String {
        // Code fence open/close
        if line.trim_start().starts_with("```") {
            if self.in_code_block {
                self.in_code_block = false;
                self.code_lang.clear();
                return format!("  {C_MUTED}───{RESET}");
            } else {
                self.in_code_block = true;
                self.code_lang = line.trim_start().trim_start_matches('`').trim().to_string();
                let label = if self.code_lang.is_empty() {
                    String::new()
                } else {
                    format!(" {C_MUTED}{}{RESET}", self.code_lang)
                };
                return format!("  {C_MUTED}───{label}{RESET}");
            }
        }

        if self.in_code_block {
            return format!("  {C_SUCCESS}│ {line}{RESET}");
        }

        // Headers
        if let Some(rest) = line.strip_prefix("### ") {
            return format!("  {C_CODE}▸ {rest}{RESET}");
        }
        if let Some(rest) = line.strip_prefix("## ") {
            return format!("  {C_HEADING}{rest}{RESET}");
        }
        if let Some(rest) = line.strip_prefix("# ") {
            return format!("  {C_HEADING}\x1b[1m{rest}{RESET}");
        }

        // Bullets
        if let Some(rest) = line.strip_prefix("- ").or_else(|| line.strip_prefix("* ")) {
            let content = render_inline(rest);
            return format!("  {C_BULLET}•{RESET} {content}");
        }

        // Numbered list
        if let Some(idx) = line.find(". ") {
            let prefix = &line[..idx];
            if prefix.chars().all(|c| c.is_ascii_digit()) {
                let rest = &line[idx + 2..];
                return format!("  {C_MUTED}{prefix}.{RESET} {}", render_inline(rest));
            }
        }

        // Blockquote
        if let Some(rest) = line.strip_prefix("> ") {
            return format!("  {C_MUTED}│ {rest}{RESET}");
        }

        // Horizontal rule
        if line.trim() == "---" || line.trim() == "***" {
            return format!("  {C_MUTED}────────────────────{RESET}");
        }

        // Empty line
        if line.trim().is_empty() {
            return String::new();
        }

        // Regular paragraph
        format!("  {}", render_inline(line))
    }

    /// Render partial (incomplete) line — minimal formatting for typewriter preview.
    fn render_partial_line(&self, text: &str) -> String {
        if self.in_code_block {
            format!("  {C_SUCCESS}│ {text}{RESET}")
        } else {
            format!("  {}", render_inline(text))
        }
    }
}

impl Default for AssistantMessage {
    fn default() -> Self {
        Self::new()
    }
}

impl Component for AssistantMessage {
    fn render(&mut self, _width: u16) -> Vec<Line> {
        let mut lines: Vec<Line> = Vec::new();

        // Render thinking block first (dim/italic), if any.
        if !self.thinking_buf.is_empty() {
            for line in self.thinking_buf.lines() {
                let rendered = if line.trim().is_empty() {
                    String::new()
                } else {
                    format!("  {C_THINKING}{ITALIC}{line}{RESET}")
                };
                lines.push(Line::raw(rendered));
            }
            // If still receiving thinking, show live partial on last line.
            if self.in_thinking {
                // partial is in thinking_buf already (streamed char by char)
            } else if !self.complete_lines.is_empty() {
                // separator between thinking and response
                lines.push(Line::raw(String::new()));
            }
        }

        // Main response lines.
        for s in &self.complete_lines {
            lines.push(Line::raw(s.clone()));
        }

        // Show partial line as typewriter preview.
        if !self.partial.is_empty() && !self.finalized {
            let preview = self.render_partial_line(&self.partial);
            lines.push(Line::raw(preview));
        }

        lines
    }

    fn is_dirty(&self) -> bool {
        self.dirty
    }

    fn mark_clean(&mut self) {
        self.dirty = false;
    }
}

/// Render inline markdown (bold, italic, inline code) to ANSI string.
pub fn render_inline(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 32);
    let bytes = text.as_bytes();
    let mut i = 0;

    while i < bytes.len() {
        // Inline code: `...`
        if bytes[i] == b'`' {
            if let Some(end) = text[i + 1..].find('`') {
                let code = &text[i + 1..i + 1 + end];
                out.push_str(&format!("{C_CODE}{code}{RESET}"));
                i += end + 2;
                continue;
            }
        }
        // Bold: **...**
        if bytes[i] == b'*' && i + 1 < bytes.len() && bytes[i + 1] == b'*' {
            if let Some(end) = text[i + 2..].find("**") {
                let bold = &text[i + 2..i + 2 + end];
                out.push_str(&format!("{C_BOLD}\x1b[1m{bold}{RESET}"));
                i += end + 4;
                continue;
            }
        }
        // Italic: *...*
        if bytes[i] == b'*' {
            if let Some(end) = text[i + 1..].find('*') {
                let italic = &text[i + 1..i + 1 + end];
                out.push_str(&format!("{C_MUTED}\x1b[3m{italic}{RESET}"));
                i += end + 2;
                continue;
            }
        }
        // Regular char — use C_TEXT color for normal text.
        if out.is_empty() || !out.ends_with(C_TEXT) {
            out.push_str(C_TEXT);
        }
        out.push(text[i..].chars().next().unwrap());
        i += text[i..].chars().next().map(|c| c.len_utf8()).unwrap_or(1);
    }

    if !out.is_empty() {
        out.push_str(RESET);
    }
    out
}
