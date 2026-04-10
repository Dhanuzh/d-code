/// GitHub Copilot provider: device-code login + Copilot chat completions API.
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context};
use async_trait::async_trait;
use futures::Stream;
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

use crate::provider::Provider;
use crate::types::{
    AuthStore, ContentBlock, CopilotAuth, Message, Role, StopReason, StreamEvent, ToolDef,
};

// ── OAuth constants ────────────────────────────────────────────────────────────

const CLIENT_ID: &str = "Ov23li8tweQw6odWQebz";
const DEVICE_CODE_URL: &str = "https://github.com/login/device/code";
const ACCESS_TOKEN_URL: &str = "https://github.com/login/oauth/access_token";
const SCOPE: &str = "read:user";

const POLL_SAFETY_MARGIN: Duration = Duration::from_secs(3);
const MAX_POLL: Duration = Duration::from_secs(15 * 60);

// ── Copilot API constants ──────────────────────────────────────────────────────

const COPILOT_TOKEN_URL: &str = "https://api.github.com/copilot_internal/v2/token";
const COPILOT_CHAT_URL: &str = "https://api.githubcopilot.com/chat/completions";
const GITHUB_USER_AGENT: &str = "GitHubCopilotChat/0.22.4";
const COPILOT_INTEGRATION: &str = "vscode-chat";
const EDITOR_VERSION: &str = "vscode/1.90.0";
const EDITOR_PLUGIN_VERSION: &str = "copilot-chat/0.22.4";

pub const DEFAULT_MODEL: &str = "gpt-4o-mini";
pub const SUPPORTED_MODELS: &[&str] = &[
    "gpt-4o",
    "gpt-4.1",
    "gpt-4o-mini",
    "gpt-5-mini",
    "claude-sonnet-4",
    "claude-sonnet-4.5",
    "gemini-2.5-pro",
];
pub const CONTEXT_WINDOW: u32 = 128_000;

// ── Device code flow ───────────────────────────────────────────────────────────

pub struct DeviceCodeStart {
    pub user_code: String,
    pub verification_uri: String,
    pub device_code: String,
    pub interval: u64,
}

#[derive(Deserialize)]
struct DeviceCodeResp {
    device_code: String,
    user_code: String,
    verification_uri: String,
    interval: Option<u64>,
}

#[derive(Deserialize)]
struct TokenResp {
    access_token: Option<String>,
    error: Option<String>,
    interval: Option<u64>,
}

pub async fn start_device_flow() -> anyhow::Result<DeviceCodeStart> {
    let client = reqwest::Client::new();
    let resp = client
        .post(DEVICE_CODE_URL)
        .header("Accept", "application/json")
        .json(&serde_json::json!({"client_id": CLIENT_ID, "scope": SCOPE}))
        .send()
        .await
        .context("device code request")?;

    if !resp.status().is_success() {
        bail!("GitHub device code error: {}", resp.status());
    }
    let data: DeviceCodeResp = resp.json().await.context("parse device code response")?;
    Ok(DeviceCodeStart {
        user_code: data.user_code,
        verification_uri: data.verification_uri,
        device_code: data.device_code,
        interval: data.interval.unwrap_or(5),
    })
}

pub async fn poll_github_token(
    device_code: &str,
    interval_secs: u64,
    cancel: Arc<tokio::sync::Notify>,
) -> anyhow::Result<String> {
    let client = reqwest::Client::new();
    let deadline = tokio::time::Instant::now() + MAX_POLL;
    let mut secs = interval_secs;

    loop {
        let sleep = Duration::from_secs(secs) + POLL_SAFETY_MARGIN;
        tokio::select! {
            _ = tokio::time::sleep(sleep) => {}
            _ = cancel.notified() => bail!("Copilot login cancelled"),
        }
        if tokio::time::Instant::now() >= deadline {
            bail!("Copilot device auth timed out");
        }

        let resp = client
            .post(ACCESS_TOKEN_URL)
            .header("Accept", "application/json")
            .json(&serde_json::json!({
                "client_id": CLIENT_ID,
                "device_code": device_code,
                "grant_type": "urn:ietf:params:oauth:grant-type:device_code",
            }))
            .send()
            .await
            .context("token poll request")?;

        if !resp.status().is_success() {
            bail!("GitHub token poll error: {}", resp.status());
        }

        let data: TokenResp = resp.json().await.context("parse token poll")?;

        if let Some(token) = data.access_token {
            return Ok(token);
        }
        match data.error.as_deref() {
            Some("authorization_pending") => {
                if let Some(i) = data.interval {
                    secs = i;
                }
            }
            Some("slow_down") => secs += 5,
            Some(err) => bail!("GitHub auth error: {err}"),
            None => bail!("unexpected empty GitHub response"),
        }
    }
}

