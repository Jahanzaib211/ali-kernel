//! Discovery + tool-protocol negotiation.
//!
//! `GET /v1/handshake` returns a manifest any client can read before sending a request.
//! `POST /v1/session` mints a short-lived JWT that locks the session to a tool mode and
//! a feature list. After that, the session header `X-Ali-Tool-Mode` (or JWT claim)
//! resolves the effective `ToolMode` per request.
//!
//! Precedence (highest first):
//!   1. `X-Ali-Tool-Mode` request header
//!   2. JWT claim (when `Authorization: Bearer <token>` validates)
//!   3. `Config::passthrough_tools` flag
//!   4. `ToolMode::Native` (legacy default)

use axum::http::HeaderMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::config::Config;
use crate::session::{decode_session, SessionClaims, ToolMode};

pub const HEADER_TOOL_MODE: &str = "x-ali-tool-mode";
pub const HEADER_DISABLE_FEATURES: &str = "x-ali-disable-features";

/// Static manifest describing the gateway surface. Cheap to rebuild per request.
pub fn manifest(config: &Config) -> Value {
    json!({
        "version": env!("CARGO_PKG_VERSION"),
        "model_cardinality": ["local"],
        "tool_protocols": ["openai-passthrough", "native-tools", "hybrid"],
        "tools_offered": crate::config::default_enabled_tool_names(),
        "tool_passthrough_modes": {
            "openai-passthrough":
                "user-supplied tools[] + tool_choice pass through verbatim; \
                 gateway native tools suppressed; recommended for third-party agents.",
            "native-tools":
                "default; only the gateway's enabled_tools set is exposed.",
            "hybrid":
                "user-supplied tools merge with gateway native tools; \
                 deduplicated by (name, parameter-schema fingerprint)."
        },
        "features": [
            "rag_memory",
            "behavioral_shield",
            "agentic_tools",
            "crate_aware_tools",
            "eval_harness",
            "rlaif_capture",
            "human_gate",
            "streaming",
            "long_ctx_l55",
            "mcp_tools"
        ],
        "defaults": {
            "tool_mode": if config.passthrough_tools { "openai-passthrough" } else { "native-tools" },
            "max_tokens_default": config.max_tokens_default,
        },
        "auth": {
            "session_endpoint": "/v1/session",
            "session_jwt_alg": "HS256",
            "session_issuer": "ali-kernel",
            "session_required": false,
        },
        "endpoints": {
            "chat_completions": "/v1/chat/completions",
            "messages":          "/v1/messages",
            "models":            "/v1/models",
            "feedback":          "/feedback",
            "approvals":         "/v1/approvals",
            "approvals_status":  "/v1/approvals/{request_id}",
            "stats":             "/stats",
            "benchmark":         "/benchmark",
            "eval":              "/eval",
            "handshake":         "/v1/handshake",
            "session":           "/v1/session",
            "mcp_tools":         "/v1/mcp/tools",
        },
    })
}

/// Mint a session token + claims for the client.
#[derive(Debug, Deserialize, Serialize)]
pub struct SessionRequest {
    pub tool_mode: Option<ToolMode>,
    pub ttl_secs: Option<i64>,
    pub features: Option<Vec<String>>,
}

#[derive(Debug, Serialize)]
pub struct SessionResponse {
    pub session_token: String,
    pub expires_at: chrono::DateTime<chrono::Utc>,
    pub tool_mode: ToolMode,
    pub features: Vec<String>,
}

/// Resolved per-request: which `ToolMode` to honour and which features to disable.
pub struct ResolvedRequest {
    pub tool_mode: ToolMode,
    pub disabled_features: Vec<String>,
    pub session: Option<SessionClaims>,
}

