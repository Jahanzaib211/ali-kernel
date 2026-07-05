//! L6-L8 two-pass think-then-answer dispatch with 8s kill-switch.
//!
//! Implementation of the Python `elite_transform.transform_twopass` semantics,
//! in Rust. Lives in its own module so the rest of `brain.rs` stays clean.
//!
//! Most of this module is `#[allow(dead_code)]` until v3.1 integration — the
//! primitives are wired here now so unit tests and the public API are
//! exercisable; main.rs opt-in lands in a follow-up commit.
//!
//! # Flow
//!
//! 1. **Pass 1**: send a short "think" prompt to llama.cpp. Goal: get a plan /
//!    hidden reasoning, NOT a final answer. Capped at 600 tokens.
//! 2. **Kill switch**: if pass-1 takes longer than `PASS1_TIMEOUT_SECS`
//!    seconds, abort and fall back to a single-pass call with the constraint
//!    mirror applied (because the user prompt is still authoritative even
//!    without a prior plan). This keeps total wall-clock bounded.
//! 3. **Pass 2**: build a system message that carries the plan inline, leave
//!    the user spec dead-last in its own user message. This is the
//!    **displacement fix** — the recency-bias anchor is on the user's spec,
//!    not on the planning block.
//! 4. **Empty recovery**: if pass-2 returns empty content (model burned the
//!    budget on reasoning), retry once with `1500` tokens so visible content
//!    has room.
//!
//! ## KV-cache invariant
//!
//! The system preamble is a static `&'static str` (`routing::preamble_for`),
//! and `extract_constraints` is a pure function of the user prompt. Same
//! preamble + same constraint block → same KV-cache prefix → cache hit.
//!
//! ## Cost-based max_tokens
//!
//! Pass 2's `max_tokens` is derived from `pass1_tokens + 400`: the model's
//! reasoning cost on the planning step informs how much room to leave for
//! the answer. Without this, prompts that trigger long thinking ("safety
//! analysis of a fantasy novel scene", for example) return empty content.
#![allow(dead_code)] // dispatch() not yet called from main.rs; v3.1 will opt in.

use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::time::{Duration, Instant};
use tokio::time::timeout;

use crate::brain;
use crate::config::Config;
use crate::routing;

/// Hard ceiling on Pass 1's wall-clock. Beyond this, we abort to single-pass
/// to keep request latency bounded (the user-perceived budget).
pub const PASS1_TIMEOUT_SECS: u64 = 8;

/// Generous retry budget for the empty-content recovery in Pass 2.
const PASS2_EMPTY_RETRY_MAX_TOKENS: u64 = 1500;

/// Per-intent budget applied when no other hint is available. Kept small
/// because two-pass lifts the model's output through reasoning, leaving
/// room for the answer in pass-2's tokens.
const DEFAULT_MAX_TOKENS: u64 = 350;

const PASS1_MAX_TOKENS: u64 = 600;
const PASS2_COST_PAD: u64 = 400;

