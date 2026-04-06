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

        // Per-turn read cache: (path|start|end) → result string.
        // Prevents re-reading the same file section if the model requests it twice.
        let mut read_cache: HashMap<String, String> = HashMap::new();

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
            // Parallelism strategy:
            //   • read_file, glob, grep, list_dir, read_image: always parallel (pure reads)
            //   • web_fetch, web_search: always parallel (independent network I/O)
            //   • write_file, edit_file: parallel only when all target different paths
            //   • bash, ask_user: always sequential (side-effects / interactive)
            //
            // ToolDone events fire as each parallel tool finishes (FuturesUnordered),
            // not all-at-once after join_all. This gives the user progressive feedback.
            //
            // Per-turn read cache: if the model reads the same file+range twice, the
            // second call is served from memory without re-reading disk or burning tokens.
            let mut tool_results: Vec<ContentBlock> = vec![];

            // Pre-fill results vec with placeholders.
            let placeholder = ContentBlock::ToolResult {
                tool_use_id: String::new(),
                content: String::new(),
                is_error: false,
            };
            tool_results.resize(tool_calls.len(), placeholder);

            // Decide write/edit parallelism: safe only if no two calls share the same path.
            let write_conflict = {
                let mut paths = std::collections::HashSet::new();
                let mut conflict = false;
                for tc in &tool_calls {
                    if matches!(tc.name.as_str(), "write_file" | "edit_file") {
                        let p = tc.input["path"].as_str().unwrap_or("").to_string();
                        if !p.is_empty() && !paths.insert(p) {
                            conflict = true;
                            break;
                        }
                    }
                }
                conflict
            };

            let is_parallel_safe = |name: &str| -> bool {
                match name {
                    // Pure read-only: always safe to parallelize.
                    "read_file" | "glob" | "grep" | "list_dir" | "read_image" => true,
                    // Independent network I/O: safe to parallelize.
                    "web_fetch" | "web_search" => true,
                    // Write ops: safe only when all target different paths.
                    "write_file" | "edit_file" => !write_conflict,
                    _ => false,
                }
            };

            let mut parallel_indices: Vec<usize> = vec![];

            // ── Sequential tools (bash, ask_user, conflicting writes) ──────────
            for (i, tc) in tool_calls.iter().enumerate() {
                if is_parallel_safe(&tc.name) {
                    parallel_indices.push(i);
                    continue;
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
                    content: compact_tool_result(&tc.name, &result),
                    is_error,
                };
            }

            // ── Parallel tools — FuturesUnordered for progressive ToolDone ────
            // Events fire as each tool finishes, not all-at-once after join_all.
            if !parallel_indices.is_empty() {
                // Check per-turn cache for read_file hits before dispatching.
                let mut cache_hits: Vec<usize> = vec![];
                let mut dispatch_indices: Vec<usize> = vec![];
                for &i in &parallel_indices {
                    let tc = &tool_calls[i];
                    if tc.name == "read_file" {
                        let key = read_cache_key(&tc.input);
                        if read_cache.contains_key(&key) {
                            cache_hits.push(i);
                            continue;
                        }
                    }
                    dispatch_indices.push(i);
                }

                // Serve cache hits immediately.
                for i in cache_hits {
                    let tc = &tool_calls[i];
                    let key = read_cache_key(&tc.input);
                    let result = read_cache[&key].clone();
                    on_event(AgentEvent::ToolDone {
                        name: tc.name.clone(),
                        input: tc.input.clone(),
                        result: result.clone(),
                        is_error: false,
                    });
                    tool_results[i] = ContentBlock::ToolResult {
                        tool_use_id: tc.id.clone(),
                        content: compact_tool_result(&tc.name, &result),
                        is_error: false,
                    };
                }

                // Dispatch remaining in parallel; emit ToolDone as each completes.
                let mut futs: futures::stream::FuturesUnordered<_> = dispatch_indices
                    .iter()
                    .map(|&i| {
                        let tc = &tool_calls[i];
                        let name = tc.name.clone();
                        let input = tc.input.clone();
                        let cwd = self.cwd.clone();
                        async move {
                            let (result, is_error) =
                                match dcode_tools::dispatch(&name, &input, &cwd).await {
                                    Ok(r) => (r, false),
                                    Err(e) => (format!("Error: {e}"), true),
                                };
                            (i, name, input, result, is_error)
                        }
                    })
                    .collect();

                use futures::StreamExt;
                while let Some((i, name, input, result, is_error)) = futs.next().await {
                    // Cache successful read_file results for this turn.
                    if name == "read_file" && !is_error {
                        let key = read_cache_key(&input);
                        read_cache.insert(key, result.clone());
                    }
                    on_event(AgentEvent::ToolDone {
                        name,
                        input,
                        result: result.clone(),
                        is_error,
                    });
                    tool_results[i] = ContentBlock::ToolResult {
                        tool_use_id: tool_calls[i].id.clone(),
                        content: compact_tool_result(&tool_calls[i].name, &result),
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

        // Heavyweight code generation: raise ceiling to 8k.
        // These requests often produce large diffs or full implementations.
        if ["refactor", "implement", "generate", "scaffold", "rewrite",
            "full implementation", "complete everything", "entire", "all files",
            "migrate", "overhaul"]
            .iter()
            .any(|k| text.contains(k))
            || user_input.len() > 2_000
        {
            return 8_192;
        }

        // Simple Q&A with no tools: cap at 2k (enough for a detailed explanation).
        if no_tools {
            return 2_048;
        }

        // Context pressure: if session is > 70% full, limit output to avoid overflow.
        let ctx_used = self.session.estimated_tokens();
        let ctx_window = self.provider.context_window();
        let ctx_pct = (ctx_used as f64 * 100.0) / ctx_window as f64;
        if ctx_pct >= 70.0 {
            // Leave room — don't use more than 20% of the window for output.
            let headroom = ((ctx_window as f64 * 0.20) as u32).max(1_024);
            return headroom.min(self.default_max_tokens);
        }

        self.default_max_tokens // 4096
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

/// Per-tool maximum inline result size (chars) kept in message history.
/// Smaller for tools whose output the model rarely needs verbatim later.
fn tool_result_limit(name: &str) -> usize {
    match name {
        // File writes/edits return short confirmation — no truncation needed.
        "write_file" | "edit_file" => 500,
        // grep/glob can be large but model only needs key lines.
        "grep" | "glob" | "list_dir" => 8_000,
        // Bash output: keep a generous window for build/test logs.
        "bash" | "run_command" => 10_000,
        // Web content can be very large; 6k keeps a full article section.
        "web_fetch" | "web_search" => 6_000,
        // read_file: keep more — model uses this for precise edits.
        "read_file" => 14_000,
        // Images stay intact (base64 must not be truncated).
        "read_image" => usize::MAX,
        _ => 12_000,
    }
}

/// Trim a fresh tool result before storing in message history.
/// Image data URIs are kept intact — truncating base64 corrupts the image.
fn compact_tool_result(name: &str, result: &str) -> String {
    if result.starts_with("data:image/") {
        return result.to_string();
    }
    let max = tool_result_limit(name);
    if result.len() <= max {
        return result.to_string();
    }
    let mut end = max;
    while end > 0 && !result.is_char_boundary(end) {
        end -= 1;
    }
    format!(
        "{}\n[… +{} chars truncated — use grep/read_file with line ranges to view more]",
        &result[..end],
        result.len().saturating_sub(end)
    )
}

/// Stable cache key for read_file: "path|start|end"
fn read_cache_key(input: &serde_json::Value) -> String {
    format!(
        "{}|{}|{}",
        input["path"].as_str().unwrap_or(""),
        input["start_line"].as_u64().unwrap_or(0),
        input["end_line"].as_u64().unwrap_or(0),
    )
}

fn should_enable_tools(input: &str) -> bool {
    let s = input.to_ascii_lowercase();

    // Absolute no-tools: single-word queries or pure social messages.
    if s.split_whitespace().count() <= 2 {
        return false;
    }

    // Always enable tools if the message is likely action-oriented.
    // These patterns strongly imply the user wants the agent to DO something.
    let action_hints = [
        // File operations
        "file", "read", "write", "edit", "create", "open", "save", "delete",
        // Code work
        "fix", "patch", "refactor", "implement", "add", "update", "remove", "change",
        "rename", "move", "copy", "import",
        // Build/test/run
        "test", "build", "run", "compile", "cargo", "npm", "yarn", "make", "lint",
        "format", "check", "coverage", "install",
        // Git
        "git", "commit", "branch", "merge", "diff", "push", "pull", "clone", "stash",
        // Search
        "search", "grep", "find", "locate", "look for", "where is",
        // Debug
        "bug", "error", "fail", "crash", "debug", "trace", "issue", "problem",
        // Web
        "fetch", "download", "url", "http", "browse", "web",
        // Images/files by extension
        ".rs", ".toml", ".json", ".py", ".ts", ".js", ".go", ".yaml", ".yml",
        ".png", ".jpg", ".jpeg", ".gif", ".webp", ".md", ".txt", ".csv",
        // Paths
        "src/", "crates/", "lib/", "bin/", "tests/", "docs/",
        // Show/display
        "show me", "list", "show", "display", "print", "tell me about",
        // Analysis
        "analyze", "review", "optimize", "improve", "performance", "check",
        "understand", "explain this", "what does this",
    ];

    if action_hints.iter().any(|k| s.contains(k)) {
        return true;
    }

    // Disable tools only for clearly abstract questions with no codebase context.
    let is_abstract_qa = [
        "what is rust", "what is python", "what is a", "what is an",
        "how does rust work", "how does async work", "tell me a joke",
        "what are the benefits of", "why use rust",
    ]
    .iter()
    .any(|k| s.starts_with(k));

    !is_abstract_qa
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
