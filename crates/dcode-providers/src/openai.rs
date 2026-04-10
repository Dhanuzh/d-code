/// OpenAI provider: API key login + chat completions streaming.
use std::pin::Pin;

use anyhow::{bail, Context};
use async_trait::async_trait;
use futures::Stream;

use crate::provider::Provider;
use crate::types::{
    AuthStore, ContentBlock, Message, OpenAiOAuth, ProviderAuth, Role, StopReason, StreamEvent,
    ToolDef,
};

const API_BASE: &str = "https://api.openai.com";
const USER_AGENT: &str = "d-code/0.1";

// ── OAuth constants (OpenAI custom device-auth flow, same as Codex CLI) ─────────
const OAUTH_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const AUTH_BASE: &str = "https://auth.openai.com";
/// Step 1 — request a user code + device_auth_id.
const DEVICE_USERCODE_URL: &str = "https://auth.openai.com/api/accounts/deviceauth/usercode";
/// Step 2 — poll until an authorization_code is issued.
const DEVICE_TOKEN_URL: &str = "https://auth.openai.com/api/accounts/deviceauth/token";
/// Step 3 — exchange authorization_code → access_token + refresh_token.
const CODE_EXCHANGE_URL: &str = "https://auth.openai.com/oauth/token";
/// Redirect URI expected by the token endpoint.
const REDIRECT_URI: &str = "https://auth.openai.com/deviceauth/callback";

pub const DEFAULT_MODEL: &str = "gpt-4.1-mini";
pub const SUPPORTED_MODELS: &[&str] = &[
    "gpt-4.1-mini",
    "gpt-4.1-nano",
    "gpt-4.1",
    "gpt-4o",
    "gpt-4o-mini",
    "o3",
    "o3-mini",
    "o4-mini",
];
pub const CONTEXT_WINDOW: u32 = 128_000;

// ── Login ──────────────────────────────────────────────────────────────────────

/// Response from POST /api/accounts/deviceauth/usercode
#[derive(serde::Deserialize)]
pub struct DeviceCodeResp {
    /// Opaque ID sent back in every poll request.
    pub device_auth_id: String,
    /// Short code the user types at the verification URL.
    pub user_code: String,
    /// URL the user opens (defaults to https://auth.openai.com/dcode/device).
    #[serde(default)]
    pub verification_uri: String,
    /// Polling interval in seconds. API returns this as a string.
    #[serde(default, deserialize_with = "de_string_or_u64")]
    pub interval: Option<u64>,
}

fn de_string_or_u64<'de, D>(d: D) -> Result<Option<u64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::Deserialize;
    #[derive(serde::Deserialize)]
    #[serde(untagged)]
    enum StrOrU64 {
        Str(String),
        Num(u64),
    }
    match Option::<StrOrU64>::deserialize(d)? {
        None => Ok(None),
        Some(StrOrU64::Num(n)) => Ok(Some(n)),
        Some(StrOrU64::Str(s)) => s
            .trim()
            .parse::<u64>()
            .map(Some)
            .map_err(serde::de::Error::custom),
    }
}

/// Response from POST /api/accounts/deviceauth/token (pending → returns authorization_code)
#[derive(serde::Deserialize)]
struct PollResp {
    authorization_code: Option<String>,
    code_verifier: Option<String>,
    error: Option<String>,
}

/// Response from POST /oauth/token (code exchange → access + refresh tokens)
#[derive(serde::Deserialize)]
struct TokenResp {
    access_token: String,
    refresh_token: String,
    expires_in: Option<u64>,
}

fn openai_client() -> anyhow::Result<reqwest::Client> {
    reqwest::Client::builder()
        .build()
        .context("build http client")
}

