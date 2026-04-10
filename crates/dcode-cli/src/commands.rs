/// Slash command handlers for the interactive REPL.
use dcode_agent::Agent;
use dcode_providers::{model_catalog, parse_provider_selector};

use crate::render;

pub enum CommandResult {
    NotACommand,
    Handled,
    Clear,
    Compact,
    Undo,
    ShowModelPicker {
        options: Vec<ModelOption>,
    },
    SwitchModel {
        provider: &'static str,
        model: Option<String>,
    },
    Login {
        provider: Option<String>,
    },
    Logout {
        provider: Option<String>,
    },
    ShowSessions,
    ResumeLatest,
    NewSession,
    Export {
        path: Option<String>,
    },
    Init,
    /// Set a display name for the current session.
    SetName {
        name: String,
    },
    /// Copy the last assistant message to clipboard.
    CopyLast,
    /// Fork the session at a specific turn (0 = current).
    Fork {
        turn: Option<usize>,
    },
    /// Show the session tree (all sessions with parent-child relationships).
    ShowTree,
    /// Share the session as a GitHub gist.
    Share,
    /// List available prompt templates.
    ListPrompts,
    /// (Unused — template expansion is handled inline before command dispatch.)
    #[allow(dead_code)]
    ExpandPrompt {
        expanded: String,
    },
    /// List available skills.
    ListSkills,
}

#[derive(Debug, Clone)]
pub struct ModelOption {
    pub provider: &'static str,
    pub model: &'static str,
}

/// Returns (input_$/M, output_$/M, label) for rough cost estimation.
fn cost_rates(model: &str) -> (f64, f64, &'static str) {
    if model.contains("opus-4-5") || model.contains("opus-4-1") {
        (15.0, 75.0, "claude-opus rates")
    } else if model.contains("sonnet-4-6") || model.contains("sonnet-4-5") {
        (3.0, 15.0, "claude-sonnet rates")
    } else if model.contains("haiku") {
        (0.8, 4.0, "claude-haiku rates")
    } else if model.contains("gpt-4.1") || model.contains("gpt-4o") {
        (2.0, 8.0, "gpt-4 rates")
    } else if model.contains("o3") || model.contains("o4") {
        (10.0, 40.0, "o3/o4 rates")
    } else {
        (1.0, 4.0, "estimated rates")
    }
}

fn provider_supports_model(provider: &str, model: &str) -> bool {
    model_catalog()
        .iter()
        .find(|c| c.provider == provider)
        .map(|c| c.models.iter().any(|m| *m == model))
        .unwrap_or(false)
}

