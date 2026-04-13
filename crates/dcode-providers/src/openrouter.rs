/// OpenRouter provider: API key auth + OpenAI-compatible API.
/// Gives access to 100+ models from Anthropic, OpenAI, Google, Meta, Mistral, etc.
use std::pin::Pin;

use anyhow::{bail, Context};
use async_trait::async_trait;
use futures::Stream;

use crate::provider::Provider;
use crate::types::{
    AuthStore, ContentBlock, Message, ProviderAuth, Role, StopReason, StreamEvent, ToolDef,
};

const API_BASE: &str = "https://openrouter.ai/api/v1";
const USER_AGENT: &str = "d-code/0.1";
const HTTP_REFERER: &str = "https://github.com/ddhanush1/d-code";

pub const DEFAULT_MODEL: &str = "deepseek/deepseek-chat-v3-0324";
pub const SUPPORTED_MODELS: &[&str] = &[
    // DeepSeek
    "deepseek/deepseek-chat-v3-0324",
    "deepseek/deepseek-r1-0528",
    // Meta Llama
    "meta-llama/llama-3.3-70b-instruct",
    "meta-llama/llama-3.1-405b-instruct",
    // Google
    "google/gemini-2.0-flash-exp:free",
    "google/gemini-2.5-flash-preview-05-20",
    "google/gemini-2.5-pro-preview-06-05",
    // Anthropic
    "anthropic/claude-opus-4",
    "anthropic/claude-sonnet-4-5",
    "anthropic/claude-3.5-sonnet",
    // OpenAI
    "openai/gpt-4.1",
    "openai/gpt-4o",
    "openai/o3",
    // Mistral
    "mistralai/mistral-large",
    "mistralai/codestral-latest",
    // Qwen
    "qwen/qwen-2.5-coder-32b-instruct",
    "qwen/qwen3-235b-a22b",
];
pub const CONTEXT_WINDOW: u32 = 128_000;

pub fn save_api_key(key: &str) -> anyhow::Result<()> {
    let mut store = AuthStore::load().unwrap_or_default();
    store.openrouter = Some(ProviderAuth {
        token: key.to_string(),
        expires_at: None,
    });
    store.save()
}

pub struct OpenRouterProvider {
    pub model: String,
    token: String,
    client: reqwest::Client,
}

impl OpenRouterProvider {
    pub fn from_auth_with_model(model: impl Into<String>) -> anyhow::Result<Self> {
        let model = model.into();
        // Env var takes priority.
        if let Ok(key) = std::env::var("OPENROUTER_API_KEY") {
            if !key.is_empty() {
                return Ok(Self::new(key, model));
            }
        }
        let store = AuthStore::load()?;
        if let Some(auth) = store.openrouter {
            return Ok(Self::new(auth.token, model));
        }
        bail!("Not logged in to OpenRouter. Run: d-code login openrouter  (or set OPENROUTER_API_KEY)")
    }

    pub fn new(token: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            token: token.into(),
            client: reqwest::Client::builder()
                .pool_max_idle_per_host(4)
                .build()
                .expect("reqwest client"),
        }
    }
}

// ── Reuse OpenAI-compatible conversion helpers ─────────────────────────────────

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
impl Provider for OpenRouterProvider {
    fn name(&self) -> &str {
        "openrouter"
    }
    fn model(&self) -> &str {
        &self.model
    }
    fn context_window(&self) -> u32 {
        CONTEXT_WINDOW
    }

    async fn list_models(&self) -> Vec<String> {
        // Try fetching from API.
        #[derive(serde::Deserialize)]
        struct ModelObj {
            id: String,
        }
        #[derive(serde::Deserialize)]
        struct ModelList {
            data: Vec<ModelObj>,
        }
        let url = format!("{API_BASE}/models");
        let Ok(resp) = self.client.get(&url).bearer_auth(&self.token).send().await else {
            return SUPPORTED_MODELS.iter().map(|s| s.to_string()).collect();
        };
        let Ok(list) = resp.json::<ModelList>().await else {
            return SUPPORTED_MODELS.iter().map(|s| s.to_string()).collect();
        };
        let mut ids: Vec<String> = list.data.into_iter().map(|m| m.id).collect();
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
        let url = format!("{API_BASE}/chat/completions");

        let mut api_messages = vec![serde_json::json!({"role": "system", "content": system})];
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
            .header("HTTP-Referer", HTTP_REFERER)
            .header("X-Title", "d-code")
            .json(&body)
            .send()
            .await
            .context("OpenRouter API request")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            bail!("OpenRouter API error {status}: {text}");
        }

        Ok(Box::pin(parse_oai_sse(resp)))
    }
}

// ── SSE parser (OpenAI-compatible) ─────────────────────────────────────────────

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
                    // Update name if it comes in a later chunk.
                    if entry.1.is_empty() {
                        if let Some(n) = call["function"]["name"].as_str() {
                            if !n.is_empty() {
                                entry.1 = n.to_string();
                            }
                        }
                    }
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
            cache_write_tokens: 0,
            cache_read_tokens: 0,
        });
    }
    out
}
