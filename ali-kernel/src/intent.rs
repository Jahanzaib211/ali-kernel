//! Intent — Limbic System: goal state machine + emotional signals
//!
//! Nature psychology: The limbic system tracks what matters — goals, frustrations,
//! satisfaction. The intent tracker is the emotional core of the agent.
//! Every user message is classified against the active intent. The state machine
//! ensures we never lose track of what we're working toward.
//!
//! States: DORMANT → ACTIVE → PURSUING → ACHIEVED | ABANDONED | BLOCKED
//!
//! The active intent is injected into context every time so the model always
//! knows what goal it is working toward — even across sessions.

use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;

// ── Types ────────────────────────────────────────────────────────────────────

/// Intent state machine states.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum IntentState {
    /// No active goal — casual chat
    Dormant,
    /// User has stated a direction
    Active,
    /// Agent is working toward it (tool calls happening)
    Pursuing,
    /// Success confirmed
    Achieved,
    /// User explicitly gave up
    Abandoned,
    /// Same obstacle hit 3+ times
    Blocked,
}

impl std::fmt::Display for IntentState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            IntentState::Dormant => write!(f, "dormant"),
            IntentState::Active => write!(f, "active"),
            IntentState::Pursuing => write!(f, "pursuing"),
            IntentState::Achieved => write!(f, "achieved"),
            IntentState::Abandoned => write!(f, "abandoned"),
            IntentState::Blocked => write!(f, "blocked"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IntentRecord {
    pub id: String,
    pub state: IntentState,
    /// User's original goal statement
    pub goal: String,
    /// When the intent was created
    pub created: DateTime<Utc>,
    /// When the intent last changed state
    pub updated: DateTime<Utc>,
    /// How many tool-call rounds we've done on this intent
    pub attempt_count: usize,
    /// Consecutive failures on same tool+args
    pub consecutive_blocks: usize,
    /// The tool+args that last blocked us
    pub last_blocked_by: Option<String>,
    /// History of state transitions
    pub state_history: Vec<StateTransition>,
    /// Journal entry ID if this intent was completed
    pub journal_entry_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateTransition {
    pub from: IntentState,
    pub to: IntentState,
    pub ts: DateTime<Utc>,
    pub reason: String,
}

/// Lightweight snapshot for context injection.
#[derive(Debug, Clone)]
pub struct IntentContext {
    pub state: IntentState,
    pub goal: String,
    pub attempt_count: usize,
    pub last_blocked_by: Option<String>,
    pub history_len: usize,
}

impl IntentRecord {
    fn new(goal: String) -> Self {
        let now = Utc::now();
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            state: IntentState::Active,
            goal,
            created: now,
            updated: now,
            attempt_count: 0,
            consecutive_blocks: 0,
            last_blocked_by: None,
            state_history: vec![StateTransition {
                from: IntentState::Dormant,
                to: IntentState::Active,
                ts: now,
                reason: "user stated goal".into(),
            }],
            journal_entry_id: None,
        }
    }

    fn transition_to(&mut self, new_state: IntentState, reason: String) {
        if self.state == new_state {
            return;
        }
        let now = Utc::now();
        self.state_history.push(StateTransition {
            from: self.state,
            to: new_state,
            ts: now,
            reason,
        });
        self.state = new_state;
        self.updated = now;
    }
}

// ── Intent Store ────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct Intent {
    store_path: PathBuf,
    /// Current active intent
    active: Arc<RwLock<Option<IntentRecord>>>,
    /// Recent resolved intents (for cross-session recall)
    recent: Arc<RwLock<VecDeque<IntentRecord>>>,
    /// Max recent intents to keep in memory
    max_recent: usize,
}

impl Intent {
    pub fn new(config: &crate::config::Config) -> Result<Self> {
        let intent_dir = config.ali_dir.join("intent");
        fs::create_dir_all(&intent_dir)?;
        Ok(Self {
            store_path: intent_dir.join("intent.jsonl"),
            active: Arc::new(RwLock::new(None)),
            recent: Arc::new(RwLock::new(VecDeque::with_capacity(50))),
            max_recent: 50,
        })
    }

    /// Load the last active intent from disk (call at startup).
    pub async fn load_last(&self) -> Result<()> {
        if !self.store_path.exists() {
            return Ok(());
        }
        let content = fs::read_to_string(&self.store_path)?;
        for line in content.lines().rev() {
            if let Ok(intent) = serde_json::from_str::<IntentRecord>(line) {
                if intent.state != IntentState::Achieved && intent.state != IntentState::Abandoned {
                    *self.active.write().await = Some(intent);
                    tracing::info!("INTENT restored goal={}", self.active.read().await.as_ref().map(|i| i.goal.as_str()).unwrap_or(""));
                    return Ok(());
                }
            }
        }
        Ok(())
    }

