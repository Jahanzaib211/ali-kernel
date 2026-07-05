use axum::{
    body::Body,
    extract::{Query, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use reqwest::Client;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio_stream::StreamExt;
use tracing::info;
use uuid::Uuid;

mod anthropic_proxy;
mod brain;
mod compress;
mod config;
mod context;
mod critique;
mod devloop;
mod evals;
mod events;
mod handshake;
mod human_gate;
mod identity;
mod intent;
mod journal;
mod logging;
mod memory;
mod passes;
mod registry;
mod router;
mod routing;
mod security;
mod session;
mod tokens;
mod tools;
mod training;
mod usage_ledger;

use config::AliConfig as Config;
use security::Constitutions;

const MAX_TOOL_ROUNDS: usize = 4;
// Single source of truth for the self-reported API version (independent of
// Cargo.toml's package version) — /health and /benchmark had drifted out of
// sync (2.1.0 vs a stale 1.0.0) before this was pulled into one constant.
const GATEWAY_VERSION: &str = "2.1.0";

#[derive(Debug, serde::Deserialize, Default)]
struct EvalParams {
    /// Cap on the number of tasks run. `None` falls back to the handler default.
    /// Clamped server-side to `evals::MAX_LIMIT` (20).
    limit: Option<usize>,
}

#[derive(Clone)]
struct AppState {
    config: Arc<Config>,
    constitutions: Arc<Constitutions>,
    client: Arc<Client>,
    human_gate: human_gate::HumanGate,
    journal: journal::Journal,
    intent: Arc<intent::Intent>,
    identity: Arc<identity::IdentityStore>,
    ledger: Arc<usage_ledger::UsageLedger>,
    /// L6 model registry: per-(id→spec) cognitive treatment. Loaded once
    /// at startup from ALI_MODELS_TOML (defaults to
    /// `~/.config/ali-kernel/models.toml`) plus the baked-in defaults
    /// shipped with the binary. See `src/registry.rs`.
    model_registry: Arc<registry::ModelRegistry>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config = Config::load()?;

    // Create log dir BEFORE tracing init so we can tee to it
    logging::init_log_dir(&config.log_dir)?;
    training::init_training_dir(&config.training_dir)?;
    // Elided middles feed the future semantic-compressor training corpus.
    compress::set_mining_path(config.training_dir.join("mined-middles.jsonl"));

    // Tee writer: all tracing output goes to both stdout AND logs/gateway.log
    use tracing_subscriber::fmt::writer::MakeWriterExt;
    let gw_log_path = config.log_dir.join("gateway.log");
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&gw_log_path)?;
    let writer = std::io::stdout.and(file);

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "ali_kernel=info".parse().unwrap()),
        )
        .with_writer(writer)
        .init();

    info!(
        "Config loaded — gateway port {}, llama port {}",
        config.gateway_port, config.llama_port
    );

    config.check_backend().await?;
    info!(
        "Backend health: OK — {} (model {})",
        config.backend_base(),
        config.backend_model
    );

    security::attest_binary(&config)?;
    let constitutions = security::verify_constitutions(&config)?;

    // Initialize the nervous system
    let journal = journal::Journal::new(&config)?;
    let intent = Arc::new(intent::Intent::new(&config)?);
    intent.load_last().await?;
    let identity = Arc::new(identity::IdentityStore::new(&config)?);
    identity.load().await?;

    let client = Client::builder()
        .timeout(std::time::Duration::from_secs(130))
        .build()?;

    // L6 Model Registry — load early so startup fails loudly if a
    // mandatory model spec can't be parsed. The path is overridable via
    // ALI_MODELS_TOML. Default: ~/.config/ali-kernel/models.toml.
    let models_toml_path = std::env::var("ALI_MODELS_TOML")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| {
            dirs::config_dir()
                .unwrap_or_else(|| PathBuf::from("/tmp"))
                .join("ali-kernel")
                .join("models.toml")
        });
    if !models_toml_path.exists() {
        // Best-effort touch — operators get an empty file to edit.
        if let Some(parent) = models_toml_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(&models_toml_path, "");
    }
    let model_registry = Arc::new(registry::ModelRegistry::load(&models_toml_path));
    tracing::info!(
        "Model registry loaded from {} ({} entries): {:?}",
        models_toml_path.display(),
        model_registry.ids().len(),
        model_registry.ids()
    );

    let state = AppState {
        config: Arc::new(config.clone()),
        constitutions: Arc::new(constitutions),
        client: Arc::new(client),
        human_gate: human_gate::HumanGate::new(),
        journal,
        intent: intent.clone(),
        identity: identity.clone(),
        ledger: Arc::new(usage_ledger::UsageLedger::new(
            config.log_dir.join("usage.jsonl"),
        )),
        model_registry,
    };

    let app = Router::new()
        .route("/health", get(health_handler))
        .route("/v1/handshake", get(handshake_handler))
        .route("/v1/session", post(session_handler))
        .route("/v1/models", get(models_handler))
        .route("/v1/chat/completions", post(chat_handler))
        .route("/v1/messages", post(anthropic_proxy::messages_handler))
        .route("/v1/mcp/tools", get(mcp_tools_handler))
        .route("/v1/events", get(events_handler))
        .route("/stats", get(usage_ledger::stats_handler))
        .route("/v1/approvals", post(approval_handler))
        .route("/v1/approvals/:request_id", get(approval_status_handler))
        .route("/benchmark", get(benchmark_handler))
        .route("/eval", get(eval_handler))
        .route("/feedback", post(feedback_handler))
        .with_state(state);

    // gateway_host defaults to 127.0.0.1; set 0.0.0.0 in config to serve
    // Docker containers (e.g. via host.docker.internal:9000).
    let addr = format!("{}:{}", config.gateway_host, config.gateway_port);
    info!("Ali Kernel listening on {addr}");

    let listener = tokio::net::TcpListener::bind(&addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

async fn health_handler(State(state): State<AppState>) -> impl IntoResponse {
    let backend_ok = state.config.check_backend().await.is_ok();
    Json(json!({
        "status": "alive",
        "version": GATEWAY_VERSION,
        "capabilities": [
            "rag_memory", "behavioral_shield", "agentic_tools",
            "crate_aware_tools", "eval_harness", "rlaif_capture",
            "human_gate", "streaming", "long_ctx_l55", "mcp_tools",
        ],
        "backend": if backend_ok { "healthy" } else { "unreachable" },
        "backend_url": state.config.backend_base(),
        "backend_local": state.config.backend_url.contains("127.0.0.1"),
        "model": state.config.backend_model,
    }))
}

async fn models_handler(State(state): State<AppState>) -> impl IntoResponse {
    Json(json!({
        "object": "list",
        "data": [{
            "id": state.config.backend_model,
            "object": "model",
            "owned_by": "ali-kernel",
            "created": 1700000000
        }]
    }))
}

/// GET /v1/handshake — discovery manifest. No auth required.
async fn handshake_handler(State(state): State<AppState>) -> impl IntoResponse {
    let mut m = handshake::manifest(&state.config);
    // Embed the loaded model registry so clients can pick an id they
    // know is wired up. The `default` slot is always present (Registry
    // Fallback per the v3.1 plan).
    if let Value::Object(ref mut map) = m {
        map.insert(
            "available_models".into(),
            handshake::manifest_models(&state.model_registry),
        );
    }
    Json(m)
}

/// POST /v1/session — mint a short-lived JWT scoped to a tool mode and feature list.
async fn session_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    // Best-effort: validate an existing JWT, but issuance is allowed without one.
    let key = match crate::session::resolve_signing_key(&state.config) {
        Some(k) => k,
        None => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({
                    "error": "session_keys_unset",
                    "message":
                        "Set ALI_SESSION_KEY (preferred) or OH_SECRET_KEY in the environment \
                         before requesting a session token. To enable a development fallback \
                         key, set [dev_insecure_auth_ok] = true in your .ali.toml."
                })),
            )
                .into_response();
        }
    };

    let req: handshake::SessionRequest = match serde_json::from_value(body) {
        Ok(r) => r,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({
                    "error": "bad_request",
                    "message": e.to_string(),
                    "schema": {
                        "tool_mode": "passthrough | native | hybrid (optional; default = config passthrough_tools or native)",
                        "ttl_secs":  "60..86400 (optional; default = 3600)",
                        "features":  "array<feature-name> (optional; default = all)"
                    }
                })),
            )
                .into_response();
        }
    };

    let ttl = crate::session::clamp_ttl(req.ttl_secs.unwrap_or(crate::session::default_ttl()));
    let now = chrono::Utc::now().timestamp();
    let exp = now + ttl;
    let resolved = handshake::resolve_request(&state.config, &headers);
    let tool_mode = req.tool_mode.unwrap_or(resolved.tool_mode);
    let features = req.features.unwrap_or_else(|| {
        vec![
            "rag_memory".into(),
            "behavioral_shield".into(),
            "agentic_tools".into(),
            "crate_aware_tools".into(),
            "streaming".into(),
            "long_ctx_l55".into(),
        ]
    });
    let claims = crate::session::SessionClaims {
        sub: Uuid::new_v4().to_string(),
        tool_mode,
        features: features.clone(),
        exp,
        iat: now,
        jti: Uuid::new_v4().to_string(),
    };

    let token = match crate::session::encode_session(&claims, &key) {
        Ok(t) => t,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": "encode_failed", "message": e.to_string()})),
            )
                .into_response();
        }
    };

    Json(handshake::SessionResponse {
        session_token: token,
        expires_at: chrono::DateTime::<chrono::Utc>::from_timestamp(exp, 0)
            .unwrap_or_else(chrono::Utc::now),
        tool_mode,
        features,
    })
    .into_response()
}

