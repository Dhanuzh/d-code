/// Login command implementations for each provider.
use std::io::{self, BufRead, Write};
use std::sync::Arc;

use dcode_providers::{anthropic, copilot, openai, AuthStore};

use crate::render;

// ── Anthropic ──────────────────────────────────────────────────────────────────

pub async fn login_anthropic() -> anyhow::Result<()> {
    // Check if already logged in.
    if let Ok(store) = AuthStore::load() {
        if store.anthropic.is_some() {
            render::print_info("Already logged in to Anthropic. Use /logout anthropic first to re-login.");
            return Ok(());
        }
    }

    let req = anthropic::create_login_url();

    println!();
    render::print_section_header("Anthropic OAuth login");
    println!("  Opening your browser to authorize d-code…");
    println!();

    // Try to open browser automatically.
    let opened = open_browser(&req.url);
    if opened {
        println!("  Browser opened. Complete the authorization and copy the code.");
    } else {
        println!("  Open this URL in your browser:");
        println!();
        println!("  {}", req.url);
    }

    println!();
    print!("  Paste the authorization code: ");
    io::stdout().flush()?;

    let mut code = String::new();
    io::stdin().lock().read_line(&mut code)?;
    let code = code.trim();

    if code.is_empty() {
        anyhow::bail!("No code provided — login cancelled");
    }

    render::print_info("Exchanging code for token…");
    let token = anthropic::exchange_code(code, &req.verifier).await?;
    anthropic::save_token(&token)?;
    render::print_success("Logged in to Anthropic  ✓  (claude-sonnet-4-5 and others now available)");
    Ok(())
}

// ── GitHub Copilot ─────────────────────────────────────────────────────────────

pub async fn login_copilot() -> anyhow::Result<()> {
    if let Ok(store) = AuthStore::load() {
        if store.copilot.is_some() {
            render::print_info("Already logged in to GitHub Copilot. Use /logout copilot first to re-login.");
            return Ok(());
        }
    }

    let start = copilot::start_device_flow().await?;

    println!();
    render::print_section_header("GitHub Copilot login");
    println!("  1. Open:  {}", start.verification_uri);
    println!("  2. Enter: \x1b[1;33m{}\x1b[0m", start.user_code);
    println!();

    // Try to open browser.
    let _ = open_browser(&start.verification_uri);

    println!("  Waiting for you to authorize in the browser…  (Ctrl-C to cancel)");
    println!();

    let cancel = Arc::new(tokio::sync::Notify::new());
    let cancel_clone = cancel.clone();
    tokio::spawn(async move {
        tokio::signal::ctrl_c().await.ok();
        cancel_clone.notify_one();
    });

    let github_token =
        copilot::poll_github_token(&start.device_code, start.interval, cancel).await?;
    copilot::save_github_token(&github_token)?;
    render::print_success("Logged in to GitHub Copilot  ✓  (GPT-4o, Claude, Gemini now available)");
    Ok(())
}

// ── OpenAI ─────────────────────────────────────────────────────────────────────

pub async fn login_openai() -> anyhow::Result<()> {
    if let Ok(store) = AuthStore::load() {
        if store.openai_oauth.is_some() || store.openai.is_some() {
            render::print_info("Already logged in to OpenAI. Use /logout openai first to re-login.");
            return Ok(());
        }
    }

    println!();
    render::print_section_header("OpenAI login");

    // Try device-code OAuth first; fall back to API key if Cloudflare blocks it.
    match openai::start_device_flow().await {
        Ok(start) => {
            println!("  1. Open:  {}", start.verification_uri);
            println!("  2. Enter: \x1b[1;33m{}\x1b[0m", start.user_code);
            println!();
            let _ = open_browser(&start.verification_uri);
            println!("  Waiting for you to authorize in the browser…  (Ctrl-C to cancel)");
            println!();

            let cancel = Arc::new(tokio::sync::Notify::new());
            let cancel_clone = cancel.clone();
            tokio::spawn(async move {
                tokio::signal::ctrl_c().await.ok();
                cancel_clone.notify_one();
            });

            let oauth = openai::poll_device_token(
                &start.device_auth_id,
                &start.user_code,
                start.interval.unwrap_or(5),
                cancel,
            ).await?;

            openai::save_oauth(&oauth)?;
            render::print_success("Logged in to OpenAI  ✓  (GPT-4.1, o3, o4-mini now available)");
        }
        Err(_e) => {
            // OAuth failed for any reason (network unreachable, Cloudflare, 4xx, etc.)
            // Fall back to API key login gracefully.
            render::print_warning("OAuth login unavailable — using API key instead.");
            println!("  Get your key at: \x1b[4mhttps://platform.openai.com/api-keys\x1b[0m");
            println!();
            let _ = open_browser("https://platform.openai.com/api-keys");
            print!("  Paste API key (sk-…): ");
            io::stdout().flush()?;

            let mut key = String::new();
            io::stdin().lock().read_line(&mut key)?;
            let key = key.trim();

            if key.is_empty() {
                anyhow::bail!("No API key provided — login cancelled");
            }
            if !key.starts_with("sk-") {
                anyhow::bail!("Invalid API key (must start with 'sk-')");
            }
            openai::save_api_key(key)?;
            render::print_success("OpenAI API key saved  ✓  (GPT-4.1, o3, o4-mini now available)");
        }
    }
    Ok(())
}

