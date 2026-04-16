/// Antigravity provider — Google Cloud Code Assist (Gemini 3, Claude, GPT-OSS).
/// Uses OAuth PKCE with a local callback server, same flow as pi-mono.
use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use futures::Stream;
use serde::Deserialize;
use tokio::sync::RwLock;

use crate::oauth::url_encode;
use crate::provider::Provider;
use crate::types::{AuthStore, ContentBlock, Message, Role, StreamEvent, ToolDef};

// ── OAuth credentials ─────────────────────────────────────────────────────────

const CLIENT_ID_ENV: &str = "DCODE_ANTIGRAVITY_CLIENT_ID";
const CLIENT_SECRET_ENV: &str = "DCODE_ANTIGRAVITY_CLIENT_SECRET";
const REDIRECT_URI: &str = "http://localhost:51121/oauth-callback";

const SCOPES: &[&str] = &[
    "https://www.googleapis.com/auth/cloud-platform",
    "https://www.googleapis.com/auth/userinfo.email",
    "https://www.googleapis.com/auth/userinfo.profile",
    "https://www.googleapis.com/auth/cclog",
    "https://www.googleapis.com/auth/experimentsandconfigs",
];

const AUTH_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";
const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
const DEFAULT_PROJECT_ID: &str = "rising-fact-p41fc";

const CLOUDCODE_ENDPOINTS: &[&str] = &[
    "https://cloudcode-pa.googleapis.com",
    "https://daily-cloudcode-pa.sandbox.googleapis.com",
];

const ANTIGRAVITY_VERSION: &str = "1.18.4";

// ── Static model list ────────────────────────────────────────────────────────

pub const DEFAULT_MODEL: &str = "claude-sonnet-4-6";
pub const SUPPORTED_MODELS: &[&str] = &[
    // Claude
    "claude-sonnet-4-5",
    "claude-sonnet-4-5-thinking",
    "claude-sonnet-4-6",
    "claude-opus-4-5-thinking",
    "claude-opus-4-6-thinking",
    "claude-haiku-4-5",
    // Gemini
    "gemini-3-flash",
    "gemini-3.1-pro-high",
    "gemini-3.1-pro-low",
    // GPT-OSS
    "gpt-oss-120b-medium",
];
pub const CONTEXT_WINDOW: u32 = 200_000;

// ── OAuth login flow ─────────────────────────────────────────────────────────

/// Data returned after a successful login.
#[derive(Debug, Clone)]
pub struct AntigravityCredentials {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_at: i64,
    pub project_id: String,
    pub email: Option<String>,
}

/// Build the authorization URL the user should open in their browser.
pub fn build_auth_url(pkce_challenge: &str, pkce_verifier: &str) -> anyhow::Result<String> {
    let client_id = client_id()?;
    let scope = SCOPES.join(" ");
    Ok(format!(
        "{AUTH_URL}?client_id={}&response_type=code&redirect_uri={}&scope={}&code_challenge={}&code_challenge_method=S256&state={}&access_type=offline&prompt=consent",
        url_encode(&client_id),
        url_encode(REDIRECT_URI),
        url_encode(&scope),
        url_encode(pkce_challenge),
        url_encode(pkce_verifier),
    ))
}

fn client_id() -> anyhow::Result<String> {
    std::env::var(CLIENT_ID_ENV).map_err(|_| {
        anyhow::anyhow!("Missing required env var: {CLIENT_ID_ENV} (Google OAuth client ID)")
    })
}

fn client_secret() -> anyhow::Result<String> {
    std::env::var(CLIENT_SECRET_ENV).map_err(|_| {
        anyhow::anyhow!("Missing required env var: {CLIENT_SECRET_ENV} (Google OAuth client secret)")
    })
}

