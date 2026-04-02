/// Interactive REPL using rustyline.
use std::path::PathBuf;

use rustyline::error::ReadlineError;
use rustyline::{Config, DefaultEditor};

use dcode_agent::{Agent, AgentEvent};
use dcode_providers::load_provider_with_model;

use crate::{commands, render};

pub async fn run(cwd: PathBuf, provider_name: Option<String>) -> anyhow::Result<()> {
    let provider = load_provider_with_model(provider_name.as_deref(), None)?;
    let mut provider_info = format!("{}/{}", provider.name(), provider.model());
    let mut agent = Agent::new(provider, cwd);

    render::print_info(&format!(
        "d-code  •  {}  •  /help for commands",
        provider_info
    ));
    println!();

    let rl_config = Config::builder()
        .history_ignore_space(true)
        .max_history_size(500)
        .unwrap()
        .build();
    let mut rl = DefaultEditor::with_config(rl_config)?;

    // Load history.
    let history_path = dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".d-code")
        .join("history");
    let _ = rl.load_history(&history_path);

    loop {
        let prompt = format!("\x1b[90m[{}]\x1b[0m \x1b[32m❯\x1b[0m ", provider_info);
        match rl.readline(&prompt) {
            Ok(line) => {
                let input = line.trim().to_string();
                if input.is_empty() {
                    continue;
                }
                let _ = rl.add_history_entry(&input);

                match commands::handle(&input, &agent) {
                    commands::CommandResult::NotACommand => {}
                    commands::CommandResult::Handled => continue,
                    commands::CommandResult::Clear => {
                        agent.session = dcode_agent::Session::new();
                        render::print_info("Conversation cleared.");
                        continue;
                    }
                    commands::CommandResult::ShowModelPicker { options } => {
                        println!("Current: {}", agent.provider_info());
                        println!("\nSelect a model:");
                        for (idx, opt) in options.iter().enumerate() {
                            println!("  {:>2}. {}/{}", idx + 1, opt.provider, opt.model);
                        }
                        println!("\nEnter number (or empty to cancel):");

                        let pick = match rl.readline("model> ") {
                            Ok(v) => v.trim().to_string(),
                            Err(_) => String::new(),
                        };
                        if pick.is_empty() {
                            render::print_info("Model switch cancelled.");
                            continue;
                        }
                        let idx = match pick.parse::<usize>() {
                            Ok(v) => v,
                            Err(_) => {
                                render::print_error("Invalid selection. Enter a number from the list.");
                                continue;
                            }
                        };
                        if idx == 0 || idx > options.len() {
                            render::print_error("Selection out of range.");
                            continue;
                        }
                        let selected = &options[idx - 1];
                        let provider = load_provider_with_model(
                            Some(selected.provider),
                            Some(selected.model),
                        )?;
                        provider_info = format!("{}/{}", provider.name(), provider.model());
                        agent.replace_provider(provider);
                        render::print_info(&format!("Switched to {provider_info}"));
                        agent.session = dcode_agent::Session::new();
                        render::print_info("Conversation cleared.");
                        continue;
                    }
                    commands::CommandResult::SwitchModel { provider, model } => {
                        let provider = load_provider_with_model(Some(provider), model.as_deref())?;
                        provider_info = format!("{}/{}", provider.name(), provider.model());
                        agent.replace_provider(provider);
                        render::print_info(&format!("Switched to {provider_info}"));
                        agent.session = dcode_agent::Session::new();
                        render::print_info("Conversation cleared.");
                        continue;
                    }
                }

                println!();
                let mut in_code_block = false;

                if let Err(e) = agent
                    .run_turn(&input, |ev| match ev {
                        AgentEvent::TextDelta(t) => {
                            render::render_delta(&t, &mut in_code_block);
                        }
                        AgentEvent::ToolStart { name } => {
                            render::print_tool_start(&name);
                        }
                        AgentEvent::ToolDone { name, is_error, .. } => {
                            render::print_tool_done(&name, is_error);
                        }
                        AgentEvent::TokenUsage { .. } => {}
                        AgentEvent::TurnDone => {
                            println!("\n");
                        }
                    })
                    .await
                {
                    render::print_error(&format!("{e:#}"));
                }
            }
            Err(ReadlineError::Interrupted) => {
                render::print_info("(Ctrl-C)");
                continue;
            }
            Err(ReadlineError::Eof) => {
                render::print_info("Goodbye.");
                break;
            }
            Err(e) => {
                render::print_error(&format!("readline: {e}"));
                break;
            }
        }
    }

    if let Some(parent) = history_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = rl.save_history(&history_path);
    Ok(())
}
