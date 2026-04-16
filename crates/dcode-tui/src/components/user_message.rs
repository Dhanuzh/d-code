//! UserMessage component — mirrors pi-mono's user-message.ts.
//! Shows the user's input in a padded background box (pi-mono style).

use crate::{Component, Line};

const C_USER_BG: &str = "\x1b[48;2;40;44;56m"; // dark blue-grey background
const C_USER_TEXT: &str = "\x1b[38;2;200;210;240m"; // light blue-white text
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
        let w = width as usize;

        // Blank line before user message (spacer)
        lines.push(Line::plain(""));

        // Box spans ~90% of terminal width, matching pi-mono style.
        let box_width = (w * 9 / 10).max(20).min(w);
        let blank_row = " ".repeat(box_width);

        // Top padding row
        lines.push(Line::raw(format!("{C_USER_BG}{blank_row}{RESET}")));

        // Text rows with left padding
        for text_line in self.text.lines() {
            let text_len = text_line.chars().count();
            let right_fill = box_width.saturating_sub(text_len + 3); // 3 left pad
            let fill = " ".repeat(right_fill);
            lines.push(Line::raw(format!(
                "{C_USER_BG}{C_USER_TEXT}   {text_line}{fill}{RESET}"
            )));
        }

        // Bottom padding row
        lines.push(Line::raw(format!("{C_USER_BG}{blank_row}{RESET}")));

        // Blank line after user message (spacer before response)
        lines.push(Line::plain(""));

        lines
    }

    fn is_dirty(&self) -> bool {
        self.dirty
    }
    fn mark_clean(&mut self) {
        self.dirty = false;
    }
}
