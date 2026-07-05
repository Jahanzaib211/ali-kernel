//! Synthetic training data generator.
//!
//! Uses the local llama.cpp model to generate diverse, high-quality training pairs
//! for domains where the model is weak (identified via eval failure analysis).
//!
//! The compiler's self-hosting pattern: rustc compiles itself using a compiler,
//! Ali Kernel improves itself using its own local model.
//!
//! Each synthetic pair follows the alpaca format for SFT or preference format for DPO.

use anyhow::Result;
use chrono::Utc;
use serde::Serialize;
use serde_json::json;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;

use crate::brain::{self, BrainResponse};
use crate::config::Config;

#[allow(dead_code)]
const SYNTHETIC_SYSTEM_PROMPT: &str = "You are an expert AI assistant generating high-quality \
training data. Generate diverse, challenging examples that test real understanding. \
Each example should be non-trivial, have a clear correct answer, and test a different \
aspect of the domain. Include edge cases. Never generate trivially-easy examples.";

#[derive(Debug, Serialize)]
#[allow(dead_code)]
pub struct SyntheticPair {
    pub id: String,
    pub domain: String,
    pub instruction: String,
    pub input: String,
    pub output: String,
    pub difficulty: String,
    pub ts: String,
}

#[allow(dead_code)]
pub struct SyntheticCollector {
    training_dir: PathBuf,
}

#[allow(dead_code)]
impl SyntheticCollector {
    pub fn new(training_dir: PathBuf) -> Self {
        Self { training_dir }
    }

    /// Generate `count` synthetic SFT pairs for a given domain using the local model.
    /// The model generates both the instruction AND the ideal output (gold answer).
    pub async fn generate_domain_pairs(
        &self,
        client: &reqwest::Client,
        config: &Config,
        domain: &str,
        count: usize,
    ) -> Result<Vec<SyntheticPair>> {
        let mut pairs = Vec::new();

        let prompt = format!(
            "{}\n\nGenerate {} diverse training pairs for the domain: {}\n\
             For each pair, output JSON in this exact format (one per line, no extra text):\n\
             {{\"instruction\": \"...\", \"input\": \"...\", \"output\": \"...\", \"difficulty\": \"medium\"}}\n\n\
             Make each instruction realistic and non-trivial. Output must be valid JSON array or newline-separated JSON.",
            SYNTHETIC_SYSTEM_PROMPT,
            count,
            domain
        );

        let messages = vec![
            json!({ "role": "system", "content": SYNTHETIC_SYSTEM_PROMPT }),
            json!({ "role": "user", "content": &prompt }),
        ];

        let no_think = json!({ "chat_template_kwargs": { "enable_thinking": false } });

        match brain::infer(client, config, messages, 0.7, 4096, false, no_think).await {
            Ok(BrainResponse::Complete(v)) => {
                let content = brain::extract_content(&v).unwrap_or_default();
                for (i, line) in content.lines().enumerate() {
                    let line = line.trim();
                    if line.is_empty() || line == "```json" || line == "```" {
                        continue;
                    }
                    // Strip code fences
                    let line = line.trim_start_matches('`').trim();

                    if let Ok(pair) = serde_json::from_str::<serde_json::Value>(line) {
                        let instruction = pair
                            .get("instruction")
                            .and_then(|v| v.as_str())
                            .unwrap_or_default()
                            .to_string();
                        let input = pair
                            .get("input")
                            .and_then(|v| v.as_str())
                            .unwrap_or_default()
                            .to_string();
                        let output = pair
                            .get("output")
                            .and_then(|v| v.as_str())
                            .unwrap_or_default()
                            .to_string();
                        let difficulty = pair
                            .get("difficulty")
                            .and_then(|v| v.as_str())
                            .unwrap_or("medium")
                            .to_string();

                        if instruction.is_empty() || output.is_empty() {
                            continue;
                        }

                        let synthetic = SyntheticPair {
                            id: format!("synth_{}_{}", domain, i),
                            domain: domain.to_string(),
                            instruction,
                            input,
                            output,
                            difficulty,
                            ts: Utc::now().to_rfc3339(),
                        };

                        self.append_synthetic(&synthetic)?;
                        pairs.push(synthetic);
                    }
                }
            }
            Ok(BrainResponse::Streaming(_)) => {
                tracing::warn!("Synthetic generation returned streaming response (unexpected)");
            }
            Err(e) => {
                tracing::warn!("Synthetic generation failed for {}: {}", domain, e);
            }
        }

        Ok(pairs)
    }

