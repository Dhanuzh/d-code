//! Spinner component — BorderedLoader style matching pi-mono.
//!
//! Layout (mirrors bordered-loader.ts):
//!   ────────────────────────   ← blue DynamicBorder
//!   ⠋ thinking  1.2s          ← animated spinner + label + elapsed
//!   ────────────────────────   ← blue DynamicBorder

use std::time::Instant;
use crate::{Component, Line};

// Pi-mono dark theme: border = blue #5f87ff, accent = #8abeb7, muted = #808080, dim = #666666
const C_BORDER: &str = "\x1b[38;2;95;135;255m";    // blue  (border)
const C_ACCENT: &str = "\x1b[38;2;138;190;183m";   // teal  (spinner frame)
const C_MUTED:  &str = "\x1b[38;2;128;128;128m";   // gray  (label)
const C_DIM:    &str = "\x1b[38;2;102;102;102m";   // dimGray (elapsed)
const RESET:    &str = "\x1b[0m";

const FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// Animated spinner shown while the agent is thinking.
/// Rendered as BorderedLoader: border / spinner+label / border.
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
        Self { started_at: Instant::now(), label: label.into() }
    }

    pub fn elapsed_ms(&self) -> u64 {
        self.started_at.elapsed().as_millis() as u64
    }
}

impl Default for Spinner {
    fn default() -> Self { Self::new() }
}

impl Component for Spinner {
    fn render(&mut self, width: u16) -> Vec<Line> {
        let ms = self.started_at.elapsed().as_millis() as usize;
        let frame_idx = ms / 80;
        let frame = FRAMES[frame_idx % FRAMES.len()];
        let w = width as usize;

        // DynamicBorder: full-width ─ line in blue.
        let border: String = std::iter::repeat('─').take(w.saturating_sub(0)).collect();
        let border_line = format!("{C_BORDER}{border}{RESET}");

        // Elapsed display.
        let secs = self.started_at.elapsed().as_secs_f32();
        let elapsed = if secs < 10.0 {
            format!("{secs:.1}s")
        } else {
            format!("{secs:.0}s")
        };

        // Spinner content line: "  ⠋ thinking  1.2s"
        let spinner_line = format!(
            "  {C_ACCENT}{frame}{RESET} {C_MUTED}{}{RESET}  {C_DIM}{elapsed}{RESET}",
            self.label
        );

        vec![
            Line::raw(border_line.clone()),
            Line::raw(spinner_line),
            Line::raw(border_line),
        ]
    }

    // Spinner always dirty — animates every frame.
    fn is_dirty(&self) -> bool { true }
}
