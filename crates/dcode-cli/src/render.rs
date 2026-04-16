/// Terminal renderer: streaming markdown with inline formatting + tool display.
use crossterm::cursor::{MoveToColumn, MoveUp};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use crossterm::style::{Attribute, Color, ResetColor, SetAttribute, SetForegroundColor};
use crossterm::terminal::{self, Clear, ClearType};
use crossterm::{execute, style::Print};
use std::io::{stdout, Write};

// ─── Pi-mono dark theme palette ───────────────────────────────────────────────
// Source: pi-mono/packages/coding-agent/src/modes/interactive/theme/dark.json

/// #8abeb7 — teal accent (bullets, inline code, prompt, spinner peak)
const C_ACCENT: Color = Color::Rgb {
    r: 138,
    g: 190,
    b: 183,
};
/// #5f87ff — blue border (borders, headings h3, italic, choice numbers)
const C_BORDER: Color = Color::Rgb {
    r: 95,
    g: 135,
    b: 255,
};
/// #b5bd68 — yellow-green success / bash mode / code blocks
const C_SUCCESS: Color = Color::Rgb {
    r: 181,
    g: 189,
    b: 104,
};
/// #cc6666 — red error
const C_ERROR: Color = Color::Rgb {
    r: 204,
    g: 102,
    b: 102,
};
/// Amber warning (no direct match — use near-yellow)
const C_WARNING: Color = Color::Rgb {
    r: 220,
    g: 175,
    b: 50,
};
/// #808080 — muted gray (output, labels, quotes)
const C_MUTED: Color = Color::Rgb {
    r: 128,
    g: 128,
    b: 128,
};
/// #666666 — dim (separators, hints, elapsed, inactive)
const C_DIM: Color = Color::Rgb {
    r: 102,
    g: 102,
    b: 102,
};
/// Near-white default text
const C_TEXT: Color = Color::Rgb {
    r: 212,
    g: 215,
    b: 222,
};
/// #f0c674 — gold heading / bold / warning icon
const C_HEADING: Color = Color::Rgb {
    r: 240,
    g: 198,
    b: 116,
};

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
        // Always flush pending — even if somehow stuck in xml mode,
        // return what we have rather than silently dropping content.
        let s = self.pending.clone();
        self.pending.clear();
        self.in_xml = false;
        self.close_tag.clear();
        s
    }
}

/// Only match known internal thinking tags that should be hidden from the user.
/// Intentionally narrow — matching ALL lowercase tags caused the filter to swallow
/// normal model output like `<br>`, `<b>`, `<code>` etc., causing the "stuck" bug.
fn parse_open_tag(s: &str) -> Option<&str> {
    let s = s.trim();
    if !s.starts_with('<') || !s.ends_with('>') {
        return None;
    }
    let inner = &s[1..s.len() - 1];
    // Only filter Anthropic/model internal thinking blocks.
    matches!(inner, "antml_thinking" | "thinking" | "antthinking").then_some(inner)
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
    /// Language tag of the current code fence (e.g. "rust", "python").
    code_lang: String,
    /// Number of terminal rows the last partial render occupied (≥1 when has_partial).
    partial_rows: usize,
    /// Cached terminal width to avoid syscall on every token.
    term_width: usize,
    /// Throttle flushes: only flush every ~16ms (60fps) for partial updates.
    last_flush: std::time::Instant,
}

impl MarkdownRenderer {
    pub fn new() -> Self {
        let term_width = terminal::size().map(|(w, _)| w as usize).unwrap_or(80);
        Self {
            line_buf: String::new(),
            in_code_block: false,
            code_lang: String::new(),
            partial_rows: 0,
            term_width,
            last_flush: std::time::Instant::now(),
        }
    }

    /// Feed a text delta.
    /// Immediately renders partial content for typewriter effect; re-renders
    /// with full markdown formatting when each line completes.
    pub fn push(&mut self, text: &str) {
        self.line_buf.push_str(text);
        let mut out = std::io::BufWriter::new(stdout());
        let mut had_complete_lines = false;

        // Process every complete line (ends with \n).
        while let Some(nl) = self.line_buf.find('\n') {
            let line = self.line_buf[..nl].to_string();
            self.line_buf.drain(..nl + 1);
            had_complete_lines = true;

            // Clear the partial-line preview we printed earlier.
            if self.partial_rows > 0 {
                Self::clear_partial_rows(&mut out, self.partial_rows);
                self.partial_rows = 0;
            }

            self.render_line(&line, &mut out);
            let _ = execute!(out, Print("\n"));
        }

        // Typewriter: immediately print whatever partial text is buffered,
        // but throttle to ~60fps to avoid a flush syscall on every token.
        let now = std::time::Instant::now();
        let should_update_partial = !self.line_buf.is_empty()
            && (had_complete_lines || now.duration_since(self.last_flush).as_millis() >= 16);

        if should_update_partial {
            if self.partial_rows > 0 {
                Self::clear_partial_rows(&mut out, self.partial_rows);
            }
            // Synchronized output: prevent flicker during partial render.
            let _ = out.write_all(b"\x1b[?2026h");
            self.render_partial(&self.line_buf.clone(), &mut out);
            let _ = out.write_all(b"\x1b[?2026l");
            self.partial_rows = self.measure_partial_rows(&self.line_buf);
            self.last_flush = now;
        }

        // Flush the BufWriter: only syscalls once per push() regardless of
        // how many escape sequences were emitted.
        if had_complete_lines || should_update_partial {
            let _ = out.flush();
        }
    }

    /// Flush any remaining buffered text at end of turn (no trailing newline).
    pub fn flush(&mut self) {
        if !self.line_buf.is_empty() {
            let line = std::mem::take(&mut self.line_buf);
            let mut out = std::io::BufWriter::new(stdout());
            if self.partial_rows > 0 {
                Self::clear_partial_rows(&mut out, self.partial_rows);
            }
            self.partial_rows = 0;
            self.render_line(&line, &mut out);
            let _ = out.flush();
        } else {
            self.partial_rows = 0;
        }
    }

    /// Clear `rows` terminal rows that were used by a partial render.
    /// Moves up (rows-1) lines then clears from cursor down.
    fn clear_partial_rows(out: &mut impl Write, rows: usize) {
        if rows > 1 {
            let _ = execute!(out, MoveUp((rows - 1) as u16));
        }
        let _ = execute!(out, MoveToColumn(0), Clear(ClearType::FromCursorDown));
    }

