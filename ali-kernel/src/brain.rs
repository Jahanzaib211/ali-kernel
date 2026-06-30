use anyhow::{Context, Result};
use bytes::Bytes;
use futures::Stream;
use reqwest::Client;
use serde_json::{json, Value};
use std::pin::Pin;
use std::time::Duration;
use tokio::time::timeout;

use crate::config::Config;

const INFER_TIMEOUT_SECS: u64 = 120;
const RETRY_DELAY_MS: u64 = 2000;
const MAX_TOKENS_CAP: u64 = 12_000;

pub enum BrainResponse {
    Complete(Value),
    Streaming(Pin<Box<dyn Stream<Item = Result<Bytes, reqwest::Error>> + Send>>),
}

pub async fn infer(
    client: &Client,
    config: &Config,
    messages: Vec<Value>,
    temperature: f64,
    max_tokens: u64,
    stream: bool,
    extra: Value,
) -> Result<BrainResponse> {
    if messages.is_empty() {
        anyhow::bail!("messages array is empty");
    }

    let capped_tokens = max_tokens.min(MAX_TOKENS_CAP);
    let url = format!("{}/chat/completions", config.backend_base());
    let api_key = config.backend_api_key.as_deref();

    let mut body = json!({
        "model": config.backend_model,
        "messages": messages,
        "temperature": temperature,
        "max_tokens": capped_tokens,
        "stream": stream,
    });

    // Merge extra fields (e.g. tools, tool_choice)
    if let (Some(obj), Some(extra_obj)) = (body.as_object_mut(), extra.as_object()) {
        for (k, v) in extra_obj {
            obj.insert(k.clone(), v.clone());
        }
    }

    if stream {
        let resp = timeout(
            Duration::from_secs(INFER_TIMEOUT_SECS),
            auth(client.post(&url).json(&body), api_key).send(),
        )
        .await
        .context("Request timed out")??;

        let stream = resp.bytes_stream();
        return Ok(BrainResponse::Streaming(Box::pin(stream)));
    }

    // Non-streaming with one retry on 5xx
    let response = call_with_retry(client, &url, &body, api_key).await?;
    Ok(BrainResponse::Complete(response))
}

/// Attach bearer auth when the backend requires an API key (remote providers).
fn auth(req: reqwest::RequestBuilder, api_key: Option<&str>) -> reqwest::RequestBuilder {
    match api_key {
        Some(k) => req.bearer_auth(k),
        None => req,
    }
}

async fn call_with_retry(
    client: &Client,
    url: &str,
    body: &Value,
    api_key: Option<&str>,
) -> Result<Value> {
    let attempt = || async {
        timeout(
            Duration::from_secs(INFER_TIMEOUT_SECS),
            auth(client.post(url).json(body), api_key).send(),
        )
        .await
        .context("Request timed out")?
        .context("Request failed")
    };

    let resp = attempt().await?;
    if resp.status().is_server_error() {
        tokio::time::sleep(Duration::from_millis(RETRY_DELAY_MS)).await;
        let retry = attempt().await?;
        return retry.json::<Value>().await.context("Invalid JSON on retry");
    }

    resp.json::<Value>()
        .await
        .context("Invalid JSON response from llama-server")
}

pub fn extract_content(response: &Value) -> Option<&str> {
    response
        .get("choices")?
        .get(0)?
        .get("message")?
        .get("content")?
        .as_str()
}

/// Shield: strip any `<think>…</think>` reasoning blocks (and a dangling unclosed
/// `<think>`) that leak into visible content. Returns `(cleaned, changed)`.
pub fn strip_think(content: &str) -> (String, bool) {
    if !content.contains("<think>") {
        return (content.to_string(), false);
    }
    let mut out = String::with_capacity(content.len());
    let mut rest = content;
    while let Some(start) = rest.find("<think>") {
        out.push_str(&rest[..start]);
        match rest[start..].find("</think>") {
            Some(end) => rest = &rest[start + end + "</think>".len()..],
            None => {
                rest = ""; // unclosed think → drop the remainder
                break;
            }
        }
    }
    out.push_str(rest);
    let cleaned = out.trim().to_string();
    (cleaned, true)
}

#[cfg(test)]
mod tests {
    use super::strip_think;

    #[test]
    fn strips_think_block() {
        let (c, changed) = strip_think("<think>reasoning here</think>\nfinal answer");
        assert!(changed);
        assert_eq!(c, "final answer");
    }

    #[test]
    fn strips_unclosed_think() {
        let (c, _) = strip_think("ok<think>still thinking and cut off");
        assert_eq!(c, "ok");
    }

    #[test]
    fn leaves_clean_content() {
        let (c, changed) = strip_think("just an answer");
        assert!(!changed);
        assert_eq!(c, "just an answer");
    }
}