/// Run two-pass dispatch when `intent` is Diagnostic, DevLoop heavy, or
/// Neutral (i.e., tasks that benefit from planning). For Extraction intents
/// (structured JSON), call `dispatch_single_pass_with_intent` instead — it
/// has tighter latency and no two-pass overhead.
pub async fn dispatch(
    client: &reqwest::Client,
    config: &Config,
    intent: routing::Intent,
    messages: Vec<Value>,
    temperature: f64,
    stream: bool,
    extra: Value,
) -> Result<brain::BrainResponse> {
    let should_two_pass = matches!(
        intent,
        routing::Intent::Diagnostic | routing::Intent::Neutral
    );
    if !should_two_pass {
        return dispatch_single_pass_with_intent(
            client,
            config,
            intent,
            messages,
            temperature,
            stream,
            extra,
        )
        .await;
    }

    let user_msg = last_user_message(&messages).unwrap_or_default();
    if user_msg.is_empty() {
        // No user content to plan against — bypass two-pass.
        return brain::infer(
            client,
            config,
            messages,
            temperature,
            DEFAULT_MAX_TOKENS,
            stream,
            extra,
        )
        .await;
    }

    let sys_existing = system_message_text(&messages);
    let cons = routing::extract_constraints(&user_msg);
    let mx = compute_max_tokens(&user_msg, intent, &cons);

    // ---- Pass 1: ask for a plan, no answer. ----
    let plan_prompt = "Think step by step. Output a brief plan as plain text \
                       starting with 'PLAN:' then stop. Do NOT yet answer the user.";
    let pass1_messages = vec![
        json!({"role": "system", "content": sys_existing}),
        json!({"role": "user",   "content": user_msg_with_constraints(&user_msg, &cons)}),
        json!({"role": "user",   "content": plan_prompt}),
    ];
    let t0 = Instant::now();
    let pass1_result = timeout(
        Duration::from_secs(PASS1_TIMEOUT_SECS),
        call_non_streaming(
            client,
            config,
            pass1_messages,
            temperature,
            PASS1_MAX_TOKENS,
            false,
            Value::Null,
        ),
    )
    .await;
    let elapsed = t0.elapsed();

    let (reasoning, pass1_tokens) = match pass1_result {
        Ok(Ok(v)) => extract_reasoning_and_tokens(&v),
        Ok(Err(e)) => {
            tracing::warn!("two_pass: pass1 call error: {e}; falling back to single-pass");
            return brain::infer(client, config, messages, temperature, mx, stream, extra).await;
        }
        Err(_) => {
            tracing::warn!(
                "two_pass: pass1 timed out after {:?}; falling back to single-pass",
                elapsed
            );
            return brain::infer(client, config, messages, temperature, mx, stream, extra).await;
        }
    };

    if reasoning.trim().is_empty() {
        tracing::info!("two_pass: pass1 returned empty reasoning; falling back to single-pass");
        return brain::infer(client, config, messages, temperature, mx, stream, extra).await;
    }

    // ---- Pass 2: displacement fix — system carries reasoning, user spec last. ----
    let plan_block = format!(
        "[PRIOR REASONING — already planned; do not re-plan; produce the final answer now]\n{}",
        &reasoning[..reasoning.len().min(1200)]
    );
    let sys_with_plan = format!("{}\n\n{}", sys_existing, plan_block);
    let user_dead_last = if cons.is_empty() {
        format!("{user_msg}\n\nNow produce the final answer (not another plan).")
    } else {
        format!("{user_msg}\n\n[REMINDER]\n{cons}")
    };
    let pass2_messages = vec![
        json!({"role": "system", "content": sys_with_plan}),
        json!({"role": "user",   "content": user_dead_last}),
    ];
    // Cost-based mx: pass-1 spent pass1_tokens; pad so pass-2 has room for
    // both residual reasoning AND visible content.
    let mx2 = std::cmp::max(mx, pass1_tokens.saturating_add(PASS2_COST_PAD));

    // extra is used at most once downstream — keep an owned clone available
    // for the empty-retry fallback path.
    let extra_for_retry = extra.clone();
    let pass2_resp = match call_non_streaming(
        client,
        config,
        pass2_messages.clone(),
        temperature,
        mx2,
        stream,
        extra,
    )
    .await
    {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("two_pass: pass2 error: {e}; returning single-pass fallback");
            return brain::infer(
                client,
                config,
                messages,
                temperature,
                mx,
                stream,
                Value::Null,
            )
            .await;
        }
    };

    // Empty recovery: if pass-2 came back empty and we have headroom, retry once.
    let final_resp =
        if extract_visible_content(&pass2_resp).is_empty() && mx2 < PASS2_EMPTY_RETRY_MAX_TOKENS {
            match call_non_streaming(
                client,
                config,
                pass2_messages,
                temperature,
                PASS2_EMPTY_RETRY_MAX_TOKENS,
                stream,
                extra_for_retry,
            )
            .await
            {
                Ok(v) => v,
                Err(_) => pass2_resp,
            }
        } else {
            pass2_resp
        };

    Ok(brain::BrainResponse::Complete(final_resp))
}

/// Single-pass dispatch with intent-aware preamble AND a constraint reminder.
/// Used for `Extraction` (tight budget, JSON output) and as the fallback when
/// the two-pass kill-switch fires.
pub async fn dispatch_single_pass_with_intent(
    client: &reqwest::Client,
    config: &Config,
    intent: routing::Intent,
    messages: Vec<Value>,
    temperature: f64,
    stream: bool,
    extra: Value,
) -> Result<brain::BrainResponse> {
    let user_msg = last_user_message(&messages).unwrap_or_default();
    let mut sys_existing = system_message_text(&messages);
    let preamble = routing::preamble_for(intent);
    if !sys_existing.contains(preamble) {
        sys_existing = format!("{preamble}\n\n{sys_existing}");
    }
    let cons = routing::extract_constraints(&user_msg);

    let mx = compute_max_tokens(&user_msg, intent, &cons);
    let user_block = user_msg_with_constraints(&user_msg, &cons);
    let routed_messages = vec![
        json!({"role": "system", "content": sys_existing}),
        json!({"role": "user",   "content": user_block}),
    ];

    brain::infer(
        client,
        config,
        routed_messages,
        temperature,
        mx,
        stream,
        extra,
    )
    .await
}

