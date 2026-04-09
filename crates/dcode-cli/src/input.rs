/// Raw-mode line editor with cursor movement, history, tab completion,
/// live slash-command dropdown, placeholder text, and word-editing shortcuts.
use std::io::{self, Write};

use crossterm::cursor::{MoveToColumn, MoveUp};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::style::{Attribute, Color, Print, ResetColor, SetAttribute, SetForegroundColor};
use crossterm::terminal::{self, Clear, ClearType};
use crossterm::{execute, queue};

// ─── Public types ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadOutcome {
    Submit(String),
    /// Ctrl-C on non-empty input — clear line, REPL continues.
    Cancel,
    /// Ctrl-C on empty input, or Ctrl-D — exit.
    Exit,
    /// Ctrl-P (forward) / Ctrl-N (backward) — cycle to next/previous model.
    CycleModel { forward: bool },
}

// ─── Raw mode guard ───────────────────────────────────────────────────────────

struct RawModeGuard;
impl RawModeGuard {
    fn enable() -> io::Result<Self> {
        terminal::enable_raw_mode()?;
        Ok(Self)
    }
}
impl Drop for RawModeGuard {
    fn drop(&mut self) {
        let _ = terminal::disable_raw_mode();
    }
}

// ─── Edit session ─────────────────────────────────────────────────────────────

struct Session {
    text: String,
    cursor: usize,            // byte offset
    rendered_rows: usize,     // total rows rendered last time
    cursor_row_offset: usize, // how many rows UP from bottom is cursor
    comp_sel: Option<usize>,  // highlighted dropdown index
}

impl Session {
    fn new() -> Self {
        Self {
            text: String::new(),
            cursor: 0,
            rendered_rows: 0,
            cursor_row_offset: 0,
            comp_sel: None,
        }
    }

    fn insert(&mut self, ch: char) {
        let mut buf = [0u8; 4];
        let s = ch.encode_utf8(&mut buf);
        self.text.insert_str(self.cursor, s);
        self.cursor += s.len();
        self.comp_sel = None;
    }

    fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let prev = prev_boundary(&self.text, self.cursor);
        self.text.drain(prev..self.cursor);
        self.cursor = prev;
        self.comp_sel = None;
    }

    fn delete(&mut self) {
        if self.cursor >= self.text.len() {
            return;
        }
        let next = next_boundary(&self.text, self.cursor);
        self.text.drain(self.cursor..next);
    }

    fn move_left(&mut self) {
        self.cursor = prev_boundary(&self.text, self.cursor);
    }
    fn move_right(&mut self) {
        self.cursor = next_boundary(&self.text, self.cursor);
    }

    fn move_home(&mut self) {
        self.cursor = match self.text[..self.cursor].rfind('\n') {
            Some(p) => p + 1,
            None => 0,
        };
    }

    fn move_end(&mut self) {
        self.cursor += match self.text[self.cursor..].find('\n') {
            Some(p) => p,
            None => self.text.len() - self.cursor,
        };
    }

    /// Ctrl+Left / Ctrl+B-word: jump to previous word start.
    fn word_left(&mut self) {
        // Skip whitespace, then skip word chars.
        while self.cursor > 0 {
            let prev = prev_boundary(&self.text, self.cursor);
            let ch = self.text[prev..self.cursor].chars().next().unwrap_or(' ');
            if ch.is_alphanumeric() || ch == '_' {
                break;
            }
            self.cursor = prev;
        }
        while self.cursor > 0 {
            let prev = prev_boundary(&self.text, self.cursor);
            let ch = self.text[prev..self.cursor].chars().next().unwrap_or(' ');
            if !ch.is_alphanumeric() && ch != '_' {
                break;
            }
            self.cursor = prev;
        }
    }

    /// Ctrl+Right / Ctrl+F-word: jump past next word end.
    fn word_right(&mut self) {
        let len = self.text.len();
        // Skip whitespace first.
        while self.cursor < len {
            let next = next_boundary(&self.text, self.cursor);
            let ch = self.text[self.cursor..next].chars().next().unwrap_or(' ');
            if ch.is_alphanumeric() || ch == '_' {
                break;
            }
            self.cursor = next;
        }
        // Skip word chars.
        while self.cursor < len {
            let next = next_boundary(&self.text, self.cursor);
            let ch = self.text[self.cursor..next].chars().next().unwrap_or(' ');
            if !ch.is_alphanumeric() && ch != '_' {
                break;
            }
            self.cursor = next;
        }
    }

    fn kill_to_line_start(&mut self) {
        let start = match self.text[..self.cursor].rfind('\n') {
            Some(p) => p + 1,
            None => 0,
        };
        self.text.drain(start..self.cursor);
        self.cursor = start;
        self.comp_sel = None;
    }

    fn kill_to_line_end(&mut self) {
        let end = match self.text[self.cursor..].find('\n') {
            Some(p) => self.cursor + p,
            None => self.text.len(),
        };
        self.text.drain(self.cursor..end);
        self.comp_sel = None;
    }

    /// Ctrl+W / Alt+Backspace: delete from cursor back to the previous word boundary.
    fn delete_word_back(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let end = self.cursor;
        // Skip trailing whitespace (but not newlines, which are intentional).
        while self.cursor > 0 {
            let prev = prev_boundary(&self.text, self.cursor);
            let ch = self.text[prev..self.cursor].chars().next().unwrap_or('\n');
            if ch == '\n' {
                break;
            }
            if ch.is_whitespace() {
                self.cursor = prev;
            } else {
                break;
            }
        }
        // Skip word chars.
        while self.cursor > 0 {
            let prev = prev_boundary(&self.text, self.cursor);
            let ch = self.text[prev..self.cursor].chars().next().unwrap_or(' ');
            if ch.is_whitespace() {
                break;
            }
            self.cursor = prev;
        }
        self.text.drain(self.cursor..end);
        self.comp_sel = None;
    }
}

