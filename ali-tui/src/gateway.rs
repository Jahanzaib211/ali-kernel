use anyhow::Result;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::mpsc;
use tokio_stream::StreamExt;

const GW_URL_DEFAULT: &str = "http://127.0.0.1:9000";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub enum GatewayEvent {
    Token(String),
    ToolCallPending {
        request_id: String,
        tool_name: String,
        args: Value,
        risk: String,
        tool_index: usize,
    },
    Done,
    Error(String),
}

#[derive(Clone)]
pub struct GatewayClient {
    client: Client,
    base_url: String,
}

impl GatewayClient {
    pub fn new() -> Self {
        Self {
            client: Client::new(),
            base_url: std::env::var("ALI_GATEWAY_URL")
                .unwrap_or_else(|_| GW_URL_DEFAULT.to_string()),
        }
    }

    pub async fn health_check(&self) -> Result<Value> {
        let url = format!("{}/health", self.base_url);
        let resp = self.client.get(&url).send().await?;
        let body: Value = resp.json().await?;
        Ok(body)
    }

    pub async fn get_stats(&self) -> Result<Value> {
        let url = format!("{}/stats", self.base_url);
        let resp = self.client.get(&url).send().await?;
        let body: Value = resp.json().await?;
        Ok(body)
    }

    pub async fn chat(
        &self,
        messages: Vec<ChatMessage>,
        tools: Vec<Value>,
        human_gate: bool,
        tx: mpsc::Sender<GatewayEvent>,
    ) -> Result<()> {
        let url = format!("{}/v1/chat/completions", self.base_url);
        let body = serde_json::json!({
            "model": "ornith",
            "messages": messages,
            "tools": tools,
            "human_gate": human_gate,
            "stream": true,
            "max_tokens": 2048,
        });

        let resp = self.client.post(&url).json(&body).send().await?;

        let stream = resp.bytes_stream();
        tokio::pin!(stream);

        let mut line_buf = String::new();

        while let Some(chunk) = stream.next().await {
            match chunk {
                Ok(bytes) => {
                    let text = String::from_utf8_lossy(&bytes);
                    for ch in text.chars() {
                        if ch == '\n' {
                            let line = line_buf.trim().to_string();
                            line_buf.clear();
                            if line.is_empty() || !line.starts_with("data:") {
                                continue;
                            }
                            let data = line.strip_prefix("data:").unwrap().trim();
                            if data == "[DONE]" {
                                let _ = tx.send(GatewayEvent::Done).await;
                                return Ok(());
                            }
                            // Try to parse. If incomplete JSON (more data coming), buffer it.
                            match serde_json::from_str::<Value>(data) {
                                Ok(v) => {
                                    if let Some(delta) = v
                                        .pointer("/choices/0/delta/content")
                                        .and_then(|c| c.as_str())
                                    {
                                        let _ =
                                            tx.send(GatewayEvent::Token(delta.to_string())).await;
                                    }
                                }
                                Err(_) => {
                                    // Incomplete JSON on this line — the full object may span multiple lines.
                                    // Put it back for next iteration.
                                    line_buf.push_str(&line);
                                }
                            }
                        } else if ch != '\r' {
                            line_buf.push(ch);
                        }
                    }
                }
                Err(e) => {
                    let _ = tx.send(GatewayEvent::Error(e.to_string())).await;
                    return Err(e.into());
                }
            }
        }

        // Stream ended — send Done
        let _ = tx.send(GatewayEvent::Done).await;
        Ok(())
    }

    pub async fn approve_tool(
        &self,
        request_id: &str,
        tool_index: usize,
        modification: Option<Value>,
    ) -> Result<Value> {
        let url = format!("{}/v1/approvals", self.base_url);
        let mut body = serde_json::json!({
            "request_id": request_id,
            "action": "approve",
            "tool_index": tool_index,
        });
        if let Some(m) = modification {
            body["modification"] = m;
        }
        let resp = self.client.post(&url).json(&body).send().await?;
        let result: Value = resp.json().await?;
        Ok(result)
    }

    pub async fn deny_tool(&self, request_id: &str, tool_index: usize) -> Result<Value> {
        let url = format!("{}/v1/approvals", self.base_url);
        let body = serde_json::json!({
            "request_id": request_id,
            "action": "deny",
            "tool_index": tool_index,
        });
        let resp = self.client.post(&url).json(&body).send().await?;
        let result: Value = resp.json().await?;
        Ok(result)
    }

    #[allow(dead_code)]
    pub async fn get_pending(&self, request_id: &str) -> Result<Value> {
        let url = format!("{}/v1/approvals/{}", self.base_url, request_id);
        let resp = self.client.get(&url).send().await?;
        let result: Value = resp.json().await?;
        Ok(result)
    }
}

impl Default for GatewayClient {
    fn default() -> Self {
        Self::new()
    }
}
