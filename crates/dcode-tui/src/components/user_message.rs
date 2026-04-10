//! UserMessage component — mirrors pi-mono's user-message.ts.
//! Shows the user's input with a subtle background, left-padded.

use crate::{Component, Line};

const C_USER_BG: &str = "\x1b[48;2;40;44;56m"; // dark blue-grey background
const C_USER_TEXT: &str = "\x1b[38;2;200;210;240m"; // light blue-white text
const C_DIM: &str = "\x1b[38;2;102;102;102m";
const C_ACCENT: &str = "\x1b[38;2;138;190;183m";
const RESET: &str = "\x1b[0m";

pub struct UserMessage {
    text: String,
    dirty: bool,
}

impl UserMessage {
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            dirty: true,
        }
    }
}

impl Component for UserMessage {
    fn render(&mut self, width: u16) -> Vec<Line> {
        self.dirty = false;
        let mut lines = Vec::new();

        // Blank line before user message (spacer)
        lines.push(Line::plain(""));

        // Render each line of user input with background highlight
        let w = width as usize;
        for text_line in self.text.lines() {
            // Pad to full width for background effect
            let visible_len = text_line.chars().count() + 2; // +2 for " " prefix
            let padding = if visible_len < w {
                " ".repeat(w - visible_len)
            } else {
                String::new()
            };
            lines.push(Line::raw(format!(
                "{C_USER_BG}{C_USER_TEXT} {text_line}{padding} {RESET}"
            )));
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
