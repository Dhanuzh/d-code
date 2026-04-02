mod commands;
mod login;
mod render;
mod repl;

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

#[tokio::main]
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
                "copilot"  | "github" => login::login_copilot().await?,
                "openai"   | "gpt"    => login::login_openai().await?,
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
            println!("Provider login status:");
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
    let mut in_code_block = false;

    agent
        .run_turn(&prompt, |ev| match ev {
            AgentEvent::TextDelta(t) => {
                render::render_delta(&t, &mut in_code_block);
            }
            AgentEvent::ToolStart { name } => {
                render::print_tool_start(&name);
            }
            AgentEvent::ToolDone { name, is_error, .. } => {
                render::print_tool_done(&name, is_error);
            }
            AgentEvent::TokenUsage { input, output } => {
                // Print usage at end.
                let _ = (input, output);
            }
            AgentEvent::TurnDone => {
                println!();
            }
        })
        .await?;

    Ok(())
}
