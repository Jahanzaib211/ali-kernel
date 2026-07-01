use anyhow::{Context, Result};
use figment::{
    providers::{Env, Format, Serialized, Toml},
    Figment,
};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::process::Command;

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AliConfig {
    // ─── Llama.cpp ────────────────────────────────────────────────────────────
    pub llama_bin: PathBuf,
    pub model_path: PathBuf,
    pub llama_port: u16,
    #[serde(default)]
    pub llama_n_ctx: usize,
    #[serde(default)]
    pub llama_n_gpu: i32,
    #[serde(default)]
    pub llama_n_threads: usize,
    #[serde(default)]
    pub llama_rope_freq_base: f32,
    #[serde(default)]
    pub llama_rope_freq_scale: f32,

    // ─── Gateway ─────────────────────────────────────────────────────────────
    pub gateway_port: u16,
    #[serde(default = "default_gateway_host")]
    pub gateway_host: String,
    pub human_gate_enabled: bool,
    #[serde(default = "default_human_gate_timeout")]
    pub human_gate_timeout_secs: u64,

    // ─── System Prompt ────────────────────────────────────────────────────────
    #[serde(default = "default_system_prompt")]
    pub system_prompt: String,

    // ─── Backend ─────────────────────────────────────────────────────────────
    #[serde(default)]
    pub backend_url: String,
    #[serde(default)]
    pub backend_api_key: Option<String>,
    #[serde(default = "default_backend_model")]
    pub backend_model: String,
    #[serde(default)]
    pub backend_temperature: f64,
    #[serde(default = "default_backend_max_tokens")]
    pub backend_max_tokens: u64,

    // ─── Pipeline ────────────────────────────────────────────────────────────
    #[serde(default = "default_router_strategy")]
    pub router_strategy: String,
    #[serde(default = "default_context_window_chars")]
    pub context_window_chars: usize,
    #[serde(default = "default_compression_budget")]
    pub compression_budget: usize,
    #[serde(default = "default_sliding_window_turns")]
    pub sliding_window_turns: usize,
    #[serde(default = "default_max_tool_rounds")]
    pub max_tool_rounds: usize,

    // ─── Memory ──────────────────────────────────────────────────────────────
    #[serde(default = "default_memory_backend")]
    pub memory_backend: String,
    #[serde(default)]
    pub qdrant_url: Option<String>,
    #[serde(default = "default_qdrant_collection")]
    pub qdrant_collection: String,
    #[serde(default = "default_top_k_memories")]
    pub top_k_memories: usize,
    #[serde(default = "default_min_similarity_score")]
    pub min_similarity_score: f32,
    #[serde(default)]
    pub memory_forget_after_days: Option<u32>,

    // ─── Ali Personal Storage ─────────────────────────────────────────────────
    pub ali_dir: PathBuf,

    // ─── Training ───────────────────────────────────────────────────────────
    pub training_dir: PathBuf,
    pub evals_dir: PathBuf,
    #[serde(default = "default_true")]
    pub sft_enabled: bool,
    #[serde(default = "default_true")]
    pub dpo_enabled: bool,
    #[serde(default = "default_critique_threshold")]
    pub critique_threshold: f32,
    #[serde(default = "default_min_score_for_positive_dpo")]
    pub min_score_for_positive_dpo: i32,
    #[serde(default)]
    pub train_on_eval_failures: bool,

    // ─── Dev Loop ────────────────────────────────────────────────────────────
    pub workspace_dir: PathBuf,
    #[serde(default = "default_true")]
    pub dev_loop_enabled: bool,
    #[serde(default)]
    pub bubblewrap_available: bool,
    #[serde(default = "default_cargo_timeout")]
    pub cargo_timeout_secs: u64,
    #[serde(default)]
    pub allowed_workspace_paths: Vec<PathBuf>,

    // ─── Safety ──────────────────────────────────────────────────────────────
    #[serde(default = "default_ram_moat")]
    pub ram_moat_path: PathBuf,
    #[serde(default = "default_true")]
    pub binary_attestation: bool,
    #[serde(default)]
    pub constitution_hashes: Vec<String>,
    #[serde(default)]
    pub allowed_tools: Vec<String>,
    #[serde(default)]
    pub denied_tools: Vec<String>,
    #[serde(default = "default_rate_limit")]
    pub rate_limit_per_minute: u32,

    // ─── System ──────────────────────────────────────────────────────────────
    pub log_dir: PathBuf,
    #[serde(default = "default_log_level")]
    pub log_level: String,
    #[serde(default = "default_true")]
    pub log_pipeline_json: bool,
    #[serde(default)]
    pub identity_version: String,
    #[serde(default)]
    pub identity_git_hash: String,
    #[serde(default)]
    pub identity_build_ts: String,
    #[serde(default)]
    pub identity_git_dirty: bool,

    // ─── TUI ────────────────────────────────────────────────────────────────
    #[serde(default = "default_tui_theme")]
    pub tui_theme: String,
    #[serde(default = "default_tui_keybindings")]
    pub tui_keybindings: String,
    #[serde(default = "default_true")]
    pub tui_status_bar: bool,
    #[serde(default = "default_true")]
    pub tui_pipeline_panel: bool,
    #[serde(default = "default_tui_chat_compression")]
    pub tui_chat_compression: usize,
    #[serde(default = "default_tui_command_mode_key")]
    pub tui_command_mode_key: String,

    // ─── Remote AI / Compression ────────────────────────────────────────────
    #[serde(default)]
    pub claude_api_key: Option<String>,
    #[serde(default = "default_claude_url")]
    pub claude_url: String,
    #[serde(default = "default_claude_model")]
    pub claude_model: String,
    #[serde(default = "default_claude_compression_ratio")]
    pub claude_compression_ratio: f32,
    #[serde(default = "default_false")]
    pub claude_enabled: bool,
    #[serde(default = "default_claude_max_burst_tokens")]
    pub claude_max_burst_tokens: usize,
    #[serde(default = "default_false")]
    pub claude_self_grade_enabled: bool,
    #[serde(default = "default_false")]
    pub claude_synthetic_data_enabled: bool,
}

