/// Slash command handlers for the interactive REPL.
use dcode_agent::Agent;
use dcode_providers::{model_catalog, parse_provider_selector};

use crate::render;

pub enum CommandResult {
    NotACommand,
    Handled,
    Clear,
    ShowModelPicker {
        options: Vec<ModelOption>,
    },
    SwitchModel {
        provider: &'static str,
        model: Option<String>,
    },
}

#[derive(Debug, Clone)]
pub struct ModelOption {
    pub provider: &'static str,
    pub model: &'static str,
}

fn provider_supports_model(provider: &str, model: &str) -> bool {
    model_catalog()
        .iter()
        .find(|c| c.provider == provider)
        .map(|c| c.models.iter().any(|m| *m == model))
        .unwrap_or(false)
}

/// Returns true if the input was a slash command (consumed it).
pub fn handle(input: &str, agent: &Agent) -> CommandResult {
    let input = input.trim();
    match input {
        "/help" | "/h" => {
            println!(
                "\
Slash commands:
  /help          Show this help
  /status        Session token usage
  /clear         Clear conversation history
  /model          Pick model from menu
  /model [name]   Set provider/model directly
  /quit  /exit   Exit d-code
"
            );
            CommandResult::Handled
        }
        "/status" | "/s" => {
            let s = &agent.session;
            let total = s.total_input_tokens + s.total_output_tokens;
            let out_ratio = if total == 0 {
                0.0
            } else {
                (s.total_output_tokens as f64 * 100.0) / total as f64
            };
            println!(
                "Turns: {}  In: {}  Out: {}  Est. ctx: {}",
                s.turn_count(),
                s.total_input_tokens,
                s.total_output_tokens,
                s.estimated_tokens(),
            );
            println!("Token mix: {:.1}% output", out_ratio);
            println!("Provider: {}", agent.provider_info());
            CommandResult::Handled
        }
        "/quit" | "/exit" | "/q" => {
            render::print_info("Goodbye.");
            std::process::exit(0);
        }
        _ if input.starts_with("/model") => {
            let parts: Vec<&str> = input.splitn(2, ' ').collect();
            if parts.len() == 1 {
                let mut options = Vec::new();
                for catalog in model_catalog() {
                    for model in catalog.models {
                        options.push(ModelOption {
                            provider: catalog.provider,
                            model,
                        });
                    }
                }
                CommandResult::ShowModelPicker { options }
            } else {
                let raw = parts[1].trim();
                match parse_provider_selector(raw, agent.provider_name()) {
                    Ok((provider, model)) => CommandResult::SwitchModel {
                        provider,
                        model: {
                            if let Some(m) = model {
                                if provider_supports_model(provider, m) {
                                    Some(m.to_string())
                                } else {
                                    render::print_error(&format!(
                                        "Model '{m}' is not supported for provider '{provider}'. Run /model to see supported values."
                                    ));
                                    return CommandResult::Handled;
                                }
                            } else {
                                None
                            }
                        },
                    },
                    Err(e) => {
                        render::print_error(&format!("{e}"));
                        CommandResult::Handled
                    }
                }
            }
        }
        "/clear" => CommandResult::Clear,
        _ if input.starts_with('/') => {
            render::print_error(&format!("Unknown command: {input}. Type /help for help."));
            CommandResult::Handled
        }
        _ => CommandResult::NotACommand,
    }
}
