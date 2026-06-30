use anyhow::Result;
use chrono::Utc;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::Write;

use crate::brain::{self, BrainResponse};
use crate::config::Config;
use crate::security::Constitutions;

#[derive(Debug, Deserialize, Clone)]
pub struct EvalTask {
    pub id: String,
    pub domain: String,
    pub prompt: String,
    pub rubric: String,
    #[serde(default = "default_max")]
    pub max_score: u32,
}

fn default_max() -> u32 {
    5
}

#[derive(Debug, Serialize)]
pub struct TaskResult {
    pub id: String,
    pub domain: String,
    pub score: u32,
    pub max_score: u32,
}

#[derive(Debug, Serialize)]
pub struct EvalReport {
    pub timestamp: String,
    pub total: f64,
    pub raw_score: u32,
    pub raw_max: u32,
    pub by_domain: BTreeMap<String, f64>,
    pub tasks: Vec<TaskResult>,
    pub model_hash: String,
}

pub fn load_suite(config: &Config) -> Result<Vec<EvalTask>> {
    let path = config.evals_dir.join("suite.json");
    let content = fs::read_to_string(&path)
        .map_err(|e| anyhow::anyhow!("cannot read {}: {e}", path.display()))?;
    let tasks: Vec<EvalTask> = serde_json::from_str(&content)?;
    Ok(tasks)
}

pub async fn run_suite(
    client: &Client,
    config: &Config,
    constitutions: &Constitutions,
) -> Result<EvalReport> {
    let tasks = load_suite(config)?;
    let mut results: Vec<TaskResult> = Vec::new();
    let mut domain_totals: BTreeMap<String, (u32, u32)> = BTreeMap::new();
    let mut raw_score = 0u32;
    let mut raw_max = 0u32;

    for task in &tasks {
        let answer = generate_answer(client, config, constitutions, &task.prompt).await;
        let score = match &answer {
            Some(a) => judge(client, config, task, a).await.min(task.max_score),
            None => 0,
        };

        raw_score += score;
        raw_max += task.max_score;
        let entry = domain_totals.entry(task.domain.clone()).or_insert((0, 0));
        entry.0 += score;
        entry.1 += task.max_score;

        results.push(TaskResult {
            id: task.id.clone(),
            domain: task.domain.clone(),
            score,
            max_score: task.max_score,
        });
    }

    let by_domain = domain_totals
        .into_iter()
        .map(|(d, (s, m))| (d, pct(s, m)))
        .collect();

    let report = EvalReport {
        timestamp: Utc::now().to_rfc3339(),
        total: pct(raw_score, raw_max),
        raw_score,
        raw_max,
        by_domain,
        tasks: results,
        model_hash: read_model_hash(config),
    };

    append_score(config, &report)?;
    Ok(report)
}

fn pct(score: u32, max: u32) -> f64 {
    if max == 0 {
        return 0.0;
    }
    ((score as f64 / max as f64) * 1000.0).round() / 10.0
}

async fn generate_answer(
    client: &Client,
    config: &Config,
    constitutions: &Constitutions,
    prompt: &str,
) -> Option<String> {
    let messages = vec![
        json!({ "role": "system", "content": format!("{}\n\n{}", constitutions.soul, constitutions.agent) }),
        json!({ "role": "user", "content": prompt }),
    ];
    match brain::infer(client, config, messages, 0.3, 1024, false, json!({})).await {
        Ok(BrainResponse::Complete(v)) => brain::extract_content(&v).map(|s| s.to_string()),
        _ => None,
    }
}

/// RLAIF self-grading: the model scores the answer against the rubric, 0..max_score.
async fn judge(client: &Client, config: &Config, task: &EvalTask, answer: &str) -> u32 {
    let prompt = format!(
        "You are a harsh, exacting grader. Score the ANSWER against the RUBRIC from 0 to {max}.\n\
         Deduct a point for ANY flaw, missing edge case, unhandled error, or deviation from the rubric. \
         A flawless full-mark answer is rare — most answers have at least one fault. Do not be generous.\n\n\
         TASK: {task_prompt}\n\nRUBRIC: {rubric}\n\nANSWER:\n{answer}\n\n\
         Respond with ONLY a single integer from 0 to {max}. No words.",
        max = task.max_score,
        task_prompt = task.prompt,
        rubric = task.rubric,
        answer = answer,
    );
    let messages = vec![json!({ "role": "user", "content": prompt })];

    // Disable the model's thinking block for grading: a reasoning model otherwise
    // burns the whole token budget inside <think> and emits empty content (score 0).
    // With thinking off it returns the bare integer in ~2 tokens.
    let no_think = json!({ "chat_template_kwargs": { "enable_thinking": false } });
    match brain::infer(client, config, messages, 0.0, 16, false, no_think).await {
        Ok(BrainResponse::Complete(v)) => brain::extract_content(&v)
            .and_then(parse_score)
            .unwrap_or(0),
        _ => 0,
    }
}

fn parse_score(text: &str) -> Option<u32> {
    text.chars()
        .skip_while(|c| !c.is_ascii_digit())
        .take_while(|c| c.is_ascii_digit())
        .collect::<String>()
        .parse::<u32>()
        .ok()
}

fn read_model_hash(config: &Config) -> String {
    fs::read_to_string(config.ram_moat.join("golden_binary.hash"))
        .map(|s| s.trim().chars().take(12).collect())
        .unwrap_or_else(|_| "unknown".into())
}

fn append_score(config: &Config, report: &EvalReport) -> Result<()> {
    fs::create_dir_all(&config.evals_dir)?;
    let path = config.evals_dir.join("eval_scores.jsonl");
    let line = json!({
        "timestamp": report.timestamp,
        "total": report.total,
        "by_domain": report.by_domain,
        "model_hash": report.model_hash,
    });
    let mut file = OpenOptions::new().create(true).append(true).open(&path)?;
    writeln!(file, "{line}")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_bare_integer() {
        assert_eq!(parse_score("4"), Some(4));
        assert_eq!(parse_score("Score: 3/5"), Some(3));
        assert_eq!(parse_score("nonsense"), None);
    }

    #[test]
    fn pct_computes_percentage() {
        assert_eq!(pct(50, 100), 50.0);
        assert_eq!(pct(0, 0), 0.0);
        assert_eq!(pct(3, 4), 75.0);
    }
}