    /// Estimate how many terminal rows the partial text will occupy.
    fn measure_partial_rows(&self, text: &str) -> usize {
        // Use cached terminal width (set at construction, avoids syscall per token).
        let w = if self.term_width > 0 {
            self.term_width
        } else {
            80
        };
        // prefix "  " = 2 chars; code block prefix "  │ " = 4 chars
        let prefix_len = if self.in_code_block { 4 } else { 2 };
        let visible_len = prefix_len + text.chars().count();
        (visible_len.saturating_sub(1) / w) + 1
    }

    /// Render a partial (incomplete) line — no block-level detection, just
    /// inline formatting so the typewriter text looks styled as it arrives.
    fn render_partial(&self, text: &str, out: &mut impl Write) {
        if self.in_code_block {
            let _ = execute!(
                out,
                SetForegroundColor(C_SUCCESS),
                Print(format!("  │ {text}")),
                ResetColor,
            );
        } else {
            let _ = execute!(out, Print("  "));
            render_inline(text, out);
        }
    }

    fn render_line(&mut self, line: &str, out: &mut impl Write) {
        // ── Code fence ───────────────────────────────────────────────────────
        if line.trim_start().starts_with("```") {
            self.in_code_block = !self.in_code_block;
            if self.in_code_block {
                let lang_str = line.trim_start()[3..].trim();
                self.code_lang = lang_str.to_lowercase();
                let _ = execute!(
                    out,
                    SetForegroundColor(C_DIM),
                    SetAttribute(Attribute::Dim),
                    Print(format!(
                        "  ╭─ {}",
                        if lang_str.is_empty() {
                            "code"
                        } else {
                            lang_str
                        }
                    )),
                    ResetColor,
                    SetAttribute(Attribute::Reset),
                );
            } else {
                self.code_lang.clear();
                let _ = execute!(
                    out,
                    SetForegroundColor(C_DIM),
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
            render_code_line_highlighted(line, &self.code_lang.clone(), out);
            return;
        }

        // ── Headers ──────────────────────────────────────────────────────────
        if let Some(rest) = line.strip_prefix("### ") {
            let _ = execute!(
                out,
                SetAttribute(Attribute::Bold),
                SetForegroundColor(C_BORDER),
                Print("  ### ")
            );
            render_inline(rest, out);
            let _ = execute!(out, ResetColor, SetAttribute(Attribute::Reset));
            return;
        }
        if let Some(rest) = line.strip_prefix("## ") {
            let _ = execute!(
                out,
                SetAttribute(Attribute::Bold),
                SetForegroundColor(C_HEADING),
                Print("  ## ")
            );
            render_inline(rest, out);
            let _ = execute!(out, ResetColor, SetAttribute(Attribute::Reset));
            return;
        }
        if let Some(rest) = line.strip_prefix("# ") {
            let _ = execute!(
                out,
                SetAttribute(Attribute::Bold),
                SetForegroundColor(C_HEADING),
                Print("  # ")
            );
            render_inline(rest, out);
            let _ = execute!(out, ResetColor, SetAttribute(Attribute::Reset));
            return;
        }

        // ── Blockquote ───────────────────────────────────────────────────────
        if let Some(rest) = line.strip_prefix("> ") {
            let _ = execute!(out, SetForegroundColor(C_MUTED), Print("  ▎ "));
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
                SetForegroundColor(C_ACCENT),
                Print(format!("{}• ", pad)),
                ResetColor
            );
            render_inline(rest, out);
            return;
        }

        // ── Ordered list ─────────────────────────────────────────────────────
        if let Some(dot_pos) = stripped_line.find(". ") {
            let prefix = &stripped_line[..dot_pos];
            if prefix.chars().all(|c| c.is_ascii_digit()) && !prefix.is_empty() {
                let rest = &stripped_line[dot_pos + 2..];
                let pad = "  ".repeat(indent / 2 + 1);
                let _ = execute!(
                    out,
                    SetForegroundColor(C_ACCENT),
                    Print(format!("{}{}. ", pad, prefix)),
                    ResetColor
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
                SetForegroundColor(C_DIM),
                SetAttribute(Attribute::Dim),
                Print(format!("  {}", "─".repeat(w.saturating_sub(2)))),
                ResetColor,
                SetAttribute(Attribute::Reset),
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
                    SetForegroundColor(C_HEADING),
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
                    SetForegroundColor(C_MUTED),
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
                let _ = execute!(out, SetForegroundColor(C_ACCENT), Print(inner), ResetColor);
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
            let _ = execute!(out, SetForegroundColor(C_TEXT), Print(chunk), ResetColor);
        }
    }
}

fn find_str(s: &str, needle: &str, from: usize) -> Option<usize> {
    s[from..].find(needle).map(|p| p + from)
}

fn find_byte(s: &str, needle: u8, from: usize) -> Option<usize> {
    s.as_bytes()[from..]
        .iter()
        .position(|&b| b == needle)
        .map(|p| p + from)
}

// ─── Syntax highlighting ──────────────────────────────────────────────────────

/// Render a single code line with basic syntax highlighting based on language.
fn render_code_line_highlighted(line: &str, lang: &str, out: &mut impl Write) {
    let _ = execute!(out, Print("  │ "));
    if line.is_empty() {
        let _ = execute!(out, Print("\n"));
        return;
    }
    let trimmed = line.trim_start();

    // Full-line comment detection
    let is_comment = match lang {
        "rust" | "rs" | "go" | "js" | "javascript" | "ts" | "typescript" | "jsx" | "tsx"
        | "java" | "c" | "cpp" | "c++" | "cs" | "swift" | "kotlin" | "scala" => {
            trimmed.starts_with("//") || trimmed.starts_with("/*") || trimmed.starts_with('*')
        }
        "py" | "python" | "sh" | "bash" | "shell" | "zsh" | "fish" | "rb" | "ruby" | "yaml"
        | "yml" | "toml" | "ini" | "conf" => trimmed.starts_with('#'),
        "sql" => trimmed.starts_with("--"),
        "html" | "xml" => trimmed.starts_with("<!--"),
        _ => trimmed.starts_with("//") || trimmed.starts_with('#'),
    };

    if is_comment {
        let _ = execute!(
            out,
            SetForegroundColor(Color::Rgb {
                r: 106,
                g: 153,
                b: 85
            }),
            Print(line),
            ResetColor,
        );
        return;
    }

    let keywords: &[&str] = match lang {
        "rust" | "rs" => &[
            "fn", "let", "mut", "if", "else", "match", "for", "while", "loop", "struct", "enum",
            "impl", "pub", "use", "mod", "return", "true", "false", "Some", "None", "Ok", "Err",
            "where", "type", "const", "static", "self", "Self", "async", "await", "move", "ref",
            "in", "dyn", "trait", "Box", "Vec", "String", "Option", "Result", "break", "continue",
            "unsafe", "extern", "crate", "super",
        ],
        "py" | "python" => &[
            "def", "class", "import", "from", "if", "else", "elif", "for", "while", "return",
            "True", "False", "None", "and", "or", "not", "in", "is", "lambda", "with", "as",
            "pass", "raise", "try", "except", "finally", "global", "nonlocal", "yield", "async",
            "await", "del", "break", "continue",
        ],
        "js" | "javascript" | "ts" | "typescript" | "jsx" | "tsx" => &[
            "function",
            "const",
            "let",
            "var",
            "if",
            "else",
            "for",
            "while",
            "return",
            "true",
            "false",
            "null",
            "undefined",
            "class",
            "extends",
            "import",
            "export",
            "from",
            "async",
            "await",
            "new",
            "this",
            "typeof",
            "instanceof",
            "interface",
            "type",
            "enum",
            "readonly",
            "abstract",
            "implements",
            "static",
            "public",
            "private",
            "protected",
            "break",
            "continue",
            "switch",
            "case",
            "default",
            "try",
            "catch",
            "finally",
            "throw",
            "delete",
            "void",
            "in",
            "of",
        ],
        "go" => &[
            "func",
            "var",
            "if",
            "else",
            "for",
            "return",
            "true",
            "false",
            "nil",
            "struct",
            "interface",
            "package",
            "import",
            "type",
            "const",
            "defer",
            "go",
            "chan",
            "select",
            "case",
            "default",
            "switch",
            "break",
            "continue",
            "range",
            "map",
            "make",
            "new",
            "len",
            "cap",
            "append",
        ],
        "sh" | "bash" | "shell" | "zsh" | "fish" => &[
            "if", "then", "else", "elif", "fi", "for", "do", "done", "while", "case", "esac",
            "function", "return", "local", "export", "echo", "read", "exit", "in",
        ],
        _ => &[],
    };

    scan_and_highlight_tokens(line, keywords, out);
}

/// Tokenise a line and emit ANSI colours for keywords, strings, numbers.
fn scan_and_highlight_tokens(line: &str, keywords: &[&str], out: &mut impl Write) {
    // Pi-mono syntaxXxx colours from dark.json
    const KW_R: u8 = 86;
    const KW_G: u8 = 156;
    const KW_B: u8 = 214; // syntaxKeyword  #569CD6
    const ST_R: u8 = 206;
    const ST_G: u8 = 145;
    const ST_B: u8 = 120; // syntaxString   #CE9178
    const NM_R: u8 = 181;
    const NM_G: u8 = 206;
    const NM_B: u8 = 168; // syntaxNumber   #B5CEA8
    const CM_R: u8 = 106;
    const CM_G: u8 = 153;
    const CM_B: u8 = 85; // syntaxComment  #6A9955
    const DF_R: u8 = 181;
    const DF_G: u8 = 189;
    const DF_B: u8 = 104; // mdCodeBlock    #b5bd68

    let bytes = line.as_bytes();
    let len = bytes.len();
    let mut i = 0;

    while i < len {
        let ch = bytes[i] as char;

        // `//` comment — rest of line
        if ch == '/' && i + 1 < len && bytes[i + 1] == b'/' {
            let _ = execute!(
                out,
                SetForegroundColor(Color::Rgb {
                    r: CM_R,
                    g: CM_G,
                    b: CM_B
                }),
                Print(&line[i..]),
                ResetColor,
            );
            return;
        }

        // `#` comment — rest of line (but only if not inside identifier)
        if ch == '#' && (i == 0 || !bytes[i - 1].is_ascii_alphanumeric()) {
            let _ = execute!(
                out,
                SetForegroundColor(Color::Rgb {
                    r: CM_R,
                    g: CM_G,
                    b: CM_B
                }),
                Print(&line[i..]),
                ResetColor,
            );
            return;
        }

        // String literals (double quote, back-tick; skip single-quote to avoid
        // misidentifying Rust lifetimes like `'a`).
        if ch == '"' || ch == '`' {
            let quote = bytes[i];
            let start = i;
            i += 1;
            while i < len {
                if bytes[i] == b'\\' {
                    i += 2;
                    continue;
                }
                if bytes[i] == quote {
                    i += 1;
                    break;
                }
                i += 1;
            }
            let _ = execute!(
                out,
                SetForegroundColor(Color::Rgb {
                    r: ST_R,
                    g: ST_G,
                    b: ST_B
                }),
                Print(&line[start..i]),
                ResetColor,
            );
            continue;
        }

        // Numbers (but not when inside an identifier)
        if ch.is_ascii_digit() && (i == 0 || !bytes[i - 1].is_ascii_alphabetic()) {
            let start = i;
            while i < len
                && (bytes[i].is_ascii_alphanumeric()
                    || bytes[i] == b'.'
                    || bytes[i] == b'_'
                    || bytes[i] == b'x'
                    || bytes[i] == b'b'
                    || bytes[i] == b'o')
            {
                i += 1;
            }
            let _ = execute!(
                out,
                SetForegroundColor(Color::Rgb {
                    r: NM_R,
                    g: NM_G,
                    b: NM_B
                }),
                Print(&line[start..i]),
                ResetColor,
            );
            continue;
        }

        // Identifiers / keywords
        if ch.is_ascii_alphabetic() || ch == '_' {
            let start = i;
            while i < len && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                i += 1;
            }
            let word = &line[start..i];
            let color = if keywords.contains(&word) {
                Color::Rgb {
                    r: KW_R,
                    g: KW_G,
                    b: KW_B,
                }
            } else {
                Color::Rgb {
                    r: DF_R,
                    g: DF_G,
                    b: DF_B,
                }
            };
            let _ = execute!(out, SetForegroundColor(color), Print(word), ResetColor);
            continue;
        }

        // Punctuation / operators — dim grey
        let _ = execute!(
            out,
            SetForegroundColor(Color::Rgb {
                r: 180,
                g: 185,
                b: 195
            }),
            Print(ch.to_string()),
            ResetColor,
        );
        i += 1;
    }
}

// ─── Tool display ─────────────────────────────────────────────────────────────

fn tool_border(w: usize) -> String {
    "─".repeat(w.saturating_sub(2))
}

pub fn print_tool_start(name: &str) {
    let w = terminal::size().map(|(w, _)| w as usize).unwrap_or(80);
    let _ = execute!(
        stdout(),
        Print("\n"),
        // Top border — blue, dim
        SetForegroundColor(C_BORDER),
        SetAttribute(Attribute::Dim),
        Print(format!("  {}", tool_border(w))),
        SetAttribute(Attribute::Reset),
        ResetColor,
        Print("\n"),
        // Pending indicator
        SetForegroundColor(C_DIM),
        Print("  ◦ "),
        SetForegroundColor(C_MUTED),
        SetAttribute(Attribute::Dim),
        Print(name),
        SetAttribute(Attribute::Reset),
        ResetColor,
    );
    let _ = stdout().flush();
}

pub fn print_tool_done(
    name: &str,
    input: &serde_json::Value,
    result: &str,
    is_error: bool,
    elapsed_ms: u64,
) {
    let detail = tool_detail(name, input);
    let elapsed_str = if elapsed_ms >= 1000 {
        format!("  · {:.1}s", elapsed_ms as f64 / 1000.0)
    } else if elapsed_ms > 0 {
        format!("  · {}ms", elapsed_ms)
    } else {
        String::new()
    };
    let w = terminal::size().map(|(w, _)| w as usize).unwrap_or(80);

    // Overwrite the pending "◦ name" line.
    let _ = execute!(stdout(), MoveToColumn(0), Clear(ClearType::CurrentLine));

    if is_error {
        let _ = execute!(
            stdout(),
            SetForegroundColor(C_ERROR),
            Print("  ✗ "),
            SetForegroundColor(C_TEXT),
            SetAttribute(Attribute::Bold),
            Print(name),
            SetAttribute(Attribute::Reset),
            ResetColor,
            SetForegroundColor(C_MUTED),
            Print(if detail.is_empty() {
                String::new()
            } else {
                format!("  {detail}")
            }),
            SetForegroundColor(C_DIM),
            SetAttribute(Attribute::Dim),
            Print(&elapsed_str),
            SetAttribute(Attribute::Reset),
            ResetColor,
            Print("\n"),
        );
        if !result.is_empty() {
            print_tool_output_preview(result, true);
        }
        // Bottom border — error red
        let _ = execute!(
            stdout(),
            SetForegroundColor(C_ERROR),
            SetAttribute(Attribute::Dim),
            Print(format!("  {}\n", tool_border(w))),
            SetAttribute(Attribute::Reset),
            ResetColor,
        );
    } else {
        // For bash/run_command show command prominently in bashMode green.
        let is_bash = name == "bash" || name == "run_command";
        if is_bash {
            let cmd = input["command"].as_str().unwrap_or("");
            let cmd_display = if cmd.chars().count() > 80 {
                format!(
                    "{}…",
                    &cmd[..cmd.char_indices().nth(77).map(|(i, _)| i).unwrap_or(77)]
                )
            } else {
                cmd.to_string()
            };
            let _ = execute!(
                stdout(),
                SetForegroundColor(C_SUCCESS),
                SetAttribute(Attribute::Bold),
                Print(format!("  $ {cmd_display}")),
                SetAttribute(Attribute::Reset),
                ResetColor,
                SetForegroundColor(C_DIM),
                SetAttribute(Attribute::Dim),
                Print(&elapsed_str),
                SetAttribute(Attribute::Reset),
                ResetColor,
                Print("\n"),
            );
            if !result.is_empty() {
                print_tool_output_preview(result, false);
            }
        } else {
            let _ = execute!(
                stdout(),
                SetForegroundColor(C_SUCCESS),
                Print("  ✓ "),
                SetForegroundColor(C_TEXT),
                SetAttribute(Attribute::Bold),
                Print(name),
                SetAttribute(Attribute::Reset),
                ResetColor,
                SetForegroundColor(C_MUTED),
                Print(if detail.is_empty() {
                    String::new()
                } else {
                    format!("  {detail}")
                }),
                SetForegroundColor(C_DIM),
                SetAttribute(Attribute::Dim),
                Print(&elapsed_str),
                SetAttribute(Attribute::Reset),
                ResetColor,
                Print("\n"),
            );
            if name == "edit_file" {
                if let (Some(old), Some(new)) =
                    (input["old_string"].as_str(), input["new_string"].as_str())
                {
                    print_inline_diff(old, new);
                }
            } else if name == "write_file" {
                if let Some(content) = input["content"].as_str() {
                    let lines = content.lines().count();
                    let bytes = content.len();
                    let _ = execute!(
                        stdout(),
                        SetForegroundColor(C_DIM),
                        SetAttribute(Attribute::Dim),
                        Print(format!("  └ {lines} lines · {bytes} bytes\n")),
                        SetAttribute(Attribute::Reset),
                        ResetColor,
                    );
                }
            }
        }
        // Bottom border — success green
        let _ = execute!(
            stdout(),
            SetForegroundColor(C_SUCCESS),
            SetAttribute(Attribute::Dim),
            Print(format!("  {}\n", tool_border(w))),
            SetAttribute(Attribute::Reset),
            ResetColor,
        );
    }
    let _ = stdout().flush();
}

/// Print up to 8 lines of tool output, muted, with truncation indicator.
fn print_tool_output_preview(output: &str, is_error: bool) {
    const MAX_PREVIEW: usize = 8;
    let all_lines: Vec<&str> = output.lines().filter(|l| !l.trim().is_empty()).collect();
    let total = all_lines.len();
    if total == 0 {
        return;
    }
    let show = total.min(MAX_PREVIEW);
    let col = if is_error { C_ERROR } else { C_MUTED };
    for (i, line) in all_lines[..show].iter().enumerate() {
        let is_last = i + 1 == show;
        let prefix = if is_last && total <= MAX_PREVIEW {
            "  └ "
        } else {
            "  │ "
        };
        let truncated = truncate_line(line, 120);
        let _ = execute!(
            stdout(),
            SetForegroundColor(col),
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
            SetForegroundColor(C_DIM),
            SetAttribute(Attribute::Dim),
            Print(format!("  └ … +{remaining} more lines\n")),
            SetAttribute(Attribute::Reset),
            ResetColor,
        );
    }
    let _ = stdout().flush();
}

// ─── Inline diff ──────────────────────────────────────────────────────────────

/// Show a colored unified-style diff between old and new strings.
/// Uses LCS to compute the diff; caps display at 12 diff lines.
/// For single-line changes (1 removed + 1 added), highlights changed words.
pub fn print_inline_diff(old: &str, new: &str) {
    const MAX_DIFF_LINES: usize = 12;
    let old_lines: Vec<&str> = old.lines().collect();
    let new_lines: Vec<&str> = new.lines().collect();

    let diff = lcs_diff(&old_lines, &new_lines);
    let out = &mut stdout();

    let changes: Vec<_> = diff
        .iter()
        .filter(|d| !matches!(d, DiffOp::Same(_)))
        .collect();
    if changes.is_empty() {
        return;
    }

    // Collect diff ops into a flat list so we can look ahead for word-diff pairing.
    let ops: Vec<&DiffOp> = diff.iter().collect();
    let mut i = 0;
    let mut shown = 0usize;
    let mut hidden = 0usize;

    while i < ops.len() {
        if shown >= MAX_DIFF_LINES {
            match ops[i] {
                DiffOp::Same(_) => {}
                _ => hidden += 1,
            }
            i += 1;
            continue;
        }
        match ops[i] {
            DiffOp::Remove(old_line) => {
                // Peek ahead: if the next non-Same op is Add, do word-level diff.
                let next_add = ops[i + 1..]
                    .iter()
                    .find(|op| !matches!(op, DiffOp::Same(_)));
                if let Some(DiffOp::Add(new_line)) = next_add {
                    // Single-line replacement — highlight changed words.
                    let (hl_old, hl_new) = word_diff_highlight(old_line, new_line);
                    let _ = execute!(out, SetForegroundColor(C_ERROR), Print("  − "), ResetColor,);
                    print!("{hl_old}");
                    let _ = execute!(out, ResetColor, Print("\n"));

                    let _ = execute!(
                        out,
                        SetForegroundColor(C_SUCCESS),
                        Print("  + "),
                        ResetColor,
                    );
                    print!("{hl_new}");
                    let _ = execute!(out, ResetColor, Print("\n"));

                    shown += 2;
                    // Skip the paired Add op.
                    i += 1;
                    while i < ops.len() {
                        if let DiffOp::Add(_) = ops[i] {
                            i += 1;
                            break;
                        }
                        i += 1;
                    }
                    continue;
                } else {
                    let _ = execute!(
                        out,
                        SetForegroundColor(C_ERROR),
                        Print("  − "),
                        SetForegroundColor(Color::Rgb {
                            r: 220,
                            g: 140,
                            b: 140
                        }),
                        Print(truncate_line(old_line, 100)),
                        ResetColor,
                        Print("\n"),
                    );
                    shown += 1;
                }
            }
            DiffOp::Add(line) => {
                let _ = execute!(
                    out,
                    SetForegroundColor(C_SUCCESS),
                    Print("  + "),
                    SetForegroundColor(Color::Rgb {
                        r: 200,
                        g: 210,
                        b: 140
                    }),
                    Print(truncate_line(line, 100)),
                    ResetColor,
                    Print("\n"),
                );
                shown += 1;
            }
            DiffOp::Same(_) => {}
        }
        i += 1;
    }
    if hidden > 0 {
        let _ = execute!(
            out,
            SetForegroundColor(Color::Rgb {
                r: 68,
                g: 76,
                b: 92
            }),
            SetAttribute(Attribute::Dim),
            Print(format!("  … {hidden} more lines\n")),
            SetAttribute(Attribute::Reset),
            ResetColor,
        );
    }
    let _ = out.flush();
}

/// Return (highlighted_old, highlighted_new) ANSI strings with changed words
/// shown in inverse video, matching pi-mono's intra-line diff style.
fn word_diff_highlight(old: &str, new: &str) -> (String, String) {
    // Split into words (whitespace-separated tokens, keeping whitespace).
    fn tokenize(s: &str) -> Vec<&str> {
        let mut tokens = Vec::new();
        let mut start = 0;
        let bytes = s.as_bytes();
        let mut in_ws = bytes
            .first()
            .map(|b| b.is_ascii_whitespace())
            .unwrap_or(false);
        for i in 1..=bytes.len() {
            let is_ws = i < bytes.len() && bytes[i].is_ascii_whitespace();
            if is_ws != in_ws {
                tokens.push(&s[start..i]);
                start = i;
                in_ws = is_ws;
            }
        }
        if start < s.len() {
            tokens.push(&s[start..]);
        }
        tokens
    }

    let old_toks = tokenize(old);
    let new_toks = tokenize(new);

    // Word-level LCS.
    let m = old_toks.len();
    let n = new_toks.len();
    let mut dp = vec![vec![0u16; n + 1]; m + 1];
    for i in 1..=m {
        for j in 1..=n {
            dp[i][j] = if old_toks[i - 1] == new_toks[j - 1] {
                dp[i - 1][j - 1] + 1
            } else {
                dp[i - 1][j].max(dp[i][j - 1])
            };
        }
    }
    // Backtrack.
    enum WOp {
        Same(usize, usize),
        Del(usize),
        Ins(usize),
    }
    let mut wops: Vec<WOp> = Vec::new();
    let mut i = m;
    let mut j = n;
    while i > 0 || j > 0 {
        if i > 0 && j > 0 && old_toks[i - 1] == new_toks[j - 1] {
            wops.push(WOp::Same(i - 1, j - 1));
            i -= 1;
            j -= 1;
        } else if j > 0 && (i == 0 || dp[i][j - 1] >= dp[i - 1][j]) {
            wops.push(WOp::Ins(j - 1));
            j -= 1;
        } else {
            wops.push(WOp::Del(i - 1));
            i -= 1;
        }
    }
    wops.reverse();

    // Build ANSI strings: changed words get inverse video.
    // Old: removed words in red+inverse, same words in normal red.
    // New: added words in green+inverse, same words in normal green.
    let mut old_out = String::new();
    let mut new_out = String::new();
    for op in &wops {
        match op {
            WOp::Same(oi, ni) => {
                old_out.push_str(&format!("\x1b[38;2;205;125;125m{}\x1b[0m", old_toks[*oi]));
                new_out.push_str(&format!("\x1b[38;2;125;195;148m{}\x1b[0m", new_toks[*ni]));
            }
            WOp::Del(oi) => {
                old_out.push_str(&format!(
                    "\x1b[38;2;235;100;100m\x1b[7m{}\x1b[0m",
                    old_toks[*oi]
                ));
            }
            WOp::Ins(ni) => {
                new_out.push_str(&format!(
                    "\x1b[38;2;80;210;120m\x1b[7m{}\x1b[0m",
                    new_toks[*ni]
                ));
            }
        }
    }
    (old_out, new_out)
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
            dp[i][j] = if old[i - 1] == new[j - 1] {
                dp[i - 1][j - 1] + 1
            } else {
                dp[i - 1][j].max(dp[i][j - 1])
            };
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
        format!(
            "{}…",
            &trimmed[..trimmed
                .char_indices()
                .nth(max)
                .map(|(i, _)| i)
                .unwrap_or(trimmed.len())]
        )
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
                format!(
                    "{}…",
                    &cmd[..cmd.char_indices().nth(61).map(|(i, _)| i).unwrap_or(61)]
                )
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
        SetForegroundColor(C_DIM),
        Print("  · "),
        SetForegroundColor(C_MUTED),
        Print(msg),
        ResetColor,
        Print("\n"),
    );
}

pub fn print_success(msg: &str) {
    let _ = execute!(
        stdout(),
        SetForegroundColor(C_SUCCESS),
        Print("  ✓ "),
        SetForegroundColor(C_TEXT),
        Print(msg),
        ResetColor,
        Print("\n"),
    );
}

pub fn print_warning(msg: &str) {
    let _ = execute!(
        stdout(),
        SetForegroundColor(C_WARNING),
        Print("  ⚠  "),
        SetForegroundColor(C_HEADING),
        Print(msg),
        ResetColor,
        Print("\n"),
    );
}

pub fn print_error(msg: &str) {
    let _ = execute!(
        stdout(),
        SetForegroundColor(C_ERROR),
        Print("  ✗ "),
        SetForegroundColor(C_TEXT),
        Print(msg),
        ResetColor,
        Print("\n"),
    );
}

/// Thin separator printed before each assistant response starts.
#[allow(dead_code)]
pub fn print_turn_divider() {
    let w = terminal::size().map(|(w, _)| w as usize).unwrap_or(80);
    let _ = execute!(
        stdout(),
        SetForegroundColor(C_DIM),
        SetAttribute(Attribute::Dim),
        Print(format!("  {}\n", "─".repeat(w.saturating_sub(4)))),
        ResetColor,
        SetAttribute(Attribute::Reset),
    );
}

/// Format token count with k/M suffix.
fn fmt_tokens(n: u32) -> String {
    if n < 1_000 {
        n.to_string()
    } else if n < 10_000 {
        format!("{:.1}k", n as f64 / 1_000.0)
    } else if n < 1_000_000 {
        format!("{}k", n / 1_000)
    } else {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    }
}

/// Rough cost rates ($/M tokens) for a given model string.
fn model_cost_rates(model: &str) -> (f64, f64) {
    if model.contains("opus") {
        (15.0, 75.0)
    } else if model.contains("sonnet") {
        (3.0, 15.0)
    } else if model.contains("haiku") {
        (0.8, 4.0)
    } else if model.contains("gpt-4.1") || model.contains("gpt-4o") {
        (2.0, 8.0)
    } else if model.contains("o3") || model.contains("o4") {
        (10.0, 40.0)
    } else if model.contains("gemini-2.5") {
        (1.25, 10.0)
    } else if model.contains("gemini") {
        (0.075, 0.30)
    } else if model.contains("deepseek") {
        (0.14, 0.28)
    } else {
        (1.0, 4.0)
    }
}

/// Print a compact post-turn footer: token counts, cost estimate, context %, and model.
/// Replaces the old bare cost hint + separate context warning.
pub fn print_turn_footer(
    total_in: u32,
    total_out: u32,
    cache_write: u32,
    cache_read: u32,
    model: &str,
    ctx_window: u32,
    ctx_used: u32,
) {
    let w = terminal::size().map(|(w, _)| w as usize).unwrap_or(80);

    // ── Line 1: ~path (branch) ────────────────────────────────────────────────
    let pwd = std::env::current_dir()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_default();
    let pwd_display = if !home.is_empty() && pwd.starts_with(&home) {
        format!("~{}", &pwd[home.len()..])
    } else {
        pwd
    };
    let branch = git_branch();
    let path_line = if branch.is_empty() {
        pwd_display.clone()
    } else {
        format!("{pwd_display} ({branch})")
    };
    // Truncate if too wide.
    let path_truncated: String = if path_line.chars().count() > w.saturating_sub(1) {
        let mut s: String = path_line.chars().take(w.saturating_sub(2)).collect();
        s.push('…');
        s
    } else {
        path_line
    };
    println!("\x1b[38;2;102;102;102m{path_truncated}\x1b[0m");

    // ── Line 2: stats left + model right ─────────────────────────────────────
    let (in_rate, out_rate) = model_cost_rates(model);
    let cost =
        (total_in as f64 / 1_000_000.0) * in_rate + (total_out as f64 / 1_000_000.0) * out_rate;
    let cost_str = if cost >= 0.01 {
        format!("${cost:.3}")
    } else if cost >= 0.0001 {
        format!("${cost:.4}")
    } else {
        String::new()
    };

    let ctx_pct = if ctx_window > 0 {
        (ctx_used as f64 * 100.0) / ctx_window as f64
    } else {
        0.0
    };
    let ctx_str = format!("{:.1}%/{}", ctx_pct, fmt_tokens(ctx_window));

    // Colorize context % — break out of dim, apply warning/error, return to dim.
    let ctx_colored = if ctx_pct >= 90.0 {
        format!("\x1b[0m\x1b[38;2;204;102;102m{ctx_str}\x1b[0m\x1b[38;2;102;102;102m")
    } else if ctx_pct >= 70.0 {
        format!("\x1b[0m\x1b[38;2;255;255;0m{ctx_str}\x1b[0m\x1b[38;2;102;102;102m")
    } else {
        ctx_str.clone()
    };

    let mut stat_parts: Vec<String> = Vec::new();
    if total_in > 0 {
        stat_parts.push(format!("↑{}", fmt_tokens(total_in)));
    }
    if total_out > 0 {
        stat_parts.push(format!("↓{}", fmt_tokens(total_out)));
    }
    if cache_write > 0 {
        stat_parts.push(format!("⚡{}", fmt_tokens(cache_write)));
    }
    if cache_read > 0 {
        stat_parts.push(format!("♻{}", fmt_tokens(cache_read)));
    }
    if !cost_str.is_empty() {
        stat_parts.push(cost_str);
    }
    stat_parts.push(ctx_colored);

    let stats_plain_len: usize = {
        let mut parts_for_len: Vec<String> = Vec::new();
        if total_in > 0 {
            parts_for_len.push(format!("↑{}", fmt_tokens(total_in)));
        }
        if total_out > 0 {
            parts_for_len.push(format!("↓{}", fmt_tokens(total_out)));
        }
        if cache_write > 0 {
            parts_for_len.push(format!("⚡{}", fmt_tokens(cache_write)));
        }
        if cache_read > 0 {
            parts_for_len.push(format!("♻{}", fmt_tokens(cache_read)));
        }
        // cost_str length
        let cost_raw = if cost >= 0.01 {
            format!("${cost:.3}")
        } else if cost >= 0.0001 {
            format!("${cost:.4}")
        } else {
            String::new()
        };
        if !cost_raw.is_empty() {
            parts_for_len.push(cost_raw);
        }
        parts_for_len.push(ctx_str.clone());
        parts_for_len.join(" ").len()
    };

    let right = if model.len() > 40 {
        &model[..40]
    } else {
        model
    };
    let right_len = right.len();
    let pad = w.saturating_sub(stats_plain_len + right_len + 2).max(2);

    let stats_ansi = stat_parts.join(" ");
    println!(
        "\x1b[38;2;102;102;102m{stats_ansi}{}{right}\x1b[0m",
        " ".repeat(pad),
    );

    // Context warnings.
    if ctx_pct >= 90.0 {
        print_warning("Context 90%+ full — run /compact now or start /new session.");
    } else if ctx_pct >= 70.0 {
        print_warning(&format!(
            "Context at {ctx_pct:.0}% — consider /compact soon."
        ));
    }
}

/// Get the current git branch name, or empty string if not in a git repo.
fn git_branch() -> String {
    std::process::Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .output()
        .ok()
        .and_then(|o| {
            if o.status.success() {
                String::from_utf8(o.stdout).ok()
            } else {
                None
            }
        })
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

pub fn print_section_header(title: &str) {
    let w = terminal::size()
        .map(|(w, _)| w as usize)
        .unwrap_or(80)
        .min(60);
    let line = "─".repeat(w.saturating_sub(4));
    let _ = execute!(
        stdout(),
        SetForegroundColor(C_ACCENT),
        SetAttribute(Attribute::Bold),
        Print(format!("  {title}")),
        ResetColor,
        SetAttribute(Attribute::Reset),
        Print("\n"),
        SetForegroundColor(C_DIM),
        SetAttribute(Attribute::Dim),
        Print(format!("  {line}")),
        ResetColor,
        SetAttribute(Attribute::Reset),
        Print("\n"),
    );
}

/// Print a welcome banner with provider status.
pub fn print_welcome_banner(provider_info: &str, auth_store: &dcode_providers::AuthStore) {
    const DIM: &str = "\x1b[38;2;102;102;102m";
    const MUTED: &str = "\x1b[38;2;128;128;128m";
    const ACCNT: &str = "\x1b[38;2;138;190;183m";
    const BORDER: &str = "\x1b[38;2;60;65;80m";
    const RST: &str = "\x1b[0m";
    const BOLD: &str = "\x1b[1m";

    let version = env!("CARGO_PKG_VERSION");
    let w = terminal::size().map(|(w, _)| w as usize).unwrap_or(80);

    println!();

    // ── Logo with gradient accent ─────────────────────────────────────────────
    // Gradient from teal→cyan across "d-code"
    let logo_chars = ['d', '-', 'c', 'o', 'd', 'e'];
    let logo_colors: [(u8, u8, u8); 6] = [
        (100, 175, 170),
        (110, 180, 175),
        (120, 190, 183),
        (138, 195, 190),
        (150, 205, 200),
        (160, 215, 210),
    ];
    print!("  {BOLD}");
    for (ch, (r, g, b)) in logo_chars.iter().zip(logo_colors.iter()) {
        print!("\x1b[38;2;{r};{g};{b}m{ch}");
    }
    println!("{RST} {DIM}v{version}{RST}");
    println!();

    // ── Keybinding hints (compact 2-column grid) ──────────────────────────────
    let hints: &[(&str, &str)] = &[
        ("^C", "exit"),
        ("^G", "editor"),
        ("^P/N", "models"),
        ("S-Tab", "thinking"),
        ("S+↵", "newline"),
        ("/", "commands"),
        ("!", "bash"),
        ("!!", "bash (quiet)"),
    ];
    let col_width: usize = 24;
    let mut i = 0;
    while i < hints.len() {
        print!("  ");
        for j in 0..2 {
            if i + j < hints.len() {
                let (key, desc) = hints[i + j];
                let cell = format!("{key} {desc}");
                let pad = col_width.saturating_sub(cell.len());
                print!("{DIM}{key}{RST} {MUTED}{desc}{RST}{}", " ".repeat(pad));
            }
        }
        println!();
        i += 2;
    }
    println!();

    // ── Provider status (pills with colored indicators) ───────────────────────
    let providers: &[(&str, bool)] = &[
        ("anthropic", auth_store.anthropic.is_some()),
        ("copilot", auth_store.copilot.is_some()),
        (
            "openai",
            auth_store.openai.is_some() || auth_store.openai_oauth.is_some(),
        ),
        (
            "gemini",
            auth_store.gemini.is_some() || std::env::var("GEMINI_API_KEY").is_ok(),
        ),
        (
            "openrouter",
            auth_store.openrouter.is_some() || std::env::var("OPENROUTER_API_KEY").is_ok(),
        ),
        ("antigravity", auth_store.antigravity.is_some()),
    ];

    // Active provider (from provider_info "copilot/gpt-4o")
    let active_provider = provider_info.split('/').next().unwrap_or("");

    print!("  ");
    for (label, authenticated) in providers {
        let is_active = *label == active_provider;
        if is_active {
            // Active: bright accent with filled dot
            print!("{ACCNT}{BOLD}● {label}{RST}  ");
        } else if *authenticated {
            // Authenticated but not active: muted with filled dot
            print!("{MUTED}● {label}{RST}  ");
        } else {
            // Not authenticated: dim with hollow dot
            print!("{DIM}○ {label}{RST}  ");
        }
    }
    println!();

    // ── Separator ─────────────────────────────────────────────────────────────
    println!("  {BORDER}{}{RST}", "─".repeat(w.saturating_sub(4)));
    println!();
}

/// Print a condensed replay of a session's conversation for context on resume.
/// Shows up to `max_turns` turns, each message truncated to fit the terminal.
pub fn print_session_recap(messages: &[dcode_providers::Message], max_turns: usize) {
    use dcode_providers::{ContentBlock, Role};

    let w = terminal::size()
        .map(|(w, _)| w as usize)
        .unwrap_or(80)
        .min(80);
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
                            if !user_text.is_empty() {
                                user_text.push(' ');
                            }
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
                            if !asst_text.is_empty() {
                                asst_text.push(' ');
                            }
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
            SetForegroundColor(C_BORDER),
            Print("  You  "),
            ResetColor,
            SetForegroundColor(C_TEXT),
            Print(format!("{user_trunc}\n")),
            ResetColor,
        );

        // Assistant line.
        let asst_trunc = truncate_to(asst, msg_width);
        let _ = execute!(
            stdout(),
            SetForegroundColor(C_ACCENT),
            Print("  d-code  "),
            ResetColor,
            SetForegroundColor(C_MUTED),
            Print(format!("{asst_trunc}\n")),
            ResetColor,
        );
        println!();
    }
    println!("{sep}");
}

