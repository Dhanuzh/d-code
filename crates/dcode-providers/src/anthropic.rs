/// Anthropic provider: OAuth PKCE login + Claude streaming API.
use std::pin::Pin;

use anyhow::{bail, Context};
use async_trait::async_trait;
use futures::Stream;
use serde::Deserialize;

use crate::oauth::{generate_pkce, url_encode};
use crate::provider::Provider;
use crate::types::{
    AuthStore, ContentBlock, Message, ProviderAuth, Role, StopReason, StreamEvent, ToolDef,
};

// ── OAuth constants ────────────────────────────────────────────────────────────

const CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
const AUTHORIZE_URL: &str = "https://claude.ai/oauth/authorize";
const TOKEN_URL: &str = "https://console.anthropic.com/v1/oauth/token";
const REDIRECT_URI: &str = "https://console.anthropic.com/oauth/code/callback";
const SCOPES: &str = "org:create_api_key user:profile user:inference";
const USER_AGENT: &str = "claude-cli/2.1.80 (external, cli)";

// ── API constants ──────────────────────────────────────────────────────────────

const API_BASE: &str = "https://api.anthropic.com";
const API_VERSION: &str = "2023-06-01";

pub const DEFAULT_MODEL: &str = "claude-sonnet-4-5";
pub const SUPPORTED_MODELS: &[&str] = &[
    "claude-sonnet-4-5",
    "claude-sonnet-4-6",
    "claude-opus-4-5",
    "claude-opus-4-1",
    "claude-3-7-sonnet-latest",
    "claude-3-5-haiku-latest",
    "claude-haiku-4-5-20251001",
];
pub const CONTEXT_WINDOW: u32 = 200_000;

// ── OAuth login flow ───────────────────────────────────────────────────────────

pub struct AnthropicLoginRequest {
    pub url: String,
    pub verifier: String,
}

/// Step 1 — produce the authorization URL + PKCE verifier.
pub fn create_login_url() -> AnthropicLoginRequest {
    let pkce = generate_pkce();
    let params = [
        ("code", "true"),
        ("response_type", "code"),
        ("client_id", CLIENT_ID),
        ("redirect_uri", REDIRECT_URI),
        ("scope", SCOPES),
        ("code_challenge", &pkce.challenge),
        ("code_challenge_method", "S256"),
        ("state", &pkce.verifier),
    ];
    let query = params
        .iter()
        .map(|(k, v)| format!("{k}={}", url_encode(v)))
        .collect::<Vec<_>>()
        .join("&");

    AnthropicLoginRequest {
        url: format!("{AUTHORIZE_URL}?{query}"),
        verifier: pkce.verifier,
    }
}

/// Step 2 — exchange the auth code for an access token.
pub async fn exchange_code(raw_code: &str, verifier: &str) -> anyhow::Result<String> {
    // Strip any `#state=...` fragment the callback page may append.
    let code = raw_code.trim().split('#').next().unwrap_or(raw_code.trim());

    let body = format!(
        "grant_type=authorization_code\
         &code={}\
         &code_verifier={}\
         &client_id={}\
         &redirect_uri={}\
         &state={}",
        url_encode(code),
        url_encode(verifier),
        url_encode(CLIENT_ID),
        url_encode(REDIRECT_URI),
        url_encode(verifier),
    );

    let client = reqwest::Client::new();
    let resp = client
        .post(TOKEN_URL)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .header("User-Agent", USER_AGENT)
        .body(body)
        .send()
        .await
        .context("token exchange request failed")?;

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        bail!("Anthropic token exchange failed {status}: {text}");
    }

    #[derive(Deserialize)]
    struct Resp {
        access_token: String,
    }
    let r: Resp = resp.json().await.context("parse token response")?;
    Ok(r.access_token)
}

