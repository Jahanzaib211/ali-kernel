use axum::{
    body::Body,
    extract::State,
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use reqwest::Client;
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Instant;
use tokio_stream::StreamExt;
use tower_http::cors::CorsLayer;
use tracing::info;
use uuid::Uuid;

mod brain;
mod config;
mod context;
mod critique;
mod evals;
mod logging;
mod memory;
mod router;
mod security;
mod tools;
mod training;

use config::Config;
use security::Constitutions;

const MAX_TOOL_ROUNDS: usize = 4;

#[derive(Clone)]
struct AppState {
    config: Arc<Config>,
    constitutions: Arc<Constitutions>,
    client: Arc<Client>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "ornith_gateway=info".parse().unwrap()),
        )
        .init();

    let config = Config::load()?;
    info!("Config loaded — gateway port {}, llama port {}", config.gateway_port, config.llama_port);

    config.check_llama_health().await?;
    info!("llama-server health: OK");

    security::attest_binary(&config)?;
    let constitutions = security::verify_constitutions(&config)?;

    logging::init_log_dir(&config.log_dir)?;
    training::init_training_dir(&config.training_dir)?;

    let client = Client::builder()
        .timeout(std::time::Duration::from_secs(130))
        .build()?;

    let state = AppState {
        config: Arc::new(config.clone()),
        constitutions: Arc::new(constitutions),
        client: Arc::new(client),
    };

    let app = Router::new()
        .route("/health", get(health_handler))
        .route("/v1/models", get(models_handler))
        .route("/v1/chat/completions", post(chat_handler))
        .route("/benchmark", get(benchmark_handler))
        .route("/eval", get(eval_handler))
        .route("/feedback", post(feedback_handler))
        .layer(CorsLayer::permissive())
        .with_state(state);

    let addr = format!("0.0.0.0:{}", config.gateway_port);
    info!("Ornith Sovereign Gateway listening on {addr}");

    let listener = tokio::net::TcpListener::bind(&addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

async fn health_handler(State(state): State<AppState>) -> impl IntoResponse {
    let llama_ok = state.config.check_llama_health().await.is_ok();
    let (llama_bin, model_path) = state.config.model_info();
    Json(json!({
        "status": "alive",
        "version": "2.0.0",
        "layers": 9,
        "phase": 2,
        "capabilities": ["rag_memory", "agentic_tools", "eval_harness", "rlaif_capture"],
        "llama_server": if llama_ok { "healthy" } else { "unreachable" },
        "llama_bin": llama_bin.display().to_string(),
        "model": model_path.file_name().and_then(|n| n.to_str()).unwrap_or("unknown")
    }))
}

async fn models_handler() -> impl IntoResponse {
    Json(json!({
        "object": "list",
        "data": [{
            "id": "ornith",
            "object": "model",
            "owned_by": "ornith-gateway",
            "created": 1700000000
        }]
    }))
}

async fn chat_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    let request_id = Uuid::new_v4().to_string();
    let start = Instant::now();

    let messages = match body.get("messages").and_then(|m| m.as_array()) {
        Some(m) if !m.is_empty() => m.clone(),
        _ => {
            return (StatusCode::BAD_REQUEST, Json(json!({
                "error": "messages array is required and must not be empty"
            }))).into_response();
        }
    };

    let temperature = body.get("temperature").and_then(|v| v.as_f64());
    let max_tokens = body.get("max_tokens").and_then(|v| v.as_u64()).unwrap_or(2048);
    let stream = body.get("stream").and_then(|v| v.as_bool()).unwrap_or(false);

    // L3: Semantic routing
    let last_user_msg = messages.iter().rev()
        .find(|m| m.get("role").and_then(|r| r.as_str()) == Some("user"))
        .and_then(|m| m.get("content").and_then(|c| c.as_str()))
        .unwrap_or("");

    let routing = router::classify(last_user_msg);
    let final_temp = temperature.unwrap_or(routing.temperature);
    let route_name = format!("{:?}", routing.strategy).to_lowercase();
    let user_prompt = last_user_msg.to_string();

    // L4: Episodic memory retrieval
    let memories = memory::retrieve(&user_prompt, &state.config).await;

    // L5: Context assembly
    let assembled = context::assemble(
        &state.constitutions,
        &memories,
        &state.config.system_prompt,
        &messages,
    );
    let assembled_for_loop = assembled.clone();

    // Build extra fields (tools, tool_choice, etc.)
    let mut extra = json!({});
    if let Some(tools) = body.get("tools") {
        extra["tools"] = tools.clone();
    }
    if let Some(tc) = body.get("tool_choice") {
        extra["tool_choice"] = tc.clone();
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
    ).await {
        Err(e) => {
            tracing::error!("Brain error: {e}");
            (StatusCode::BAD_GATEWAY, Json(json!({"error": e.to_string()}))).into_response()
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
            // L7: Agentic tool loop — execute tools, feed results back, repeat
            let (response, tool_called) = run_agentic_loop(
                &state,
                response,
                assembled_for_loop,
                final_temp,
                max_tokens,
            ).await;

            // Capture pre-critique content so a rewrite becomes a DPO pair
            let original_content = brain::extract_content(&response)
                .unwrap_or("")
                .to_string();

            // L8: Critique loop
            let (response, critique_applied) = critique::maybe_critique(
                &state.client,
                &state.config,
                &state.constitutions,
                &response,
                final_temp,
            ).await;

            let final_content = brain::extract_content(&response)
                .unwrap_or("")
                .to_string();

            // RLAIF data capture (Step 2): SFT always, DPO when critique rewrote
            if let Err(e) = training::log_sft(
                &state.config.training_dir, &user_prompt, &final_content, &route_name,
            ) {
                tracing::warn!("SFT log failed: {e}");
            }
            if critique_applied && original_content != final_content {
                if let Err(e) = training::log_dpo(
                    &state.config.training_dir, &user_prompt, &final_content, &original_content,
                ) {
                    tracing::warn!("DPO log failed: {e}");
                }
            }

            // L4 write-back: store interaction in episodic memory
            memory::store(&state.config, &user_prompt, &final_content).await;

            // L9: Logging
            let latency_ms = start.elapsed().as_millis() as u64;
            let log_entry = logging::make_entry(
                &request_id,
                &route_name,
                &response,
                latency_ms,
                tool_called,
                critique_applied,
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
async fn run_agentic_loop(
    state: &AppState,
    first_response: Value,
    mut conversation: Vec<Value>,
    temperature: f64,
    max_tokens: u64,
) -> (Value, bool) {
    let mut response = first_response;
    let mut tool_called = false;

    for _ in 0..MAX_TOOL_ROUNDS {
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

        // Execute tools — execute_tools attaches "_tool_results"
        let enriched = tools::execute_tools(response.clone(), &state.config).await;
        let results = enriched
            .get("_tool_results")
            .and_then(|r| r.as_array())
            .cloned()
            .unwrap_or_default();

        if results.is_empty() {
            return (enriched, tool_called);
        }
        for r in results {
            conversation.push(r);
        }

        // Re-infer with tool outputs in context
        match brain::infer(
            &state.client, &state.config, conversation.clone(),
            temperature, max_tokens, false, serde_json::json!({}),
        ).await {
            Ok(brain::BrainResponse::Complete(next)) => response = next,
            _ => break,
        }
    }

    (response, tool_called)
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
        );

        let ok = brain::infer(
            &state.client,
            &state.config,
            assembled,
            0.6,
            256,
            false,
            json!({}),
        ).await.is_ok();

        results.push(json!({
            "prompt": prompt,
            "latency_ms": start.elapsed().as_millis(),
            "ok": ok
        }));
    }

    Json(json!({
        "benchmark": results,
        "model": "ornith",
        "gateway_version": "1.0.0"
    }))
}

/// GET /eval — run the 20-task benchmark, self-grade, persist score. The proof engine.
async fn eval_handler(State(state): State<AppState>) -> Response {
    match evals::run_suite(&state.client, &state.config, &state.constitutions).await {
        Ok(report) => Json(json!(serde_json::to_value(&report).unwrap())).into_response(),
        Err(e) => {
            tracing::error!("Eval failed: {e}");
            (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))).into_response()
        }
    }
}

/// POST /feedback — Claude-as-teacher submits a score and optional gold response.
/// score < 3 with a gold answer writes a DPO preference pair (gold=chosen).
async fn feedback_handler(
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> Response {
    let prompt = body.get("prompt").and_then(|v| v.as_str()).unwrap_or("");
    let score = body.get("score").and_then(|v| v.as_i64()).unwrap_or(5);
    let gold = body.get("gold").and_then(|v| v.as_str());
    let rejected = body.get("rejected").and_then(|v| v.as_str()).unwrap_or("");

    if prompt.is_empty() {
        return (StatusCode::BAD_REQUEST, Json(json!({"error": "prompt required"}))).into_response();
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

    Json(json!({ "accepted": true, "dpo_pair_written": wrote_pair, "score": score })).into_response()
}
