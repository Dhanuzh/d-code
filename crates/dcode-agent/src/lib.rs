pub mod compact;
pub mod prompt;
pub mod session;

pub use compact::maybe_compact;

use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::Context;
use dcode_providers::{BoxProvider, ContentBlock, Message, Role, StopReason, StreamEvent, ToolDef};
use dcode_tools::builtin_tools;
use futures::StreamExt;

pub use session::Session;

/// Events emitted by the agent to the UI layer.
#[derive(Debug)]
pub enum AgentEvent {
    /// A chunk of assistant text (stream it live).
    TextDelta(String),
    /// A tool call is starting (name only — input not yet complete).
    ToolStart { name: String },
    /// A tool call finished with result + the parsed input args.
    ToolDone {
        name: String,
        input: serde_json::Value,
        result: String,
        is_error: bool,
    },
    /// Token usage update.
    TokenUsage { input: u32, output: u32 },
    /// The agent detected a doom loop and stopped.
    DoomLoop { tool: String },
    /// The agent is asking the user a question (ask_user tool).
    UserQuestion { question: String, choices: Vec<String> },
    /// The agent is about to run a dangerous bash command — shown before approval prompt.
    ConfirmBash { command: String },
    /// The agent turn is complete.
    TurnDone,
}

/// The main agentic loop.
pub struct Agent {
    pub provider: BoxProvider,
    pub session: Session,
    cwd: PathBuf,
    tools: Vec<ToolDef>,
    default_max_tokens: u32,
    /// Maximum tool call iterations per turn (prevents infinite loops).
    max_iterations: u32,
    /// Called before executing a dangerous bash command. Return false to block it.
    /// If None, all commands execute without confirmation (dangerous ones still warned via event).
    pub bash_approver: Option<Box<dyn Fn(&str) -> bool + Send + Sync>>,
    /// Called when the AI uses the ask_user tool. Returns the user's answer.
    pub user_prompter: Option<Box<dyn Fn(&str, &[String]) -> String + Send + Sync>>,
}

