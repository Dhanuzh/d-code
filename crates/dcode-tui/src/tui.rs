//! The [`Tui`] engine — differential terminal renderer.
//!
//! Mirrors pi-mono packages/tui/src/tui.ts core logic:
//! - Component list → lines
//! - Diff against previous frame
//! - Write only changed lines
//! - Synchronized output to prevent flicker
//! - 16ms render throttle (60fps cap)

use std::io::{stdout, BufWriter, Write};
use std::time::{Duration, Instant};

use crossterm::terminal;

use crate::{Component, Line};

/// The render engine.
///
/// Usage:
/// ```no_run
/// let mut tui = Tui::new();
/// tui.push(Box::new(MyComponent::new()));
/// // on each event:
/// tui.request_render();
/// // when done with current content:
/// tui.commit(); // flushes to static output, clears diff state
/// ```
pub struct Tui {
    components: Vec<Box<dyn Component>>,
    /// Lines from the previous render frame (for diffing).
    prev_lines: Vec<String>,
    /// When the last render ran (for throttling).
    last_render: Instant,
    /// Pending render request (set by request_render(), cleared on actual render).
    render_pending: bool,
    /// Latest lines staged during a throttled call — rendered on next opportunity.
    /// Prevents the "bulk paste" effect where skipped frames all appear at once.
    pending_lines: Option<Vec<String>>,
    /// Cached terminal width.
    width: u16,
}

impl Tui {
    pub fn new() -> Self {
        let width = terminal::size().map(|(w, _)| w).unwrap_or(80);
        Self {
            components: Vec::new(),
            prev_lines: Vec::new(),
            last_render: Instant::now() - Duration::from_secs(1),
            render_pending: false,
            pending_lines: None,
            width,
        }
    }

    /// Add a component to the bottom of the stack.
    pub fn push(&mut self, component: Box<dyn Component>) {
        self.components.push(component);
        self.render_pending = true;
    }

    /// Remove the last component.
    pub fn pop(&mut self) -> Option<Box<dyn Component>> {
        let c = self.components.pop();
        self.render_pending = true;
        c
    }

    /// Replace the last component.
    pub fn replace_last(&mut self, component: Box<dyn Component>) {
        if let Some(last) = self.components.last_mut() {
            *last = component;
            self.render_pending = true;
        }
    }

    /// Get a mutable reference to the last component.
    pub fn last_mut(&mut self) -> Option<&mut Box<dyn Component>> {
        self.components.last_mut()
    }

    /// Get a mutable reference to a component by index.
    pub fn get_mut(&mut self, idx: usize) -> Option<&mut Box<dyn Component>> {
        self.components.get_mut(idx)
    }

    /// Number of components.
    pub fn len(&self) -> usize {
        self.components.len()
    }

    /// Mark a render as needed. Actual render happens on next `flush()` call,
    /// subject to the 16ms throttle.
    pub fn request_render(&mut self) {
        self.render_pending = true;
    }

    /// Render now, regardless of throttle. Use for final renders (turn end).
    pub fn render_now(&mut self) {
        self.render_pending = false;
        self.do_render();
    }

    /// Render a pre-built list of lines differentially.
    /// Use this when you manage your own component state (no Component trait needed).
    pub fn render_lines(&mut self, lines: Vec<String>) {
        if lines == self.prev_lines {
            self.last_render = Instant::now();
            return;
        }
        self.write_diff(lines);
    }

    /// Render a pre-built list of lines, throttled to ~120fps (8ms).
    ///
    /// When within the throttle window, the lines are stored as `pending_lines`
    /// so the next eligible call renders the latest state instead of the stale one.
    /// This prevents the "bulk paste" effect where skipped frames all appear at once.
    pub fn render_lines_throttled(&mut self, lines: Vec<String>) {
        if self.last_render.elapsed() >= Duration::from_millis(8) {
            // Drain any pending lines first; then render current.
            self.pending_lines = None;
            self.write_diff(lines);
        } else {
            // Too soon — store latest so the next eligible call renders it.
            self.pending_lines = Some(lines);
        }
    }