fn default_gateway_host() -> String {
    "127.0.0.1".into()
}
fn default_human_gate_timeout() -> u64 {
    300
}
fn default_backend_model() -> String {
    "ornith".into()
}
fn default_backend_max_tokens() -> u64 {
    2048
}
fn default_router_strategy() -> String {
    "auto".into()
}
fn default_context_window_chars() -> usize {
    48000
}
fn default_compression_budget() -> usize {
    8000
}
fn default_sliding_window_turns() -> usize {
    10
}
fn default_max_tool_rounds() -> usize {
    4
}
fn default_memory_backend() -> String {
    "local".into()
}
fn default_qdrant_collection() -> String {
    "ornith_memory".into()
}
fn default_top_k_memories() -> usize {
    3
}
fn default_min_similarity_score() -> f32 {
    0.12
}
fn default_true() -> bool {
    true
}
fn default_false() -> bool {
    false
}
fn default_critique_threshold() -> f32 {
    3.0
}
fn default_min_score_for_positive_dpo() -> i32 {
    4
}
fn default_cargo_timeout() -> u64 {
    240
}
fn default_ram_moat() -> PathBuf {
    PathBuf::from("/tmp/ali-ram-moat")
}
fn default_ali_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".ali")
}
fn default_rate_limit() -> u32 {
    60
}
fn default_log_level() -> String {
    "info".into()
}
fn default_tui_theme() -> String {
    "dark".into()
}
fn default_tui_keybindings() -> String {
    "default".into()
}
fn default_tui_chat_compression() -> usize {
    200
}
fn default_tui_command_mode_key() -> String {
    ":".into()
}
fn default_claude_url() -> String {
    "https://api.anthropic.com/v1".into()
}
fn default_claude_model() -> String {
    "claude-sonnet-4-20250514".into()
}
fn default_claude_compression_ratio() -> f32 {
    0.3
}
fn default_claude_max_burst_tokens() -> usize {
    4096
}
fn default_system_prompt() -> String {
    "You are a helpful AI assistant.".into()
}

// ─── System Identity (layer 6 - lowest priority) ─────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct SystemIdentity {
    pub version: String,
    pub git_hash: String,
    pub git_dirty: bool,
    pub build_timestamp: String,
    pub rustc_version: String,
    pub cargo_features: Vec<String>,
    pub enabled_tools: Vec<String>,
    pub config_schema_version: String,
    pub hostname: String,
    pub total_config_options: usize,
}

