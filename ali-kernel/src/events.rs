//! Events — Thalamus: structured event frame protocol
//!
//! Nature psychology: The thalamus receives sensory input and routes it to the
//! appropriate processing areas. It also acts as a relay for the cerebral cortex.
//! Events are the sensory input of the agent — every meaningful thing that happens
//! gets packaged as a structured event and routed to whoever needs it.
//!
//! This is the wire protocol between the kernel and TUI. Instead of PIPE lines
//! in a log file, we emit structured events that the TUI can render immediately.
//!
//! Event flow:
//!   Kernel emits events → WebSocket/Channel → TUI renders live
//!
//! All times are UTC ISO-8601 strings.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Every event that can flow between kernel and TUI.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AliEvent {
    // ── Pipeline lifecycle ─────────────────────────────────────────────────
    /// L3 routing decision
    Route { route: String, temperature: f64 },
    /// L4 memory retrieval completed
    MemoriesRetrieved { count: usize, sources: Vec<MemorySource> },
    /// L5 context assembly completed
    ContextAssembled { token_count: usize, budget: usize },
    /// L6 inference started
    InferenceStarted { request_id: String },
    /// L6 inference completed
    InferenceDone { request_id: String, duration_ms: u64, tokens: u64 },
    /// L8 critique applied
    CritiqueApplied { rewritten: bool },
    /// L9 pipeline complete
    PipelineDone {
        request_id: String,
        latency_ms: u64,
        tool_called: bool,
        prompt_tokens: u64,
        completion_tokens: u64,
    },

    // ── Tool lifecycle ────────────────────────────────────────────────────
    /// Tool execution started
    ToolStart {
        id: String,
        name: String,
        arguments: serde_json::Value,
    },
    /// Tool emitted stdout/stderr
    ToolOutput {
        id: String,
        name: String,
        chunk: String,
        stream: ToolStream,
    },
    /// Tool completed
    ToolEnd {
        id: String,
        name: String,
        exit_code: Option<i32>,
        duration_ms: u64,
        success: bool,
        output_preview: String,
    },
    /// Tool result compressed
    ToolCompressed {
        id: String,
        original_chars: usize,
        compressed_chars: usize,
    },

    // ── Intent / Journal ──────────────────────────────────────────────────
    /// Intent state changed
    IntentUpdate {
        state: String,
        goal: String,
        attempt_count: usize,
    },
    /// Journal entry written
    JournalEntry {
        id: String,
        session_name: String,
        emotional_tone: String,
        energy_level: String,
    },
    /// Session started
    SessionStart { session_name: String, intent_summary: String },
    /// Session ended
    SessionEnd { session_name: String, duration_secs: u64 },

    // ── Identity / Feedback ───────────────────────────────────────────────
    /// Identity preference observed
    PreferenceInferred { key: String, value: String, confidence: f32 },
    /// User gave feedback on a response
    UserFeedback { score: u8, note: Option<String> },

    // ── System ────────────────────────────────────────────────────────────
    /// Human gate pending approval
    HumanGatePending {
        request_id: String,
        tool_name: String,
        risk: String,
        tool_index: usize,
    },
    /// Human gate approved
    HumanGateApproved { request_id: String, tool_name: String },
    /// Human gate denied
    HumanGateDenied { request_id: String, tool_name: String },
    /// Error occurred
    Error { code: String, message: String },
    /// Keepalive/heartbeat
    Ping { ts: DateTime<Utc> },
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ToolStream {
    Stdout,
    Stderr,
    Both,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemorySource {
    pub source: String,
    pub score: f32,
}

impl AliEvent {
    /// Parse a PIPE-formatted log line into an AliEvent.
    /// This is the backward-compatibility shim: existing PIPE L3/L4/etc lines
    /// in gateway.log are parsed into structured events for the TUI.
    pub fn from_pipe_line(line: &str) -> Option<Self> {
        let line = line.trim();
        if !line.starts_with("PIPE ") {
            return None;
        }

        let rest = line.strip_prefix("PIPE ").unwrap();

        if let Some(route) = rest.strip_prefix("L3 route=") {
            let parts: Vec<&str> = route.splitn(2, ' ').collect();
            let route = parts[0].trim().to_string();
            let temp = parts.get(1).and_then(|s| s.strip_prefix("temp="))
                .and_then(|s| s.parse::<f64>().ok())
                .unwrap_or(0.6);
            Some(AliEvent::Route { route, temperature: temp })
        } else if let Some(n) = rest.strip_prefix("L4 memories=") {
            let count = n.trim().parse::<usize>().unwrap_or(0);
            Some(AliEvent::MemoriesRetrieved { count, sources: Vec::new() })
        } else if let Some(n) = rest.strip_prefix("L5 messages=") {
            let count = n.trim().parse::<usize>().unwrap_or(0);
            Some(AliEvent::ContextAssembled { token_count: count, budget: 12000 })
        } else if let Some(_rest) = rest.strip_prefix("L5.5 ") {
            // Compressed: "L5.5 read_file 1234→456 chars"
            Some(AliEvent::ToolCompressed {
                id: String::new(),
                original_chars: 0,
                compressed_chars: 0,
            })
        } else if let Some(rest) = rest.strip_prefix("L7 ") {
            if let Some(name) = rest.strip_prefix("tool_executed name=") {
                let parts: Vec<&str> = name.splitn(2, ' ').collect();
                let tool_name = parts[0].to_string();
                let preview = parts.get(1).map(|s| s.to_string()).unwrap_or_default();
                Some(AliEvent::ToolEnd {
                    id: String::new(),
                    name: tool_name,
                    exit_code: Some(0),
                    duration_ms: 0,
                    success: true,
                    output_preview: preview,
                })
            } else if rest.starts_with("human_gate_pending tool=") {
                let tool = rest.strip_prefix("human_gate_pending tool=").unwrap()
                    .split_whitespace().next().unwrap_or("");
                Some(AliEvent::HumanGatePending {
                    request_id: String::new(),
                    tool_name: tool.to_string(),
                    risk: "MEDIUM".to_string(),
                    tool_index: 0,
                })
            } else if rest.starts_with("human_gate_approved tool=") {
                let tool = rest.strip_prefix("human_gate_approved tool=").unwrap()
                    .split_whitespace().next().unwrap_or("");
                Some(AliEvent::HumanGateApproved {
                    request_id: String::new(),
                    tool_name: tool.to_string(),
                })
            } else {
                None
            }
        } else if let Some(detail) = rest.strip_prefix("L8 ") {
            let rewritten = detail.contains("critique=true");
            Some(AliEvent::CritiqueApplied { rewritten })
        } else if let Some(stats) = rest.strip_prefix("L9 ") {
            Some(AliEvent::PipelineDone {
                request_id: String::new(),
                latency_ms: 0,
                tool_called: stats.contains("tool_called=true"),
                prompt_tokens: 0,
                completion_tokens: 0,
            })
        } else {
            None
        }
    }

    /// Serialize to JSON bytes for WebSocket transmission.
    pub fn to_json_bytes(&self) -> Vec<u8> {
        serde_json::to_string(self).unwrap_or_default().into_bytes()
    }

    /// Deserialize from JSON bytes.
    pub fn from_json_bytes(bytes: &[u8]) -> Option<Self> {
        serde_json::from_slice(bytes).ok()
    }

    /// Get the event timestamp (for events that have one).
    pub fn timestamp(&self) -> DateTime<Utc> {
        Utc::now()
    }

    /// Human-readable one-line summary for the TUI status bar.
    pub fn one_line_summary(&self) -> String {
        match self {
            AliEvent::Route { route, .. } => format!("route={}", route),
            AliEvent::MemoriesRetrieved { count, .. } => format!("memories={}", count),
            AliEvent::ContextAssembled { token_count, .. } => format!("ctx={}t", token_count),
            AliEvent::ToolStart { name, .. } => format!("→ {}", name),
            AliEvent::ToolOutput { name, chunk, .. } => {
                let preview = if chunk.len() > 40 { format!("{}...", &chunk[..40]) } else { chunk.clone() };
                format!("{}: {}", name, preview)
            }
            AliEvent::ToolEnd { name, success, duration_ms, .. } => {
                let icon = if *success { "✓" } else { "✗" };
                format!("{} {} ({}ms)", icon, name, duration_ms)
            }
            AliEvent::IntentUpdate { state, goal, .. } => {
                let short_goal = if goal.len() > 30 { format!("{}...", &goal[..30]) } else { goal.clone() };
                format!("intent={} {}", state, short_goal)
            }
            AliEvent::SessionStart { session_name, .. } => format!("session: {}", session_name),
            AliEvent::SessionEnd { session_name, duration_secs } => {
                format!("session ended: {} ({}s)", session_name, duration_secs)
            }
            AliEvent::HumanGatePending { tool_name, .. } => format!("awaiting approval: {}", tool_name),
            AliEvent::CritiqueApplied { rewritten } => {
                if *rewritten { "critique: rewritten".into() } else { "critique: passed".into() }
            }
            AliEvent::PipelineDone { latency_ms, tool_called, .. } => {
                format!("done {}ms tools={}", latency_ms, tool_called)
            }
            AliEvent::Error { code, message } => format!("error {}: {}", code, message),
            AliEvent::Ping { .. } => String::new(),
            _ => String::new(),
        }
    }
}

/// Event emitter that broadcasts to all connected TUI clients.
/// The actual broadcast transport is handled by the WebSocket layer.
pub trait EventEmitter: Send + Sync {
    fn emit(&self, event: AliEvent);
}

#[derive(Clone, Default)]
pub struct NullEmitter;

impl EventEmitter for NullEmitter {
    fn emit(&self, _event: AliEvent) {}
}