async fn chat_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    let request_id = Uuid::new_v4().to_string();
    let start = Instant::now();

    // Resolve per-request tool mode + session BEFORE the L3 routing decision so
    // the constitution layer, system prompt, and tool merge are all consistent
    // with the same mode.
    let resolved = handshake::resolve_request(&state.config, &headers);

    let messages = match body.get("messages").and_then(|m| m.as_array()) {
        Some(m) if !m.is_empty() => m.clone(),
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({
                    "error": "messages array is required and must not be empty"
                })),
            )
                .into_response();
        }
    };

    let temperature = body.get("temperature").and_then(|v| v.as_f64());
    let max_tokens = body
        .get("max_tokens")
        .and_then(|v| v.as_u64())
        .unwrap_or(2048);
    let stream = body
        .get("stream")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    // Pillar 2: client profile — emitted as a tracing event so the
    // gateway's pipeline log records who is talking without changing
    // dispatch behaviour yet (the Integration commit will route
    // always_compress_system into L5.5).
    let request_model = body
        .get("model")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    tracing::info!(
        "PIPE L0 client=\"{}\" source={:?} format={:?} request_model={:?}",
        resolved.client.name,
        resolved.client.source,
        resolved.client.request_format,
        request_model
    );

    let human_gate = body
        .get("human_gate")
        .and_then(|v| v.as_bool())
        .unwrap_or(state.config.human_gate_enabled);

    // L3: Semantic routing
    let last_user_msg = messages
        .iter()
        .rev()
        .find(|m| m.get("role").and_then(|r| r.as_str()) == Some("user"))
        .and_then(|m| m.get("content").and_then(|c| c.as_str()))
        .unwrap_or("");

    // Nervous system: Intent classification (limbic — goal tracking)
    let _intent_activated = state.intent.classify(last_user_msg).await;
    if let Some(ctx) = state.intent.get_context().await {
        tracing::info!("INTENT state={} goal={}", ctx.state, &ctx.goal);
    }

    // Nervous system: Begin session journal entry (hippocampus)
    let session_name = format!(
        "{}_{}",
        chrono::Utc::now().format("%Y-%m-%d"),
        last_user_msg
            .split_whitespace()
            .take(3)
            .collect::<Vec<_>>()
            .join("-")
    );
    state
        .journal
        .begin_session(session_name.clone(), last_user_msg.to_string())
        .await;

    let routing = router::classify(last_user_msg);
    let final_temp = temperature.unwrap_or(routing.temperature);
    let route_name = format!("{:?}", routing.strategy).to_lowercase();
    let user_prompt = last_user_msg.to_string();
    tracing::info!("PIPE L3 route={route_name} temp={final_temp:.1}");

    // L4: Episodic memory retrieval
    let memories = memory::retrieve(&user_prompt, &state.config).await;
    tracing::info!("PIPE L4 memories={}", memories.len());

    // L5: Context assembly (adaptive — now includes intent + identity)
    let assembled = context::assemble(
        &state.constitutions,
        &memories,
        &state.config.system_prompt,
        &messages,
        state.intent.clone(),
        state.identity.clone(),
        resolved.tool_mode,
    )
    .await;
    let assembled_for_loop = assembled.clone();
    tracing::info!("PIPE L5 messages={}", assembled.len());

    // Build extra fields (tools, tool_choice, etc.) honouring the resolved
    // tool mode: passthrough (caller wins), native (gateway wins), hybrid (merged).
    let user_tools = body.get("tools");
    let user_choice = body.get("tool_choice");
    let merged = handshake::merge_tools(resolved.tool_mode, user_tools, user_choice);
    let extra = if let Value::Object(map) = merged {
        Value::Object(map)
    } else {
        json!({})
    };
    tracing::info!(
        "tool_mode={:?} disabled_features={:?}",
        resolved.tool_mode,
        resolved.disabled_features,
    );
    if let Some(ref session) = resolved.session {
        tracing::info!("session_sub={} jti={}", session.sub, session.jti,);
    }

    // L6: Brain inference
    match brain::infer(
        &state.client,
        &state.config,
        assembled,
        final_temp,
        max_tokens,
        stream,
        extra,
    )
    .await
    {
        Err(e) => {
            tracing::error!("Brain error: {e}");
            (
                StatusCode::BAD_GATEWAY,
                Json(json!({"error": e.to_string()})),
            )
                .into_response()
        }

        Ok(brain::BrainResponse::Streaming(s)) => {
            // SSE streaming passthrough
            let mapped = s.map(|chunk| {
                chunk.map(|b| {
                    let text = String::from_utf8_lossy(&b);
                    // Forward raw SSE chunks as-is (already "data: ...\n\n" from llama-server)
                    bytes::Bytes::from(text.into_owned())
                })
            });
            let body = Body::from_stream(mapped);
            Response::builder()
                .status(200)
                .header(header::CONTENT_TYPE, "text/event-stream")
                .header(header::CACHE_CONTROL, "no-cache")
                .header("X-Request-Id", &request_id)
                .body(body)
                .unwrap()
        }

        Ok(brain::BrainResponse::Complete(response)) => {
            // L7: Agentic tool loop — execute tools, feed results back, repeat.
            // Shield: tool-call XML is normalized into structured tool_calls inside the loop.
            let (response, tool_called, toolcall_normalized) = run_agentic_loop(
                &state,
                response,
                assembled_for_loop,
                final_temp,
                max_tokens,
                human_gate,
                request_id.clone(),
            )
            .await;

            // Capture pre-critique content (think-stripped) so a rewrite becomes a clean DPO pair
            let original_content =
                brain::strip_think(brain::extract_content(&response).unwrap_or("")).0;

            // L8: Critique loop (strict-but-honest)
            let (mut response, critique_applied) = critique::maybe_critique(
                &state.client,
                &state.config,
                &state.constitutions,
                &response,
                final_temp,
            )
            .await;

            // Shield: strip any leaked <think> from the harness-facing output and write it back.
            let (final_content, think_stripped) =
                brain::strip_think(brain::extract_content(&response).unwrap_or(""));
            if think_stripped {
                if let Some(msg) = response.pointer_mut("/choices/0/message") {
                    msg["content"] = Value::String(final_content.clone());
                }
            }

            // RLAIF data capture (Step 2): SFT always, DPO when critique rewrote
            if let Err(e) = training::log_sft(
                &state.config.training_dir,
                &user_prompt,
                &final_content,
                &route_name,
            ) {
                tracing::warn!("SFT log failed: {e}");
            }
            if critique_applied && original_content != final_content {
                if let Err(e) = training::log_dpo(
                    &state.config.training_dir,
                    &user_prompt,
                    &final_content,
                    &original_content,
                ) {
                    tracing::warn!("DPO log failed: {e}");
                }
            }

            // L4 write-back: store interaction in episodic memory
            memory::store(&state.config, &user_prompt, &final_content).await;

            // Nervous system: End journal session (hippocampus)
            let _entry = state
                .journal
                .end_session(if tool_called {
                    journal::EmotionalTone::Focused
                } else {
                    journal::EmotionalTone::Neutral
                })
                .await;

            // Nervous system: End intent session (limbic)
            state.intent.on_session_end(tool_called).await;

            // L9: Logging
            let latency_ms = start.elapsed().as_millis() as u64;
            tracing::info!("PIPE L8 critique={critique_applied} normalized={toolcall_normalized} think_stripped={think_stripped}");
            tracing::info!(
                "PIPE L9 latency_ms={} tokens_in={} tokens_out={}",
                latency_ms,
                response
                    .get("usage")
                    .and_then(|u| u.get("prompt_tokens"))
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0),
                response
                    .get("usage")
                    .and_then(|u| u.get("completion_tokens"))
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0)
            );
            let log_entry = logging::make_entry(
                &request_id,
                &route_name,
                &response,
                latency_ms,
                tool_called,
                critique_applied,
                think_stripped,
                toolcall_normalized,
            );
            if let Err(e) = logging::log_request(&state.config.log_dir, log_entry) {
                tracing::warn!("Log write failed: {e}");
            }

            let _ = headers; // auth header forwarded to llama-server implicitly
            Response::builder()
                .status(200)
                .header(header::CONTENT_TYPE, "application/json")
                .header("X-Request-Id", &request_id)
                .body(Body::from(serde_json::to_string(&response).unwrap()))
                .unwrap()
        }
    }
}

