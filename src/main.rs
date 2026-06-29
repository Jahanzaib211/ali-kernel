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
mod logging;
mod memory;
mod router;
mod security;
mod tools;

use config::Config;
use security::Constitutions;

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
    Json(json!({
        "status": "alive",
        "version": "1.0.0",
        "layers": 9,
        "llama_server": if llama_ok { "healthy" } else { "unreachable" }
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

    // L4: Memory (stub)
    let memories = memory::retrieve(last_user_msg, &state.config).await;

    // L5: Context assembly
    let assembled = context::assemble(
        &state.constitutions,
        &memories,
        &state.config.system_prompt,
        &messages,
    );

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

        Ok(brain::BrainResponse::Complete(mut response)) => {
            // L7: Tool execution (passthrough)
            response = tools::execute_tools(response, &state.config).await;

            // L8: Critique loop
            let (response, critique_applied) = critique::maybe_critique(
                &state.client,
                &state.config,
                &state.constitutions,
                &response,
                final_temp,
            ).await;

            // L9: Logging
            let latency_ms = start.elapsed().as_millis() as u64;
            let log_entry = logging::make_entry(
                &request_id,
                &route_name,
                &response,
                latency_ms,
                false,
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
