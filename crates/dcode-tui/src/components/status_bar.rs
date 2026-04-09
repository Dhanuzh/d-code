//! StatusBar component — 2-line footer matching pi-mono's footer.ts.
//!
//! Line 1: `~path (branch)` dim
//! Line 2: `↑in ↓out $cost ctx%/window` left  +  `model` right — both dim
//!         context % colorized: >90% red, >70% yellow, else default

use crate::{Component, Line};

// Pi-mono dark theme colors
const C_DIM:     &str = "\x1b[38;2;102;102;102m";   // dimGray #666666
const C_WARNING: &str = "\x1b[38;2;255;255;0m";     // yellow  #ffff00
const C_ERROR:   &str = "\x1b[38;2;204;102;102m";   // red     #cc6666
const RESET:     &str = "\x1b[0m";

pub struct StatusBar {
    pub total_input: u32,
    pub total_output: u32,
    pub model: String,
    pub context_used: u32,
    pub context_window: u32,
    pub cost_usd: f64,
    /// Working directory (shown with ~ for home), e.g. "~/projects/foo"
    pub cwd: String,
    /// Git branch, e.g. "main"
    pub branch: String,
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
            cwd: String::new(),
            branch: String::new(),
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

    pub fn set_cwd(&mut self, cwd: impl Into<String>) {
        self.cwd = cwd.into();
        self.dirty = true;
    }

    pub fn set_branch(&mut self, branch: impl Into<String>) {
        self.branch = branch.into();
        self.dirty = true;
    }
}

impl Component for StatusBar {
    fn render(&mut self, width: u16) -> Vec<Line> {
        self.dirty = false;
        let w = width as usize;

        // ── Line 1: cwd (branch) ──────────────────────────────────────────────
        let pwd_text = {
            let mut s = if self.cwd.is_empty() {
                String::new()
            } else {
                self.cwd.clone()
            };
            if !self.branch.is_empty() {
                s.push_str(&format!(" ({})", self.branch));
            }
            s
        };
        // Truncate to width.
        let pwd_truncated = truncate_str(&pwd_text, w.saturating_sub(1));
        let line1 = format!("{C_DIM}{pwd_truncated}{RESET}");

        // ── Line 2: stats left + model right ─────────────────────────────────
        let ctx_pct = if self.context_window > 0 {
            (self.context_used as f64 / self.context_window as f64) * 100.0
        } else {
            0.0
        };

        let ctx_color = if ctx_pct >= 90.0 { C_ERROR }
            else if ctx_pct >= 70.0 { C_WARNING }
            else { "" };  // default (no extra color, dimmed by wrapper)

        // Stats: ↑in ↓out $cost ctx%/window
        let mut stat_parts: Vec<String> = Vec::new();
        if self.total_input > 0 {
            stat_parts.push(format!("↑{}", fmt_tokens(self.total_input)));
        }
        if self.total_output > 0 {
            stat_parts.push(format!("↓{}", fmt_tokens(self.total_output)));
        }
        if self.cost_usd > 0.0 {
            stat_parts.push(format!("${:.3}", self.cost_usd));
        }
        if self.context_window > 0 {
            let ctx_str = format!("{:.1}%/{}", ctx_pct, fmt_tokens(self.context_window));
            if ctx_color.is_empty() {
                stat_parts.push(ctx_str);
            } else {
                // Color the ctx% part, then re-apply dim via the outer wrapper.
                // We break out of dim, apply the warning/error color, then go back to dim.
                stat_parts.push(format!("{RESET}{ctx_color}{ctx_str}{RESET}{C_DIM}"));
            }
        }
        let stats_left = stat_parts.join(" ");
        let stats_left_vis = strip_ansi_len(&stats_left);

        let model_right = &self.model;
        let model_vis = model_right.len();

        let gap = w.saturating_sub(stats_left_vis + model_vis + 2); // +2 min padding
        let padding = " ".repeat(gap.max(2));

        let line2 = format!("{C_DIM}{stats_left}{padding}{model_right}{RESET}");

        vec![Line::raw(line1), Line::raw(line2)]
    }

    fn is_dirty(&self) -> bool { self.dirty }
    fn mark_clean(&mut self) { self.dirty = false; }
    fn height_hint(&self) -> Option<u16> { Some(2) }
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

fn truncate_str(s: &str, max_chars: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= max_chars {
        s.to_string()
    } else {
        let truncated: String = chars[..max_chars.saturating_sub(1)].iter().collect();
        format!("{truncated}…")
    }
}
