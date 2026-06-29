use anyhow::Result;
use chrono::Utc;
use serde::Serialize;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;

#[derive(Debug, Serialize)]
pub struct PipelineLog {
    pub timestamp: String,
    pub request_id: String,
    pub model: String,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub latency_ms: u64,
    pub route: String,
    pub tool_called: bool,
    pub critique_applied: bool,
}

pub fn init_log_dir(log_dir: &Path) -> Result<()> {
    fs::create_dir_all(log_dir)?;
    Ok(())
}

pub fn log_request(log_dir: &Path, entry: PipelineLog) -> Result<()> {
    let log_path = log_dir.join("pipeline.jsonl");
    let line = serde_json::to_string(&entry)?;

    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)?;

    writeln!(file, "{}", line)?;
    Ok(())
}

pub fn make_entry(
    request_id: &str,
    route: &str,
    response: &serde_json::Value,
    latency_ms: u64,
    tool_called: bool,
    critique_applied: bool,
) -> PipelineLog {
    let usage = response.get("usage");
    let prompt_tokens = usage
        .and_then(|u| u.get("prompt_tokens"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let completion_tokens = usage
        .and_then(|u| u.get("completion_tokens"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0);

    PipelineLog {
        timestamp: Utc::now().to_rfc3339(),
        request_id: request_id.to_string(),
        model: "ornith".into(),
        prompt_tokens,
        completion_tokens,
        latency_ms,
        route: route.to_string(),
        tool_called,
        critique_applied,
    }
}
