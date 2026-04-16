pub mod antigravity;
pub mod anthropic;
pub mod copilot;
pub mod gemini;
pub mod oauth;
pub mod openai;
pub mod openrouter;
pub mod provider;
pub mod types;

pub use provider::{BoxProvider, Provider};
pub use types::{AuthStore, ContentBlock, Message, Role, StopReason, StreamEvent, ToolDef};

#[derive(Debug, Clone)]
pub struct ProviderModelCatalog {
    pub provider: &'static str,
    pub aliases: &'static [&'static str],
    pub default_model: &'static str,
    pub models: &'static [&'static str],
}

pub fn model_catalog() -> &'static [ProviderModelCatalog] {
    &[
        ProviderModelCatalog {
            provider: "anthropic",
            aliases: &["claude"],
            default_model: anthropic::DEFAULT_MODEL,
            models: anthropic::SUPPORTED_MODELS,
        },
        ProviderModelCatalog {
            provider: "copilot",
            aliases: &["github"],
            default_model: copilot::DEFAULT_MODEL,
            models: copilot::SUPPORTED_MODELS,
        },
        ProviderModelCatalog {
            provider: "openai",
            aliases: &["gpt"],
            default_model: openai::DEFAULT_MODEL,
            models: openai::SUPPORTED_MODELS,
        },
        ProviderModelCatalog {
            provider: "gemini",
            aliases: &["google"],
            default_model: gemini::DEFAULT_MODEL,
            models: gemini::SUPPORTED_MODELS,
        },
        ProviderModelCatalog {
            provider: "openrouter",
            aliases: &["or"],
            default_model: openrouter::DEFAULT_MODEL,
            models: openrouter::SUPPORTED_MODELS,
        },
        ProviderModelCatalog {
            provider: "antigravity",
            aliases: &["ag"],
            default_model: antigravity::DEFAULT_MODEL,
            models: antigravity::SUPPORTED_MODELS,
        },
    ]
}

fn normalize_provider_name(name: &str) -> Option<&'static str> {
    match name {
        "anthropic" | "claude" => Some("anthropic"),
        "copilot" | "github" => Some("copilot"),
        "openai" | "gpt" => Some("openai"),
        "gemini" | "google" => Some("gemini"),
        "openrouter" | "or" => Some("openrouter"),
        "antigravity" | "ag" => Some("antigravity"),
        _ => None,
    }
}

/// Parse a provider selector of the form:
/// - provider
/// - provider/model
/// - model (uses fallback provider)
pub fn parse_provider_selector<'a>(
    selector: &'a str,
    fallback_provider: &'a str,
) -> anyhow::Result<(&'static str, Option<&'a str>)> {
    let raw = selector.trim();
    if raw.is_empty() {
        anyhow::bail!("Model selector cannot be empty")
    }

    if let Some((p, m)) = raw.split_once('/') {
        let provider = normalize_provider_name(p.trim())
            .ok_or_else(|| anyhow::anyhow!("Unknown provider: {}", p.trim()))?;
        let model = m.trim();
        if model.is_empty() {
            anyhow::bail!("Model name cannot be empty")
        }
        return Ok((provider, Some(model)));
    }

    if let Some(provider) = normalize_provider_name(raw) {
        return Ok((provider, None));
    }

    let provider = normalize_provider_name(fallback_provider)
        .ok_or_else(|| anyhow::anyhow!("Unknown provider: {fallback_provider}"))?;
    Ok((provider, Some(raw)))
}

/// Load the active provider from auth storage.
/// Provider priority: anthropic > copilot > openai.
pub fn load_provider(preferred: Option<&str>) -> anyhow::Result<BoxProvider> {
    load_provider_with_model(preferred, None)
}

pub fn load_provider_with_model(
    preferred: Option<&str>,
    model: Option<&str>,
) -> anyhow::Result<BoxProvider> {
    let store = AuthStore::load().unwrap_or_default();

    let name = preferred.unwrap_or_else(|| {
        if store.anthropic.is_some() {
            "anthropic"
        } else if store.copilot.is_some() {
            "copilot"
        } else if store.openai.is_some() || store.openai_oauth.is_some() {
            "openai"
        } else if store.gemini.is_some() {
            "gemini"
        } else if store.openrouter.is_some() {
            "openrouter"
        } else {
            "anthropic"
        }
    });

    let provider = normalize_provider_name(name).ok_or_else(|| {
        anyhow::anyhow!("Unknown provider: {name}. Use: anthropic, copilot, openai")
    })?;
    let model = model.unwrap_or_else(|| {
        model_catalog()
            .iter()
            .find(|c| c.provider == provider)
            .map(|c| c.default_model)
            .unwrap_or("")
    });

    match provider {
        "anthropic" => Ok(Box::new(
            anthropic::AnthropicProvider::from_auth_with_model(model)?,
        )),
        "copilot" => Ok(Box::new(copilot::CopilotProvider::from_auth_with_model(
            model,
        )?)),
        "openai" => Ok(Box::new(openai::OpenAIProvider::from_auth_with_model(
            model,
        )?)),
        "gemini" => Ok(Box::new(gemini::GeminiProvider::from_auth_with_model(
            model,
        )?)),
        "openrouter" => Ok(Box::new(
            openrouter::OpenRouterProvider::from_auth_with_model(model)?,
        )),
        "antigravity" => Ok(Box::new(
            antigravity::AntigravityProvider::from_auth_with_model(model)?,
        )),
        _ => anyhow::bail!(
            "Unknown provider: {provider}. Use: anthropic, copilot, openai, gemini, openrouter, antigravity"
        ),
    }
}
