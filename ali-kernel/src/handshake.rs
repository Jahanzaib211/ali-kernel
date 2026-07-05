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
            "mcp_tools",
            "model_registry"
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

/// Render the model registry as a JSON object: each model id → its spec,
/// plus a `default` slot at the top of the response. Used by `main.rs` to
/// publish the registry on `/v1/handshake`.
pub fn manifest_models(registry: &crate::registry::ModelRegistry) -> Value {
    let mut out = serde_json::Map::new();
    for id in registry.ids() {
        if let Some(spec) = registry.get(&id) {
            out.insert(
                id,
                serde_json::to_value(spec).unwrap_or(serde_json::Value::Null),
            );
        }
    }
    Value::Object(out)
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
    /// Detected client profile (see `ClientProfile`). Defaults to
    /// `Unknown` when no `X-Ali-Client` header and no recognizable
    /// User-Agent substring match.
    pub client: ClientProfile,
}

/// How aggressively the gateway should compress the system prompt before
/// forwarding. Wired into L5.5 in the Integration commit (Pillar 3+).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SystemPromptStrategy {
    /// Gateway owns SOUL/AGENT/OWNER only — the rest is the client's verbatim.
    IdentityOnly,
    /// + L5.5 structural compression even on small system prompts.
    IdentityPlusHistory,
    /// Trust the client's system prompt verbatim.
    PassThrough,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientSource {
    /// Explicit `X-Ali-Client: <name>` header. Wins over User-Agent.
    AliHeader,
    /// Recognizable User-Agent substring match (e.g. "aider" in UA).
    UserAgent,
    /// Body-shape heuristic fallback (currently a placeholder; reserved
    /// for the future body fingerprint path).
    Heuristic,
    /// No match.
    Unknown,
}

/// Per-client profile detected from headers. Operators can override
/// individual fields in `~/.config/ali-kernel/models.toml` (Pillar 2
/// follow-up: per-client overrides section).
#[derive(Debug, Clone)]
pub struct ClientProfile {
    pub name: String,
    pub source: ClientSource,
    /// Default tool mode the gateway should select when no
    /// `X-Ali-Tool-Mode` / session claim / config default wins.
    pub default_tool_mode: ToolMode,
    /// Wire format the client prefers to receive.
    pub request_format: crate::registry::WireFormat,
    /// How L5.5 should handle the system prompt.
    pub system_prompt_strategy: SystemPromptStrategy,
    /// If true, run `compress_observation_by_tokens` over the system
    /// prompt even when the prompt is small.
    pub always_compress_system: bool,
}

impl ClientProfile {
    pub fn unknown() -> Self {
        Self {
            name: "unknown".into(),
            source: ClientSource::Unknown,
            default_tool_mode: ToolMode::Native,
            request_format: crate::registry::WireFormat::OpenAI,
            system_prompt_strategy: SystemPromptStrategy::IdentityOnly,
            always_compress_system: false,
        }
    }
}

/// Built-in table of known agents keyed by canonical name. Detection
/// uses the lowercase name from `X-Ali-Client` or a User-Agent substring
/// match. Operators can add a `[clients.<name>]` section to
/// `~/.config/ali-kernel/models.toml` (Pillar 2 follow-up) to override
/// these defaults.
fn profile_for(name: &str) -> Option<ClientProfile> {
    let p = |name: &str,
             default_mode: ToolMode,
             strategy: SystemPromptStrategy,
             always_compress: bool,
             request_format: crate::registry::WireFormat,
             source: ClientSource| ClientProfile {
        name: name.into(),
        source,
        default_tool_mode: default_mode,
        request_format,
        system_prompt_strategy: strategy,
        always_compress_system: always_compress,
    };
    match name {
        "aider" => Some(p(
            "aider",
            ToolMode::Passthrough,
            SystemPromptStrategy::PassThrough,
            false,
            crate::registry::WireFormat::OpenAI,
            ClientSource::AliHeader,
        )),
        "continue" | "continue-dev" => Some(p(
            "continue",
            ToolMode::Hybrid,
            SystemPromptStrategy::IdentityPlusHistory,
            true,
            crate::registry::WireFormat::OpenAI,
            ClientSource::AliHeader,
        )),
        "claude-code" | "claude" => Some(p(
            "claude-code",
            ToolMode::Passthrough,
            SystemPromptStrategy::IdentityOnly,
            false,
            crate::registry::WireFormat::Anthropic,
            ClientSource::AliHeader,
        )),
        "cursor" => Some(p(
            "cursor",
            ToolMode::Hybrid,
            SystemPromptStrategy::IdentityPlusHistory,
            true,
            crate::registry::WireFormat::OpenAI,
            ClientSource::AliHeader,
        )),
        "openhands" => Some(p(
            "openhands",
            ToolMode::Hybrid,
            SystemPromptStrategy::IdentityPlusHistory,
            true,
            crate::registry::WireFormat::OpenAI,
            ClientSource::AliHeader,
        )),
        "cline" | "kilo" => Some(p(
            name,
            ToolMode::Hybrid,
            SystemPromptStrategy::IdentityPlusHistory,
            true,
            crate::registry::WireFormat::OpenAI,
            ClientSource::AliHeader,
        )),
        _ => None,
    }
}

