//! Spinner component — minimal animated indicator.
//!
//! Layout:
//!   ⠋ thinking  1.2s          ← animated braille frame + label + elapsed

use crate::{Component, Line};
use std::time::Instant;

const C_ACCENT: &str = "\x1b[38;2;138;190;183m"; // teal  (spinner frame)
const C_MUTED: &str = "\x1b[38;2;128;128;128m"; // gray  (label)
const C_DIM: &str = "\x1b[38;2;102;102;102m"; // dimGray (elapsed)
const RESET: &str = "\x1b[0m";

const FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// Animated spinner shown while the agent is thinking or waiting.
pub struct Spinner {
    started_at: Instant,
    pub label: String,
}

impl Spinner {
    pub fn new() -> Self {
        Self {
            started_at: Instant::now(),
            label: "thinking".into(),
        }
    }

    pub fn with_label(label: impl Into<String>) -> Self {
        Self {
            started_at: Instant::now(),
            label: label.into(),
        }
    }

    pub fn elapsed_ms(&self) -> u64 {
        self.started_at.elapsed().as_millis() as u64
    }

    /// Returns (frame, elapsed_str) for rendering outside the TUI (e.g. bottom-right overlay).
    pub fn overlay_parts(&self) -> (&'static str, String) {
        let ms = self.started_at.elapsed().as_millis() as usize;
        let frame = FRAMES[(ms / 80) % FRAMES.len()];
        let secs = self.started_at.elapsed().as_secs_f32();
        let elapsed = if secs < 10.0 {
            format!("{secs:.1}s")
        } else {
            format!("{secs:.0}s")
        };
        (frame, elapsed)
    }
}

impl Default for Spinner {
    fn default() -> Self {
        Self::new()
    }
}

impl Component for Spinner {
    fn render(&mut self, _width: u16) -> Vec<Line> {
        let ms = self.started_at.elapsed().as_millis() as usize;
        let frame = FRAMES[(ms / 80) % FRAMES.len()];

        let secs = self.started_at.elapsed().as_secs_f32();
        let elapsed = if secs < 10.0 {
            format!("{secs:.1}s")
        } else {
            format!("{secs:.0}s")
        };

        let line = format!(
            "  {C_ACCENT}{frame}{RESET} {C_MUTED}{}{RESET}  {C_DIM}{elapsed}{RESET}",
            self.label
        );

        vec![Line::raw(line)]
    }

    // Spinner always dirty — animates every frame.
    fn is_dirty(&self) -> bool {
        true
    }
}