pub fn resolve_request(config: &Config, headers: &HeaderMap) -> ResolvedRequest {
    let mut disabled_features: Vec<String> = Vec::new();
    let mut session: Option<SessionClaims> = None;

    // 2. JWT claim wins over config default when present + valid.
    if let Some(auth) = headers.get(axum::http::header::AUTHORIZATION) {
        if let Ok(s) = auth.to_str() {
            if let Some(token) = s
                .strip_prefix("Bearer ")
                .or_else(|| s.strip_prefix("bearer "))
            {
                if let Some(key) = crate::session::resolve_signing_key(config) {
                    if let Ok(claims) = decode_session(token, &key) {
                        session = Some(claims.clone());
                    }
                }
            }
        }
    }

    // 1. Per-request header overrides everything.
    let header_mode: Option<ToolMode> = headers
        .get(HEADER_TOOL_MODE)
        .and_then(|v| v.to_str().ok())
        .and_then(parse_tool_mode);

    if let Some(d) = headers.get(HEADER_DISABLE_FEATURES) {
        if let Ok(s) = d.to_str() {
            disabled_features = s
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
        }
    }

    let tool_mode = header_mode
        .or_else(|| session.as_ref().map(|c| c.tool_mode))
        .unwrap_or_else(|| {
            if config.passthrough_tools {
                ToolMode::Passthrough
            } else {
                ToolMode::Native
            }
        });

    ResolvedRequest {
        tool_mode,
        disabled_features,
        session,
    }
}

pub fn parse_tool_mode(s: &str) -> Option<ToolMode> {
    match s.trim().to_ascii_lowercase().as_str() {
        "openai-passthrough" | "passthrough" | "pass-through" => Some(ToolMode::Passthrough),
        "native-tools" | "native" => Some(ToolMode::Native),
        "hybrid" => Some(ToolMode::Hybrid),
        _ => None,
    }
}

/// Merge a request's user-supplied `tools[]` and `tool_choice` with the gateway's
/// native tool set, according to the resolved mode.
///
/// Returns `Value::Object` containing either or both keys, ready to splat into the
/// brain request body via `extra`.
pub fn merge_tools(
    mode: ToolMode,
    user_tools: Option<&Value>,
    user_choice: Option<&Value>,
) -> Value {
    let native = json!(config_native_tools_schema());

    match mode {
        ToolMode::Passthrough => {
            let mut out = serde_json::Map::new();
            if let Some(t) = user_tools {
                out.insert("tools".into(), t.clone());
            }
            if let Some(c) = user_choice {
                out.insert("tool_choice".into(), c.clone());
            }
            Value::Object(out)
        }
        ToolMode::Native => {
            let mut out = serde_json::Map::new();
            // Native = ignore user tools, expose only gateway's. Preserve user choice if
            // explicit; otherwise omit so the model picks freely.
            out.insert("tools".into(), native);
            if let Some(c) = user_choice {
                if !c.is_null() {
                    out.insert("tool_choice".into(), c.clone());
                }
            }
            Value::Object(out)
        }
        ToolMode::Hybrid => {
            let mut merged: Vec<Value> = config_native_tools_schema();
            if let Some(Value::Array(user_arr)) = user_tools {
                for ut in user_arr {
                    if !merged
                        .iter()
                        .any(|n| tool_fingerprint(n) == tool_fingerprint(ut))
                    {
                        merged.push(ut.clone());
                    }
                }
            }
            let mut out = serde_json::Map::new();
            out.insert("tools".into(), Value::Array(merged));
            if let Some(c) = user_choice {
                out.insert("tool_choice".into(), c.clone());
            }
            Value::Object(out)
        }
    }
}

/// Stable fingerprint for dedup: name + canonicalised JSON-schema of the function.
/// Two tools with the same name and same parameter schema are considered duplicates.
fn tool_fingerprint(t: &Value) -> String {
    let name = t
        .get("name")
        .or_else(|| t.get("function").and_then(|f| f.get("name")))
        .and_then(|v| v.as_str())
        .unwrap_or("?")
        .to_string();
    let schema = t
        .get("parameters")
        .or_else(|| t.get("function").and_then(|f| f.get("parameters")))
        .cloned()
        .unwrap_or(Value::Null);
    let canonical = serde_json::to_string(&schema).unwrap_or_default();
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(name.as_bytes());
    h.update(b"\0");
    h.update(canonical.as_bytes());
    format!("{}:{}", name, hex::encode(h.finalize()))
}

/// Local, stable canonical schema for the gateway's native tools. Mirrors
/// `config::default_enabled_tool_names()` and is sufficient for OpenAI-format
/// tools[] consumption.
fn config_native_tools_schema() -> Vec<Value> {
    let names = crate::config::default_enabled_tool_names();
    names
        .into_iter()
        .map(|name| {
            json!({
                "type": "function",
                "function": {
                    "name": name,
                    "description": format!("gateway-native tool: {name}"),
                    "parameters": {"type": "object", "properties": {}, "additionalProperties": true}
                }
            })
        })
        .collect()
}
