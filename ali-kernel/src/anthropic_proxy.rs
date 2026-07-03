//! Anthropic Messages API proxy — L5.5 compresses context before forwarding to
//! api.anthropic.com. Point Claude Code at the gateway with:
//!   export ANTHROPIC_BASE_URL=http://127.0.0.1:9000
//! Auth passthrough: the caller's x-api-key or Authorization header is forwarded
//! unchanged; the gateway never stores or logs the key.

use axum::{
    body::Body,
    extract::State,
    http::{HeaderMap, HeaderName, Response, StatusCode},
    Json,
};
use futures::StreamExt;
use serde_json::Value;
use tokio_stream::wrappers::ReceiverStream;
use tracing::info;

use crate::usage_ledger::{UsageRecord, UsageScanner};
use crate::{compress, AppState};

const ANTHROPIC_API: &str = "https://api.anthropic.com";
const SYSTEM_BUDGET: usize = 8_000;
const MSG_BUDGET: usize = 4_000;

pub async fn messages_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(mut body): Json<Value>,
) -> Response<Body> {
    // Resolve auth — Anthropic sends x-api-key; some clients use Authorization: Bearer
    let api_key = headers
        .get("x-api-key")
        .or_else(|| headers.get("authorization"))
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

    let Some(auth_value) = api_key else {
        return error_response(
            StatusCode::UNAUTHORIZED,
            "missing x-api-key or Authorization header",
        );
    };

    // Measure context before compression
    let before = serialized_size(&body);
    let req_model = body
        .get("model")
        .and_then(|m| m.as_str())
        .unwrap_or("unknown")
        .to_string();

    // Compress system prompt (often enormous — CLAUDE.md, rules, etc.)
    let mut squeeze = compress::SqueezeStats::default();
    compress_system(&mut body, &mut squeeze);

    // Compress large message content blocks
    compress_messages(&mut body, &mut squeeze);

    let after = serialized_size(&body);
    let pct = after
        .checked_mul(100)
        .and_then(|v| v.checked_div(before))
        .map(|v| 100u64.saturating_sub(v as u64))
        .unwrap_or(0);
    info!(
        "PIPE L5.5 compressed: {} → {} chars (-{}%) [claude-proxy]",
        before, after, pct
    );

    // Build forwarding request
    let target = format!("{ANTHROPIC_API}/v1/messages");
    let mut req = state
        .client
        .post(&target)
        .header("content-type", "application/json");

    // Auth: prefer x-api-key (Anthropic native); fall back to Authorization
    if headers.contains_key("x-api-key") {
        req = req.header("x-api-key", &auth_value);
    } else {
        req = req.header("authorization", &auth_value);
    }

    // Forward safe headers that Anthropic expects
    for name in &[
        "anthropic-version",
        "anthropic-beta",
        "anthropic-dangerous-direct-browser-access",
    ] {
        if let Some(v) = headers.get(*name) {
            req = req.header(*name, v);
        }
    }

    // Default version if caller didn't provide one
    if !headers.contains_key("anthropic-version") {
        req = req.header("anthropic-version", "2023-06-01");
    }

    let upstream = match req.json(&body).send().await {
        Ok(r) => r,
        Err(e) => return error_response(StatusCode::BAD_GATEWAY, &e.to_string()),
    };

    let status = upstream.status();
    let mut builder = Response::builder().status(status);

    // Forward response headers (skip hop-by-hop)
    for (k, v) in upstream.headers() {
        let name = k.as_str();
        if matches!(name, "transfer-encoding" | "connection" | "keep-alive") {
            continue;
        }
        if let Ok(hn) = HeaderName::from_bytes(k.as_ref()) {
            builder = builder.header(hn, v);
        }
    }

    // Tee the response through a channel: bytes are forwarded to the client
    // as they arrive while a copy feeds the usage scanner, so capturing the
    // billed `usage` block adds no client-visible latency.
    // OAuth bearer = subscription plan (Claude Code Pro/Max); x-api-key =
    // pay-per-token. Recorded per request so /stats can split value honestly.
    let auth_kind = if headers.contains_key("x-api-key") {
        "api_key"
    } else {
        "subscription"
    };

    let sse = upstream
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .map(|ct| ct.contains("text/event-stream"))
        .unwrap_or(false);
    let record_usage = status.is_success();
    let ledger = state.ledger.clone();
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<bytes::Bytes, reqwest::Error>>(32);
    tokio::spawn(async move {
        let mut scanner = UsageScanner::new(sse);
        let mut stream = upstream.bytes_stream();
        while let Some(item) = stream.next().await {
            if record_usage {
                if let Ok(chunk) = &item {
                    scanner.feed(chunk);
                }
            }
            if tx.send(item).await.is_err() {
                // Client hung up; drain upstream only for the usage record.
                if !record_usage {
                    return;
                }
            }
        }
        scanner.finish();
        if record_usage && scanner.found_usage() {
            ledger.append(&UsageRecord {
                ts: chrono::Utc::now(),
                model: scanner.model.unwrap_or(req_model),
                auth: auth_kind.to_string(),
                chars_raw: before as u64,
                chars_compressed: after as u64,
                chars_lossless_saved: squeeze.raw.saturating_sub(squeeze.lossless),
                input_tokens: scanner.usage.input_tokens,
                cache_creation_input_tokens: scanner.usage.cache_creation_input_tokens,
                cache_read_input_tokens: scanner.usage.cache_read_input_tokens,
                output_tokens: scanner.usage.output_tokens,
            });
        }
    });

    builder
        .body(Body::from_stream(ReceiverStream::new(rx)))
        .unwrap_or_else(|_| error_response(StatusCode::INTERNAL_SERVER_ERROR, "body build failed"))
}

