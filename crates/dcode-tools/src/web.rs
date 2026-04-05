/// Web fetch and web search tools.
use anyhow::Context;

const MAX_RESPONSE_BYTES: usize = 2 * 1024 * 1024; // 2 MB
const FETCH_TIMEOUT_SECS: u64 = 30;

// ─── Read local image ─────────────────────────────────────────────────────────

/// Read a local image file and return it as a data URI (`data:image/png;base64,...`).
/// The caller (provider serialization) converts it to the appropriate API image block.
pub fn read_image(path: &str) -> anyhow::Result<String> {
    let ext = std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_lowercase);
    let mime = match ext.as_deref() {
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("png")  => "image/png",
        Some("gif")  => "image/gif",
        Some("webp") => "image/webp",
        _ => anyhow::bail!(
            "Unsupported image format '{}'. Supported: jpg, png, gif, webp",
            path
        ),
    };

    let bytes = std::fs::read(path)
        .map_err(|e| anyhow::anyhow!("Cannot read '{path}': {e}"))?;

    // 10 MB guard — base64 of 10MB is ~14MB of text, which is already large.
    if bytes.len() > 10 * 1024 * 1024 {
        anyhow::bail!("Image too large ({}MB, max 10MB)", bytes.len() / 1024 / 1024);
    }

    let b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &bytes);
    Ok(format!("data:{mime};base64,{b64}"))
}

// ─── Web Fetch ────────────────────────────────────────────────────────────────

pub async fn web_fetch(url: &str) -> anyhow::Result<String> {
    if !url.starts_with("http://") && !url.starts_with("https://") {
        anyhow::bail!("URL must start with http:// or https://");
    }

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(FETCH_TIMEOUT_SECS))
        .user_agent(
            "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 \
             (KHTML, like Gecko) Chrome/124.0 Safari/537.36",
        )
        .build()?;

    let resp = client
        .get(url)
        .header("Accept", "text/html,text/plain,text/markdown,*/*;q=0.8")
        .send()
        .await
        .context("fetch failed")?;

    let status = resp.status();
    if !status.is_success() {
        anyhow::bail!("HTTP {status} for {url}");
    }

    let content_type = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_lowercase();

    let bytes = resp.bytes().await.context("reading body")?;
    if bytes.len() > MAX_RESPONSE_BYTES {
        anyhow::bail!(
            "Response too large ({} bytes, max {})",
            bytes.len(),
            MAX_RESPONSE_BYTES
        );
    }

    // Images: return as a data-URI so the model can see them.
    let mime = content_type.split(';').next().unwrap_or("").trim();
    let is_image = matches!(mime, "image/jpeg" | "image/jpg" | "image/png" | "image/gif" | "image/webp");
    if is_image {
        let b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &bytes);
        return Ok(format!("data:{mime};base64,{b64}"));
    }

    let text = String::from_utf8_lossy(&bytes).into_owned();

    if content_type.contains("text/html") {
        Ok(html_to_text(&text))
    } else {
        Ok(text)
    }
}

/// Very lightweight HTML → readable text conversion.
/// Strips scripts/styles, replaces block tags with newlines, strips remaining tags,
/// decodes common entities.
fn html_to_text(html: &str) -> String {
    // 1. Remove <script>…</script> and <style>…</style> blocks (case-insensitive, dotall).
    let re_script = regex::Regex::new(r"(?is)<(script|style)[^>]*>.*?</\1>").unwrap();
    let s = re_script.replace_all(html, " ");

    // 2. Block-level tags → newlines.
    let re_block =
        regex::Regex::new(r"(?i)</(p|div|h[1-6]|li|tr|br|article|section|header|footer)>")
            .unwrap();
    let s = re_block.replace_all(&s, "\n");
    let re_br = regex::Regex::new(r"(?i)<br\s*/?>").unwrap();
    let s = re_br.replace_all(&s, "\n");

    // 3. Strip all remaining tags.
    let re_tag = regex::Regex::new(r"<[^>]+>").unwrap();
    let s = re_tag.replace_all(&s, "");

    // 4. Decode common HTML entities.
    let s = s
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&nbsp;", " ")
        .replace("&mdash;", "—")
        .replace("&ndash;", "–")
        .replace("&hellip;", "…");

    // 5. Collapse runs of blank lines to max 2.
    let re_blanks = regex::Regex::new(r"\n{3,}").unwrap();
    let s = re_blanks.replace_all(&s, "\n\n");

    s.trim().to_string()
}

// ─── Web Search (Exa via MCP) ─────────────────────────────────────────────────

pub async fn web_search(query: &str, num_results: usize) -> anyhow::Result<String> {
    let payload = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {
            "name": "web_search_exa",
            "arguments": {
                "query": query,
                "type": "auto",
                "numResults": num_results,
                "livecrawl": "fallback",
                "contextMaxCharacters": 8000
            }
        }
    });

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(25))
        .build()?;

    let resp = client
        .post("https://mcp.exa.ai/mcp")
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream")
        .json(&payload)
        .send()
        .await
        .context("web search request failed")?;

    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("Search API returned {status}: {body}");
    }

    let body = resp.text().await.context("reading search response")?;

    // Parse SSE lines: "data: <json>"
    for line in body.lines() {
        if let Some(json_str) = line.strip_prefix("data: ") {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(json_str) {
                if let Some(text) = v
                    .pointer("/result/content/0/text")
                    .and_then(|t| t.as_str())
                {
                    return Ok(text.to_string());
                }
            }
        }
    }

    // Fallback: try parsing entire body as JSON.
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&body) {
        if let Some(text) = v
            .pointer("/result/content/0/text")
            .and_then(|t| t.as_str())
        {
            return Ok(text.to_string());
        }
    }

    anyhow::bail!("No search results found. Try a different query.")
}