/// Start a local HTTP server on :51121 to receive the OAuth callback.
/// Returns `(auth_code, state)` on success.
pub async fn wait_for_callback() -> anyhow::Result<(String, String)> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:51121").await?;

    loop {
        let (mut stream, _) = listener.accept().await?;
        let (reader, mut writer) = stream.split();
        let mut buf_reader = BufReader::new(reader);
        let mut request_line = String::new();
        buf_reader.read_line(&mut request_line).await?;

        // Parse GET /oauth-callback?code=xxx&state=yyy HTTP/1.1
        let path = request_line
            .split_whitespace()
            .nth(1)
            .unwrap_or("")
            .to_string();

        if !path.starts_with("/oauth-callback") {
            let body = "<html><body>Not found</body></html>";
            let resp = format!(
                "HTTP/1.1 404 Not Found\r\nContent-Type: text/html\r\nContent-Length: {}\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = writer.write_all(resp.as_bytes()).await;
            continue;
        }

        // Parse query params
        let url = url::Url::parse(&format!("http://localhost{path}"))
            .map_err(|e| anyhow::anyhow!("Bad callback URL: {e}"))?;
        let code = url
            .query_pairs()
            .find(|(k, _)| k == "code")
            .map(|(_, v)| v.to_string());
        let state = url
            .query_pairs()
            .find(|(k, _)| k == "state")
            .map(|(_, v)| v.to_string());

        // Send success HTML
        let body = "<html><body><h2>Authentication complete!</h2><p>You can close this window and return to d-code.</p></body></html>";
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        let _ = writer.write_all(resp.as_bytes()).await;

        match (code, state) {
            (Some(c), Some(s)) => return Ok((c, s)),
            _ => {
                anyhow::bail!("Callback missing code or state parameter");
            }
        }
    }
}

/// Exchange the authorization code for tokens.
pub async fn exchange_code(
    code: &str,
    verifier: &str,
) -> anyhow::Result<AntigravityCredentials> {
    let client_id = client_id()?;
    let client_secret = client_secret()?;
    let client = reqwest::Client::new();
    let params = [
        ("client_id", client_id.as_str()),
        ("client_secret", client_secret.as_str()),
        ("code", code),
        ("grant_type", "authorization_code"),
        ("redirect_uri", REDIRECT_URI),
        ("code_verifier", verifier),
    ];

    let resp = client.post(TOKEN_URL).form(&params).send().await?;
    if !resp.status().is_success() {
        let text = resp.text().await.unwrap_or_default();
        anyhow::bail!("Token exchange failed: {text}");
    }

    #[derive(Deserialize)]
    struct TokenResp {
        access_token: String,
        refresh_token: Option<String>,
        expires_in: i64,
    }
    let data: TokenResp = resp.json().await?;
    let refresh_token = data
        .refresh_token
        .ok_or_else(|| anyhow::anyhow!("No refresh token — try logging in again"))?;

    let now = chrono::Utc::now().timestamp();
    let expires_at = now + data.expires_in - 300; // 5 min buffer

    // Discover project
    let project_id = discover_project(&data.access_token).await;

    // Get email
    let email = get_user_email(&data.access_token).await;

    Ok(AntigravityCredentials {
        access_token: data.access_token,
        refresh_token,
        expires_at,
        project_id,
        email,
    })
}

/// Discover the user's Cloud Code Assist project.
async fn discover_project(access_token: &str) -> String {
    let client = reqwest::Client::new();
    let body = serde_json::json!({
        "metadata": {
            "ideType": "IDE_UNSPECIFIED",
            "platform": "PLATFORM_UNSPECIFIED",
            "pluginType": "GEMINI"
        }
    });

    for endpoint in CLOUDCODE_ENDPOINTS {
        let url = format!("{endpoint}/v1internal:loadCodeAssist");
        let resp = client
            .post(&url)
            .bearer_auth(access_token)
            .header("Content-Type", "application/json")
            .header("User-Agent", "google-api-nodejs-client/9.15.1")
            .header(
                "X-Goog-Api-Client",
                "google-cloud-sdk vscode_cloudshelleditor/0.1",
            )
            .json(&body)
            .send()
            .await;

        if let Ok(r) = resp {
            if r.status().is_success() {
                if let Ok(v) = r.json::<serde_json::Value>().await {
                    // Handle both string and object formats
                    if let Some(s) = v["cloudaicompanionProject"].as_str() {
                        if !s.is_empty() {
                            return s.to_string();
                        }
                    }
                    if let Some(s) = v["cloudaicompanionProject"]["id"].as_str() {
                        if !s.is_empty() {
                            return s.to_string();
                        }
                    }
                }
            }
        }
    }
    DEFAULT_PROJECT_ID.to_string()
}

/// Get user email from access token.
async fn get_user_email(access_token: &str) -> Option<String> {
    let client = reqwest::Client::new();
    let resp = client
        .get("https://www.googleapis.com/oauth2/v1/userinfo?alt=json")
        .bearer_auth(access_token)
        .send()
        .await
        .ok()?;
    let v: serde_json::Value = resp.json().await.ok()?;
    v["email"].as_str().map(|s| s.to_string())
}

