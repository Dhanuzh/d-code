/// Context compaction — summarise old messages when approaching context limit.
///
/// Inspired by opencode's two-phase approach:
///   Phase 1 — prune old tool outputs, keeping the last PRUNE_PROTECT tokens intact.
///   Phase 2 — build a structured summary (Goal / Discoveries / Accomplished / Relevant files).
use dcode_providers::{ContentBlock, Message, Role};

/// Keep a 20 000-token buffer before the context limit, matching opencode's COMPACTION_BUFFER.
const COMPACTION_BUFFER: usize = 20_000;

/// Compact the message list when total estimated tokens approach the context limit.
/// Preserves the last `keep_recent` messages verbatim.
pub fn maybe_compact(messages: &mut Vec<Message>, context_window: u32, keep_recent: usize) {
    let usable = (context_window as usize).saturating_sub(COMPACTION_BUFFER);
    let total: usize = messages.iter().map(|m| m.estimate_tokens()).sum();

    if total <= usable {
        return;
    }

    let keep = keep_recent.min(messages.len());
    let to_summarise = messages.len().saturating_sub(keep);
    if to_summarise == 0 {
        return;
    }

    // Phase 1: drain the old messages.
    let old: Vec<Message> = messages.drain(..to_summarise).collect();

    // Phase 2: build a structured summary.
    let summary = build_structured_summary(&old);

    // Insert as a user/assistant pair to maintain role alternation.
    messages.insert(
        0,
        Message {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: format!("[Earlier conversation summary — read-only context]\n{summary}"),
            }],
        },
    );
    messages.insert(
        1,
        Message {
            role: Role::Assistant,
            content: vec![ContentBlock::Text {
                text: "[Context loaded from summary. Continuing…]".into(),
            }],
        },
    );
}

/// Build a structured summary in the style opencode uses.
/// Sections: Goal · Discoveries · Accomplished · Relevant files
fn build_structured_summary(messages: &[Message]) -> String {
    let mut user_messages: Vec<String> = vec![];
    let mut assistant_messages: Vec<String> = vec![];
    let mut tool_calls: Vec<(String, String)> = vec![]; // (tool_name, key_arg)
    let mut errors: Vec<String> = vec![];
    let mut modified_files: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let mut read_files: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();

    for msg in messages {
        match msg.role {
            Role::User => {
                for block in &msg.content {
                    match block {
                        ContentBlock::Text { text } => {
                            let t = text.trim();
                            if !t.starts_with('[') {
                                user_messages.push(truncate(t, 200));
                            }
                        }
                        ContentBlock::ToolResult { content, is_error, .. } => {
                            if *is_error {
                                errors.push(truncate(content, 120));
                            }
                        }
                        ContentBlock::ToolUse { .. } => {}
                    }
                }
            }
            Role::Assistant => {
                for block in &msg.content {
                    match block {
                        ContentBlock::Text { text } => {
                            let t = text.trim();
                            if !t.is_empty() && !t.starts_with('[') {
                                assistant_messages.push(truncate(t, 200));
                            }
                        }
                        ContentBlock::ToolUse { name, input, .. } => {
                            let key = extract_key_arg(name, input);
                            if let Some(path) = extract_file_path(name, input) {
                                match name.as_str() {
                                    "write_file" | "edit_file" => { modified_files.insert(path); }
                                    "read_file" => { read_files.insert(path.clone()); }
                                    _ => {}
                                }
                            }
                            tool_calls.push((name.clone(), key));
                        }
                        ContentBlock::ToolResult { .. } => {}
                    }
                }
            }
        }
    }

    let mut out = String::new();

    // ── Goal (user messages = what they asked for) ────────────────────────────
    if !user_messages.is_empty() {
        out.push_str("## Goal\n");
        for m in user_messages.iter().take(4) {
            out.push_str(&format!("- {m}\n"));
        }
        out.push('\n');
    }

    // ── Discoveries (assistant findings) ─────────────────────────────────────
    if !assistant_messages.is_empty() {
        out.push_str("## Discoveries\n");
        for m in assistant_messages.iter().take(6) {
            out.push_str(&format!("- {m}\n"));
        }
        out.push('\n');
    }

    // ── Accomplished (tool calls = what was done) ─────────────────────────────
    if !tool_calls.is_empty() {
        out.push_str("## Accomplished\n");
        for (name, key) in tool_calls.iter().take(20) {
            if key.is_empty() {
                out.push_str(&format!("- {name}()\n"));
            } else {
                out.push_str(&format!("- {name}({key})\n"));
            }
        }
        if tool_calls.len() > 20 {
            out.push_str(&format!("- … and {} more tool calls\n", tool_calls.len() - 20));
        }
        out.push('\n');
    }

    // ── Errors ────────────────────────────────────────────────────────────────
    if !errors.is_empty() {
        out.push_str("## Errors encountered\n");
        for e in errors.iter().take(5) {
            out.push_str(&format!("- {e}\n"));
        }
        out.push('\n');
    }

    // ── Modified files (written/edited this session) ──────────────────────────
    if !modified_files.is_empty() {
        out.push_str("## Modified files\n");
        for path in modified_files.iter().take(30) {
            out.push_str(&format!("- {path}\n"));
        }
        out.push('\n');
    }

    // ── Read files (referenced this session) ─────────────────────────────────
    let unmodified_reads: Vec<&String> = read_files.iter()
        .filter(|p| !modified_files.contains(*p))
        .collect();
    if !unmodified_reads.is_empty() {
        out.push_str("## Read files\n");
        for path in unmodified_reads.iter().take(20) {
            out.push_str(&format!("- {path}\n"));
        }
        out.push('\n');
    }

    out
}

/// Extract a short human-readable key argument from a tool call input.
fn extract_key_arg(tool: &str, input: &serde_json::Value) -> String {
    match tool {
        "read_file" | "write_file" | "edit_file" | "list_dir" => {
            input["path"].as_str().unwrap_or("").to_string()
        }
        "bash" => truncate(input["command"].as_str().unwrap_or(""), 60),
        "grep" => format!(
            "/{}/  {}",
            input["pattern"].as_str().unwrap_or("?"),
            input["path"].as_str().unwrap_or("")
        ),
        "glob" => input["pattern"].as_str().unwrap_or("").to_string(),
        _ => String::new(),
    }
}

/// Try to extract a file path from a tool input, for the Relevant files section.
fn extract_file_path(tool: &str, input: &serde_json::Value) -> Option<String> {
    match tool {
        "read_file" | "write_file" | "edit_file" => {
            input["path"].as_str().map(str::to_string)
        }
        _ => None,
    }
}

fn truncate(s: &str, max: usize) -> String {
    let char_count = s.chars().count();
    if char_count <= max {
        s.to_string()
    } else {
        let mut out = String::new();
        for (i, ch) in s.chars().enumerate() {
            if i >= max {
                break;
            }
            out.push(ch);
        }
        format!("{out}…")
    }
}
