/// Terminal renderer: streaming text with minimal markdown highlighting.
use crossterm::style::{Attribute, Color, ResetColor, SetAttribute, SetForegroundColor};
use crossterm::{execute, style::Print};
use std::io::{stdout, Write};

/// Render a streaming text delta to the terminal.
/// Detects code fences and applies dim color, headers get bold.
pub fn render_delta(text: &str, in_code_block: &mut bool) {
    let mut out = stdout();
    for line in text.split_inclusive('\n') {
        if line.starts_with("```") {
            *in_code_block = !*in_code_block;
            if *in_code_block {
                let _ = execute!(out, SetForegroundColor(Color::DarkGrey));
            } else {
                let _ = execute!(out, ResetColor);
            }
            let _ = execute!(out, Print(line));
            continue;
        }
        if *in_code_block {
            let _ = execute!(out, Print(line));
            continue;
        }
        // Headers.
        if let Some(rest) = line.strip_prefix("### ") {
            let _ = execute!(out, SetAttribute(Attribute::Bold), Print(format!("### {rest}")), SetAttribute(Attribute::Reset));
        } else if let Some(rest) = line.strip_prefix("## ") {
            let _ = execute!(out, SetAttribute(Attribute::Bold), SetForegroundColor(Color::Cyan), Print(format!("## {rest}")), ResetColor, SetAttribute(Attribute::Reset));
        } else if let Some(rest) = line.strip_prefix("# ") {
            let _ = execute!(out, SetAttribute(Attribute::Bold), SetForegroundColor(Color::Green), Print(format!("# {rest}")), ResetColor, SetAttribute(Attribute::Reset));
        } else {
            let _ = execute!(out, Print(line));
        }
    }
    let _ = out.flush();
}

pub fn print_tool_start(name: &str) {
    let _ = execute!(
        stdout(),
        Print("\n"),
        SetForegroundColor(Color::DarkYellow),
        Print(format!("  ⟳ {name}")),
        ResetColor,
        Print(" "),
    );
    let _ = stdout().flush();
}

pub fn print_tool_done(name: &str, is_error: bool) {
    let color = if is_error { Color::Red } else { Color::Green };
    let symbol = if is_error { "✗" } else { "✓" };
    let _ = execute!(
        stdout(),
        SetForegroundColor(color),
        Print(format!("{symbol} {name}\n")),
        ResetColor,
    );
}

pub fn print_info(msg: &str) {
    let _ = execute!(
        stdout(),
        SetForegroundColor(Color::DarkGrey),
        Print(format!("{msg}\n")),
        ResetColor,
    );
}

pub fn print_error(msg: &str) {
    let _ = execute!(
        stdout(),
        SetForegroundColor(Color::Red),
        Print(format!("error: {msg}\n")),
        ResetColor,
    );
}

pub fn print_prompt_hint(provider: &str) {
    let _ = execute!(
        stdout(),
        SetForegroundColor(Color::DarkGrey),
        Print(format!("[{provider}] "),),
        ResetColor,
    );
    let _ = stdout().flush();
}