/// Step 1 — request a device_auth_id + user_code from OpenAI.
pub async fn start_device_flow() -> anyhow::Result<DeviceCodeResp> {
    let client = openai_client()?;
    let resp = client
        .post(DEVICE_USERCODE_URL)
        .header("Accept", "application/json")
        .json(&serde_json::json!({ "client_id": OAUTH_CLIENT_ID }))
        .send()
        .await
        .context("device usercode request")?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        if status.as_u16() == 403 || body.contains("Just a moment") {
            bail!("OpenAI OAuth endpoint blocked — falling back to API key login.");
        }
        bail!("OpenAI device code error {status}: {body}");
    }

    let mut data: DeviceCodeResp = resp.json().await.context("parse usercode response")?;
    if data.verification_uri.is_empty() {
        data.verification_uri = format!("{AUTH_BASE}/codex/device");
    }
    Ok(data)
}

/// Step 2 — poll until the user authorises; returns authorization_code + code_verifier.
async fn poll_for_auth_code(
    device_auth_id: &str,
    user_code: &str,
    interval_secs: u64,
    cancel: std::sync::Arc<tokio::sync::Notify>,
) -> anyhow::Result<(String, String)> {
    use std::time::Duration;
    let client = openai_client()?;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15 * 60);
    let mut interval = interval_secs.max(5);

    loop {
        if tokio::time::Instant::now() > deadline {
            bail!("OpenAI login timed out — try again");
        }
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_secs(interval)) => {}
            _ = cancel.notified() => bail!("Login cancelled"),
        }

        let resp = client
            .post(DEVICE_TOKEN_URL)
            .header("Accept", "application/json")
            .json(&serde_json::json!({
                "device_auth_id": device_auth_id,
                "user_code": user_code,
            }))
            .send()
            .await
            .context("poll token request")?;

        let status = resp.status().as_u16();
        // 403/404 while pending is expected — keep polling.
        if status == 403 || status == 404 {
            continue;
        }

        let data: PollResp = resp.json().await.context("parse poll response")?;

        if let Some(err) = &data.error {
            match err.as_str() {
                "authorization_pending" => continue,
                "slow_down" => {
                    interval += 5;
                    continue;
                }
                "expired_token" => bail!("Device code expired — run login again"),
                "access_denied" => bail!("Authorization denied"),
                other => bail!("OAuth poll error: {other}"),
            }
        }

        if let (Some(auth_code), Some(code_verifier)) =
            (data.authorization_code, data.code_verifier)
        {
            return Ok((auth_code, code_verifier));
        }
    }
}

/// Step 3 — exchange authorization_code for access_token + refresh_token.
async fn exchange_code(auth_code: &str, code_verifier: &str) -> anyhow::Result<OpenAiOAuth> {
    let client = openai_client()?;
    let body = format!(
        "grant_type=authorization_code&code={}&redirect_uri={}&client_id={}&code_verifier={}",
        urlencoding::encode(auth_code),
        urlencoding::encode(REDIRECT_URI),
        urlencoding::encode(OAUTH_CLIENT_ID),
        urlencoding::encode(code_verifier),
    );
    let resp = client
        .post(CODE_EXCHANGE_URL)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(body)
        .send()
        .await
        .context("code exchange request")?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        bail!("OpenAI code exchange failed {status}: {body}");
    }
    let data: TokenResp = resp.json().await.context("parse token response")?;
    let expires_at = data
        .expires_in
        .map(|s| chrono::Utc::now().timestamp() + s as i64);
    Ok(OpenAiOAuth {
        access_token: data.access_token,
        refresh_token: data.refresh_token,
        expires_at,
    })
}

/// Step 2+3 — poll for auth code then exchange it for OAuth tokens.
pub async fn poll_device_token(
    device_auth_id: &str,
    user_code: &str,
    interval_secs: u64,
    cancel: std::sync::Arc<tokio::sync::Notify>,
) -> anyhow::Result<OpenAiOAuth> {
    let (auth_code, code_verifier) =
        poll_for_auth_code(device_auth_id, user_code, interval_secs, cancel).await?;
    exchange_code(&auth_code, &code_verifier).await
}

/// Save OAuth tokens to auth store.
pub fn save_oauth(oauth: &OpenAiOAuth) -> anyhow::Result<()> {
    let mut store = AuthStore::load().unwrap_or_default();
    store.openai_oauth = Some(oauth.clone());
    store.save()
}