/// Human-readable relative time for a UTC RFC3339 timestamp.
pub fn time_ago_from_rfc3339(rfc3339: &str) -> String {
    let Ok(dt) = chrono::DateTime::parse_from_rfc3339(rfc3339) else {
        return rfc3339.to_string();
    };
    let now = chrono::Local::now();
    let secs = (now.signed_duration_since(dt)).num_seconds();
    match secs {
        s if s < 60 => "just now".into(),
        s if s < 3600 => format!("{}m ago", s / 60),
        s if s < 86400 => format!("{}h ago", s / 3600),
        s if s < 604800 => format!("{}d ago", s / 86400),
        s => format!("{}w ago", s / 604800),
    }
}

/// Print a tree of sessions showing parent-child (fork) relationships.
pub fn print_session_tree(sessions: &[crate::sessions::SavedSession], current_id: Option<&str>) {
    use std::collections::HashMap;

    println!();
    println!("  \x1b[1mSession tree\x1b[0m");
    println!();

    // Build parent → children map.
    let mut children: HashMap<Option<String>, Vec<usize>> = HashMap::new();
    for (i, s) in sessions.iter().enumerate() {
        children.entry(s.parent_id.clone()).or_default().push(i);
    }

    fn print_node(
        sessions: &[crate::sessions::SavedSession],
        children: &HashMap<Option<String>, Vec<usize>>,
        id: Option<&str>,
        depth: usize,
        current_id: Option<&str>,
    ) {
        let key: Option<String> = id.map(|s| s.to_string());
        let Some(child_indices) = children.get(&key) else {
            return;
        };

        for &idx in child_indices {
            let s = &sessions[idx];
            let indent = "  ".repeat(depth + 1);
            let marker = if depth == 0 { "●" } else { "⎇" };
            let is_current = current_id == Some(s.id.as_str());
            let title = s.display_title();
            let ago = time_ago_from_rfc3339(&s.updated_at);
            let turns = s.turn_count;
            if is_current {
                println!("  {indent}\x1b[32m{marker} {title}\x1b[0m  \x1b[2m({turns} turns · {ago}) ← current\x1b[0m");
            } else {
                println!("  {indent}\x1b[2m{marker}\x1b[0m {title}  \x1b[2m({turns} turns · {ago})\x1b[0m");
            }
            print_node(sessions, children, Some(&s.id), depth + 1, current_id);
        }
    }

    print_node(sessions, &children, None, 0, current_id);
    println!();
}

