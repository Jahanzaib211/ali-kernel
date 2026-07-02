use ratatui::style::Color;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

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

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
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
    Command,
    Approval,
    Error,
}

/// One line in the live event feed, parsed from gateway.log.
#[derive(Debug, Clone)]
pub struct LiveEvent {
    pub ts: String,
    pub tag: String,
    pub text: String,
}

impl LiveEvent {
    pub fn tag_color(&self) -> Color {
        match self.tag.as_str() {
            "L5.5" => Color::Magenta,
            "L3" => Color::Cyan,
            "L4" => Color::Blue,
            "L5" => Color::LightBlue,
            "L7" | "TOOL" => Color::Yellow,
            "L8" => Color::Green,
            "L9" => Color::LightGreen,
            _ => Color::Gray,
        }
    }
}

const MAX_LIVE_EVENTS: usize = 300;

/// Aggregates from the gateway's usage ledger (`GET /stats`) — real billed
/// tokens, not the char-count heuristic shown in the status bar.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SavingsStats {
    pub today_requests: u64,
    pub today_spend_usd: f64,
    pub today_saved_usd: f64,
    pub all_requests: u64,
    pub all_spend_usd: f64,
    pub all_saved_usd: f64,
    pub all_compression_pct: f64,
}

impl SavingsStats {
    pub fn from_value(v: &serde_json::Value) -> Self {
        let f = |section: &str, key: &str| v[section][key].as_f64().unwrap_or(0.0);
        let u = |section: &str, key: &str| v[section][key].as_u64().unwrap_or(0);
        Self {
            today_requests: u("today", "requests"),
            today_spend_usd: f("today", "actual_spend_usd"),
            today_saved_usd: f("today", "estimated_savings_usd"),
            all_requests: u("all_time", "requests"),
            all_spend_usd: f("all_time", "actual_spend_usd"),
            all_saved_usd: f("all_time", "estimated_savings_usd"),
            all_compression_pct: f("all_time", "compression_pct"),
        }
    }
}

/// Remove ANSI escape sequences without pulling in a regex dependency.
pub fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            if chars.peek() == Some(&'[') {
                chars.next();
                for e in chars.by_ref() {
                    if ('@'..='~').contains(&e) {
                        break;
                    }
                }
            }
            continue;
        }
        out.push(c);
    }
    out
}

#[derive(Debug, Clone)]
pub struct App {
    pub messages: Vec<ChatMessage>,
    pub current_input: String,
    /// Cursor position in `current_input`, measured in chars.
    pub cursor: usize,
    pub input_history: Vec<String>,
    pub history_index: Option<usize>,
    pub pending_approvals: Vec<PendingApproval>,
    pub current_request_id: Option<String>,
    pub pipeline: PipelineState,
    pub mode: AppMode,
    pub status_msg: Option<String>,
    pub streaming_content: String,
    pub is_typing: bool,
    pub connected: bool,
    pub live_events: VecDeque<LiveEvent>,
    /// Total chars saved by L5.5 compression since the TUI started.
    pub saved_chars: u64,
    pub compress_events: u64,
    /// Transcript scroll: wrapped lines up from the bottom. 0 = follow live output.
    pub scroll_offset: usize,
    /// Latest usage-ledger aggregates from the gateway's /stats endpoint.
    pub savings: Option<SavingsStats>,
}

impl App {
    pub fn new() -> Self {
        Self {
            messages: Vec::new(),
            current_input: String::new(),
            cursor: 0,
            input_history: Vec::new(),
            history_index: None,
            pending_approvals: Vec::new(),
            current_request_id: None,
            pipeline: PipelineState::default(),
            mode: AppMode::Chat,
            status_msg: Some("Connecting...".to_string()),
            streaming_content: String::new(),
            is_typing: false,
            connected: false,
            live_events: VecDeque::new(),
            saved_chars: 0,
            compress_events: 0,
            scroll_offset: 0,
            savings: None,
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
        }
        self.is_typing = false;
    }

    pub fn add_pending_approval(&mut self, approval: PendingApproval) {
        self.mode = AppMode::Approval;
        self.pending_approvals.push(approval);
    }

    pub fn clear_pending_approvals(&mut self) {
        self.pending_approvals.clear();
        self.mode = AppMode::Chat;
    }

    // --- input editing (char-boundary safe) ---

    fn byte_at(&self, char_idx: usize) -> usize {
        self.current_input
            .char_indices()
            .nth(char_idx)
            .map(|(b, _)| b)
            .unwrap_or(self.current_input.len())
    }

    pub fn insert_char(&mut self, c: char) {
        let b = self.byte_at(self.cursor);
        self.current_input.insert(b, c);
        self.cursor += 1;
    }

    pub fn insert_str(&mut self, s: &str) {
        let b = self.byte_at(self.cursor);
        self.current_input.insert_str(b, s);
        self.cursor += s.chars().count();
    }