/// Save the token to auth store.
pub fn save_token(token: &str) -> anyhow::Result<()> {
    let mut store = AuthStore::load().unwrap_or_default();
    store.anthropic = Some(ProviderAuth {
        token: token.to_string(),
        expires_at: None,
    });
    store.save()
}

// ── Provider implementation ────────────────────────────────────────────────────

pub struct AnthropicProvider {
    pub model: String,
    token: String,
    client: reqwest::Client,
}

impl AnthropicProvider {
    pub fn from_auth() -> anyhow::Result<Self> {
        Self::from_auth_with_model(DEFAULT_MODEL)
    }

    pub fn from_auth_with_model(model: impl Into<String>) -> anyhow::Result<Self> {
        let store = AuthStore::load()?;
        let auth = store.anthropic.ok_or_else(|| {
            anyhow::anyhow!("Not logged in to Anthropic. Run: d-code login anthropic")
        })?;
        Ok(Self::new(auth.token, model))
    }

    pub fn new(token: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            token: token.into(),
            client: reqwest::Client::builder()
                .connection_verbose(false)
                .pool_max_idle_per_host(4)
                .build()
                .expect("reqwest client"),
        }
    }
}

// ── Streaming SSE parser ───────────────────────────────────────────────────────

/// Internal SSE event types from Anthropic.
#[allow(dead_code)]
#[derive(Deserialize, Debug)]
#[serde(tag = "type", rename_all = "snake_case")]
enum SseEvent {
    MessageStart {
        message: MessageStartData,
    },
    ContentBlockStart {
        index: u32,
        content_block: ContentBlockStart,
    },
    ContentBlockDelta {
        index: u32,
        delta: ContentBlockDelta,
    },
    ContentBlockStop {
        index: u32,
    },
    MessageDelta {
        delta: MessageDeltaData,
        usage: Option<UsageData>,
    },
    MessageStop,
    Ping,
    Error {
        error: serde_json::Value,
    },
}

#[derive(Deserialize, Debug)]
struct MessageStartData {
    usage: Option<UsageData>,
}

#[allow(dead_code)]
#[derive(Deserialize, Debug)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ContentBlockStart {
    Text { text: String },
    ToolUse { id: String, name: String },
}

#[derive(Deserialize, Debug)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ContentBlockDelta {
    TextDelta { text: String },
    InputJsonDelta { partial_json: String },
}

#[derive(Deserialize, Debug)]
struct MessageDeltaData {
    stop_reason: Option<String>,
}

#[derive(Deserialize, Debug)]
struct UsageData {
    input_tokens: Option<u32>,
    output_tokens: Option<u32>,
}

// ── Request / response types ──────────────────────────────────────────────────

/// Convert messages to API format. When `use_cache` is true, marks a cache
/// checkpoint on the second-to-last user message for long conversations,
/// saving up to 90% on those prefix tokens.
fn messages_to_api_with_cache(messages: &[Message]) -> Vec<serde_json::Value> {
    messages_to_api_with_cache_inner(messages, true)
}

