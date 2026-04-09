//! The [`Component`] trait — the building block of the TUI.
//!
//! Modeled after pi-mono's component system in interactive-mode.ts.
//! Every renderable element (assistant message, tool execution, input bar,
//! status bar, spinner) implements this trait.

use crate::Line;

/// A renderable TUI element.
///
/// Components are owned by [`crate::Tui`] and rendered each frame.
/// The TUI engine calls `render(width)` and compares the result against
/// the previous frame to produce a minimal terminal diff.
pub trait Component: Send {
    /// Render to a list of terminal lines at the given column width.
    ///
    /// Called every render cycle. Should be cheap — use internal caching
    /// (`dirty` flag + cached lines) to avoid re-computing when nothing changed.
    fn render(&mut self, width: u16) -> Vec<Line>;

    /// Whether this component's content has changed since the last render.
    /// The TUI engine uses this as a hint — if no component is dirty, the
    /// render is skipped entirely.
    fn is_dirty(&self) -> bool {
        true // Conservative default: always re-render.
    }

    /// Called by the TUI engine after render() to mark the component clean.
    fn mark_clean(&mut self) {}

    /// Approximate height in lines. Used for layout planning.
    /// Returns None if unknown (component determines height dynamically).
    fn height_hint(&self) -> Option<u16> {
        None
    }
}

/// A static block of already-rendered lines that never changes.
/// Used to represent finalized (committed) content from previous turns.
pub struct StaticLines {
    lines: Vec<Line>,
}

impl StaticLines {
    pub fn new(lines: Vec<Line>) -> Self {
        Self { lines }
    }
}

impl Component for StaticLines {
    fn render(&mut self, _width: u16) -> Vec<Line> {
        self.lines.clone()
    }
    fn is_dirty(&self) -> bool { false }
}

/// A blank vertical spacer of N lines.
pub struct Spacer {
    pub lines: u16,
}

impl Component for Spacer {
    fn render(&mut self, _width: u16) -> Vec<Line> {
        vec![Line::plain(""); self.lines as usize]
    }
    fn is_dirty(&self) -> bool { false }
    fn height_hint(&self) -> Option<u16> { Some(self.lines) }
}

/// A horizontal rule (full-width line of a repeated character).
pub struct HRule {
    pub ch: char,
    pub style: Option<String>,
    dirty: bool,
    last_width: u16,
}

impl HRule {
    pub fn new(ch: char, style: impl Into<String>) -> Self {
        Self { ch, style: Some(style.into()), dirty: true, last_width: 0 }
    }
    pub fn plain(ch: char) -> Self {
        Self { ch, style: None, dirty: true, last_width: 0 }
    }
}

impl Component for HRule {
    fn render(&mut self, width: u16) -> Vec<Line> {
        self.last_width = width;
        self.dirty = false;
        let text: String = std::iter::repeat(self.ch).take(width as usize).collect();
        let line = if let Some(s) = &self.style {
            Line::styled(text, s.clone())
        } else {
            Line::plain(text)
        };
        vec![line]
    }
    fn is_dirty(&self) -> bool { self.dirty }
    fn mark_clean(&mut self) { self.dirty = false; }
    fn height_hint(&self) -> Option<u16> { Some(1) }
}
