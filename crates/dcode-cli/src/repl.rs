/// Interactive REPL using a custom raw-mode line editor.
use std::io::Write;
use std::path::PathBuf;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

/// Human-readable relative time for a UTC RFC3339 timestamp.
fn time_ago(rfc3339: &str) -> String {
    let Ok(dt) = chrono::DateTime::parse_from_rfc3339(rfc3339) else {
        return rfc3339.to_string();
    };
    let now = chrono::Local::now();
    let secs = (now.signed_duration_since(dt)).num_seconds();
    match secs {
        s if s < 60      => "just now".into(),
        s if s < 3600    => format!("{}m ago", s / 60),
        s if s < 86400   => format!("{}h ago", s / 3600),
        s if s < 604800  => format!("{}d ago", s / 86400),
        s                => format!("{}w ago", s / 604800),
    }
}

use dcode_agent::{Agent, AgentEvent};
use dcode_providers::load_provider_with_model;

use crate::{
    commands,
    input::{LineEditor, ReadOutcome},
    login, render, sessions,
};

// ─── Thinking spinner ─────────────────────────────────────────────────────────

struct Spinner {
    running: Arc<AtomicBool>,
    handle: tokio::task::JoinHandle<()>,
}

impl Spinner {
    fn start() -> Self {
        let running = Arc::new(AtomicBool::new(true));
        let flag = Arc::clone(&running);
        let handle = tokio::spawn(async move {
            // Braille frames for smooth spinner animation.
            let frames = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
            // Shimmer: cycle green brightness to create a pulse on the spinner char.
            let shimmer: &[(u8, u8, u8)] = &[
                (45, 95, 60),
                (52, 120, 72),
                (60, 148, 84),
                (68, 170, 96),
                (75, 190, 108),
                (80, 200, 118),  // peak
                (75, 188, 108),
                (68, 165, 95),
                (58, 138, 80),
                (50, 108, 66),
            ];
            let start = std::time::Instant::now();
            let mut i = 0usize;
            loop {
                if !flag.load(Ordering::Relaxed) {
                    break;
                }
                let secs = start.elapsed().as_secs_f32();
                let elapsed = if secs < 10.0 {
                    format!("{:.1}s", secs)
                } else {
                    format!("{:.0}s", secs)
                };
                let (sr, sg, sb) = shimmer[i % shimmer.len()];
                print!("\r  \x1b[38;2;{sr};{sg};{sb}m{}\x1b[0m \x1b[38;2;90;98;118mthinking\x1b[0m  \x1b[38;2;55;62;76m{elapsed}\x1b[0m",
                    frames[i % frames.len()]);
                let _ = std::io::stdout().flush();
                tokio::time::sleep(std::time::Duration::from_millis(80)).await;
                i += 1;
            }
        });
        Self { running, handle }
    }

    fn stop(self) {
        self.running.store(false, Ordering::Relaxed);
        self.handle.abort();
        print!("\r");
        let _ = crossterm::execute!(
            std::io::stdout(),
            crossterm::terminal::Clear(crossterm::terminal::ClearType::CurrentLine)
        );
        let _ = std::io::stdout().flush();
    }
}

/// Slash-command tab-completion candidates.
fn slash_completions() -> Vec<String> {
    vec![
        "/help".into(),
        "/status".into(),
        "/compact".into(),
        "/undo".into(),
        "/clear".into(),
        "/export".into(),
        "/model".into(),
        "/login".into(),
        "/logout".into(),
        "/sessions".into(),
        "/resume".into(),
        "/new".into(),
        "/quit".into(),
        "/init".into(),
    ]
}