/// L7 agentic loop: while the model emits tool_calls, execute them, append
/// results as `role:"tool"` messages, and re-infer. Bounded by MAX_TOOL_ROUNDS.
/// If human_gate is true, pauses before executing each tool and waits for approval
/// via the /v1/approvals endpoint.
async fn run_agentic_loop(
    state: &AppState,
    first_response: Value,
    mut conversation: Vec<Value>,
    temperature: f64,
    max_tokens: u64,
    human_gate: bool,
    request_id: String,
) -> (Value, bool, bool) {
    let mut response = first_response;
    let mut tool_called = false;
    let mut normalized = false;

    for _ in 0..MAX_TOOL_ROUNDS {
        // Shield: rewrite any XML-style tool call into structured tool_calls first.
        if tools::normalize_tool_calls(&mut response) {
            normalized = true;
        }
        let has_tool_calls = response
            .get("choices")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("message"))
            .and_then(|m| m.get("tool_calls"))
            .and_then(|t| t.as_array())
            .map(|a| !a.is_empty())
            .unwrap_or(false);

        if !has_tool_calls {
            break;
        }
        tool_called = true;

        // Append the assistant turn that requested the tools
        if let Some(msg) = response
            .get("choices")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("message"))
        {
            conversation.push(msg.clone());
        }

        // Extract tool calls for human gate if enabled
        if human_gate {
            let pending_tools = human_gate::extract_tool_calls(&response);
            if !pending_tools.is_empty() {
                // Register pending approval
                state
                    .human_gate
                    .register(request_id.clone(), pending_tools.clone())
                    .await;

                // Log the pending event
                for tool in &pending_tools {
                    tracing::info!(
                        "PIPE L7 human_gate_pending tool={} risk={} request_id={}",
                        tool.tool_name,
                        tool.risk,
                        request_id
                    );
                }

                // Wait for approval (polling with timeout)
                let timeout_secs = 300u64; // 5 minute approval window
                let start = std::time::Instant::now();
                loop {
                    if start.elapsed().as_secs() > timeout_secs {
                        tracing::warn!("Human gate timeout for request_id={}", request_id);
                        state.human_gate.remove(&request_id).await;
                        return (response, tool_called, normalized);
                    }
                    if state.human_gate.is_resolved(&request_id).await {
                        break;
                    }
                    tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;
                }

                // Build tool results based on approval decisions
                let mut results: Vec<Value> = Vec::new();
                for (i, tool) in pending_tools.iter().enumerate() {
                    let approval = state.human_gate.approve(&request_id, i, None).await;
                    match approval {
                        Some(human_gate::ApprovalResult::Approved { modified_args }) => {
                            // Execute the approved tool with original or modified args
                            let args = modified_args.unwrap_or_else(|| tool.args.clone());
                            let result =
                                tools::dispatch_single_tool(&tool.tool_name, &args, &state.config)
                                    .await;
                            let success = result.is_ok();
                            let raw = match result {
                                Ok(output) => output,
                                Err(e) => format!("Tool error: {e}"),
                            };
                            // L5.5: token-budgeted compression — exact cl100k_base.
                            let compressed = crate::compress::compress_observation_by_tokens(
                                &raw,
                                crate::compress::DEFAULT_OBS_BUDGET_TOKENS,
                            );

                            // Nervous system: record tool in journal
                            state
                                .journal
                                .record_tool(
                                    tool.tool_name.clone(),
                                    success,
                                    raw.chars().take(200).collect(),
                                )
                                .await;
                            state.journal.record_file(tool.tool_name.clone()).await;

                            results.push(json!({
                                "tool_call_id": format!("call_{}", i),
                                "role": "tool",
                                "name": tool.tool_name,
                                "content": compressed
                            }));
                            tracing::info!(
                                "PIPE L7 human_gate_approved tool={} request_id={}",
                                tool.tool_name,
                                request_id
                            );
                        }
                        Some(human_gate::ApprovalResult::Denied) | None => {
                            // Return denial as tool error result
                            results.push(json!({
                                "tool_call_id": format!("call_{}", i),
                                "role": "tool",
                                "name": tool.tool_name,
                                "content": format!("Tool `{}` execution denied by user.", tool.tool_name)
                            }));
                            tracing::info!(
                                "PIPE L7 human_gate_denied tool={} request_id={}",
                                tool.tool_name,
                                request_id
                            );
                        }
                    }
                }

                // Inject tool results and clean up
                state.human_gate.remove(&request_id).await;
                for r in results {
                    conversation.push(r);
                }

                // Re-infer with tool outputs in context
                match brain::infer(
                    &state.client,
                    &state.config,
                    conversation.clone(),
                    temperature,
                    max_tokens,
                    false,
                    serde_json::json!({}),
                )
                .await
                {
                    Ok(brain::BrainResponse::Complete(next)) => response = next,
                    _ => return (response, tool_called, normalized),
                }
                continue;
            }
        }

        // Execute tools — execute_tools attaches "_tool_results"
        let enriched = tools::execute_tools(response.clone(), &state.config).await;
        let results = enriched
            .get("_tool_results")
            .and_then(|r| r.as_array())
            .cloned()
            .unwrap_or_default();

        // Nervous system: update intent state after tool round
        if let Some(r) = results.first() {
            let name = r.get("name").and_then(|n| n.as_str()).unwrap_or("");
            let args = r.get("content").and_then(|c| c.as_str()).unwrap_or("");
            let success = !r
                .get("content")
                .and_then(|c| c.as_str())
                .map(|s| s.starts_with("Tool error"))
                .unwrap_or(false);
            state.intent.on_tool_round(name, args, success).await;
            if !success {
                state
                    .journal
                    .record_frustration(format!(
                        "tool {} failed: {}",
                        name,
                        &args[..args.len().min(100)]
                    ))
                    .await;
            } else {
                state
                    .journal
                    .record_win(format!("tool {} succeeded", name))
                    .await;
            }
        }

        if results.is_empty() {
            return (enriched, tool_called, normalized);
        }
        for r in results {
            conversation.push(r);
        }

        // Re-infer with tool outputs in context
        match brain::infer(
            &state.client,
            &state.config,
            conversation.clone(),
            temperature,
            max_tokens,
            false,
            serde_json::json!({}),
        )
        .await
        {
            Ok(brain::BrainResponse::Complete(next)) => response = next,
            _ => break,
        }
    }

    (response, tool_called, normalized)
}

