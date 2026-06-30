use reqwest::Client;
use serde_json::{json, Value};
use std::time::Duration;
use tokio::time::timeout;

use crate::brain::{self, BrainResponse};
use crate::config::Config;
use crate::security::Constitutions;

const CRITIQUE_TIMEOUT_SECS: u64 = 20;
const FIX_TIMEOUT_SECS: u64 = 40;

const REVIEWER: &str = "You are a ruthless senior code reviewer. You never praise. You find concrete, \
specific faults: bugs, panics, unwrap/expect on runtime paths, missing error handling, security holes, \
edge cases, and incorrect logic. Output ONLY a JSON object, no prose, in exactly this shape: \
{\"issues\":[{\"severity\":\"low|med|high\",\"detail\":\"...\"}],\"verdict\":\"pass\"|\"fail\"}. \
Use verdict \"fail\" if there is ANY real issue. Only return an empty issues array with verdict \"pass\" \
if the code is genuinely flawless.";

/// L8 strict-but-honest critique. Reviews code answers; on a real, named issue it
/// asks the model to rewrite and returns `(fixed, true)` (a DPO pair upstream).
/// Genuinely-clean code is left untouched — no forced-rewrite noise.
pub async fn maybe_critique(
    client: &Client,
    config: &Config,
    constitutions: &Constitutions,
    response: &Value,
    _temperature: f64,
) -> (Value, bool) {
    let content = match brain::extract_content(response) {
        Some(c) => c.to_string(),
        None => return (response.clone(), false),
    };
    if !content.contains("```") {
        return (response.clone(), false);
    }

    // Grading: thinking off + temp 0 for a terse, deterministic JSON verdict.
    let no_think = json!({ "chat_template_kwargs": { "enable_thinking": false } });
    let critique_messages = vec![
        json!({ "role": "system", "content": REVIEWER }),
        json!({
            "role": "user",
            "content": format!("Review this code. Output only the JSON verdict.\n\n{content}")
        }),
    ];

    let verdict = match run_complete(
        client,
        config,
        critique_messages,
        256,
        no_think.clone(),
        CRITIQUE_TIMEOUT_SECS,
    )
    .await
    .and_then(|t| parse_verdict(&t))
    {
        Some(v) => v,
        None => return (response.clone(), false), // no structured critique → don't touch
    };

    if !verdict.needs_fix {
        return (response.clone(), false); // honest pass — no rewrite
    }

    let fix_messages = vec![
        json!({
            "role": "system",
            "content": format!("{}\n\n{}", constitutions.soul, constitutions.agent)
        }),
        json!({
            "role": "user",
            "content": format!(
                "A reviewer found these issues:\n{}\n\nRewrite the code to fix every one. \
                 Output only the corrected code in a single code block.\n\nORIGINAL:\n{content}",
                verdict.issues
            )
        }),
    ];

    match run_full(
        client,
        config,
        fix_messages,
        2048,
        no_think,
        FIX_TIMEOUT_SECS,
    )
    .await
    {
        Some(fixed) => (fixed, true),
        None => (response.clone(), false),
    }
}

struct Verdict {
    needs_fix: bool,
    issues: String,
}

/// Extract the JSON verdict from the model's reply (tolerates code fences / stray prose).
fn parse_verdict(text: &str) -> Option<Verdict> {
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    let v: Value = serde_json::from_str(text.get(start..=end)?).ok()?;
    let issues = v
        .get("issues")
        .and_then(|i| i.as_array())
        .cloned()
        .unwrap_or_default();
    let verdict_fail = v.get("verdict").and_then(|x| x.as_str()) == Some("fail");
    let summary = issues
        .iter()
        .filter_map(|i| i.get("detail").and_then(|d| d.as_str()))
        .map(|d| format!("- {d}"))
        .collect::<Vec<_>>()
        .join("\n");
    Some(Verdict {
        needs_fix: verdict_fail || !issues.is_empty(),
        issues: summary,
    })
}

async fn run_complete(
    client: &Client,
    config: &Config,
    messages: Vec<Value>,
    max_tokens: u64,
    extra: Value,
    timeout_secs: u64,
) -> Option<String> {
    run_full(client, config, messages, max_tokens, extra, timeout_secs)
        .await
        .and_then(|v| brain::extract_content(&v).map(|s| s.to_string()))
}

async fn run_full(
    client: &Client,
    config: &Config,
    messages: Vec<Value>,
    max_tokens: u64,
    extra: Value,
    timeout_secs: u64,
) -> Option<Value> {
    match timeout(
        Duration::from_secs(timeout_secs),
        brain::infer(client, config, messages, 0.0, max_tokens, false, extra),
    )
    .await
    {
        Ok(Ok(BrainResponse::Complete(v))) => Some(v),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::parse_verdict;

    #[test]
    fn fail_verdict_needs_fix() {
        let v = parse_verdict(
            r#"{"issues":[{"severity":"high","detail":"unwrap panics"}],"verdict":"fail"}"#,
        )
        .unwrap();
        assert!(v.needs_fix);
        assert!(v.issues.contains("unwrap"));
    }

    #[test]
    fn clean_pass_no_fix() {
        let v = parse_verdict(r#"{"issues":[],"verdict":"pass"}"#).unwrap();
        assert!(!v.needs_fix);
    }

    #[test]
    fn tolerates_fenced_json() {
        let v = parse_verdict("```json\n{\"issues\":[],\"verdict\":\"pass\"}\n```").unwrap();
        assert!(!v.needs_fix);
    }

    #[test]
    fn non_json_returns_none() {
        assert!(parse_verdict("the code looks good to me").is_none());
    }
}
