use serde::{Deserialize, Serialize};

// ── Message types ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    Assistant,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text {
        text: String,
    },
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    ToolResult {
        tool_use_id: String,
        content: String,
        #[serde(default)]
        is_error: bool,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: Vec<ContentBlock>,
}

impl Message {
    pub fn user(text: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: vec![ContentBlock::Text { text: text.into() }],
        }
    }

    pub fn assistant(text: impl Into<String>) -> Self {
        Self {
            role: Role::Assistant,
            content: vec![ContentBlock::Text { text: text.into() }],
        }
    }

    /// Estimate tokens for this message (4 chars ≈ 1 token heuristic).
    pub fn estimate_tokens(&self) -> usize {
        self.content
            .iter()
            .map(|b| b.estimate_tokens())
            .sum::<usize>()
            + 4
    }
}

impl ContentBlock {
    pub fn estimate_tokens(&self) -> usize {
        match self {
            ContentBlock::Text { text } => text.len() / 4 + 1,
            ContentBlock::ToolUse { name, input, .. } => {
                name.len() / 4 + input.to_string().len() / 4 + 4
            }
            ContentBlock::ToolResult { content, .. } => content.len() / 4 + 4,
        }
    }
}

// ── Tool definitions ───────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDef {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
}

// ── Streaming events ───────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum StreamEvent {
    /// A delta of text in the current text block.
    TextDelta(String),
    /// A delta of extended thinking content (streaming reasoning).
    ThinkingDelta(String),
    /// A tool call started.
    ToolUseStart { id: String, name: String },
    /// A partial JSON fragment for the tool call input.
    ToolUseDelta(String),
    /// The current tool call block is complete.
    ToolUseEnd,
    /// Token usage reported by the model.
    Usage {
        input_tokens: u32,
        output_tokens: u32,
        /// Prompt-cache tokens written this request (Anthropic prompt caching).
        cache_write_tokens: u32,
        /// Prompt-cache tokens read this request (Anthropic prompt caching).
        cache_read_tokens: u32,
    },
    /// The model has finished (end_turn, tool_use, max_tokens, etc.).
    Done { stop_reason: StopReason },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StopReason {
    EndTurn,
    ToolUse,
    MaxTokens,
    Other(String),
}

impl StopReason {
    pub fn parse(s: &str) -> Self {
        match s {
            "end_turn" => Self::EndTurn,
            "tool_use" => Self::ToolUse,
            "max_tokens" => Self::MaxTokens,
            other => Self::Other(other.to_string()),
        }
    }
}

// ── Thinking level ─────────────────────────────────────────────────────────────

/// Extended thinking budget levels (mirrors pi-mono ThinkingLevel).
/// Maps to Anthropic `thinking.budget_tokens` values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ThinkingLevel {
    #[default]
    Off,
    Low,    // ~2k tokens
    Medium, // ~8k tokens
    High,   // ~16k tokens
    Max,    // ~32k tokens
}

impl ThinkingLevel {
    /// Token budget for this level. None means thinking is disabled.
    pub fn budget_tokens(self) -> Option<u32> {
        match self {
            Self::Off => None,
            Self::Low => Some(2_000),
            Self::Medium => Some(8_000),
            Self::High => Some(16_000),
            Self::Max => Some(32_000),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Max => "max",
        }
    }

    pub fn cycle_next(self) -> Self {
        match self {
            Self::Off => Self::Low,
            Self::Low => Self::Medium,
            Self::Medium => Self::High,
            Self::High => Self::Max,
            Self::Max => Self::Off,
        }
    }
}

// ── Auth storage ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AuthStore {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub anthropic: Option<ProviderAuth>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub copilot: Option<CopilotAuth>,
    /// Legacy API-key login (kept for backward compat).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub openai: Option<ProviderAuth>,
    /// OAuth login via OpenAI device-code flow (preferred over api key).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub openai_oauth: Option<OpenAiOAuth>,
    /// Google Gemini API key.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gemini: Option<ProviderAuth>,
    /// OpenRouter API key (access to 100+ models).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub openrouter: Option<ProviderAuth>,
}

/// Stored after a successful OpenAI device-code OAuth login.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenAiOAuth {
    /// Short-lived access token sent as Bearer on every API call.
    pub access_token: String,
    /// Long-lived refresh token used to obtain new access tokens.
    pub refresh_token: String,
    /// Unix timestamp (seconds) when `access_token` expires.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderAuth {
    pub token: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CopilotAuth {
    /// GitHub OAuth token (long-lived).
    pub github_token: String,
    /// Cached short-lived Copilot token.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub copilot_token: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub copilot_expires_at: Option<i64>,
}

impl AuthStore {
    pub fn load() -> anyhow::Result<Self> {
        let path = auth_path();
        if !path.exists() {
            return Ok(Self::default());
        }
        let raw = std::fs::read_to_string(&path)?;
        Ok(serde_json::from_str(&raw)?)
    }

    pub fn save(&self) -> anyhow::Result<()> {
        let path = auth_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let raw = serde_json::to_string_pretty(self)?;
        std::fs::write(&path, raw)?;
        Ok(())
    }
}

fn auth_path() -> std::path::PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join(".d-code")
        .join("auth.json")
}
