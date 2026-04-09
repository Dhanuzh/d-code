//! Spinner component — animated "thinking" indicator.
//!
//! Unlike the old Spinner (a separate tokio task racing with the renderer),
//! this is a proper Component that the TUI engine renders on each frame.
//! No thread races, no stdout contention.

use std::time::Instant;
use crate::{Component, Line};

const C_DIM: &str = "\x1b[38;2;102;102;102m";
const C_MUTED: &str = "\x1b[38;2;128;128;128m";
const RESET: &str = "\x1b[0m";

// Teal shimmer palette (same as old repl.rs spinner).
const SHIMMER: &[(u8, u8, u8)] = &[
    (60,  110, 105),
    (75,  135, 130),
    (95,  155, 150),
    (115, 170, 165),
    (130, 182, 178),
    (138, 190, 183), // peak #8abeb7
    (128, 180, 173),
    (108, 165, 158),
    (88,  148, 142),
    (70,  128, 122),
];

const FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// Animated spinner shown while the agent is thinking.
pub struct Spinner {
    started_at: Instant,
    /// Label shown next to spinner (default: "thinking").
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
    fn render(&mut self, _width: u16) -> Vec<Line> {
        let ms = self.started_at.elapsed().as_millis() as usize;
        let frame_idx = ms / 80; // advance frame every 80ms
        let frame = FRAMES[frame_idx % FRAMES.len()];
        let (sr, sg, sb) = SHIMMER[frame_idx % SHIMMER.len()];

        let secs = self.started_at.elapsed().as_secs_f32();
        let elapsed = if secs < 10.0 {
            format!("{secs:.1}s")
        } else {
            format!("{secs:.0}s")
        };

        let line = format!(
            "  \x1b[38;2;{sr};{sg};{sb}m{frame}{RESET} {C_MUTED}{}{RESET}  {C_DIM}{elapsed}{RESET}",
            self.label
        );

        vec![Line::raw(line)]
    }

    // Spinner is always dirty — it animates every frame.
    fn is_dirty(&self) -> bool { true }
}