impl SystemIdentity {
    pub fn detect() -> Self {
        let git_hash = Self::git_rev_parse();
        let git_dirty = Self::git_dirty();
        let build_timestamp =
            std::env::var("BUILD_TIMESTAMP").unwrap_or_else(|_| chrono::Utc::now().to_rfc3339());
        let rustc_version = rustc_version::version()
            .map(|v| v.to_string())
            .unwrap_or_else(|_| "unknown".into());
        let cargo_features = Self::get_cargo_features();
        let enabled_tools = Self::get_enabled_tools();
        let hostname = hostname::get()
            .map(|h| h.to_string_lossy().into_owned())
            .unwrap_or_else(|_| "unknown".into());

        Self {
            version: env!("CARGO_PKG_VERSION").into(),
            git_hash,
            git_dirty,
            build_timestamp,
            rustc_version,
            cargo_features,
            enabled_tools,
            config_schema_version: "1.0".into(),
            hostname,
            total_config_options: 55,
        }
    }

    fn git_rev_parse() -> String {
        Command::new("git")
            .args(["rev-parse", "--short", "HEAD"])
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim().to_string())
            .unwrap_or_else(|| "unknown".into())
    }

    fn git_dirty() -> bool {
        Command::new("git")
            .args(["status", "--porcelain"])
            .output()
            .map(|o| !o.stdout.is_empty())
            .unwrap_or(false)
    }

    fn get_cargo_features() -> Vec<String> {
        std::env::var("CARGO_FEATURE_*")
            .ok()
            .map(|s| s.split(',').map(String::from).collect())
            .unwrap_or_default()
    }

    fn get_enabled_tools() -> Vec<String> {
        vec![
            "read_file".into(),
            "write_file".into(),
            "bash_exec".into(),
            "crate_search".into(),
            "docs_lookup".into(),
            "cargo_new".into(),
            "cargo_add".into(),
            "cargo_build".into(),
            "cargo_check".into(),
            "cargo_test".into(),
            "cargo_clippy".into(),
            "write_project_file".into(),
        ]
    }
}

// ─── Layer 5: Build-time defaults ────────────────────────────────────────────────

fn build_defaults() -> AliConfig {
    AliConfig {
        // Llama.cpp
        llama_bin: PathBuf::from("/home/jahanzaib/llama.cpp/build/bin/llama-server"),
        model_path: PathBuf::from("/home/jahanzaib/ornith-1.0-9b-Q4_K_M.gguf"),
        llama_port: 8081,
        llama_n_ctx: 0,
        llama_n_gpu: 0,
        llama_n_threads: 0,
        llama_rope_freq_base: 0.0,
        llama_rope_freq_scale: 0.0,
        // Gateway
        gateway_port: 9000,
        gateway_host: default_gateway_host(),
        human_gate_enabled: false,
        human_gate_timeout_secs: default_human_gate_timeout(),
        // System Prompt
        system_prompt: default_system_prompt(),
        // Backend
        backend_url: String::new(),
        backend_api_key: None,
        backend_model: default_backend_model(),
        backend_temperature: 0.6,
        backend_max_tokens: default_backend_max_tokens(),
        // Pipeline
        router_strategy: default_router_strategy(),
        context_window_chars: default_context_window_chars(),
        compression_budget: default_compression_budget(),
        sliding_window_turns: default_sliding_window_turns(),
        max_tool_rounds: default_max_tool_rounds(),
        // Memory
        memory_backend: default_memory_backend(),
        qdrant_url: None,
        qdrant_collection: default_qdrant_collection(),
        top_k_memories: default_top_k_memories(),
        min_similarity_score: default_min_similarity_score(),
        memory_forget_after_days: None,
        // Ali Personal
        ali_dir: default_ali_dir(),
        // Training
        training_dir: PathBuf::from("training"),
        evals_dir: PathBuf::from("evals"),
        sft_enabled: default_true(),
        dpo_enabled: default_true(),
        critique_threshold: default_critique_threshold(),
        min_score_for_positive_dpo: default_min_score_for_positive_dpo(),
        train_on_eval_failures: default_false(),
        // Dev Loop
        workspace_dir: PathBuf::from("/tmp/ali-workspace"),
        dev_loop_enabled: default_true(),
        bubblewrap_available: false,
        cargo_timeout_secs: default_cargo_timeout(),
        allowed_workspace_paths: vec![],
        // Safety
        ram_moat_path: default_ram_moat(),
        binary_attestation: default_true(),
        constitution_hashes: vec![],
        allowed_tools: vec![],
        denied_tools: vec![],
        rate_limit_per_minute: default_rate_limit(),
        // System
        log_dir: PathBuf::from("logs"),
        log_level: default_log_level(),
        log_pipeline_json: default_true(),
        identity_version: env!("CARGO_PKG_VERSION").into(),
        identity_git_hash: String::new(),
        identity_build_ts: String::new(),
        identity_git_dirty: false,
        // TUI
        tui_theme: default_tui_theme(),
        tui_keybindings: default_tui_keybindings(),
        tui_status_bar: default_true(),
        tui_pipeline_panel: default_true(),
        tui_chat_compression: default_tui_chat_compression(),
        tui_command_mode_key: default_tui_command_mode_key(),
        // Remote AI
        claude_api_key: None,
        claude_url: default_claude_url(),
        claude_model: default_claude_model(),
        claude_compression_ratio: default_claude_compression_ratio(),
        claude_enabled: default_false(),
        claude_max_burst_tokens: default_claude_max_burst_tokens(),
        claude_self_grade_enabled: default_false(),
        claude_synthetic_data_enabled: default_false(),
    }
}