fn prev_boundary(s: &str, pos: usize) -> usize {
    if pos == 0 {
        return 0;
    }
    let mut p = pos - 1;
    while p > 0 && !s.is_char_boundary(p) {
        p -= 1;
    }
    p
}

fn next_boundary(s: &str, pos: usize) -> usize {
    if pos >= s.len() {
        return s.len();
    }
    let mut p = pos + 1;
    while p <= s.len() && !s.is_char_boundary(p) {
        p += 1;
    }
    p
}

// ─── LineEditor ───────────────────────────────────────────────────────────────

pub struct LineEditor {
    prompt: String,
    pub history: Vec<String>,
    completions: Vec<String>,
}

impl LineEditor {
    pub fn new(prompt: impl Into<String>, completions: Vec<String>) -> Self {
        Self {
            prompt: prompt.into(),
            history: Vec::new(),
            completions,
        }
    }

    pub fn set_prompt(&mut self, prompt: impl Into<String>) {
        self.prompt = prompt.into();
    }

    pub fn push_history(&mut self, entry: impl Into<String>) {
        let s = entry.into();
        if !s.trim().is_empty() {
            self.history.push(s);
        }
    }

    pub fn read_line(&mut self) -> io::Result<ReadOutcome> {
        let _raw = RawModeGuard::enable()?;
        let mut out = io::stdout();
        let mut sess = Session::new();
        let mut hist_idx: Option<usize> = None;
        let mut hist_snap = String::new();
        let mut tab_prefix = String::new();
        let mut tab_idx: Option<usize> = None;

        self.render(&mut sess, &mut out)?;

        loop {
            let Ok(Event::Key(key)) = event::read() else {
                continue;
            };
            if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
                continue;
            }

            if key.code != KeyCode::Tab {
                tab_idx = None;
            }

            match key {
                // ── Exit / cancel ─────────────────────────────────────────
                KeyEvent {
                    code: KeyCode::Char('c'),
                    modifiers,
                    ..
                } if modifiers.contains(KeyModifiers::CONTROL) => {
                    self.erase(&mut sess, &mut out)?;
                    execute!(out, Print("\r\n"))?;
                    return Ok(if sess.text.is_empty() {
                        ReadOutcome::Exit
                    } else {
                        ReadOutcome::Cancel
                    });
                }
                KeyEvent {
                    code: KeyCode::Char('d'),
                    modifiers,
                    ..
                } if modifiers.contains(KeyModifiers::CONTROL) => {
                    self.erase(&mut sess, &mut out)?;
                    execute!(out, Print("\r\n"))?;
                    return Ok(ReadOutcome::Exit);
                }
                // ── Ctrl-U (kill to line start) ───────────────────────────
                KeyEvent {
                    code: KeyCode::Char('u'),
                    modifiers,
                    ..
                } if modifiers.contains(KeyModifiers::CONTROL) => {
                    sess.kill_to_line_start();
                    hist_idx = None;
                }
                // ── Ctrl-K (kill to line end) ──────────────────────────────
                KeyEvent {
                    code: KeyCode::Char('k'),
                    modifiers,
                    ..
                } if modifiers.contains(KeyModifiers::CONTROL) => {
                    sess.kill_to_line_end();
                    hist_idx = None;
                }
                // ── Ctrl-G: open external editor ($VISUAL / $EDITOR) ──────
                KeyEvent {
                    code: KeyCode::Char('g'),
                    modifiers,
                    ..
                } if modifiers.contains(KeyModifiers::CONTROL) => {
                    sess.rendered_rows = 0;
                    sess.cursor_row_offset = 0;
                    match open_in_editor(&sess.text, &mut out) {
                        Ok(text) => {
                            sess.text = text;
                            sess.cursor = sess.text.len();
                            sess.comp_sel = None;
                        }
                        Err(_) => {}
                    }
                }
                // ── Ctrl-P / Ctrl-N: cycle model ────────────────────────────
                KeyEvent {
                    code: KeyCode::Char('p'),
                    modifiers,
                    ..
                } if modifiers.contains(KeyModifiers::CONTROL) => {
                    self.erase(&mut sess, &mut out)?;
                    execute!(out, Print("\r\n"))?;
                    return Ok(ReadOutcome::CycleModel { forward: true });
                }
                KeyEvent {
                    code: KeyCode::Char('n'),
                    modifiers,
                    ..
                } if modifiers.contains(KeyModifiers::CONTROL) => {
                    self.erase(&mut sess, &mut out)?;
                    execute!(out, Print("\r\n"))?;
                    return Ok(ReadOutcome::CycleModel { forward: false });
                }
                // ── Ctrl-A (go to line start) ─────────────────────────────
                KeyEvent {
                    code: KeyCode::Char('a'),
                    modifiers,
                    ..
                } if modifiers.contains(KeyModifiers::CONTROL) => {
                    sess.move_home();
                }
                // ── Ctrl-E (go to line end) ───────────────────────────────
                KeyEvent {
                    code: KeyCode::Char('e'),
                    modifiers,
                    ..
                } if modifiers.contains(KeyModifiers::CONTROL) => {
                    sess.move_end();
                }
                // ── Ctrl-W (delete word backwards) ────────────────────────
                KeyEvent {
                    code: KeyCode::Char('w'),
                    modifiers,
                    ..
                } if modifiers.contains(KeyModifiers::CONTROL) => {
                    sess.delete_word_back();
                    hist_idx = None;
                }
                // ── Alt-Backspace (delete word backwards) ─────────────────
                KeyEvent {
                    code: KeyCode::Backspace,
                    modifiers,
                    ..
                } if modifiers.contains(KeyModifiers::ALT) => {
                    sess.delete_word_back();
                    hist_idx = None;
                }
                // ── Submit ────────────────────────────────────────────────
                KeyEvent {
                    code: KeyCode::Enter,
                    modifiers,
                    ..
                } if !modifiers.contains(KeyModifiers::SHIFT) => {
                    // If a dropdown item is highlighted → apply and submit immediately.
                    if let Some(sel) = sess.comp_sel {
                        let matches = self.dropdown_matches(&sess.text);
                        if let Some(&item) = matches.get(sel) {
                            sess.text = item.to_string();
                            sess.cursor = sess.text.len();
                            sess.comp_sel = None;
                            self.finalize(&mut sess, &mut out)?;
                            return Ok(ReadOutcome::Submit(sess.text.clone()));
                        }
                    }
                    // If exactly one match and our text isn't it yet → auto-complete & submit.
                    let matches = self.dropdown_matches(&sess.text);
                    if matches.len() == 1 && matches[0] != sess.text.as_str() {
                        let completed = matches[0].to_string();
                        sess.text = completed;
                        sess.cursor = sess.text.len();
                        sess.comp_sel = None;
                        self.finalize(&mut sess, &mut out)?;
                        return Ok(ReadOutcome::Submit(sess.text.clone()));
                    }
                    let text = sess.text.clone();
                    sess.cursor = text.len();
                    self.finalize(&mut sess, &mut out)?;
                    return Ok(ReadOutcome::Submit(text));
                }
                // ── Shift-Enter (newline) ─────────────────────────────────
                KeyEvent {
                    code: KeyCode::Enter,
                    ..
                } => {
                    sess.insert('\n');
                    hist_idx = None;
                }
                // ── Arrow Down: navigate dropdown if showing ──────────────
                KeyEvent {
                    code: KeyCode::Down,
                    ..
                } => {
                    let matches = self.dropdown_matches(&sess.text);
                    if !matches.is_empty() {
                        let next = match sess.comp_sel {
                            None => 0,
                            Some(i) => (i + 1).min(matches.len() - 1),
                        };
                        sess.comp_sel = Some(next);
                    } else {
                        // History navigation.
                        match hist_idx {
                            None => {}
                            Some(i) if i + 1 >= self.history.len() => {
                                hist_idx = None;
                                sess.text = hist_snap.clone();
                                sess.cursor = sess.text.len();
                                sess.comp_sel = None;
                            }
                            Some(i) => {
                                hist_idx = Some(i + 1);
                                sess.text = self.history[i + 1].clone();
                                sess.cursor = sess.text.len();
                                sess.comp_sel = None;
                            }
                        }
                    }
                }
                // ── Arrow Up: navigate dropdown or history ────────────────
                KeyEvent {
                    code: KeyCode::Up, ..
                } => {
                    let matches = self.dropdown_matches(&sess.text);
                    if !matches.is_empty() && sess.comp_sel.is_some() {
                        sess.comp_sel = match sess.comp_sel {
                            Some(0) | None => None,
                            Some(i) => Some(i - 1),
                        };
                    } else {
                        // History navigation.
                        if self.history.is_empty() {
                            self.render(&mut sess, &mut out)?;
                            continue;
                        }
                        let new_idx = match hist_idx {
                            None => {
                                hist_snap = sess.text.clone();
                                self.history.len() - 1
                            }
                            Some(i) => i.saturating_sub(1),
                        };
                        hist_idx = Some(new_idx);
                        sess.text = self.history[new_idx].clone();
                        sess.cursor = sess.text.len();
                        sess.comp_sel = None;
                    }
                }
                // ── Movement ─────────────────────────────────────────────
                KeyEvent {
                    code: KeyCode::Left,
                    modifiers,
                    ..
                } => {
                    if modifiers.contains(KeyModifiers::CONTROL) {
                        sess.word_left();
                    } else {
                        sess.move_left();
                    }
                }
                KeyEvent {
                    code: KeyCode::Right,
                    modifiers,
                    ..
                } => {
                    if modifiers.contains(KeyModifiers::CONTROL) {
                        sess.word_right();
                    } else {
                        sess.move_right();
                    }
                }
                KeyEvent {
                    code: KeyCode::Home,
                    ..
                } => sess.move_home(),
                KeyEvent {
                    code: KeyCode::End, ..
                } => sess.move_end(),
                // ── Deletion ─────────────────────────────────────────────
                KeyEvent {
                    code: KeyCode::Backspace,
                    ..
                } => {
                    sess.backspace();
                    hist_idx = None;
                }
                KeyEvent {
                    code: KeyCode::Delete,
                    ..
                } => sess.delete(),
                // ── Tab: cycle completions ────────────────────────────────
                KeyEvent {
                    code: KeyCode::Tab, ..
                } => {
                    if tab_idx.is_none() {
                        tab_prefix = sess.text.clone();
                    }
                    let matches = self.dropdown_matches(&tab_prefix);
                    if !matches.is_empty() {
                        let idx = tab_idx.map(|i| (i + 1) % matches.len()).unwrap_or(0);
                        tab_idx = Some(idx);
                        sess.comp_sel = Some(idx);
                        // Don't fill text yet — show highlighted in dropdown.
                    }
                }
                // ── Regular char ─────────────────────────────────────────
                KeyEvent {
                    code: KeyCode::Char(ch),
                    modifiers,
                    ..
                } if !modifiers.contains(KeyModifiers::CONTROL)
                    && !modifiers.contains(KeyModifiers::ALT) =>
                {
                    sess.insert(ch);
                    hist_idx = None;
                }
                _ => {}
            }

            self.render(&mut sess, &mut out)?;
        }
    }

    // ─── Dropdown helpers ─────────────────────────────────────────────────────

    fn dropdown_matches<'a>(&'a self, text: &str) -> Vec<&'a str> {
        if !text.starts_with('/') || text.contains(' ') {
            return vec![];
        }
        let matches: Vec<&str> = self
            .completions
            .iter()
            .filter(|c| c.starts_with(text))
            .map(String::as_str)
            .collect();
        // Hide dropdown if text is already an exact match (nothing more to complete).
        if matches.len() == 1 && matches[0] == text {
            return vec![];
        }
        matches
    }

    // ─── Rendering ────────────────────────────────────────────────────────────

    fn render(&self, sess: &mut Session, out: &mut impl Write) -> io::Result<()> {
        // Move cursor to top of previously rendered area.
        let rows_to_top = if sess.rendered_rows > 1 {
            sess.rendered_rows - 1 - sess.cursor_row_offset
        } else {
            0
        };
        if rows_to_top > 0 {
            queue!(out, MoveUp(rows_to_top as u16))?;
        }
        queue!(out, MoveToColumn(0), Clear(ClearType::FromCursorDown))?;

        let matches = self.dropdown_matches(&sess.text);
        // Show up to (terminal_height - 6) items so the dropdown never fills the screen.
        let max_visible = terminal::size()
            .map(|(_, h)| (h as usize).saturating_sub(6).clamp(6, 20))
            .unwrap_or(14);
        let visible_count = matches.len().min(max_visible);
        let dropdown_count = if matches.is_empty() {
            0
        } else {
            visible_count + 2 + if matches.len() > visible_count { 1 } else { 0 }
            // +2 borders, +1 "more" line when truncated
        };

        // ── Prompt ────────────────────────────────────────────────────────────
        // Accent teal, matching pi-mono's primary color
        queue!(
            out,
            SetForegroundColor(Color::Rgb { r: 138, g: 190, b: 183 }),
            SetAttribute(Attribute::Bold),
            Print(&self.prompt),
            ResetColor,
            SetAttribute(Attribute::Reset),
        )?;

        // ── Text (may be multi-line) ──────────────────────────────────────────
        let text = &sess.text;
        let lines: Vec<&str> = text.split('\n').collect();
        let n_lines = lines.len();

        let hints_row = text.is_empty();
        if hints_row {
            // Placeholder text.
            queue!(
                out,
                SetForegroundColor(Color::Rgb { r: 55, g: 60, b: 72 }),
                SetAttribute(Attribute::Italic),
                Print("Message…"),
                ResetColor,
                SetAttribute(Attribute::Reset),
            )?;
            // Keybinding hints on a new line below (shown only when input is empty).
            // Pi-mono style: dim key + muted description pairs.
            const DIM:   &str = "\x1b[38;2;102;102;102m";
            const MUTED: &str = "\x1b[38;2;128;128;128m";
            const RST:   &str = "\x1b[0m";
            let hints = format!(
                "  {DIM}^C{RST} {MUTED}exit{RST}  {DIM}^G{RST} {MUTED}editor{RST}  \
                 {DIM}^P/N{RST} {MUTED}model{RST}  {DIM}Shift+↵{RST} {MUTED}newline{RST}  \
                 {DIM}/{RST} {MUTED}commands{RST}"
            );
            queue!(
                out,
                Print("\r\n"),
                MoveToColumn(0),
                Print(hints),
                Clear(ClearType::UntilNewLine),
            )?;
        } else {
            for (i, line) in lines.iter().enumerate() {
                if i > 0 {
                    // Continuation line indicator.
                    queue!(
                        out,
                        SetForegroundColor(Color::Rgb { r: 60, g: 65, b: 78 }),
                        Print("  ↳ "),
                        ResetColor,
                    )?;
                }
                queue!(
                    out,
                    SetForegroundColor(Color::Rgb { r: 220, g: 225, b: 235 }),
                    Print(line),
                    ResetColor,
                )?;
                if i + 1 < n_lines {
                    queue!(out, Print("\r\n"))?;
                }
            }

            // Multiline badge: show line count on same row after the last line.
            if n_lines > 1 {
                queue!(
                    out,
                    SetForegroundColor(Color::Rgb { r: 60, g: 65, b: 78 }),
                    Print(format!("  \x1b[2m[{n_lines} lines]\x1b[0m")),
                    ResetColor,
                )?;
            }
        }

        // ── Dropdown (boxed slash-command menu) ───────────────────────────────
        if !matches.is_empty() {
            // Compute box width from all visible items.
            let max_item_len = matches.iter().take(visible_count).map(|s| s.len()).max().unwrap_or(4);
            let box_inner = max_item_len + 4; // "  item  "
            let border_col = Color::Rgb { r: 75, g: 85, b: 110 };
            let item_col   = Color::Rgb { r: 190, g: 195, b: 210 };
            let sel_col    = Color::Rgb { r: 80, g: 200, b: 120 };

            // Top border.
            queue!(out, Print("\r\n"), MoveToColumn(0))?;
            queue!(
                out,
                SetForegroundColor(border_col),
                Print(format!("  ╭{}╮", "─".repeat(box_inner))),
                ResetColor,
                Clear(ClearType::UntilNewLine),
            )?;

            for (i, item) in matches.iter().take(visible_count).enumerate() {
                let is_sel = sess.comp_sel == Some(i);
                let padding = box_inner.saturating_sub(item.len() + 2); // 2 for "❯ " / "  "
                queue!(out, Print("\r\n"), MoveToColumn(0))?;
                if is_sel {
                    queue!(
                        out,
                        SetForegroundColor(border_col),
                        Print("  │ "),
                        SetForegroundColor(sel_col),
                        SetAttribute(Attribute::Bold),
                        Print(format!("❯ {}{}", item, " ".repeat(padding))),
                        ResetColor,
                        SetAttribute(Attribute::Reset),
                        SetForegroundColor(border_col),
                        Print(" │"),
                        ResetColor,
                        Clear(ClearType::UntilNewLine),
                    )?;
                } else {
                    queue!(
                        out,
                        SetForegroundColor(border_col),
                        Print("  │ "),
                        SetForegroundColor(item_col),
                        Print(format!("  {}{}", item, " ".repeat(padding))),
                        ResetColor,
                        SetForegroundColor(border_col),
                        Print(" │"),
                        ResetColor,
                        Clear(ClearType::UntilNewLine),
                    )?;
                }
            }

            // "↓ N more" line when there are hidden items.
            let hidden = matches.len().saturating_sub(visible_count);
            if hidden > 0 {
                queue!(out, Print("\r\n"), MoveToColumn(0))?;
                queue!(
                    out,
                    SetForegroundColor(border_col),
                    Print("  │ "),
                    SetForegroundColor(Color::Rgb { r: 120, g: 128, b: 150 }),
                    Print(format!("  ↓ {} more{}", hidden, " ".repeat(box_inner.saturating_sub(10)))),
                    SetForegroundColor(border_col),
                    Print(" │"),
                    ResetColor,
                    Clear(ClearType::UntilNewLine),
                )?;
            }

            // Bottom border.
            queue!(out, Print("\r\n"), MoveToColumn(0))?;
            queue!(
                out,
                SetForegroundColor(border_col),
                Print(format!("  ╰{}╯", "─".repeat(box_inner))),
                ResetColor,
                Clear(ClearType::UntilNewLine),
            )?;
        }

        // ── Position cursor ───────────────────────────────────────────────────
        let prefix = &text[..sess.cursor];
        let cursor_line_idx = prefix.bytes().filter(|&b| b == b'\n').count();
        let cursor_col_text = match prefix.rfind('\n') {
            Some(p) => prefix[p + 1..].chars().count(),
            None => prefix.chars().count(),
        };
        let prompt_len = visible_len(&self.prompt);
        let term_width = terminal::size()
            .map(|(w, _)| usize::from(w.max(1)))
            .unwrap_or(80);

        // Continuation marker "  ↳ " adds 4 visible chars to non-first lines.
        let continuation_prefix_len = 4usize; // "  ↳ "

        let mut visual_total_rows = 0usize;
        let mut cursor_visual_row = 0usize;
        for (i, line) in lines.iter().enumerate() {
            let prefix_cols = if i == 0 {
                prompt_len
            } else {
                continuation_prefix_len
            };
            let cols = prefix_cols + line.chars().count();
            let rows = cols / term_width + 1;
            if i < cursor_line_idx {
                visual_total_rows += rows;
            } else if i == cursor_line_idx {
                let cursor_abs_cols = prefix_cols + cursor_col_text;
                cursor_visual_row = visual_total_rows + (cursor_abs_cols / term_width);
                visual_total_rows += rows;
            } else {
                visual_total_rows += rows;
            }
        }

        // For placeholder text, cursor stays at prompt end.
        let cursor_abs_cols = if text.is_empty() {
            prompt_len
        } else if cursor_line_idx == 0 {
            prompt_len + cursor_col_text
        } else {
            continuation_prefix_len + cursor_col_text
        };
        let cursor_col = cursor_abs_cols % term_width;

        // The hints line adds one extra row below the text when input is empty.
        let hints_extra = if hints_row { 1 } else { 0 };

        let rows_below_cursor =
            visual_total_rows.saturating_sub(cursor_visual_row + 1) + dropdown_count + hints_extra;
        if rows_below_cursor > 0 {
            queue!(out, MoveUp(rows_below_cursor as u16))?;
        }
        queue!(out, MoveToColumn(cursor_col as u16))?;

        // Track state for next render.
        sess.rendered_rows = dropdown_count + visual_total_rows + hints_extra;
        sess.cursor_row_offset = rows_below_cursor;

        out.flush()
    }

    fn erase(&self, sess: &mut Session, out: &mut impl Write) -> io::Result<()> {
        let rows_to_top = if sess.rendered_rows > 1 {
            sess.rendered_rows - 1 - sess.cursor_row_offset
        } else {
            0
        };
        if rows_to_top > 0 {
            queue!(out, MoveUp(rows_to_top as u16))?;
        }
        queue!(out, MoveToColumn(0), Clear(ClearType::FromCursorDown))?;
        sess.rendered_rows = 0;
        sess.cursor_row_offset = 0;
        out.flush()
    }

    fn finalize(&self, sess: &mut Session, out: &mut impl Write) -> io::Result<()> {
        self.erase(sess, out)?;
        // Print user message in pi-mono style: dark background, full-width highlight.
        let text = sess.text.replace('\n', " ↵ ");
        let term_width = crossterm::terminal::size().map(|(w, _)| w as usize).unwrap_or(80);
        // Pad to terminal width for full-width background effect.
        let visible_len = text.chars().count() + 1; // +1 for leading space
        let padding = if visible_len < term_width { " ".repeat(term_width - visible_len) } else { String::new() };
        execute!(
            out,
            // userMessageBg #343541 = rgb(52,53,65), default fg (no explicit color = inherit terminal default)
            Print("\x1b[48;2;52;53;65m"),
            Print(format!(" {text}{padding}")),
            Print("\x1b[0m\r\n"),
        )?;
        out.flush()
    }
}