/// Legacy: save a plain API key.
pub fn save_api_key(key: &str) -> anyhow::Result<()> {
    let mut store = AuthStore::load().unwrap_or_default();
    store.openai = Some(ProviderAuth {
        token: key.to_string(),
        expires_at: None,
    });
    store.save()
}

/// Refresh an expired access token using the stored refresh token.
/// Saves the new tokens to disk and returns the fresh access token.
async fn refresh_access_token(oauth: &OpenAiOAuth) -> anyhow::Result<OpenAiOAuth> {
    let client = openai_client()?;
    let body = format!(
        "grant_type=refresh_token&refresh_token={}&client_id={}",
        urlencoding::encode(&oauth.refresh_token),
        urlencoding::encode(OAUTH_CLIENT_ID),
    );
    let resp = client
        .post(CODE_EXCHANGE_URL)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(body)
        .send()
        .await
        .context("refresh token request")?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        bail!("OpenAI token refresh failed {status}: {body}");
    }
    let data: TokenResp = resp.json().await.context("parse refresh response")?;
    let expires_at = data
        .expires_in
        .map(|s| chrono::Utc::now().timestamp() + s as i64);
    let fresh = OpenAiOAuth {
        access_token: data.access_token,
        refresh_token: data.refresh_token,
        expires_at,
    };
    save_oauth(&fresh)?;
    Ok(fresh)
}

// ── Provider ───────────────────────────────────────────────────────────────────

pub struct OpenAIProvider {
    pub model: String,
    token: String,
    base_url: String,
    client: reqwest::Client,
}

impl OpenAIProvider {
    pub fn from_auth() -> anyhow::Result<Self> {
        Self::from_auth_with_model(DEFAULT_MODEL)
    }

    pub fn from_auth_with_model(model: impl Into<String>) -> anyhow::Result<Self> {
        let store = AuthStore::load()?;

        // Prefer OAuth token over legacy API key.
        if let Some(oauth) = &store.openai_oauth {
            // Check if access token is still valid (with 60s buffer).
            let now = chrono::Utc::now().timestamp();
            let expired = oauth.expires_at.map(|exp| now >= exp - 60).unwrap_or(false);
            if !expired {
                return Ok(Self::new(oauth.access_token.clone(), model, API_BASE));
            }
            // Expired — refresh synchronously via a blocking call.
            // We're in a sync constructor so spawn a blocking refresh.
            let oauth = oauth.clone();
            let rt = tokio::runtime::Handle::try_current()
                .map(|h| {
                    let oauth = oauth.clone();
                    std::thread::spawn(move || h.block_on(refresh_access_token(&oauth)))
                        .join()
                        .ok()?
                        .ok()
                })
                .ok()
                .flatten();
            if let Some(fresh) = rt {
                return Ok(Self::new(fresh.access_token, model, API_BASE));
            }
            // Refresh failed — fall through to error below.
        }

        // Fall back to legacy API key.
        if let Some(auth) = store.openai {
            return Ok(Self::new(auth.token, model, API_BASE));
        }

        anyhow::bail!("Not logged in to OpenAI. Run: d-code login openai")
    }

    pub fn new(
        token: impl Into<String>,
        model: impl Into<String>,
        base_url: impl Into<String>,
    ) -> Self {
        Self {
            model: model.into(),
            token: token.into(),
            base_url: base_url.into().trim_end_matches('/').to_string(),
            client: reqwest::Client::builder()
                .pool_max_idle_per_host(4)
                .build()
                .expect("reqwest client"),
        }
    }
}

// ── Conversion helpers ─────────────────────────────────────────────────────────

fn parse_data_image_uri(s: &str) -> Option<(&str, &str)> {
    let rest = s.strip_prefix("data:")?;
    let (mime, rest) = rest.split_once(';')?;
    let data = rest.strip_prefix("base64,")?;
    if matches!(
        mime,
        "image/jpeg" | "image/png" | "image/gif" | "image/webp"
    ) {
        Some((mime, data))
    } else {
        None
    }
}