pub async fn run(cwd: PathBuf, provider_name: Option<String>) -> anyhow::Result<()> {
    // Load saved model preference.
    let (init_provider, init_model) = if provider_name.is_some() {
        (provider_name.as_deref(), None)
    } else {
        match load_saved_model() {
            Some(saved) => {
                if let Some((p, m)) = saved.split_once('/') {
                    let p = Box::leak(p.to_string().into_boxed_str());
                    let m = Box::leak(m.to_string().into_boxed_str());
                    (Some(p as &str), Some(m as &str))
                } else {
                    (provider_name.as_deref(), None)
                }
            }
            None => (provider_name.as_deref(), None),
        }
    };

    // Clean up stale large-output tmp files in the background.
    tokio::spawn(async { dcode_tools::truncate::cleanup_old_tmp() });

    let provider = load_provider_with_model(init_provider, init_model)?;
    let mut provider_info = format!("{}/{}", provider.name(), provider.model());
    let mut agent = Agent::new(provider, cwd.clone());

    let dcode_dir = dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".d-code");

    // Load per-project + global config for permissions.
    let config = DcodeConfig::load(&dcode_dir);

    // Wire up bash confirmation (dangerous commands always need approval;
    // confirm_bash:true in config also requires approval for all bash commands).
    let _confirm_all = config.confirm_bash;
    agent.bash_approver = Some(Box::new(|cmd| {
        render::confirm_dangerous_bash(cmd)
    }));

    // Wire up ask_user prompter.
    agent.user_prompter = Some(Box::new(|question, choices| {
        render::prompt_user_question(question, choices)
    }));

    // Welcome banner with provider status.
    let auth_store = dcode_providers::AuthStore::load().unwrap_or_default();
    render::print_welcome_banner(&provider_info, &auth_store);

    let make_prompt = |info: &str, tokens: Option<u32>, cwd: &std::path::Path| -> String {
        let branch = git_branch(cwd)
            .map(|b| format!(" \x1b[2m\x1b[38;5;179m{b}\x1b[0m"))
            .unwrap_or_default();
        match tokens {
            Some(t) if t > 0 => {
                let display = if t >= 1_000_000 {
                    format!("{:.1}M", t as f64 / 1_000_000.0)
                } else if t >= 1_000 {
                    format!("{:.1}k", t as f64 / 1_000.0)
                } else {
                    format!("{}", t)
                };
                format!(" {}{} \x1b[2m[{}]\x1b[0m ▸ ", info, branch, display)
            }
            _ => format!(" {}{} ▸ ", info, branch),
        }
    };
    let mut editor = LineEditor::new(make_prompt(&provider_info, None, &cwd), slash_completions());

    let history_path = dcode_dir.join("history");
    load_history(&mut editor, &history_path);

    // Offer to resume last session.
    if let Some(last) = sessions::load_latest() {
        if !last.messages.is_empty() {
            let ago = time_ago(&last.updated_at);
            let title = last.display_title().to_string();
            render::print_info(&format!(
                "Resume last session? \"{title}\"  ({} turns · {ago})  [y/N]",
                last.turn_count
            ));
            if let Ok(ReadOutcome::Submit(ans)) = editor.read_line() {
                if ans.trim().eq_ignore_ascii_case("y") {
                    agent.session.messages = last.messages.clone();
                    render::print_session_recap(&last.messages, 4);
                    render::print_info(&format!("Resumed \"{title}\"  ({} turns)", last.turn_count));
                }
            }
        }
    }

    loop {
        // Update prompt with token usage.
        let total_tokens = agent.session.total_input_tokens + agent.session.total_output_tokens;
        editor.set_prompt(make_prompt(
            &provider_info,
            if total_tokens > 0 { Some(total_tokens) } else { None },
            &cwd,
        ));

        match editor.read_line()? {
            ReadOutcome::Exit => {
                // Auto-save session on exit.
                if !agent.session.messages.is_empty() {
                    sessions::save(
                        &provider_info,
                        &agent.session.messages,
                        agent.session.turn_count(),
                    );
                }
                render::print_info("Goodbye.");
                break;
            }
            ReadOutcome::Cancel => continue,
            ReadOutcome::CycleModel { forward } => {
                cycle_model(&mut agent, &mut editor, &mut provider_info, &dcode_dir, &cwd, forward)?;
                continue;
            }
            ReadOutcome::Submit(line) => {
                let input = line.trim().to_string();
                if input.is_empty() {
                    continue;
                }

                editor.push_history(&input);

                // ── Inline bash: ! = add to context, !! = silent ─────────────
                if let Some(cmd) = input.strip_prefix("!!") {
                    let cmd = cmd.trim();
                    if !cmd.is_empty() {
                        run_bash_inline(cmd, false, &mut agent).await;
                    }
                    continue;
                } else if let Some(cmd) = input.strip_prefix('!') {
                    let cmd = cmd.trim();
                    if !cmd.is_empty() {
                        run_bash_inline(cmd, true, &mut agent).await;
                    }
                    continue;
                }

                match commands::handle(&input, &agent) {
                    commands::CommandResult::NotACommand => {}
                    commands::CommandResult::Handled => continue,
                    commands::CommandResult::Undo => {
                        let msgs = &mut agent.session.messages;
                        // Pop the last full turn: assistant reply + user message.
                        // A turn is user-msg → (tool iterations) → assistant-msg.
                        // We pop messages from the end until we've removed one user message
                        // and at least one assistant message.
                        let before = msgs.len();
                        let mut removed_assistant = false;
                        while let Some(msg) = msgs.last() {
                            use dcode_providers::Role;
                            let is_user = msg.role == Role::User;
                            msgs.pop();
                            if is_user && removed_assistant {
                                break;
                            }
                            if !is_user {
                                removed_assistant = true;
                            }
                        }
                        let after = msgs.len();
                        if after < before {
                            render::print_info(&format!(
                                "Undone: removed {} messages. Conversation back to {} turns.",
                                before - after,
                                agent.session.turn_count()
                            ));
                        } else {
                            render::print_info("Nothing to undo.");
                        }
                        continue;
                    }
                    commands::CommandResult::Compact => {
                        let before = agent.session.estimated_tokens();
                        let ctx_window = agent.provider_context_window();
                        dcode_agent::maybe_compact(
                            &mut agent.session.messages,
                            ctx_window,
                            4,
                        );
                        let after = agent.session.estimated_tokens();
                        render::print_info(&format!(
                            "Compacted: ~{before} → ~{after} tokens in context."
                        ));
                        continue;
                    }
                    commands::CommandResult::Clear => {
                        agent.session = dcode_agent::Session::new();
                        render::print_info("Conversation cleared.");
                        continue;
                    }
                    commands::CommandResult::NewSession => {
                        if !agent.session.messages.is_empty() {
                            sessions::save(
                                &provider_info,
                                &agent.session.messages,
                                agent.session.turn_count(),
                            );
                            render::print_info("Session saved.");
                        }
                        agent.session = dcode_agent::Session::new();
                        render::print_info("New session started.");
                        continue;
                    }
                    commands::CommandResult::Export { path } => {
                        export_session(
                            &agent.session.messages,
                            &provider_info,
                            path.as_deref(),
                        );
                        continue;
                    }
                    commands::CommandResult::Init => {
                        init_project(&cwd);
                        continue;
                    }
                    commands::CommandResult::ShowSessions => {
                        let list = sessions::list();
                        if list.is_empty() {
                            render::print_info("No saved sessions.");
                            continue;
                        }
                        let labels: Vec<String> = list
                            .iter()
                            .map(|s| {
                                let title = s.display_title();
                                let preview = s.last_reply_preview();
                                let ago = time_ago(&s.updated_at);
                                if preview.is_empty() {
                                    format!("{title}  [{} turns · {ago} · {}]", s.turn_count, s.provider_model)
                                } else {
                                    format!("{title}  ↳ {preview}  [{} turns · {ago}]", s.turn_count)
                                }
                            })
                            .collect();
                        match render::select_interactive("Resume a session:", &labels) {
                            None => render::print_info("Cancelled."),
                            Some(idx) => {
                                let selected = &list[idx];
                                if !agent.session.messages.is_empty() {
                                    sessions::save(
                                        &provider_info,
                                        &agent.session.messages,
                                        agent.session.turn_count(),
                                    );
                                }
                                agent.session.messages = selected.messages.clone();
                                render::print_session_recap(&selected.messages, 4);
                                render::print_info(&format!(
                                    "Resumed: \"{}\" ({} turns)",
                                    selected.display_title(),
                                    selected.turn_count
                                ));
                            }
                        }
                        continue;
                    }
                    commands::CommandResult::ResumeLatest => {
                        match sessions::load_latest() {
                            None => {
                                render::print_info("No saved sessions.");
                            }
                            Some(selected) => {
                                if !agent.session.messages.is_empty() {
                                    sessions::save(
                                        &provider_info,
                                        &agent.session.messages,
                                        agent.session.turn_count(),
                                    );
                                }
                                agent.session.messages = selected.messages.clone();
                                render::print_session_recap(&selected.messages, 4);
                                render::print_info(&format!(
                                    "Resumed: \"{}\" ({} turns)",
                                    selected.display_title(), selected.turn_count
                                ));
                            }
                        }
                        continue;
                    }
                    commands::CommandResult::Login { provider } => {
                        let p = provider.as_deref().unwrap_or("anthropic");
                        match p {
                            "anthropic" | "claude" => {
                                let _ = login::login_anthropic().await;
                            }
                            "copilot" | "github" => {
                                let _ = login::login_copilot().await;
                            }
                            "openai" | "gpt" => {
                                let _ = login::login_openai().await;
                            }
                            other => render::print_error(&format!(
                                "Unknown provider '{other}'. Use: anthropic, copilot, openai"
                            )),
                        }
                        continue;
                    }
                    commands::CommandResult::Logout { provider } => {
                        let p = provider.as_deref().unwrap_or_else(|| {
                            Box::leak(
                                provider_info
                                    .split('/')
                                    .next()
                                    .unwrap_or("")
                                    .to_string()
                                    .into_boxed_str(),
                            )
                        });
                        match login::logout(p) {
                            Ok(()) => render::print_info(&format!("Logged out from {p}.")),
                            Err(e) => render::print_error(&format!("{e}")),
                        }
                        continue;
                    }
                    commands::CommandResult::ShowModelPicker { .. } => {
                        // Show a loading indicator while fetching model lists.
                        print!("  \x1b[2mFetching models…\x1b[0m");
                        let _ = std::io::stdout().flush();

                        let store = dcode_providers::AuthStore::load().unwrap_or_default();
                        let mut labels: Vec<String> = Vec::new();
                        for m in agent.provider.list_models().await {
                            labels.push(format!("{}/{}", agent.provider.name(), m));
                        }
                        let other_providers: Vec<&str> = ["anthropic", "copilot", "openai"]
                            .iter()
                            .filter(|&&p| p != agent.provider.name())
                            .filter(|&&p| match p {
                                "anthropic" => store.anthropic.is_some(),
                                "copilot"   => store.copilot.is_some(),
                                "openai"    => store.openai.is_some() || store.openai_oauth.is_some(),
                                _ => false,
                            })
                            .copied()
                            .collect();
                        for p in other_providers {
                            if let Ok(tmp) = load_provider_with_model(Some(p), None) {
                                for m in tmp.list_models().await {
                                    labels.push(format!("{p}/{m}"));
                                }
                            }
                        }
                        // Clear the loading line.
                        print!("\r\x1b[2K");
                        let _ = std::io::stdout().flush();

                        // Drain any buffered keypresses that accumulated during the fetch
                        // (e.g. the Enter that submitted /model) to avoid phantom selections.
                        while crossterm::event::poll(std::time::Duration::ZERO).unwrap_or(false) {
                            let _ = crossterm::event::read();
                        }

                        let current = agent.provider_info();
                        let current_idx = labels.iter().position(|l| l == &current);
                        match render::select_interactive_with_current("Switch model:", &labels, current_idx)
                        {
                            None => {}
                            Some(idx) => {
                                let label = &labels[idx];
                                if label != &current {
                                    if let Some((p, m)) = label.split_once('/') {
                                        let provider = load_provider_with_model(Some(p), Some(m))?;
                                        provider_info = format!("{}/{}", provider.name(), provider.model());
                                        agent.replace_provider(provider);
                                        editor.set_prompt(make_prompt(&provider_info, None, &cwd));
                                        save_model(&provider_info, &dcode_dir);
                                        let fresh_store = dcode_providers::AuthStore::load().unwrap_or_default();
                                        render::print_welcome_banner(&provider_info, &fresh_store);
                                    }
                                }
                            }
                        }
                        continue;
                    }
                    commands::CommandResult::SwitchModel { provider, model } => {
                        let provider = load_provider_with_model(Some(provider), model.as_deref())?;
                        provider_info = format!("{}/{}", provider.name(), provider.model());
                        agent.replace_provider(provider);
                        editor.set_prompt(make_prompt(&provider_info, None, &cwd));
                        save_model(&provider_info, &dcode_dir);
                        let fresh_store = dcode_providers::AuthStore::load().unwrap_or_default();
                        render::print_welcome_banner(&provider_info, &fresh_store);
                        continue;
                    }
                }

                // Expand @file mentions before sending to agent.
                let input = expand_at_mentions(&input, &cwd);

                println!();
                let tokens_before = agent.session.total_input_tokens + agent.session.total_output_tokens;
                let mut md = render::MarkdownRenderer::new();
                let mut xml_filter = render::XmlFilter::new();
                let spinner = Spinner::start();
                let mut spinner_opt = Some(spinner);
                let mut tool_start: std::collections::HashMap<String, std::time::Instant> =
                    std::collections::HashMap::new();

                let mut turn_divider_shown = false;
                if let Err(e) = agent
                    .run_turn(&input, |ev| match ev {
                        AgentEvent::TextDelta(t) => {
                            if let Some(sp) = spinner_opt.take() {
                                sp.stop();
                            }
                            if !turn_divider_shown {
                                turn_divider_shown = true;
                                render::print_turn_divider();
                            }
                            let clean = xml_filter.push(&t);
                            if !clean.is_empty() {
                                md.push(&clean);
                            }
                        }
                        AgentEvent::ToolStart { name } => {
                            if let Some(sp) = spinner_opt.take() {
                                sp.stop();
                            }
                            let leftover = xml_filter.flush();
                            if !leftover.is_empty() {
                                md.push(&leftover);
                            }
                            md.flush();
                            tool_start.insert(name.clone(), std::time::Instant::now());
                            render::print_tool_start(&name);
                        }
                        AgentEvent::ToolDone {
                            name,
                            input,
                            result,
                            is_error,
                        } => {
                            let elapsed_ms = tool_start.remove(&name)
                                .map(|t| t.elapsed().as_millis() as u64)
                                .unwrap_or(0);
                            render::print_tool_done(&name, &input, &result, is_error, elapsed_ms);
                            // Show spinner again while waiting for next chunk.
                            spinner_opt = Some(Spinner::start());
                        }
                        AgentEvent::TokenUsage { .. } => {}
                        AgentEvent::UserQuestion { .. } => {
                            // Handled synchronously by user_prompter before this event fires.
                            // Event is informational only.
                            if let Some(sp) = spinner_opt.take() { sp.stop(); }
                        }
                        AgentEvent::ConfirmBash { .. } => {
                            // bash_approver runs synchronously; this event is informational.
                            if let Some(sp) = spinner_opt.take() { sp.stop(); }
                        }
                        AgentEvent::DoomLoop { tool } => {
                            if let Some(sp) = spinner_opt.take() {
                                sp.stop();
                            }
                            render::print_error(&format!(
                                "Doom loop: '{tool}' called 3× with same args. Stopping."
                            ));
                        }
                        AgentEvent::TurnDone => {
                            if let Some(sp) = spinner_opt.take() {
                                sp.stop();
                            }
                            let leftover = xml_filter.flush();
                            if !leftover.is_empty() {
                                md.push(&leftover);
                            }
                            md.flush();
                            println!();
                        }
                    })
                    .await
                {
                    if let Some(sp) = spinner_opt.take() {
                        sp.stop();
                    }
                    render::print_error(&friendly_error(&format!("{e:#}")));
                }

                // Show per-turn cost hint.
                let tokens_after = agent.session.total_input_tokens + agent.session.total_output_tokens;
                let delta = tokens_after.saturating_sub(tokens_before);
                if delta > 0 {
                    print_turn_cost(delta, agent.model_name());
                }

                // Context usage warning at 70% and 90%.
                let ctx_used = agent.session.estimated_tokens();
                let ctx_window = agent.provider_context_window() as usize;
                let ctx_pct = (ctx_used as f64 * 100.0) / ctx_window as f64;
                if ctx_pct >= 90.0 {
                    render::print_warning("Context 90%+ full — run /compact now or start /new session.");
                } else if ctx_pct >= 70.0 {
                    render::print_warning(&format!("Context at {ctx_pct:.0}% — consider /compact soon."));
                }
            }
        }
    }

    save_history(&editor, &history_path);
    Ok(())
}