    pub fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let b = self.byte_at(self.cursor - 1);
        self.current_input.remove(b);
        self.cursor -= 1;
    }

    pub fn delete_forward(&mut self) {
        if self.cursor >= self.current_input.chars().count() {
            return;
        }
        let b = self.byte_at(self.cursor);
        self.current_input.remove(b);
    }

    pub fn cursor_left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    pub fn cursor_right(&mut self) {
        let len = self.current_input.chars().count();
        if self.cursor < len {
            self.cursor += 1;
        }
    }

    pub fn cursor_home(&mut self) {
        self.cursor = 0;
    }

    pub fn cursor_end(&mut self) {
        self.cursor = self.current_input.chars().count();
    }

    pub fn take_input(&mut self) -> String {
        self.cursor = 0;
        self.history_index = None;
        std::mem::take(&mut self.current_input)
    }

    pub fn set_input(&mut self, s: String) {
        self.cursor = s.chars().count();
        self.current_input = s;
    }

    pub fn history_prev(&mut self) {
        if self.input_history.is_empty() {
            return;
        }
        let idx = match self.history_index {
            None => self.input_history.len() - 1,
            Some(0) => 0,
            Some(i) => i - 1,
        };
        self.history_index = Some(idx);
        self.set_input(self.input_history[idx].clone());
    }

    pub fn history_next(&mut self) {
        match self.history_index {
            None => {}
            Some(i) if i + 1 < self.input_history.len() => {
                self.history_index = Some(i + 1);
                self.set_input(self.input_history[i + 1].clone());
            }
            Some(_) => {
                self.history_index = None;
                self.set_input(String::new());
            }
        }
    }

    // --- live event feed ---

    /// Ingest one raw gateway.log line: strip ANSI, extract timestamp,
    /// route PIPE lines into both the pipeline snapshot and the live feed.
    pub fn ingest_log_line(&mut self, raw: &str) {
        let clean = strip_ansi(raw);
        let (payload, ts) = if let Some(pos) = clean.find("PIPE ") {
            (&clean[pos..], extract_time(&clean))
        } else if let Some(pos) = clean.find("Tool executed:") {
            (&clean[pos..], extract_time(&clean))
        } else {
            return;
        };

        self.update_pipeline(payload);

        let (tag, text) = if let Some(rest) = payload.strip_prefix("PIPE ") {
            let mut parts = rest.splitn(2, ' ');
            let tag = parts.next().unwrap_or("?").to_string();
            let text = parts.next().unwrap_or("").to_string();
            (tag, text)
        } else {
            ("TOOL".to_string(), payload.trim().to_string())
        };

        if tag == "L5.5" {
            if let Some((before, after)) = parse_compression(&text) {
                self.saved_chars += before.saturating_sub(after);
                self.compress_events += 1;
            }
        }

        self.live_events.push_back(LiveEvent { ts, tag, text });
        while self.live_events.len() > MAX_LIVE_EVENTS {
            self.live_events.pop_front();
        }
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
        } else if let Some(rest) = line.strip_prefix("PIPE L5.5 ") {
            self.pipeline.l5_5_compression = Some(rest.trim().to_string());
        } else if let Some(rest) = line.strip_prefix("PIPE L7 ") {
            if let Some(name) = rest.strip_prefix("tool_executed name=") {
                let parts: Vec<&str> = name.splitn(2, ' ').collect();
                self.pipeline.l7_tool = Some(parts.first().unwrap_or(&name).to_string());
            } else if let Some(tool) = rest.strip_prefix("human_gate_pending tool=") {
                let tool_name = tool.split_whitespace().next().unwrap_or(tool);
                self.pipeline.l7_tool = Some(tool_name.to_string());
            } else if let Some(tool) = rest.strip_prefix("human_gate_approved tool=") {
                let tool_name = tool.split_whitespace().next().unwrap_or(tool);
                self.pipeline.l7_tool = Some(tool_name.to_string());
            }
        } else if let Some(detail) = line.strip_prefix("Tool executed:") {
            let parts: Vec<&str> = detail.split_whitespace().collect();
            if let Some(name) = parts.first() {
                self.pipeline.l7_tool = Some(name.to_string());
            }
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

/// Pull "HH:MM:SS" out of a tracing timestamp like "2026-07-02T01:33:49.889064Z".
fn extract_time(line: &str) -> String {
    line.split('T')
        .nth(1)
        .and_then(|t| t.get(..8))
        .unwrap_or("")
        .to_string()
}

/// Parse "compressed: 51200 → 8000 chars (-84%) ..." into (before, after).
fn parse_compression(text: &str) -> Option<(u64, u64)> {
    let rest = text.strip_prefix("compressed:")?.trim_start();
    let mut parts = rest.split_whitespace();
    let before: u64 = parts.next()?.parse().ok()?;
    let arrow = parts.next()?;
    if arrow != "→" && arrow != "->" {
        return None;
    }
    let after: u64 = parts.next()?.parse().ok()?;
    Some((before, after))
}

/// Human-readable char count: 1234 → "1.2K", 1234567 → "1.2M".
pub fn human_chars(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}K", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_ansi() {
        let s = "\x1b[2m2026-07-02T01:33:49.889064Z\x1b[0m \x1b[32m INFO\x1b[0m PIPE L5.5 x";
        assert_eq!(
            strip_ansi(s),
            "2026-07-02T01:33:49.889064Z  INFO PIPE L5.5 x"
        );
    }

    #[test]
    fn ingests_compression_line() {
        let mut app = App::new();
        let raw = "\x1b[2m2026-07-02T01:33:49.889064Z\x1b[0m \x1b[32m INFO\x1b[0m \x1b[2mali_kernel::anthropic_proxy\x1b[0m\x1b[2m:\x1b[0m PIPE L5.5 compressed: 51200 → 8000 chars (-84%) [claude-proxy]";
        app.ingest_log_line(raw);
        assert_eq!(app.live_events.len(), 1);
        let ev = &app.live_events[0];
        assert_eq!(ev.tag, "L5.5");
        assert_eq!(ev.ts, "01:33:49");
        assert_eq!(app.saved_chars, 43200);
        assert!(app.pipeline.l5_5_compression.is_some());
    }

    #[test]
    fn input_editing_is_char_safe() {
        let mut app = App::new();
        app.insert_str("héllo");
        app.cursor_left();
        app.backspace();
        assert_eq!(app.current_input, "hélo");
        app.cursor_end();
        app.insert_char('!');
        assert_eq!(app.current_input, "hélo!");
    }
}
