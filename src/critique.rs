use reqwest::Client;
use serde_json::{json, Value};
use std::time::Duration;
use tokio::time::timeout;

use crate::brain::{self, BrainResponse};
use crate::config::Config;
use crate::security::Constitutions;

const CRITIQUE_TIMEOUT_SECS: u64 = 10;
const FIX_TIMEOUT_SECS: u64 = 30;

pub async fn maybe_critique(
    client: &Client,
    config: &Config,
    constitutions: &Constitutions,
    response: &Value,
    temperature: f64,
) -> (Value, bool) {
    let content = match brain::extract_content(response) {
        Some(c) => c.to_string(),
        None => return (response.clone(), false),
    };

    if !content.contains("```") {
        return (response.clone(), false);
    }

    let critique_messages = vec![
        json!({
            "role": "system",
            "content": format!("{}\n\n{}", constitutions.soul, constitutions.agent)
        }),
        json!({
            "role": "user",
            "content": format!(
                "Review this code for bugs, security issues, and correctness. List any problems found:\n\n{}",
                content
            )
        }),
    ];

    let critique_result = timeout(
        Duration::from_secs(CRITIQUE_TIMEOUT_SECS),
        brain::infer(
            client,
            config,
            critique_messages,
            temperature,
            512,
            false,
            json!({}),
        ),
    )
    .await;

    let critique_text = match critique_result {
        Ok(Ok(BrainResponse::Complete(v))) => match brain::extract_content(&v) {
            Some(t) => t.to_string(),
            None => return (response.clone(), false),
        },
        _ => return (response.clone(), false),
    };

    let has_issues = critique_text.to_lowercase().contains("bug")
        || critique_text.to_lowercase().contains("error")
        || critique_text.to_lowercase().contains("issue")
        || critique_text.to_lowercase().contains("problem")
        || critique_text.to_lowercase().contains("vulnerability");

    if !has_issues {
        return (response.clone(), false);
    }

    let fix_messages = vec![
        json!({
            "role": "system",
            "content": format!("{}\n\n{}", constitutions.soul, constitutions.agent)
        }),
        json!({
            "role": "user",
            "content": format!(
                "Fix the issues identified: {}\n\nOriginal code:\n{}\n\nOutput only the corrected code.",
                critique_text, content
            )
        }),
    ];

    let fix_result = timeout(
        Duration::from_secs(FIX_TIMEOUT_SECS),
        brain::infer(
            client,
            config,
            fix_messages,
            temperature,
            2048,
            false,
            json!({}),
        ),
    )
    .await;

    match fix_result {
        Ok(Ok(BrainResponse::Complete(fixed))) => (fixed, true),
        _ => (response.clone(), false),
    }
}
