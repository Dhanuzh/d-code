/// Login command implementations for each provider.
use std::io::{self, BufRead, Write};
use std::sync::Arc;

use dcode_providers::{anthropic, copilot, openai};

use crate::render;

pub async fn login_anthropic() -> anyhow::Result<()> {
    let req = anthropic::create_login_url();

    println!("\nAnthropicOAuth (PKCE)");
    println!("─────────────────────");
    println!("1. Open this URL in your browser:\n");
    println!("   {}\n", req.url);
    println!("2. Authorize and copy the code shown on the callback page.");
    print!("\nPaste the code here: ");
    io::stdout().flush()?;

    let mut code = String::new();
    io::stdin().lock().read_line(&mut code)?;
    let code = code.trim();

    if code.is_empty() {
        anyhow::bail!("No code provided");
    }

    render::print_info("Exchanging code for token…");
    let token = anthropic::exchange_code(code, &req.verifier).await?;
    anthropic::save_token(&token)?;
    render::print_info("Logged in to Anthropic. Token saved to ~/.d-code/auth.json");
    Ok(())
}

pub async fn login_copilot() -> anyhow::Result<()> {
    let start = copilot::start_device_flow().await?;

    println!("\nGitHub Copilot (device code)");
    println!("────────────────────────────");
    println!("1. Open: {}", start.verification_uri);
    println!("2. Enter code: \x1b[1;33m{}\x1b[0m", start.user_code);
    println!("\nWaiting for authorization…");

    let cancel = Arc::new(tokio::sync::Notify::new());

    // Allow Ctrl-C to cancel.
    let cancel_clone = cancel.clone();
    tokio::spawn(async move {
        tokio::signal::ctrl_c().await.ok();
        cancel_clone.notify_one();
    });

    let github_token =
        copilot::poll_github_token(&start.device_code, start.interval, cancel).await?;
    copilot::save_github_token(&github_token)?;
    render::print_info("Logged in to GitHub Copilot. Token saved to ~/.d-code/auth.json");
    Ok(())
}

pub async fn login_openai() -> anyhow::Result<()> {
    println!("\nOpenAI API key");
    println!("──────────────");
    println!("Get your API key from: https://platform.openai.com/api-keys");
    print!("\nPaste API key: ");
    io::stdout().flush()?;

    let mut key = String::new();
    io::stdin().lock().read_line(&mut key)?;
    let key = key.trim();

    if key.is_empty() {
        anyhow::bail!("No API key provided");
    }
    if !key.starts_with("sk-") {
        anyhow::bail!("Invalid OpenAI API key (should start with 'sk-')");
    }

    openai::save_api_key(key)?;
    render::print_info("OpenAI API key saved to ~/.d-code/auth.json");
    Ok(())
}

pub fn show_status() {
    match dcode_providers::AuthStore::load() {
        Ok(store) => {
            let anthropic = if store.anthropic.is_some() { "✓ logged in" } else { "✗ not logged in" };
            let copilot   = if store.copilot.is_some()   { "✓ logged in" } else { "✗ not logged in" };
            let openai    = if store.openai.is_some()    { "✓ logged in" } else { "✗ not logged in" };
            println!("  anthropic  {anthropic}");
            println!("  copilot    {copilot}");
            println!("  openai     {openai}");
        }
        Err(e) => render::print_error(&format!("Could not read auth store: {e}")),
    }
}

pub fn logout(provider: &str) -> anyhow::Result<()> {
    let mut store = dcode_providers::AuthStore::load().unwrap_or_default();
    match provider {
        "anthropic" | "claude" => store.anthropic = None,
        "copilot" | "github"  => store.copilot   = None,
        "openai" | "gpt"      => store.openai     = None,
        "all"                 => {
            store.anthropic = None;
            store.copilot   = None;
            store.openai    = None;
        }
        other => anyhow::bail!("Unknown provider: {other}"),
    }
    store.save()?;
    render::print_info(&format!("Logged out from {provider}"));
    Ok(())
}