async fn approval_handler(State(state): State<AppState>, Json(body): Json<Value>) -> Response {
    let request_id = match body.get("request_id").and_then(|v| v.as_str()) {
        Some(id) => id.to_string(),
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": "request_id required"})),
            )
                .into_response();
        }
    };

    let action = match body.get("action").and_then(|v| v.as_str()) {
        Some(a) => a,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": "action required"})),
            )
                .into_response();
        }
    };

    let tool_index = body
        .get("tool_index")
        .and_then(|v| v.as_u64())
        .map(|v| v as usize);

    match action {
        "approve" => {
            let modified_args = body.get("modification").cloned();
            if let Some(idx) = tool_index {
                match state
                    .human_gate
                    .approve(&request_id, idx, modified_args)
                    .await
                {
                    Some(_) => {
                        tracing::info!("HG approve: request_id={} tool_index={}", request_id, idx);
                        Json(json!({"status": "ok", "request_id": request_id, "executed": true, "denied": false}))
                            .into_response()
                    }
                    None => (
                        StatusCode::NOT_FOUND,
                        Json(json!({"error": "pending approval not found"})),
                    )
                        .into_response(),
                }
            } else {
                (
                    StatusCode::BAD_REQUEST,
                    Json(json!({"error": "tool_index required for approve"})),
                )
                    .into_response()
            }
        }
        "deny" => {
            if let Some(idx) = tool_index {
                match state.human_gate.deny(&request_id, idx).await {
                    Some(_) => {
                        tracing::info!("HG deny: request_id={} tool_index={}", request_id, idx);
                        Json(json!({"status": "ok", "request_id": request_id, "executed": false, "denied": true}))
                            .into_response()
                    }
                    None => (
                        StatusCode::NOT_FOUND,
                        Json(json!({"error": "pending approval not found"})),
                    )
                        .into_response(),
                }
            } else {
                (
                    StatusCode::BAD_REQUEST,
                    Json(json!({"error": "tool_index required for deny"})),
                )
                    .into_response()
            }
        }
        _ => (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "action must be 'approve' or 'deny'"})),
        )
            .into_response(),
    }
}

