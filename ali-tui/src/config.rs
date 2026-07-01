use ratatui::style::Color;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct Theme {
    pub bg: Color,
    pub fg: Color,
    pub accent: Color,
    pub accent2: Color,
    pub success: Color,
    pub warning: Color,
    pub error: Color,
    pub user_msg: Color,
    pub assistant_msg: Color,
    pub tool_msg: Color,
    pub border: Color,
    pub muted: Color,
}

impl Default for Theme {
    fn default() -> Self {
        themes::dark()
    }
}

pub mod themes {
    use super::*;

    pub fn dark() -> Theme {
        Theme {
            bg: Color::Rgb(16, 16, 16),
            fg: Color::Rgb(220, 220, 220),
            accent: Color::Rgb(0, 200, 255),
            accent2: Color::Rgb(100, 100, 255),
            success: Color::Rgb(0, 200, 100),
            warning: Color::Rgb(255, 180, 0),
            error: Color::Rgb(220, 60, 60),
            user_msg: Color::Rgb(0, 180, 255),
            assistant_msg: Color::Rgb(100, 220, 100),
            tool_msg: Color::Rgb(255, 200, 50),
            border: Color::Rgb(60, 60, 60),
            muted: Color::Rgb(100, 100, 100),
        }
    }

    pub fn matrix() -> Theme {
        Theme {
            bg: Color::Rgb(0, 0, 0),
            fg: Color::Rgb(0, 255, 0),
            accent: Color::Rgb(0, 255, 100),
            accent2: Color::Rgb(0, 200, 50),
            success: Color::Rgb(0, 255, 0),
            warning: Color::Rgb(180, 255, 0),
            error: Color::Rgb(255, 50, 50),
            user_msg: Color::Rgb(0, 255, 200),
            assistant_msg: Color::Rgb(0, 255, 0),
            tool_msg: Color::Rgb(200, 255, 0),
            border: Color::Rgb(0, 80, 0),
            muted: Color::Rgb(0, 60, 0),
        }
    }

    pub fn monokai() -> Theme {
        Theme {
            bg: Color::Rgb(39, 40, 34),
            fg: Color::Rgb(248, 248, 242),
            accent: Color::Rgb(249, 38, 114),
            accent2: Color::Rgb(102, 217, 230),
            success: Color::Rgb(166, 226, 46),
            warning: Color::Rgb(230, 219, 116),
            error: Color::Rgb(248, 62, 62),
            user_msg: Color::Rgb(102, 217, 230),
            assistant_msg: Color::Rgb(166, 226, 46),
            tool_msg: Color::Rgb(230, 219, 116),
            border: Color::Rgb(80, 80, 60),
            muted: Color::Rgb(120, 120, 100),
        }
    }

    pub fn gruvbox() -> Theme {
        Theme {
            bg: Color::Rgb(40, 40, 35),
            fg: Color::Rgb(213, 196, 161),
            accent: Color::Rgb(214, 93, 14),
            accent2: Color::Rgb(235, 219, 178),
            success: Color::Rgb(152, 151, 26),
            warning: Color::Rgb(215, 135, 0),
            error: Color::Rgb(204, 68, 68),
            user_msg: Color::Rgb(131, 165, 152),
            assistant_msg: Color::Rgb(152, 151, 26),
            tool_msg: Color::Rgb(215, 153, 33),
            border: Color::Rgb(80, 70, 55),
            muted: Color::Rgb(100, 90, 75),
        }
    }

    pub fn nord() -> Theme {
        Theme {
            bg: Color::Rgb(46, 52, 64),
            fg: Color::Rgb(216, 222, 233),
            accent: Color::Rgb(136, 192, 208),
            accent2: Color::Rgb(129, 161, 193),
            success: Color::Rgb(163, 190, 140),
            warning: Color::Rgb(235, 203, 139),
            error: Color::Rgb(191, 97, 106),
            user_msg: Color::Rgb(136, 192, 208),
            assistant_msg: Color::Rgb(163, 190, 140),
            tool_msg: Color::Rgb(235, 203, 139),
            border: Color::Rgb(67, 76, 94),
            muted: Color::Rgb(107, 114, 128),
        }
    }