fn messages_to_oai(messages: &[Message]) -> Vec<serde_json::Value> {
    messages
        .iter()
        .flat_map(|m| {
            let role = match m.role {
                Role::User => "user",
                Role::Assistant => "assistant",
            };
            let mut out = vec![];
            let mut texts: Vec<&str> = vec![];
            let mut tool_calls: Vec<serde_json::Value> = vec![];

            for block in &m.content {
                match block {
                    ContentBlock::Text { text } => texts.push(text),
                    ContentBlock::ToolUse { id, name, input } => {
                        tool_calls.push(serde_json::json!({
                            "id": id,
                            "type": "function",
                            "function": {
                                "name": name,
                                "arguments": input.to_string(),
                            }
                        }));
                    }
                    ContentBlock::ToolResult {
                        tool_use_id,
                        content,
                        ..
                    } => {
                        let content_val = if let Some((mime, data)) = parse_data_image_uri(content)
                        {
                            serde_json::json!([{
                                "type": "image_url",
                                "image_url": { "url": format!("data:{mime};base64,{data}") }
                            }])
                        } else {
                            serde_json::json!(content)
                        };
                        out.push(serde_json::json!({
                            "role": "tool",
                            "tool_call_id": tool_use_id,
                            "content": content_val,
                        }));
                    }
                }
            }

            let mut msg = serde_json::json!({"role": role});
            if !texts.is_empty() {
                msg["content"] = serde_json::json!(texts.join("\n"));
            }
            if !tool_calls.is_empty() {
                msg["tool_calls"] = serde_json::json!(tool_calls);
            }
            let mut result = vec![];
            if !texts.is_empty() || !tool_calls.is_empty() {
                result.push(msg);
            }
            result.extend(out);
            result
        })
        .collect()
}

fn tools_to_oai(tools: &[ToolDef]) -> Vec<serde_json::Value> {
    tools
        .iter()
        .map(|t| {
            serde_json::json!({
                "type": "function",
                "function": {
                    "name": t.name,
                    "description": t.description,
                    "parameters": t.input_schema,
                }
            })
        })
        .collect()
}

#[async_trait]
impl Provider for OpenAIProvider {
    fn name(&self) -> &str {
        "openai"
    }
    fn model(&self) -> &str {
        &self.model
    }
    fn context_window(&self) -> u32 {
        CONTEXT_WINDOW
    }

    async fn list_models(&self) -> Vec<String> {
        #[derive(serde::Deserialize)]
        struct ModelObj {
            id: String,
        }
        #[derive(serde::Deserialize)]
        struct ModelList {
            data: Vec<ModelObj>,
        }

        let url = format!("{}/v1/models", self.base_url);
        let Ok(resp) = self.client.get(&url).bearer_auth(&self.token).send().await else {
            return SUPPORTED_MODELS.iter().map(|s| s.to_string()).collect();
        };
        let Ok(list) = resp.json::<ModelList>().await else {
            return SUPPORTED_MODELS.iter().map(|s| s.to_string()).collect();
        };
        let mut ids: Vec<String> = list
            .data
            .into_iter()
            .map(|m| m.id)
            .filter(|id| {
                id.starts_with("gpt-")
                    || id.starts_with("o1")
                    || id.starts_with("o3")
                    || id.starts_with("o4")
            })
            .collect();
        ids.sort();
        if ids.is_empty() {
            SUPPORTED_MODELS.iter().map(|s| s.to_string()).collect()
        } else {
            ids
        }
    }

    async fn chat_stream(
        &self,
        system: &str,
        messages: &[Message],
        tools: &[ToolDef],
        max_tokens: u32,
    ) -> anyhow::Result<Pin<Box<dyn Stream<Item = anyhow::Result<StreamEvent>> + Send>>> {
        let url = format!("{}/v1/chat/completions", self.base_url);

        let mut api_messages = vec![serde_json::json!({"role":"system","content":system})];
        api_messages.extend(messages_to_oai(messages));

        let mut body = serde_json::json!({
            "model": self.model,
            "max_tokens": max_tokens,
            "stream": true,
            "stream_options": {"include_usage": true},
            "messages": api_messages,
        });
        if !tools.is_empty() {
            body["tools"] = serde_json::json!(tools_to_oai(tools));
            body["tool_choice"] = serde_json::json!("auto");
        }

        let resp = self
            .client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.token))
            .header("User-Agent", USER_AGENT)
            .json(&body)
            .send()
            .await
            .context("OpenAI API request")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            bail!("OpenAI API error {status}: {text}");
        }

        let stream = parse_oai_sse(resp);
        Ok(Box::pin(stream))
    }
}

