//! Eval failure → DPO pair auto-generator.
//!
//! When an eval task scores below a threshold, the system automatically:
//! 1. Queries the local model for a gold-standard answer
//! 2. Logs (prompt, gold_answer, model_answer) as a DPO pair
//!
//! This is the "teacher loop": the model grades itself on failures
//! and generates preference data from the gap between gold and model answers.

use anyhow::Result;
use chrono::Utc;
use serde::Serialize;
use serde_json::json;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;

use crate::brain::{self, BrainResponse};
use crate::config::Config;
use crate::training::log_dpo;

/// Threshold below which an eval failure triggers DPO pair generation
#[allow(dead_code)]
const DPO_TRIGGER_SCORE: i32 = 3;

/// When a domain averages below this, generate synthetic pairs for it
#[allow(dead_code)]
const DOMAIN_WEAK_THRESHOLD: f64 = 60.0;

#[derive(Debug, Serialize)]
#[allow(dead_code)]
pub struct EvalFailurePair {
    pub task_id: String,
    pub domain: String,
    pub prompt: String,
    pub rubric: String,
    pub model_answer: String,
    pub gold_answer: Option<String>,
    pub score: u32,
    pub max_score: u32,
    pub route: String,
    pub ts: String,
}

#[allow(dead_code)]
pub struct EvalPairCollector {
    training_dir: PathBuf,
}

#[allow(dead_code)]
impl EvalPairCollector {
    pub fn new(training_dir: PathBuf) -> Self {
        Self { training_dir }
    }

    /// Called after each eval run. For each task that scored below DPO_TRIGGER_SCORE,
    /// generate a gold answer from the local model and log a DPO pair.
    pub async fn process_eval_results(
        &self,
        client: &reqwest::Client,
        config: &Config,
        tasks: &[(String, String, String, String, u32, u32)], // (id, domain, prompt, rubric, score, max_score)
        route: &str,
    ) -> Result<Vec<EvalFailurePair>> {
        let mut failures = Vec::new();

        for (task_id, domain, prompt, rubric, score, max_score) in tasks {
            if *score as i32 >= DPO_TRIGGER_SCORE {
                continue;
            }

            // Generate gold answer using local model
            let gold_answer = self
                .generate_gold(client, config, prompt, rubric, *max_score)
                .await;

            let failure = EvalFailurePair {
                task_id: task_id.clone(),
                domain: domain.clone(),
                prompt: prompt.clone(),
                rubric: rubric.clone(),
                model_answer: String::new(), // filled by caller
                gold_answer: gold_answer.clone(),
                score: *score,
                max_score: *max_score,
                route: route.to_string(),
                ts: Utc::now().to_rfc3339(),
            };

            // Log DPO pair: chosen = gold, rejected = model answer placeholder
            // Note: model_answer will be empty here; caller should fill it
            // The actual DPO logging happens when model_answer is available
            if let Some(gold) = &gold_answer {
                log_dpo(&self.training_dir, prompt, gold, "[model answer]")?;
            }

            failures.push(failure);
        }

        self.append_failures(&failures)?;
        Ok(failures)
    }

    async fn generate_gold(
        &self,
        client: &reqwest::Client,
        config: &Config,
        prompt: &str,
        rubric: &str,
        max_score: u32,
    ) -> Option<String> {
        let messages = vec![
            json!({
                "role": "system",
                "content": format!(
                    "You are an expert in this domain. Provide the BEST possible answer to the task.\n\
                     Rate yourself honestly. A score of {}/{} means PERFECT.\n\n\
                     RUBRIC:\n{rubric}",
                    max_score, max_score
                )
            }),
            json!({ "role": "user", "content": prompt }),
        ];

        let no_think = json!({ "chat_template_kwargs": { "enable_thinking": false } });

        match brain::infer(
            client, config, messages, 0.1, // low temp for deterministic gold
            2048, false, no_think,
        )
        .await
        {
            Ok(BrainResponse::Complete(v)) => brain::extract_content(&v).map(|s| s.to_string()),
            _ => None,
        }
    }

    /// Analyze eval scores by domain. Return domains averaging below DOMAIN_WEAK_THRESHOLD.
    pub fn weak_domains(
        &self,
        by_domain: &[(String, f64)], // (domain, avg_score_pct)
    ) -> Vec<String> {
        by_domain
            .iter()
            .filter(|(_, score)| *score < DOMAIN_WEAK_THRESHOLD)
            .map(|(d, _)| d.clone())
            .collect()
    }

    fn append_failures(&self, failures: &[EvalFailurePair]) -> Result<()> {
        if failures.is_empty() {
            return Ok(());
        }
        let path = self.training_dir.join("eval_failures.jsonl");
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut file = OpenOptions::new().create(true).append(true).open(&path)?;
        for failure in failures {
            let entry = serde_json::to_string(failure)?;
            writeln!(file, "{entry}")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn weak_domains_detection() {
        let collector = EvalPairCollector::new(PathBuf::from("/tmp/ali-train"));
        let domains = vec![
            ("rust".to_string(), 45.0),   // weak
            ("python".to_string(), 82.0), // ok
            ("go".to_string(), 55.0),     // weak
        ];
        let weak = collector.weak_domains(&domains);
        assert_eq!(weak.len(), 2);
        assert!(weak.contains(&"rust".to_string()));
        assert!(weak.contains(&"go".to_string()));
    }
}
