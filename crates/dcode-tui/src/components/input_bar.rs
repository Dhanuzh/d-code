//! InputBar component — shows the current input prompt line.
//! Read-only view (actual input is handled by LineEditor in dcode-cli).

use crate::{Component, Line};

const C_ACCENT: &str = "\x1b[38;2;138;190;183m";
const C_DIM:    &str = "\x1b[38;2;102;102;102m";
const RESET:    &str = "\x1b[0m";

pub struct InputBar {
    pub prompt: String,
    dirty: bool,
}

impl InputBar {
    pub fn new(prompt: impl Into<String>) -> Self {
        Self { prompt: prompt.into(), dirty: true }
    }

    pub fn set_prompt(&mut self, prompt: impl Into<String>) {
        self.prompt = prompt.into();
        self.dirty = true;
    }
}

impl Component for InputBar {
    fn render(&mut self, _width: u16) -> Vec<Line> {
        self.dirty = false;
        vec![Line::raw(format!("  {C_ACCENT}▸{RESET} {C_DIM}{}{RESET}", self.prompt))]
    }
    fn is_dirty(&self) -> bool { self.dirty }
    fn mark_clean(&mut self) { self.dirty = false; }
    fn height_hint(&self) -> Option<u16> { Some(1) }
}