async fn approval_status_handler(
    State(state): State<AppState>,
    axum::extract::Path(request_id): axum::extract::Path<String>,
) -> Response {
    match state.human_gate.get_pending(&request_id).await {
        Some(tools) => Json(json!({
            "request_id": request_id,
            "pending": true,
            "tools": tools,
        }))
        .into_response(),
        None => Json(json!({
            "request_id": request_id,
            "pending": false,
            "tools": [],
        }))
        .into_response(),
    }
}

async fn benchmark_handler(State(state): State<AppState>) -> impl IntoResponse {
    let prompts = vec![
        "Write a binary search in Rust",
        "Explain TCP/IP in one sentence",
        "What is 2+2?",
        "Write a Python HTTP server",
        "List 3 sorting algorithms",
    ];

    let mut results = Vec::new();

    for prompt in prompts {
        let start = Instant::now();
        let messages = vec![json!({"role": "user", "content": prompt})];
        let assembled = context::assemble(
            &state.constitutions,
            &[],
            &state.config.system_prompt,
            &messages,
            state.intent.clone(),
            state.identity.clone(),
            crate::session::ToolMode::Native,
        )
        .await;

        let ok = brain::infer(
            &state.client,
            &state.config,
            assembled,
            0.6,
            256,
            false,
            json!({}),
        )
        .await
        .is_ok();

        results.push(json!({
            "prompt": prompt,
            "latency_ms": start.elapsed().as_millis(),
            "ok": ok
        }));
    }

    Json(json!({
        "benchmark": results,
        "model": "ornith",
        "gateway_version": GATEWAY_VERSION
    }))
}

