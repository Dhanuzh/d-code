/// Interactive REPL using a custom raw-mode line editor.
use std::io::Write;
use std::path::PathBuf;

/// Human-readable relative time for a UTC RFC3339 timestamp.
fn time_ago(rfc3339: &str) -> String {
    let Ok(dt) = chrono::DateTime::parse_from_rfc3339(rfc3339) else {
        return rfc3339.to_string();
    };
    let now = chrono::Local::now();
    let secs = (now.signed_duration_since(dt)).num_seconds();
    match secs {
        s if s < 60 => "just now".into(),
        s if s < 3600 => format!("{}m ago", s / 60),
        s if s < 86400 => format!("{}h ago", s / 3600),
        s if s < 604800 => format!("{}d ago", s / 86400),
        s => format!("{}w ago", s / 604800),
    }
}

use dcode_agent::{Agent, AgentEvent};
use dcode_providers::load_provider_with_model;
use dcode_tui::summarize_input;
use dcode_tui::{AssistantMessage, Component, Spinner, ToolExecution, Tui};

use crate::{
    commands,
    input::{LineEditor, ReadOutcome},
    login, prompts, render, sessions,
};

// ─── Thinking spinner ─────────────────────────────────────────────────────────

// Old thread-based Spinner replaced by dcode_tui::Spinner component.

/// Slash-command tab-completion candidates.
fn slash_completions() -> Vec<String> {
    vec![
        "/help".into(),
        "/status".into(),
        "/model".into(),
        "/new".into(),
        "/sessions".into(),
        "/resume".into(),
        "/fork".into(),
        "/tree".into(),
        "/name".into(),
        "/copy".into(),
        "/share".into(),
        "/compact".into(),
        "/undo".into(),
        "/clear".into(),
        "/export".into(),
        "/init".into(),
        "/prompts".into(),
        "/skills".into(),
        "/login".into(),
        "/logout".into(),
        "/quit".into(),
    ]
}