// ─── Config Loading (6-layer figment stacking) ─────────────────────────────────────

#[allow(dead_code)]
impl AliConfig {
    pub fn load() -> Result<Self> {
        let identity = SystemIdentity::detect();

        // Discover config file paths
        let user_config_path = Self::user_config_path();
        let project_config_path = Self::project_config_path();

        // Layer 6 (lowest): Self-referential identity
        // Layer 5: Build-time defaults
        // Layer 4: Project config (.ali.toml in repo)
        // Layer 3: User config (~/.config/ali/config.toml)
        // Layer 2: Environment variables (ALI_* prefix)
        // Layer 1 (highest): CLI/runtime overrides (empty here, set in TUI)

        let mut figment = Figment::new()
            // Layer 6: Self-referential (baked at compile time)
            .merge(Serialized::defaults(identity.clone()))
            // Layer 5: Build defaults
            .merge(Serialized::defaults(build_defaults()))
            // Layer 4: Project-level config (repo .ali.toml)
            .join(Toml::file(&project_config_path).nested())
            // Layer 3: User config (~/.config/ali/config.toml)
            .join(Toml::file(&user_config_path).nested());

        // Layer 2: Environment variables — prefix ALI_ with ORNITH_ backward compat
        // ORNITH_* vars are handled in apply_ornith_backward_compat below
        figment = figment
            .merge(Env::prefixed("ALI_"))
            .merge(Env::prefixed("LLAMA_").only(&["LLAMA_BIN", "LLAMA_PORT", "MODEL_PATH"]));

        // Extract with re-mapping for backward compat env vars
        let mut config: AliConfig = figment.extract().context("config extraction failed")?;

        // Apply backward-compat ORNITH_* vars that figment might have missed
        Self::apply_ornith_backward_compat(&mut config);

        // Resolve backend_base from override or local llama
        if config.backend_url.is_empty() {
            config.backend_url = format!("http://127.0.0.1:{}/v1", config.llama_port);
        }

        // Fill identity fields from runtime detection
        config.identity_git_hash = identity.git_hash.clone();
        config.identity_build_ts = identity.build_timestamp.clone();
        config.identity_git_dirty = identity.git_dirty;

        // Validate local backend has required binaries only when using local llama
        if !config.backend_url.starts_with("http") || config.backend_url.contains("127.0.0.1") {
            if !config.llama_bin.exists() {
                anyhow::bail!(
                    "LLAMA_BIN not found: {} (or set BACKEND_URL for remote)",
                    config.llama_bin.display()
                );
            }
            if !config.model_path.exists() {
                anyhow::bail!("MODEL_PATH not found: {}", config.model_path.display());
            }
        }

        // Resolve training_dir and evals_dir relative to ali_dir if they're relative
        if !config.training_dir.is_absolute() {
            config.training_dir = config.ali_dir.join(&config.training_dir);
        }
        if !config.evals_dir.is_absolute() {
            config.evals_dir = config.ali_dir.join(&config.evals_dir);
        }

        Ok(config)
    }