// ── Status ─────────────────────────────────────────────────────────────────────

pub fn show_status() {
    match AuthStore::load() {
        Ok(store) => print_auth_table(&store),
        Err(e) => render::print_error(&format!("Could not read auth store: {e}")),
    }
}

/// Print a formatted auth status table.
pub fn print_auth_table(store: &AuthStore) {
    println!();
    println!("  ┌──────────────┬────────────────────────────────────────────┐");
    println!("  │ Provider     │ Status                                     │");
    println!("  ├──────────────┼────────────────────────────────────────────┤");

    let anthropic_status = if store.anthropic.is_some() {
        "\x1b[32m✓ logged in\x1b[0m   — claude-sonnet-4-5/4-6, opus-4-5, haiku"
    } else {
        "\x1b[2m✗ not logged in\x1b[0m  run: d-code login anthropic"
    };
    let copilot_status = if store.copilot.is_some() {
        "\x1b[32m✓ logged in\x1b[0m   — gpt-4o, claude-sonnet-4, gemini-2.5"
    } else {
        "\x1b[2m✗ not logged in\x1b[0m  run: d-code login copilot"
    };
    let openai_status = if store.openai.is_some() {
        "\x1b[32m✓ logged in\x1b[0m   — gpt-4.1, gpt-4o, o3, o4-mini"
    } else {
        "\x1b[2m✗ not logged in\x1b[0m  run: d-code login openai"
    };

    println!("  │ anthropic    │ {}│", pad_to(anthropic_status, 42));
    println!("  │ copilot      │ {}│", pad_to(copilot_status, 42));
    println!("  │ openai       │ {}│", pad_to(openai_status, 42));
    println!("  └──────────────┴────────────────────────────────────────────┘");
    println!();
}

pub fn logout(provider: &str) -> anyhow::Result<()> {
    let mut store = AuthStore::load().unwrap_or_default();
    match provider {
        "anthropic" | "claude" => {
            store.anthropic = None;
            render::print_success("Logged out from Anthropic.");
        }
        "copilot" | "github" => {
            store.copilot = None;
            render::print_success("Logged out from GitHub Copilot.");
        }
        "openai" | "gpt" => {
            store.openai = None;
            render::print_success("Logged out from OpenAI.");
        }
        "all" => {
            store.anthropic = None;
            store.copilot = None;
            store.openai = None;
            render::print_success("Logged out from all providers.");
        }
        other => anyhow::bail!("Unknown provider: {other}. Use: anthropic, copilot, openai, all"),
    }
    store.save()?;
    Ok(())
}

// ── Helpers ────────────────────────────────────────────────────────────────────

/// Try to open a URL in the default browser. Returns true if a command was found.
fn open_browser(url: &str) -> bool {
    #[cfg(target_os = "macos")]
    let cmd = std::process::Command::new("open").arg(url).spawn();
    #[cfg(target_os = "linux")]
    let cmd = std::process::Command::new("xdg-open").arg(url).spawn();
    #[cfg(target_os = "windows")]
    let cmd = std::process::Command::new("cmd")
        .args(["/c", "start", url])
        .spawn();
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    let cmd: Result<_, _> = Err(std::io::Error::new(std::io::ErrorKind::Other, "unsupported"));

    cmd.is_ok()
}

/// Pad a string (with ANSI escape codes) to a visible width.
fn pad_to(s: &str, target_visible: usize) -> String {
    let visible = visible_len(s);
    if visible >= target_visible {
        s.to_string()
    } else {
        format!("{}{}", s, " ".repeat(target_visible - visible))
    }
}

fn visible_len(s: &str) -> usize {
    let mut len = 0usize;
    let mut in_escape = false;
    for ch in s.chars() {
        if in_escape {
            if ch == 'm' { in_escape = false; }
        } else if ch == '\x1b' {
            in_escape = true;
        } else {
            len += 1;
        }
    }
    len
}
