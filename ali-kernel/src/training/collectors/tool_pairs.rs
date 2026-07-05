//! Tool execution pair collector.
//!
//! Every tool execution logs a (task_description, tool_name, args, compressed_result) pair
//! that teaches the model *when* to use which tool and *how* to call it.
//!
//! This follows the compiler's query cache pattern: tool outputs are cached by
//! (tool_name, args_hash) so repeated executions with same args skip re-computation.

use anyhow::Result;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolPair {
    pub task: String,
    pub tool_name: String,
    pub args: Value,
    pub result: String,
    pub result_hash: String,
    pub compressed: bool,
    pub route: String,
    pub ts: String,
    pub args_hash: String,
    pub critique_applied: bool,
}

#[allow(dead_code)]
pub struct ToolPairCollector {
    cache: Arc<RwLock<HashMap<String, ToolPair>>>,
    training_dir: PathBuf,
}

#[allow(dead_code)]
impl ToolPairCollector {
    pub fn new(training_dir: PathBuf) -> Self {
        Self {
            cache: Arc::new(RwLock::new(HashMap::new())),
            training_dir,
        }
    }

    pub fn args_hash(args: &Value) -> String {
        let json = serde_json::to_string(args).unwrap_or_default();
        let mut hasher = Sha256::new();
        hasher.update(json.as_bytes());
        format!("{:x}", hasher.finalize())[..16].to_string()
    }

    pub fn result_hash(result: &str) -> String {
        let mut hasher = Sha256::new();
        hasher.update(result.as_bytes());
        format!("{:x}", hasher.finalize())[..16].to_string()
    }

    /// Log a tool execution pair. Skips if the same (tool, args_hash) was already logged today.
    pub async fn log(
        &self,
        task: &str,
        tool_name: &str,
        args: &Value,
        result: &str,
        route: &str,
        critique_applied: bool,
    ) -> Result<()> {
        let args_h = Self::args_hash(args);
        let result_h = Self::result_hash(result);

        // Skip if already cached (incremental: same tool+args = same pair)
        {
            let cache = self.cache.read().await;
            if let Some(existing) = cache.get(&format!("{tool_name}:{args_h}")) {
                if existing.result_hash == result_h {
                    return Ok(()); // identical execution, skip
                }
            }
        }

        let pair = ToolPair {
            task: task.to_string(),
            tool_name: tool_name.to_string(),
            args: args.clone(),
            result: result.to_string(),
            result_hash: result_h,
            compressed: true,
            route: route.to_string(),
            ts: Utc::now().to_rfc3339(),
            args_hash: args_h,
            critique_applied,
        };

        // Cache it
        {
            let mut cache = self.cache.write().await;
            cache.insert(format!("{}:{}", tool_name, pair.args_hash), pair.clone());
        }

        // Append to training file
        self.append_pair(&pair)?;
        Ok(())
    }

    /// Check if a tool execution result is already in cache
    #[allow(unused)]
    pub async fn is_cached(&self, tool_name: &str, args: &Value) -> bool {
        let args_h = Self::args_hash(args);
        let cache = self.cache.read().await;
        cache.contains_key(&format!("{tool_name}:{args_h}"))
    }

    /// Get cached result
    #[allow(unused)]
    pub async fn get_cached(&self, tool_name: &str, args: &Value) -> Option<String> {
        let args_h = Self::args_hash(args);
        let cache = self.cache.read().await;
        cache
            .get(&format!("{tool_name}:{args_h}"))
            .map(|p| p.result.clone())
    }

    fn append_pair(&self, pair: &ToolPair) -> Result<()> {
        let path = self.training_dir.join("tool_pairs.jsonl");
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let entry = serde_json::to_string(pair)?;
        let mut file = OpenOptions::new().create(true).append(true).open(&path)?;
        writeln!(file, "{entry}")?;
        Ok(())
    }

    #[allow(unused)]
    pub async fn load_cache(&self) -> Result<()> {
        let path = self.training_dir.join("tool_pairs.jsonl");
        if !path.exists() {
            return Ok(());
        }
        let content = fs::read_to_string(&path)?;
        let mut cache = self.cache.write().await;
        for line in content.lines() {
            if let Ok(pair) = serde_json::from_str::<ToolPair>(line) {
                cache.insert(format!("{}:{}", pair.tool_name, pair.args_hash), pair);
            }
        }
        Ok(())
    }

    /// Count cached pairs
    pub async fn count(&self) -> usize {
        let cache = self.cache.read().await;
        cache.len()
    }

    #[allow(unused)]
    pub async fn clear_cache(&self) {
        let mut cache = self.cache.write().await;
        cache.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn args_hash_deterministic() {
        let args = serde_json::json!({"path": "/tmp/foo"});
        let h1 = ToolPairCollector::args_hash(&args);
        let h2 = ToolPairCollector::args_hash(&args);
        assert_eq!(h1, h2);
    }

    #[test]
    fn args_hash_different_for_different_args() {
        let args1 = serde_json::json!({"path": "/tmp/foo"});
        let args2 = serde_json::json!({"path": "/tmp/bar"});
        let h1 = ToolPairCollector::args_hash(&args1);
        let h2 = ToolPairCollector::args_hash(&args2);
        assert_ne!(h1, h2);
    }

    #[tokio::test]
    async fn cache_skips_identical() {
        let tmp = std::env::temp_dir().join("tool-pair-test");
        let _ = fs::remove_dir_all(&tmp);
        let collector = ToolPairCollector::new(tmp.clone());

        let args = serde_json::json!({"command": "ls"});
        collector
            .log(
                "list files",
                "bash_exec",
                &args,
                "file1\nfile2",
                "quick",
                false,
            )
            .await
            .unwrap();

        assert_eq!(collector.count().await, 1);

        // Same tool+args again → skipped (cache hit)
        collector
            .log(
                "list files",
                "bash_exec",
                &args,
                "file1\nfile2",
                "quick",
                false,
            )
            .await
            .unwrap();

        assert_eq!(collector.count().await, 1); // still 1, not 2
    }
}