    /// Classify a user message and update intent state.
    /// Returns true if a new intent was activated.
    pub async fn classify(&self, user_message: &str) -> bool {
        let msg_lower = user_message.to_lowercase();

        // Resume patterns — pick up a previous thread
        let resume_patterns = [
            "continue", "keep going", "pick up", "carry on",
            "again", "more", "still", "another",
        ];
        let is_resume = resume_patterns.iter().any(|p| msg_lower.contains(p));

        // Goal-statement patterns
        let goal_patterns = [
            "i want", "i need", "let's", "add", "build", "create",
            "implement", "fix", "change", "update", "migrate", "refactor",
            "write", "make", "set up", "configure", "help me",
            "can you", "please",
        ];
        let is_goal_statement = goal_patterns.iter().any(|p| msg_lower.contains(p));

        // Abandonment patterns
        let abandon_patterns = ["forget it", "never mind", "cancel", "skip this", "stop it"];
        let is_abandon = abandon_patterns.iter().any(|p| msg_lower.contains(p));

        // Achievement patterns
        let achieve_patterns = [
            "perfect", "that works", "thanks", "done", "great",
            "exactly", "awesome", "nice", "good", "worked",
        ];
        let is_achieve = achieve_patterns.iter().any(|p| msg_lower.contains(p));

        let mut guard = self.active.write().await;

        // Determine what action to take based on current state
        enum Action { None, Resume, Abandon, Achieve, NewGoal, SubGoal }
        let mut action = Action::None;
        let mut new_intent_activated = false;

        // Handle "no active intent + goal" case BEFORE match (so match can process it)
        if guard.as_ref().is_none() && is_goal_statement {
            let new_intent = IntentRecord::new(user_message.to_string());
            tracing::info!("INTENT activated goal={}", new_intent.goal);
            *guard = Some(new_intent);
            new_intent_activated = true;
        }

        {
            let current = (&mut *guard).as_mut();

            match (current, is_resume, is_goal_statement, is_abandon, is_achieve) {
                // Resume
                (Some(ref mut intent), true, _, _, _) if intent.state != IntentState::Dormant => {
                    let was = intent.state;
                    intent.transition_to(IntentState::Pursuing, "user resumed".into());
                    tracing::info!("INTENT resumed goal={} {}→pursuing", intent.goal, was);
                }
                // Explicit abandon
                (Some(ref mut intent), _, _, true, _) => {
                    intent.transition_to(IntentState::Abandoned, "user abandoned".into());
                    tracing::info!("INTENT abandoned goal={}", intent.goal);
                    action = Action::Abandon;
                }
                // Explicit achievement
                (Some(ref mut intent), _, _, _, true) if intent.state == IntentState::Pursuing => {
                    intent.transition_to(IntentState::Achieved, "user confirmed success".into());
                    tracing::info!("INTENT achieved goal={}", intent.goal);
                    action = Action::Achieve;
                }
                // New goal while pursuing
                (Some(ref mut intent), _, true, _, _) if intent.state == IntentState::Pursuing => {
                    tracing::info!("INTENT sub-goal detected: {}", user_message);
                }
                _ => {}
            }
        } // inner borrow ends here



        // Handle actions that need async operations (outside the borrow)
        match action {
            Action::Abandon => {
                drop(guard);
                self.abandon("user abandoned").await;
            }
            Action::Achieve => {
                drop(guard);
                self.achieve("user confirmed success").await;
            }
            _ => {}
        }

        new_intent_activated
    }

    /// Called after each tool execution round. Updates attempt count and detects blocks.
    pub async fn on_tool_round(&self, tool_name: &str, tool_args: &str, tool_success: bool) {
        let mut guard = self.active.write().await;
        {
            let inner = (&mut *guard);
            if let Some(ref mut intent) = inner.as_mut() {
                intent.attempt_count += 1;

                if !tool_success {
                    if intent.state == IntentState::Active {
                        intent.transition_to(IntentState::Pursuing, "tool round started".into());
                    }
                    let block_sig = format!("{}({})", tool_name, tool_args);
                    if intent.last_blocked_by.as_ref() == Some(&block_sig) {
                        intent.consecutive_blocks += 1;
                    } else {
                        intent.consecutive_blocks = 1;
                        intent.last_blocked_by = Some(block_sig);
                    }

                    if intent.consecutive_blocks >= 3 {
                        intent.transition_to(IntentState::Blocked, format!("same tool blocked 3x: {}", tool_name));
                        tracing::warn!("INTENT blocked goal={}", intent.goal);
                    }
                } else {
                    intent.consecutive_blocks = 0;
                    intent.last_blocked_by = None;
                    if intent.state == IntentState::Blocked {
                        intent.transition_to(IntentState::Pursuing, "tool succeeded after block".into());
                    } else if intent.state == IntentState::Active {
                        intent.transition_to(IntentState::Pursuing, "tool round succeeded".into());
                    }
                }
            }
        }
    }