/// Open the current input text in $VISUAL or $EDITOR, read back the result.
/// Temporarily exits raw mode so the editor has full terminal control.
fn open_in_editor(current: &str, out: &mut impl Write) -> io::Result<String> {
    let editor = std::env::var("VISUAL")
        .or_else(|_| std::env::var("EDITOR"))
        .unwrap_or_else(|_| "vi".to_string());

    let tmp_path = std::env::temp_dir()
        .join(format!("d-code-{}.md", std::process::id()));
    std::fs::write(&tmp_path, current)?;

    // Release terminal for the editor.
    terminal::disable_raw_mode()?;
    execute!(out, Print("\r\n"))?;

    let parts: Vec<&str> = editor.split_whitespace().collect();
    let status = std::process::Command::new(parts[0])
        .args(&parts[1..])
        .arg(&tmp_path)
        .status();

    // Re-acquire terminal.
    terminal::enable_raw_mode()?;
    // Clear screen so editor output doesn't bleed through.
    execute!(out, Clear(ClearType::All), crossterm::cursor::MoveTo(0, 0))?;

    let _ = status?;

    let text = std::fs::read_to_string(&tmp_path)
        .unwrap_or_else(|_| current.to_string());
    let _ = std::fs::remove_file(&tmp_path);

    // Strip trailing newline editors often append.
    Ok(text.trim_end_matches('\n').to_string())
}

fn visible_len(s: &str) -> usize {
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