// ─── Model persistence ────────────────────────────────────────────────────────

fn model_config_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".d-code")
        .join("config.json")
}

fn load_saved_model() -> Option<String> {
    let content = std::fs::read_to_string(model_config_path()).ok()?;
    let v: serde_json::Value = serde_json::from_str(&content).ok()?;
    v["model"].as_str().map(String::from)
}

fn save_model(provider_model: &str, dcode_dir: &PathBuf) {
    let _ = std::fs::create_dir_all(dcode_dir);
    let data = serde_json::json!({ "model": provider_model });
    let _ = std::fs::write(dcode_dir.join("config.json"), data.to_string());
}

// ─── Inline bash ──────────────────────────────────────────────────────────────

/// Run a shell command inline. If `add_to_context` is true, injects the output
/// as a user/assistant message pair so future turns can reference it.
async fn run_bash_inline(cmd: &str, add_to_context: bool, agent: &mut dcode_agent::Agent) {
    use dcode_providers::Message;

    render::print_info(&format!("$ {cmd}"));
    let result = dcode_tools::bash::bash_exec(dcode_tools::bash::BashArgs {
        command: cmd.to_string(),
        timeout_secs: Some(30),
        working_dir: None,
    })
    .await;

    match result {
        Ok(output) => {
            println!("{output}");
            if add_to_context && !output.trim().is_empty() {
                let ctx = format!("[User ran: {cmd}]\n{}", &output[..output.len().min(4_000)]);
                agent.session.push(Message::user(ctx));
                agent.session.push(Message::assistant("[Bash output noted]"));
            }
        }
        Err(e) => render::print_error(&format!("{e}")),
    }
}

