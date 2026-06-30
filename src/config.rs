use anyhow::{Context, Result};
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct Config {
    // Local llama.cpp paths — validated at startup; reserved for model-GGUF
    // attestation (Phase 1) and an optional local supervisor.
    #[allow(dead_code)]
    pub llama_bin: PathBuf,
    #[allow(dead_code)]
    pub model_path: PathBuf,
    pub llama_port: u16,
    pub gateway_port: u16,
    pub system_prompt: String,
    pub ram_moat: PathBuf,
    pub log_dir: PathBuf,
    pub training_dir: PathBuf,
    pub evals_dir: PathBuf,
    pub qdrant_url: Option<String>,
    /// OpenAI-compatible backend base URL (ends in `/v1`). Defaults to the local
    /// llama.cpp server; override with `BACKEND_URL` to use any provider.
    pub backend_base: String,
    pub backend_api_key: Option<String>,
    pub backend_model: String,
    /// True when serving the bundled local llama.cpp (enables binary/model checks).
    pub local_backend: bool,
    /// Confined workspace for the iterative dev-loop tools (cargo new/build/test).
    pub workspace_dir: PathBuf,
    pub dev_loop_enabled: bool,
}

impl Config {
    pub fn load() -> Result<Self> {
        dotenvy::dotenv().ok();

        let llama_bin = PathBuf::from(std::env::var("LLAMA_BIN").context("LLAMA_BIN not set")?);
        let model_path = PathBuf::from(std::env::var("MODEL_PATH").context("MODEL_PATH not set")?);
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
            std::env::var("RAM_MOAT").unwrap_or_else(|_| "/tmp/ornith-ram-moat".into()),
        );
        let log_dir = PathBuf::from(std::env::var("LOG_DIR").unwrap_or_else(|_| "logs".into()));
        let training_dir =
            PathBuf::from(std::env::var("TRAINING_DIR").unwrap_or_else(|_| "training".into()));
        let evals_dir =
            PathBuf::from(std::env::var("EVALS_DIR").unwrap_or_else(|_| "evals".into()));
        let qdrant_url = std::env::var("QDRANT_URL").ok().filter(|s| !s.is_empty());

        // Pluggable model backend. Default = local llama.cpp; BACKEND_URL points at
        // any OpenAI-compatible provider (must include the /v1 path).
        let backend_override = std::env::var("BACKEND_URL").ok().filter(|s| !s.is_empty());
        let local_backend = backend_override.is_none();
        let backend_base =
            backend_override.unwrap_or_else(|| format!("http://127.0.0.1:{llama_port}/v1"));
        let backend_api_key = std::env::var("BACKEND_API_KEY")
            .ok()
            .filter(|s| !s.is_empty());
        let backend_model = std::env::var("BACKEND_MODEL").unwrap_or_else(|_| "ornith".into());

        let workspace_dir = PathBuf::from(
            std::env::var("ORNITH_WORKSPACE").unwrap_or_else(|_| "/tmp/ornith-workspace".into()),
        );
        let dev_loop_enabled = std::env::var("ORNITH_DEV_LOOP")
            .map(|v| v != "0")
            .unwrap_or(true);

        // Bundled-binary/model checks only matter when we serve the local backend.
        if local_backend {
            if !llama_bin.exists() {
                anyhow::bail!("LLAMA_BIN not found: {}", llama_bin.display());
            }
            if !model_path.exists() {
                anyhow::bail!("MODEL_PATH not found: {}", model_path.display());
            }
        }

        Ok(Self {
            llama_bin,
            model_path,
            llama_port,
            gateway_port,
            system_prompt,
            ram_moat,
            log_dir,
            training_dir,
            evals_dir,
            qdrant_url,
            backend_base,
            backend_api_key,
            backend_model,
            local_backend,
            workspace_dir,
            dev_loop_enabled,
        })
    }

    /// OpenAI-compatible base URL for the active backend (ends in `/v1`).
    pub fn backend_base(&self) -> &str {
        &self.backend_base
    }

    /// Backend reachability via the OpenAI-compatible `/models` endpoint —
    /// works for llama.cpp and any compliant provider.
    pub async fn check_backend(&self) -> Result<()> {
        let url = format!("{}/models", self.backend_base);
        let mut req = reqwest::Client::new().get(&url);
        if let Some(key) = &self.backend_api_key {
            req = req.bearer_auth(key);
        }
        let resp = req.send().await.context("Failed to reach model backend")?;
        if !resp.status().is_success() {
            anyhow::bail!("backend not healthy: HTTP {}", resp.status());
        }
        Ok(())
    }
}
