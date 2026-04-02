/// OpenAI provider: API key login + chat completions streaming.
use std::pin::Pin;

use anyhow::{bail, Context};
use async_trait::async_trait;
use futures::Stream;

use crate::provider::Provider;
use crate::types::{AuthStore, ContentBlock, Message, ProviderAuth, Role, StreamEvent, StopReason, ToolDef};

const API_BASE: &str = "https://api.openai.com";
const USER_AGENT: &str = "d-code/0.1";

pub const DEFAULT_MODEL: &str = "gpt-4o";
pub const SUPPORTED_MODELS: &[&str] = &[
    "gpt-4o",
    "gpt-4.1",
    "gpt-4.1-mini",
    "o3",
    "o3-mini",
];
pub const CONTEXT_WINDOW: u32 = 128_000;

// ── Login ──────────────────────────────────────────────────────────────────────

pub fn save_api_key(key: &str) -> anyhow::Result<()> {
    let mut store = AuthStore::load().unwrap_or_default();
    store.openai = Some(ProviderAuth { token: key.to_string(), expires_at: None });
    store.save()
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
        let auth = store.openai.ok_or_else(|| {
            anyhow::anyhow!("Not logged in to OpenAI. Run: d-code login openai")
        })?;
        Ok(Self::new(auth.token, model, API_BASE))
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
                    ContentBlock::ToolResult { tool_use_id, content, .. } => {
                        out.push(serde_json::json!({
                            "role": "tool",
                            "tool_call_id": tool_use_id,
                            "content": content,
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
            let mut result = vec![msg];
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
            .map(StopReason::from_str)
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