// ─── Model cycling ────────────────────────────────────────────────────────────

fn cycle_model(
    agent: &mut dcode_agent::Agent,
    editor: &mut LineEditor,
    provider_info: &mut String,
    dcode_dir: &PathBuf,
    cwd: &std::path::Path,
    forward: bool,
) -> anyhow::Result<()> {
    use dcode_providers::{load_provider_with_model, model_catalog};

    let catalog = model_catalog();
    let all_models: Vec<(&str, &str)> = catalog
        .iter()
        .flat_map(|c| c.models.iter().map(move |m| (c.provider, *m)))
        .collect();

    if all_models.is_empty() {
        return Ok(());
    }

    let current = agent.provider_info();
    let pos = all_models
        .iter()
        .position(|(p, m)| format!("{p}/{m}") == current)
        .unwrap_or(0);

    let next_pos = if forward {
        (pos + 1) % all_models.len()
    } else {
        (pos + all_models.len().saturating_sub(1)) % all_models.len()
    };

    let (next_p, next_m) = all_models[next_pos];
    match load_provider_with_model(Some(next_p), Some(next_m)) {
        Ok(provider) => {
            *provider_info = format!("{}/{}", provider.name(), provider.model());
            agent.replace_provider(provider);
            let branch = git_branch(cwd).map(|b| format!(" \x1b[2m\x1b[38;5;179m{b}\x1b[0m")).unwrap_or_default();
            editor.set_prompt(format!(" {}{} ▸ ", provider_info, branch));
            save_model(provider_info, dcode_dir);
            render::print_info(&format!("Switched to {provider_info}"));
        }
        Err(e) => render::print_error(&format!("Cannot switch model: {e}")),
    }
    Ok(())
}