/// Refresh an Antigravity access token.
pub async fn refresh_token(
    refresh_token: &str,
    project_id: &str,
) -> anyhow::Result<AntigravityCredentials> {
    let client_id = client_id()?;
    let client_secret = client_secret()?;
    let client = reqwest::Client::new();
    let params = [
        ("client_id", client_id.as_str()),
        ("client_secret", client_secret.as_str()),
        ("refresh_token", refresh_token),
        ("grant_type", "refresh_token"),
    ];
    let resp = client.post(TOKEN_URL).form(&params).send().await?;
    if !resp.status().is_success() {
        let text = resp.text().await.unwrap_or_default();
        anyhow::bail!("Antigravity token refresh failed: {text}");
    }
    #[derive(Deserialize)]
    struct RefreshResp {
        access_token: String,
        expires_in: i64,
        refresh_token: Option<String>,
    }
    let data: RefreshResp = resp.json().await?;
    let now = chrono::Utc::now().timestamp();
    Ok(AntigravityCredentials {
        access_token: data.access_token,
        refresh_token: data.refresh_token.unwrap_or_else(|| refresh_token.to_string()),
        expires_at: now + data.expires_in - 300,
        project_id: project_id.to_string(),
        email: None,
    })
}

/// Save Antigravity credentials to the auth store.
pub fn save_credentials(creds: &AntigravityCredentials) -> anyhow::Result<()> {
    let mut store = AuthStore::load().unwrap_or_default();
    store.antigravity = Some(crate::types::AntigravityAuth {
        access_token: creds.access_token.clone(),
        refresh_token: creds.refresh_token.clone(),
        expires_at: Some(creds.expires_at),
        project_id: creds.project_id.clone(),
        email: creds.email.clone(),
    });
    store.save()
}

// ── Provider ─────────────────────────────────────────────────────────────────

pub struct AntigravityProvider {
    model: String,
    access_token: Arc<RwLock<String>>,
    refresh_tok: String,
    project_id: String,
    expires_at: Arc<RwLock<i64>>,
    client: reqwest::Client,
}

impl AntigravityProvider {
    pub fn from_auth_with_model(model: impl Into<String>) -> anyhow::Result<Self> {
        let store = AuthStore::load()?;
        let auth = store.antigravity.ok_or_else(|| {
            anyhow::anyhow!("Not logged in to Antigravity. Run: d-code login antigravity")
        })?;
        Ok(Self {
            model: model.into(),
            access_token: Arc::new(RwLock::new(auth.access_token)),
            refresh_tok: auth.refresh_token,
            project_id: auth.project_id,
            expires_at: Arc::new(RwLock::new(auth.expires_at.unwrap_or(0))),
            client: reqwest::Client::builder()
                .pool_max_idle_per_host(4)
                .build()
                .expect("reqwest client"),
        })
    }

    /// Ensure the access token is fresh; refresh if expired.
    async fn fresh_token(&self) -> anyhow::Result<String> {
        {
            let exp = self.expires_at.read().await;
            let now = chrono::Utc::now().timestamp();
            if now < *exp {
                return Ok(self.access_token.read().await.clone());
            }
        }
        // Refresh
        let creds = refresh_token(&self.refresh_tok, &self.project_id).await?;
        *self.access_token.write().await = creds.access_token.clone();
        *self.expires_at.write().await = creds.expires_at;
        // Persist refreshed token
        let _ = save_credentials(&creds);
        Ok(creds.access_token)
    }
}

