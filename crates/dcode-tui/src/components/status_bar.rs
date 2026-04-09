//! StatusBar component — footer line showing token usage, cost, model.
//!
//! Mirrors pi-mono's footer.ts component.

use crate::{Component, Line};

const C_MUTED:   &str = "\x1b[38;2;128;128;128m";
const C_DIM:     &str = "\x1b[38;2;102;102;102m";
const C_SUCCESS: &str = "\x1b[38;2;181;189;104m";
const C_WARNING: &str = "\x1b[38;2;220;175;50m";
const C_ERROR:   &str = "\x1b[38;2;204;102;102m";
const C_ACCENT:  &str = "\x1b[38;2;138;190;183m";
const RESET:     &str = "\x1b[0m";

pub struct StatusBar {
    pub total_input: u32,
    pub total_output: u32,
    pub model: String,
    pub context_used: u32,
    pub context_window: u32,
    pub cost_usd: f64,
    dirty: bool,
}

impl StatusBar {
    pub fn new(model: impl Into<String>, context_window: u32) -> Self {
        Self {
            total_input: 0,
            total_output: 0,
            model: model.into(),
            context_used: 0,
            context_window,
            cost_usd: 0.0,
            dirty: true,
        }
    }

    pub fn update(&mut self, input: u32, output: u32, context_used: u32, cost_usd: f64) {
        self.total_input = input;
        self.total_output = output;
        self.context_used = context_used;
        self.cost_usd = cost_usd;
        self.dirty = true;
    }
}

impl Component for StatusBar {
    fn render(&mut self, width: u16) -> Vec<Line> {
        self.dirty = false;

        let ctx_pct = if self.context_window > 0 {
            (self.context_used as f64 / self.context_window as f64) * 100.0
        } else {
            0.0
        };

        let ctx_color = if ctx_pct >= 80.0 { C_ERROR }
            else if ctx_pct >= 60.0 { C_WARNING }
            else { C_SUCCESS };

        // Left side: token counts + cost
        let left = {
            let mut parts = Vec::new();
            if self.total_input > 0 {
                parts.push(format!("{C_MUTED}↑{}{RESET}", fmt_tokens(self.total_input)));
            }
            if self.total_output > 0 {
                parts.push(format!("{C_MUTED}↓{}{RESET}", fmt_tokens(self.total_output)));
            }
            if self.cost_usd > 0.0 {
                parts.push(format!("{C_DIM}${:.4}{RESET}", self.cost_usd));
            }
            if self.context_window > 0 {
                parts.push(format!(
                    "{ctx_color}{:.1}%/{}{RESET}",
                    ctx_pct,
                    fmt_tokens(self.context_window)
                ));
            }
            parts.join("  ")
        };

        // Right side: model name
        let right = format!("{C_DIM}{}{RESET}", self.model);

        // Measure visible widths.
        let left_vis = strip_ansi_len(&left);
        let right_vis = strip_ansi_len(&right);
        let w = width as usize;
        let gap = w.saturating_sub(left_vis + right_vis + 4); // 4 for "  " padding each side
        let padding: String = " ".repeat(gap);

        vec![Line::raw(format!("  {left}{padding}{right}"))]
    }

    fn is_dirty(&self) -> bool { self.dirty }
    fn mark_clean(&mut self) { self.dirty = false; }
    fn height_hint(&self) -> Option<u16> { Some(1) }
}

fn fmt_tokens(n: u32) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.0}k", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

fn strip_ansi_len(s: &str) -> usize {
    crate::line::strip_ansi(s).len()
}