    /// Flush any pending lines staged during throttled calls.
    /// Call this after receiving each event to catch frames that were staged but not yet rendered.
    pub fn flush_pending(&mut self) {
        if self.last_render.elapsed() < Duration::from_millis(8) {
            return;
        }
        if let Some(lines) = self.pending_lines.take() {
            self.write_diff(lines);
        }
    }

    /// Render if pending and throttle interval has elapsed (16ms = 60fps).
    pub fn flush(&mut self) {
        if !self.render_pending {
            return;
        }
        let elapsed = self.last_render.elapsed();
        if elapsed < Duration::from_millis(8) {
            return; // Too soon — will render on next flush() call.
        }
        self.render_pending = false;
        self.do_render();
    }

    /// Commit: finalize current content as static (no longer tracked for diff).
    /// Call at end of each turn. The content stays on screen; future renders
    /// start fresh from the current cursor position (appending below).
    ///
    /// NOTE: if using render_lines() directly (external rendering), do NOT call
    /// render_now() here — content is already on screen and render_now() would
    /// erase it by diffing against empty component list.
    pub fn commit(&mut self) {
        // Only re-render from components if components are actually registered.
        if !self.components.is_empty() {
            self.render_now();
        }
        // Reset diff state — content is now static, cursor is at bottom.
        self.prev_lines.clear();
        self.components.clear();
        self.render_pending = false;
    }

    /// Clear all components and prev state (hard reset).
    pub fn clear(&mut self) {
        self.components.clear();
        self.prev_lines.clear();
        self.render_pending = false;
    }

    // ── Internal ─────────────────────────────────────────────────────────────

    /// Collect all lines from all components.
    fn collect_lines(&mut self) -> Vec<String> {
        let width = self.width;
        let mut result = Vec::new();
        for component in &mut self.components {
            let mut lines = component.render(width);
            for line in &mut lines {
                result.push(line.render().to_string());
            }
            component.mark_clean();
        }
        result
    }

    /// Core differential render.
    fn do_render(&mut self) {
        if let Ok((w, _)) = terminal::size() {
            self.width = w;
        }
        let new_lines = self.collect_lines();
        if new_lines == self.prev_lines {
            self.last_render = Instant::now();
            return;
        }
        self.write_diff(new_lines);
    }

    /// Write a diff of new_lines vs prev_lines to the terminal.
    /// Algorithm mirrors pi-mono tui.ts doRender:
    /// 1. Find first changed line.
    /// 2. Move cursor up to that line.
    /// 3. Write only the changed range.
    /// 4. Erase extra lines if new is shorter.
    /// 5. Wrap in synchronized output (no flicker).
    fn write_diff(&mut self, new_lines: Vec<String>) {
        let mut out = BufWriter::new(stdout());

        // Synchronized output — prevents mid-render flicker.
        let _ = out.write_all(b"\x1b[?2026h");

        let prev_len = self.prev_lines.len();
        let new_len = new_lines.len();
        let min_len = prev_len.min(new_len);

        let first_changed = (0..min_len)
            .find(|&i| self.prev_lines[i] != new_lines[i])
            .unwrap_or(min_len);

        // Move cursor up to first changed line.
        let lines_above = prev_len.saturating_sub(first_changed);
        if lines_above > 0 {
            let _ = write!(out, "\x1b[{}A", lines_above);
        }

        // Write changed + new lines.
        for line in &new_lines[first_changed..] {
            let _ = write!(out, "\r\x1b[2K{}\n", line);
        }

        // Erase extra lines from previous render.
        if new_len < prev_len {
            let extra = prev_len - new_len;
            for _ in 0..extra {
                let _ = write!(out, "\r\x1b[2K\n");
            }
            let _ = write!(out, "\x1b[{}A", extra);
        }

        let _ = out.write_all(b"\x1b[?2026l");
        let _ = out.flush();

        self.prev_lines = new_lines;
        self.last_render = Instant::now();
    }
}

impl Default for Tui {
    fn default() -> Self {
        Self::new()
    }
}