pub fn save_github_token(github_token: &str) -> anyhow::Result<()> {
    let mut store = AuthStore::load().unwrap_or_default();
    store.copilot = Some(CopilotAuth {
        github_token: github_token.to_string(),
        copilot_token: None,
        copilot_expires_at: None,
    });
    store.save()
}

// ── Copilot token exchange ─────────────────────────────────────────────────────

#[derive(Deserialize)]
struct CopilotTokenResp {
    token: String,
    expires_at: Option<i64>,
}

async fn get_copilot_token(github_token: &str) -> anyhow::Result<(String, Option<i64>)> {
    let client = reqwest::Client::new();
    let resp = client
        .get(COPILOT_TOKEN_URL)
        .header("Authorization", format!("token {github_token}"))
        .header("User-Agent", GITHUB_USER_AGENT)
        .header("Editor-Version", EDITOR_VERSION)
        .header("Editor-Plugin-Version", EDITOR_PLUGIN_VERSION)
        .header("Accept", "application/json")
        .send()
        .await
        .context("Copilot token request")?;

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        bail!("Copilot token error {status}: {text}");
    }
    let r: CopilotTokenResp = resp.json().await.context("parse copilot token")?;
    Ok((r.token, r.expires_at))
}

// ── Provider implementation ────────────────────────────────────────────────────

pub struct CopilotProvider {
    pub model: String,
    github_token: String,
    copilot_token: Arc<RwLock<Option<(String, i64)>>>,
    client: reqwest::Client,
}

impl CopilotProvider {
    pub fn from_auth() -> anyhow::Result<Self> {
        Self::from_auth_with_model(DEFAULT_MODEL)
    }

    pub fn from_auth_with_model(model: impl Into<String>) -> anyhow::Result<Self> {
        let store = AuthStore::load()?;
        let auth = store.copilot.ok_or_else(|| {
            anyhow::anyhow!("Not logged in to GitHub Copilot. Run: d-code login copilot")
        })?;
        Ok(Self::new(auth.github_token, model))
    }

    pub fn new(github_token: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            github_token: github_token.into(),
            copilot_token: Arc::new(RwLock::new(None)),
            client: reqwest::Client::builder()
                .pool_max_idle_per_host(4)
                .build()
                .expect("reqwest client"),
        }
    }

    async fn fresh_copilot_token(&self) -> anyhow::Result<String> {
        // Check cached token.
        {
            let guard = self.copilot_token.read().await;
            if let Some((token, exp)) = guard.as_ref() {
                let now = chrono::Utc::now().timestamp();
                if *exp > now + 60 {
                    return Ok(token.clone());
                }
            }
        }
        // Fetch a new one.
        let (token, exp) = match get_copilot_token(&self.github_token).await {
            Ok(v) => v,
            Err(e) => {
                let msg = e.to_string();
                if msg.contains("404 Not Found") || msg.contains("\"status\":\"404\"") {
                    (
                        self.github_token.clone(),
                        Some(chrono::Utc::now().timestamp() + 3600),
                    )
                } else {
                    return Err(e);
                }
            }
        };
        let exp_ts = exp.unwrap_or_else(|| chrono::Utc::now().timestamp() + 1800);
        *self.copilot_token.write().await = Some((token.clone(), exp_ts));
        Ok(token)
    }
}

// ── OpenAI-compatible message conversion ──────────────────────────────────────

#[allow(dead_code)]
#[derive(Serialize)]
struct OAIMessage {
    role: &'static str,
    content: serde_json::Value,
}

#[allow(dead_code)]
#[derive(Serialize)]
struct OAITool {
    #[serde(rename = "type")]
    kind: &'static str,
    function: OAIFunction,
}

#[allow(dead_code)]
#[derive(Serialize)]
struct OAIFunction {
    name: String,
    description: String,
    parameters: serde_json::Value,
}

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
            // Each message may have multiple content blocks; flatten where needed.
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

// ── Streaming SSE parser (OpenAI format) ──────────────────────────────────────