/// Prompt the user to confirm a dangerous bash command.
/// Returns true if approved. Called synchronously from inside the agent loop.
pub fn confirm_dangerous_bash(cmd: &str) -> bool {
    let _ = execute!(
        stdout(),
        Print("\n"),
        SetForegroundColor(C_WARNING),
        SetAttribute(Attribute::Bold),
        Print(format!("  ⚠  $ {cmd}\n")),
        SetAttribute(Attribute::Reset),
        ResetColor,
        SetForegroundColor(C_MUTED),
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
            SetForegroundColor(C_ERROR),
            Print("  Blocked.\n"),
            ResetColor
        );
    }
    approved
}

/// Prompt the user with a question from the AI's ask_user tool.
/// Returns the user's text answer.
pub fn prompt_user_question(question: &str, choices: &[String]) -> String {
    use crossterm::style::{Attribute, ResetColor, SetAttribute, SetForegroundColor};
    println!();
    let _ = execute!(
        stdout(),
        SetForegroundColor(C_ACCENT),
        SetAttribute(Attribute::Bold),
        Print("  ? "),
        ResetColor,
        SetForegroundColor(C_TEXT),
        SetAttribute(Attribute::Reset),
        Print(question),
        Print("\n"),
        ResetColor,
    );
    if !choices.is_empty() {
        for (i, choice) in choices.iter().enumerate() {
            let _ = execute!(
                stdout(),
                SetForegroundColor(C_BORDER),
                Print(format!("    {}. ", i + 1)),
                ResetColor,
                SetForegroundColor(C_TEXT),
                Print(format!("{choice}\n")),
                ResetColor,
            );
        }
        let _ = execute!(
            stdout(),
            SetForegroundColor(C_MUTED),
            Print(format!("  Choice [1-{}] or type answer: ", choices.len())),
            ResetColor,
        );
    } else {
        let _ = execute!(
            stdout(),
            SetForegroundColor(C_MUTED),
            Print("  Answer: "),
            ResetColor
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

#[allow(dead_code)]
fn visible_str_len(s: &str) -> usize {
    let mut len = 0usize;
    let mut esc = false;
    for ch in s.chars() {
        if esc {
            if ch == 'm' {
                esc = false;
            }
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
#[allow(clippy::too_many_arguments)]
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

        for (rel_i, &item_idx) in filtered[scroll_offset..visible_end].iter().enumerate() {
            let vis_i = scroll_offset + rel_i;
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
