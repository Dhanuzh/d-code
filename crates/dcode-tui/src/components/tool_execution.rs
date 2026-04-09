//! ToolExecution component — mirrors pi-mono's tool-execution.ts.
//!
//! Shows tool name, status (running/done/error), elapsed time,
//! and a preview of the output. Uses DynamicBorder-style separators.

use std::time::Instant;
use crate::{Component, Line};

const C_BORDER:  &str = "\x1b[38;2;95;135;255m";   // #5f87ff blue
const C_ACCENT:  &str = "\x1b[38;2;138;190;183m";  // #8abeb7 teal
const C_SUCCESS: &str = "\x1b[38;2;181;189;104m";  // #b5bd68 green
const C_ERROR:   &str = "\x1b[38;2;204;102;102m";  // #cc6666 red
const C_MUTED:   &str = "\x1b[38;2;128;128;128m";
const C_DIM:     &str = "\x1b[38;2;102;102;102m";
const C_TEXT:    &str = "\x1b[38;2;212;215;222m";
const RESET:     &str = "\x1b[0m";

#[derive(Debug, Clone, PartialEq)]
pub enum ToolStatus {
    Running,
    Done,
    Error,
}

/// A tool call in progress or completed.
pub struct ToolExecution {
    pub name: String,
    pub status: ToolStatus,
    /// Parsed tool input (shown as brief summary).
    pub input_summary: String,
    /// Output preview (first few lines of result).
    pub output_preview: String,
    /// Elapsed ms (set on completion).
    pub elapsed_ms: u64,
    started_at: Instant,
    dirty: bool,
    /// Cached render to avoid re-computing on every frame when not dirty.
    cached_lines: Vec<String>,
    last_width: u16,
}

impl ToolExecution {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            status: ToolStatus::Running,
            input_summary: String::new(),
            output_preview: String::new(),
            elapsed_ms: 0,
            started_at: Instant::now(),
            dirty: true,
            cached_lines: Vec::new(),
            last_width: 0,
        }
    }

    /// Mark as done with result.
    pub fn finish(&mut self, output: impl Into<String>, is_error: bool, input_summary: impl Into<String>) {
        self.status = if is_error { ToolStatus::Error } else { ToolStatus::Done };
        self.elapsed_ms = self.started_at.elapsed().as_millis() as u64;
        self.output_preview = truncate_preview(output.into());
        self.input_summary = input_summary.into();
        self.dirty = true;
    }

    /// Current elapsed ms (for live display while running).
    pub fn current_elapsed_ms(&self) -> u64 {
        self.started_at.elapsed().as_millis() as u64
    }

    fn build_lines(&self, width: u16) -> Vec<String> {
        let w = width as usize;
        let border_line: String = std::iter::repeat('─').take(w.saturating_sub(2)).collect();

        let mut lines = Vec::new();

        match self.status {
            ToolStatus::Running => {
                // Top border
                lines.push(format!("  {C_BORDER}{border_line}{RESET}"));
                // Tool name with running indicator
                let elapsed = format_elapsed(self.current_elapsed_ms());
                lines.push(format!("  {C_ACCENT}◦ {}{RESET}  {C_DIM}{elapsed}{RESET}", self.name));
            }
            ToolStatus::Done => {
                let elapsed = format_elapsed(self.elapsed_ms);
                // Top border (green for success)
                lines.push(format!("  {C_SUCCESS}{border_line}{RESET}"));
                // Status line
                let summary = if !self.input_summary.is_empty() {
                    format!("  {C_DIM}{}{RESET}", self.input_summary)
                } else {
                    String::new()
                };
                lines.push(format!(
                    "  {C_SUCCESS}✓ {}{RESET}{summary}  {C_DIM}{elapsed}{RESET}",
                    self.name
                ));
                // Output preview
                if !self.output_preview.is_empty() {
                    for l in self.output_preview.lines().take(8) {
                        lines.push(format!("    {C_MUTED}{}{RESET}", l));
                    }
                }
                // Bottom border
                lines.push(format!("  {C_SUCCESS}{border_line}{RESET}"));
            }
            ToolStatus::Error => {
                let elapsed = format_elapsed(self.elapsed_ms);
                lines.push(format!("  {C_ERROR}{border_line}{RESET}"));
                lines.push(format!(
                    "  {C_ERROR}✗ {}{RESET}  {C_DIM}{elapsed}{RESET}",
                    self.name
                ));
                if !self.output_preview.is_empty() {
                    for l in self.output_preview.lines().take(6) {
                        lines.push(format!("    {C_ERROR}{}{RESET}", l));
                    }
                }
                lines.push(format!("  {C_ERROR}{border_line}{RESET}"));
            }
        }

        lines
    }
}

impl Component for ToolExecution {
    fn render(&mut self, width: u16) -> Vec<Line> {
        if self.dirty || self.last_width != width {
            self.cached_lines = self.build_lines(width);
            self.last_width = width;
            self.dirty = false;
        }
        // Running tools are always dirty (elapsed time changes).
        if self.status == ToolStatus::Running {
            self.dirty = true; // re-render next frame
            self.cached_lines = self.build_lines(width);
        }
        self.cached_lines.iter().map(|s| Line::raw(s.clone())).collect()
    }

    fn is_dirty(&self) -> bool { self.dirty }
    fn mark_clean(&mut self) { self.dirty = false; }
}

/// Format elapsed milliseconds as human-readable.
fn format_elapsed(ms: u64) -> String {
    if ms < 1000 {
        format!("{ms}ms")
    } else if ms < 60_000 {
        format!("{:.1}s", ms as f64 / 1000.0)
    } else {
        format!("{}m{}s", ms / 60_000, (ms % 60_000) / 1000)
    }
}

/// Truncate output to a sensible preview length.
fn truncate_preview(output: String) -> String {
    let lines: Vec<&str> = output.lines().take(8).collect();
    if output.lines().count() > 8 {
        format!("{}\n  …", lines.join("\n"))
    } else {
        lines.join("\n")
    }
}

/// Build a short summary string from a tool's JSON input args.
pub fn summarize_input(name: &str, input: &serde_json::Value) -> String {
    match name {
        "read_file" => input["path"].as_str().unwrap_or("").to_string(),
        "write_file" | "create_file" => input["path"].as_str().unwrap_or("").to_string(),
        "bash" | "run_bash" => {
            let cmd = input["command"].as_str().unwrap_or("");
            let truncated = cmd.chars().take(60).collect::<String>();
            if cmd.len() > 60 { format!("{truncated}…") } else { truncated }
        }
        "grep" | "search" => {
            let pattern = input["pattern"].as_str().unwrap_or("");
            format!("{pattern}")
        }
        "glob" | "list_files" => input["pattern"].as_str().unwrap_or("").to_string(),
        "list_dir" => input["path"].as_str().unwrap_or("").to_string(),
        _ => String::new(),
    }
}
