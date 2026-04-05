mod commands;
mod input;
mod login;
mod render;
mod repl;
mod sessions;

use std::path::PathBuf;

use clap::{Parser, Subcommand};

/// d-code — lightweight CLI AI coding agent
#[derive(Parser)]
#[command(name = "d-code", version, about = "Lightweight AI coding agent")]
struct Cli {
    /// One-shot prompt (non-interactive mode).
    #[arg(short = 'p', long, value_name = "PROMPT")]
    prompt: Option<String>,

    /// Provider to use: anthropic, copilot, openai.
    #[arg(short = 'P', long, value_name = "PROVIDER")]
    provider: Option<String>,

    /// Working directory (defaults to current directory).
    #[arg(short = 'C', long, value_name = "DIR")]
    dir: Option<PathBuf>,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Login to an AI provider.
    Login {
        /// Provider: anthropic, copilot, openai.
        provider: Option<String>,
    },
    /// Logout from a provider.
    Logout {
        /// Provider (or 'all').
        provider: String,
    },
    /// Show login status.
    Status,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    let cwd = cli
        .dir
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));

    match cli.command {
        // ── Login subcommands ──────────────────────────────────────────────
        Some(Command::Login { provider }) => {
            let p = provider.as_deref().unwrap_or("anthropic");
            match p {
                "anthropic" | "claude" => login::login_anthropic().await?,
                "copilot" | "github" => login::login_copilot().await?,
                "openai" | "gpt" => login::login_openai().await?,
                other => {
                    render::print_error(&format!(
                        "Unknown provider '{other}'. Use: anthropic, copilot, openai"
                    ));
                    std::process::exit(1);
                }
            }
        }
        Some(Command::Logout { provider }) => {
            login::logout(&provider)?;
        }
        Some(Command::Status) => {
            login::show_status();
        }

        // ── Main modes ─────────────────────────────────────────────────────
        None => {
            if let Some(prompt) = cli.prompt {
                // One-shot mode.
                run_oneshot(prompt, cwd, cli.provider).await?;
            } else {
                // Interactive REPL.
                repl::run(cwd, cli.provider).await?;
            }
        }
    }

    Ok(())
}

async fn run_oneshot(
    prompt: String,
    cwd: PathBuf,
    provider_name: Option<String>,
) -> anyhow::Result<()> {
    use dcode_agent::{Agent, AgentEvent};
    use dcode_providers::load_provider;

    let provider = load_provider(provider_name.as_deref())?;
    let mut agent = Agent::new(provider, cwd);
    let mut md = render::MarkdownRenderer::new();
    let mut xml_filter = render::XmlFilter::new();

    agent
        .run_turn(&prompt, |ev| match ev {
            AgentEvent::TextDelta(t) => {
                let clean = xml_filter.push(&t);
                if !clean.is_empty() {
                    md.push(&clean);
                }
            }
            AgentEvent::ToolStart { name } => {
                md.flush();
                render::print_tool_start(&name);
            }
            AgentEvent::ToolDone {
                name,
                input,
                is_error,
                ..
            } => {
                render::print_tool_done(&name, &input, is_error);
            }
            AgentEvent::TokenUsage { .. } => {}
            AgentEvent::UserQuestion { question, choices } => {
                render::prompt_user_question(&question, &choices);
            }
            AgentEvent::ConfirmBash { command } => {
                render::confirm_dangerous_bash(&command);
            }
            AgentEvent::DoomLoop { tool } => {
                render::print_error(&format!(
                    "Doom loop: '{tool}' called 3× with same args. Stopping."
                ));
            }
            AgentEvent::TurnDone => {
                let leftover = xml_filter.flush();
                if !leftover.is_empty() {
                    md.push(&leftover);
                }
                md.flush();
                println!();
            }
        })
        .await?;

    Ok(())
}