    /// Called when all tool rounds complete. Finalize state.
    pub async fn on_session_end(&self, had_success: bool) {
        let mut guard = self.active.write().await;
        {
            let inner = (&mut *guard);
            if let Some(ref mut intent) = inner.as_mut() {
                if had_success && intent.state == IntentState::Pursuing {
                    intent.transition_to(IntentState::Achieved, "session ended with success".into());
                    tracing::info!("INTENT achieved at session end goal={}", intent.goal);
                }
                if let Err(e) = self.persist(&intent) {
                    tracing::error!("INTENT persist failed: {e}");
                }
            }
        }
    }

    /// Mark the active intent as achieved.
    pub async fn achieve(&self, description: &str) {
        let mut guard = self.active.write().await;
        {
            let inner = (&mut *guard);
            if let Some(ref mut intent) = inner.as_mut() {
                intent.transition_to(IntentState::Achieved, description.into());
                tracing::info!("INTENT achieved goal={}", intent.goal);
                if let Err(e) = self.persist(&intent) {
                    tracing::error!("INTENT persist failed: {e}");
                }
            }
        }
    }

    /// Mark the active intent as abandoned.
    pub async fn abandon(&self, reason: &str) {
        let mut guard = self.active.write().await;
        {
            let inner = (&mut *guard);
            if let Some(ref mut intent) = inner.as_mut() {
                intent.transition_to(IntentState::Abandoned, reason.into());
                tracing::info!("INTENT abandoned goal={}", intent.goal);
                if let Err(e) = self.persist(&intent) {
                    tracing::error!("INTENT persist failed: {e}");
                }
            }
        }
    }

    /// Get the active intent context for injection into the system prompt.
    pub async fn get_context(&self) -> Option<IntentContext> {
        self.active.read().await.as_ref().map(|i| IntentContext {
            state: i.state,
            goal: i.goal.clone(),
            attempt_count: i.attempt_count,
            last_blocked_by: i.last_blocked_by.clone(),
            history_len: i.state_history.len(),
        })
    }

    /// Get the full active intent record.
    pub async fn get_active(&self) -> Option<IntentRecord> {
        self.active.read().await.clone()
    }

    /// Get recent resolved intents for cross-session context.
    pub async fn get_recent(&self, limit: usize) -> Vec<IntentRecord> {
        let guard = self.recent.read().await;
        guard.iter().take(limit).cloned().collect()
    }

    // ── Private ──────────────────────────────────────────────────────────────

    async fn push_recent(&self) {
        if let Some(intent) = self.active.write().await.take() {
            let mut r = self.recent.write().await;
            r.push_front(intent);
            while r.len() > self.max_recent {
                r.pop_back();
            }
        }
    }

    fn persist(&self, intent: &IntentRecord) -> Result<()> {
        let line = serde_json::to_string(intent)?;
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.store_path)?;
        writeln!(file, "{}", line)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_intent() -> Intent {
        let cfg = crate::config::Config {
            ali_dir: std::env::temp_dir().join("ali_intent_test"),
            ..Default::default()
        };
        Intent::new(&cfg).unwrap()
    }

    #[tokio::test]
    async fn new_goal_activates_intent() {
        let i = temp_intent();
        let activated = i.classify("I want to add JWT auth to the API").await;
        assert!(activated);
        let ctx = i.get_context().await.unwrap();
        assert_eq!(ctx.state, IntentState::Active);
    }

    #[tokio::test]
    async fn resume_picks_up_intent() {
        let i = temp_intent();
        i.classify("add auth to the API").await;
        i.on_tool_round("bash_exec", "cargo build", false).await;
        let ctx_before = i.get_context().await.unwrap();
        assert_eq!(ctx_before.state, IntentState::Pursuing);
        // Simulate 3 blocks → blocked
        i.on_tool_round("bash_exec", "cargo build", false).await;
        i.on_tool_round("bash_exec", "cargo build", false).await;
        let ctx = i.get_context().await.unwrap();
        assert_eq!(ctx.state, IntentState::Blocked);
    }

    #[tokio::test]
    async fn classify_abandon() {
        let i = temp_intent();
        i.classify("add auth to the API").await;
        i.classify("forget it").await;
        let ctx = i.get_context().await.unwrap();
        assert_eq!(ctx.state, IntentState::Abandoned);
    }

    #[tokio::test]
    async fn classify_achieve() {
        let i = temp_intent();
        i.classify("add auth to the API").await;
        let _ = i.on_tool_round("bash_exec", "cargo build", true).await;
        i.classify("perfect, that's exactly what I wanted").await;
        let ctx = i.get_context().await.unwrap();
        assert_eq!(ctx.state, IntentState::Achieved);
    }

    #[tokio::test]
    async fn context_injects_correctly() {
        let i = temp_intent();
        i.classify("implement the user profile page").await;
        let ctx = i.get_context().await.unwrap();
        assert!(ctx.goal.contains("user profile"));
        assert_eq!(ctx.state, IntentState::Active);
        assert_eq!(ctx.attempt_count, 0);
    }
}