// ── Helpers ───────────────────────────────────────────────────────────────

fn user_msg_with_constraints(user_msg: &str, cons: &str) -> String {
    if cons.is_empty() {
        user_msg.to_string()
    } else {
        format!("{user_msg}\n\n[REMINDER]\n{cons}")
    }
}

fn last_user_message(messages: &[Value]) -> Option<String> {
    messages.iter().rev().find_map(|m| {
        if m.get("role").and_then(|r| r.as_str()) == Some("user") {
            m.get("content")
                .and_then(|c| c.as_str())
                .map(|s| s.to_string())
        } else {
            None
        }
    })
}

fn system_message_text(messages: &[Value]) -> String {
    messages
        .iter()
        .find_map(|m| {
            if m.get("role").and_then(|r| r.as_str()) == Some("system") {
                m.get("content")
                    .and_then(|c| c.as_str())
                    .map(|s| s.to_string())
            } else {
                None
            }
        })
        .unwrap_or_default()
}

fn extract_reasoning_and_tokens(v: &Value) -> (String, u64) {
    let empty = String::new();
    let content = v
        .pointer("/choices/0/message/content")
        .and_then(|c| c.as_str())
        .unwrap_or(&empty)
        .to_string();
    let reasoning = v
        .pointer("/choices/0/message/reasoning_content")
        .and_then(|c| c.as_str())
        .unwrap_or(&empty)
        .to_string();
    let combined = format!("{reasoning}\n{content}").trim().to_string();
    let tokens = v
        .pointer("/usage/completion_tokens")
        .and_then(|t| t.as_u64())
        .unwrap_or(0);
    (combined, tokens)
}

fn extract_visible_content(v: &Value) -> String {
    v.pointer("/choices/0/message/content")
        .and_then(|c| c.as_str())
        .unwrap_or("")
        .to_string()
}

fn compute_max_tokens(user_msg: &str, intent: routing::Intent, _cons: &str) -> u64 {
    let pn = crate::tokens::count_tokens(user_msg);
    let base = if pn < 100 {
        std::cmp::max(pn * 6, 200) as u64
    } else if pn < 500 {
        std::cmp::max(pn * 3, 400) as u64
    } else {
        1024
    };
    let cap: u64 = match intent {
        routing::Intent::Diagnostic => 350,
        routing::Intent::Extraction => 400,
        routing::Intent::DevLoop => 1024,
        routing::Intent::Neutral => 800,
    };
    base.min(cap)
}

async fn call_non_streaming(
    client: &reqwest::Client,
    config: &Config,
    messages: Vec<Value>,
    temperature: f64,
    max_tokens: u64,
    stream: bool,
    extra: Value,
) -> Result<Value> {
    let resp = brain::infer(
        client,
        config,
        messages,
        temperature,
        max_tokens,
        stream,
        extra,
    )
    .await?;
    let brain::BrainResponse::Complete(v) = resp else {
        anyhow::bail!("two_pass expected Complete response, got Streaming");
    };
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passthrough_when_no_user_message() {
        // Empty user_msg list should fall back gracefully.
        let v = last_user_message(&[]);
        assert_eq!(v, None);
    }

    #[test]
    fn static_extracts_reasoning_text() {
        let v = json!({
            "choices": [{"message": {
                "reasoning_content": "PLAN: do X",
                "content": ""
            }}],
            "usage": {"completion_tokens": 7}
        });
        let (r, t) = extract_reasoning_and_tokens(&v);
        assert!(r.contains("PLAN"));
        assert_eq!(t, 7);
    }

    #[test]
    fn empty_visible_triggers_retry_when_under_cap() {
        let mx = compute_max_tokens("hi", routing::Intent::Neutral, "");
        assert!(
            mx < PASS2_EMPTY_RETRY_MAX_TOKENS,
            "mx={mx} should be < {PASS2_EMPTY_RETRY_MAX_TOKENS}"
        );
    }
}