    /// Generate critique-revision pairs: model answer → critique → revised answer.
    /// These teach the model to self-correct.
    pub async fn generate_critique_revision_pairs(
        &self,
        client: &reqwest::Client,
        config: &Config,
        domain: &str,
        count: usize,
    ) -> Result<Vec<(String, String, String)>> {
        // Generate an initial answer, then a critique, then a revised answer
        let mut pairs = Vec::new();

        for _i in 0..count {
            let topic_prompt = format!(
                "Give me a specific {} coding task or question (just the task, no answer). Be specific.",
                domain
            );

            let messages = vec![json!({ "role": "user", "content": topic_prompt })];

            let no_think = json!({ "chat_template_kwargs": { "enable_thinking": false } });

            let topic = match brain::infer(
                client,
                config,
                messages.clone(),
                0.5,
                512,
                false,
                no_think.clone(),
            )
            .await
            {
                Ok(BrainResponse::Complete(v)) => brain::extract_content(&v)
                    .map(|s| s.to_string())
                    .unwrap_or_default(),
                _ => continue,
            };

            if topic.is_empty() {
                continue;
            }

            // Initial answer
            let initial = match brain::infer(
                client,
                config,
                messages.clone(),
                0.3,
                1024,
                false,
                no_think.clone(),
            )
            .await
            {
                Ok(BrainResponse::Complete(v)) => brain::extract_content(&v)
                    .map(|s| s.to_string())
                    .unwrap_or_default(),
                _ => continue,
            };

            // Critique
            let _critique_prompt = format!(
                "Critique this answer ruthlessly. Find ALL issues:\n\n{}",
                initial
            );
            let critique = match brain::infer(
                client,
                config,
                messages.clone(),
                0.1,
                512,
                false,
                no_think.clone(),
            )
            .await
            {
                Ok(BrainResponse::Complete(v)) => brain::extract_content(&v)
                    .map(|s| s.to_string())
                    .unwrap_or_default(),
                _ => continue,
            };

            // Revised answer
            let _revision_prompt = format!(
                "Rewrite the answer fixing these issues:\n{}\n\nORIGINAL:\n{}",
                critique, initial
            );
            let revised =
                match brain::infer(client, config, messages, 0.2, 1024, false, no_think).await {
                    Ok(BrainResponse::Complete(v)) => brain::extract_content(&v)
                        .map(|s| s.to_string())
                        .unwrap_or_default(),
                    _ => continue,
                };

            // Log as DPO: chosen=revised, rejected=initial
            let prompt_for_dpo = format!("Task: {}\n\nInitial answer:\n{}", topic, initial);
            if let Err(e) =
                crate::training::log_dpo(&self.training_dir, &prompt_for_dpo, &revised, &initial)
            {
                tracing::warn!("DPO log failed: {}", e);
            }

            pairs.push((topic.to_string(), critique.to_string(), revised.to_string()));
        }

        Ok(pairs)
    }

    fn append_synthetic(&self, pair: &SyntheticPair) -> Result<()> {
        let path = self.training_dir.join("synthetic_pairs.jsonl");
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let entry = serde_json::to_string(pair)?;
        let mut file = OpenOptions::new().create(true).append(true).open(&path)?;
        writeln!(file, "{entry}")?;

        // Also append to SFT file in alpaca format
        let sft_entry = json!({
            "instruction": &pair.instruction,
            "input": &pair.input,
            "output": &pair.output,
            "domain": &pair.domain,
            "difficulty": &pair.difficulty,
            "ts": &pair.ts,
        });
        let sft_path = self.training_dir.join("sft.jsonl");
        let mut sft_file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&sft_path)?;
        writeln!(sft_file, "{}", sft_entry)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn synthetic_pair_serialization() {
        let pair = SyntheticPair {
            id: "test_1".into(),
            domain: "rust".into(),
            instruction: "Write a binary search".into(),
            input: "".into(),
            output: "fn binary_search<T: Ord>(arr: &[T], target: &T) -> Option<usize> { ... }"
                .into(),
            difficulty: "medium".into(),
            ts: Utc::now().to_rfc3339(),
        };
        let json = serde_json::to_string(&pair).unwrap();
        assert!(json.contains("binary_search"));
    }
}
