use anyhow::{Context, Result};
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct Config {
    pub llama_bin: PathBuf,
    pub model_path: PathBuf,
    pub llama_port: u16,
    pub gateway_port: u16,
    pub system_prompt: String,
    pub ram_moat: PathBuf,
    pub log_dir: PathBuf,
}

impl Config {
    pub fn load() -> Result<Self> {
        dotenvy::dotenv().ok();

        let llama_bin = PathBuf::from(
            std::env::var("LLAMA_BIN").context("LLAMA_BIN not set")?
        );
        let model_path = PathBuf::from(
            std::env::var("MODEL_PATH").context("MODEL_PATH not set")?
        );
        let llama_port: u16 = std::env::var("LLAMA_PORT")
            .unwrap_or_else(|_| "8081".into())
            .parse()
            .context("LLAMA_PORT must be a valid port number")?;
        let gateway_port: u16 = std::env::var("GATEWAY_PORT")
            .unwrap_or_else(|_| "9000".into())
            .parse()
            .context("GATEWAY_PORT must be a valid port number")?;
        let system_prompt = std::env::var("SYSTEM_PROMPT")
            .unwrap_or_else(|_| "You are a helpful AI assistant.".into());
        let ram_moat = PathBuf::from(
            std::env::var("RAM_MOAT").unwrap_or_else(|_| "/tmp/ornith-ram-moat".into())
        );
        let log_dir = PathBuf::from(
            std::env::var("LOG_DIR").unwrap_or_else(|_| "logs".into())
        );

        if !llama_bin.exists() {
            anyhow::bail!("LLAMA_BIN not found: {}", llama_bin.display());
        }
        if !model_path.exists() {
            anyhow::bail!("MODEL_PATH not found: {}", model_path.display());
        }

        Ok(Self {
            llama_bin,
            model_path,
            llama_port,
            gateway_port,
            system_prompt,
            ram_moat,
            log_dir,
        })
    }

    pub fn llama_base_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.llama_port)
    }

    pub async fn check_llama_health(&self) -> Result<()> {
        let url = format!("{}/health", self.llama_base_url());
        let resp = reqwest::get(&url)
            .await
            .context("Failed to reach llama-server")?;
        let body: serde_json::Value = resp.json().await.context("Invalid health response")?;
        if body.get("status").and_then(|v| v.as_str()) != Some("ok") {
            anyhow::bail!("llama-server not healthy: {}", body);
        }
        Ok(())
    }
}