/// Detect the client profile from request headers. Resolution order:
/// 1. Explicit `X-Ali-Client: <name>` header.
/// 2. User-Agent substring match against the built-in agent table.
/// 3. Unknown fallback.
pub fn detect_client(headers: &HeaderMap) -> ClientProfile {
    // 1. Explicit header.
    if let Some(v) = headers.get("x-ali-client").and_then(|v| v.to_str().ok()) {
        let v = v.trim().to_ascii_lowercase();
        if let Some(mut p) = profile_for(&v) {
            p.source = ClientSource::AliHeader;
            return p;
        }
    }
    // 2. User-Agent substring.
    if let Some(v) = headers
        .get(axum::http::header::USER_AGENT)
        .and_then(|v| v.to_str().ok())
    {
        let v_lower = v.to_ascii_lowercase();
        // Order matters: most specific needle first; the first hit wins.
        for (needle, canonical) in &[
            ("aider-chat", "aider"),
            ("continue-dev", "continue"),
            ("continue.dev", "continue"),
            ("continue cli", "continue"),
            ("claude-code", "claude-code"),
            ("claude-code-cli", "claude-code"),
            ("openhands-agent", "openhands"),
            ("cursor", "cursor"),
            ("cline", "cline"),
            ("kilo", "kilo"),
            // Generic fallbacks (last so the specific ones win).
            ("aider", "aider"),
            ("continue", "continue"),
        ] {
            if v_lower.contains(needle) {
                if let Some(mut p) = profile_for(canonical) {
                    p.source = ClientSource::UserAgent;
                    return p;
                }
            }
        }
    }
    // 3. Unknown fallback.
    ClientProfile::unknown()
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

    // Client profile detection runs LAST so the profile is purely
    // observational here — the tool_mode / disabled_features decision
    // doesn't depend on it. Pillar 3 integration will route the client's
    // `always_compress_system` preference into L5.5.
    let client = detect_client(headers);

    ResolvedRequest {
        tool_mode,
        disabled_features,
        session,
        client,
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

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::{header::USER_AGENT, HeaderMap, HeaderValue};

    fn h_static(name: &'static str, value: &'static str) -> HeaderMap {
        let mut m = HeaderMap::new();
        m.insert(name, HeaderValue::from_static(value));
        m
    }
    fn h_user_agent(value: &'static str) -> HeaderMap {
        h_static(USER_AGENT.as_str(), value)
    }
    fn h_x_ali_client(value: &'static str) -> HeaderMap {
        h_static("x-ali-client", value)
    }

    #[test]
    fn explicit_x_ali_client_wins_over_user_agent() {
        let mut headers = HeaderMap::new();
        headers.insert("x-ali-client", HeaderValue::from_static("aider"));
        headers.insert(USER_AGENT, HeaderValue::from_static("curl/8.0"));
        let p = detect_client(&headers);
        assert_eq!(p.name, "aider");
        assert_eq!(p.source, ClientSource::AliHeader);
        assert_eq!(p.default_tool_mode, ToolMode::Passthrough);
        assert!(matches!(
            p.system_prompt_strategy,
            SystemPromptStrategy::PassThrough
        ));
    }

    #[test]
    fn user_agent_substring_falls_back_to_aider() {
        let headers = h_user_agent("aider-chat/0.86.2 (Linux; Rust)");
        let p = detect_client(&headers);
        assert_eq!(p.name, "aider");
        assert_eq!(p.source, ClientSource::UserAgent);
    }

    #[test]
    fn continue_dev_user_agent_uses_identity_plus_history() {
        let headers = h_user_agent("Continue.dev CLI (1.5.47)");
        let p = detect_client(&headers);
        assert_eq!(p.name, "continue");
        assert!(p.always_compress_system);
        assert!(matches!(
            p.system_prompt_strategy,
            SystemPromptStrategy::IdentityPlusHistory
        ));
    }

    #[test]
    fn claude_code_routes_anthropic_format() {
        let headers = h_user_agent("claude-code/2.0.0");
        let p = detect_client(&headers);
        assert_eq!(p.name, "claude-code");
        assert_eq!(p.request_format, crate::registry::WireFormat::Anthropic);
    }

    #[test]
    fn unknown_user_agent_returns_unknown_profile() {
        let headers = h_user_agent("curl/8.0.1");
        let p = detect_client(&headers);
        assert_eq!(p.name, "unknown");
        assert_eq!(p.source, ClientSource::Unknown);
        assert_eq!(p.default_tool_mode, ToolMode::Native);
        assert!(!p.always_compress_system);
    }

    #[test]
    fn no_headers_returns_unknown() {
        let headers = HeaderMap::new();
        let p = detect_client(&headers);
        assert_eq!(p.source, ClientSource::Unknown);
    }
}