#[async_trait]
impl Provider for CopilotProvider {
    fn name(&self) -> &str {
        "copilot"
    }
    fn model(&self) -> &str {
        &self.model
    }
    fn context_window(&self) -> u32 {
        CONTEXT_WINDOW
    }

    async fn list_models(&self) -> Vec<String> {
        #[derive(serde::Deserialize)]
        struct Capabilities {
            #[serde(rename = "type")]
            kind: Option<String>,
        }
        #[derive(serde::Deserialize)]
        struct ModelObj {
            id: String,
            capabilities: Option<Capabilities>,
        }
        #[derive(serde::Deserialize)]
        struct ModelList {
            data: Vec<ModelObj>,
        }

        let token = match self.fresh_copilot_token().await {
            Ok(t) => t,
            Err(_) => return SUPPORTED_MODELS.iter().map(|s| s.to_string()).collect(),
        };
        let Ok(resp) = self
            .client
            .get("https://api.githubcopilot.com/models")
            .bearer_auth(&token)
            .header("Copilot-Integration-Id", "vscode-chat")
            .send()
            .await
        else {
            return SUPPORTED_MODELS.iter().map(|s| s.to_string()).collect();
        };
        let Ok(list) = resp.json::<ModelList>().await else {
            return SUPPORTED_MODELS.iter().map(|s| s.to_string()).collect();
        };
        // Only include models that explicitly support chat completions.
        // Exclude models with no capability info or capabilities.type != "chat"
        // (e.g. gpt-5.3-codex is "completions"-only and fails on /chat/completions).
        let mut ids: Vec<String> = list
            .data
            .into_iter()
            .filter(|m| {
                m.capabilities
                    .as_ref()
                    .and_then(|c| c.kind.as_deref())
                    .map(|k| k == "chat")
                    .unwrap_or(false) // exclude if capability info is missing
            })
            .map(|m| m.id)
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
        let copilot_token = self.fresh_copilot_token().await?;

        let mut api_messages = vec![serde_json::json!({"role":"system","content":system})];
        api_messages.extend(messages_to_oai(messages));

        let mut body = serde_json::json!({
            "model": self.model,
            "max_tokens": max_tokens,
            "stream": true,
            "messages": api_messages,
        });
        if !tools.is_empty() {
            body["tools"] = serde_json::json!(tools_to_oai(tools));
            body["tool_choice"] = serde_json::json!("auto");
        }

        let resp = self
            .client
            .post(COPILOT_CHAT_URL)
            .header("Authorization", format!("Bearer {copilot_token}"))
            .header("Copilot-Integration-Id", COPILOT_INTEGRATION)
            .header("User-Agent", GITHUB_USER_AGENT)
            .header("Editor-Version", EDITOR_VERSION)
            .header("Editor-Plugin-Version", EDITOR_PLUGIN_VERSION)
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .context("Copilot API request")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            bail!("Copilot API error {status}: {text}");
        }

        let stream = parse_oai_sse(resp);
        Ok(Box::pin(stream))
    }
}

fn parse_oai_sse(
    resp: reqwest::Response,
) -> impl Stream<Item = anyhow::Result<StreamEvent>> + Send {
    use futures::StreamExt;

    let byte_stream = resp.bytes_stream();

    async_stream::stream! {
        use futures::pin_mut;
        pin_mut!(byte_stream);

        let mut buf = String::new();
        // Accumulate tool call arguments per index.
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

        // Emit any pending tool calls.
        for (id, name, args) in tool_calls.values() {
            yield Ok(StreamEvent::ToolUseStart { id: id.clone(), name: name.clone() });
            yield Ok(StreamEvent::ToolUseDelta(args.clone()));
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
    let choices = match v["choices"].as_array() {
        Some(c) => c,
        None => return out,
    };
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
                    let id = call["id"].as_str().unwrap_or("").to_string();
                    let name = call["function"]["name"].as_str().unwrap_or("").to_string();
                    (id, name, String::new())
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

    // Usage data.
    if let Some(usage) = v.get("usage") {
        if !usage.is_null() {
            out.push(StreamEvent::Usage {
                input_tokens: usage["prompt_tokens"].as_u64().unwrap_or(0) as u32,
                output_tokens: usage["completion_tokens"].as_u64().unwrap_or(0) as u32,
            });
        }
    }

    out
}