// ─── Session export ────────────────────────────────────────────────────────────

fn export_session(
    messages: &[dcode_providers::Message],
    provider_info: &str,
    path: Option<&str>,
) {
    use dcode_providers::{ContentBlock, Role};

    let out_path = path.unwrap_or("session.md");
    let mut out = String::new();
    out.push_str(&format!("# d-code session  —  {provider_info}\n\n"));

    for msg in messages {
        match msg.role {
            Role::User => {
                for block in &msg.content {
                    if let ContentBlock::Text { text } = block {
                        let t = text.trim();
                        if !t.is_empty() && !t.starts_with('[') {
                            out.push_str(&format!("**You:** {t}\n\n"));
                        }
                    }
                }
            }
            Role::Assistant => {
                for block in &msg.content {
                    if let ContentBlock::Text { text } = block {
                        let t = text.trim();
                        if !t.is_empty() && !t.starts_with('[') {
                            out.push_str(&format!("{t}\n\n"));
                        }
                    }
                    if let ContentBlock::ToolUse { name, input, .. } = block {
                        let args = serde_json::to_string(input).unwrap_or_default();
                        let short = if args.len() > 80 { format!("{}…", &args[..80]) } else { args };
                        out.push_str(&format!("*Tool: `{name}({short})`*\n\n"));
                    }
                }
            }
        }
    }

    match std::fs::write(out_path, &out) {
        Ok(()) => render::print_info(&format!("Exported to {out_path}  ({} bytes)", out.len())),
        Err(e) => render::print_error(&format!("Export failed: {e}")),
    }
}

