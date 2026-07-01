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
use serde_json::Value;
use tracing::info;

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

    // Compress system prompt (often enormous — CLAUDE.md, rules, etc.)
    compress_system(&mut body);

    // Compress large message content blocks
    compress_messages(&mut body);

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

    let stream = upstream.bytes_stream();
    builder
        .body(Body::from_stream(stream))
        .unwrap_or_else(|_| error_response(StatusCode::INTERNAL_SERVER_ERROR, "body build failed"))
}

fn compress_system(body: &mut Value) {
    if let Some(sys) = body.get("system").and_then(|s| s.as_str()) {
        if sys.len() > SYSTEM_BUDGET {
            let compressed = compress::compress_observation(sys, SYSTEM_BUDGET);
            body["system"] = Value::String(compressed);
        }
    }
}

fn compress_messages(body: &mut Value) {
    let Some(msgs) = body.get_mut("messages").and_then(|m| m.as_array_mut()) else {
        return;
    };
    for msg in msgs.iter_mut() {
        // Simple string content
        if let Some(text) = msg.get("content").and_then(|c| c.as_str()) {
            if text.len() > MSG_BUDGET {
                let compressed = compress::compress_observation(text, MSG_BUDGET);
                msg["content"] = Value::String(compressed);
            }
            continue;
        }
        // Array content blocks (tool results, multi-part)
        if let Some(blocks) = msg.get_mut("content").and_then(|c| c.as_array_mut()) {
            for block in blocks.iter_mut() {
                if block.get("type").and_then(|t| t.as_str()) != Some("text") {
                    continue;
                }
                if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                    if text.len() > MSG_BUDGET {
                        let compressed = compress::compress_observation(text, MSG_BUDGET);
                        block["text"] = Value::String(compressed);
                    }
                }
            }
        }
    }
}

fn serialized_size(v: &Value) -> usize {
    v.to_string().len()
}

fn error_response(status: StatusCode, msg: &str) -> Response<Body> {
    let body = serde_json::json!({ "error": { "type": "proxy_error", "message": msg } });
    Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}