fn compress_system(body: &mut Value, sq: &mut compress::SqueezeStats) {
    match body.get_mut("system") {
        // Legacy string form
        Some(Value::String(s)) if s.len() > SYSTEM_BUDGET => {
            let compressed = compress::compress_observation_with(s, SYSTEM_BUDGET, sq);
            body["system"] = Value::String(compressed);
        }
        // Claude Code sends an array of blocks, each possibly carrying
        // cache_control — compress text in place, keep block structure intact.
        Some(Value::Array(blocks)) => {
            for block in blocks.iter_mut() {
                compress_text_field(block, "text", SYSTEM_BUDGET, sq);
            }
        }
        _ => {}
    }
}

/// Compress `obj[key]` in place when it's a string over `budget`. Other keys
/// (cache_control, type, …) are untouched so prompt caching keeps working.
fn compress_text_field(obj: &mut Value, key: &str, budget: usize, sq: &mut compress::SqueezeStats) {
    if let Some(text) = obj.get(key).and_then(|t| t.as_str()) {
        if text.len() > budget {
            let compressed = compress::compress_observation_with(text, budget, sq);
            obj[key] = Value::String(compressed);
        }
    }
}

fn compress_messages(body: &mut Value, sq: &mut compress::SqueezeStats) {
    let Some(msgs) = body.get_mut("messages").and_then(|m| m.as_array_mut()) else {
        return;
    };
    for msg in msgs.iter_mut() {
        // Simple string content
        if let Some(text) = msg.get("content").and_then(|c| c.as_str()) {
            if text.len() > MSG_BUDGET {
                let compressed = compress::compress_observation_with(text, MSG_BUDGET, sq);
                msg["content"] = Value::String(compressed);
            }
            continue;
        }
        // Array content blocks (tool results, multi-part)
        if let Some(blocks) = msg.get_mut("content").and_then(|c| c.as_array_mut()) {
            for block in blocks.iter_mut() {
                compress_block(block, sq);
            }
        }
    }
}