fn messages_to_api_with_cache_inner(messages: &[Message], use_cache: bool) -> Vec<serde_json::Value> {
    // Find the index of the penultimate assistant message for cache checkpoint.
    // We cache everything up to (but not including) the last two messages so
    // the stable "history" is cached and only new content is charged full price.
    let cache_checkpoint_idx: Option<usize> = if use_cache && messages.len() >= 4 {
        // Mark cache on the message at len-4 (second to last full turn)
        Some(messages.len().saturating_sub(4))
    } else {
        None
    };

    messages
        .iter()
        .enumerate()
        .map(|(idx, m)| {
            let role = match m.role {
                Role::User => "user",
                Role::Assistant => "assistant",
            };
            let is_cache_point = cache_checkpoint_idx == Some(idx);
            let n_blocks = m.content.len();
            let content: Vec<serde_json::Value> = m
                .content
                .iter()
                .enumerate()
                .map(|(block_idx, b)| {
                    let is_last_block = block_idx + 1 == n_blocks;
                    let add_cache = is_cache_point && is_last_block;
                    match b {
                        ContentBlock::Text { text } => {
                            if add_cache {
                                serde_json::json!({"type":"text","text":text,"cache_control":{"type":"ephemeral"}})
                            } else {
                                serde_json::json!({"type":"text","text":text})
                            }
                        }
                        ContentBlock::ToolUse { id, name, input } => {
                            serde_json::json!({"type":"tool_use","id":id,"name":name,"input":input})
                        }
                        ContentBlock::ToolResult {
                            tool_use_id,
                            content,
                            is_error,
                        } => {
                            // Detect inline images encoded as data URIs.
                            let content_val = if let Some(img) = parse_data_image_uri(content) {
                                serde_json::json!([{
                                    "type": "image",
                                    "source": {
                                        "type": "base64",
                                        "media_type": img.0,
                                        "data": img.1,
                                    }
                                }])
                            } else {
                                serde_json::json!(content)
                            };
                            if add_cache {
                                serde_json::json!({
                                    "type":"tool_result",
                                    "tool_use_id":tool_use_id,
                                    "content":content_val,
                                    "is_error":is_error,
                                    "cache_control":{"type":"ephemeral"},
                                })
                            } else {
                                serde_json::json!({
                                    "type":"tool_result",
                                    "tool_use_id":tool_use_id,
                                    "content":content_val,
                                    "is_error":is_error,
                                })
                            }
                        }
                    }
                })
                .collect();
            serde_json::json!({"role": role, "content": content})
        })
        .collect()
}

/// Parse a `data:<mime>;base64,<data>` URI from a tool result.
/// Returns `Some((mime_type, base64_data))` for supported image types.
fn parse_data_image_uri(s: &str) -> Option<(&str, &str)> {
    let rest = s.strip_prefix("data:")?;
    let (mime, rest) = rest.split_once(';')?;
    let data = rest.strip_prefix("base64,")?;
    // Only pass through supported Anthropic image types.
    if matches!(mime, "image/jpeg" | "image/png" | "image/gif" | "image/webp") {
        Some((mime, data))
    } else {
        None
    }
}

fn tools_to_api(tools: &[ToolDef]) -> Vec<serde_json::Value> {
    tools
        .iter()
        .map(|t| {
            serde_json::json!({
                "name": t.name,
                "description": t.description,
                "input_schema": t.input_schema,
            })
        })
        .collect()
}

#[async_trait]
impl Provider for AnthropicProvider {
    fn name(&self) -> &str {
        "anthropic"
    }
    fn model(&self) -> &str {
        &self.model
    }
    fn context_window(&self) -> u32 {
        CONTEXT_WINDOW
    }

    async fn list_models(&self) -> Vec<String> {
        // Anthropic doesn't expose a public models-list endpoint; return static catalog.
        SUPPORTED_MODELS.iter().map(|s| s.to_string()).collect()
    }

    async fn chat_stream(
        &self,
        system: &str,
        messages: &[Message],
        tools: &[ToolDef],
        max_tokens: u32,
    ) -> anyhow::Result<Pin<Box<dyn Stream<Item = anyhow::Result<StreamEvent>> + Send>>> {
        let url = format!("{API_BASE}/v1/messages");

        // System prompt with prompt caching — cached after first call, saving ~90% on hits.
        let system_block = serde_json::json!([{
            "type": "text",
            "text": system,
            "cache_control": {"type": "ephemeral"}
        }]);

        let mut body = serde_json::json!({
            "model": self.model,
            "max_tokens": max_tokens,
            "stream": true,
            "system": system_block,
            "messages": messages_to_api_with_cache(messages),
        });
        if !tools.is_empty() {
            // Cache the entire tools block (last tool gets the breakpoint marker).
            let mut tools_json = tools_to_api(tools);
            if let Some(last) = tools_json.last_mut() {
                last["cache_control"] = serde_json::json!({"type": "ephemeral"});
            }
            body["tools"] = serde_json::json!(tools_json);
        }

        let resp = self
            .client
            .post(&url)
            .header("x-api-key", &self.token)
            .header("anthropic-version", API_VERSION)
            .header("anthropic-beta", "prompt-caching-2024-07-31")
            .header("content-type", "application/json")
            .header("User-Agent", USER_AGENT)
            .json(&body)
            .send()
            .await
            .context("Anthropic API request failed")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            bail!("Anthropic API error {status}: {text}");
        }