    fn user_config_path() -> PathBuf {
        dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("ali")
            .join("config.toml")
    }

    fn project_config_path() -> PathBuf {
        PathBuf::from(".ali.toml")
    }

    fn apply_ornith_backward_compat(config: &mut AliConfig) {
        // ORNITH_WORKSPACE → ALI_WORKSPACE
        if let Ok(v) = std::env::var("ORNITH_WORKSPACE") {
            if !v.is_empty() {
                config.workspace_dir = PathBuf::from(v);
            }
        }
        // ORNITH_DEV_LOOP → ALI_DEV_LOOP
        if let Ok(v) = std::env::var("ORNITH_DEV_LOOP") {
            if v == "0" {
                config.dev_loop_enabled = false;
            }
        }
        // ORNITH_LOGS → ALI_LOG_DIR
        if let Ok(v) = std::env::var("ORNITH_LOGS") {
            if !v.is_empty() {
                config.log_dir = PathBuf::from(v);
            }
        }
        // SYSTEM_PROMPT backward compat
        if let Ok(v) = std::env::var("SYSTEM_PROMPT") {
            if !v.is_empty() {
                config.system_prompt = v;
            }
        }
        // ALI_HUMAN_GATE backward compat (was boolean as string "1")
        if let Ok(v) = std::env::var("ALI_HUMAN_GATE") {
            config.human_gate_enabled = v == "1";
        }
    }

    /// OpenAI-compatible base URL for the active backend (ends in `/v1`).
    pub fn backend_base(&self) -> &str {
        &self.backend_url
    }

