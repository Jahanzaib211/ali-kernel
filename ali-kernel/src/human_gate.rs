use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallPending {
    pub request_id: String,
    pub tool_index: usize,
    pub tool_name: String,
    pub args: serde_json::Value,
    pub risk: String,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalRequest {
    pub request_id: String,
    pub action: String,
    pub tool_index: Option<usize>,
    pub modification: Option<serde_json::Value>,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalResponse {
    pub status: String,
    pub request_id: String,
    pub executed: bool,
    pub denied: bool,
}

#[derive(Debug, Clone)]
pub enum ApprovalResult {
    Approved {
        modified_args: Option<serde_json::Value>,
    },
    Denied,
}

#[derive(Debug)]
pub struct PendingApproval {
    #[allow(dead_code)]
    pub request_id: String,
    pub tools: Vec<ToolCallPending>,
    pub results: Vec<Option<ApprovalResult>>,
    #[allow(dead_code)]
    pub resolved: bool,
}

impl PendingApproval {
    pub fn new(request_id: String, tools: Vec<ToolCallPending>) -> Self {
        let results = vec![None; tools.len()];
        Self {
            request_id,
            tools,
            results,
            resolved: false,
        }
    }

    pub fn approve(&mut self, tool_index: usize, modified_args: Option<serde_json::Value>) -> bool {
        if tool_index >= self.results.len() {
            return false;
        }
        self.results[tool_index] = Some(ApprovalResult::Approved { modified_args });
        self.results.iter().all(|r| r.is_some())
    }

    pub fn deny(&mut self, tool_index: usize) -> bool {
        if tool_index >= self.results.len() {
            return false;
        }
        self.results[tool_index] = Some(ApprovalResult::Denied);
        self.results.iter().all(|r| r.is_some())
    }
}

#[derive(Clone)]
pub struct HumanGate {
    inner: Arc<Mutex<HashMap<String, PendingApproval>>>,
}

impl HumanGate {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub async fn register(&self, request_id: String, tools: Vec<ToolCallPending>) {
        let approval = PendingApproval::new(request_id.clone(), tools);
        let mut map = self.inner.lock().await;
        map.insert(request_id, approval);
    }

    pub async fn approve(
        &self,
        request_id: &str,
        tool_index: usize,
        modified_args: Option<serde_json::Value>,
    ) -> Option<ApprovalResult> {
        let mut map = self.inner.lock().await;
        if let Some(pending) = map.get_mut(request_id) {
            pending.approve(tool_index, modified_args);
            return pending.results.get(tool_index).cloned().flatten();
        }
        None
    }

    pub async fn deny(&self, request_id: &str, tool_index: usize) -> Option<ApprovalResult> {
        let mut map = self.inner.lock().await;
        if let Some(pending) = map.get_mut(request_id) {
            pending.deny(tool_index);
            return pending.results.get(tool_index).cloned().flatten();
        }
        None
    }

    pub async fn is_resolved(&self, request_id: &str) -> bool {
        let map = self.inner.lock().await;
        map.get(request_id)
            .map(|p| p.results.iter().all(|r| r.is_some()))
            .unwrap_or(true)
    }

    pub async fn get_pending(&self, request_id: &str) -> Option<Vec<ToolCallPending>> {
        let map = self.inner.lock().await;
        map.get(request_id).map(|p| p.tools.clone())
    }

    pub async fn remove(&self, request_id: &str) {
        let mut map = self.inner.lock().await;
        map.remove(request_id);
    }
}

pub fn classify_risk(tool_name: &str) -> &'static str {
    match tool_name {
        "read_file" | "crate_search" | "docs_lookup" => "LOW",
        "bash_exec" | "execute_bash" | "run_command" | "write_file" => "MEDIUM",
        "cargo_new" | "cargo_add" | "write_project_file" => "HIGH",
        _ => "MEDIUM",
    }
}

pub fn extract_tool_calls(response: &serde_json::Value) -> Vec<ToolCallPending> {
    let request_id = Uuid::new_v4().to_string();
    let calls = match response
        .get("choices")
        .and_then(|c| c.get(0))
        .and_then(|c| c.get("message"))
        .and_then(|m| m.get("tool_calls"))
        .and_then(|t| t.as_array())
    {
        Some(calls) => calls.clone(),
        None => return vec![],
    };

    calls
        .iter()
        .enumerate()
        .filter_map(|(i, call)| {
            let name = call
                .get("function")
                .and_then(|f| f.get("name"))
                .and_then(|v| v.as_str())?;
            let args_str = call
                .get("function")
                .and_then(|f| f.get("arguments"))
                .and_then(|v| v.as_str())
                .unwrap_or("{}");
            let args: serde_json::Value = serde_json::from_str(args_str).unwrap_or_default();
            Some(ToolCallPending {
                request_id: request_id.clone(),
                tool_index: i,
                tool_name: name.to_string(),
                args,
                risk: classify_risk(name).to_string(),
            })
        })
        .collect()
}