#[async_trait]
impl Provider for AntigravityProvider {
    fn name(&self) -> &str {
        "antigravity"
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
        let token = self.fresh_token().await?;

        // Build tool_use_id → tool_name map for functionResponse correlation.
        let mut tool_name_map = std::collections::HashMap::<String, String>::new();
        for msg in messages {
            for block in &msg.content {
                if let ContentBlock::ToolUse { id, name, .. } = block {
                    tool_name_map.insert(id.clone(), name.clone());
                }
            }
        }

        // Claude and GPT-OSS models require id fields on functionCall/functionResponse.
        let needs_id = self.model.starts_with("claude-") || self.model.starts_with("gpt-oss-");

        // Build Gemini-style contents from messages.
        let mut contents = Vec::new();
        for msg in messages {
            let role = match msg.role {
                Role::User => "user",
                Role::Assistant => "model",
            };
            let mut parts = Vec::new();
            for block in &msg.content {
                match block {
                    ContentBlock::Text { text } => {
                        parts.push(serde_json::json!({"text": text}));
                    }
                    ContentBlock::ToolUse { id, name, input } => {
                        let mut fc = serde_json::json!({
                            "name": name,
                            "args": input
                        });
                        if needs_id {
                            fc["id"] = serde_json::json!(id);
                        }
                        parts.push(serde_json::json!({"functionCall": fc}));
                    }
                    ContentBlock::ToolResult {
                        tool_use_id,
                        content,
                        is_error,
                    } => {
                        let tool_name = tool_name_map
                            .get(tool_use_id)
                            .cloned()
                            .unwrap_or_else(|| "unknown".to_string());
                        let response = if *is_error {
                            serde_json::json!({"error": content})
                        } else {
                            serde_json::json!({"output": content})
                        };
                        let mut fr = serde_json::json!({
                            "name": tool_name,
                            "response": response
                        });
                        if needs_id {
                            fr["id"] = serde_json::json!(tool_use_id);
                        }
                        parts.push(serde_json::json!({"functionResponse": fr}));
                    }
                }
            }
            if !parts.is_empty() {
                // Tool results must be in "user" turns for Cloud Code Assist.
                let has_tool_result = msg.content.iter().any(|b| matches!(b, ContentBlock::ToolResult { .. }));
                let effective_role = if has_tool_result { "user" } else { role };
                contents.push(serde_json::json!({"role": effective_role, "parts": parts}));
            }
        }

        // Build tools definition.
        let gemini_tools = if !tools.is_empty() {
            let func_decls: Vec<serde_json::Value> = tools
                .iter()
                .map(|t| {
                    serde_json::json!({
                        "name": t.name,
                        "description": t.description,
                        "parameters": t.input_schema
                    })
                })
                .collect();
            Some(serde_json::json!([{"functionDeclarations": func_decls}]))
        } else {
            None
        };

        // Build the Cloud Code Assist request.
        let mut request = serde_json::json!({
            "contents": contents,
            "systemInstruction": {
                "parts": [{"text": system}]
            },
            "generationConfig": {
                "maxOutputTokens": max_tokens,
                "temperature": 1.0
            }
        });
        if let Some(t) = gemini_tools {
            request["tools"] = t;
            request["toolConfig"] = serde_json::json!({
                "functionCallingConfig": {"mode": "AUTO"}
            });
        }

        let body = serde_json::json!({
            "project": self.project_id,
            "model": self.model,
            "request": request,
            "requestType": "agent",
            "userAgent": format!("antigravity/{ANTIGRAVITY_VERSION}"),
            "requestId": format!("agent-{}-{}", chrono::Utc::now().timestamp_millis(),
                                 rand::random::<u32>() % 1_000_000)
        });

        // Try endpoints in order, with retry for transient errors (429/503).
        let mut last_err = String::new();
        let mut resp: Option<reqwest::Response> = None;

        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert("Content-Type", "application/json".parse().unwrap());
        headers.insert("Accept", "text/event-stream".parse().unwrap());
        headers.insert(
            "User-Agent",
            format!("antigravity/{ANTIGRAVITY_VERSION} linux/x86_64")
                .parse()
                .unwrap(),
        );
        if self.model.starts_with("claude-") && self.model.contains("thinking") {
            headers.insert(
                "anthropic-beta",
                "interleaved-thinking-2025-05-14".parse().unwrap(),
            );
        }

        'outer: for endpoint in CLOUDCODE_ENDPOINTS {
            let url = format!("{endpoint}/v1internal:streamGenerateContent?alt=sse");

            // Retry transient errors (429 rate-limit, 503 no capacity) up to 3 times.
            for attempt in 0..3u32 {
                if attempt > 0 {
                    tokio::time::sleep(std::time::Duration::from_millis(
                        1000 * (attempt as u64),
                    ))
                    .await;
                }

                match self
                    .client
                    .post(&url)
                    .bearer_auth(&token)
                    .headers(headers.clone())
                    .json(&body)
                    .send()
                    .await
                {
                    Ok(r) if r.status().is_success() => {
                        resp = Some(r);
                        break 'outer;
                    }
                    Ok(r) => {
                        let status = r.status();
                        let text = r.text().await.unwrap_or_default();
                        last_err = format!("Antigravity API error {status} {text}");
                        match status.as_u16() {
                            403 | 404 => break,       // try next endpoint
                            429 | 503 => continue,    // retry after backoff
                            _ => break 'outer,        // non-retryable
                        }
                    }
                    Err(e) => {
                        last_err = format!("Antigravity request failed: {e}");
                        break; // try next endpoint
                    }
                }
            }
        }

