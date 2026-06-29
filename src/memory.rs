use chrono::Utc;
use serde_json::json;
use std::fs::{self, OpenOptions};
use std::io::Write;

use crate::config::Config;

const TOP_K: usize = 3;
const MIN_SCORE: f32 = 0.12;

#[derive(Debug, Clone)]
pub struct Memory {
    pub content: String,
    pub score: f32,
    pub source: String,
}

impl Memory {
    pub fn rank_key(&self) -> (u32, &str) {
        ((self.score * 1000.0) as u32, &self.source)
    }
}

/// Retrieve top-k relevant past interactions for the query.
/// Qdrant mode when QDRANT_URL is set and reachable; otherwise local keyword fallback.
pub async fn retrieve(query: &str, config: &Config) -> Vec<Memory> {
    if let Some(url) = &config.qdrant_url {
        if let Ok(hits) = qdrant_retrieve(url, query, config).await {
            if !hits.is_empty() {
                return hits;
            }
        }
        tracing::warn!("Qdrant retrieve unavailable — falling back to local memory");
    }
    local_retrieve(query, config)
}

/// Persist one interaction to episodic memory.
pub async fn store(config: &Config, prompt: &str, response: &str) {
    if prompt.trim().is_empty() || response.trim().is_empty() {
        return;
    }
    if let Some(url) = &config.qdrant_url {
        if qdrant_store(url, prompt, response, config).await.is_ok() {
            return;
        }
        tracing::warn!("Qdrant store unavailable — falling back to local memory");
    }
    if let Err(e) = local_store(config, prompt, response) {
        tracing::warn!("Local memory store failed: {e}");
    }
}

// ── Local fallback (zero-dependency keyword retrieval) ──────────────────────

fn memory_path(config: &Config) -> std::path::PathBuf {
    config.training_dir.join("memory.jsonl")
}

fn local_store(config: &Config, prompt: &str, response: &str) -> anyhow::Result<()> {
    let path = memory_path(config);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let entry = json!({
        "prompt": prompt,
        "response": response,
        "ts": Utc::now().to_rfc3339(),
    });
    let mut file = OpenOptions::new().create(true).append(true).open(&path)?;
    writeln!(file, "{entry}")?;
    Ok(())
}

fn local_retrieve(query: &str, config: &Config) -> Vec<Memory> {
    let path = memory_path(config);
    let content = match fs::read_to_string(&path) {
        Ok(c) => c,
        Err(_) => return vec![],
    };

    let query_tokens = tokenize(query);
    if query_tokens.is_empty() {
        return vec![];
    }

    let mut scored: Vec<Memory> = content
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter_map(|v| {
            let prompt = v.get("prompt")?.as_str()?.to_string();
            let response = v.get("response")?.as_str()?.to_string();
            let haystack = format!("{prompt} {response}");
            let score = jaccard(&query_tokens, &tokenize(&haystack));
            if score < MIN_SCORE {
                return None;
            }
            Some(Memory {
                content: format!("Q: {prompt}\nA: {response}"),
                score,
                source: "local".into(),
            })
        })
        .collect();

    scored.sort_by(|a, b| b.rank_key().0.cmp(&a.rank_key().0));
    scored.truncate(TOP_K);
    scored
}

fn tokenize(s: &str) -> Vec<String> {
    s.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| t.len() > 2)
        .map(|t| t.to_string())
        .collect()
}

fn jaccard(a: &[String], b: &[String]) -> f32 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let set_a: std::collections::HashSet<&String> = a.iter().collect();
    let set_b: std::collections::HashSet<&String> = b.iter().collect();
    let inter = set_a.intersection(&set_b).count() as f32;
    let union = set_a.union(&set_b).count() as f32;
    if union == 0.0 { 0.0 } else { inter / union }
}

// ── Qdrant mode (optional, embeddings via llama-server) ─────────────────────

async fn embed(query: &str, config: &Config) -> anyhow::Result<Vec<f32>> {
    let url = format!("{}/v1/embeddings", config.llama_base_url());
    let resp = reqwest::Client::new()
        .post(&url)
        .json(&json!({ "input": query, "model": "ornith" }))
        .send()
        .await?
        .json::<serde_json::Value>()
        .await?;
    let vec = resp
        .get("data")
        .and_then(|d| d.get(0))
        .and_then(|d| d.get("embedding"))
        .and_then(|e| e.as_array())
        .ok_or_else(|| anyhow::anyhow!("no embedding in response"))?
        .iter()
        .filter_map(|v| v.as_f64().map(|f| f as f32))
        .collect();
    Ok(vec)
}

async fn qdrant_retrieve(url: &str, query: &str, config: &Config) -> anyhow::Result<Vec<Memory>> {
    let vector = embed(query, config).await?;
    let search = json!({
        "vector": vector,
        "limit": TOP_K,
        "with_payload": true,
    });
    let resp = reqwest::Client::new()
        .post(format!("{url}/collections/ornith_memory/points/search"))
        .json(&search)
        .send()
        .await?
        .json::<serde_json::Value>()
        .await?;

    let hits = resp
        .get("result")
        .and_then(|r| r.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|h| {
                    let score = h.get("score").and_then(|s| s.as_f64()).unwrap_or(0.0) as f32;
                    let payload = h.get("payload")?;
                    let prompt = payload.get("prompt")?.as_str()?;
                    let response = payload.get("response")?.as_str()?;
                    Some(Memory {
                        content: format!("Q: {prompt}\nA: {response}"),
                        score,
                        source: "qdrant".into(),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    Ok(hits)
}

async fn qdrant_store(url: &str, prompt: &str, response: &str, config: &Config) -> anyhow::Result<()> {
    let vector = embed(prompt, config).await?;
    let point = json!({
        "points": [{
            "id": uuid::Uuid::new_v4().to_string(),
            "vector": vector,
            "payload": { "prompt": prompt, "response": response },
        }]
    });
    reqwest::Client::new()
        .put(format!("{url}/collections/ornith_memory/points"))
        .json(&point)
        .send()
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jaccard_overlap() {
        let a = tokenize("what was my deploy key");
        let b = tokenize("remember my deploy key is ORNITH-7723");
        assert!(jaccard(&a, &b) > 0.0);
    }

    #[test]
    fn tokenize_drops_short_words() {
        let t = tokenize("a an the deploy");
        assert_eq!(t, vec!["the".to_string(), "deploy".to_string()]);
    }
}