pub async fn run(cwd: PathBuf, provider_name: Option<String>) -> anyhow::Result<()> {
    // Load saved model preference.
    // `needs_model_pick` = true when the user has never chosen a model — we'll
    // show the interactive picker right after the welcome banner.
    let (init_provider, init_model, needs_model_pick) = if provider_name.is_some() {
        (provider_name.as_deref(), None, false)
    } else {
        match load_saved_model() {
            Some(saved) => {
                if let Some((p, m)) = saved.split_once('/') {
                    let p = Box::leak(p.to_string().into_boxed_str());
                    let m = Box::leak(m.to_string().into_boxed_str());
                    (Some(p as &str), Some(m as &str), false)
                } else {
                    (provider_name.as_deref(), None, true)
                }
            }
            None => (provider_name.as_deref(), None, true), // first run — ask user
        }
    };

    // Clean up stale large-output tmp files in the background.
    tokio::spawn(async { dcode_tools::truncate::cleanup_old_tmp() });

    // Try loading the saved/preferred provider. If it fails (e.g. not logged in),
    // fall back to auto-detect from available credentials rather than hard-erroring.
    let provider = match load_provider_with_model(init_provider, init_model) {
        Ok(p) => p,
        Err(_) if init_provider.is_some() && provider_name.is_none() => {
            // Saved preference is stale — auto-detect instead.
            load_provider_with_model(None, None)?
        }
        Err(e) => return Err(e),
    };
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
    agent.bash_approver = Some(Box::new(render::confirm_dangerous_bash));

    // Wire up ask_user prompter.
    agent.user_prompter = Some(Box::new(|question, choices| {
        render::prompt_user_question(question, choices)
    }));

    // Welcome banner with provider status.
    let auth_store = dcode_providers::AuthStore::load().unwrap_or_default();
    render::print_welcome_banner(&provider_info, &auth_store);

    // ── First-run: no saved model → show picker immediately ──────────────────
    if needs_model_pick {
        render::print_info("  No model selected. Pick one to get started:");
        println!();
        print!("  \x1b[2mFetching models…\x1b[0m");
        let _ = std::io::stdout().flush();

        let labels = fetch_all_models(&agent, 8).await;
        print!("\r\x1b[2K");
        let _ = std::io::stdout().flush();

        if labels.is_empty() {
            render::print_info("  No providers authenticated. Run `/login` to add one.");
        } else {
            while crossterm::event::poll(std::time::Duration::ZERO).unwrap_or(false) {
                let _ = crossterm::event::read();
            }
            if let Some(idx) =
                render::select_interactive_with_current("Choose model:", &labels, None)
            {
                let label = &labels[idx];
                if let Some((p, m)) = label.split_once('/') {
                    let new_provider = load_provider_with_model(Some(p), Some(m))?;
                    provider_info = format!("{}/{}", new_provider.name(), new_provider.model());
                    agent.replace_provider(new_provider);
                    agent.refresh_system_prompt();
                    save_model(&provider_info, &dcode_dir);
                }
            }
            println!();
        }
    }

    let make_prompt = |info: &str, tokens: Option<u32>, cwd: &std::path::Path| -> String {
        // Compact prompt: model branch [tokens] ❯
        let branch = git_branch(cwd)
            .map(|b| format!(" \x1b[38;2;95;135;175m{b}\x1b[0m"))
            .unwrap_or_default();
        let tok_str = match tokens {
            Some(t) if t > 0 => {
                let display = if t >= 1_000_000 {
                    format!("{:.1}M", t as f64 / 1_000_000.0)
                } else if t >= 1_000 {
                    format!("{:.0}k", t as f64 / 1_000.0)
                } else {
                    format!("{t}")
                };
                format!(" \x1b[38;2;80;85;100m{display}\x1b[0m")
            }
            _ => String::new(),
        };
        format!("\x1b[38;2;138;190;183m{info}\x1b[0m{branch}{tok_str} \x1b[38;2;138;190;183m❯\x1b[0m ")
    };
    let mut editor = LineEditor::new(make_prompt(&provider_info, None, &cwd), slash_completions());

    let history_path = dcode_dir.join("history");
    load_history(&mut editor, &history_path);

    // Load prompt templates and skills once at startup.
    let templates = prompts::load_templates(&cwd);
    if !templates.is_empty() {
        render::print_info(&format!(
            "{} prompt template(s) loaded  (use /template-name to expand)",
            templates.len()
        ));
    }
    let skills = dcode_agent::skills::load_skills(&cwd);
    if !skills.is_empty() {
        render::print_info(&format!("{} skill(s) loaded", skills.len()));
    }

    // Track the current session id for live-saving and forking.
    let mut current_session_id: Option<String> = None;

    loop {
        // Update prompt with token usage.
        let total_tokens = agent.session.total_input_tokens + agent.session.total_output_tokens;
        editor.set_prompt(make_prompt(
            &provider_info,
            if total_tokens > 0 {
                Some(total_tokens)
            } else {
                None
            },
            &cwd,
        ));

        match editor.read_line()? {
            ReadOutcome::Exit => {
                // Auto-save session on exit.
                if !agent.session.messages.is_empty() {
                    sessions::save_with_opts(
                        &provider_info,
                        &agent.session.messages,
                        agent.session.turn_count(),
                        current_session_id.as_deref(),
                        None,
                        None,
                    );
                }
                render::print_info("Goodbye.");
                break;
            }
            ReadOutcome::Cancel => continue,
            ReadOutcome::CycleModel { forward } => {
                cycle_model(
                    &mut agent,
                    &mut editor,
                    &mut provider_info,
                    &dcode_dir,
                    &cwd,
                    forward,
                )?;
                continue;
            }
            ReadOutcome::CycleThinking => {
                if agent.provider_name() != "anthropic" {
                    render::print_info("  Thinking is only supported with Anthropic.");
                } else {
                    let new_level = agent.provider.thinking_level().cycle_next();
                    agent.provider.set_thinking_level(new_level);
                    editor.set_thinking_border(new_level.label());
                    render::print_info(&format!("  Thinking: {}", new_level.label()));
                }
                continue;
            }
            ReadOutcome::Submit(line) => {
                let input = line.trim().to_string();
                if input.is_empty() {
                    continue;
                }

                editor.push_history(&input);

                // ── Prompt template expansion ────────────────────────────────
                // Check if the input matches a prompt template before dispatching.
                // Templates take precedence over unknown slash commands but not builtins.
                if input.starts_with('/') {
                    if let Some(expanded) = prompts::expand(&input, &templates) {
                        // Template matched — treat expanded text as new input.
                        let expanded = expanded.trim().to_string();
                        if !expanded.is_empty() {
                            render::print_info(&format!(
                                "  Template expanded ({} chars)",
                                expanded.len()
                            ));
                            let expanded_input = expand_at_mentions(&expanded, &cwd);
                            println!();
                            run_turn_with_tui(&mut agent, &expanded_input).await;
                            render::print_turn_footer(
                                agent.session.total_input_tokens,
                                agent.session.total_output_tokens,
                                0,
                                0,
                                agent.model_name(),
                                agent.provider_context_window(),
                                agent.session.estimated_tokens() as u32,
                            );
                            // Live-save after template turn.
                            if !agent.session.messages.is_empty() {
                                if let Some(id) = sessions::save_with_opts(
                                    &provider_info,
                                    &agent.session.messages,
                                    agent.session.turn_count(),
                                    current_session_id.as_deref(),
                                    None,
                                    None,
                                ) {
                                    current_session_id = Some(id);
                                }
                            }
                        }
                        continue;
                    }
                }

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
                        dcode_agent::maybe_compact(&mut agent.session.messages, ctx_window, 4);
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
                            sessions::save_with_opts(
                                &provider_info,
                                &agent.session.messages,
                                agent.session.turn_count(),
                                current_session_id.as_deref(),
                                None,
                                None,
                            );
                            render::print_info("Session saved.");
                        }
                        agent.session = dcode_agent::Session::new();
                        current_session_id = None;
                        render::print_info("New session started.");
                        continue;
                    }
                    commands::CommandResult::Export { path } => {
                        export_session(&agent.session.messages, &provider_info, path.as_deref());
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
                                let branch_mark = if s.parent_id.is_some() { " ⎇" } else { "" };
                                if preview.is_empty() {
                                    format!(
                                        "{title}{branch_mark}  [{} turns · {ago} · {}]",
                                        s.turn_count, s.provider_model
                                    )
                                } else {
                                    format!(
                                        "{title}{branch_mark}  ↳ {preview}  [{} turns · {ago}]",
                                        s.turn_count
                                    )
                                }
                            })
                            .collect();
                        match render::select_interactive("Resume a session:", &labels) {
                            None => render::print_info("Cancelled."),
                            Some(idx) => {
                                let selected = &list[idx];
                                if !agent.session.messages.is_empty() {
                                    sessions::save_with_opts(
                                        &provider_info,
                                        &agent.session.messages,
                                        agent.session.turn_count(),
                                        current_session_id.as_deref(),
                                        None,
                                        None,
                                    );
                                }
                                agent.session.messages = selected.messages.clone();
                                current_session_id = Some(selected.id.clone());
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
                                    sessions::save_with_opts(
                                        &provider_info,
                                        &agent.session.messages,
                                        agent.session.turn_count(),
                                        current_session_id.as_deref(),
                                        None,
                                        None,
                                    );
                                }
                                agent.session.messages = selected.messages.clone();
                                current_session_id = Some(selected.id.clone());
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
                    commands::CommandResult::SetName { name } => {
                        if name.is_empty() {
                            render::print_info("Usage: /name <title>");
                        } else if let Some(id) = &current_session_id {
                            if sessions::set_display_name(id, &name) {
                                render::print_info(&format!("Session named: \"{name}\""));
                            } else {
                                // Session not yet saved — save first.
                                if !agent.session.messages.is_empty() {
                                    if let Some(new_id) = sessions::save_with_opts(
                                        &provider_info,
                                        &agent.session.messages,
                                        agent.session.turn_count(),
                                        None,
                                        Some(&name),
                                        None,
                                    ) {
                                        current_session_id = Some(new_id);
                                        render::print_info(&format!("Session named: \"{name}\""));
                                    }
                                } else {
                                    render::print_info("No messages to save yet.");
                                }
                            }
                        } else {
                            // No session id yet — save now with name.
                            if !agent.session.messages.is_empty() {
                                if let Some(id) = sessions::save_with_opts(
                                    &provider_info,
                                    &agent.session.messages,
                                    agent.session.turn_count(),
                                    None,
                                    Some(&name),
                                    None,
                                ) {
                                    current_session_id = Some(id);
                                    render::print_info(&format!("Session named: \"{name}\""));
                                }
                            } else {
                                render::print_info("No messages yet — start chatting first.");
                            }
                        }
                        continue;
                    }
                    commands::CommandResult::CopyLast => {
                        use dcode_providers::{ContentBlock, Role};
                        let last_text = agent
                            .session
                            .messages
                            .iter()
                            .rev()
                            .find(|m| m.role == Role::Assistant)
                            .and_then(|m| {
                                m.content
                                    .iter()
                                    .filter_map(|b| {
                                        if let ContentBlock::Text { text } = b {
                                            Some(text.as_str())
                                        } else {
                                            None
                                        }
                                    })
                                    .find(|t| !t.trim().is_empty() && !t.starts_with('['))
                            })
                            .map(|s| s.to_string());
                        if let Some(text) = last_text {
                            if copy_to_clipboard(&text) {
                                render::print_info(&format!(
                                    "Copied {} chars to clipboard.",
                                    text.len()
                                ));
                            } else {
                                render::print_warning("Clipboard not available. Last message:");
                                println!("{text}");
                            }
                        } else {
                            render::print_info("No assistant message to copy.");
                        }
                        continue;
                    }
                    commands::CommandResult::Fork { turn } => {
                        if agent.session.messages.is_empty() {
                            render::print_info("No conversation to fork.");
                        } else {
                            // Save current session first.
                            let parent_id = if let Some(id) = sessions::save_with_opts(
                                &provider_info,
                                &agent.session.messages,
                                agent.session.turn_count(),
                                current_session_id.as_deref(),
                                None,
                                None,
                            ) {
                                Some(id)
                            } else {
                                current_session_id.clone()
                            };

                            // Determine fork point.
                            let all_msgs = &agent.session.messages;
                            let fork_msgs = if let Some(t) = turn {
                                // Count turns (user messages).
                                let mut count = 0;
                                let mut end = 0;
                                for (i, msg) in all_msgs.iter().enumerate() {
                                    if msg.role == dcode_providers::Role::User {
                                        count += 1;
                                        if count >= t {
                                            end = i + 1;
                                            break;
                                        }
                                    }
                                }
                                if end == 0 {
                                    end = all_msgs.len();
                                }
                                all_msgs[..end].to_vec()
                            } else {
                                all_msgs.clone()
                            };

                            // Start new session with forked messages.
                            let new_id = sessions::save_with_opts(
                                &provider_info,
                                &fork_msgs,
                                fork_msgs
                                    .iter()
                                    .filter(|m| m.role == dcode_providers::Role::User)
                                    .count(),
                                None,
                                None,
                                parent_id.as_deref(),
                            );
                            agent.session.messages = fork_msgs;
                            current_session_id = new_id;
                            render::print_info(&format!(
                                "Forked: new session with {} turns (parent saved).",
                                agent.session.turn_count()
                            ));
                        }
                        continue;
                    }
                    commands::CommandResult::ShowTree => {
                        let all = sessions::list();
                        if all.is_empty() {
                            render::print_info("No saved sessions.");
                        } else {
                            render::print_session_tree(&all, current_session_id.as_deref());
                        }
                        continue;
                    }
                    commands::CommandResult::Share => {
                        if agent.session.messages.is_empty() {
                            render::print_info("No conversation to share.");
                        } else {
                            share_session_as_gist(&agent.session.messages, &provider_info).await;
                        }
                        continue;
                    }
                    commands::CommandResult::ListPrompts => {
                        if templates.is_empty() {
                            render::print_info(
                                "No prompt templates found. Place .md files in ~/.d-code/prompts/",
                            );
                        } else {
                            println!();
                            println!(
                                "  \x1b[1mPrompt templates\x1b[0m  (use /name [args] to expand)"
                            );
                            println!();
                            for t in &templates {
                                let desc = if t.description.is_empty() {
                                    "(no description)".to_string()
                                } else {
                                    t.description.clone()
                                };
                                println!("  \x1b[32m/{}\x1b[0m  —  {}", t.name, desc);
                            }
                            println!();
                        }
                        continue;
                    }
                    commands::CommandResult::ListSkills => {
                        if skills.is_empty() {
                            render::print_info(
                                "No skills found. Place SKILL.md files in ~/.d-code/skills/<name>/",
                            );
                        } else {
                            println!();
                            println!("  \x1b[1mSkills\x1b[0m  (auto-loaded into system prompt)");
                            println!();
                            for s in &skills {
                                println!("  \x1b[33m{}\x1b[0m  —  {}", s.name, s.description);
                                println!("    {}", s.file_path.display());
                            }
                            println!();
                        }
                        continue;
                    }
                    commands::CommandResult::ExpandPrompt { .. } => {
                        // Handled above before command dispatch.
                        continue;
                    }
                    commands::CommandResult::Login { provider } => {
                        let p = match provider.as_deref() {
                            Some(s) => s.to_string(),
                            None => {
                                // Show interactive picker.
                                let choices = vec![
                                    "anthropic".to_string(),
                                    "copilot".to_string(),
                                    "openai".to_string(),
                                    "gemini".to_string(),
                                    "openrouter".to_string(),
                                    "antigravity".to_string(),
                                ];
                                // Drain any buffered keypresses.
                                while crossterm::event::poll(std::time::Duration::ZERO)
                                    .unwrap_or(false)
                                {
                                    let _ = crossterm::event::read();
                                }
                                match render::select_interactive("Login to:", &choices) {
                                    Some(idx) => choices[idx].clone(),
                                    None => {
                                        continue;
                                    }
                                }
                            }
                        };
                        match p.as_str() {
                            "anthropic" | "claude" => {
                                let _ = login::login_anthropic().await;
                            }
                            "copilot" | "github" => {
                                let _ = login::login_copilot().await;
                            }
                            "openai" | "gpt" => {
                                let _ = login::login_openai().await;
                            }
                            "gemini" | "google" => {
                                let _ = login::login_gemini().await;
                            }
                            "openrouter" | "or" => {
                                let _ = login::login_openrouter().await;
                            }
                            "antigravity" | "ag" => {
                                let _ = login::login_antigravity().await;
                            }
                            other => render::print_error(&format!(
                                "Unknown provider '{other}'. Use: anthropic, copilot, openai, gemini, openrouter, antigravity"
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

                        let labels = fetch_all_models(&agent, 8).await;

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
                        match render::select_interactive_with_current(
                            "Switch model:",
                            &labels,
                            current_idx,
                        ) {
                            None => {}
                            Some(idx) => {
                                let label = &labels[idx];
                                if label != &current {
                                    if let Some((p, m)) = label.split_once('/') {
                                        let provider = load_provider_with_model(Some(p), Some(m))?;
                                        provider_info =
                                            format!("{}/{}", provider.name(), provider.model());
                                        agent.replace_provider(provider);
                                        agent.refresh_system_prompt();
                                        editor.set_prompt(make_prompt(&provider_info, None, &cwd));
                                        editor.set_thinking_border("off");
                                        save_model(&provider_info, &dcode_dir);
                                        let fresh_store =
                                            dcode_providers::AuthStore::load().unwrap_or_default();
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
                        editor.set_thinking_border("off");
                        save_model(&provider_info, &dcode_dir);
                        let fresh_store = dcode_providers::AuthStore::load().unwrap_or_default();
                        render::print_welcome_banner(&provider_info, &fresh_store);
                        continue;
                    }
                }

                // Expand @file mentions before sending to agent.
                let input = expand_at_mentions(&input, &cwd);

                run_turn_with_tui(&mut agent, &input).await;

                // Live-save after each turn.
                if !agent.session.messages.is_empty() {
                    if let Some(id) = sessions::save_with_opts(
                        &provider_info,
                        &agent.session.messages,
                        agent.session.turn_count(),
                        current_session_id.as_deref(),
                        None,
                        None,
                    ) {
                        current_session_id = Some(id);
                    }
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
                agent
                    .session
                    .push(Message::assistant("[Bash output noted]"));
            }
        }
        Err(e) => render::print_error(&format!("{e}")),
    }
}

// ─── TUI-driven turn ──────────────────────────────────────────────────────────

/// Run one agent turn using the dcode-tui differential renderer.
/// Layout mirrors pi-mono: UserMessage → spinner/tools → AssistantMessage → StatusBar footer.
///
/// Uses tokio::select! with a 16ms render ticker so the spinner animates smoothly
/// even while waiting for the model (no events flowing), and text streaming renders
/// at ~60fps without bulk-paste bursts.
async fn run_turn_with_tui(agent: &mut dcode_agent::Agent, input: &str) {
    use tokio::sync::mpsc::unbounded_channel;
    use tokio::time::{interval, Duration, MissedTickBehavior};

    let mut tui = Tui::new();
    let mut xml_filter = render::XmlFilter::new();
    let width = crossterm::terminal::size().map(|(w, _)| w).unwrap_or(80);

    let mut spinner: Option<Spinner> = Some(Spinner::new());
    let mut assistant: Option<AssistantMessage> = None;
    let mut completed_tools: Vec<ToolExecution> = Vec::new();
    let mut active_tools: Vec<ToolExecution> = Vec::new();
    let mut total_in = 0u32;
    let mut total_out = 0u32;
    let mut total_cache_write = 0u32;
    let mut total_cache_read = 0u32;

    macro_rules! render_state {
        () => {{
            let mut _lines: Vec<String> = Vec::new();
            // Spinner is NOT included here — it renders as a bottom-right overlay instead.
            for tool in completed_tools.iter_mut() {
                for mut l in tool.render(width) {
                    _lines.push(l.render().to_string());
                }
            }
            for tool in active_tools.iter_mut() {
                for mut l in tool.render(width) {
                    _lines.push(l.render().to_string());
                }
            }
            if let Some(msg) = assistant.as_mut() {
                for mut l in msg.render(width) {
                    _lines.push(l.render().to_string());
                }
            }
            _lines
        }};
    }

    // Initial render: show spinner immediately.
    tui.render_lines(render_state!());

    let model_name = agent.model_name().to_string();
    let ctx_window = agent.provider_context_window();

    // Decouple the sync event callback from the async render loop via a channel.
    // This lets us interleave event processing with a timer tick for smooth animation.
    let (tx, mut rx) = unbounded_channel::<AgentEvent>();

    let mut agent_fut = Box::pin(agent.run_turn(input, {
        let tx = tx.clone();
        move |ev| {
            let _ = tx.send(ev);
        }
    }));

    // 16ms tick (~60fps) drives spinner animation and flushes pending text frames.
    // biased select! ensures events always drain first so streaming isn't delayed.
    let mut ticker = interval(Duration::from_millis(16));
    ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);

    let mut agent_result: Option<anyhow::Result<()>> = None;

    loop {
        tokio::select! {
            biased;

            // ── Events (highest priority — drain before timer) ────────────────
            Some(ev) = rx.recv() => {
                match ev {
                    AgentEvent::ThinkingDelta(t) => {
                        if spinner.take().is_some() { clear_spinner_br(); }
                        assistant.get_or_insert_with(AssistantMessage::new).push_thinking(&t);
                        tui.render_lines_throttled(render_state!());
                    }
                    AgentEvent::TextDelta(t) => {
                        if spinner.take().is_some() { clear_spinner_br(); }
                        // First text after thinking — end the thinking block.
                        if let Some(ref mut asst) = assistant {
                            if asst.in_thinking {
                                asst.end_thinking();
                            }
                        }
                        let clean = xml_filter.push(&t);
                        if !clean.is_empty() {
                            assistant.get_or_insert_with(AssistantMessage::new).push(&clean);
                        }
                        // Throttled render — timer tick will flush any pending frames.
                        tui.render_lines_throttled(render_state!());
                    }
                    AgentEvent::ToolStart { name } => {
                        if spinner.take().is_some() { clear_spinner_br(); }
                        if let Some(ref mut asst) = assistant {
                            let leftover = xml_filter.flush();
                            if !leftover.is_empty() { asst.push(&leftover); }
                            asst.finalize();
                        }
                        active_tools.push(ToolExecution::new(&name));
                        tui.render_lines(render_state!());
                    }
                    AgentEvent::ToolDone { name, input: ti, result, is_error } => {
                        let summary = summarize_input(&name, &ti);
                        if let Some(idx) = active_tools.iter().position(|t| t.name == name) {
                            let mut tool = active_tools.remove(idx);
                            // Only preview output for tools where it's meaningful.
                            let preview = if matches!(name.as_str(),
                                "bash" | "run_command" | "grep" | "search" | "glob" | "list_files")
                            {
                                result
                            } else {
                                String::new()
                            };
                            tool.finish(&preview, is_error, summary);
                            completed_tools.push(tool);
                        }
                        assistant = None;
                        spinner = Some(Spinner::new());
                        tui.render_lines(render_state!());
                    }
                    AgentEvent::TokenUsage { input, output, cache_write, cache_read } => {
                        total_in += input;
                        total_out += output;
                        total_cache_write += cache_write;
                        total_cache_read += cache_read;
                    }
                    AgentEvent::UserQuestion { .. } | AgentEvent::ConfirmBash { .. } => {
                        tui.render_lines(render_state!());
                        tui.commit();
                    }
                    AgentEvent::DoomLoop { tool } => {
                        tui.commit();
                        render::print_error(&format!(
                            "Doom loop: '{tool}' called 3× with same args. Stopping."
                        ));
                    }
                    AgentEvent::TurnDone => {
                        if spinner.take().is_some() { clear_spinner_br(); }
                        let leftover = xml_filter.flush();
                        if !leftover.is_empty() {
                            assistant.get_or_insert_with(AssistantMessage::new).push(&leftover);
                        }
                        if let Some(ref mut asst) = assistant { asst.finalize(); }
                        tui.render_lines(render_state!());
                        tui.commit();
                        break;
                    }
                }
            }

            // ── Agent future completion ───────────────────────────────────────
            result = &mut agent_fut, if agent_result.is_none() => {
                agent_result = Some(result);
                if agent_result.as_ref().unwrap().is_err() {
                    // Error path: TurnDone won't arrive — drain remaining events then bail.
                    while let Ok(ev) = rx.try_recv() {
                        if let AgentEvent::TurnDone = ev { break; }
                    }
                    clear_spinner_br();
                    tui.commit();
                    break;
                }
                // Success path: TurnDone will arrive in the channel — continue draining.
            }

            // ── Render tick: animate spinner overlay + flush pending text ────
            _ = ticker.tick() => {
                tui.render_lines_throttled(render_state!());
                tui.flush_pending();
                // Animate the bottom-right spinner overlay.
                if let Some(ref sp) = spinner {
                    let (frame, elapsed) = sp.overlay_parts();
                    render_spinner_br(frame, &sp.label.clone(), &elapsed);
                }
            }
        }
    }

    // Drop the pinned future to release the mutable borrow on `agent`
    // before we access agent.session below.
    drop(agent_fut);

    if let Some(Err(e)) = agent_result {
        render::print_error(&friendly_error(&format!("{e:#}")));
        return;
    }

    // Footer: token counts, cost, context% — mirrors pi-mono footer.ts
    let ctx_used = agent.session.estimated_tokens() as u32;
    render::print_turn_footer(total_in, total_out, total_cache_write, total_cache_read, &model_name, ctx_window, ctx_used);
    println!();
    println!();
}

// ─── Model cycling ────────────────────────────────────────────────────────────

// ─── Bottom-right spinner overlay ─────────────────────────────────────────────

/// Write the spinner status at the bottom-right corner of the terminal.
/// Uses cursor save/restore so it doesn't disturb the main render position.
fn render_spinner_br(frame: &str, label: &str, elapsed: &str) {
    use std::io::Write;
    let (cols, rows) = crossterm::terminal::size().unwrap_or((80, 24));
    // Format: "⠋ thinking  1.2s" — braille is 3 bytes but 1 column wide.
    let text = format!("{frame} {label}  {elapsed}");
    let text_cols = label.len() + elapsed.len() + 5; // frame(1) + spaces + padding
    let col = cols.saturating_sub(text_cols as u16).max(1);
    let _ = write!(
        std::io::stdout(),
        // \x1b7 = save cursor, move to position, write dimmed text, \x1b8 = restore cursor
        "\x1b7\x1b[{rows};{col}H\x1b[38;2;102;102;102m{text}\x1b[0m\x1b8"
    );
    let _ = std::io::stdout().flush();
}

/// Erase the spinner from the bottom-right corner.
fn clear_spinner_br() {
    use std::io::Write;
    let (cols, rows) = crossterm::terminal::size().unwrap_or((80, 24));
    let clear_width: u16 = 28;
    let col = cols.saturating_sub(clear_width).max(1);
    let blanks = " ".repeat(clear_width as usize);
    let _ = write!(std::io::stdout(), "\x1b7\x1b[{rows};{col}H{blanks}\x1b8");
    let _ = std::io::stdout().flush();
}

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
            let branch = git_branch(cwd)
                .map(|b| format!(" \x1b[2m\x1b[38;5;179m{b}\x1b[0m"))
                .unwrap_or_default();
            editor.set_prompt(format!(" {}{} ▸ ", provider_info, branch));
            editor.set_thinking_border("off");
            save_model(provider_info, dcode_dir);
            render::print_info(&format!("Switched to {provider_info}"));
        }
        Err(e) => render::print_error(&format!("Cannot switch model: {e}")),
    }
    Ok(())
}

// ─── Session export ────────────────────────────────────────────────────────────

fn export_session(messages: &[dcode_providers::Message], provider_info: &str, path: Option<&str>) {
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
                        let short = if args.len() > 80 {
                            format!("{}…", &args[..80])
                        } else {
                            args
                        };
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

// ─── Clipboard ────────────────────────────────────────────────────────────────

/// Copy text to the system clipboard. Returns true if successful.
fn copy_to_clipboard(text: &str) -> bool {
    // Try xclip, xsel, wl-copy, pbcopy in order.
    let commands = [
        ("xclip", vec!["-selection", "clipboard"]),
        ("xsel", vec!["--clipboard", "--input"]),
        ("wl-copy", vec![]),
        ("pbcopy", vec![]),
    ];
    for (cmd, args) in &commands {
        let mut proc = match std::process::Command::new(cmd)
            .args(args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
        {
            Ok(p) => p,
            Err(_) => continue,
        };
        if let Some(stdin) = proc.stdin.as_mut() {
            use std::io::Write;
            let _ = stdin.write_all(text.as_bytes());
        }
        if proc.wait().map(|s| s.success()).unwrap_or(false) {
            return true;
        }
    }

    // OSC 52 terminal escape sequence (works in many modern terminals).
    use base64::Engine;
    let encoded = base64::engine::general_purpose::STANDARD.encode(text.as_bytes());
    print!("\x1b]52;c;{encoded}\x07");
    let _ = std::io::stdout().flush();
    true // optimistically assume OSC 52 works
}

// ─── Gist sharing ─────────────────────────────────────────────────────────────

/// Share session as an anonymous GitHub gist. Prints the URL on success.
async fn share_session_as_gist(messages: &[dcode_providers::Message], provider_info: &str) {
    use dcode_providers::{ContentBlock, Role};

    // Build markdown content.
    let mut md = format!("# d-code session — {provider_info}\n\n");
    for msg in messages {
        match msg.role {
            Role::User => {
                for block in &msg.content {
                    if let ContentBlock::Text { text } = block {
                        let t = text.trim();
                        if !t.is_empty() && !t.starts_with('[') {
                            md.push_str(&format!("**You:** {t}\n\n"));
                        }
                    }
                }
            }
            Role::Assistant => {
                for block in &msg.content {
                    if let ContentBlock::Text { text } = block {
                        let t = text.trim();
                        if !t.is_empty() && !t.starts_with('[') {
                            md.push_str(&format!("{t}\n\n"));
                        }
                    }
                }
            }
        }
    }

    let body = serde_json::json!({
        "description": format!("d-code session — {provider_info}"),
        "public": false,
        "files": {
            "session.md": { "content": md }
        }
    });

    let client = reqwest::Client::new();
    match client
        .post("https://api.github.com/gists")
        .header("User-Agent", "d-code/0.1")
        .header("Accept", "application/vnd.github+json")
        .json(&body)
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => {
            if let Ok(v) = resp.json::<serde_json::Value>().await {
                let url = v["html_url"].as_str().unwrap_or("(no URL)");
                render::print_success(&format!("Shared: {url}"));
                let _ = open_browser_share(url);
            }
        }
        Ok(resp) => {
            let status = resp.status();
            render::print_error(&format!("GitHub API error {status}"));
        }
        Err(e) => {
            render::print_error(&format!("Share failed: {e}"));
        }
    }
}

fn open_browser_share(url: &str) -> bool {
    #[cfg(target_os = "linux")]
    let _ = std::process::Command::new("xdg-open").arg(url).spawn();
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open").arg(url).spawn();
    #[cfg(target_os = "windows")]
    let _ = std::process::Command::new("cmd")
        .args(["/c", "start", url])
        .spawn();
    true
}

// ─── @mention expansion ───────────────────────────────────────────────────────

/// Expand `@path/to/file` tokens in user input by injecting file contents.
/// Returns the original string unchanged if no valid @mentions are found.
fn expand_at_mentions(input: &str, cwd: &std::path::Path) -> String {
    let mut injections: Vec<String> = Vec::new();
    let mut clean = input.to_string();

    for word in input.split_whitespace() {
        let Some(path_str) = word.strip_prefix('@') else {
            continue;
        };
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
    content.push_str(
        "<!-- Auto-generated by /init — edit freely to add project-specific guidance. -->\n\n",
    );

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
        let branch = branch_bytes
            .trim()
            .strip_prefix("ref: refs/heads/")
            .unwrap_or(branch_bytes.trim());
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
        let pm = if cwd.join("bun.lockb").exists() || cwd.join("bun.lock").exists() {
            "bun"
        } else if cwd.join("pnpm-lock.yaml").exists() {
            "pnpm"
        } else if cwd.join("yarn.lock").exists() {
            "yarn"
        } else {
            "npm"
        };
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
    const SKIP: &[&str] = &[
        "target",
        "node_modules",
        ".git",
        ".cache",
        "dist",
        "build",
        "__pycache__",
        ".next",
    ];
    let indent = "  ".repeat(depth);
    let mut out = String::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return out;
    };
    let mut entries: Vec<_> = entries.filter_map(|e| e.ok()).collect();
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if name_str.starts_with('.') && name_str != ".d-code" {
            continue;
        }
        if SKIP.contains(&name_str.as_ref()) {
            continue;
        }
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
        let Ok(content) = std::fs::read_to_string(&path) else {
            return Self::default();
        };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&content) else {
            return Self::default();
        };
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

/// Return `"provider/model"` labels for all authenticated providers using the
/// static model catalog. Instant — no network calls, no timeouts, no hangs.
async fn fetch_all_models(_agent: &Agent, _timeout_secs: u64) -> Vec<String> {
    use dcode_providers::model_catalog;

    let store = dcode_providers::AuthStore::load().unwrap_or_default();
    let catalog = model_catalog();

    let auth_flags: &[(&str, bool)] = &[
        ("anthropic", store.anthropic.is_some()),
        ("copilot", store.copilot.is_some()),
        (
            "openai",
            store.openai.is_some() || store.openai_oauth.is_some(),
        ),
        ("gemini", store.gemini.is_some()),
        ("openrouter", store.openrouter.is_some()),
        ("antigravity", store.antigravity.is_some()),
    ];

    let mut labels = Vec::new();
    for &(pname, is_auth) in auth_flags {
        if !is_auth {
            continue;
        }
        if let Some(cat) = catalog.iter().find(|c| c.provider == pname) {
            for m in cat.models {
                labels.push(format!("{pname}/{m}"));
            }
        }
    }
    labels
}

/// Strip raw JSON from provider API errors and return a clean one-liner.
fn friendly_error(raw: &str) -> String {
    // Try to extract "message" from OpenAI/Anthropic JSON error bodies.
    if let Some(start) = raw.find('{') {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&raw[start..]) {
            // OpenAI: {"error": {"message": "..."}}
            if let Some(msg) = v.pointer("/error/message").and_then(|m| m.as_str()) {
                // Prepend the non-JSON prefix (e.g. "chat_stream: OpenAI API error 429")
                let prefix = raw[..start].trim_end_matches([':', ' ']);
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