    pub fn by_name(name: &str) -> Theme {
        match name {
            "matrix" => matrix(),
            "monokai" => monokai(),
            "gruvbox" => gruvbox(),
            "nord" => nord(),
            _ => dark(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TuiConfig {
    pub gateway_port: u16,
    pub gateway_host: String,
    pub human_gate_enabled: bool,
    pub llama_port: u16,
    pub backend_url: String,
    pub backend_api_key: Option<String>,
    pub backend_model: String,
    pub system_prompt: String,
    pub human_gate: bool,
    pub claude_enabled: bool,
    pub claude_api_key: Option<String>,
    pub claude_model: String,
    pub claude_compression_ratio: f32,
    pub tui_theme: String,
    pub tui_pipeline_panel: bool,
    pub log_dir: PathBuf,
    pub identity_version: String,
    pub identity_git_hash: String,
    pub identity_build_ts: String,
    #[serde(skip)]
    pub theme: Theme,
}

impl TuiConfig {
    pub fn apply_theme(&mut self) {
        self.theme = themes::by_name(&self.tui_theme);
    }
}

impl Default for TuiConfig {
    fn default() -> Self {
        Self {
            gateway_port: 9000,
            gateway_host: "127.0.0.1".into(),
            human_gate_enabled: false,
            llama_port: 8081,
            backend_url: "http://127.0.0.1:8081/v1".into(),
            backend_api_key: None,
            backend_model: "ornith".into(),
            system_prompt: "You are a helpful AI assistant.".into(),
            human_gate: false,
            claude_enabled: false,
            claude_api_key: None,
            claude_model: "claude-sonnet-4-20250514".into(),
            claude_compression_ratio: 0.3,
            tui_theme: "dark".into(),
            tui_pipeline_panel: true,
            log_dir: std::env::current_dir()
                .unwrap_or_else(|_| PathBuf::from("."))
                .join("logs"),
            identity_version: env!("CARGO_PKG_VERSION").into(),
            theme: Theme::default(),
            identity_git_hash: "unknown".into(),
            identity_build_ts: "unknown".into(),
        }
    }
}

impl TuiConfig {
    pub async fn load() -> Self {
        let mut cfg = Self::default();

        // Layer: environment variables (highest priority in TUI)
        if let Ok(v) = std::env::var("ALI_GATEWAY_PORT") {
            if let Ok(p) = v.parse() {
                cfg.gateway_port = p;
            }
        }
        if let Ok(v) = std::env::var("ALI_GATEWAY_HOST") {
            cfg.gateway_host = v;
        }
        if let Ok(v) = std::env::var("ALI_HUMAN_GATE") {
            cfg.human_gate_enabled = v == "1";
            cfg.human_gate = v == "1";
        }
        if let Ok(v) = std::env::var("ALI_LLAMA_PORT") {
            if let Ok(p) = v.parse() {
                cfg.llama_port = p;
            }
        }
        if let Ok(v) = std::env::var("BACKEND_URL") {
            cfg.backend_url = v;
        }
        if let Ok(v) = std::env::var("BACKEND_API_KEY") {
            cfg.backend_api_key = Some(v);
        }
        if let Ok(v) = std::env::var("BACKEND_MODEL") {
            cfg.backend_model = v;
        }
        if let Ok(v) = std::env::var("SYSTEM_PROMPT") {
            cfg.system_prompt = v;
        }
        if let Ok(v) = std::env::var("CLAUDE_ENABLED") {
            cfg.claude_enabled = v == "true" || v == "1";
        }
        if let Ok(v) = std::env::var("CLAUDE_API_KEY") {
            cfg.claude_api_key = Some(v);
        }
        if let Ok(v) = std::env::var("CLAUDE_MODEL") {
            cfg.claude_model = v;
        }
        if let Ok(v) = std::env::var("ALI_CONFIG_DIR") {
            let config_path = PathBuf::from(v).join("config.toml");
            if config_path.exists() {
                if let Ok(content) = tokio::fs::read_to_string(&config_path).await {
                    if let Ok(toml::Value::Table(m)) = content.parse() {
                        Self::merge_toml(&mut cfg, m);
                    }
                }
            }
        }

        // Layer: user config file
        if let Some(config_dir) = dirs::config_dir() {
            let config_path = config_dir.join("ali").join("config.toml");
            if config_path.exists() {
                if let Ok(content) = tokio::fs::read_to_string(&config_path).await {
                    if let Ok(toml::Value::Table(m)) = content.parse() {
                        Self::merge_toml(&mut cfg, m);
                    }
                }
            }
        }

        // Detect identity from binary / build info (static for now)
        cfg.identity_version = env!("CARGO_PKG_VERSION").into();

        // Override log_dir from env if set (ALI_LOG_DIR=.../logs)
        if let Ok(v) = std::env::var("ALI_LOG_DIR") {
            cfg.log_dir = PathBuf::from(v);
        }

        // Apply theme after all config layers are merged
        cfg.apply_theme();

        cfg
    }

    fn merge_toml(cfg: &mut TuiConfig, table: toml::map::Map<String, toml::Value>) {
        use toml::Value;
        if let Some(&Value::Integer(v)) = table.get("gateway_port") {
            cfg.gateway_port = v as u16;
        }
        if let Some(&Value::Integer(v)) = table.get("llama_port") {
            cfg.llama_port = v as u16;
        }
        if let Some(Value::String(ref v)) = table.get("backend_url") {
            cfg.backend_url = v.clone();
        }
        if let Some(Value::String(ref v)) = table.get("backend_model") {
            cfg.backend_model = v.clone();
        }
        if let Some(&Value::Boolean(v)) = table.get("human_gate") {
            cfg.human_gate_enabled = v;
            cfg.human_gate = v;
        }
        if let Some(&Value::Boolean(v)) = table.get("claude_enabled") {
            cfg.claude_enabled = v;
        }
        if let Some(Value::String(ref v)) = table.get("claude_model") {
            cfg.claude_model = v.clone();
        }
        if let Some(&Value::Float(v)) = table.get("claude_compression_ratio") {
            cfg.claude_compression_ratio = v as f32;
        }
        if let Some(Value::String(ref v)) = table.get("tui_theme") {
            cfg.tui_theme = v.clone();
        }
        if let Some(&Value::Boolean(v)) = table.get("tui_pipeline_panel") {
            cfg.tui_pipeline_panel = v;
        }
    }

    pub fn gateway_url(&self) -> String {
        format!("http://{}:{}", self.gateway_host, self.gateway_port)
    }

    pub fn set(&mut self, key: &str, value: &str) -> Option<()> {
        match key {
            "human_gate" | "human_gate_enabled" => {
                self.human_gate_enabled = value == "true" || value == "1";
                self.human_gate = self.human_gate_enabled;
            }
            "gateway_port" => self.gateway_port = value.parse().ok()?,
            "llama_port" => self.llama_port = value.parse().ok()?,
            "backend_url" => self.backend_url = value.to_string(),
            "backend_model" => self.backend_model = value.to_string(),
            "claude_enabled" => self.claude_enabled = value == "true" || value == "1",
            "claude_model" => self.claude_model = value.to_string(),
            "tui_theme" => self.tui_theme = value.to_string(),
            "tui_pipeline_panel" => self.tui_pipeline_panel = value != "false",
            _ => return None,
        }
        Some(())
    }

    pub async fn save(&self) -> std::io::Result<PathBuf> {
        let path = dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("ali")
            .join("config.toml");
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let table = toml::map::Map::from_iter([
            ("gateway_port".to_string(), toml::Value::Integer(self.gateway_port as i64)),
            ("llama_port".to_string(), toml::Value::Integer(self.llama_port as i64)),
            ("backend_url".to_string(), toml::Value::String(self.backend_url.clone())),
            ("backend_model".to_string(), toml::Value::String(self.backend_model.clone())),
            ("human_gate".to_string(), toml::Value::Boolean(self.human_gate_enabled)),
            ("claude_enabled".to_string(), toml::Value::Boolean(self.claude_enabled)),
            ("claude_model".to_string(), toml::Value::String(self.claude_model.clone())),
            ("claude_compression_ratio".to_string(), toml::Value::Float(self.claude_compression_ratio as f64)),
            ("tui_theme".to_string(), toml::Value::String(self.tui_theme.clone())),
            ("tui_pipeline_panel".to_string(), toml::Value::Boolean(self.tui_pipeline_panel)),
        ]);
        let content = toml::to_string_pretty(&toml::Value::Table(table)).unwrap_or_default();
        tokio::fs::write(&path, content).await?;
        Ok(path)
    }
}