        let stream = parse_anthropic_sse(resp);
        Ok(Box::pin(stream))
    }
}

fn parse_anthropic_sse(
    resp: reqwest::Response,
) -> impl Stream<Item = anyhow::Result<StreamEvent>> + Send {
    use futures::StreamExt;

    let byte_stream = resp.bytes_stream();

    async_stream::stream! {
        use futures::pin_mut;
        pin_mut!(byte_stream);

        let mut buf = String::new();
        let mut stop_reason: Option<String> = None;

        while let Some(chunk) = byte_stream.next().await {
            let chunk = chunk.context("SSE read error")?;
            let text = std::str::from_utf8(&chunk).context("SSE UTF-8")?;
            buf.push_str(text);

            // Process complete SSE lines.
            while let Some(pos) = buf.find('\n') {
                let line = buf[..pos].trim_end_matches('\r').to_string();
                buf.drain(..=pos);

                if let Some(data) = line.strip_prefix("data: ") {
                    if data == "[DONE]" {
                        break;
                    }
                    match serde_json::from_str::<SseEvent>(data) {
                        Ok(event) => {
                            for ev in translate_sse(event, &mut stop_reason) {
                                yield Ok(ev);
                            }
                        }
                        Err(e) => {
                            // Skip unparseable events (ping, etc.)
                            let _ = e;
                        }
                    }
                }
            }
        }

        // Emit final Done if we haven't already.
        let reason = stop_reason
            .as_deref()
            .map(StopReason::from_str)
            .unwrap_or(StopReason::EndTurn);
        yield Ok(StreamEvent::Done { stop_reason: reason });
    }
}

fn translate_sse(event: SseEvent, stop_reason: &mut Option<String>) -> Vec<StreamEvent> {
    match event {
        SseEvent::MessageStart { message } => {
            if let Some(u) = message.usage {
                vec![StreamEvent::Usage {
                    input_tokens: u.input_tokens.unwrap_or(0),
                    output_tokens: u.output_tokens.unwrap_or(0),
                }]
            } else {
                vec![]
            }
        }
        SseEvent::ContentBlockStart { content_block, .. } => match content_block {
            ContentBlockStart::ToolUse { id, name } => {
                vec![StreamEvent::ToolUseStart { id, name }]
            }
            ContentBlockStart::Text { .. } => vec![],
        },
        SseEvent::ContentBlockDelta { delta, .. } => match delta {
            ContentBlockDelta::TextDelta { text } => vec![StreamEvent::TextDelta(text)],
            ContentBlockDelta::InputJsonDelta { partial_json } => {
                vec![StreamEvent::ToolUseDelta(partial_json)]
            }
        },
        SseEvent::ContentBlockStop { .. } => vec![StreamEvent::ToolUseEnd],
        SseEvent::MessageDelta { delta, usage } => {
            let mut out = vec![];
            if let Some(reason) = delta.stop_reason {
                *stop_reason = Some(reason);
            }
            if let Some(u) = usage {
                out.push(StreamEvent::Usage {
                    input_tokens: u.input_tokens.unwrap_or(0),
                    output_tokens: u.output_tokens.unwrap_or(0),
                });
            }
            out
        }
        SseEvent::MessageStop => vec![],
        SseEvent::Ping => vec![],
        SseEvent::Error { error } => {
            vec![StreamEvent::TextDelta(format!("\n[API error: {error}]\n"))]
        }
    }
}