    /// Backend reachability via the OpenAI-compatible `/models` endpoint.
    pub async fn check_backend(&self) -> Result<()> {
        let url = format!("{}/models", self.backend_url);
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

    /// Write current config to user config file (~/.config/ali/config.toml)
    pub fn save_user_config(&self) -> Result<PathBuf> {
        let path = Self::user_config_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let toml = toml::to_string_pretty(self)?;
        std::fs::write(&path, toml)?;
        Ok(path)
    }

    /// Get a config value by key name for TUI :get command
    pub fn get(&self, key: &str) -> Option<String> {
        let v = match key {
            // Llama.cpp
            "llama_bin" => serde_json::json!(self.llama_bin.to_string_lossy().to_string()),
            "model_path" => serde_json::json!(self.model_path.to_string_lossy().to_string()),
            "llama_port" => serde_json::json!(self.llama_port),
            "llama_n_ctx" => serde_json::json!(self.llama_n_ctx),
            "llama_n_gpu" => serde_json::json!(self.llama_n_gpu),
            "llama_n_threads" => serde_json::json!(self.llama_n_threads),
            // Gateway
            "gateway_port" => serde_json::json!(self.gateway_port),
            "human_gate_enabled" => serde_json::json!(self.human_gate_enabled),
            "human_gate_timeout_secs" => serde_json::json!(self.human_gate_timeout_secs),
            // Backend
            "backend_url" => serde_json::json!(self.backend_url),
            "backend_model" => serde_json::json!(self.backend_model),
            "backend_temperature" => serde_json::json!(self.backend_temperature),
            // Pipeline
            "context_window_chars" => serde_json::json!(self.context_window_chars),
            "compression_budget" => serde_json::json!(self.compression_budget),
            "sliding_window_turns" => serde_json::json!(self.sliding_window_turns),
            // Memory
            "memory_backend" => serde_json::json!(self.memory_backend),
            "top_k_memories" => serde_json::json!(self.top_k_memories),
            // Training
            "sft_enabled" => serde_json::json!(self.sft_enabled),
            "dpo_enabled" => serde_json::json!(self.dpo_enabled),
            "critique_threshold" => serde_json::json!(self.critique_threshold),
            // Dev Loop
            "dev_loop_enabled" => serde_json::json!(self.dev_loop_enabled),
            "workspace_dir" => serde_json::json!(self.workspace_dir.to_string_lossy().to_string()),
            // System
            "log_dir" => serde_json::json!(self.log_dir.to_string_lossy().to_string()),
            "log_level" => serde_json::json!(self.log_level),
            // Identity
            "identity_version" => serde_json::json!(self.identity_version),
            "identity_git_hash" => serde_json::json!(self.identity_git_hash),
            "identity_build_ts" => serde_json::json!(self.identity_build_ts),
            "identity_git_dirty" => serde_json::json!(self.identity_git_dirty),
            // TUI
            "tui_theme" => serde_json::json!(self.tui_theme),
            "tui_pipeline_panel" => serde_json::json!(self.tui_pipeline_panel),
            // Remote AI
            "claude_enabled" => serde_json::json!(self.claude_enabled),
            "claude_model" => serde_json::json!(self.claude_model),
            "claude_compression_ratio" => serde_json::json!(self.claude_compression_ratio),
            _ => return None,
        };
        Some(v.to_string())
    }

    /// Set a config value by key name (returns Some(()) on success)
    pub fn set(&mut self, key: &str, value: &str) -> Option<()> {
        match key {
            // Bool
            "human_gate_enabled" | "human_gate" => {
                self.human_gate_enabled = value == "true" || value == "1";
            }
            "sft_enabled" => self.sft_enabled = value == "true" || value == "1",
            "dpo_enabled" => self.dpo_enabled = value == "true" || value == "1",
            "dev_loop_enabled" | "dev_loop" => {
                self.dev_loop_enabled = value == "true" || value == "1";
            }
            "binary_attestation" => self.binary_attestation = value == "true" || value == "1",
            "claude_enabled" => self.claude_enabled = value == "true" || value == "1",
            "claude_self_grade_enabled" => {
                self.claude_self_grade_enabled = value == "true" || value == "1";
            }
            "claude_synthetic_data_enabled" => {
                self.claude_synthetic_data_enabled = value == "true" || value == "1";
            }
            "train_on_eval_failures" => {
                self.train_on_eval_failures = value == "true" || value == "1";
            }
            // u16
            "llama_port" => self.llama_port = value.parse().ok()?,
            "gateway_port" => self.gateway_port = value.parse().ok()?,
            // usize
            "llama_n_ctx" => self.llama_n_ctx = value.parse().ok()?,
            "llama_n_gpu" => self.llama_n_gpu = value.parse().ok()?,
            "llama_n_threads" => self.llama_n_threads = value.parse().ok()?,
            "context_window_chars" => self.context_window_chars = value.parse().ok()?,
            "compression_budget" => self.compression_budget = value.parse().ok()?,
            "sliding_window_turns" => self.sliding_window_turns = value.parse().ok()?,
            "max_tool_rounds" => self.max_tool_rounds = value.parse().ok()?,
            "top_k_memories" => self.top_k_memories = value.parse().ok()?,
            // f64
            "backend_temperature" => self.backend_temperature = value.parse().ok()?,
            "critique_threshold" => self.critique_threshold = value.parse().ok()?,
            // String
            "llama_bin" => self.llama_bin = PathBuf::from(value),
            "model_path" => self.model_path = PathBuf::from(value),
            "backend_url" => self.backend_url = value.to_string(),
            "backend_model" => self.backend_model = value.to_string(),
            "router_strategy" => self.router_strategy = value.to_string(),
            "memory_backend" => self.memory_backend = value.to_string(),
            "log_level" => self.log_level = value.to_string(),
            "tui_theme" => self.tui_theme = value.to_string(),
            // PathBuf
            "workspace_dir" => self.workspace_dir = PathBuf::from(value),
            "log_dir" => self.log_dir = PathBuf::from(value),
            "ram_moat_path" => self.ram_moat_path = PathBuf::from(value),
            // String with env lookup
            "claude_url" => self.claude_url = value.to_string(),
            "claude_model" => self.claude_model = value.to_string(),
            _ => return None,
        };
        Some(())
    }
}

impl Default for AliConfig {
    fn default() -> Self {
        build_defaults()
    }
}

/// Backward-compat type alias — all internal modules use `Config`
pub type Config = AliConfig;