// ─── Cost display ─────────────────────────────────────────────────────────────

fn print_turn_cost(delta_tokens: u32, model: &str) {
    // Very rough rates in $/M tokens; display only as orientation.
    let (in_rate, out_rate) = if model.contains("opus") {
        (15.0_f64, 75.0)
    } else if model.contains("sonnet") {
        (3.0, 15.0)
    } else if model.contains("haiku") {
        (0.8, 4.0)
    } else if model.contains("gpt-4") {
        (2.0, 8.0)
    } else {
        (1.0, 4.0)
    };
    // Assume ~70/30 input/output split as a rough heuristic.
    let est = ((delta_tokens as f64 * 0.7) / 1_000_000.0) * in_rate
        + ((delta_tokens as f64 * 0.3) / 1_000_000.0) * out_rate;
    if est >= 0.0001 {
        let tok_str = if delta_tokens >= 1000 {
            format!("{:.1}k", delta_tokens as f64 / 1000.0)
        } else {
            delta_tokens.to_string()
        };
        println!(
            "  \x1b[38;2;50;56;70m\x1b[2m{tok_str} tokens  ~${est:.4}\x1b[0m"
        );
    }
}

// ─── @mention expansion ───────────────────────────────────────────────────────

/// Expand `@path/to/file` tokens in user input by injecting file contents.
/// Returns the original string unchanged if no valid @mentions are found.
fn expand_at_mentions(input: &str, cwd: &std::path::Path) -> String {
    let mut injections: Vec<String> = Vec::new();
    let mut clean = input.to_string();

    for word in input.split_whitespace() {
        let Some(path_str) = word.strip_prefix('@') else { continue };
        // Only treat as file path if it has a path separator or extension.
        if path_str.is_empty() || (!path_str.contains('/') && !path_str.contains('.')) {
            continue;
        }
        let full_path = if std::path::Path::new(path_str).is_absolute() {
            std::path::PathBuf::from(path_str)
        } else {
            cwd.join(path_str)
        };
        if let Ok(content) = std::fs::read_to_string(&full_path) {
            let lines: Vec<&str> = content.lines().collect();
            let shown = if lines.len() > 200 {
                format!(
                    "{}\n[… {} more lines — use read_file with line ranges for the rest]",
                    lines[..200].join("\n"),
                    lines.len() - 200
                )
            } else {
                content.trim_end().to_string()
            };
            let ext = full_path.extension().and_then(|e| e.to_str()).unwrap_or("");
            injections.push(format!("Contents of `{path_str}`:\n```{ext}\n{shown}\n```"));
            // Replace @path token with backtick reference in the message.
            clean = clean.replace(word, &format!("`{path_str}`"));
            render::print_info(&format!("@{path_str}  injected ({} lines)", lines.len()));
        }
    }

    if injections.is_empty() {
        return input.to_string();
    }

    format!("{}\n\n{}", injections.join("\n\n"), clean)
}

