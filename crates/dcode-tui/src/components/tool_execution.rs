//! ToolExecution component — mirrors pi-mono's tool-execution.ts.
//!
//! Shows tool name, status (running/done/error) with pi-mono colored backgrounds:
//!   Running  → toolPendingBg  #282832
//!   Done     → toolSuccessBg  #283228
//!   Error    → toolErrorBg    #3c2828

use std::time::Instant;
use crate::{Component, Line};

// Background colors (pi-mono dark.json)
const BG_PENDING: &str = "\x1b[48;2;40;40;50m";    // toolPendingBg  #282832
const BG_SUCCESS: &str = "\x1b[48;2;40;50;40m";    // toolSuccessBg  #283228
const BG_ERROR:   &str = "\x1b[48;2;60;40;40m";    // toolErrorBg    #3c2828

// Foreground colors
const C_ACCENT:  &str = "\x1b[38;2;138;190;183m";  // accent teal
const C_SUCCESS: &str = "\x1b[38;2;181;189;104m";  // green
const C_ERROR:   &str = "\x1b[38;2;204;102;102m";  // red
const C_MUTED:   &str = "\x1b[38;2;128;128;128m";  // gray
const C_DIM:     &str = "\x1b[38;2;102;102;102m";  // dimGray
const RESET:     &str = "\x1b[0m";
const BOLD:      &str = "\x1b[1m";

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
    pub input_summary: String,
    pub output_preview: String,
    pub elapsed_ms: u64,
    started_at: Instant,
    dirty: bool,
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
        }
    }

    pub fn finish(&mut self, output: impl Into<String>, is_error: bool, input_summary: impl Into<String>) {
        self.status = if is_error { ToolStatus::Error } else { ToolStatus::Done };
        self.elapsed_ms = self.started_at.elapsed().as_millis() as u64;
        self.output_preview = truncate_preview(output.into());
        self.input_summary = input_summary.into();
        self.dirty = true;
    }

    pub fn current_elapsed_ms(&self) -> u64 {
        self.started_at.elapsed().as_millis() as u64
    }

    fn build_lines(&self, width: u16) -> Vec<String> {
        let w = width as usize;
        let mut lines = Vec::new();

        match self.status {
            ToolStatus::Running => {
                let bg = BG_PENDING;
                let elapsed = format_elapsed(self.current_elapsed_ms());
                let summary = if !self.input_summary.is_empty() {
                    format!("  {C_DIM}{}{RESET}", self.input_summary)
                } else {
                    String::new()
                };
                let content = format!(
                    " {C_ACCENT}◦ {BOLD}{}{RESET}{bg}{summary}  {C_DIM}{elapsed}",
                    self.name
                );
                lines.push(bg_line(bg, &content, w));
            }
            ToolStatus::Done => {
                let bg = BG_SUCCESS;
                let elapsed = format_elapsed(self.elapsed_ms);
                let summary = if !self.input_summary.is_empty() {
                    format!("  {C_DIM}{}{RESET}", self.input_summary)
                } else {
                    String::new()
                };
                let content = format!(
                    " {C_SUCCESS}✓ {BOLD}{}{RESET}{bg}{summary}  {C_DIM}{elapsed}",
                    self.name
                );
                lines.push(bg_line(bg, &content, w));
                if !self.output_preview.is_empty() {
                    for l in self.output_preview.lines().take(6) {
                        let out_content = format!("   {C_MUTED}{l}");
                        lines.push(bg_line(bg, &out_content, w));
                    }
                }
            }
            ToolStatus::Error => {
                let bg = BG_ERROR;
                let elapsed = format_elapsed(self.elapsed_ms);
                let summary = if !self.input_summary.is_empty() {
                    format!("  {C_DIM}{}{RESET}", self.input_summary)
                } else {
                    String::new()
                };
                let content = format!(
                    " {C_ERROR}✗ {BOLD}{}{RESET}{bg}{summary}  {C_DIM}{elapsed}",
                    self.name
                );
                lines.push(bg_line(bg, &content, w));
                if !self.output_preview.is_empty() {
                    for l in self.output_preview.lines().take(4) {
                        let out_content = format!("   {C_ERROR}{l}");
                        lines.push(bg_line(bg, &out_content, w));
                    }
                }
            }
        }

        lines
    }
}

impl Component for ToolExecution {
    fn render(&mut self, width: u16) -> Vec<Line> {
        // Running tools re-render every frame for live elapsed time.
        let lines = self.build_lines(width);
        self.dirty = self.status == ToolStatus::Running;
        lines.into_iter().map(|s| Line::raw(s)).collect()
    }

    fn is_dirty(&self) -> bool { self.dirty }
    fn mark_clean(&mut self) { self.dirty = false; }
}

/// Apply background color to a content string and pad to full terminal width.
///
/// The content may contain ANSI codes (foreground colors, resets). We need to:
/// 1. Set the background color
/// 2. Print the content (which may include fg color resets — those clear the bg!)
/// 3. Pad remaining visual columns with the background still active
/// 4. Final RESET
///
/// To prevent fg RESET codes inside content from clearing the background mid-line,
/// we re-apply the bg color after any reset. We do this by replacing `\x1b[0m`
/// within the content with `\x1b[0m<BG>`.
fn bg_line(bg: &str, content: &str, width: usize) -> String {
    // Replace resets inside content with reset + re-apply bg so the bg persists.
    let content_patched = content.replace(RESET, &format!("{RESET}{bg}"));
    let visible = strip_ansi_len(content);
    let pad = width.saturating_sub(visible);
    format!("{bg}{content_patched}{}{RESET}", " ".repeat(pad))
}

fn format_elapsed(ms: u64) -> String {
    if ms < 1000 {
        format!("{ms}ms")
    } else if ms < 60_000 {
        format!("{:.1}s", ms as f64 / 1000.0)
    } else {
        format!("{}m{}s", ms / 60_000, (ms % 60_000) / 1000)
    }
}

fn truncate_preview(output: String) -> String {
    let lines: Vec<&str> = output.lines().take(6).collect();
    if output.lines().count() > 6 {
        format!("{}\n  …", lines.join("\n"))
    } else {
        lines.join("\n")
    }
}

fn strip_ansi_len(s: &str) -> usize {
    crate::line::strip_ansi(s).len()
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
        "grep" | "search" => input["pattern"].as_str().unwrap_or("").to_string(),
        "glob" | "list_files" => input["pattern"].as_str().unwrap_or("").to_string(),
        "list_dir" => input["path"].as_str().unwrap_or("").to_string(),
        _ => String::new(),
    }
}