/// GET /eval — run the benchmark, self-grade, persist score. The proof engine.
///
/// Returns a JSON envelope within `EVAL_TIMEOUT_SECS`:
///   200 + the report on success,
///   408 + `{"error":"timeout", …}` if the suite stalls,
///   500 + `{"error":…}` on internal failure.
/// Optional query params: `?limit=N` clamps the number of tasks run (default 5,
/// max 20). Smaller N → faster response, fewer scores. Useful for `/health`-style
/// smoke runs.
async fn eval_handler(State(state): State<AppState>, Query(params): Query<EvalParams>) -> Response {
    const EVAL_TIMEOUT_SECS: u64 = 60;
    let limit = params.limit.unwrap_or(5);
    let limit = limit.clamp(1, evals::MAX_LIMIT);

    let client = state.client.clone();
    let config = state.config.clone();
    let constitutions = state.constitutions.clone();
    let fut = tokio::spawn(async move {
        evals::run_suite_with_limit(&client, &config, &constitutions, Some(limit)).await
    });

    match tokio::time::timeout(Duration::from_secs(EVAL_TIMEOUT_SECS), fut).await {
        Ok(Ok(Ok(report))) => Json(json!(serde_json::to_value(&report).unwrap())).into_response(),
        Ok(Ok(Err(e))) => {
            tracing::error!("Eval failed: {e}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": "evals_failed", "message": e.to_string()})),
            )
                .into_response()
        }
        Ok(Err(join_err)) => {
            tracing::error!("Eval task panicked: {join_err}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": "evals_panicked", "message": join_err.to_string()})),
            )
                .into_response()
        }
        Err(_elapsed) => {
            tracing::warn!("Eval exceeded {EVAL_TIMEOUT_SECS}s budget; returning 408");
            (
                StatusCode::REQUEST_TIMEOUT,
                Json(json!({
                    "error": "timeout",
                    "message": format!(
                        "eval suite did not complete within {EVAL_TIMEOUT_SECS}s; \
                         partial results discarded. Re-run with a leaner suite or longer budget."
                    ),
                    "timeout_secs": EVAL_TIMEOUT_SECS,
                })),
            )
                .into_response()
        }
    }
}
/// GET /v1/mcp/tools — Model Context Protocol catalog. Returns the gateway
/// native tool inventory as JSON-Schema, so any MCP client (headroom,
/// serena, custom) can list tools without an OpenAI-format chat round-trip.
async fn mcp_tools_handler(State(state): State<AppState>) -> Response {
    let names = crate::config::default_enabled_tool_names();
    let mut tools = Vec::with_capacity(names.len());
    for n in names {
        tools.push(serde_json::json!({
            "name": n,
            "description": format!("gateway-native tool: {n}"),
            "inputSchema": {"type":"object","properties":{},"additionalProperties":true}
        }));
    }
    let body = serde_json::json!({
        "schema_version": "v1",
        "server": { "name": "ali-kernel", "version": env!("CARGO_PKG_VERSION") },
        "tools": tools,
    });
    Json(body).into_response()
}

