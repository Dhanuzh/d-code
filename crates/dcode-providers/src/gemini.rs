/// Google Gemini provider: API key auth + Gemini streaming API.
use std::collections::HashMap;
use std::pin::Pin;

use anyhow::{bail, Context};
use async_trait::async_trait;
use futures::Stream;

use crate::provider::Provider;
use crate::types::{
    AuthStore, ContentBlock, Message, ProviderAuth, Role, StopReason, StreamEvent, ToolDef,
};

const API_BASE: &str = "https://generativelanguage.googleapis.com/v1beta";

pub const DEFAULT_MODEL: &str = "gemini-2.0-flash";
pub const SUPPORTED_MODELS: &[&str] = &[
    "gemini-2.0-flash",
    "gemini-2.0-flash-lite",
    "gemini-2.5-flash-preview-05-20",
    "gemini-2.5-pro-preview-06-05",
    "gemini-1.5-flash",
    "gemini-1.5-pro",
];
pub const CONTEXT_WINDOW: u32 = 1_000_000;

pub fn save_api_key(key: &str) -> anyhow::Result<()> {
    let mut store = AuthStore::load().unwrap_or_default();
    store.gemini = Some(ProviderAuth {
        token: key.to_string(),
        expires_at: None,
    });
    store.save()
}

pub struct GeminiProvider {
    pub model: String,
    api_key: String,
    client: reqwest::Client,
}

impl GeminiProvider {
    pub fn from_auth_with_model(model: impl Into<String>) -> anyhow::Result<Self> {
        let model = model.into();
        // Env var takes priority.
        if let Ok(key) = std::env::var("GEMINI_API_KEY") {
            if !key.is_empty() {
                return Ok(Self::new(key, model));
            }
        }
        let store = AuthStore::load()?;
        if let Some(auth) = store.gemini {
            return Ok(Self::new(auth.token, model));
        }
        bail!("Not logged in to Gemini. Run: d-code login gemini  (or set GEMINI_API_KEY)")
    }

    pub fn new(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            api_key: api_key.into(),
            client: reqwest::Client::builder()
                .pool_max_idle_per_host(4)
                .build()
                .expect("reqwest client"),
        }
    }
}

// ── Message conversion ─────────────────────────────────────────────────────────

/// Build a map from tool_use_id → tool_name by scanning assistant messages.
fn build_tool_id_to_name(messages: &[Message]) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for msg in messages {
        if msg.role == Role::Assistant {
            for block in &msg.content {
                if let ContentBlock::ToolUse { id, name, .. } = block {
                    map.insert(id.clone(), name.clone());
                }
            }
        }
    }
    map
}

fn messages_to_gemini(messages: &[Message]) -> Vec<serde_json::Value> {
    let tool_id_to_name = build_tool_id_to_name(messages);
    let mut result = vec![];

    for msg in messages {
        match msg.role {
            Role::User => {
                let mut text_parts: Vec<serde_json::Value> = vec![];
                let mut fn_parts: Vec<serde_json::Value> = vec![];

                for block in &msg.content {
                    match block {
                        ContentBlock::Text { text } => {
                            let t = text.trim();
                            if !t.is_empty() {
                                text_parts.push(serde_json::json!({"text": t}));
                            }
                        }
                        ContentBlock::ToolResult {
                            tool_use_id,
                            content,
                            ..
                        } => {
                            let fn_name = tool_id_to_name
                                .get(tool_use_id)
                                .map(|s| s.as_str())
                                .unwrap_or(tool_use_id.as_str());
                            fn_parts.push(serde_json::json!({
                                "functionResponse": {
                                    "name": fn_name,
                                    "response": {"content": content}
                                }
                            }));
                        }
                        ContentBlock::ToolUse { .. } => {}
                    }
                }

                // Emit text message if any
                if !text_parts.is_empty() {
                    result.push(serde_json::json!({"role": "user", "parts": text_parts}));
                }
                // Emit function responses as separate user turn
                if !fn_parts.is_empty() {
                    result.push(serde_json::json!({"role": "user", "parts": fn_parts}));
                }
            }
            Role::Assistant => {
                let mut parts: Vec<serde_json::Value> = vec![];
                for block in &msg.content {
                    match block {
                        ContentBlock::Text { text } => {
                            let t = text.trim();
                            if !t.is_empty() {
                                parts.push(serde_json::json!({"text": t}));
                            }
                        }
                        ContentBlock::ToolUse { name, input, .. } => {
                            parts.push(serde_json::json!({
                                "functionCall": {"name": name, "args": input}
                            }));
                        }
                        ContentBlock::ToolResult { .. } => {}
                    }
                }
                if !parts.is_empty() {
                    result.push(serde_json::json!({"role": "model", "parts": parts}));
                }
            }
        }
    }

    // Gemini requires alternating user/model turns.
    // Merge consecutive same-role contents to satisfy this constraint.
    deduplicate_consecutive_same_role(result)
}

/// Merge consecutive messages with the same role by combining their parts.
fn deduplicate_consecutive_same_role(contents: Vec<serde_json::Value>) -> Vec<serde_json::Value> {
    let mut out: Vec<serde_json::Value> = vec![];
    for content in contents {
        let role = content["role"].as_str().unwrap_or("").to_string();
        if let Some(last) = out.last_mut() {
            let last_role = last["role"].as_str().unwrap_or("").to_string();
            if last_role == role {
                // Merge parts
                if let (Some(last_parts), Some(new_parts)) =
                    (last["parts"].as_array_mut(), content["parts"].as_array())
                {
                    last_parts.extend(new_parts.iter().cloned());
                }
                continue;
            }
        }
        out.push(content);
    }
    out
}