impl Agent {
    pub fn new(provider: BoxProvider, cwd: PathBuf) -> Self {
        // 4096 is plenty for most tasks; pick_max_tokens raises it for heavy requests.
        let default_max_tokens = 4_096;
        Self {
            tools: builtin_tools(),
            provider,
            session: Session::new(),
            cwd,
            default_max_tokens,
            max_iterations: 12,
            bash_approver: None,
            user_prompter: None,
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

    pub fn provider_context_window(&self) -> u32 {
        self.provider.context_window()
    }

    /// Run one user turn through the agentic loop.
    /// Emits `AgentEvent`s via the callback.
    pub async fn run_turn<F>(&mut self, user_input: &str, mut on_event: F) -> anyhow::Result<()>
    where
        F: FnMut(AgentEvent),
    {
        // Add the user message.
        self.session.push(Message::user(user_input));

        // Compact context if needed before the turn.
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
        let max_iterations = if tools_for_turn.is_empty() { 1 } else { self.max_iterations };
        let max_tokens = self.pick_max_tokens(user_input, tools_for_turn.is_empty());

        // Doom-loop tracker: (tool_name, canonical_input_json) → call_count.
        // If the same tool is called ≥3 times with the same args, we abort the
        // turn to prevent burning tokens on a stuck loop (same pattern as opencode).
        let mut doom_tracker: HashMap<String, usize> = HashMap::new();

        for _iter in 0..max_iterations {
            let mut stream = self
                .provider
                .chat_stream(&system, &self.session.messages, tools_for_turn, max_tokens)
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
                            input: serde_json::Value::Null,
                        });
                    }
                    StreamEvent::ToolUseDelta(delta) => {
                        if let Some(tc) = &mut current_tool {
                            tc.input_buf.push_str(&delta);
                        }
                    }
                    StreamEvent::ToolUseEnd => {
                        if let Some(mut tc) = current_tool.take() {
                            // Parse once here — reused for assistant message, doom detection, and dispatch.
                            tc.input = serde_json::from_str(&tc.input_buf)
                                .unwrap_or(serde_json::Value::Null);
                            tool_calls.push(tc);
                        }
                    }
                    StreamEvent::Usage { input_tokens, output_tokens } => {
                        self.session.record_usage(input_tokens, output_tokens);
                        on_event(AgentEvent::TokenUsage {
                            input: input_tokens,
                            output: output_tokens,
                        });
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
                assistant_content.push(ContentBlock::ToolUse {
                    id: tc.id.clone(),
                    name: tc.name.clone(),
                    input: tc.input.clone(),
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

            // ── Doom-loop detection ─────────────────────────────────────────────
            // Build canonical key: "tool_name|sorted_json" so minor JSON whitespace
            // differences don't count as different calls.
            let mut doom_hit: Option<String> = None;
            for tc in &tool_calls {
                let key = format!("{}|{}", tc.name, canonical_json(&tc.input));
                let count = doom_tracker.entry(key).or_insert(0);
                *count += 1;
                if *count >= 3 {
                    doom_hit = Some(tc.name.clone());
                    break;
                }
            }

            if let Some(tool_name) = doom_hit {
                on_event(AgentEvent::DoomLoop { tool: tool_name.clone() });
                // Inject a note into the conversation so the model knows why we stopped.
                self.session.push(Message {
                    role: Role::User,
                    content: vec![ContentBlock::Text {
                        text: format!(
                            "[System: Doom loop detected — '{tool_name}' was called 3× with the same arguments. \
                             Stop repeating this call. Change your approach or ask the user for clarification.]"
                        ),
                    }],
                });
                break;
            }

            // ── Execute tool calls ──────────────────────────────────────────────
            // Interactive tools (ask_user, bash with confirmation) run sequentially.
            // Pure read-only tools (read_file, glob, grep, list_dir, read_image) run
            // in parallel to eliminate round-trip latency when the model batches them.
            let mut tool_results: Vec<ContentBlock> = vec![];

            // Partition: interactive vs parallelisable.
            let is_parallel_safe = |name: &str| {
                matches!(name, "read_file" | "glob" | "grep" | "list_dir" | "read_image")
            };

            // Handle interactive tools sequentially first, preserving order via index.
            let mut parallel_indices: Vec<usize> = vec![];
            let placeholder = ContentBlock::ToolResult {
                tool_use_id: String::new(),
                content: String::new(),
                is_error: false,
            };
            // Pre-fill results vec with placeholders; fill in order below.
            tool_results.resize(tool_calls.len(), placeholder);

            for (i, tc) in tool_calls.iter().enumerate() {
                if is_parallel_safe(&tc.name) {
                    parallel_indices.push(i);
                    continue; // deferred
                }

                // ask_user
                if tc.name == "ask_user" {
                    let question = tc.input["question"].as_str().unwrap_or("?");
                    let choices: Vec<String> = tc.input["choices"]
                        .as_array()
                        .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
                        .unwrap_or_default();
                    on_event(AgentEvent::UserQuestion {
                        question: question.to_string(),
                        choices: choices.clone(),
                    });
                    let answer = if let Some(ref prompter) = self.user_prompter {
                        prompter(question, &choices)
                    } else {
                        "[User unavailable]".to_string()
                    };
                    tool_results[i] = ContentBlock::ToolResult {
                        tool_use_id: tc.id.clone(),
                        content: answer,
                        is_error: false,
                    };
                    continue;
                }

                // bash (possibly interactive)
                if tc.name == "bash" {
                    let cmd = tc.input["command"].as_str().unwrap_or("");
                    let danger = is_dangerous_bash(cmd);
                    let confirm_all = self.bash_approver.is_some() && !danger;
                    if danger || confirm_all {
                        on_event(AgentEvent::ConfirmBash { command: cmd.to_string() });
                        if let Some(ref approver) = self.bash_approver {
                            if !approver(cmd) {
                                on_event(AgentEvent::ToolDone {
                                    name: tc.name.clone(),
                                    input: tc.input.clone(),
                                    result: "[blocked by user]".to_string(),
                                    is_error: false,
                                });
                                tool_results[i] = ContentBlock::ToolResult {
                                    tool_use_id: tc.id.clone(),
                                    content: "[Command blocked: user denied execution. Try a different approach.]".to_string(),
                                    is_error: false,
                                };
                                continue;
                            }
                        }
                    }
                }

                let (result, is_error) = match dcode_tools::dispatch(&tc.name, &tc.input, &self.cwd).await {
                    Ok(r) => (r, false),
                    Err(e) => (format!("Error: {e}"), true),
                };
                on_event(AgentEvent::ToolDone {
                    name: tc.name.clone(),
                    input: tc.input.clone(),
                    result: result.clone(),
                    is_error,
                });
                tool_results[i] = ContentBlock::ToolResult {
                    tool_use_id: tc.id.clone(),
                    content: compact_tool_result(&result),
                    is_error,
                };
            }

            // Run parallel-safe tools concurrently.
            if !parallel_indices.is_empty() {
                let futures: Vec<_> = parallel_indices.iter().map(|&i| {
                    let tc = &tool_calls[i];
                    let name = tc.name.clone();
                    let input = tc.input.clone();
                    let cwd = self.cwd.clone();
                    async move {
                        let (result, is_error) = match dcode_tools::dispatch(&name, &input, &cwd).await {
                            Ok(r) => (r, false),
                            Err(e) => (format!("Error: {e}"), true),
                        };
                        (i, name, input, result, is_error)
                    }
                }).collect();

                let outcomes = futures::future::join_all(futures).await;
                for (i, name, input, result, is_error) in outcomes {
                    on_event(AgentEvent::ToolDone {
                        name,
                        input,
                        result: result.clone(),
                        is_error,
                    });
                    tool_results[i] = ContentBlock::ToolResult {
                        tool_use_id: tool_calls[i].id.clone(),
                        content: compact_tool_result(&result),
                        is_error,
                    };
                }
            }

            // Add tool results as user message.
            self.session.push(Message {
                role: Role::User,
                content: tool_results,
            });

            // Compact again after tool results (context may have grown).
            compact::maybe_compact(
                &mut self.session.messages,
                self.provider.context_window(),
                6,
            );
        }

        // After the turn: stub out image data URIs everywhere — the model already
        // saw and responded to them; keeping MB of base64 in context is wasteful.
        stub_image_tool_results(&mut self.session.messages);

        // Trim stale text tool-result content in older messages.
        // Keep the last 8 messages (4 full turns) intact; older messages get stubs.
        trim_old_tool_results(&mut self.session.messages, 8);

        on_event(AgentEvent::TurnDone);
        Ok(())
    }

    fn pick_max_tokens(&self, user_input: &str, no_tools: bool) -> u32 {
        let text = user_input.to_ascii_lowercase();
        let budget = self.default_max_tokens; // 4096

        // Simple Q&A: cap at 1 500 to avoid over-generating.
        if no_tools
            && ["explain", "what", "why", "how", "summarize", "summary", "status", "describe"]
                .iter()
                .any(|k| text.contains(k))
        {
            return budget.min(1_500);
        }

        // Heavyweight code generation: raise ceiling.
        if ["refactor", "implement", "generate", "scaffold", "rewrite",
            "full implementation", "complete", "entire", "all files"]
            .iter()
            .any(|k| text.contains(k))
            || user_input.len() > 2_000
        {
            return budget.max(8_192);
        }

        budget
    }
}

/// Produce a canonical, stable JSON string for doom-loop key comparison.
/// Serialises keys in sorted order so `{"b":1,"a":2}` == `{"a":2,"b":1}`.
fn canonical_json(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::Object(map) => {
            let mut keys: Vec<_> = map.keys().collect();
            keys.sort();
            let inner: Vec<String> = keys
                .iter()
                .map(|k| format!("\"{}\":{}", k, canonical_json(&map[*k])))
                .collect();
            format!("{{{}}}", inner.join(","))
        }
        other => other.to_string(),
    }
}

/// Replace ALL image data URIs in tool results with a short stub.
/// Called after every turn — the model already responded to the image,
/// so keeping MB of base64 in context wastes tokens on every subsequent request.
fn stub_image_tool_results(messages: &mut Vec<Message>) {
    for msg in messages.iter_mut() {
        for block in msg.content.iter_mut() {
            if let ContentBlock::ToolResult { content, .. } = block {
                if content.starts_with("data:image/") {
                    // Extract the mime type for the stub message.
                    let mime = content
                        .strip_prefix("data:")
                        .and_then(|s| s.split(';').next())
                        .unwrap_or("image");
                    *content = format!("[{mime} image — already shown to model]");
                }
            }
        }
    }
}

/// After each turn, shrink ToolResult content in older messages.
/// Keeps only the last `keep_recent` messages untouched.
/// Images (data URIs) in old messages are replaced with a short stub to save context.
fn trim_old_tool_results(messages: &mut Vec<Message>, keep_recent: usize) {
    const STUB_MAX: usize = 120;
    let len = messages.len();
    let trim_up_to = len.saturating_sub(keep_recent);
    for msg in messages[..trim_up_to].iter_mut() {
        for block in msg.content.iter_mut() {
            if let ContentBlock::ToolResult { content, .. } = block {
                // Images are already stubbed by stub_image_tool_results before this runs.
                if content.len() > STUB_MAX {
                    let mut end = STUB_MAX;
                    while end > 0 && !content.is_char_boundary(end) {
                        end -= 1;
                    }
                    *content = format!("{}…", &content[..end]);
                }
            }
        }
    }
}

/// Trim a fresh tool result before storing in message history.
/// 12 000 chars ≈ 3 000 tokens.
/// Image data URIs are kept intact — truncating base64 corrupts the image.
fn compact_tool_result(result: &str) -> String {
    // Never truncate image data URIs — must remain intact for provider serialization.
    if result.starts_with("data:image/") {
        return result.to_string();
    }
    const MAX: usize = 12_000;
    if result.len() <= MAX {
        return result.to_string();
    }
    let mut end = MAX;
    while end > 0 && !result.is_char_boundary(end) {
        end -= 1;
    }
    format!(
        "{}\n[… +{} chars truncated — use grep/read_file with line ranges to view more]",
        &result[..end],
        result.len().saturating_sub(end)
    )
}

fn should_enable_tools(input: &str) -> bool {
    let s = input.to_ascii_lowercase();

    // Pure question starters → no tools needed (saves tokens on Q&A).
    let is_pure_qa = ["what is", "what are", "why is", "why are", "how does", "how do",
        "explain", "describe", "tell me", "can you tell", "summarize", "summary",
        "what does", "what did", "what should", "help me understand",
    ]
    .iter()
    .any(|k| s.starts_with(k));

    if is_pure_qa {
        // Still allow tools if the question references a specific file path or line.
        let has_file_ref = s.contains(".rs") || s.contains(".toml") || s.contains(".json")
            || s.contains(".py") || s.contains(".ts") || s.contains(".js")
            || s.contains("line ") || s.contains("src/") || s.contains("crate");
        if !has_file_ref {
            return false;
        }
    }

    [
        // Explicit file/edit actions
        "read file", "write file", "edit file", "open file", "create file", "delete file",
        "read the file", "edit the", "write to", "save to",
        // Code mutations
        "fix", "patch", "refactor", "implement", "add ", "update ", "remove ", "change ",
        "rename", "move ", "copy ",
        // Build/test/run
        "test", "build", "run ", "compile", "cargo", "npm", "yarn", "make ", "lint",
        "format", "check ", "coverage",
        // Git actions
        "git ", "commit", "branch", "merge", "diff", "push", "pull", "clone",
        // Search actions
        "search", "grep", "find ", "locate",
        // Debug
        "bug", "error", "fail", "crash", "debug", "trace",
        // Directory listing (explicit request)
        "list files", "list dir", "show files", "show dir",
        // Web / fetch (action verbs)
        "fetch ", "download", "curl ", "browse ",
        // Image paths / vision
        "image", "screenshot", "photo", ".png", ".jpg", ".jpeg", ".gif", ".webp",
        "look at this", "see this", "read this image",
    ]
    .iter()
    .any(|k| s.contains(k))
}

/// Returns true if the bash command matches known dangerous patterns.
/// These commands get a confirmation prompt regardless of settings.
fn is_dangerous_bash(cmd: &str) -> bool {
    let c = cmd.to_ascii_lowercase();
    // Destructive filesystem ops
    if c.contains("rm -rf") || c.contains("rm -fr") || c.contains("sudo rm") {
        return true;
    }
    // Disk/filesystem operations
    if c.contains("mkfs") || c.contains("dd if=") || c.contains("shred") {
        return true;
    }
    // Privilege escalation writing to system paths
    if (c.contains("sudo") || c.contains("tee ")) && (c.contains("/etc/") || c.contains("/sys/") || c.contains("/dev/")) {
        return true;
    }
    // Pipe to shell (classic supply-chain attack vector)
    if (c.contains("| sh") || c.contains("| bash") || c.contains("|sh") || c.contains("|bash"))
        && (c.contains("curl") || c.contains("wget") || c.contains("http"))
    {
        return true;
    }
    // Fork bomb
    if c.contains(":(){ :|:") {
        return true;
    }
    // Dangerous chmod/chown on root
    if (c.contains("chmod") || c.contains("chown")) && c.contains(" /") && !c.contains(" ./") {
        return true;
    }
    false
}

struct PendingToolCall {
    id: String,
    name: String,
    /// Raw JSON string accumulated from stream deltas.
    input_buf: String,
    /// Parsed once when the tool call is finalised (ToolUseEnd).
    input: serde_json::Value,
}