/// Handle a slash command. Returns a CommandResult describing what to do.
pub fn handle(input: &str, agent: &Agent) -> CommandResult {
    let input = input.trim();
    let normalized = if let Some(rest) = input.strip_prefix('/') {
        format!("/{}", rest.trim_start())
    } else {
        input.to_string()
    };
    let input = normalized.as_str();
    match input {
        "/help" | "/h" => {
            println!(
                "\
Slash commands:
  /help              Show this help
  /status            Session token usage, cost & provider info
  /compact           Force context compaction (save tokens)
  /undo              Undo the last turn
  /clear             Clear conversation (no save)
  /new               Save current session and start fresh
  /name <title>      Set a display name for the current session
  /sessions          Browse and resume saved sessions
  /resume            Resume latest saved session
  /tree              Show session tree (branches from /fork)
  /fork [N]          Fork session at turn N (default: current)
  /export [file]     Export session to markdown (default: session.md)
  /share             Share session as a secret GitHub gist
  /copy              Copy last assistant message to clipboard
  /init              Scan project and generate DCODE.md context file
  /model             Pick model from menu
  /model [name]      Set provider/model  (e.g. /model gemini/gemini-2.0-flash)
  /login [provider]  Login to provider (anthropic/copilot/openai/gemini/openrouter)
  /logout [provider] Logout from provider
  /prompts           List available prompt templates
  /skills            List available skills
  /quit              Exit d-code

Providers:
  anthropic   — Claude models (OAuth)
  copilot     — GitHub Copilot (OAuth)
  openai      — GPT & o-series (OAuth or API key)
  gemini      — Google Gemini (API key)
  openrouter  — 100+ models via openrouter.ai (API key)

Keyboard shortcuts:
  Ctrl+G          Open current input in $VISUAL/$EDITOR
  Ctrl+P/N        Cycle models forward/backward
  Ctrl+W          Delete word backwards
  Ctrl+K          Kill to end of line
  Shift+Enter     New line in input
  !cmd            Run shell command and add output to context
  !!cmd           Run shell command without adding to context

Tips for saving tokens:
  • Use /compact when context is getting large
  • Use /model or Ctrl+P to switch to a cheaper model for simple tasks
  • Use /new to start a fresh session

Features:
  • Dangerous bash commands require confirmation before running
  • AI can ask you questions mid-task with ask_user tool
  • Git branch shown in prompt automatically
  • Skills: place .md files in ~/.d-code/skills/ for reusable instructions
  • Prompt templates: place .md files in ~/.d-code/prompts/ (invoke with /name)
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
            let ctx_used = s.estimated_tokens();
            // Use the provider's actual context window.
            let ctx_window = agent.provider_context_window() as usize;
            let ctx_pct = (ctx_used as f64 * 100.0) / ctx_window as f64;

            // Rough cost estimate — uses typical rates; shown as orientation only.
            let (input_rate, output_rate, rate_label) = cost_rates(agent.model_name());
            let est_cost_usd = (s.total_input_tokens as f64 / 1_000_000.0) * input_rate
                + (s.total_output_tokens as f64 / 1_000_000.0) * output_rate;

            println!();
            println!("  Provider : {}", agent.provider_info());
            println!("  Turns    : {}", s.turn_count());
            println!(
                "  Tokens   : {} in / {} out  ({:.1}% output)",
                s.total_input_tokens, s.total_output_tokens, out_ratio
            );
            println!("  Context  : ~{ctx_used} tokens in ctx  ({ctx_pct:.1}% of {ctx_window})");
            println!("  Est. cost: ~${est_cost_usd:.4} USD  ({rate_label})");
            println!("  Tip      : /compact · /undo · /new for fresh session");
            println!();
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
        "/init" => CommandResult::Init,
        "/compact" => CommandResult::Compact,
        "/undo" | "/u" => CommandResult::Undo,
        "/clear" => CommandResult::Clear,
        "/login" => CommandResult::Login { provider: None },
        "/logout" => CommandResult::Logout { provider: None },
        "/sessions" => CommandResult::ShowSessions,
        "/resume" => CommandResult::ResumeLatest,
        "/new" => CommandResult::NewSession,
        "/export" => CommandResult::Export { path: None },
        "/copy" => CommandResult::CopyLast,
        "/fork" => CommandResult::Fork { turn: None },
        "/tree" => CommandResult::ShowTree,
        "/share" => CommandResult::Share,
        "/prompts" => CommandResult::ListPrompts,
        "/skills" => CommandResult::ListSkills,
        _ if input.starts_with("/name") => {
            let name = input.strip_prefix("/name").unwrap_or("").trim().to_string();
            CommandResult::SetName { name }
        }
        _ if input.starts_with("/fork") => {
            let arg = input.strip_prefix("/fork").unwrap_or("").trim();
            let turn = arg.parse::<usize>().ok();
            CommandResult::Fork { turn }
        }
        _ if input.starts_with("/resume") => CommandResult::ResumeLatest,
        _ if input.starts_with("/login") => {
            let provider = input
                .strip_prefix("/login")
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .map(String::from);
            CommandResult::Login { provider }
        }
        _ if input.starts_with("/logout") => {
            let provider = input
                .strip_prefix("/logout")
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .map(String::from);
            CommandResult::Logout { provider }
        }
        _ if input.starts_with("/export") => {
            let path = input
                .strip_prefix("/export")
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .map(String::from);
            CommandResult::Export { path }
        }
        _ if input.starts_with('/') => {
            render::print_error(&format!("Unknown command: {input}. Type /help for help."));
            CommandResult::Handled
        }
        _ => CommandResult::NotACommand,
    }
}