/// GET /v1/events — Server-Sent Events stream of recent gateway pipeline lines
/// (`PIPE L*` and `JOURNAL *`) from `gateway.log`. Consumers (TUI, dashboards)
/// use this instead of polling `/stats`. The stream tails the file live;
/// reconnects resume from the current end-of-file on the next call.
async fn events_handler(State(state): State<AppState>) -> Response {
    use axum::body::Body;
    use axum::http::header;
    use tokio_stream::StreamExt;

    let log_path = std::path::PathBuf::from(&state.config.log_dir).join("gateway.log");
    let stream = async_stream::stream! {
        let mut last_pos: u64 = match std::fs::metadata(&log_path) {
            Ok(m) => m.len(),
            Err(_) => 0,
        };
        loop {
            if let Ok(metadata) = std::fs::metadata(&log_path) {
                let size = metadata.len();
                if size > last_pos {
                        if let Ok(mut f) = std::fs::File::open(&log_path) {
                        use std::io::{Read, Seek};
                        let _ = f.seek(std::io::SeekFrom::Start(last_pos));
                        let mut buf = vec![0u8; (size - last_pos) as usize];
                        if f.read_exact(&mut buf).is_ok() {
                            let chunk = String::from_utf8_lossy(&buf);
                            // ANSI escapes (`\x1b[..m`) come from `tracing` colored
                            // output; strip them so SSE consumers get plain text.
                            let ansi = regex::Regex::new(r"\x1b\[[0-9;?]*[a-zA-Z]").unwrap();
                            for line in chunk.lines() {
                                let line = line.trim_end_matches('\n').trim_end_matches('\r');
                                if line.is_empty() { continue; }
                                let clean = ansi.replace_all(line, "");
                                yield Ok::<_, std::io::Error>(bytes::Bytes::from(
                                    format!("data: {}\n\n", clean)
                                ));
                            }
                        }
                        last_pos = size;
                    }
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }
    };
    let body = Body::from_stream(stream.map(|r| r));
    axum::response::Response::builder()
        .status(200)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .header(header::CACHE_CONTROL, "no-cache")
        .body(body)
        .unwrap()
}

async fn feedback_handler(State(state): State<AppState>, Json(body): Json<Value>) -> Response {
    let prompt = body.get("prompt").and_then(|v| v.as_str()).unwrap_or("");
    let score = body.get("score").and_then(|v| v.as_i64()).unwrap_or(5);
    let gold = body.get("gold").and_then(|v| v.as_str());
    let rejected = body.get("rejected").and_then(|v| v.as_str()).unwrap_or("");

    if prompt.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "prompt required"})),
        )
            .into_response();
    }

    let mut wrote_pair = false;
    if score < 3 {
        if let Some(gold) = gold {
            if let Err(e) = training::log_dpo(&state.config.training_dir, prompt, gold, rejected) {
                tracing::warn!("feedback DPO log failed: {e}");
            } else {
                wrote_pair = true;
            }
        }
    }
    // Strong responses become positive SFT examples
    if score >= 4 {
        if let Some(gold) = gold {
            let _ = training::log_sft(&state.config.training_dir, prompt, gold, "feedback");
        }
    }

    Json(json!({ "accepted": true, "dpo_pair_written": wrote_pair, "score": score }))
        .into_response()
}
