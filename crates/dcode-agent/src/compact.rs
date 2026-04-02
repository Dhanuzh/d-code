/// Context compaction — summarise old messages when approaching context limit.
use dcode_providers::{ContentBlock, Message, Role};

/// Compact the message list when total estimated tokens exceed `threshold`.
/// Preserves the last `keep_recent` messages verbatim.
pub fn maybe_compact(messages: &mut Vec<Message>, context_window: u32, keep_recent: usize) {
    let threshold = (context_window as usize) * 55 / 100;
    let total: usize = messages.iter().map(|m| m.estimate_tokens()).sum();

    if total <= threshold {
        return;
    }

    let keep = keep_recent.min(messages.len());
    let to_summarise = messages.len().saturating_sub(keep);
    if to_summarise == 0 {
        return;
    }

    let old: Vec<Message> = messages.drain(..to_summarise).collect();
    let summary = summarise(&old);

    // Inject summary as first user message.
    messages.insert(
        0,
        Message {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: format!(
                    "[Context summary — earlier conversation]\n{summary}\n\
                     [End summary. Continuing from here.]"
                ),
            }],
        },
    );
}

fn summarise(messages: &[Message]) -> String {
    let mut parts = vec![];
    for msg in messages {
        let role = match msg.role {
            Role::User => "User",
            Role::Assistant => "Assistant",
        };
        for block in &msg.content {
            match block {
                ContentBlock::Text { text } => {
                    let snippet = truncate(text, 400);
                    parts.push(format!("{role}: {snippet}"));
                }
                ContentBlock::ToolUse { name, input, .. } => {
                    let args = truncate(&input.to_string(), 200);
                    parts.push(format!("Tool call: {name}({args})"));
                }
                ContentBlock::ToolResult { content, .. } => {
                    let snippet = truncate(content, 200);
                    parts.push(format!("Tool result: {snippet}"));
                }
            }
        }
    }
    parts.join("\n")
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}...", &s[..max])
    }
}