/// Compress one content block. `text` blocks carry a plain string; `tool_result`
/// blocks nest either a string or another block array — in coding sessions those
/// are the largest payloads (file reads, build logs).
fn compress_block(block: &mut Value, sq: &mut compress::SqueezeStats) {
    match block.get("type").and_then(|t| t.as_str()) {
        Some("text") => compress_text_field(block, "text", MSG_BUDGET, sq),
        Some("tool_result") => match block.get_mut("content") {
            Some(Value::String(s)) => {
                // Tool outputs get the lossless pass (ANSI, blank runs) at any
                // size; only over-budget ones are additionally truncated.
                let new = if s.len() > MSG_BUDGET {
                    compress::compress_observation_with(s, MSG_BUDGET, sq)
                } else {
                    compress_lossless_counted(s, sq)
                };
                if new != *s {
                    block["content"] = Value::String(new);
                }
            }
            Some(Value::Array(inner)) => {
                for b in inner.iter_mut() {
                    if b.get("type").and_then(|t| t.as_str()) == Some("text") {
                        if let Some(text) = b.get("text").and_then(|t| t.as_str()) {
                            let new = if text.len() > MSG_BUDGET {
                                compress::compress_observation_with(text, MSG_BUDGET, sq)
                            } else {
                                compress_lossless_counted(text, sq)
                            };
                            if new != text {
                                b["text"] = Value::String(new);
                            }
                        }
                    }
                }
            }
            _ => {}
        },
        _ => {}
    }
}

fn serialized_size(v: &Value) -> usize {
    v.to_string().len()
}

/// Lossless pass that also counts sizes into the squeeze accumulator.
fn compress_lossless_counted(s: &str, sq: &mut compress::SqueezeStats) -> String {
    let out = compress::compress_lossless(s);
    sq.raw += s.len() as u64;
    sq.lossless += out.len() as u64;
    sq.sent += out.len() as u64;
    out
}

fn error_response(status: StatusCode, msg: &str) -> Response<Body> {
    let body = serde_json::json!({ "error": { "type": "proxy_error", "message": msg } });
    Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn big(n: usize) -> String {
        "x".repeat(n)
    }

    #[test]
    fn system_array_blocks_compressed_cache_control_kept() {
        let mut body = json!({
            "system": [
                {"type": "text", "text": "small", "cache_control": {"type": "ephemeral"}},
                {"type": "text", "text": big(20_000), "cache_control": {"type": "ephemeral"}}
            ]
        });
        compress_system(&mut body, &mut compress::SqueezeStats::default());
        let blocks = body["system"].as_array().unwrap();
        assert_eq!(blocks[0]["text"], "small");
        assert!(blocks[0].get("cache_control").is_some());
        let t = blocks[1]["text"].as_str().unwrap();
        assert!(t.len() < 20_000);
        assert!(t.contains("compressed by L5.5"));
        assert!(blocks[1].get("cache_control").is_some());
    }

    #[test]
    fn tool_result_string_content_compressed() {
        let mut body = json!({
            "messages": [{
                "role": "user",
                "content": [{
                    "type": "tool_result",
                    "tool_use_id": "tu_1",
                    "content": big(50_000)
                }]
            }]
        });
        compress_messages(&mut body, &mut compress::SqueezeStats::default());
        let c = body["messages"][0]["content"][0]["content"]
            .as_str()
            .unwrap();
        assert!(c.len() < 50_000);
        assert!(c.contains("compressed by L5.5"));
        assert_eq!(body["messages"][0]["content"][0]["tool_use_id"], "tu_1");
    }

    #[test]
    fn tool_result_nested_blocks_compressed() {
        let mut body = json!({
            "messages": [{
                "role": "user",
                "content": [{
                    "type": "tool_result",
                    "tool_use_id": "tu_2",
                    "content": [
                        {"type": "text", "text": big(30_000)},
                        {"type": "image", "source": {"type": "base64", "data": "AAAA"}}
                    ]
                }]
            }]
        });
        compress_messages(&mut body, &mut compress::SqueezeStats::default());
        let inner = body["messages"][0]["content"][0]["content"]
            .as_array()
            .unwrap();
        let t = inner[0]["text"].as_str().unwrap();
        assert!(t.len() < 30_000);
        // non-text blocks pass through untouched
        assert_eq!(inner[1]["source"]["data"], "AAAA");
    }

    #[test]
    fn small_payloads_untouched() {
        let mut body = json!({
            "system": "short system",
            "messages": [
                {"role": "user", "content": "hello"},
                {"role": "assistant", "content": [{"type": "text", "text": "hi"}]}
            ]
        });
        let before = body.clone();
        compress_system(&mut body, &mut compress::SqueezeStats::default());
        compress_messages(&mut body, &mut compress::SqueezeStats::default());
        assert_eq!(body, before);
    }
}