// ── SSE parser (shared with Copilot) ──────────────────────────────────────────

fn parse_oai_sse(
    resp: reqwest::Response,
) -> impl Stream<Item = anyhow::Result<StreamEvent>> + Send {
    use futures::StreamExt;

    let byte_stream = resp.bytes_stream();

    async_stream::stream! {
        use futures::pin_mut;
        pin_mut!(byte_stream);

        let mut buf = String::new();
        let mut tool_calls: std::collections::HashMap<u32, (String, String, String)> =
            std::collections::HashMap::new();
        let mut stop_reason: Option<String> = None;

        while let Some(chunk) = byte_stream.next().await {
            let chunk = chunk.context("SSE read error")?;
            let text = std::str::from_utf8(&chunk).context("SSE UTF-8")?;
            buf.push_str(text);

            while let Some(pos) = buf.find('\n') {
                let line = buf[..pos].trim_end_matches('\r').to_string();
                buf.drain(..=pos);

                if let Some(data) = line.strip_prefix("data: ") {
                    if data == "[DONE]" {
                        break;
                    }
                    if let Ok(v) = serde_json::from_str::<serde_json::Value>(data) {
                        for ev in translate_oai_chunk(&v, &mut tool_calls, &mut stop_reason) {
                            yield Ok(ev);
                        }
                    }
                }
            }
        }

        // Flush pending tool calls.
        let mut calls: Vec<_> = tool_calls.into_iter().collect();
        calls.sort_by_key(|(idx, _)| *idx);
        for (_, (id, name, args)) in calls {
            yield Ok(StreamEvent::ToolUseStart { id, name });
            yield Ok(StreamEvent::ToolUseDelta(args));
            yield Ok(StreamEvent::ToolUseEnd);
        }

        let reason = stop_reason
            .as_deref()
            .map(StopReason::parse)
            .unwrap_or(StopReason::EndTurn);
        yield Ok(StreamEvent::Done { stop_reason: reason });
    }
}

fn translate_oai_chunk(
    v: &serde_json::Value,
    tool_calls: &mut std::collections::HashMap<u32, (String, String, String)>,
    stop_reason: &mut Option<String>,
) -> Vec<StreamEvent> {
    let mut out = vec![];
    if let Some(choices) = v["choices"].as_array() {
        for choice in choices {
            let delta = &choice["delta"];
            if let Some(text) = delta["content"].as_str() {
                if !text.is_empty() {
                    out.push(StreamEvent::TextDelta(text.to_string()));
                }
            }
            if let Some(calls) = delta["tool_calls"].as_array() {
                for call in calls {
                    let idx = call["index"].as_u64().unwrap_or(0) as u32;
                    let entry = tool_calls.entry(idx).or_insert_with(|| {
                        (
                            call["id"].as_str().unwrap_or("").to_string(),
                            call["function"]["name"].as_str().unwrap_or("").to_string(),
                            String::new(),
                        )
                    });
                    if let Some(args) = call["function"]["arguments"].as_str() {
                        entry.2.push_str(args);
                    }
                }
            }
            if let Some(reason) = choice["finish_reason"].as_str() {
                if !reason.is_empty() && reason != "null" {
                    *stop_reason = Some(reason.to_string());
                }
            }
        }
    }
    if let Some(usage) = v.get("usage").filter(|u| !u.is_null()) {
        out.push(StreamEvent::Usage {
            input_tokens: usage["prompt_tokens"].as_u64().unwrap_or(0) as u32,
            output_tokens: usage["completion_tokens"].as_u64().unwrap_or(0) as u32,
        });
    }
    out
}