// ─── /init ────────────────────────────────────────────────────────────────────

/// Scan the current project and write a DCODE.md context file.
fn init_project(cwd: &std::path::Path) {
    let dcode_md = cwd.join("DCODE.md");

    if dcode_md.exists() {
        render::print_info("DCODE.md already exists. Overwrite? [y/N]");
        let mut ans = String::new();
        let _ = std::io::stdin().read_line(&mut ans);
        if !ans.trim().eq_ignore_ascii_case("y") {
            render::print_info("Cancelled.");
            return;
        }
    }

    let mut content = String::from("# Project Context for d-code\n\n");
    content.push_str("<!-- Auto-generated by /init — edit freely to add project-specific guidance. -->\n\n");

    // Stack detection.
    let stack = init_detect_stack(cwd);
    if !stack.is_empty() {
        content.push_str("## Stack\n");
        for s in &stack {
            content.push_str(&format!("- {s}\n"));
        }
        content.push('\n');
    }

    // Directory structure (2 levels, skip noise).
    content.push_str("## Structure\n```\n");
    content.push_str(&init_list_structure(cwd, 0, 2));
    content.push_str("```\n\n");

    // README excerpt.
    if let Some(readme) = init_read_readme(cwd) {
        content.push_str("## README\n");
        content.push_str(&readme);
        content.push_str("\n\n");
    }

    // Git info.
    if let Ok(branch_bytes) = std::fs::read_to_string(cwd.join(".git").join("HEAD")) {
        let branch = branch_bytes.trim().strip_prefix("ref: refs/heads/").unwrap_or(branch_bytes.trim());
        content.push_str(&format!("## Git\nDefault branch: `{branch}`\n\n"));
    }

    content.push_str("## Notes\n");
    content.push_str("<!-- Add project-specific instructions here, e.g.:\n");
    content.push_str("- Always run tests after changes: `cargo test`\n");
    content.push_str("- Use snake_case for function names\n");
    content.push_str("-->\n");

    match std::fs::write(&dcode_md, &content) {
        Ok(()) => render::print_success(&format!(
            "Created DCODE.md  ({} bytes) — d-code will auto-load it as project context.",
            content.len()
        )),
        Err(e) => render::print_error(&format!("Failed to write DCODE.md: {e}")),
    }
}

fn init_detect_stack(cwd: &std::path::Path) -> Vec<String> {
    let mut stacks = Vec::new();
    if cwd.join("Cargo.toml").exists() {
        stacks.push("Rust — `cargo build` · `cargo test` · `cargo clippy`".to_string());
    }
    if cwd.join("package.json").exists() {
        let pm = if cwd.join("bun.lockb").exists() || cwd.join("bun.lock").exists() { "bun" }
            else if cwd.join("pnpm-lock.yaml").exists() { "pnpm" }
            else if cwd.join("yarn.lock").exists() { "yarn" }
            else { "npm" };
        stacks.push(format!("Node.js / {pm} — `{pm} run build` · `{pm} test`"));
    }
    if cwd.join("pyproject.toml").exists() || cwd.join("requirements.txt").exists() {
        stacks.push("Python — `pytest` · `pip install -e .`".to_string());
    }
    if cwd.join("go.mod").exists() {
        stacks.push("Go — `go build ./...` · `go test ./...`".to_string());
    }
    if cwd.join("Dockerfile").exists() {
        stacks.push("Docker — `docker build .`".to_string());
    }
    if cwd.join("Makefile").exists() || cwd.join("makefile").exists() {
        stacks.push("Make — `make` · `make test`".to_string());
    }
    stacks
}

