use anyhow::Result;
use chrono::Utc;
use serde_json::json;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;

/// Append one SFT (supervised fine-tuning) example in alpaca format.
/// File: {training_dir}/sft.jsonl — consumed directly by unsloth/axolotl.
pub fn log_sft(training_dir: &Path, instruction: &str, output: &str, route: &str) -> Result<()> {
    if output.trim().is_empty() {
        return Ok(());
    }
    let entry = json!({
        "instruction": instruction,
        "input": "",
        "output": output,
        "route": route,
        "ts": Utc::now().to_rfc3339(),
    });
    append_line(&training_dir.join("sft.jsonl"), &entry.to_string())
}

/// Append one DPO preference pair.
/// File: {training_dir}/dpo_pairs.jsonl — chosen > rejected for this prompt.
pub fn log_dpo(training_dir: &Path, prompt: &str, chosen: &str, rejected: &str) -> Result<()> {
    if chosen.trim().is_empty() || rejected.trim().is_empty() || chosen == rejected {
        return Ok(());
    }
    let entry = json!({
        "prompt": prompt,
        "chosen": chosen,
        "rejected": rejected,
        "ts": Utc::now().to_rfc3339(),
    });
    append_line(&training_dir.join("dpo_pairs.jsonl"), &entry.to_string())
}

pub fn init_training_dir(training_dir: &Path) -> Result<()> {
    fs::create_dir_all(training_dir)?;
    Ok(())
}

fn append_line(path: &Path, line: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    writeln!(file, "{line}")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn tmp_dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ornith-train-test-{name}"));
        let _ = fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn sft_writes_alpaca_schema() {
        let dir = tmp_dir("sft");
        log_sft(&dir, "do X", "result", "full").unwrap();
        let content = fs::read_to_string(dir.join("sft.jsonl")).unwrap();
        let v: serde_json::Value = serde_json::from_str(content.trim()).unwrap();
        assert_eq!(v["instruction"], "do X");
        assert_eq!(v["output"], "result");
        assert!(v["ts"].is_string());
    }

    #[test]
    fn dpo_skips_identical_pairs() {
        let dir = tmp_dir("dpo-identical");
        log_dpo(&dir, "p", "same", "same").unwrap();
        assert!(!dir.join("dpo_pairs.jsonl").exists());
    }

    #[test]
    fn dpo_writes_preference_pair() {
        let dir = tmp_dir("dpo-pair");
        log_dpo(&dir, "fix bug", "good code", "bad code").unwrap();
        let content = fs::read_to_string(dir.join("dpo_pairs.jsonl")).unwrap();
        let v: serde_json::Value = serde_json::from_str(content.trim()).unwrap();
        assert_eq!(v["chosen"], "good code");
        assert_eq!(v["rejected"], "bad code");
    }
}
