pub mod compact;
pub mod prompt;
pub mod session;

use std::path::PathBuf;

use anyhow::Context;
use dcode_providers::{
    BoxProvider, ContentBlock, Message, Role, StopReason, StreamEvent, ToolDef,
};
use dcode_tools::builtin_tools;
use futures::StreamExt;

pub use session::Session;

/// Events emitted by the agent to the UI layer.
#[derive(Debug)]
pub enum AgentEvent {
    /// A chunk of assistant text (stream it live).
    TextDelta(String),
    /// A tool call is starting.
    ToolStart { name: String },
    /// A tool call finished with result.
    ToolDone { name: String, result: String, is_error: bool },
    /// Token usage update.
    TokenUsage { input: u32, output: u32 },
    /// The agent turn is complete.
    TurnDone,
}

/// The main agentic loop.
pub struct Agent {
    provider: BoxProvider,
    pub session: Session,
    cwd: PathBuf,
    tools: Vec<ToolDef>,
    max_tokens: u32,
    /// Maximum tool call iterations per turn (prevents infinite loops).
    max_iterations: u32,
}

impl Agent {
    pub fn new(provider: BoxProvider, cwd: PathBuf) -> Self {
        let max_tokens = match provider.name() {
            "anthropic" => 4_096,
            _ => 2_048,
        };
        Self {
            tools: builtin_tools(),
            provider,
            session: Session::new(),
            cwd,
            max_tokens,
            max_iterations: 8,
        }
    }

    pub fn provider_info(&self) -> String {
        format!("{} / {}", self.provider.name(), self.provider.model())
    }

    pub fn provider_name(&self) -> &str {
        self.provider.name()
    }

    pub fn model_name(&self) -> &str {
        self.provider.model()
    }

    pub fn replace_provider(&mut self, provider: BoxProvider) {
        self.provider = provider;
    }

    /// Run one user turn through the agentic loop.
    /// Emits `AgentEvent`s via the callback.
    pub async fn run_turn<F>(&mut self, user_input: &str, mut on_event: F) -> anyhow::Result<()>
    where
        F: FnMut(AgentEvent),
    {
        // Add the user message.
        self.session.push(Message::user(user_input));

        // Compact context if needed.
        compact::maybe_compact(
            &mut self.session.messages,
            self.provider.context_window(),
            6,
        );

        let system = prompt::build_system_prompt(&self.cwd);
        let tools_for_turn: &[ToolDef] = if should_enable_tools(user_input) {
            &self.tools
        } else {
            &[]
        };
        let max_iterations = if tools_for_turn.is_empty() {
            1
        } else {
            self.max_iterations
        };

        for _iter in 0..max_iterations {
            let mut stream = self
                .provider
                .chat_stream(
                    &system,
                    &self.session.messages,
                    tools_for_turn,
                    self.max_tokens,
                )
                .await
                .context("chat_stream")?;

            // Collect the response.
            let mut text_buf = String::new();
            let mut tool_calls: Vec<PendingToolCall> = vec![];
            let mut current_tool: Option<PendingToolCall> = None;
            let mut stop_reason = StopReason::EndTurn;

            while let Some(ev) = stream.next().await {
                let ev = ev.context("stream event")?;
                match ev {
                    StreamEvent::TextDelta(t) => {
                        on_event(AgentEvent::TextDelta(t.clone()));
                        text_buf.push_str(&t);
                    }
                    StreamEvent::ToolUseStart { id, name } => {
                        on_event(AgentEvent::ToolStart { name: name.clone() });
                        current_tool = Some(PendingToolCall {
                            id,
                            name,
                            input_buf: String::new(),
                        });
                    }
                    StreamEvent::ToolUseDelta(delta) => {
                        if let Some(tc) = &mut current_tool {
                            tc.input_buf.push_str(&delta);
                        }
                    }
                    StreamEvent::ToolUseEnd => {
                        if let Some(tc) = current_tool.take() {
                            tool_calls.push(tc);
                        }
                    }
                    StreamEvent::Usage { input_tokens, output_tokens } => {
                        self.session.record_usage(input_tokens, output_tokens);
                        on_event(AgentEvent::TokenUsage { input: input_tokens, output: output_tokens });
                    }
                    StreamEvent::Done { stop_reason: r } => {
                        stop_reason = r;
                    }
                }
            }

            // Build the assistant message.
            let mut assistant_content: Vec<ContentBlock> = vec![];
            if !text_buf.is_empty() {
                assistant_content.push(ContentBlock::Text { text: text_buf });
            }
            for tc in &tool_calls {
                let input: serde_json::Value =
                    serde_json::from_str(&tc.input_buf).unwrap_or(serde_json::Value::Null);
                assistant_content.push(ContentBlock::ToolUse {
                    id: tc.id.clone(),
                    name: tc.name.clone(),
                    input,
                });
            }
            if !assistant_content.is_empty() {
                self.session.push(Message {
                    role: Role::Assistant,
                    content: assistant_content,
                });
            }

            // No tool calls → done.
            if tool_calls.is_empty() || stop_reason == StopReason::EndTurn {
                break;
            }

            // Execute tool calls and collect results.
            let mut tool_results: Vec<ContentBlock> = vec![];
            for tc in &tool_calls {
                let input: serde_json::Value =
                    serde_json::from_str(&tc.input_buf).unwrap_or(serde_json::Value::Null);
                let (result, is_error) = match dcode_tools::dispatch(&tc.name, &input).await {
                    Ok(r) => (r, false),
                    Err(e) => (format!("Error: {e}"), true),
                };
                on_event(AgentEvent::ToolDone {
                    name: tc.name.clone(),
                    result: result.clone(),
                    is_error,
                });
                tool_results.push(ContentBlock::ToolResult {
                    tool_use_id: tc.id.clone(),
                    content: compact_tool_result(&result),
                    is_error,
                });
            }

            // Add tool results as user message.
            self.session.push(Message {
                role: Role::User,
                content: tool_results,
            });

            // Compact again if context grew.
            compact::maybe_compact(
                &mut self.session.messages,
                self.provider.context_window(),
                6,
            );
        }

        on_event(AgentEvent::TurnDone);
        Ok(())
    }
}

fn compact_tool_result(result: &str) -> String {
    const MAX: usize = 1200;
    if result.len() <= MAX {
        return result.to_string();
    }
    let mut end = 0;
    for (idx, _) in result.char_indices() {
        if idx <= MAX {
            end = idx;
        } else {
            break;
        }
    }
    let kept = &result[..end];
    format!(
        "{kept}\n\n[tool output truncated: {} chars omitted]",
        result.len().saturating_sub(end)
    )
}

fn should_enable_tools(input: &str) -> bool {
    let s = input.to_ascii_lowercase();
    [
        "file",
        "code",
        "edit",
        "write",
        "read",
        "patch",
        "refactor",
        "fix",
        "test",
        "build",
        "run",
        "compile",
        "cargo",
        "git",
        "commit",
        "search",
        "grep",
        "bug",
    ]
    .iter()
    .any(|k| s.contains(k))
}

struct PendingToolCall {
    id: String,
    name: String,
    input_buf: String,
}
