use ratatui::style::Color;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
    #[allow(dead_code)]
    pub tool_calls: Option<Vec<ToolCall>>,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct ToolCall {
    pub name: String,
    pub args: serde_json::Value,
    pub risk: String,
    pub tool_index: usize,
}

#[derive(Debug, Clone)]
pub struct PendingApproval {
    pub request_id: String,
    pub tool_name: String,
    pub tool_index: usize,
    pub args: serde_json::Value,
    pub risk: RiskLevel,
    #[allow(dead_code)]
    pub modified_args: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RiskLevel {
    Low,
    Medium,
    High,
}

impl RiskLevel {
    pub fn color(&self) -> Color {
        match self {
            RiskLevel::Low => Color::Green,
            RiskLevel::Medium => Color::Yellow,
            RiskLevel::High => Color::Red,
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            RiskLevel::Low => "LOW",
            RiskLevel::Medium => "MED",
            RiskLevel::High => "HIGH",
        }
    }

    pub fn from_str(s: &str) -> Self {
        match s {
            "LOW" => RiskLevel::Low,
            "HIGH" => RiskLevel::High,
            _ => RiskLevel::Medium,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[derive(Default)]
pub struct PipelineState {
    pub l3_route: Option<String>,
    pub l4_memories: Option<usize>,
    pub l5_messages: Option<usize>,
    pub l5_5_compression: Option<String>,
    pub l7_tool: Option<String>,
    pub l8_critique: Option<bool>,
    pub l9_tokens: Option<String>,
}


#[derive(Debug, Clone, PartialEq)]
#[allow(dead_code)]
pub enum AppMode {
    Chat,
    Approval,
    Error,
}

#[derive(Debug, Clone)]
pub struct App {
    pub messages: Vec<ChatMessage>,
    pub current_input: String,
    pub input_history: Vec<String>,
    pub pending_approvals: Vec<PendingApproval>,
    pub current_request_id: Option<String>,
    pub pipeline: PipelineState,
    pub mode: AppMode,
    pub status_msg: Option<String>,
    pub streaming_content: String,
    pub is_typing: bool,
    pub connected: bool,
}

impl App {
    pub fn new() -> Self {
        Self {
            messages: Vec::new(),
            current_input: String::new(),
            input_history: Vec::new(),
            pending_approvals: Vec::new(),
            current_request_id: None,
            pipeline: PipelineState::default(),
            mode: AppMode::Chat,
            status_msg: Some("Connecting...".to_string()),
            streaming_content: String::new(),
            is_typing: false,
            connected: false,
        }
    }

    pub fn push_message(&mut self, role: &str, content: String) {
        self.messages.push(ChatMessage {
            role: role.to_string(),
            content,
            tool_calls: None,
        });
    }

    pub fn append_token(&mut self, token: &str) {
        self.streaming_content.push_str(token);
        self.is_typing = true;
    }

    pub fn finalize_streaming(&mut self) {
        if !self.streaming_content.is_empty() {
            self.push_message("assistant", self.streaming_content.clone());
            self.streaming_content.clear();
            self.is_typing = false;
        }
    }

    pub fn add_pending_approval(&mut self, approval: PendingApproval) {
        self.mode = AppMode::Approval;
        self.pending_approvals.push(approval);
    }

    pub fn clear_pending_approvals(&mut self) {
        self.pending_approvals.clear();
        self.mode = AppMode::Chat;
    }

    pub fn update_pipeline(&mut self, line: &str) {
        if let Some(route) = line.strip_prefix("PIPE L3 route=") {
            self.pipeline.l3_route = Some(route.trim().to_string());
        } else if let Some(n) = line.strip_prefix("PIPE L4 memories=") {
            if let Ok(count) = n.trim().parse::<usize>() {
                self.pipeline.l4_memories = Some(count);
            }
        } else if let Some(n) = line.strip_prefix("PIPE L5 messages=") {
            if let Ok(count) = n.trim().parse::<usize>() {
                self.pipeline.l5_messages = Some(count);
            }
        } else if line.contains("L5.5 compressed") {
            self.pipeline.l5_5_compression = Some(line.trim().to_string());
        } else if let Some(tool) = line.strip_prefix("Tool executed:") {
            self.pipeline.l7_tool = Some(tool.trim().to_string());
        } else if let Some(detail) = line.strip_prefix("PIPE L8 ") {
            if detail.contains("critique=true") {
                self.pipeline.l8_critique = Some(true);
            } else if detail.contains("critique=false") {
                self.pipeline.l8_critique = Some(false);
            }
        } else if let Some(stats) = line.strip_prefix("PIPE L9 ") {
            self.pipeline.l9_tokens = Some(stats.trim().to_string());
        }
    }

    pub fn reset_pipeline(&mut self) {
        self.pipeline = PipelineState::default();
    }
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}