fn init_list_structure(dir: &std::path::Path, depth: usize, max_depth: usize) -> String {
    const SKIP: &[&str] = &["target", "node_modules", ".git", ".cache", "dist", "build", "__pycache__", ".next"];
    let indent = "  ".repeat(depth);
    let mut out = String::new();
    let Ok(entries) = std::fs::read_dir(dir) else { return out };
    let mut entries: Vec<_> = entries.filter_map(|e| e.ok()).collect();
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if name_str.starts_with('.') && name_str != ".d-code" { continue; }
        if SKIP.contains(&name_str.as_ref()) { continue; }
        let path = entry.path();
        if path.is_dir() {
            out.push_str(&format!("{indent}{name_str}/\n"));
            if depth < max_depth {
                out.push_str(&init_list_structure(&path, depth + 1, max_depth));
            }
        } else {
            out.push_str(&format!("{indent}{name_str}\n"));
        }
    }
    out
}

fn init_read_readme(cwd: &std::path::Path) -> Option<String> {
    for name in &["README.md", "README.txt", "README"] {
        let path = cwd.join(name);
        if let Ok(content) = std::fs::read_to_string(&path) {
            let lines: Vec<&str> = content.lines().take(30).collect();
            let text = lines.join("\n").trim().to_string();
            if !text.is_empty() {
                return Some(if content.lines().count() > 30 {
                    format!("{text}\n[… truncated]")
                } else {
                    text
                });
            }
        }
    }
    None
}

// ─── Git branch ──────────────────────────────────────────────────────────────

/// Read the current git branch by walking up from `cwd` to find `.git/HEAD`.
/// Returns None if not in a git repo.
fn git_branch(cwd: &std::path::Path) -> Option<String> {
    let mut dir = Some(cwd);
    while let Some(d) = dir {
        let head = d.join(".git").join("HEAD");
        if let Ok(content) = std::fs::read_to_string(&head) {
            let content = content.trim();
            if let Some(branch) = content.strip_prefix("ref: refs/heads/") {
                return Some(branch.to_string());
            }
            // Detached HEAD — show short hash.
            if content.len() >= 7 {
                return Some(format!("@{}", &content[..7]));
            }
        }
        dir = d.parent();
    }
    None
}

// ─── Config ───────────────────────────────────────────────────────────────────

/// Per-user d-code configuration loaded from ~/.d-code/config.json.
#[derive(Default)]
struct DcodeConfig {
    /// Confirm ALL bash commands (not just dangerous ones).
    #[allow(dead_code)]
    pub confirm_bash: bool,
}

impl DcodeConfig {
    fn load(dcode_dir: &std::path::Path) -> Self {
        let path = dcode_dir.join("config.json");
        let Ok(content) = std::fs::read_to_string(&path) else { return Self::default() };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&content) else { return Self::default() };
        Self {
            confirm_bash: v["confirm_bash"].as_bool().unwrap_or(false),
        }
    }
}

// ─── History persistence ──────────────────────────────────────────────────────

fn load_history(editor: &mut LineEditor, path: &std::path::Path) {
    if let Ok(content) = std::fs::read_to_string(path) {
        for line in content.lines() {
            if !line.is_empty() {
                editor.push_history(line);
            }
        }
    }
}

fn save_history(editor: &LineEditor, path: &std::path::Path) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(path, editor.history.join("\n"));
}

/// Strip raw JSON from provider API errors and return a clean one-liner.
fn friendly_error(raw: &str) -> String {
    // Try to extract "message" from OpenAI/Anthropic JSON error bodies.
    if let Some(start) = raw.find('{') {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&raw[start..]) {
            // OpenAI: {"error": {"message": "..."}}
            if let Some(msg) = v.pointer("/error/message").and_then(|m| m.as_str()) {
                // Prepend the non-JSON prefix (e.g. "chat_stream: OpenAI API error 429")
                let prefix = raw[..start].trim_end_matches(|c: char| c == ':' || c == ' ');
                return if prefix.is_empty() {
                    msg.to_string()
                } else {
                    format!("{prefix}: {msg}")
                };
            }
            // Anthropic: {"type":"error","error":{"type":"...","message":"..."}}
            if let Some(msg) = v.pointer("/error/message").and_then(|m| m.as_str()) {
                return msg.to_string();
            }
        }
    }
    raw.to_string()
}