        let resp = resp.ok_or_else(|| anyhow::anyhow!("{last_err}"))?;

        // Parse SSE stream — Cloud Code Assist returns Gemini-style JSON chunks.
        let stream = async_stream::try_stream! {
            use futures::StreamExt;

            let mut byte_stream = resp.bytes_stream();
            let mut buffer = String::new();
            let current_tool_name = String::new();
            let tool_args_buf = String::new();
            let in_tool = false;

            while let Some(chunk) = byte_stream.next().await {
                let chunk = chunk?;
                buffer.push_str(&String::from_utf8_lossy(&chunk));

                // Process complete SSE lines.
                while let Some(pos) = buffer.find('\n') {
                    let line = buffer[..pos].trim_end().to_string();
                    buffer = buffer[pos + 1..].to_string();

                    if line.is_empty() || line.starts_with(':') {
                        continue;
                    }
                    let data = if let Some(d) = line.strip_prefix("data: ") {
                        d
                    } else {
                        continue;
                    };

                    let Ok(v) = serde_json::from_str::<serde_json::Value>(data) else {
                        continue;
                    };

                    // Parse candidates[0].content.parts
                    if let Some(parts) = v.pointer("/response/candidates/0/content/parts")
                        .and_then(|p| p.as_array())
                    {
                        for part in parts {
                            // Text part — check "thought" flag for thinking content.
                            if let Some(text) = part["text"].as_str() {
                                if !text.is_empty() {
                                    let is_thought = part.get("thought")
                                        .and_then(|v| v.as_bool())
                                        .unwrap_or(false);
                                    if is_thought {
                                        yield StreamEvent::ThinkingDelta(text.to_string());
                                    } else {
                                        yield StreamEvent::TextDelta(text.to_string());
                                    }
                                }
                            }
                            // Function call
                            if let Some(fc) = part.get("functionCall") {
                                let name = fc["name"].as_str().unwrap_or("").to_string();
                                let args = fc.get("args").cloned()
                                    .unwrap_or(serde_json::Value::Object(Default::default()));
                                // Preserve server-assigned id if present (needed for Claude/GPT-OSS round-trips).
                                let id = fc.get("id")
                                    .and_then(|v| v.as_str())
                                    .map(|s| s.to_string())
                                    .unwrap_or_else(|| format!("toolu_{}", rand::random::<u32>()));
                                yield StreamEvent::ToolUseStart {
                                    id: id.clone(),
                                    name: name.clone(),
                                };
                                yield StreamEvent::ToolUseDelta(
                                    serde_json::to_string(&args).unwrap_or_default()
                                );
                                yield StreamEvent::ToolUseEnd;
                            }
                        }
                    }

                    // Usage metadata
                    if let Some(usage) = v.pointer("/response/usageMetadata") {
                        let input = usage["promptTokenCount"].as_u64().unwrap_or(0) as u32;
                        let output = (usage["candidatesTokenCount"].as_u64().unwrap_or(0)
                            + usage["thoughtsTokenCount"].as_u64().unwrap_or(0))
                            as u32;
                        let cache_read = usage["cachedContentTokenCount"].as_u64().unwrap_or(0) as u32;
                        yield StreamEvent::Usage {
                            input_tokens: input.saturating_sub(cache_read),
                            output_tokens: output,
                            cache_write_tokens: 0,
                            cache_read_tokens: cache_read,
                        };
                    }

                    // Stop reason
                    if let Some(reason) = v.pointer("/response/candidates/0/finishReason")
                        .and_then(|r| r.as_str())
                    {
                        match reason {
                            "STOP" => yield StreamEvent::Done { stop_reason: crate::types::StopReason::EndTurn },
                            "MAX_TOKENS" => yield StreamEvent::Done { stop_reason: crate::types::StopReason::MaxTokens },
                            _ => {}
                        }
                    }
                }
            }

            let _ = (current_tool_name, tool_args_buf, in_tool); // suppress unused warnings
        };

        Ok(Box::pin(stream))
    }
}