fn tools_to_gemini(tools: &[ToolDef]) -> serde_json::Value {
    let declarations: Vec<serde_json::Value> = tools
        .iter()
        .map(|t| {
            serde_json::json!({
                "name": t.name,
                "description": t.description,
                "parameters": t.input_schema,
            })
        })
        .collect();
    serde_json::json!([{"function_declarations": declarations}])
}

// ── Provider ───────────────────────────────────────────────────────────────────

#[async_trait]
impl Provider for GeminiProvider {
    fn name(&self) -> &str {
        "gemini"
    }
    fn model(&self) -> &str {
        &self.model
    }
    fn context_window(&self) -> u32 {
        CONTEXT_WINDOW
    }

    async fn list_models(&self) -> Vec<String> {
        SUPPORTED_MODELS.iter().map(|s| s.to_string()).collect()
    }

    async fn chat_stream(
        &self,
        system: &str,
        messages: &[Message],
        tools: &[ToolDef],
        max_tokens: u32,
    ) -> anyhow::Result<Pin<Box<dyn Stream<Item = anyhow::Result<StreamEvent>> + Send>>> {
        let url = format!(
            "{}/models/{}:streamGenerateContent?alt=sse&key={}",
            API_BASE, self.model, self.api_key
        );

        let contents = messages_to_gemini(messages);

        let mut body = serde_json::json!({
            "system_instruction": {"parts": [{"text": system}]},
            "contents": contents,
            "generationConfig": {
                "maxOutputTokens": max_tokens,
            }
        });

        if !tools.is_empty() {
            body["tools"] = tools_to_gemini(tools);
            body["tool_config"] = serde_json::json!({
                "function_calling_config": {"mode": "AUTO"}
            });
        }

        let resp = self
            .client
            .post(&url)
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .context("Gemini API request")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            bail!("Gemini API error {status}: {text}");
        }

        Ok(Box::pin(parse_gemini_sse(resp)))
    }
}

// ── SSE parser ─────────────────────────────────────────────────────────────────

fn parse_gemini_sse(
    resp: reqwest::Response,
) -> impl Stream<Item = anyhow::Result<StreamEvent>> + Send {
    use futures::StreamExt;

    let byte_stream = resp.bytes_stream();

    async_stream::stream! {
        use futures::pin_mut;
        pin_mut!(byte_stream);

        let mut buf = String::new();
        // Track pending function calls by name → (generated_id, args_json)
        let mut pending_calls: Vec<(String, String, serde_json::Value)> = vec![]; // (id, name, args)
        let mut stop_reason = StopReason::EndTurn;
        let mut usage_sent = false;

        while let Some(chunk) = byte_stream.next().await {
            let chunk = chunk.context("Gemini SSE read error")?;
            let text = std::str::from_utf8(&chunk).context("Gemini SSE UTF-8")?;
            buf.push_str(text);

            while let Some(pos) = buf.find('\n') {
                let line = buf[..pos].trim_end_matches('\r').to_string();
                buf.drain(..=pos);

                if let Some(data) = line.strip_prefix("data: ") {
                    if let Ok(v) = serde_json::from_str::<serde_json::Value>(data) {
                        // Extract usage metadata
                        if let Some(usage) = v.get("usageMetadata") {
                            let input = usage["promptTokenCount"].as_u64().unwrap_or(0) as u32;
                            let output = usage["candidatesTokenCount"].as_u64().unwrap_or(0) as u32;
                            if !usage_sent && (input > 0 || output > 0) {
                                yield Ok(StreamEvent::Usage { input_tokens: input, output_tokens: output, cache_write_tokens: 0, cache_read_tokens: 0 });
                                usage_sent = true;
                            }
                        }

                        // Extract candidates
                        if let Some(candidates) = v["candidates"].as_array() {
                            for candidate in candidates {
                                // Finish reason
                                if let Some(reason) = candidate["finishReason"].as_str() {
                                    stop_reason = match reason {
                                        "STOP" => StopReason::EndTurn,
                                        "FUNCTION_CALL" | "TOOL_CALLS" => StopReason::ToolUse,
                                        "MAX_TOKENS" => StopReason::MaxTokens,
                                        other => StopReason::Other(other.to_string()),
                                    };
                                }

                                // Content parts
                                if let Some(parts) = candidate["content"]["parts"].as_array() {
                                    for part in parts {
                                        if let Some(text) = part["text"].as_str() {
                                            if !text.is_empty() {
                                                yield Ok(StreamEvent::TextDelta(text.to_string()));
                                            }
                                        }
                                        if let Some(fc) = part.get("functionCall") {
                                            let name = fc["name"].as_str().unwrap_or("").to_string();
                                            let args = fc["args"].clone();
                                            let id = format!("gemini_{}", uuid_v4_short());
                                            pending_calls.push((id, name, args));
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        // Emit pending tool calls after stream ends.
        for (id, name, args) in pending_calls {
            let args_str = args.to_string();
            yield Ok(StreamEvent::ToolUseStart { id, name });
            yield Ok(StreamEvent::ToolUseDelta(args_str));
            yield Ok(StreamEvent::ToolUseEnd);
        }

        yield Ok(StreamEvent::Done { stop_reason });
    }
}

/// Generate a short unique ID (not RFC 4122, just unique enough).
fn uuid_v4_short() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let t = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos();
    format!("{:08x}", t ^ (rand_u32()))
}

fn rand_u32() -> u32 {
    // Simple LCG using stack address as seed.
    let x: u64 = 0;
    let addr = &x as *const u64 as u64;
    (addr
        .wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407)
        >> 32) as u32
}
